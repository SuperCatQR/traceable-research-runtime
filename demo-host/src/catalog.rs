use std::{
    fs,
    path::Path,
    sync::{Mutex, MutexGuard},
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use traceable_search::ResearchAnswerStyle;
use uuid::Uuid;

use crate::security::EncryptedCredential;

const IDEMPOTENCY_TAKEOVER_SECONDS: i64 = 5 * 60;
const CATALOG_SCHEMA_VERSION: i64 = 7;
const CATALOG_SCHEMA_V1: &str = include_str!("../../docs/database/0001-demo-catalog.sql");
const CATALOG_SCHEMA_V2: &str =
    include_str!("../../docs/database/0002-research-turn-answer-style.sql");
const CATALOG_SCHEMA_V3: &str =
    include_str!("../../docs/database/0003-workspace-idempotency-and-archive-recovery.sql");
const CATALOG_SCHEMA_V4: &str =
    include_str!("../../docs/database/0004-active-model-profile-names.sql");
const CATALOG_SCHEMA_V5: &str =
    include_str!("../../docs/database/0005-idempotency-claim-fencing.sql");
const CATALOG_SCHEMA_V6: &str =
    include_str!("../../docs/database/0006-one-active-turn-per-conversation.sql");
const CATALOG_SCHEMA_V7: &str =
    include_str!("../../docs/database/0007-idempotency-operation-journal.sql");
pub const DEFAULT_RESEARCH_CONVERSATION_TITLE: &str = "New research";

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("catalog lock was poisoned")]
    LockPoisoned,
    #[error("catalog schema version {0} is not supported")]
    UnsupportedSchema(i64),
    #[error("catalog object was not found")]
    NotFound,
    #[error("catalog operation conflicts with existing state: {0}")]
    Conflict(CatalogConflict),
    #[error("catalog contains invalid data: {0}")]
    InvalidData(&'static str),
    #[error("catalog response serialization failed: {0}")]
    ResponseSerialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogConflict {
    EmailAlreadyRegistered,
    ModelProfileNameAlreadyExists,
    ModelProfileInUseByActiveTurn,
    ModelProfileInUseByConversation,
    ConversationHasActiveTurn,
    ConversationModelProfileArchived,
    ConversationModelProfileChanged,
    ModelProfileChanged,
    ResearchTurnStatusChanged,
}

impl std::fmt::Display for CatalogConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::EmailAlreadyRegistered => "email is already registered",
            Self::ModelProfileNameAlreadyExists => "model profile name already exists",
            Self::ModelProfileInUseByActiveTurn => "model profile is used by an active turn",
            Self::ModelProfileInUseByConversation => {
                "model profile is used by an active conversation"
            }
            Self::ConversationHasActiveTurn => "conversation has an active turn",
            Self::ConversationModelProfileArchived => "conversation model profile is archived",
            Self::ConversationModelProfileChanged => {
                "conversation model profile changed before this research turn was created"
            }
            Self::ModelProfileChanged => "model profile revision changed",
            Self::ResearchTurnStatusChanged => "research turn status changed",
        };
        formatter.write_str(message)
    }
}

pub type CatalogResult<T> = Result<T, CatalogError>;

pub struct DemoCatalog {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAccountRecord {
    pub user_id: String,
    pub normalized_email: String,
    pub display_name: String,
    pub password_hash: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProfileRecord {
    pub profile_id: String,
    pub user_id: String,
    pub display_name: String,
    pub api_base_url: String,
    pub model_id: String,
    pub encrypted_api_key: EncryptedCredential,
    pub revision: i64,
    pub is_default: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub verified_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedModelProfileRecord {
    pub profile: ModelProfileRecord,
    pub archived_at: i64,
}

pub struct NewModelProfile<'a> {
    pub profile_id: &'a str,
    pub user_id: &'a str,
    pub display_name: &'a str,
    pub api_base_url: &'a str,
    pub model_id: &'a str,
    pub encrypted_api_key: &'a EncryptedCredential,
    pub make_default: bool,
    pub now: i64,
}

pub struct UpdatedModelProfile<'a> {
    pub profile_id: &'a str,
    pub user_id: &'a str,
    pub expected_revision: i64,
    pub display_name: &'a str,
    pub api_base_url: &'a str,
    pub model_id: &'a str,
    pub encrypted_api_key: &'a EncryptedCredential,
    pub now: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchConversationRecord {
    pub conversation_id: String,
    pub user_id: String,
    pub core_conversation_id: String,
    pub title: String,
    pub model_profile_id: String,
    pub model_profile_name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub turn_count: i64,
    pub latest_turn_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedConversationRecord {
    pub conversation: ResearchConversationRecord,
    pub archived_at: i64,
    pub model_profile_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyClaim {
    Claimed {
        claim_token: String,
    },
    InProgress,
    Replay {
        status_code: i64,
        response_json: String,
    },
    Reused,
}

pub struct NewIdempotencyClaim<'a> {
    pub user_id: &'a str,
    pub method: &'a str,
    pub resource_scope: &'a str,
    pub key: &'a str,
    pub request_hash: &'a str,
    pub now: i64,
    pub expires_at: i64,
}

pub struct CompleteIdempotency<'a> {
    pub user_id: &'a str,
    pub method: &'a str,
    pub resource_scope: &'a str,
    pub key: &'a str,
    pub claim_token: &'a str,
    pub status_code: i64,
    pub response_json: &'a str,
}

/// The durable identity of one idempotent operation. The operation identity
/// survives a fencing-token takeover; only the claim token changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyClaimLease {
    pub operation_id: String,
    pub operation_created_at: i64,
    pub claim_token: String,
    pub serialization_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableIdempotencyClaim {
    Claimed(IdempotencyClaimLease),
    InProgress {
        operation_id: String,
        operation_created_at: i64,
    },
    Blocked {
        operation_id: String,
        operation_created_at: i64,
    },
    Replay {
        operation_id: String,
        operation_created_at: i64,
        status_code: i64,
        response_json: String,
    },
    Reused,
}

pub struct NewDurableIdempotencyClaim<'a> {
    pub user_id: &'a str,
    pub method: &'a str,
    pub resource_scope: &'a str,
    pub key: &'a str,
    pub request_hash: &'a str,
    pub serialization_key: Option<&'a str>,
    pub now: i64,
    pub expires_at: i64,
}

pub struct DurableIdempotencyCompletion<'a> {
    pub user_id: &'a str,
    pub method: &'a str,
    pub resource_scope: &'a str,
    pub key: &'a str,
    pub operation_id: &'a str,
    pub operation_created_at: i64,
    pub claim_token: &'a str,
    pub status_code: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotentCommit<R, P> {
    pub resource: R,
    pub projection: P,
    pub response_json: String,
}

pub struct NewResearchConversation<'a> {
    pub conversation_id: &'a str,
    pub user_id: &'a str,
    pub core_conversation_id: &'a str,
    pub title: &'a str,
    pub model_profile_id: &'a str,
    pub now: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchTurnStatus {
    Clarifying,
    Ready,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ResearchTurnStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clarifying => "clarifying",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    const fn is_nonterminal(self) -> bool {
        matches!(self, Self::Clarifying | Self::Ready | Self::Running)
    }

    const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Clarifying => true,
            Self::Ready => !matches!(next, Self::Clarifying),
            Self::Running => !matches!(next, Self::Clarifying | Self::Ready),
            Self::Completed => matches!(next, Self::Completed),
            Self::Failed => matches!(next, Self::Failed),
            Self::Cancelled => matches!(next, Self::Cancelled),
        }
    }

    fn parse(value: &str) -> CatalogResult<Self> {
        match value {
            "clarifying" => Ok(Self::Clarifying),
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(CatalogError::InvalidData("unknown research turn status")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchTurnRecord {
    pub turn_id: String,
    pub conversation_id: String,
    pub turn_number: i64,
    pub clarification_id: String,
    pub run_id: Option<String>,
    pub user_question: String,
    pub status: ResearchTurnStatus,
    pub answer_style: ResearchAnswerStyle,
    pub model_profile_id: String,
    pub model_profile_revision: i64,
    pub model_api_base_url: String,
    pub model_id: String,
    pub answer_json: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

/// A persisted turn that may have been interrupted after the model decided to
/// start research. The owner is included so background recovery still applies
/// the same model-profile ownership and revision checks as HTTP requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticExecutionRecoveryCandidate {
    pub user_id: String,
    pub turn: ResearchTurnRecord,
}

pub struct NewResearchTurn<'a> {
    pub turn_id: &'a str,
    pub conversation_id: &'a str,
    pub turn_number: i64,
    pub clarification_id: &'a str,
    pub user_question: &'a str,
    pub status: ResearchTurnStatus,
    pub answer_style: ResearchAnswerStyle,
    pub model_profile: &'a ModelProfileRecord,
    pub now: i64,
}

impl DemoCatalog {
    pub fn open(path: impl AsRef<Path>) -> CatalogResult<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CatalogError::Sql(rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
            })?;
        }
        let mut connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS schema_migrations (
                 version INTEGER PRIMARY KEY NOT NULL,
                 applied_at INTEGER NOT NULL
             ) STRICT;",
        )?;
        let current_version =
            connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?;
        let mut version = match current_version {
            None => {
                let transaction = connection.transaction()?;
                transaction.execute_batch(CATALOG_SCHEMA_V1)?;
                transaction.commit()?;
                1
            }
            Some(version) => version,
        };
        if version == 1 {
            connection.execute_batch(CATALOG_SCHEMA_V2)?;
            version = 2;
        }
        if version == 2 {
            connection.execute_batch(CATALOG_SCHEMA_V3)?;
            version = 3;
        }
        if version == 3 {
            connection.execute_batch(CATALOG_SCHEMA_V4)?;
            version = 4;
        }
        if version == 4 {
            connection.execute_batch(CATALOG_SCHEMA_V5)?;
            version = 5;
        }
        if version == 5 {
            connection.execute_batch(CATALOG_SCHEMA_V6)?;
            version = 6;
        }
        if version == 6 {
            connection.execute_batch(CATALOG_SCHEMA_V7)?;
            version = 7;
        }
        if version != CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::UnsupportedSchema(version));
        }
        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idempotency_records_operation_unique
             ON idempotency_records(operation_id);",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn create_user_account(
        &self,
        user_id: &str,
        normalized_email: &str,
        display_name: &str,
        password_hash: &str,
        now: i64,
    ) -> CatalogResult<UserAccountRecord> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO user_accounts (
                    user_id, normalized_email, display_name, password_hash, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![user_id, normalized_email, display_name, password_hash, now],
            )
            .map_err(|error| map_constraint(error, CatalogConflict::EmailAlreadyRegistered))?;
        drop(connection);
        self.user_account_by_id(user_id)
    }

    pub fn user_account_by_email(
        &self,
        normalized_email: &str,
    ) -> CatalogResult<Option<UserAccountRecord>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT user_id, normalized_email, display_name, password_hash, created_at
                 FROM user_accounts WHERE normalized_email = ?1",
                [normalized_email],
                map_user_account,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn user_account_by_id(&self, user_id: &str) -> CatalogResult<UserAccountRecord> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT user_id, normalized_email, display_name, password_hash, created_at
                 FROM user_accounts WHERE user_id = ?1",
                [user_id],
                map_user_account,
            )
            .optional()?
            .ok_or(CatalogError::NotFound)
    }

    pub fn create_login_session(
        &self,
        token_hash: &[u8; 32],
        user_id: &str,
        now: i64,
        expires_at: i64,
    ) -> CatalogResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO login_sessions (
                token_hash, user_id, created_at, last_seen_at, expires_at, revoked_at
             ) VALUES (?1, ?2, ?3, ?3, ?4, NULL)",
            params![token_hash.as_slice(), user_id, now, expires_at],
        )?;
        Ok(())
    }

    pub fn authenticated_user(
        &self,
        token_hash: &[u8; 32],
        now: i64,
    ) -> CatalogResult<Option<UserAccountRecord>> {
        let connection = self.connection()?;
        let user = connection
            .query_row(
                "SELECT u.user_id, u.normalized_email, u.display_name, u.password_hash, u.created_at
                 FROM login_sessions s
                 JOIN user_accounts u ON u.user_id = s.user_id
                 WHERE s.token_hash = ?1
                   AND s.revoked_at IS NULL
                   AND s.expires_at > ?2",
                params![token_hash.as_slice(), now],
                map_user_account,
            )
            .optional()?;
        if user.is_some() {
            connection.execute(
                "UPDATE login_sessions
                 SET last_seen_at = ?2
                 WHERE token_hash = ?1 AND last_seen_at < ?2 - 300",
                params![token_hash.as_slice(), now],
            )?;
        }
        Ok(user)
    }

    pub fn revoke_login_session(&self, token_hash: &[u8; 32], now: i64) -> CatalogResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE login_sessions SET revoked_at = COALESCE(revoked_at, ?2) WHERE token_hash = ?1",
            params![token_hash.as_slice(), now],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn create_model_profile(
        &self,
        profile: NewModelProfile<'_>,
    ) -> CatalogResult<ModelProfileRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let created = insert_model_profile_tx(&transaction, profile)?;
        transaction.commit()?;
        Ok(created)
    }

    pub fn list_model_profiles(&self, user_id: &str) -> CatalogResult<Vec<ModelProfileRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT profile_id, user_id, display_name, api_base_url, model_id,
                    api_key_ciphertext, api_key_nonce, revision, is_default,
                    created_at, updated_at, verified_at
             FROM model_profiles
             WHERE user_id = ?1 AND archived_at IS NULL
             ORDER BY is_default DESC, updated_at DESC, display_name",
        )?;
        let rows = statement.query_map([user_id], map_model_profile)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_archived_model_profiles(
        &self,
        user_id: &str,
    ) -> CatalogResult<Vec<ArchivedModelProfileRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT profile_id, user_id, display_name, api_base_url, model_id,
                    api_key_ciphertext, api_key_nonce, revision, 0,
                    created_at, updated_at, verified_at, archived_at
             FROM model_profiles
             WHERE user_id = ?1 AND archived_at IS NOT NULL
             ORDER BY archived_at DESC, display_name",
        )?;
        let rows = statement.query_map([user_id], |row| {
            Ok(ArchivedModelProfileRecord {
                profile: map_model_profile(row)?,
                archived_at: row.get(12)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn model_profile(
        &self,
        user_id: &str,
        profile_id: &str,
    ) -> CatalogResult<ModelProfileRecord> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT profile_id, user_id, display_name, api_base_url, model_id,
                        api_key_ciphertext, api_key_nonce, revision, is_default,
                        created_at, updated_at, verified_at
                 FROM model_profiles
                 WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
                params![user_id, profile_id],
                map_model_profile,
            )
            .optional()?
            .ok_or(CatalogError::NotFound)
    }

    pub fn default_model_profile(&self, user_id: &str) -> CatalogResult<ModelProfileRecord> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT profile_id, user_id, display_name, api_base_url, model_id,
                        api_key_ciphertext, api_key_nonce, revision, is_default,
                        created_at, updated_at, verified_at
                 FROM model_profiles
                 WHERE user_id = ?1 AND is_default = 1 AND archived_at IS NULL",
                [user_id],
                map_model_profile,
            )
            .optional()?
            .ok_or(CatalogError::NotFound)
    }

    pub fn update_model_profile(
        &self,
        profile: UpdatedModelProfile<'_>,
    ) -> CatalogResult<ModelProfileRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if active_turn_uses_profile(&transaction, profile.user_id, profile.profile_id)? {
            return Err(CatalogError::Conflict(
                CatalogConflict::ModelProfileInUseByActiveTurn,
            ));
        }
        let changed = transaction
            .execute(
                "UPDATE model_profiles
                 SET display_name = ?3, api_base_url = ?4, model_id = ?5,
                     api_key_ciphertext = ?6, api_key_nonce = ?7,
                     revision = revision + 1, updated_at = ?8, verified_at = NULL
                 WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL
                   AND revision = ?9",
                params![
                    profile.user_id,
                    profile.profile_id,
                    profile.display_name,
                    profile.api_base_url,
                    profile.model_id,
                    profile.encrypted_api_key.ciphertext,
                    profile.encrypted_api_key.nonce.as_slice(),
                    profile.now,
                    profile.expected_revision,
                ],
            )
            .map_err(|error| {
                map_constraint(error, CatalogConflict::ModelProfileNameAlreadyExists)
            })?;
        if changed == 0 {
            let revision = transaction
                .query_row(
                    "SELECT revision FROM model_profiles
                     WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
                    params![profile.user_id, profile.profile_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            return Err(match revision {
                Some(_) => CatalogError::Conflict(CatalogConflict::ModelProfileChanged),
                None => CatalogError::NotFound,
            });
        }
        let updated = transaction.query_row(
            "SELECT profile_id, user_id, display_name, api_base_url, model_id,
                    api_key_ciphertext, api_key_nonce, revision, is_default,
                    created_at, updated_at, verified_at
             FROM model_profiles
             WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
            params![profile.user_id, profile.profile_id],
            map_model_profile,
        )?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn set_default_model_profile(
        &self,
        user_id: &str,
        profile_id: &str,
        now: i64,
    ) -> CatalogResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM model_profiles
                WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL
             )",
            params![user_id, profile_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(CatalogError::NotFound);
        }
        transaction.execute(
            "UPDATE model_profiles SET is_default = 0, updated_at = ?2 WHERE user_id = ?1",
            params![user_id, now],
        )?;
        transaction.execute(
            "UPDATE model_profiles SET is_default = 1, updated_at = ?3
             WHERE user_id = ?1 AND profile_id = ?2",
            params![user_id, profile_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_model_profile_verified(
        &self,
        user_id: &str,
        profile_id: &str,
        expected_revision: i64,
        now: i64,
    ) -> CatalogResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE model_profiles SET verified_at = ?4, updated_at = ?4
             WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL
               AND revision = ?3",
            params![user_id, profile_id, expected_revision, now],
        )?;
        if changed == 0 {
            let revision = transaction
                .query_row(
                    "SELECT revision FROM model_profiles
                     WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
                    params![user_id, profile_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            return Err(match revision {
                Some(_) => CatalogError::Conflict(CatalogConflict::ModelProfileChanged),
                None => CatalogError::NotFound,
            });
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn archive_model_profile(
        &self,
        user_id: &str,
        profile_id: &str,
        now: i64,
    ) -> CatalogResult<()> {
        let connection = self.connection()?;
        if active_turn_uses_profile(&connection, user_id, profile_id)? {
            return Err(CatalogError::Conflict(
                CatalogConflict::ModelProfileInUseByActiveTurn,
            ));
        }
        let selected_conversations: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM research_conversations
                WHERE user_id = ?1 AND model_profile_id = ?2 AND archived_at IS NULL
             )",
            params![user_id, profile_id],
            |row| row.get(0),
        )?;
        if selected_conversations {
            return Err(CatalogError::Conflict(
                CatalogConflict::ModelProfileInUseByConversation,
            ));
        }
        let changed = connection.execute(
            "UPDATE model_profiles
             SET archived_at = ?3, updated_at = ?3, is_default = 0
             WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
            params![user_id, profile_id, now],
        )?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
        Ok(())
    }

    pub fn restore_model_profile(
        &self,
        user_id: &str,
        profile_id: &str,
        now: i64,
    ) -> CatalogResult<ModelProfileRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM model_profiles WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NOT NULL)",
            params![user_id, profile_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(CatalogError::NotFound);
        }
        let name: String = transaction.query_row(
            "SELECT display_name FROM model_profiles WHERE user_id = ?1 AND profile_id = ?2",
            params![user_id, profile_id],
            |row| row.get(0),
        )?;
        let duplicate: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM model_profiles WHERE user_id = ?1 AND display_name = ?2 AND archived_at IS NULL)",
            params![user_id, name],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(CatalogError::Conflict(
                CatalogConflict::ModelProfileNameAlreadyExists,
            ));
        }
        let has_default: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM model_profiles WHERE user_id = ?1 AND is_default = 1 AND archived_at IS NULL)",
            [user_id],
            |row| row.get(0),
        )?;
        transaction
            .execute(
                "UPDATE model_profiles SET archived_at = NULL, is_default = ?3, updated_at = ?4
                 WHERE user_id = ?1 AND profile_id = ?2",
                params![user_id, profile_id, i64::from(!has_default), now],
            )
            .map_err(|error| {
                map_constraint(error, CatalogConflict::ModelProfileNameAlreadyExists)
            })?;
        transaction.commit()?;
        drop(connection);
        self.model_profile(user_id, profile_id)
    }

    pub fn create_research_conversation(
        &self,
        conversation: NewResearchConversation<'_>,
    ) -> CatalogResult<ResearchConversationRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created = insert_research_conversation_tx(&transaction, conversation)?;
        transaction.commit()?;
        Ok(created)
    }

    pub fn list_research_conversations(
        &self,
        user_id: &str,
    ) -> CatalogResult<Vec<ResearchConversationRecord>> {
        let connection = self.connection()?;
        let sql = format!(
            "{} ORDER BY c.updated_at DESC",
            conversation_select("WHERE c.user_id = ?1 AND c.archived_at IS NULL")
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([user_id], map_research_conversation)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_archived_research_conversations(
        &self,
        user_id: &str,
    ) -> CatalogResult<Vec<ArchivedConversationRecord>> {
        let connection = self.connection()?;
        let sql = "SELECT c.conversation_id, c.user_id, c.core_conversation_id, c.title,
                          c.model_profile_id, p.display_name, c.created_at, c.updated_at,
                          COUNT(t.turn_id),
                          (SELECT latest.status FROM research_turns latest
                           WHERE latest.conversation_id = c.conversation_id
                           ORDER BY latest.turn_number DESC LIMIT 1),
                          c.archived_at,
                          EXISTS(SELECT 1 FROM model_profiles active_profile
                                 WHERE active_profile.profile_id = c.model_profile_id
                                   AND active_profile.user_id = c.user_id
                                   AND active_profile.archived_at IS NULL)
                   FROM research_conversations c
                   JOIN model_profiles p ON p.profile_id = c.model_profile_id
                   LEFT JOIN research_turns t ON t.conversation_id = c.conversation_id
                   WHERE c.user_id = ?1 AND c.archived_at IS NOT NULL
                   GROUP BY c.conversation_id
                   ORDER BY c.archived_at DESC, c.updated_at DESC";
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map([user_id], |row| {
            let conversation = map_research_conversation(row)?;
            let archived_at: i64 = row.get(10)?;
            let available: i64 = row.get(11)?;
            Ok(ArchivedConversationRecord {
                conversation,
                archived_at,
                model_profile_available: available != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn research_conversation(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> CatalogResult<ResearchConversationRecord> {
        let connection = self.connection()?;
        connection
            .query_row(
                &conversation_select(
                    "WHERE c.user_id = ?1 AND c.conversation_id = ?2 AND c.archived_at IS NULL",
                ),
                params![user_id, conversation_id],
                map_research_conversation,
            )
            .optional()?
            .ok_or(CatalogError::NotFound)
    }

    pub fn update_research_conversation(
        &self,
        user_id: &str,
        conversation_id: &str,
        title: &str,
        model_profile_id: &str,
        now: i64,
    ) -> CatalogResult<ResearchConversationRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_model_profile_id = transaction
            .query_row(
                "SELECT model_profile_id FROM research_conversations
                 WHERE user_id = ?1 AND conversation_id = ?2 AND archived_at IS NULL",
                params![user_id, conversation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(CatalogError::NotFound)?;
        let profile_available: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM model_profiles
                WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL
             )",
            params![user_id, model_profile_id],
            |row| row.get(0),
        )?;
        if !profile_available {
            return Err(CatalogError::NotFound);
        }
        if conversation_has_unfinished_turn(&transaction, user_id, conversation_id)?
            && current_model_profile_id != model_profile_id
        {
            return Err(CatalogError::Conflict(
                CatalogConflict::ConversationHasActiveTurn,
            ));
        }
        transaction.execute(
            "UPDATE research_conversations
             SET title = ?3, model_profile_id = ?4, updated_at = ?5
             WHERE user_id = ?1 AND conversation_id = ?2 AND archived_at IS NULL",
            params![user_id, conversation_id, title, model_profile_id, now],
        )?;
        transaction.commit()?;
        drop(connection);
        self.research_conversation(user_id, conversation_id)
    }

    pub fn archive_research_conversation(
        &self,
        user_id: &str,
        conversation_id: &str,
        now: i64,
    ) -> CatalogResult<()> {
        let connection = self.connection()?;
        if conversation_has_unfinished_turn(&connection, user_id, conversation_id)? {
            return Err(CatalogError::Conflict(
                CatalogConflict::ConversationHasActiveTurn,
            ));
        }
        let changed = connection.execute(
            "UPDATE research_conversations SET archived_at = ?3, updated_at = ?3
             WHERE user_id = ?1 AND conversation_id = ?2 AND archived_at IS NULL",
            params![user_id, conversation_id, now],
        )?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
        Ok(())
    }

    pub fn restore_research_conversation(
        &self,
        user_id: &str,
        conversation_id: &str,
        model_profile_id: Option<&str>,
        now: i64,
    ) -> CatalogResult<ResearchConversationRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let archived_profile: String = transaction
            .query_row(
                "SELECT model_profile_id FROM research_conversations
                 WHERE user_id = ?1 AND conversation_id = ?2 AND archived_at IS NOT NULL",
                params![user_id, conversation_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CatalogError::NotFound)?;
        let selected = model_profile_id.unwrap_or(&archived_profile);
        let selected_archived_at = transaction
            .query_row(
                "SELECT archived_at FROM model_profiles WHERE user_id = ?1 AND profile_id = ?2",
                params![user_id, selected],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?;
        match selected_archived_at {
            None => return Err(CatalogError::NotFound),
            Some(Some(_)) => {
                return Err(CatalogError::Conflict(
                    CatalogConflict::ConversationModelProfileArchived,
                ));
            }
            Some(None) => {}
        }
        transaction.execute(
            "UPDATE research_conversations SET archived_at = NULL, model_profile_id = ?3, updated_at = ?4
             WHERE user_id = ?1 AND conversation_id = ?2 AND archived_at IS NOT NULL",
            params![user_id, conversation_id, selected, now],
        )?;
        transaction.commit()?;
        drop(connection);
        self.research_conversation(user_id, conversation_id)
    }

    /// Claims an operation while retaining one durable operation identity.
    /// A stale owner may replace only the fencing token and claim timestamp;
    /// operation identity and creation time never change on takeover.
    pub fn claim_operation(
        &self,
        claim: NewDurableIdempotencyClaim<'_>,
    ) -> CatalogResult<DurableIdempotencyClaim> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Only completed replay records are disposable. In-progress and
        // fail-closed legacy records must remain visible to recovery.
        transaction.execute(
            "DELETE FROM idempotency_records
             WHERE status = 'completed' AND expires_at <= ?1",
            [claim.now],
        )?;
        let existing = transaction
            .query_row(
                "SELECT request_hash, operation_id, operation_created_at, claim_token,
                        claimed_at, serialization_key, status, status_code, response_json
                 FROM idempotency_records
                 WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
                   AND idempotency_key = ?4",
                params![claim.user_id, claim.method, claim.resource_scope, claim.key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?;
        let result = match existing {
            None => {
                if let Some(serialization_key) = claim.serialization_key
                    && let Some((operation_id, operation_created_at)) = transaction
                        .query_row(
                            "SELECT operation_id, operation_created_at
                             FROM idempotency_records
                             WHERE user_id = ?1
                               AND (serialization_key = ?2
                                    OR serialization_key = 'legacy:' || ?1 || ':' || ?3 || ':' || ?4)
                               AND status IN ('in_progress', 'blocked')",
                            params![claim.user_id, serialization_key, claim.method, claim.resource_scope],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                        )
                        .optional()?
                {
                    return Ok(DurableIdempotencyClaim::InProgress {
                        operation_id,
                        operation_created_at,
                    });
                }
                let operation_id = new_idempotency_operation_id();
                let claim_token = new_idempotency_claim_token();
                transaction.execute(
                    "INSERT INTO idempotency_records
                     (user_id, method, resource_scope, idempotency_key, request_hash,
                      operation_id, operation_created_at, claim_token, claimed_at,
                      serialization_key, status, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?7, ?9, 'in_progress', ?7, ?10)",
                    params![
                        claim.user_id,
                        claim.method,
                        claim.resource_scope,
                        claim.key,
                        claim.request_hash,
                        operation_id,
                        claim.now,
                        claim_token,
                        claim.serialization_key,
                        claim.expires_at,
                    ],
                )?;
                DurableIdempotencyClaim::Claimed(IdempotencyClaimLease {
                    operation_id,
                    operation_created_at: claim.now,
                    claim_token,
                    serialization_key: claim.serialization_key.map(str::to_owned),
                })
            }
            Some((existing_hash, _, _, _, _, _, _, _, _))
                if existing_hash != claim.request_hash =>
            {
                DurableIdempotencyClaim::Reused
            }
            Some((
                _,
                operation_id,
                operation_created_at,
                _,
                _,
                _,
                status,
                Some(status_code),
                Some(response_json),
            )) if status == "completed" => DurableIdempotencyClaim::Replay {
                operation_id,
                operation_created_at,
                status_code,
                response_json,
            },
            Some((_, operation_id, operation_created_at, _, _, _, status, _, _))
                if status == "blocked" =>
            {
                DurableIdempotencyClaim::Blocked {
                    operation_id,
                    operation_created_at,
                }
            }
            Some((
                _,
                operation_id,
                operation_created_at,
                _,
                claimed_at,
                serialization_key,
                status,
                _,
                _,
            )) if status == "in_progress"
                && claimed_at <= claim.now.saturating_sub(IDEMPOTENCY_TAKEOVER_SECONDS) =>
            {
                let claim_token = new_idempotency_claim_token();
                transaction.execute(
                    "UPDATE idempotency_records
                     SET claim_token = ?5, claimed_at = ?6
                     WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
                       AND idempotency_key = ?4 AND status = 'in_progress'",
                    params![
                        claim.user_id,
                        claim.method,
                        claim.resource_scope,
                        claim.key,
                        claim_token,
                        claim.now,
                    ],
                )?;
                DurableIdempotencyClaim::Claimed(IdempotencyClaimLease {
                    operation_id,
                    operation_created_at,
                    claim_token,
                    serialization_key,
                })
            }
            Some((_, operation_id, operation_created_at, _, _, _, _, _, _)) => {
                DurableIdempotencyClaim::InProgress {
                    operation_id,
                    operation_created_at,
                }
            }
        };
        transaction.commit()?;
        Ok(result)
    }

    /// Alias kept intentionally descriptive for callers that treat the row
    /// as a durable operation journal rather than an HTTP key record.
    #[allow(dead_code)]
    pub fn claim_durable_idempotency(
        &self,
        claim: NewDurableIdempotencyClaim<'_>,
    ) -> CatalogResult<DurableIdempotencyClaim> {
        self.claim_operation(claim)
    }

    pub fn claim_idempotency_with_serialization(
        &self,
        claim: NewIdempotencyClaim<'_>,
        serialization_key: Option<&str>,
    ) -> CatalogResult<DurableIdempotencyClaim> {
        self.claim_operation(NewDurableIdempotencyClaim {
            user_id: claim.user_id,
            method: claim.method,
            resource_scope: claim.resource_scope,
            key: claim.key,
            request_hash: claim.request_hash,
            serialization_key,
            now: claim.now,
            expires_at: claim.expires_at,
        })
    }

    pub fn claim_idempotency(
        &self,
        claim: NewIdempotencyClaim<'_>,
    ) -> CatalogResult<IdempotencyClaim> {
        Ok(
            match self.claim_idempotency_with_serialization(claim, None)? {
                DurableIdempotencyClaim::Claimed(lease) => IdempotencyClaim::Claimed {
                    claim_token: lease.claim_token,
                },
                DurableIdempotencyClaim::InProgress { .. }
                | DurableIdempotencyClaim::Blocked { .. } => IdempotencyClaim::InProgress,
                DurableIdempotencyClaim::Replay {
                    status_code,
                    response_json,
                    ..
                } => IdempotencyClaim::Replay {
                    status_code,
                    response_json,
                },
                DurableIdempotencyClaim::Reused => IdempotencyClaim::Reused,
            },
        )
    }

    fn commit_idempotent<R, P, M, Q>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        mutate: M,
        project: Q,
    ) -> CatalogResult<IdempotentCommit<R, P>>
    where
        P: Serialize,
        M: for<'tx> FnOnce(&Transaction<'tx>) -> CatalogResult<R>,
        Q: for<'tx> FnOnce(&Transaction<'tx>, &R) -> CatalogResult<P>,
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation_exists: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM idempotency_records
                WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
                  AND idempotency_key = ?4 AND operation_id = ?5
                  AND operation_created_at = ?6 AND claim_token = ?7
                  AND status = 'in_progress'
            )",
            params![
                completion.user_id,
                completion.method,
                completion.resource_scope,
                completion.key,
                completion.operation_id,
                completion.operation_created_at,
                completion.claim_token,
            ],
            |row| row.get(0),
        )?;
        if !operation_exists {
            return Err(CatalogError::NotFound);
        }
        let resource = mutate(&transaction)?;
        let projection = project(&transaction, &resource)?;
        let response_json = serde_json::to_string(&projection)?;
        let changed = transaction.execute(
            "UPDATE idempotency_records
             SET status = 'completed', status_code = ?8, response_json = ?9,
                 serialization_key = NULL
             WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
               AND idempotency_key = ?4 AND operation_id = ?5
               AND operation_created_at = ?6 AND claim_token = ?7
               AND status = 'in_progress'",
            params![
                completion.user_id,
                completion.method,
                completion.resource_scope,
                completion.key,
                completion.operation_id,
                completion.operation_created_at,
                completion.claim_token,
                completion.status_code,
                response_json,
            ],
        )?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
        transaction.commit()?;
        Ok(IdempotentCommit {
            resource,
            projection,
            response_json,
        })
    }

    fn commit_idempotent_result<R, P, M, Q>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        mutate: M,
        project: Q,
    ) -> CatalogResult<IdempotentCommit<R, P>>
    where
        P: Serialize,
        M: for<'tx> FnOnce(&Transaction<'tx>) -> CatalogResult<R>,
        Q: for<'tx> FnOnce(&Transaction<'tx>, &R) -> CatalogResult<P>,
    {
        self.commit_idempotent(completion, mutate, project)
    }

    pub fn commit_model_profile_idempotent<P, F>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        profile: NewModelProfile<'_>,
        project: F,
    ) -> CatalogResult<IdempotentCommit<ModelProfileRecord, P>>
    where
        P: Serialize,
        F: FnOnce(&ModelProfileRecord) -> P,
    {
        self.commit_idempotent(
            completion,
            |transaction| insert_model_profile_tx(transaction, profile),
            |_, record| Ok(project(record)),
        )
    }

    pub fn commit_research_conversation_idempotent<P, F>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        conversation: NewResearchConversation<'_>,
        project: F,
    ) -> CatalogResult<IdempotentCommit<ResearchConversationRecord, P>>
    where
        P: Serialize,
        F: FnOnce(&ResearchConversationRecord) -> P,
    {
        self.commit_idempotent(
            completion,
            |transaction| insert_research_conversation_tx(transaction, conversation),
            |_, record| Ok(project(record)),
        )
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn commit_research_turn_idempotent<P, F>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        turn: NewResearchTurn<'_>,
        project: F,
    ) -> CatalogResult<IdempotentCommit<ResearchTurnRecord, P>>
    where
        P: Serialize,
        F: FnOnce(&ResearchTurnRecord) -> P,
    {
        self.commit_idempotent(
            completion,
            |transaction| insert_research_turn_tx(transaction, turn),
            |_, record| Ok(project(record)),
        )
    }

    pub fn commit_research_turn_idempotent_result<P, F>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        turn: NewResearchTurn<'_>,
        project: F,
    ) -> CatalogResult<IdempotentCommit<ResearchTurnRecord, P>>
    where
        P: Serialize,
        F: FnOnce(&ResearchTurnRecord) -> CatalogResult<P>,
    {
        self.commit_idempotent_result(
            completion,
            |transaction| insert_research_turn_tx(transaction, turn),
            |_, record| project(record),
        )
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn commit_research_turn_status_idempotent<P, F>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        turn_id: &str,
        status: ResearchTurnStatus,
        run_id: Option<&str>,
        answer_json: Option<&str>,
        now: i64,
        project: F,
    ) -> CatalogResult<IdempotentCommit<ResearchTurnRecord, P>>
    where
        P: Serialize,
        F: FnOnce(&ResearchTurnRecord) -> P,
    {
        self.commit_idempotent(
            completion,
            |transaction| {
                update_research_turn_status_tx(
                    transaction,
                    turn_id,
                    status,
                    run_id,
                    answer_json,
                    now,
                )
            },
            |_, record| Ok(project(record)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn commit_research_turn_status_idempotent_result<P, F>(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        turn_id: &str,
        status: ResearchTurnStatus,
        run_id: Option<&str>,
        answer_json: Option<&str>,
        now: i64,
        project: F,
    ) -> CatalogResult<IdempotentCommit<ResearchTurnRecord, P>>
    where
        P: Serialize,
        F: FnOnce(&ResearchTurnRecord) -> CatalogResult<P>,
    {
        self.commit_idempotent_result(
            completion,
            |transaction| {
                update_research_turn_status_tx(
                    transaction,
                    turn_id,
                    status,
                    run_id,
                    answer_json,
                    now,
                )
            },
            |_, record| project(record),
        )
    }

    pub fn complete_idempotency(&self, completion: CompleteIdempotency<'_>) -> CatalogResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (operation_id, operation_created_at): (String, i64) = transaction
            .query_row(
                "SELECT operation_id, operation_created_at
                 FROM idempotency_records
                 WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
                   AND idempotency_key = ?4 AND claim_token = ?5
                   AND status = 'in_progress'",
                params![
                    completion.user_id,
                    completion.method,
                    completion.resource_scope,
                    completion.key,
                    completion.claim_token,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(CatalogError::NotFound)?;
        let changed = transaction.execute(
            "UPDATE idempotency_records
             SET status = 'completed', status_code = ?8, response_json = ?9,
                 serialization_key = NULL
             WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
               AND idempotency_key = ?4 AND operation_id = ?5
               AND operation_created_at = ?6 AND claim_token = ?7
               AND status = 'in_progress'",
            params![
                completion.user_id,
                completion.method,
                completion.resource_scope,
                completion.key,
                operation_id,
                operation_created_at,
                completion.claim_token,
                completion.status_code,
                completion.response_json,
            ],
        )?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
        transaction.commit()?;
        Ok(())
    }

    /// Completes a durable claim without a resource mutation. This is used
    /// for deterministic validation/conflict responses after a claim has
    /// already been acquired; resource-producing success paths should use
    /// `commit_idempotent` so mutation and response fencing share one tx.
    pub fn complete_durable_idempotency(
        &self,
        completion: DurableIdempotencyCompletion<'_>,
        response_json: &str,
    ) -> CatalogResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE idempotency_records
             SET status = 'completed', status_code = ?8, response_json = ?9,
                 serialization_key = NULL
             WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
               AND idempotency_key = ?4 AND operation_id = ?5
               AND operation_created_at = ?6 AND claim_token = ?7
               AND status = 'in_progress'",
            params![
                completion.user_id,
                completion.method,
                completion.resource_scope,
                completion.key,
                completion.operation_id,
                completion.operation_created_at,
                completion.claim_token,
                completion.status_code,
                response_json,
            ],
        )?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
        transaction.commit()?;
        Ok(())
    }

    /// Kept for source compatibility, but intentionally does not delete an
    /// open operation. HTTP Drop guards must never erase evidence after a
    /// Runtime side effect has committed.
    pub fn abandon_idempotency(
        &self,
        _user_id: &str,
        _method: &str,
        _resource_scope: &str,
        _key: &str,
        _claim_token: &str,
    ) -> CatalogResult<()> {
        Ok(())
    }

    /// Explicit test/administrative helper. Production request cleanup must
    /// use reconciliation, not this destructive escape hatch.
    #[allow(dead_code)]
    pub fn release_idempotency_for_test(
        &self,
        user_id: &str,
        method: &str,
        resource_scope: &str,
        key: &str,
        claim_token: &str,
    ) -> CatalogResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM idempotency_records
             WHERE user_id = ?1 AND method = ?2 AND resource_scope = ?3
               AND idempotency_key = ?4 AND claim_token = ?5
               AND status = 'in_progress'",
            params![user_id, method, resource_scope, key, claim_token],
        )?;
        Ok(())
    }

    pub fn create_research_turn(
        &self,
        turn: NewResearchTurn<'_>,
    ) -> CatalogResult<ResearchTurnRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created = insert_research_turn_tx(&transaction, turn)?;
        transaction.commit()?;
        Ok(created)
    }

    pub fn list_research_turns(
        &self,
        conversation_id: &str,
    ) -> CatalogResult<Vec<ResearchTurnRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "{} WHERE research_turns.conversation_id = ?1 ORDER BY research_turns.turn_number",
            research_turn_select()
        ))?;
        let rows = statement.query_map([conversation_id], map_research_turn)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn automatic_execution_recovery_candidates(
        &self,
    ) -> CatalogResult<Vec<AutomaticExecutionRecoveryCandidate>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT research_turns.turn_id, research_turns.conversation_id,
                    research_turns.turn_number, research_turns.clarification_id,
                    research_turns.run_id, research_turns.user_question, research_turns.status,
                    research_turns.answer_style, research_turns.model_profile_id,
                    research_turns.model_profile_revision,
                    research_turns.model_api_base_url, research_turns.model_id,
                    research_turns.answer_json, research_turns.created_at,
                    research_turns.updated_at, research_turns.completed_at,
                    research_conversations.user_id
             FROM research_turns
             JOIN research_conversations
               ON research_conversations.conversation_id = research_turns.conversation_id
             WHERE research_conversations.archived_at IS NULL
               AND research_turns.status IN ('ready', 'running')
             ORDER BY research_turns.created_at, research_turns.turn_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AutomaticExecutionRecoveryCandidate {
                user_id: row.get(16)?,
                turn: map_research_turn(row)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn owned_research_turn(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn_id: &str,
    ) -> CatalogResult<ResearchTurnRecord> {
        let connection = self.connection()?;
        connection
            .query_row(
                &format!(
                    "{} JOIN research_conversations c ON c.conversation_id = research_turns.conversation_id
                     WHERE c.user_id = ?1 AND c.archived_at IS NULL
                       AND research_turns.conversation_id = ?2
                       AND research_turns.turn_id = ?3",
                    research_turn_select()
                ),
                params![user_id, conversation_id, turn_id],
                map_research_turn,
            )
            .optional()?
            .ok_or(CatalogError::NotFound)
    }

    pub fn update_research_turn_status(
        &self,
        turn_id: &str,
        status: ResearchTurnStatus,
        run_id: Option<&str>,
        answer_json: Option<&str>,
        now: i64,
    ) -> CatalogResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        update_research_turn_status_tx(&transaction, turn_id, status, run_id, answer_json, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn model_profile_for_turn(
        &self,
        user_id: &str,
        turn: &ResearchTurnRecord,
    ) -> CatalogResult<ModelProfileRecord> {
        let profile = self.model_profile(user_id, &turn.model_profile_id)?;
        if profile.revision != turn.model_profile_revision {
            return Err(CatalogError::Conflict(CatalogConflict::ModelProfileChanged));
        }
        Ok(profile)
    }

    fn connection(&self) -> CatalogResult<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| CatalogError::LockPoisoned)
    }
}

fn map_user_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserAccountRecord> {
    Ok(UserAccountRecord {
        user_id: row.get(0)?,
        normalized_email: row.get(1)?,
        display_name: row.get(2)?,
        password_hash: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn new_idempotency_claim_token() -> String {
    Uuid::new_v4().simple().to_string()
}

fn new_idempotency_operation_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn insert_model_profile_tx(
    transaction: &Transaction<'_>,
    profile: NewModelProfile<'_>,
) -> CatalogResult<ModelProfileRecord> {
    let active_profile_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM model_profiles WHERE user_id = ?1 AND archived_at IS NULL",
        [profile.user_id],
        |row| row.get(0),
    )?;
    let is_default = profile.make_default || active_profile_count == 0;
    if is_default {
        transaction.execute(
            "UPDATE model_profiles SET is_default = 0, updated_at = ?2 WHERE user_id = ?1",
            params![profile.user_id, profile.now],
        )?;
    }
    transaction
        .execute(
            "INSERT INTO model_profiles (
                profile_id, user_id, display_name, api_base_url, model_id,
                api_key_ciphertext, api_key_nonce, revision, is_default,
                created_at, updated_at, verified_at, archived_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?9, NULL, NULL)",
            params![
                profile.profile_id,
                profile.user_id,
                profile.display_name,
                profile.api_base_url,
                profile.model_id,
                profile.encrypted_api_key.ciphertext,
                profile.encrypted_api_key.nonce.as_slice(),
                i64::from(is_default),
                profile.now,
            ],
        )
        .map_err(|error| map_constraint(error, CatalogConflict::ModelProfileNameAlreadyExists))?;
    transaction
        .query_row(
            "SELECT profile_id, user_id, display_name, api_base_url, model_id,
                    api_key_ciphertext, api_key_nonce, revision, is_default,
                    created_at, updated_at, verified_at
             FROM model_profiles
             WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
            params![profile.user_id, profile.profile_id],
            map_model_profile,
        )
        .map_err(Into::into)
}

fn insert_research_conversation_tx(
    transaction: &Transaction<'_>,
    conversation: NewResearchConversation<'_>,
) -> CatalogResult<ResearchConversationRecord> {
    let profile_available: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM model_profiles
            WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL
         )",
        params![conversation.user_id, conversation.model_profile_id],
        |row| row.get(0),
    )?;
    if !profile_available {
        return Err(CatalogError::NotFound);
    }
    transaction.execute(
        "INSERT INTO research_conversations (
            conversation_id, user_id, core_conversation_id, title,
            model_profile_id, created_at, updated_at, archived_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, NULL)",
        params![
            conversation.conversation_id,
            conversation.user_id,
            conversation.core_conversation_id,
            conversation.title,
            conversation.model_profile_id,
            conversation.now,
        ],
    )?;
    transaction
        .query_row(
            &conversation_select(
                "WHERE c.user_id = ?1 AND c.conversation_id = ?2 AND c.archived_at IS NULL",
            ),
            params![conversation.user_id, conversation.conversation_id],
            map_research_conversation,
        )
        .map_err(Into::into)
}

fn insert_research_turn_tx(
    transaction: &Transaction<'_>,
    turn: NewResearchTurn<'_>,
) -> CatalogResult<ResearchTurnRecord> {
    let conversation_profile_id = transaction
        .query_row(
            "SELECT model_profile_id FROM research_conversations
             WHERE user_id = ?1 AND conversation_id = ?2 AND archived_at IS NULL",
            params![turn.model_profile.user_id, turn.conversation_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(CatalogError::NotFound)?;
    if conversation_profile_id != turn.model_profile.profile_id {
        return Err(CatalogError::Conflict(
            CatalogConflict::ConversationModelProfileChanged,
        ));
    }
    let profile_revision = transaction
        .query_row(
            "SELECT revision FROM model_profiles
             WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
            params![turn.model_profile.user_id, turn.model_profile.profile_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(CatalogError::NotFound)?;
    if profile_revision != turn.model_profile.revision {
        return Err(CatalogError::Conflict(CatalogConflict::ModelProfileChanged));
    }
    if turn.status.is_nonterminal()
        && conversation_has_unfinished_turn(
            transaction,
            &turn.model_profile.user_id,
            turn.conversation_id,
        )?
    {
        return Err(CatalogError::Conflict(
            CatalogConflict::ConversationHasActiveTurn,
        ));
    }
    transaction.execute(
        "INSERT INTO research_turns (
            turn_id, conversation_id, turn_number, clarification_id, run_id,
            user_question, status, answer_style, model_profile_id, model_profile_revision,
            model_api_base_url, model_id, answer_json,
            created_at, updated_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, ?12, NULL)",
        params![
            turn.turn_id,
            turn.conversation_id,
            turn.turn_number,
            turn.clarification_id,
            turn.user_question,
            turn.status.as_str(),
            answer_style_name(turn.answer_style),
            turn.model_profile.profile_id,
            turn.model_profile.revision,
            turn.model_profile.api_base_url,
            turn.model_profile.model_id,
            turn.now,
        ],
    )?;
    transaction.execute(
        "UPDATE research_conversations
         SET title = CASE WHEN title = ?5 AND NOT EXISTS (
                SELECT 1 FROM research_turns existing
                WHERE existing.conversation_id = research_conversations.conversation_id
                  AND existing.turn_id <> ?2
             ) THEN ?3 ELSE title END,
             updated_at = ?4
         WHERE conversation_id = ?1",
        params![
            turn.conversation_id,
            turn.turn_id,
            turn.user_question,
            turn.now,
            DEFAULT_RESEARCH_CONVERSATION_TITLE,
        ],
    )?;
    transaction
        .query_row(
            &format!(
                "{} WHERE research_turns.conversation_id = ?1
                     AND research_turns.turn_id = ?2",
                research_turn_select()
            ),
            params![turn.conversation_id, turn.turn_id],
            map_research_turn,
        )
        .map_err(Into::into)
}

fn update_research_turn_status_tx(
    transaction: &Transaction<'_>,
    turn_id: &str,
    status: ResearchTurnStatus,
    run_id: Option<&str>,
    answer_json: Option<&str>,
    now: i64,
) -> CatalogResult<ResearchTurnRecord> {
    let completed_at = (status == ResearchTurnStatus::Completed).then_some(now);
    let (current_status, conversation_id) = transaction
        .query_row(
            "SELECT status, conversation_id FROM research_turns WHERE turn_id = ?1",
            [turn_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .ok_or(CatalogError::NotFound)?;
    let current_status = ResearchTurnStatus::parse(&current_status)?;
    if !current_status.can_transition_to(status) {
        return Err(CatalogError::Conflict(
            CatalogConflict::ResearchTurnStatusChanged,
        ));
    }
    transaction.execute(
        "UPDATE research_turns
         SET status = ?2,
             run_id = COALESCE(?3, run_id),
             answer_json = COALESCE(?4, answer_json),
             updated_at = ?5,
             completed_at = COALESCE(?6, completed_at)
         WHERE turn_id = ?1",
        params![
            turn_id,
            status.as_str(),
            run_id,
            answer_json,
            now,
            completed_at
        ],
    )?;
    transaction.execute(
        "UPDATE research_conversations
         SET updated_at = ?2
         WHERE conversation_id = ?1",
        params![conversation_id, now],
    )?;
    transaction
        .query_row(
            &format!(
                "{} WHERE research_turns.turn_id = ?1",
                research_turn_select()
            ),
            [turn_id],
            map_research_turn,
        )
        .map_err(Into::into)
}

fn map_model_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelProfileRecord> {
    let nonce: Vec<u8> = row.get(6)?;
    let nonce: [u8; 12] = nonce.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Blob,
            "model profile nonce must be 12 bytes".into(),
        )
    })?;
    Ok(ModelProfileRecord {
        profile_id: row.get(0)?,
        user_id: row.get(1)?,
        display_name: row.get(2)?,
        api_base_url: row.get(3)?,
        model_id: row.get(4)?,
        encrypted_api_key: EncryptedCredential {
            ciphertext: row.get(5)?,
            nonce,
        },
        revision: row.get(7)?,
        is_default: row.get::<_, i64>(8)? != 0,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        verified_at: row.get(11)?,
    })
}

fn conversation_select(suffix: &str) -> String {
    format!(
        "SELECT c.conversation_id, c.user_id, c.core_conversation_id, c.title,
                c.model_profile_id, p.display_name, c.created_at, c.updated_at,
                COUNT(t.turn_id),
                (SELECT latest.status FROM research_turns latest
                 WHERE latest.conversation_id = c.conversation_id
                 ORDER BY latest.turn_number DESC LIMIT 1)
         FROM research_conversations c
         JOIN model_profiles p ON p.profile_id = c.model_profile_id
         LEFT JOIN research_turns t ON t.conversation_id = c.conversation_id
         {suffix}
         GROUP BY c.conversation_id"
    )
}

fn map_research_conversation(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ResearchConversationRecord> {
    Ok(ResearchConversationRecord {
        conversation_id: row.get(0)?,
        user_id: row.get(1)?,
        core_conversation_id: row.get(2)?,
        title: row.get(3)?,
        model_profile_id: row.get(4)?,
        model_profile_name: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        turn_count: row.get(8)?,
        latest_turn_status: row.get(9)?,
    })
}

fn research_turn_select() -> &'static str {
    "SELECT research_turns.turn_id, research_turns.conversation_id,
            research_turns.turn_number, research_turns.clarification_id,
            research_turns.run_id, research_turns.user_question, research_turns.status,
            research_turns.answer_style, research_turns.model_profile_id,
            research_turns.model_profile_revision,
            research_turns.model_api_base_url, research_turns.model_id,
            research_turns.answer_json, research_turns.created_at,
            research_turns.updated_at, research_turns.completed_at
     FROM research_turns"
}

fn map_research_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResearchTurnRecord> {
    let status: String = row.get(6)?;
    let status = ResearchTurnStatus::parse(&status).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ResearchTurnRecord {
        turn_id: row.get(0)?,
        conversation_id: row.get(1)?,
        turn_number: row.get(2)?,
        clarification_id: row.get(3)?,
        run_id: row.get(4)?,
        user_question: row.get(5)?,
        status,
        answer_style: parse_answer_style(&row.get::<_, String>(7)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        model_profile_id: row.get(8)?,
        model_profile_revision: row.get(9)?,
        model_api_base_url: row.get(10)?,
        model_id: row.get(11)?,
        answer_json: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        completed_at: row.get(15)?,
    })
}

fn answer_style_name(style: ResearchAnswerStyle) -> &'static str {
    match style {
        ResearchAnswerStyle::WebFirst => "web_first",
        ResearchAnswerStyle::KnowledgeFirst => "knowledge_first",
    }
}

fn parse_answer_style(value: &str) -> CatalogResult<ResearchAnswerStyle> {
    match value {
        "web_first" => Ok(ResearchAnswerStyle::WebFirst),
        "knowledge_first" => Ok(ResearchAnswerStyle::KnowledgeFirst),
        _ => Err(CatalogError::InvalidData("unknown research answer style")),
    }
}

fn active_turn_uses_profile(
    connection: &Connection,
    user_id: &str,
    profile_id: &str,
) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM research_turns t
            JOIN research_conversations c ON c.conversation_id = t.conversation_id
            WHERE c.user_id = ?1 AND t.model_profile_id = ?2
              AND t.status IN ('clarifying', 'ready', 'running')
         )",
        params![user_id, profile_id],
        |row| row.get(0),
    )
}

fn conversation_has_unfinished_turn(
    connection: &Connection,
    user_id: &str,
    conversation_id: &str,
) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM research_turns t
            JOIN research_conversations c ON c.conversation_id = t.conversation_id
            WHERE c.user_id = ?1 AND c.conversation_id = ?2
              AND t.status IN ('clarifying', 'ready', 'running')
         )",
        params![user_id, conversation_id],
        |row| row.get(0),
    )
}

fn map_constraint(error: rusqlite::Error, conflict: CatalogConflict) -> CatalogError {
    match &error {
        rusqlite::Error::SqliteFailure(sqlite, _)
            if matches!(
                sqlite.extended_code,
                rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                    | rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            ) =>
        {
            CatalogError::Conflict(conflict)
        }
        _ => CatalogError::Sql(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    const USER_A_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const USER_B_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const PROFILE_A_ID: &str = "pppppppppppppppppppppppppppppppp";
    const PROFILE_B_ID: &str = "qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    const PROFILE_C_ID: &str = "rrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr";
    const CONVERSATION_A_ID: &str = "cccccccccccccccccccccccccccccccc";
    const CONVERSATION_B_ID: &str = "dddddddddddddddddddddddddddddddd";
    const CONVERSATION_C_ID: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn catalog() -> (DemoCatalog, std::path::PathBuf) {
        let path = catalog_path("current");
        (DemoCatalog::open(&path).unwrap(), path)
    }

    fn catalog_path(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "traceable-demo-catalog-{label}-{}-{unique}.sqlite",
            std::process::id()
        ))
    }

    fn encrypted(key: u8) -> EncryptedCredential {
        EncryptedCredential {
            ciphertext: vec![key; 24],
            nonce: [key; 12],
        }
    }

    fn create_legacy_catalog(version: i64) -> std::path::PathBuf {
        let path = catalog_path(&format!("v{version}"));
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(CATALOG_SCHEMA_V1).unwrap();
        if version >= 2 {
            connection.execute_batch(CATALOG_SCHEMA_V2).unwrap();
        }
        if version >= 3 {
            connection.execute_batch(CATALOG_SCHEMA_V3).unwrap();
        }
        if version >= 4 {
            connection.execute_batch(CATALOG_SCHEMA_V4).unwrap();
        }
        if version >= 5 {
            connection.execute_batch(CATALOG_SCHEMA_V5).unwrap();
        }
        if version >= 6 {
            connection.execute_batch(CATALOG_SCHEMA_V6).unwrap();
        }
        path
    }

    fn seed_legacy_research(path: &Path, answer_style: Option<&str>) {
        let connection = Connection::open(path).unwrap();
        connection.execute("PRAGMA foreign_keys = ON", []).unwrap();
        connection
            .execute(
                "INSERT INTO user_accounts (
                    user_id, normalized_email, display_name, password_hash, created_at, updated_at
                 ) VALUES (?1, 'legacy@example.com', 'Legacy', 'hash', 10, 10)",
                [USER_A_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_profiles (
                    profile_id, user_id, display_name, api_base_url, model_id,
                    api_key_ciphertext, api_key_nonce, revision, is_default,
                    created_at, updated_at, verified_at, archived_at
                 ) VALUES (?1, ?2, 'Legacy profile', 'https://example.com/v1/', 'legacy-model',
                           ?3, ?4, 2, 1, 20, 20, NULL, NULL)",
                params![PROFILE_A_ID, USER_A_ID, vec![3_u8; 24], vec![3_u8; 12]],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO research_conversations (
                    conversation_id, user_id, core_conversation_id, title,
                    model_profile_id, created_at, updated_at, archived_at
                 ) VALUES (?1, ?2, 'legacy-core', 'Legacy research', ?3, 30, 30, NULL)",
                params![CONVERSATION_A_ID, USER_A_ID, PROFILE_A_ID],
            )
            .unwrap();
        match answer_style {
            Some(answer_style) => connection
                .execute(
                    "INSERT INTO research_turns (
                        turn_id, conversation_id, turn_number, clarification_id, run_id,
                        user_question, status, answer_style, model_profile_id,
                        model_profile_revision, model_api_base_url, model_id, answer_json,
                        created_at, updated_at, completed_at
                     ) VALUES ('legacy-turn', ?1, 1, 'legacy-clarification', 'legacy-run',
                               'Legacy question', 'completed', ?2, ?3, 2,
                               'https://example.com/v1/', 'legacy-model',
                               '{\"answer\":\"Legacy answer\"}', 40, 40, 40)",
                    params![CONVERSATION_A_ID, answer_style, PROFILE_A_ID],
                )
                .unwrap(),
            None => connection
                .execute(
                    "INSERT INTO research_turns (
                        turn_id, conversation_id, turn_number, clarification_id, run_id,
                        user_question, status, model_profile_id, model_profile_revision,
                        model_api_base_url, model_id, answer_json,
                        created_at, updated_at, completed_at
                     ) VALUES ('legacy-turn', ?1, 1, 'legacy-clarification', 'legacy-run',
                               'Legacy question', 'completed', ?2, 2,
                               'https://example.com/v1/', 'legacy-model',
                               '{\"answer\":\"Legacy answer\"}', 40, 40, 40)",
                    params![CONVERSATION_A_ID, PROFILE_A_ID],
                )
                .unwrap(),
        };
    }

    #[test]
    fn fresh_database_exposes_latest_catalog_capabilities() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "fresh@example.com", "Fresh", "hash", 10)
            .unwrap();
        catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Fresh profile",
                api_base_url: "https://example.com/v1/",
                model_id: "fresh-model",
                encrypted_api_key: &encrypted(1),
                make_default: true,
                now: 20,
            })
            .unwrap();
        assert_eq!(catalog.list_model_profiles(USER_A_ID).unwrap().len(), 1);
        assert!(matches!(
            catalog
                .claim_idempotency(NewIdempotencyClaim {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "fresh-key",
                    request_hash: "fresh-hash",
                    now: 30,
                    expires_at: 90_000,
                })
                .unwrap(),
            IdempotencyClaim::Claimed { .. }
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn version_one_catalog_upgrades_to_latest_without_losing_research() {
        let path = create_legacy_catalog(1);
        seed_legacy_research(&path, None);

        let catalog = DemoCatalog::open(&path).unwrap();
        assert_eq!(
            catalog.user_account_by_id(USER_A_ID).unwrap().display_name,
            "Legacy"
        );
        assert_eq!(
            catalog
                .research_conversation(USER_A_ID, CONVERSATION_A_ID)
                .unwrap()
                .model_profile_id,
            PROFILE_A_ID
        );
        let turn = catalog
            .owned_research_turn(USER_A_ID, CONVERSATION_A_ID, "legacy-turn")
            .unwrap();
        assert_eq!(turn.answer_style, ResearchAnswerStyle::WebFirst);
        assert_eq!(
            catalog
                .model_profile_for_turn(USER_A_ID, &turn)
                .unwrap()
                .revision,
            2
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn version_two_catalog_upgrades_to_latest_and_adds_idempotency() {
        let path = create_legacy_catalog(2);
        seed_legacy_research(&path, Some("knowledge_first"));

        let catalog = DemoCatalog::open(&path).unwrap();
        let turn = catalog
            .owned_research_turn(USER_A_ID, CONVERSATION_A_ID, "legacy-turn")
            .unwrap();
        assert_eq!(turn.answer_style, ResearchAnswerStyle::KnowledgeFirst);
        assert!(matches!(
            catalog
                .claim_idempotency(NewIdempotencyClaim {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "upgraded-key",
                    request_hash: "upgraded-hash",
                    now: 50,
                    expires_at: 90_000,
                })
                .unwrap(),
            IdempotencyClaim::Claimed { .. }
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn version_three_catalog_upgrades_to_active_name_uniqueness() {
        let path = create_legacy_catalog(3);
        seed_legacy_research(&path, Some("web_first"));

        let catalog = DemoCatalog::open(&path).unwrap();
        catalog
            .archive_research_conversation(USER_A_ID, CONVERSATION_A_ID, 50)
            .unwrap();
        catalog
            .archive_model_profile(USER_A_ID, PROFILE_A_ID, 60)
            .unwrap();
        let replacement = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_B_ID,
                user_id: USER_A_ID,
                display_name: "Legacy profile",
                api_base_url: "https://example.com/v1/",
                model_id: "replacement-model",
                encrypted_api_key: &encrypted(5),
                make_default: false,
                now: 70,
            })
            .unwrap();
        assert_eq!(replacement.profile_id, PROFILE_B_ID);

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn version_four_catalog_preserves_completed_idempotency_on_upgrade() {
        let path = create_legacy_catalog(4);
        seed_legacy_research(&path, Some("web_first"));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO idempotency_records (
                    user_id, method, resource_scope, idempotency_key, request_hash,
                    status, status_code, response_json, created_at, expires_at
                 ) VALUES (?1, 'POST', 'conversations', 'legacy-key', 'legacy-hash',
                           'completed', 409, '{\"code\":\"legacy_conflict\"}', 50, 90_000)",
                [USER_A_ID],
            )
            .unwrap();
        drop(connection);

        let catalog = DemoCatalog::open(&path).unwrap();
        assert_eq!(
            catalog
                .claim_idempotency(NewIdempotencyClaim {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "legacy-key",
                    request_hash: "legacy-hash",
                    now: 60,
                    expires_at: 90_010,
                })
                .unwrap(),
            IdempotencyClaim::Replay {
                status_code: 409,
                response_json: r#"{"code":"legacy_conflict"}"#.into(),
            }
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn version_five_catalog_upgrades_to_one_active_turn_per_conversation() {
        let path = create_legacy_catalog(5);
        seed_legacy_research(&path, Some("web_first"));

        let catalog = DemoCatalog::open(&path).unwrap();
        let profile = catalog.model_profile(USER_A_ID, PROFILE_A_ID).unwrap();
        catalog
            .create_research_turn(NewResearchTurn {
                turn_id: "upgraded-active-turn",
                conversation_id: CONVERSATION_A_ID,
                turn_number: 2,
                clarification_id: "upgraded-active-clarification",
                user_question: "Follow-up",
                status: ResearchTurnStatus::Ready,
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile: &profile,
                now: 60,
            })
            .unwrap();
        assert!(matches!(
            catalog.create_research_turn(NewResearchTurn {
                turn_id: "upgraded-second-active-turn",
                conversation_id: CONVERSATION_A_ID,
                turn_number: 3,
                clarification_id: "upgraded-second-active-clarification",
                user_question: "Another follow-up",
                status: ResearchTurnStatus::Running,
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile: &profile,
                now: 70,
            }),
            Err(CatalogError::Conflict(
                CatalogConflict::ConversationHasActiveTurn,
            ))
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn latest_catalog_can_be_reopened_without_losing_data() {
        let path = catalog_path("reopen");
        let catalog = DemoCatalog::open(&path).unwrap();
        catalog
            .create_user_account(USER_A_ID, "reopen@example.com", "Reopen", "hash", 10)
            .unwrap();
        catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Persistent profile",
                api_base_url: "https://example.com/v1/",
                model_id: "persistent-model",
                encrypted_api_key: &encrypted(4),
                make_default: true,
                now: 20,
            })
            .unwrap();
        drop(catalog);

        let reopened = DemoCatalog::open(&path).unwrap();
        assert_eq!(
            reopened
                .default_model_profile(USER_A_ID)
                .unwrap()
                .profile_id,
            PROFILE_A_ID
        );
        drop(reopened);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unsupported_future_schema_version_is_rejected() {
        let path = catalog_path("future");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY NOT NULL,
                    applied_at INTEGER NOT NULL
                 ) STRICT;
                 INSERT INTO schema_migrations(version, applied_at) VALUES (8, 10);",
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            DemoCatalog::open(&path),
            Err(CatalogError::UnsupportedSchema(8))
        ));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn archived_model_profile_names_can_be_reused_but_conflict_on_restore() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(1),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .archive_model_profile(USER_A_ID, PROFILE_A_ID, 30)
            .unwrap();

        let replacement = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_B_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-b",
                encrypted_api_key: &encrypted(2),
                make_default: false,
                now: 40,
            })
            .unwrap();
        assert_eq!(replacement.profile_id, PROFILE_B_ID);
        assert!(replacement.is_default);
        assert!(matches!(
            catalog.restore_model_profile(USER_A_ID, PROFILE_A_ID, 50),
            Err(CatalogError::Conflict(
                CatalogConflict::ModelProfileNameAlreadyExists,
            ))
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn conversation_restore_distinguishes_missing_and_archived_replacement_profiles() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        catalog
            .create_user_account(USER_B_ID, "b@example.com", "B", "hash", 10)
            .unwrap();
        for (profile_id, user_id, display_name) in [
            (PROFILE_A_ID, USER_A_ID, "Original"),
            (PROFILE_B_ID, USER_B_ID, "Other account"),
            (PROFILE_C_ID, USER_A_ID, "Archived replacement"),
        ] {
            catalog
                .create_model_profile(NewModelProfile {
                    profile_id,
                    user_id,
                    display_name,
                    api_base_url: "https://example.com/v1/",
                    model_id: "model",
                    encrypted_api_key: &encrypted(2),
                    make_default: false,
                    now: 20,
                })
                .unwrap();
        }
        catalog
            .archive_model_profile(USER_A_ID, PROFILE_C_ID, 30)
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "restore-ownership",
                title: "Archived research",
                model_profile_id: PROFILE_A_ID,
                now: 40,
            })
            .unwrap();
        catalog
            .archive_research_conversation(USER_A_ID, CONVERSATION_A_ID, 50)
            .unwrap();

        assert!(matches!(
            catalog.restore_research_conversation(
                USER_A_ID,
                CONVERSATION_A_ID,
                Some("mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm"),
                60,
            ),
            Err(CatalogError::NotFound)
        ));
        assert!(matches!(
            catalog.restore_research_conversation(
                USER_A_ID,
                CONVERSATION_A_ID,
                Some(PROFILE_B_ID),
                60,
            ),
            Err(CatalogError::NotFound)
        ));
        assert!(matches!(
            catalog.restore_research_conversation(
                USER_A_ID,
                CONVERSATION_A_ID,
                Some(PROFILE_C_ID),
                60,
            ),
            Err(CatalogError::Conflict(
                CatalogConflict::ConversationModelProfileArchived,
            ))
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn conversation_writes_require_an_active_owned_model_profile() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        catalog
            .create_user_account(USER_B_ID, "b@example.com", "B", "hash", 10)
            .unwrap();
        for (profile_id, user_id, display_name) in [
            (PROFILE_A_ID, USER_A_ID, "Primary"),
            (PROFILE_B_ID, USER_B_ID, "Other account"),
            (PROFILE_C_ID, USER_A_ID, "Archived"),
        ] {
            catalog
                .create_model_profile(NewModelProfile {
                    profile_id,
                    user_id,
                    display_name,
                    api_base_url: "https://example.com/v1/",
                    model_id: "model",
                    encrypted_api_key: &encrypted(2),
                    make_default: false,
                    now: 20,
                })
                .unwrap();
        }
        catalog
            .archive_model_profile(USER_A_ID, PROFILE_C_ID, 30)
            .unwrap();
        let create_with_profile = |conversation_id, core_conversation_id, model_profile_id| {
            catalog.create_research_conversation(NewResearchConversation {
                conversation_id,
                user_id: USER_A_ID,
                core_conversation_id,
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id,
                now: 40,
            })
        };
        assert!(matches!(
            create_with_profile(CONVERSATION_B_ID, "foreign-profile", PROFILE_B_ID),
            Err(CatalogError::NotFound)
        ));
        assert!(matches!(
            create_with_profile(CONVERSATION_B_ID, "archived-profile", PROFILE_C_ID),
            Err(CatalogError::NotFound)
        ));
        assert!(matches!(
            create_with_profile(
                CONVERSATION_B_ID,
                "missing-profile",
                "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
            ),
            Err(CatalogError::NotFound)
        ));
        create_with_profile(CONVERSATION_A_ID, "valid-profile", PROFILE_A_ID).unwrap();

        for invalid_profile_id in [PROFILE_B_ID, PROFILE_C_ID] {
            assert!(matches!(
                catalog.update_research_conversation(
                    USER_A_ID,
                    CONVERSATION_A_ID,
                    "Should not persist",
                    invalid_profile_id,
                    50,
                ),
                Err(CatalogError::NotFound)
            ));
        }
        let conversation = catalog
            .research_conversation(USER_A_ID, CONVERSATION_A_ID)
            .unwrap();
        assert_eq!(conversation.model_profile_id, PROFILE_A_ID);
        assert_eq!(conversation.title, DEFAULT_RESEARCH_CONVERSATION_TITLE);

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn users_sessions_and_model_profiles_enforce_ownership() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash-a", 10)
            .unwrap();
        catalog
            .create_user_account(USER_B_ID, "b@example.com", "B", "hash-b", 10)
            .unwrap();
        catalog
            .create_login_session(&[1; 32], USER_A_ID, 10, 100)
            .unwrap();
        assert_eq!(
            catalog
                .authenticated_user(&[1; 32], 20)
                .unwrap()
                .unwrap()
                .user_id,
            USER_A_ID
        );
        assert!(catalog.authenticated_user(&[1; 32], 101).unwrap().is_none());

        catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(2),
                make_default: false,
                now: 20,
            })
            .unwrap();
        assert!(catalog.model_profile(USER_B_ID, PROFILE_A_ID).is_err());
        assert!(catalog.default_model_profile(USER_A_ID).unwrap().is_default);
        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn model_profile_updates_and_verification_reject_stale_revisions() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let original = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(1),
                make_default: true,
                now: 20,
            })
            .unwrap();
        let first_update = catalog
            .update_model_profile(UpdatedModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                expected_revision: original.revision,
                display_name: "First update",
                api_base_url: "https://example.com/v1/",
                model_id: "model-b",
                encrypted_api_key: &encrypted(2),
                now: 30,
            })
            .unwrap();

        assert!(matches!(
            catalog.update_model_profile(UpdatedModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                expected_revision: original.revision,
                display_name: "Stale update",
                api_base_url: "https://stale.example.com/v1/",
                model_id: "stale-model",
                encrypted_api_key: &encrypted(3),
                now: 40,
            }),
            Err(CatalogError::Conflict(CatalogConflict::ModelProfileChanged))
        ));
        assert!(matches!(
            catalog.mark_model_profile_verified(USER_A_ID, PROFILE_A_ID, original.revision, 40,),
            Err(CatalogError::Conflict(CatalogConflict::ModelProfileChanged))
        ));

        let persisted = catalog.model_profile(USER_A_ID, PROFILE_A_ID).unwrap();
        assert_eq!(persisted, first_update);
        assert_eq!(persisted.encrypted_api_key, encrypted(2));
        assert_eq!(persisted.verified_at, None);

        catalog
            .mark_model_profile_verified(USER_A_ID, PROFILE_A_ID, first_update.revision, 50)
            .unwrap();
        assert_eq!(
            catalog
                .model_profile(USER_A_ID, PROFILE_A_ID)
                .unwrap()
                .verified_at,
            Some(50)
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn active_turn_locks_model_profile_and_conversation_mutation() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let profile = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(3),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "session-a",
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();
        catalog
            .update_research_conversation(
                USER_A_ID,
                CONVERSATION_A_ID,
                "Custom research title",
                PROFILE_A_ID,
                35,
            )
            .unwrap();
        catalog
            .create_research_turn(NewResearchTurn {
                turn_id: "turn-a",
                conversation_id: CONVERSATION_A_ID,
                turn_number: 1,
                clarification_id: "clarification-a",
                user_question: "Question",
                status: ResearchTurnStatus::Clarifying,
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile: &profile,
                now: 40,
            })
            .unwrap();
        assert_eq!(
            catalog
                .research_conversation(USER_A_ID, CONVERSATION_A_ID)
                .unwrap()
                .title,
            "Custom research title"
        );

        let renamed = catalog
            .update_research_conversation(
                USER_A_ID,
                CONVERSATION_A_ID,
                "Renamed during research",
                PROFILE_A_ID,
                45,
            )
            .unwrap();
        assert_eq!(renamed.title, "Renamed during research");

        assert!(
            catalog
                .update_model_profile(UpdatedModelProfile {
                    profile_id: PROFILE_A_ID,
                    user_id: USER_A_ID,
                    expected_revision: profile.revision,
                    display_name: "Changed",
                    api_base_url: "https://example.com/v1/",
                    model_id: "model-b",
                    encrypted_api_key: &encrypted(4),
                    now: 50,
                })
                .is_err()
        );
        assert!(
            catalog
                .archive_research_conversation(USER_A_ID, CONVERSATION_A_ID, 50)
                .is_err()
        );
        catalog
            .update_research_turn_status(
                "turn-a",
                ResearchTurnStatus::Completed,
                Some("run-a"),
                Some(r#"{"answer":"done","claims":[]}"#),
                60,
            )
            .unwrap();
        assert!(
            catalog
                .update_model_profile(UpdatedModelProfile {
                    profile_id: PROFILE_A_ID,
                    user_id: USER_A_ID,
                    expected_revision: profile.revision,
                    display_name: "Changed",
                    api_base_url: "https://example.com/v1/",
                    model_id: "model-b",
                    encrypted_api_key: &encrypted(4),
                    now: 70,
                })
                .is_ok()
        );
        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn conversation_accepts_only_one_nonterminal_research_turn() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let profile = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model",
                encrypted_api_key: &encrypted(3),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "single-active-turn",
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();
        let new_turn = |turn_id, turn_number, status| NewResearchTurn {
            turn_id,
            conversation_id: CONVERSATION_A_ID,
            turn_number,
            clarification_id: turn_id,
            user_question: "Question",
            status,
            answer_style: ResearchAnswerStyle::WebFirst,
            model_profile: &profile,
            now: 40,
        };
        catalog
            .create_research_turn(new_turn("turn-one", 1, ResearchTurnStatus::Clarifying))
            .unwrap();
        assert!(matches!(
            catalog.create_research_turn(new_turn("turn-two", 2, ResearchTurnStatus::Ready)),
            Err(CatalogError::Conflict(
                CatalogConflict::ConversationHasActiveTurn,
            ))
        ));

        catalog
            .update_research_turn_status(
                "turn-one",
                ResearchTurnStatus::Completed,
                Some("run-one"),
                Some(r#"{"answer":"done"}"#),
                50,
            )
            .unwrap();
        assert!(
            catalog
                .create_research_turn(new_turn("turn-two", 2, ResearchTurnStatus::Ready))
                .is_ok()
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn turn_status_and_conversation_activity_commit_atomically_and_never_regress() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let profile = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model",
                encrypted_api_key: &encrypted(3),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "atomic-status",
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();
        catalog
            .create_research_turn(NewResearchTurn {
                turn_id: "turn-atomic",
                conversation_id: CONVERSATION_A_ID,
                turn_number: 1,
                clarification_id: "clarification-atomic",
                user_question: "Question",
                status: ResearchTurnStatus::Clarifying,
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile: &profile,
                now: 40,
            })
            .unwrap();

        {
            let connection = catalog.connection().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_conversation_activity_update
                     BEFORE UPDATE OF updated_at ON research_conversations
                     BEGIN
                         SELECT RAISE(ABORT, 'forced conversation update failure');
                     END;",
                )
                .unwrap();
        }
        assert!(matches!(
            catalog.update_research_turn_status(
                "turn-atomic",
                ResearchTurnStatus::Ready,
                None,
                None,
                50,
            ),
            Err(CatalogError::Sql(_))
        ));
        let turn = catalog
            .owned_research_turn(USER_A_ID, CONVERSATION_A_ID, "turn-atomic")
            .unwrap();
        assert_eq!(turn.status, ResearchTurnStatus::Clarifying);
        assert_eq!(turn.updated_at, 40);
        assert_eq!(
            catalog
                .research_conversation(USER_A_ID, CONVERSATION_A_ID)
                .unwrap()
                .updated_at,
            40
        );

        {
            let connection = catalog.connection().unwrap();
            connection
                .execute_batch("DROP TRIGGER reject_conversation_activity_update")
                .unwrap();
        }
        catalog
            .update_research_turn_status(
                "turn-atomic",
                ResearchTurnStatus::Completed,
                Some("run-atomic"),
                Some(r#"{"answer":"done"}"#),
                60,
            )
            .unwrap();
        assert!(matches!(
            catalog.update_research_turn_status(
                "turn-atomic",
                ResearchTurnStatus::Running,
                None,
                None,
                70,
            ),
            Err(CatalogError::Conflict(
                CatalogConflict::ResearchTurnStatusChanged,
            ))
        ));
        let turn = catalog
            .owned_research_turn(USER_A_ID, CONVERSATION_A_ID, "turn-atomic")
            .unwrap();
        assert_eq!(turn.status, ResearchTurnStatus::Completed);
        assert_eq!(turn.updated_at, 60);
        assert_eq!(
            catalog
                .research_conversation(USER_A_ID, CONVERSATION_A_ID)
                .unwrap()
                .updated_at,
            60
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn concurrent_catalog_connections_create_only_one_nonterminal_turn() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let profile = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model",
                encrypted_api_key: &encrypted(3),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "concurrent-turns",
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();
        drop(catalog);

        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for (turn_id, turn_number) in [("concurrent-turn-a", 1), ("concurrent-turn-b", 2)] {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let profile = profile.clone();
            handles.push(std::thread::spawn(move || {
                let catalog = DemoCatalog::open(path).unwrap();
                barrier.wait();
                catalog.create_research_turn(NewResearchTurn {
                    turn_id,
                    conversation_id: CONVERSATION_A_ID,
                    turn_number,
                    clarification_id: turn_id,
                    user_question: "Question",
                    status: ResearchTurnStatus::Clarifying,
                    answer_style: ResearchAnswerStyle::WebFirst,
                    model_profile: &profile,
                    now: 40,
                })
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    matches!(
                        result,
                        Err(CatalogError::Conflict(
                            CatalogConflict::ConversationHasActiveTurn,
                        ))
                    )
                })
                .count(),
            1
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn research_turn_creation_rejects_a_changed_conversation_model() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let original_profile = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Original",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(3),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_B_ID,
                user_id: USER_A_ID,
                display_name: "Replacement",
                api_base_url: "https://example.com/v1/",
                model_id: "model-b",
                encrypted_api_key: &encrypted(4),
                make_default: false,
                now: 20,
            })
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "changed-model",
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();
        catalog
            .update_research_conversation(
                USER_A_ID,
                CONVERSATION_A_ID,
                DEFAULT_RESEARCH_CONVERSATION_TITLE,
                PROFILE_B_ID,
                40,
            )
            .unwrap();

        assert!(matches!(
            catalog.create_research_turn(NewResearchTurn {
                turn_id: "changed-model-turn",
                conversation_id: CONVERSATION_A_ID,
                turn_number: 1,
                clarification_id: "changed-model-clarification",
                user_question: "Question",
                status: ResearchTurnStatus::Clarifying,
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile: &original_profile,
                now: 50,
            }),
            Err(CatalogError::Conflict(
                CatalogConflict::ConversationModelProfileChanged,
            ))
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn research_turn_creation_rejects_a_changed_model_profile_revision() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let stale_profile = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(3),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "changed-revision",
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();
        catalog
            .update_model_profile(UpdatedModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                expected_revision: stale_profile.revision,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-b",
                encrypted_api_key: &encrypted(4),
                now: 40,
            })
            .unwrap();

        assert!(matches!(
            catalog.create_research_turn(NewResearchTurn {
                turn_id: "changed-revision-turn",
                conversation_id: CONVERSATION_A_ID,
                turn_number: 1,
                clarification_id: "changed-revision-clarification",
                user_question: "Question",
                status: ResearchTurnStatus::Clarifying,
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile: &stale_profile,
                now: 50,
            }),
            Err(CatalogError::Conflict(CatalogConflict::ModelProfileChanged,))
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn research_turn_creation_hides_missing_foreign_and_archived_conversations() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        catalog
            .create_user_account(USER_B_ID, "b@example.com", "B", "hash", 10)
            .unwrap();
        let profile_a = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "A profile",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(3),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_B_ID,
                user_id: USER_B_ID,
                display_name: "B profile",
                api_base_url: "https://example.com/v1/",
                model_id: "model-b",
                encrypted_api_key: &encrypted(4),
                make_default: true,
                now: 20,
            })
            .unwrap();
        for (conversation_id, user_id, core_id, profile_id) in [
            (
                CONVERSATION_A_ID,
                USER_A_ID,
                "archived-conversation",
                PROFILE_A_ID,
            ),
            (
                CONVERSATION_B_ID,
                USER_B_ID,
                "foreign-conversation",
                PROFILE_B_ID,
            ),
        ] {
            catalog
                .create_research_conversation(NewResearchConversation {
                    conversation_id,
                    user_id,
                    core_conversation_id: core_id,
                    title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                    model_profile_id: profile_id,
                    now: 30,
                })
                .unwrap();
        }
        catalog
            .archive_research_conversation(USER_A_ID, CONVERSATION_A_ID, 40)
            .unwrap();

        for (conversation_id, turn_id) in [
            (CONVERSATION_A_ID, "archived-turn"),
            (CONVERSATION_B_ID, "foreign-turn"),
            (
                "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
                "missing-conversation-turn",
            ),
        ] {
            assert!(matches!(
                catalog.create_research_turn(NewResearchTurn {
                    turn_id,
                    conversation_id,
                    turn_number: 1,
                    clarification_id: turn_id,
                    user_question: "Question",
                    status: ResearchTurnStatus::Clarifying,
                    answer_style: ResearchAnswerStyle::WebFirst,
                    model_profile: &profile_a,
                    now: 50,
                }),
                Err(CatalogError::NotFound)
            ));
        }

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn check_constraint_is_not_misreported_as_duplicate_email() {
        let (catalog, path) = catalog();
        let error = catalog
            .create_user_account("short-id", "unique@example.com", "A", "hash", 10)
            .unwrap_err();
        assert!(matches!(error, CatalogError::Sql(_)));
        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn automatic_execution_recovery_only_lists_active_execution_states() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let profile = catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(7),
                make_default: true,
                now: 20,
            })
            .unwrap();
        for (conversation_id, core_conversation_id, turn_id, clarification_id, status) in [
            (
                CONVERSATION_A_ID,
                "session-recovery-ready",
                "turn-ready",
                "clarification-ready",
                ResearchTurnStatus::Ready,
            ),
            (
                CONVERSATION_B_ID,
                "session-recovery-running",
                "turn-running",
                "clarification-running",
                ResearchTurnStatus::Running,
            ),
            (
                CONVERSATION_C_ID,
                "session-recovery-completed",
                "turn-completed",
                "clarification-completed",
                ResearchTurnStatus::Completed,
            ),
        ] {
            catalog
                .create_research_conversation(NewResearchConversation {
                    conversation_id,
                    user_id: USER_A_ID,
                    core_conversation_id,
                    title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                    model_profile_id: PROFILE_A_ID,
                    now: 30,
                })
                .unwrap();
            catalog
                .create_research_turn(NewResearchTurn {
                    turn_id,
                    conversation_id,
                    turn_number: 1,
                    clarification_id,
                    user_question: "Question",
                    status,
                    answer_style: ResearchAnswerStyle::WebFirst,
                    model_profile: &profile,
                    now: 40,
                })
                .unwrap();
        }

        let candidates = catalog.automatic_execution_recovery_candidates().unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.user_id == USER_A_ID)
        );
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.turn.turn_id.as_str())
                .collect::<Vec<_>>(),
            vec!["turn-ready", "turn-running"],
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn archived_resources_are_owned_listed_and_restored_in_place() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        catalog
            .create_user_account(USER_B_ID, "b@example.com", "B", "hash", 10)
            .unwrap();
        catalog
            .create_model_profile(NewModelProfile {
                profile_id: PROFILE_A_ID,
                user_id: USER_A_ID,
                display_name: "Primary",
                api_base_url: "https://example.com/v1/",
                model_id: "model-a",
                encrypted_api_key: &encrypted(8),
                make_default: true,
                now: 20,
            })
            .unwrap();
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "session-archive",
                title: "Archived research",
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();

        catalog
            .archive_research_conversation(USER_A_ID, CONVERSATION_A_ID, 40)
            .unwrap();
        catalog
            .archive_model_profile(USER_A_ID, PROFILE_A_ID, 50)
            .unwrap();

        let profiles = catalog.list_archived_model_profiles(USER_A_ID).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.profile_id, PROFILE_A_ID);
        assert_eq!(profiles[0].archived_at, 50);
        let conversations = catalog
            .list_archived_research_conversations(USER_A_ID)
            .unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(
            conversations[0].conversation.conversation_id,
            CONVERSATION_A_ID
        );
        assert!(!conversations[0].model_profile_available);
        assert!(
            catalog
                .list_archived_model_profiles(USER_B_ID)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            catalog.restore_research_conversation(USER_A_ID, CONVERSATION_A_ID, None, 60),
            Err(CatalogError::Conflict(
                CatalogConflict::ConversationModelProfileArchived,
            ))
        ));

        let restored_profile = catalog
            .restore_model_profile(USER_A_ID, PROFILE_A_ID, 70)
            .unwrap();
        assert_eq!(restored_profile.profile_id, PROFILE_A_ID);
        assert!(restored_profile.is_default);
        let restored_conversation = catalog
            .restore_research_conversation(USER_A_ID, CONVERSATION_A_ID, None, 80)
            .unwrap();
        assert_eq!(restored_conversation.conversation_id, CONVERSATION_A_ID);
        assert_eq!(restored_conversation.model_profile_id, PROFILE_A_ID);

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn idempotency_records_claim_replay_conflict_and_expire() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();

        let first_token = match catalog
            .claim_idempotency(NewIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "request-key-1",
                request_hash: "hash-a",
                now: 20,
                expires_at: 100,
            })
            .unwrap()
        {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected claim, got {other:?}"),
        };
        assert_eq!(
            catalog
                .claim_idempotency(NewIdempotencyClaim {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "request-key-1",
                    request_hash: "hash-a",
                    now: 21,
                    expires_at: 101,
                })
                .unwrap(),
            IdempotencyClaim::InProgress
        );
        assert_eq!(
            catalog
                .claim_idempotency(NewIdempotencyClaim {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "request-key-1",
                    request_hash: "hash-b",
                    now: 21,
                    expires_at: 101,
                })
                .unwrap(),
            IdempotencyClaim::Reused
        );
        catalog
            .complete_idempotency(CompleteIdempotency {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "request-key-1",
                claim_token: &first_token,
                status_code: 200,
                response_json: r#"{"conversation_id":"c"}"#,
            })
            .unwrap();
        assert_eq!(
            catalog
                .claim_idempotency(NewIdempotencyClaim {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "request-key-1",
                    request_hash: "hash-a",
                    now: 22,
                    expires_at: 102,
                })
                .unwrap(),
            IdempotencyClaim::Replay {
                status_code: 200,
                response_json: r#"{"conversation_id":"c"}"#.into(),
            }
        );
        assert!(matches!(
            catalog
                .claim_idempotency(NewIdempotencyClaim {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "request-key-1",
                    request_hash: "hash-c",
                    now: 101,
                    expires_at: 200,
                })
                .unwrap(),
            IdempotencyClaim::Claimed { .. }
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn v6_in_progress_claims_are_blocked_after_v7_migration() {
        let path = create_legacy_catalog(6);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO user_accounts (
                    user_id, normalized_email, display_name, password_hash, created_at, updated_at
                 ) VALUES (?1, 'blocked@example.com', 'Blocked', 'hash', 10, 10)",
                [USER_A_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO idempotency_records (
                    user_id, method, resource_scope, idempotency_key, request_hash,
                    claim_token, status, created_at, expires_at
                 ) VALUES (?1, 'POST', 'conversations', 'legacy-blocked-key',
                           'same-request', '11111111111111111111111111111111',
                           'in_progress', 20, 90_000)",
                [USER_A_ID],
            )
            .unwrap();
        drop(connection);

        let catalog = DemoCatalog::open(&path).unwrap();
        let claim = catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "legacy-blocked-key",
                request_hash: "same-request",
                serialization_key: None,
                now: 320,
                expires_at: 90_300,
            })
            .unwrap();
        assert!(matches!(claim, DurableIdempotencyClaim::Blocked { .. }));
        let reopened_claim = catalog
            .claim_idempotency(NewIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "legacy-blocked-key",
                request_hash: "same-request",
                now: 90_301,
                expires_at: 90_302,
            })
            .unwrap();
        assert_eq!(reopened_claim, IdempotencyClaim::InProgress);
        let different_key = catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "different-legacy-key",
                request_hash: "another-request",
                serialization_key: Some("user:conversations:active"),
                now: 90_302,
                expires_at: 90_303,
            })
            .unwrap();
        assert!(matches!(
            different_key,
            DurableIdempotencyClaim::InProgress { .. }
        ));
        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn durable_claim_keeps_operation_identity_and_serializes_other_keys() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "durable@example.com", "Durable", "hash", 10)
            .unwrap();
        let first = match catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations/one/turns",
                key: "operation-key-a",
                request_hash: "same-request",
                serialization_key: Some("conversation:one"),
                now: 20,
                expires_at: 90_000,
            })
            .unwrap()
        {
            DurableIdempotencyClaim::Claimed(lease) => lease,
            other => panic!("expected durable claim, got {other:?}"),
        };
        let blocked = catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations/one/turns",
                key: "operation-key-b",
                request_hash: "different-request",
                serialization_key: Some("conversation:one"),
                now: 21,
                expires_at: 90_001,
            })
            .unwrap();
        assert!(matches!(
            blocked,
            DurableIdempotencyClaim::InProgress { .. }
        ));

        let takeover = match catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations/one/turns",
                key: "operation-key-a",
                request_hash: "same-request",
                serialization_key: Some("conversation:one"),
                now: 320,
                expires_at: 90_320,
            })
            .unwrap()
        {
            DurableIdempotencyClaim::Claimed(lease) => lease,
            other => panic!("expected takeover claim, got {other:?}"),
        };
        assert_eq!(takeover.operation_id, first.operation_id);
        assert_eq!(takeover.operation_created_at, first.operation_created_at);
        assert_ne!(takeover.claim_token, first.claim_token);

        let committed = catalog
            .commit_idempotent(
                DurableIdempotencyCompletion {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations/one/turns",
                    key: "operation-key-a",
                    operation_id: &takeover.operation_id,
                    operation_created_at: takeover.operation_created_at,
                    claim_token: &takeover.claim_token,
                    status_code: 200,
                },
                |_| Ok("resource"),
                |_, resource| Ok(serde_json::json!({"resource": resource})),
            )
            .unwrap();
        assert_eq!(committed.response_json, r#"{"resource":"resource"}"#);

        let second = catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations/one/turns",
                key: "operation-key-b",
                request_hash: "different-request",
                serialization_key: Some("conversation:one"),
                now: 321,
                expires_at: 90_321,
            })
            .unwrap();
        assert!(matches!(second, DurableIdempotencyClaim::Claimed(_)));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn typed_idempotent_commit_supports_profile_conversation_turn_and_status() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "typed@example.com", "Typed", "hash", 10)
            .unwrap();
        let profile_claim = match catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "model-profiles",
                key: "typed-profile-key",
                request_hash: "profile-request",
                serialization_key: Some("profile:create"),
                now: 20,
                expires_at: 90_000,
            })
            .unwrap()
        {
            DurableIdempotencyClaim::Claimed(lease) => lease,
            other => panic!("expected profile claim, got {other:?}"),
        };
        let profile = catalog
            .commit_model_profile_idempotent(
                DurableIdempotencyCompletion {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "model-profiles",
                    key: "typed-profile-key",
                    operation_id: &profile_claim.operation_id,
                    operation_created_at: profile_claim.operation_created_at,
                    claim_token: &profile_claim.claim_token,
                    status_code: 200,
                },
                NewModelProfile {
                    profile_id: PROFILE_A_ID,
                    user_id: USER_A_ID,
                    display_name: "Typed profile",
                    api_base_url: "https://example.com/v1/",
                    model_id: "typed-model",
                    encrypted_api_key: &encrypted(7),
                    make_default: true,
                    now: 20,
                },
                |record| serde_json::json!({"profile_id": record.profile_id}),
            )
            .unwrap()
            .resource;

        let conversation_claim = match catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "typed-conversation-key",
                request_hash: "conversation-request",
                serialization_key: Some("conversation:create"),
                now: 30,
                expires_at: 90_000,
            })
            .unwrap()
        {
            DurableIdempotencyClaim::Claimed(lease) => lease,
            other => panic!("expected conversation claim, got {other:?}"),
        };
        let conversation = catalog
            .commit_research_conversation_idempotent(
                DurableIdempotencyCompletion {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations",
                    key: "typed-conversation-key",
                    operation_id: &conversation_claim.operation_id,
                    operation_created_at: conversation_claim.operation_created_at,
                    claim_token: &conversation_claim.claim_token,
                    status_code: 200,
                },
                NewResearchConversation {
                    conversation_id: CONVERSATION_A_ID,
                    user_id: USER_A_ID,
                    core_conversation_id: "typed-core",
                    title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                    model_profile_id: &profile.profile_id,
                    now: 30,
                },
                |record| serde_json::json!({"conversation_id": record.conversation_id}),
            )
            .unwrap()
            .resource;

        let turn_claim = match catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations/turns",
                key: "typed-turn-key",
                request_hash: "turn-request",
                serialization_key: Some("conversation:typed"),
                now: 40,
                expires_at: 90_000,
            })
            .unwrap()
        {
            DurableIdempotencyClaim::Claimed(lease) => lease,
            other => panic!("expected turn claim, got {other:?}"),
        };
        let turn = catalog
            .commit_research_turn_idempotent(
                DurableIdempotencyCompletion {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations/turns",
                    key: "typed-turn-key",
                    operation_id: &turn_claim.operation_id,
                    operation_created_at: turn_claim.operation_created_at,
                    claim_token: &turn_claim.claim_token,
                    status_code: 200,
                },
                NewResearchTurn {
                    turn_id: "tttttttttttttttttttttttttttttttt",
                    conversation_id: &conversation.conversation_id,
                    turn_number: 1,
                    clarification_id: "clarification-typed",
                    user_question: "typed question",
                    status: ResearchTurnStatus::Clarifying,
                    answer_style: ResearchAnswerStyle::WebFirst,
                    model_profile: &profile,
                    now: 40,
                },
                |record| serde_json::json!({"turn_id": record.turn_id}),
            )
            .unwrap()
            .resource;

        let status_claim = match catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations/turns/messages",
                key: "typed-status-key",
                request_hash: "status-request",
                serialization_key: Some("turn:typed"),
                now: 50,
                expires_at: 90_000,
            })
            .unwrap()
        {
            DurableIdempotencyClaim::Claimed(lease) => lease,
            other => panic!("expected status claim, got {other:?}"),
        };
        let updated = catalog
            .commit_research_turn_status_idempotent(
                DurableIdempotencyCompletion {
                    user_id: USER_A_ID,
                    method: "POST",
                    resource_scope: "conversations/turns/messages",
                    key: "typed-status-key",
                    operation_id: &status_claim.operation_id,
                    operation_created_at: status_claim.operation_created_at,
                    claim_token: &status_claim.claim_token,
                    status_code: 200,
                },
                &turn.turn_id,
                ResearchTurnStatus::Ready,
                None,
                None,
                50,
                |record| serde_json::json!({"status": record.status.as_str()}),
            )
            .unwrap()
            .resource;
        assert_eq!(updated.status, ResearchTurnStatus::Ready);
        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn stale_idempotency_claim_can_be_taken_over_after_five_minutes() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let claim = |now, expires_at| NewIdempotencyClaim {
            user_id: USER_A_ID,
            method: "POST",
            resource_scope: "conversations",
            key: "stale-key",
            request_hash: "same-request",
            now,
            expires_at,
        };

        let first_token = match catalog.claim_idempotency(claim(20, 90_000)).unwrap() {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected claim, got {other:?}"),
        };
        assert_eq!(first_token.len(), 32);
        assert_eq!(
            catalog.claim_idempotency(claim(319, 90_299)).unwrap(),
            IdempotencyClaim::InProgress
        );
        let takeover_token = match catalog.claim_idempotency(claim(320, 90_300)).unwrap() {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected takeover claim, got {other:?}"),
        };
        assert_ne!(first_token, takeover_token);
        assert_eq!(
            catalog.claim_idempotency(claim(321, 90_301)).unwrap(),
            IdempotencyClaim::InProgress
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn stale_idempotency_owner_cannot_complete_after_takeover() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let claim = |now| NewIdempotencyClaim {
            user_id: USER_A_ID,
            method: "POST",
            resource_scope: "conversations",
            key: "fenced-key",
            request_hash: "same-request",
            now,
            expires_at: now + 86_400,
        };
        let first_token = match catalog.claim_idempotency(claim(20)).unwrap() {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected first claim, got {other:?}"),
        };
        let takeover_token = match catalog.claim_idempotency(claim(320)).unwrap() {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected takeover claim, got {other:?}"),
        };
        assert_ne!(first_token, takeover_token);

        assert!(matches!(
            catalog.complete_idempotency(CompleteIdempotency {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "fenced-key",
                claim_token: &first_token,
                status_code: 200,
                response_json: r#"{"owner":"stale"}"#,
            }),
            Err(CatalogError::NotFound)
        ));
        catalog
            .complete_idempotency(CompleteIdempotency {
                user_id: USER_A_ID,
                method: "POST",
                resource_scope: "conversations",
                key: "fenced-key",
                claim_token: &takeover_token,
                status_code: 200,
                response_json: r#"{"owner":"takeover"}"#,
            })
            .unwrap();
        assert_eq!(
            catalog.claim_idempotency(claim(321)).unwrap(),
            IdempotencyClaim::Replay {
                status_code: 200,
                response_json: r#"{"owner":"takeover"}"#.into(),
            }
        );

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn stale_idempotency_owner_cannot_abandon_after_takeover() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let claim = |now| NewIdempotencyClaim {
            user_id: USER_A_ID,
            method: "POST",
            resource_scope: "conversations",
            key: "fenced-abandon-key",
            request_hash: "same-request",
            now,
            expires_at: now + 86_400,
        };
        let first_token = match catalog.claim_idempotency(claim(20)).unwrap() {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected first claim, got {other:?}"),
        };
        let takeover_token = match catalog.claim_idempotency(claim(320)).unwrap() {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected takeover claim, got {other:?}"),
        };

        catalog
            .release_idempotency_for_test(
                USER_A_ID,
                "POST",
                "conversations",
                "fenced-abandon-key",
                &first_token,
            )
            .unwrap();
        assert_eq!(
            catalog.claim_idempotency(claim(321)).unwrap(),
            IdempotencyClaim::InProgress
        );
        catalog
            .release_idempotency_for_test(
                USER_A_ID,
                "POST",
                "conversations",
                "fenced-abandon-key",
                &takeover_token,
            )
            .unwrap();
        assert!(matches!(
            catalog.claim_idempotency(claim(322)).unwrap(),
            IdempotencyClaim::Claimed { .. }
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn failed_idempotent_operation_releases_its_in_progress_claim() {
        let (catalog, path) = catalog();
        catalog
            .create_user_account(USER_A_ID, "a@example.com", "A", "hash", 10)
            .unwrap();
        let claim = |now| NewIdempotencyClaim {
            user_id: USER_A_ID,
            method: "POST",
            resource_scope: "conversations",
            key: "failed-key",
            request_hash: "same-request",
            now,
            expires_at: now + 86_400,
        };

        let claim_token = match catalog.claim_idempotency(claim(20)).unwrap() {
            IdempotencyClaim::Claimed { claim_token } => claim_token,
            other => panic!("expected claim, got {other:?}"),
        };
        catalog
            .release_idempotency_for_test(
                USER_A_ID,
                "POST",
                "conversations",
                "failed-key",
                &claim_token,
            )
            .unwrap();
        assert!(matches!(
            catalog.claim_idempotency(claim(21)).unwrap(),
            IdempotencyClaim::Claimed { .. }
        ));

        drop(catalog);
        let _ = fs::remove_file(path);
    }
}

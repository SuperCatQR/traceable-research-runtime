use std::{
    fs,
    path::Path,
    sync::{Mutex, MutexGuard},
};

use rusqlite::{Connection, OptionalExtension, params};
use traceable_search::ResearchAnswerStyle;

use crate::security::EncryptedCredential;

const CATALOG_SCHEMA_VERSION: i64 = 2;
const CATALOG_SCHEMA_V1: &str = include_str!("../../docs/database/0001-demo-catalog.sql");
const CATALOG_SCHEMA_V2: &str =
    include_str!("../../docs/database/0002-research-turn-answer-style.sql");
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
    Conflict(&'static str),
    #[error("catalog contains invalid data: {0}")]
    InvalidData(&'static str),
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
        if version != CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::UnsupportedSchema(version));
        }
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
            .map_err(|error| map_constraint(error, "email is already registered"))?;
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

    pub fn create_model_profile(
        &self,
        profile: NewModelProfile<'_>,
    ) -> CatalogResult<ModelProfileRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
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
            .map_err(|error| map_constraint(error, "model profile name already exists"))?;
        transaction.commit()?;
        drop(connection);
        self.model_profile(profile.user_id, profile.profile_id)
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
        let connection = self.connection()?;
        if active_turn_uses_profile(&connection, profile.user_id, profile.profile_id)? {
            return Err(CatalogError::Conflict(
                "finish or cancel active research turns before editing this model profile",
            ));
        }
        let changed = connection
            .execute(
                "UPDATE model_profiles
                 SET display_name = ?3, api_base_url = ?4, model_id = ?5,
                     api_key_ciphertext = ?6, api_key_nonce = ?7,
                     revision = revision + 1, updated_at = ?8, verified_at = NULL
                 WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
                params![
                    profile.user_id,
                    profile.profile_id,
                    profile.display_name,
                    profile.api_base_url,
                    profile.model_id,
                    profile.encrypted_api_key.ciphertext,
                    profile.encrypted_api_key.nonce.as_slice(),
                    profile.now,
                ],
            )
            .map_err(|error| map_constraint(error, "model profile name already exists"))?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
        drop(connection);
        self.model_profile(profile.user_id, profile.profile_id)
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
        now: i64,
    ) -> CatalogResult<()> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE model_profiles SET verified_at = ?3, updated_at = ?3
             WHERE user_id = ?1 AND profile_id = ?2 AND archived_at IS NULL",
            params![user_id, profile_id, now],
        )?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
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
                "finish or cancel active research turns before archiving this model profile",
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
                "choose another model for active conversations before archiving this profile",
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

    pub fn create_research_conversation(
        &self,
        conversation: NewResearchConversation<'_>,
    ) -> CatalogResult<ResearchConversationRecord> {
        self.model_profile(conversation.user_id, conversation.model_profile_id)?;
        let connection = self.connection()?;
        connection.execute(
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
        drop(connection);
        self.research_conversation(conversation.user_id, conversation.conversation_id)
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
        self.model_profile(user_id, model_profile_id)?;
        let connection = self.connection()?;
        if conversation_has_unfinished_turn(&connection, user_id, conversation_id)? {
            return Err(CatalogError::Conflict(
                "finish or cancel the active research turn before changing this conversation",
            ));
        }
        let changed = connection.execute(
            "UPDATE research_conversations
             SET title = ?3, model_profile_id = ?4, updated_at = ?5
             WHERE user_id = ?1 AND conversation_id = ?2 AND archived_at IS NULL",
            params![user_id, conversation_id, title, model_profile_id, now],
        )?;
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
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
                "finish or cancel the active research turn before archiving this conversation",
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

    pub fn create_research_turn(
        &self,
        turn: NewResearchTurn<'_>,
    ) -> CatalogResult<ResearchTurnRecord> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
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
        transaction.commit()?;
        drop(connection);
        self.research_turn_by_id(turn.conversation_id, turn.turn_id)
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
        let completed_at = (status == ResearchTurnStatus::Completed).then_some(now);
        let connection = self.connection()?;
        let changed = connection.execute(
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
        if changed == 0 {
            return Err(CatalogError::NotFound);
        }
        connection.execute(
            "UPDATE research_conversations
             SET updated_at = ?2
             WHERE conversation_id = (
                 SELECT conversation_id FROM research_turns WHERE turn_id = ?1
             )",
            params![turn_id, now],
        )?;
        Ok(())
    }

    pub fn model_profile_for_turn(
        &self,
        user_id: &str,
        turn: &ResearchTurnRecord,
    ) -> CatalogResult<ModelProfileRecord> {
        let profile = self.model_profile(user_id, &turn.model_profile_id)?;
        if profile.revision != turn.model_profile_revision {
            return Err(CatalogError::Conflict(
                "the model profile changed after this research turn started",
            ));
        }
        Ok(profile)
    }

    fn research_turn_by_id(
        &self,
        conversation_id: &str,
        turn_id: &str,
    ) -> CatalogResult<ResearchTurnRecord> {
        let connection = self.connection()?;
        connection
            .query_row(
                &format!(
                    "{} WHERE research_turns.conversation_id = ?1
                         AND research_turns.turn_id = ?2",
                    research_turn_select()
                ),
                params![conversation_id, turn_id],
                map_research_turn,
            )
            .optional()?
            .ok_or(CatalogError::NotFound)
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

fn map_constraint(error: rusqlite::Error, message: &'static str) -> CatalogError {
    match &error {
        rusqlite::Error::SqliteFailure(sqlite, _)
            if matches!(
                sqlite.extended_code,
                rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                    | rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            ) =>
        {
            CatalogError::Conflict(message)
        }
        _ => CatalogError::Sql(error),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    const USER_A_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const USER_B_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const PROFILE_A_ID: &str = "pppppppppppppppppppppppppppppppp";
    const CONVERSATION_A_ID: &str = "cccccccccccccccccccccccccccccccc";

    fn catalog() -> (DemoCatalog, std::path::PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "traceable-demo-catalog-{}-{unique}.sqlite",
            std::process::id()
        ));
        (DemoCatalog::open(&path).unwrap(), path)
    }

    fn encrypted(key: u8) -> EncryptedCredential {
        EncryptedCredential {
            ciphertext: vec![key; 24],
            nonce: [key; 12],
        }
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

        assert!(
            catalog
                .update_model_profile(UpdatedModelProfile {
                    profile_id: PROFILE_A_ID,
                    user_id: USER_A_ID,
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
        catalog
            .create_research_conversation(NewResearchConversation {
                conversation_id: CONVERSATION_A_ID,
                user_id: USER_A_ID,
                core_conversation_id: "session-recovery",
                title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                model_profile_id: PROFILE_A_ID,
                now: 30,
            })
            .unwrap();
        for (turn_id, turn_number, clarification_id, status) in [
            (
                "turn-ready",
                1,
                "clarification-ready",
                ResearchTurnStatus::Ready,
            ),
            (
                "turn-running",
                2,
                "clarification-running",
                ResearchTurnStatus::Running,
            ),
            (
                "turn-completed",
                3,
                "clarification-completed",
                ResearchTurnStatus::Completed,
            ),
        ] {
            catalog
                .create_research_turn(NewResearchTurn {
                    turn_id,
                    conversation_id: CONVERSATION_A_ID,
                    turn_number,
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
}

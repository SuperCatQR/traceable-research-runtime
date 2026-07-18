//! Document Research Conversation lifecycle event log and reducer.

use crate::domain::{AnswerCompositionStyle, PublicMarkdownResearchAnswer, canonical_content_hash};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::identity::{
    CommandId, DocumentResearchConversationId, DocumentResearchRequestId, PrincipalCapability,
    ResearchPrincipal, SubjectId,
};
use crate::storage::{EventStream, NewCommandCommit, NewEvent, Storage, StoredEvent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// Current conversation event schema.
pub const DOCUMENT_RESEARCH_CONVERSATION_EVENT_SCHEMA_VERSION: u32 = 1;

/// Full lifecycle status of one Document Research Request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentResearchRequestStatus {
    /// Waiting for strong-model question evaluation.
    ResearchQuestionEvaluationPending,
    /// Waiting for another user clarification message.
    AwaitingResearchQuestionClarification,
    /// The latest question evaluation failed and may be retried.
    ResearchQuestionEvaluationFailed,
    /// A Frozen Document Research Brief can be prepared for execution.
    DocumentResearchBriefReadyForExecution,
    /// The execution contract has been frozen.
    MarkdownResearchExecutionPrepared,
    /// The fixed execution is running.
    MarkdownResearchExecutionRunning,
    /// All requested answers were completed.
    DocumentResearchRequestCompleted,
    /// Preparation or execution ended in failure.
    MarkdownResearchExecutionFailed,
    /// The user cancelled the request.
    DocumentResearchRequestCancelled,
}

impl DocumentResearchRequestStatus {
    /// Whether the request can no longer advance.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::DocumentResearchRequestCompleted
                | Self::MarkdownResearchExecutionFailed
                | Self::DocumentResearchRequestCancelled
        )
    }
}

/// Replayable state of one request within a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentResearchRequestState {
    /// Stable request ID.
    pub document_research_request_id: DocumentResearchRequestId,
    /// Monotonic one-based number within the conversation.
    pub document_research_request_number: u64,
    /// Original user question.
    pub original_user_question: String,
    /// Frozen requested answer styles.
    pub requested_answer_composition_styles: Vec<AnswerCompositionStyle>,
    /// Current lifecycle status.
    pub document_research_request_status: DocumentResearchRequestStatus,
    /// Public answers retained for history/context only.
    pub public_markdown_research_answers: Vec<PublicMarkdownResearchAnswer>,
    /// Safe failure/cancellation explanation, if any.
    pub terminal_explanation: Option<String>,
}

/// Complete replayed conversation state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentResearchConversation {
    /// Stable conversation ID.
    pub document_research_conversation_id: DocumentResearchConversationId,
    /// Owning subject.
    pub owner_subject_id: SubjectId,
    /// Requests in number order.
    pub document_research_requests: Vec<DocumentResearchRequestState>,
    /// Active request, if one exists.
    pub active_document_research_request_id: Option<DocumentResearchRequestId>,
    /// Last replayed sequence.
    pub last_document_research_conversation_event_sequence_number: u64,
    /// Last event time.
    pub last_document_research_conversation_event_recorded_at: DateTime<Utc>,
}

impl DocumentResearchConversation {
    /// Returns the active request.
    #[must_use]
    pub fn active_document_research_request(&self) -> Option<&DocumentResearchRequestState> {
        self.active_document_research_request_id.as_ref().and_then(|id| {
            self.document_research_requests
                .iter()
                .find(|request| &request.document_research_request_id == id)
        })
    }

    /// Returns a request by ID.
    #[must_use]
    pub fn document_research_request(
        &self,
        request_id: &DocumentResearchRequestId,
    ) -> Option<&DocumentResearchRequestState> {
        self.document_research_requests
            .iter()
            .find(|request| &request.document_research_request_id == request_id)
    }
}

/// One versioned lifecycle fact.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "document_research_conversation_event_type", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum DocumentResearchConversationEventKind {
    /// The conversation was created.
    DocumentResearchConversationCreated {
        /// Stable conversation ID.
        document_research_conversation_id: DocumentResearchConversationId,
        /// Owning subject.
        owner_subject_id: SubjectId,
    },
    /// A new request became active.
    DocumentResearchRequestStarted {
        /// Stable request ID.
        document_research_request_id: DocumentResearchRequestId,
        /// Monotonic number.
        document_research_request_number: u64,
        /// Original question.
        original_user_question: String,
        /// Requested styles.
        requested_answer_composition_styles: Vec<AnswerCompositionStyle>,
    },
    /// A request advanced to another valid status.
    DocumentResearchRequestStatusChanged {
        /// Target request.
        document_research_request_id: DocumentResearchRequestId,
        /// New status.
        document_research_request_status: DocumentResearchRequestStatus,
        /// Safe transition explanation.
        status_change_explanation: Option<String>,
    },
    /// The request completed with public answers.
    DocumentResearchRequestCompleted {
        /// Target request.
        document_research_request_id: DocumentResearchRequestId,
        /// One public answer per requested style.
        public_markdown_research_answers: Vec<PublicMarkdownResearchAnswer>,
    },
}

impl DocumentResearchConversationEventKind {
    fn event_type(&self) -> &'static str {
        match self {
            Self::DocumentResearchConversationCreated { .. } => {
                "document_research_conversation_created"
            }
            Self::DocumentResearchRequestStarted { .. } => "document_research_request_started",
            Self::DocumentResearchRequestStatusChanged { .. } => {
                "document_research_request_status_changed"
            }
            Self::DocumentResearchRequestCompleted { .. } => "document_research_request_completed",
        }
    }
}

/// Versioned event envelope returned by replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentResearchConversationEvent {
    /// Schema version.
    pub document_research_conversation_event_schema_version: u32,
    /// Conversation ID.
    pub document_research_conversation_id: DocumentResearchConversationId,
    /// Contiguous one-based sequence.
    pub document_research_conversation_event_sequence_number: u64,
    /// UTC event time.
    pub document_research_conversation_event_recorded_at: DateTime<Utc>,
    /// Command ID.
    pub command_id: CommandId,
    /// Event payload.
    pub event_kind: DocumentResearchConversationEventKind,
}

/// Persistent append/replay Module for conversations.
#[derive(Debug, Clone)]
pub struct DocumentResearchConversationLog {
    storage: Storage,
}

impl DocumentResearchConversationLog {
    /// Opens a file-backed lifecycle log.
    #[allow(dead_code)]
    pub(crate) fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { storage: Storage::open(database_path)? })
    }

    /// Creates the Module from shared Runtime storage.
    pub(crate) fn from_storage(storage: Storage) -> Self {
        Self { storage }
    }

    /// Creates an empty conversation idempotently.
    pub async fn create_document_research_conversation(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: DocumentResearchConversationId,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<DocumentResearchConversation> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let principal = principal.clone();
        self.storage
            .run_blocking(move |storage| {
                let scope = conversation_scope(&conversation_id);
                let kind =
                    DocumentResearchConversationEventKind::DocumentResearchConversationCreated {
                        document_research_conversation_id: conversation_id.clone(),
                        owner_subject_id: principal.subject_id.clone(),
                    };
                commit_conversation_event(
                    storage,
                    &scope,
                    &principal.subject_id,
                    command_id,
                    recorded_at,
                    kind,
                )
            })
            .await
    }

    /// Starts one active request; another non-terminal request is rejected.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_document_research_request(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: DocumentResearchRequestId,
        original_user_question: impl Into<String>,
        mut requested_answer_composition_styles: Vec<AnswerCompositionStyle>,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<DocumentResearchConversation> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let question = normalize_question(original_user_question.into())?;
        requested_answer_composition_styles.sort();
        requested_answer_composition_styles.dedup();
        if requested_answer_composition_styles.is_empty()
            || requested_answer_composition_styles.len() > 2
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Lifecycle,
                "one or two answer composition styles are required",
            ));
        }
        let scope = conversation_scope(conversation_id);
        let principal = principal.clone();
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    let request_hash = canonical_content_hash(&(
                        &principal.subject_id,
                        &request_id,
                        &question,
                        &requested_answer_composition_styles,
                    ))?;
                    if let Some(existing) = transaction.read_command_commit(&scope, &command_id)? {
                        ensure_request_hash(&existing.request_hash, &request_hash)?;
                        return replay_conversation_rows(
                            transaction.read_events(EventStream::Lifecycle, &scope)?,
                            Some(&principal.subject_id),
                        );
                    }
                    let rows = transaction.read_events(EventStream::Lifecycle, &scope)?;
                    let state = replay_conversation_rows(rows, Some(&principal.subject_id))?;
                    if state.active_document_research_request_id.is_some() {
                        return Err(RuntimeError::InvalidState {
                            stage: RuntimeStage::Lifecycle,
                            message: "conversation already has a non-terminal request".to_owned(),
                        });
                    }
                    let number =
                        u64::try_from(state.document_research_requests.len()).map_err(|_| {
                            RuntimeError::Internal { message: "request number overflow".to_owned() }
                        })? + 1;
                    let kind =
                        DocumentResearchConversationEventKind::DocumentResearchRequestStarted {
                            document_research_request_id: request_id,
                            document_research_request_number: number,
                            original_user_question: question,
                            requested_answer_composition_styles,
                        };
                    append_kind(
                        transaction,
                        &scope,
                        &principal.subject_id,
                        command_id,
                        recorded_at,
                        kind,
                        request_hash,
                    )?;
                    replay_conversation_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&principal.subject_id),
                    )
                })
            })
            .await
    }

    /// Loads and fully replays a conversation owned by the principal.
    pub async fn load_document_research_conversation(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
    ) -> Result<DocumentResearchConversation> {
        let scope = conversation_scope(conversation_id);
        let owner = principal.subject_id.clone();
        self.storage
            .run_blocking(move |storage| {
                replay_conversation_rows(
                    storage.read_events(EventStream::Lifecycle, &scope)?,
                    Some(&owner),
                )
            })
            .await
    }

    /// Changes a request status after enforcing the lifecycle state machine.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn change_document_research_request_status(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        target_status: DocumentResearchRequestStatus,
        explanation: Option<String>,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<DocumentResearchConversation> {
        let scope = conversation_scope(conversation_id);
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    let request_hash = canonical_content_hash(&(
                        &owner,
                        &request_id,
                        target_status,
                        &explanation,
                    ))?;
                    if let Some(existing) =
                        transaction.read_command_commit(&scope, &command_id)?
                    {
                        ensure_request_hash(&existing.request_hash, &request_hash)?;
                        return replay_conversation_rows(
                            transaction.read_events(EventStream::Lifecycle, &scope)?,
                            Some(&owner),
                        );
                    }
                    let state = replay_conversation_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )?;
                    let request = state.document_research_request(&request_id).ok_or(
                        RuntimeError::ObjectNotAvailable {
                            stage: RuntimeStage::Lifecycle,
                        },
                    )?;
                    if request.document_research_request_status == target_status
                        && target_status.is_terminal()
                    {
                        return Ok(state);
                    }
                    validate_status_transition(
                        request.document_research_request_status,
                        target_status,
                    )?;
                    let kind = DocumentResearchConversationEventKind::DocumentResearchRequestStatusChanged {
                        document_research_request_id: request_id,
                        document_research_request_status: target_status,
                        status_change_explanation: explanation,
                    };
                    append_kind(
                        transaction,
                        &scope,
                        &owner,
                        command_id,
                        recorded_at,
                        kind,
                        request_hash,
                    )?;
                    replay_conversation_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )
                })
            })
            .await
    }

    /// Completes a request with exactly the requested answer styles.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn complete_document_research_request(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        answers: Vec<PublicMarkdownResearchAnswer>,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<DocumentResearchConversation> {
        let scope = conversation_scope(conversation_id);
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    let request_hash = canonical_content_hash(&(&owner, &request_id, &answers))?;
                    if let Some(existing) = transaction.read_command_commit(&scope, &command_id)? {
                        ensure_request_hash(&existing.request_hash, &request_hash)?;
                        return replay_conversation_rows(
                            transaction.read_events(EventStream::Lifecycle, &scope)?,
                            Some(&owner),
                        );
                    }
                    let state = replay_conversation_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )?;
                    let request = state.document_research_request(&request_id).ok_or(
                        RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle },
                    )?;
                    validate_completed_answers(request, &answers)?;
                    validate_status_transition(
                        request.document_research_request_status,
                        DocumentResearchRequestStatus::DocumentResearchRequestCompleted,
                    )?;
                    let kind =
                        DocumentResearchConversationEventKind::DocumentResearchRequestCompleted {
                            document_research_request_id: request_id,
                            public_markdown_research_answers: answers,
                        };
                    append_kind(
                        transaction,
                        &scope,
                        &owner,
                        command_id,
                        recorded_at,
                        kind,
                        request_hash,
                    )?;
                    replay_conversation_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )
                })
            })
            .await
    }
}

/// Replays already decoded conversation events through the same reducer used on append.
pub fn replay_document_research_conversation(
    events: &[DocumentResearchConversationEvent],
) -> Result<DocumentResearchConversation> {
    let mut state = None;
    for event in events {
        state = Some(reduce_document_research_conversation_event(state, event)?);
    }
    state.ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle })
}

/// Applies one event while enforcing sequence, time and lifecycle invariants.
pub fn reduce_document_research_conversation_event(
    state: Option<DocumentResearchConversation>,
    event: &DocumentResearchConversationEvent,
) -> Result<DocumentResearchConversation> {
    if event.document_research_conversation_event_schema_version
        != DOCUMENT_RESEARCH_CONVERSATION_EVENT_SCHEMA_VERSION
    {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Lifecycle,
            message: "unsupported conversation event schema version".to_owned(),
        });
    }
    match (state, &event.event_kind) {
        (
            None,
            DocumentResearchConversationEventKind::DocumentResearchConversationCreated {
                document_research_conversation_id,
                owner_subject_id,
            },
        ) if event.document_research_conversation_event_sequence_number == 1
            && document_research_conversation_id == &event.document_research_conversation_id =>
        {
            Ok(DocumentResearchConversation {
                document_research_conversation_id: document_research_conversation_id.clone(),
                owner_subject_id: owner_subject_id.clone(),
                document_research_requests: Vec::new(),
                active_document_research_request_id: None,
                last_document_research_conversation_event_sequence_number: 1,
                last_document_research_conversation_event_recorded_at: event
                    .document_research_conversation_event_recorded_at,
            })
        }
        (None, _) => Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Lifecycle,
            message: "conversation stream must start with its creation event".to_owned(),
        }),
        (Some(mut state), kind) => {
            if state.document_research_conversation_id != event.document_research_conversation_id
                || event.document_research_conversation_event_sequence_number
                    != state.last_document_research_conversation_event_sequence_number + 1
                || event.document_research_conversation_event_recorded_at
                    < state.last_document_research_conversation_event_recorded_at
            {
                return Err(RuntimeError::CorruptState {
                    stage: RuntimeStage::Lifecycle,
                    message: "conversation event identity, sequence or time is invalid".to_owned(),
                });
            }
            match kind {
                DocumentResearchConversationEventKind::DocumentResearchConversationCreated {
                    ..
                } => {
                    return Err(RuntimeError::CorruptState {
                        stage: RuntimeStage::Lifecycle,
                        message: "conversation creation event is duplicated".to_owned(),
                    });
                }
                DocumentResearchConversationEventKind::DocumentResearchRequestStarted {
                    document_research_request_id,
                    document_research_request_number,
                    original_user_question,
                    requested_answer_composition_styles,
                } => {
                    if state.active_document_research_request_id.is_some()
                        || *document_research_request_number
                            != u64::try_from(state.document_research_requests.len())
                                .unwrap_or(u64::MAX)
                                + 1
                        || state.document_research_request(document_research_request_id).is_some()
                    {
                        return Err(RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "request start violates conversation invariants".to_owned(),
                        });
                    }
                    let request = DocumentResearchRequestState {
                        document_research_request_id: document_research_request_id.clone(),
                        document_research_request_number: *document_research_request_number,
                        original_user_question: original_user_question.clone(),
                        requested_answer_composition_styles: requested_answer_composition_styles
                            .clone(),
                        document_research_request_status:
                            DocumentResearchRequestStatus::ResearchQuestionEvaluationPending,
                        public_markdown_research_answers: Vec::new(),
                        terminal_explanation: None,
                    };
                    state.active_document_research_request_id =
                        Some(document_research_request_id.clone());
                    state.document_research_requests.push(request);
                }
                DocumentResearchConversationEventKind::DocumentResearchRequestStatusChanged {
                    document_research_request_id,
                    document_research_request_status,
                    status_change_explanation,
                } => {
                    let request = state
                        .document_research_requests
                        .iter_mut()
                        .find(|request| {
                            &request.document_research_request_id == document_research_request_id
                        })
                        .ok_or_else(|| RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "status event references an unknown request".to_owned(),
                        })?;
                    validate_status_transition(
                        request.document_research_request_status,
                        *document_research_request_status,
                    )?;
                    request.document_research_request_status = *document_research_request_status;
                    if document_research_request_status.is_terminal() {
                        request.terminal_explanation = status_change_explanation.clone();
                        state.active_document_research_request_id = None;
                    }
                }
                DocumentResearchConversationEventKind::DocumentResearchRequestCompleted {
                    document_research_request_id,
                    public_markdown_research_answers,
                } => {
                    let request = state
                        .document_research_requests
                        .iter_mut()
                        .find(|request| {
                            &request.document_research_request_id == document_research_request_id
                        })
                        .ok_or_else(|| RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "completion references an unknown request".to_owned(),
                        })?;
                    validate_status_transition(
                        request.document_research_request_status,
                        DocumentResearchRequestStatus::DocumentResearchRequestCompleted,
                    )?;
                    validate_completed_answers(request, public_markdown_research_answers)?;
                    request.document_research_request_status =
                        DocumentResearchRequestStatus::DocumentResearchRequestCompleted;
                    request.public_markdown_research_answers =
                        public_markdown_research_answers.clone();
                    state.active_document_research_request_id = None;
                }
            }
            state.last_document_research_conversation_event_sequence_number =
                event.document_research_conversation_event_sequence_number;
            state.last_document_research_conversation_event_recorded_at =
                event.document_research_conversation_event_recorded_at;
            Ok(state)
        }
    }
}

fn commit_conversation_event(
    storage: &Storage,
    scope: &str,
    owner: &SubjectId,
    command_id: CommandId,
    recorded_at: DateTime<Utc>,
    kind: DocumentResearchConversationEventKind,
) -> Result<DocumentResearchConversation> {
    storage.transact(|transaction| {
        let request_hash = canonical_content_hash(&(owner, &kind))?;
        if let Some(existing) = transaction.read_command_commit(scope, &command_id)? {
            ensure_request_hash(&existing.request_hash, &request_hash)?;
            return replay_conversation_rows(
                transaction.read_events(EventStream::Lifecycle, scope)?,
                Some(owner),
            );
        }
        let rows = transaction.read_events(EventStream::Lifecycle, scope)?;
        if !rows.is_empty() {
            return Err(RuntimeError::Conflict {
                stage: RuntimeStage::Lifecycle,
                message: "conversation already exists".to_owned(),
            });
        }
        append_kind(transaction, scope, owner, command_id, recorded_at, kind, request_hash)?;
        replay_conversation_rows(
            transaction.read_events(EventStream::Lifecycle, scope)?,
            Some(owner),
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_kind(
    transaction: &mut crate::storage::StorageTransaction<'_>,
    scope: &str,
    owner: &SubjectId,
    command_id: CommandId,
    recorded_at: DateTime<Utc>,
    kind: DocumentResearchConversationEventKind,
    request_hash: String,
) -> Result<()> {
    let payload_json = serde_json::to_string(&kind)?;
    let result_json = serde_json::to_string(&serde_json::json!({ "scope": scope }))?;
    let event = NewEvent {
        scope: scope.to_owned(),
        owner_subject_id: owner.clone(),
        command_id: command_id.clone(),
        event_schema_version: DOCUMENT_RESEARCH_CONVERSATION_EVENT_SCHEMA_VERSION,
        event_type: kind.event_type().to_owned(),
        recorded_at,
        payload_json,
    };
    let command = NewCommandCommit {
        scope: scope.to_owned(),
        command_id,
        request_hash,
        result_json,
        committed_at: recorded_at,
    };
    transaction.append_events_with_command(EventStream::Lifecycle, &command, &[event])?;
    Ok(())
}

pub(crate) fn replay_conversation_rows(
    rows: Vec<StoredEvent>,
    expected_owner: Option<&SubjectId>,
) -> Result<DocumentResearchConversation> {
    if rows.is_empty() {
        return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle });
    }
    let conversation_id =
        rows[0].scope.strip_prefix("conversation:").ok_or_else(|| RuntimeError::CorruptState {
            stage: RuntimeStage::Lifecycle,
            message: "invalid conversation scope".to_owned(),
        })?;
    let conversation_id = DocumentResearchConversationId::from_value(conversation_id)?;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        if expected_owner.is_some_and(|owner| owner != &row.owner_subject_id) {
            return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle });
        }
        let kind: DocumentResearchConversationEventKind = serde_json::from_str(&row.payload_json)
            .map_err(|error| {
            RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: format!("invalid conversation event payload: {error}"),
            }
        })?;
        if row.event_type != kind.event_type() {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: "conversation event type does not match payload".to_owned(),
            });
        }
        events.push(DocumentResearchConversationEvent {
            document_research_conversation_event_schema_version: row.event_schema_version,
            document_research_conversation_id: conversation_id.clone(),
            document_research_conversation_event_sequence_number: u64::try_from(row.sequence)
                .map_err(|_| RuntimeError::CorruptState {
                    stage: RuntimeStage::Lifecycle,
                    message: "negative conversation event sequence".to_owned(),
                })?,
            document_research_conversation_event_recorded_at: row.recorded_at,
            command_id: row.command_id,
            event_kind: kind,
        });
    }
    replay_document_research_conversation(&events)
}

fn conversation_scope(conversation_id: &DocumentResearchConversationId) -> String {
    format!("conversation:{conversation_id}")
}

fn normalize_question(question: String) -> Result<String> {
    let question = question.trim();
    if question.is_empty() || question.len() > 64 * 1024 || question.contains('\0') {
        return Err(RuntimeError::validation(
            RuntimeStage::Lifecycle,
            "original user question is invalid",
        ));
    }
    Ok(question.to_owned())
}

fn ensure_request_hash(existing: &str, requested: &str) -> Result<()> {
    if existing == requested {
        Ok(())
    } else {
        Err(RuntimeError::Conflict {
            stage: RuntimeStage::Lifecycle,
            message: "command ID was already used with another request".to_owned(),
        })
    }
}

fn validate_status_transition(
    current: DocumentResearchRequestStatus,
    target: DocumentResearchRequestStatus,
) -> Result<()> {
    use DocumentResearchRequestStatus as S;
    let valid = matches!(
        (current, target),
        (S::ResearchQuestionEvaluationPending, S::AwaitingResearchQuestionClarification)
            | (S::AwaitingResearchQuestionClarification, S::ResearchQuestionEvaluationPending)
            | (S::ResearchQuestionEvaluationPending, S::ResearchQuestionEvaluationFailed)
            | (S::ResearchQuestionEvaluationFailed, S::ResearchQuestionEvaluationPending)
            | (S::ResearchQuestionEvaluationPending, S::DocumentResearchBriefReadyForExecution)
            | (S::DocumentResearchBriefReadyForExecution, S::MarkdownResearchExecutionPrepared)
            | (S::MarkdownResearchExecutionPrepared, S::MarkdownResearchExecutionRunning)
            | (S::MarkdownResearchExecutionPrepared, S::MarkdownResearchExecutionFailed)
            | (S::MarkdownResearchExecutionRunning, S::MarkdownResearchExecutionFailed)
            | (S::MarkdownResearchExecutionRunning, S::DocumentResearchRequestCompleted)
            | (S::AwaitingResearchQuestionClarification, S::DocumentResearchRequestCancelled)
            | (S::ResearchQuestionEvaluationPending, S::DocumentResearchRequestCancelled)
            | (S::ResearchQuestionEvaluationFailed, S::DocumentResearchRequestCancelled)
            | (S::DocumentResearchBriefReadyForExecution, S::DocumentResearchRequestCancelled)
            | (S::MarkdownResearchExecutionPrepared, S::DocumentResearchRequestCancelled)
            | (S::MarkdownResearchExecutionRunning, S::DocumentResearchRequestCancelled)
    );
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::InvalidState {
            stage: RuntimeStage::Lifecycle,
            message: format!("cannot transition from {current:?} to {target:?}"),
        })
    }
}

fn validate_completed_answers(
    request: &DocumentResearchRequestState,
    answers: &[PublicMarkdownResearchAnswer],
) -> Result<()> {
    let requested: BTreeSet<_> =
        request.requested_answer_composition_styles.iter().copied().collect();
    let actual: BTreeSet<_> =
        answers.iter().map(|answer| answer.source_attributed_answer_composition_style).collect();
    if requested != actual || answers.len() != actual.len() {
        return Err(RuntimeError::validation(
            RuntimeStage::Lifecycle,
            "completed answer styles do not match the frozen request",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal() -> ResearchPrincipal {
        ResearchPrincipal::new(
            SubjectId::from_value("subject-1").unwrap(),
            [PrincipalCapability::ExecuteMarkdownResearch],
        )
    }

    #[tokio::test]
    async fn enforces_one_active_request_and_monotonic_numbers() {
        let log = DocumentResearchConversationLog { storage: Storage::open_in_memory().unwrap() };
        let principal = principal();
        let conversation_id = DocumentResearchConversationId::generate();
        log.create_document_research_conversation(
            &principal,
            conversation_id.clone(),
            CommandId::generate(),
            Utc::now(),
        )
        .await
        .unwrap();
        let state = log
            .start_document_research_request(
                &principal,
                &conversation_id,
                DocumentResearchRequestId::generate(),
                "question",
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
                CommandId::generate(),
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(state.document_research_requests[0].document_research_request_number, 1);
        assert!(
            log.start_document_research_request(
                &principal,
                &conversation_id,
                DocumentResearchRequestId::generate(),
                "second",
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
                CommandId::generate(),
                Utc::now(),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn repeated_command_reuses_first_result_and_conflicting_payload_fails() {
        let log = DocumentResearchConversationLog { storage: Storage::open_in_memory().unwrap() };
        let principal = principal();
        let conversation_id = DocumentResearchConversationId::generate();
        let command = CommandId::generate();
        log.create_document_research_conversation(
            &principal,
            conversation_id.clone(),
            command.clone(),
            Utc::now(),
        )
        .await
        .unwrap();
        let repeated = log
            .create_document_research_conversation(&principal, conversation_id, command, Utc::now())
            .await
            .unwrap();
        assert_eq!(repeated.last_document_research_conversation_event_sequence_number, 1);
    }

    #[test]
    fn replay_rejects_truncated_or_out_of_order_streams() {
        let event = DocumentResearchConversationEvent {
            document_research_conversation_event_schema_version:
                DOCUMENT_RESEARCH_CONVERSATION_EVENT_SCHEMA_VERSION,
            document_research_conversation_id: DocumentResearchConversationId::generate(),
            document_research_conversation_event_sequence_number: 2,
            document_research_conversation_event_recorded_at: Utc::now(),
            command_id: CommandId::generate(),
            event_kind: DocumentResearchConversationEventKind::DocumentResearchRequestStarted {
                document_research_request_id: DocumentResearchRequestId::generate(),
                document_research_request_number: 1,
                original_user_question: "q".to_owned(),
                requested_answer_composition_styles: vec![
                    AnswerCompositionStyle::ModelKnowledgeLed,
                ],
            },
        };
        assert!(replay_document_research_conversation(&[event]).is_err());
    }
}

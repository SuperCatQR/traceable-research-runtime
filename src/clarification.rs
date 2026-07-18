//! Research Question Clarification lifecycle and closed model response validation.

use crate::conversation::DocumentResearchRequestStatus;
use crate::domain::{FrozenDocumentResearchBrief, MAX_RESEARCH_TEXT_BYTES, canonical_content_hash};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::identity::{
    CommandId, DocumentResearchConversationId, DocumentResearchRequestId, PrincipalCapability,
    ResearchPrincipal, SubjectId,
};
use crate::storage::{EventStream, NewCommandCommit, NewEvent, Storage, StoredEvent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Current clarification event schema.
pub const RESEARCH_QUESTION_CLARIFICATION_EVENT_SCHEMA_VERSION: u32 = 1;

/// Role of one clarification dialogue message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogueRole {
    /// User-supplied context.
    User,
    /// Model-generated clarification question or status.
    Assistant,
}

/// A bounded, replayable clarification message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQuestionClarificationDialogueMessage {
    /// Message role.
    pub dialogue_role: DialogueRole,
    /// Message text.
    pub dialogue_message_text: String,
    /// Clarification revision at which it was accepted.
    pub research_question_clarification_revision: u64,
}

/// Draft brief fields before content hash/freeze.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentResearchBriefDraft {
    /// Original question.
    pub original_user_question: String,
    /// Current clarified question.
    pub clarified_research_question: String,
    /// Known context.
    pub known_document_research_context: Vec<String>,
    /// Assumptions.
    pub document_research_assumptions: Vec<String>,
    /// Still unresolved ambiguities.
    pub unresolved_research_question_ambiguities: Vec<String>,
    /// Answer requirements.
    pub requested_research_answer_requirements: Vec<String>,
}

impl DocumentResearchBriefDraft {
    /// Freezes this draft into the immutable domain brief.
    pub fn freeze(self) -> Result<FrozenDocumentResearchBrief> {
        FrozenDocumentResearchBrief::freeze(
            self.original_user_question,
            self.clarified_research_question,
            self.known_document_research_context,
            self.document_research_assumptions,
            self.unresolved_research_question_ambiguities,
            self.requested_research_answer_requirements,
        )
    }
}

/// Decision returned by the strong clarification task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchQuestionClarificationDecision {
    /// Ask the user for more context.
    RequestAdditionalQuestionContext,
    /// Begin the fixed Markdown research execution.
    StartMarkdownResearchExecution,
}

/// Closed-schema strong model response for clarification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQuestionClarificationModelOutput {
    /// Revision being evaluated.
    pub research_question_clarification_revision: u64,
    /// Model decision.
    pub research_question_clarification_decision: ResearchQuestionClarificationDecision,
    /// User-facing clarification message when requested.
    pub research_question_clarification_message: Option<String>,
    /// Proposed normalized draft.
    pub document_research_brief_draft: DocumentResearchBriefDraft,
}

impl ResearchQuestionClarificationModelOutput {
    /// Validates revision, message and draft bounds without trusting model IDs.
    pub fn validate(&self, expected_revision: u64) -> Result<()> {
        if self.research_question_clarification_revision != expected_revision {
            return Err(RuntimeError::ModelResponse {
                message: "clarification response revision does not match request".to_owned(),
            });
        }
        if let Some(message) = &self.research_question_clarification_message
            && (message.trim().is_empty() || message.len() > MAX_RESEARCH_TEXT_BYTES)
        {
            return Err(RuntimeError::ModelResponse {
                message: "clarification message is empty or too long".to_owned(),
            });
        }
        if matches!(
            self.research_question_clarification_decision,
            ResearchQuestionClarificationDecision::RequestAdditionalQuestionContext
        ) && self.research_question_clarification_message.is_none()
        {
            return Err(RuntimeError::ModelResponse {
                message: "additional context decision needs a user-facing message".to_owned(),
            });
        }
        if matches!(
            self.research_question_clarification_decision,
            ResearchQuestionClarificationDecision::StartMarkdownResearchExecution
        ) && !self
            .document_research_brief_draft
            .unresolved_research_question_ambiguities
            .is_empty()
        {
            return Err(RuntimeError::ModelResponse {
                message: "execution cannot start while material ambiguities remain".to_owned(),
            });
        }
        let _ = self.document_research_brief_draft.clone().freeze()?;
        Ok(())
    }
}

/// Replayed clarification state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQuestionClarificationState {
    /// Conversation ID.
    pub document_research_conversation_id: DocumentResearchConversationId,
    /// Request ID.
    pub document_research_request_id: DocumentResearchRequestId,
    /// Owning subject.
    pub owner_subject_id: SubjectId,
    /// Original user question.
    pub original_user_question: String,
    /// Current status.
    pub research_question_clarification_status: DocumentResearchRequestStatus,
    /// Monotonic clarification revision.
    pub research_question_clarification_revision: u64,
    /// Dialogue messages.
    pub research_question_clarification_dialogue: Vec<ResearchQuestionClarificationDialogueMessage>,
    /// Latest model draft.
    pub document_research_brief_draft: Option<DocumentResearchBriefDraft>,
    /// Frozen brief after the model says execution may start.
    pub frozen_document_research_brief: Option<FrozenDocumentResearchBrief>,
    /// Safe failure explanation.
    pub failure_explanation: Option<String>,
    /// Last replayed event sequence.
    pub last_research_question_clarification_event_sequence_number: u64,
    /// Last replayed event time.
    pub last_research_question_clarification_event_recorded_at: DateTime<Utc>,
}

/// Clarification event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "research_question_clarification_event_type", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum ResearchQuestionClarificationEventKind {
    /// Starts clarification for a request.
    ResearchQuestionClarificationStarted {
        /// Conversation ID.
        document_research_conversation_id: DocumentResearchConversationId,
        /// Request ID.
        document_research_request_id: DocumentResearchRequestId,
        /// Owning subject.
        owner_subject_id: SubjectId,
        /// Original question.
        original_user_question: String,
    },
    /// User supplied more context.
    ResearchQuestionClarificationUserMessageSubmitted {
        /// Revision after the message.
        research_question_clarification_revision: u64,
        /// Message text.
        user_message_text: String,
    },
    /// Strong model asks for more context.
    ResearchQuestionClarificationAdditionalQuestionContextRequested {
        /// Revision evaluated.
        research_question_clarification_revision: u64,
        /// User-facing question.
        research_question_clarification_message: String,
        /// Draft brief.
        document_research_brief_draft: DocumentResearchBriefDraft,
    },
    /// Strong model says execution may begin.
    DocumentResearchBriefReadyForExecution {
        /// Revision evaluated.
        research_question_clarification_revision: u64,
        /// Frozen brief.
        frozen_document_research_brief: FrozenDocumentResearchBrief,
    },
    /// Model evaluation failed.
    ResearchQuestionEvaluationFailed {
        /// Revision evaluated.
        research_question_clarification_revision: u64,
        /// Safe failure explanation.
        failure_explanation: String,
    },
    /// Retry returns a failed evaluation to pending.
    ResearchQuestionEvaluationRetried {
        /// Revision after retry.
        research_question_clarification_revision: u64,
    },
    /// User cancelled clarification.
    DocumentResearchRequestCancelled {
        /// Safe explanation.
        cancellation_explanation: Option<String>,
    },
}

impl ResearchQuestionClarificationEventKind {
    fn event_type(&self) -> &'static str {
        match self {
            Self::ResearchQuestionClarificationStarted { .. } => {
                "research_question_clarification_started"
            }
            Self::ResearchQuestionClarificationUserMessageSubmitted { .. } => {
                "research_question_clarification_user_message_submitted"
            }
            Self::ResearchQuestionClarificationAdditionalQuestionContextRequested { .. } => {
                "research_question_clarification_additional_question_context_requested"
            }
            Self::DocumentResearchBriefReadyForExecution { .. } => {
                "document_research_brief_ready_for_execution"
            }
            Self::ResearchQuestionEvaluationFailed { .. } => "research_question_evaluation_failed",
            Self::ResearchQuestionEvaluationRetried { .. } => {
                "research_question_evaluation_retried"
            }
            Self::DocumentResearchRequestCancelled { .. } => "document_research_request_cancelled",
        }
    }
}

/// Versioned clarification event returned by replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQuestionClarificationEvent {
    /// Schema version.
    pub research_question_clarification_event_schema_version: u32,
    /// Conversation ID.
    pub document_research_conversation_id: DocumentResearchConversationId,
    /// Request ID.
    pub document_research_request_id: DocumentResearchRequestId,
    /// Sequence.
    pub research_question_clarification_event_sequence_number: u64,
    /// Timestamp.
    pub research_question_clarification_event_recorded_at: DateTime<Utc>,
    /// Command ID.
    pub command_id: CommandId,
    /// Event kind.
    pub event_kind: ResearchQuestionClarificationEventKind,
}

/// Persistent clarification log.
#[derive(Debug, Clone)]
pub struct ResearchQuestionClarificationLog {
    storage: Storage,
}

impl ResearchQuestionClarificationLog {
    /// Opens a file-backed clarification log.
    #[allow(dead_code)]
    pub(crate) fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { storage: Storage::open(database_path)? })
    }

    /// Creates the Module from shared Runtime storage.
    pub(crate) fn from_storage(storage: Storage) -> Self {
        Self { storage }
    }

    /// Starts clarification for one request.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: DocumentResearchConversationId,
        request_id: DocumentResearchRequestId,
        original_user_question: String,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<ResearchQuestionClarificationState> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let owner = principal.subject_id.clone();
        let scope = clarification_scope(&request_id);
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    let kind = ResearchQuestionClarificationEventKind::ResearchQuestionClarificationStarted {
                        document_research_conversation_id: conversation_id.clone(),
                        document_research_request_id: request_id.clone(),
                        owner_subject_id: owner.clone(),
                        original_user_question: original_user_question.clone(),
                    };
                    let request_hash = canonical_content_hash(&(&owner, &kind))?;
                    if let Some(existing) = transaction.read_command_commit(&scope, &command_id)? {
                        ensure_hash(&existing.request_hash, &request_hash)?;
                        return replay_rows(
                            transaction.read_events(EventStream::Lifecycle, &scope)?,
                            Some(&owner),
                        );
                    }
                    if !transaction.read_events(EventStream::Lifecycle, &scope)?.is_empty() {
                        return Err(RuntimeError::Conflict {
                            stage: RuntimeStage::Lifecycle,
                            message: "clarification already exists".to_owned(),
                        });
                    }
                    append_kind(
                        transaction,
                        &scope,
                        &owner,
                        command_id,
                        recorded_at,
                        kind,
                        request_hash,
                    )?;
                    replay_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )
                })
            })
            .await
    }

    /// Appends a natural-language user clarification message.
    pub(crate) async fn submit_user_message(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
        message: String,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<ResearchQuestionClarificationState> {
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        let scope = clarification_scope(&request_id);
        let message = normalize_message(message)?;
        let request_hash = canonical_content_hash(&(&owner, &request_id, &message))?;
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    if let Some(existing) = transaction.read_command_commit(&scope, &command_id)? {
                        ensure_hash(&existing.request_hash, &request_hash)?;
                        return replay_rows(
                            transaction.read_events(EventStream::Lifecycle, &scope)?,
                            Some(&owner),
                        );
                    }
                    let state = replay_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )?;
                    if state.research_question_clarification_status
                        != DocumentResearchRequestStatus::AwaitingResearchQuestionClarification
                    {
                        return Err(RuntimeError::InvalidState {
                            stage: RuntimeStage::Lifecycle,
                            message: "user clarification message is not currently expected".to_owned(),
                        });
                    }
                    let kind = ResearchQuestionClarificationEventKind::ResearchQuestionClarificationUserMessageSubmitted {
                        research_question_clarification_revision:
                            state.research_question_clarification_revision + 1,
                        user_message_text: message.clone(),
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
                    replay_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )
                })
            })
            .await
    }

    /// Commits a validated strong-model clarification decision.
    pub(crate) async fn apply_model_output(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
        output: ResearchQuestionClarificationModelOutput,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<ResearchQuestionClarificationState> {
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        let scope = clarification_scope(&request_id);
        let request_hash = canonical_content_hash(&(&owner, &request_id, &output))?;
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    if let Some(existing) = transaction.read_command_commit(&scope, &command_id)? {
                        ensure_hash(&existing.request_hash, &request_hash)?;
                        return replay_rows(
                            transaction.read_events(EventStream::Lifecycle, &scope)?,
                            Some(&owner),
                        );
                    }
                    let state = replay_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )?;
                    output.validate(state.research_question_clarification_revision)?;
                    if state.research_question_clarification_status
                        != DocumentResearchRequestStatus::ResearchQuestionEvaluationPending
                    {
                        return Err(RuntimeError::InvalidState {
                            stage: RuntimeStage::Lifecycle,
                            message: "clarification model output is not currently expected".to_owned(),
                        });
                    }
                    let kind = match output.research_question_clarification_decision {
                        ResearchQuestionClarificationDecision::RequestAdditionalQuestionContext => {
                            ResearchQuestionClarificationEventKind::ResearchQuestionClarificationAdditionalQuestionContextRequested {
                                research_question_clarification_revision:
                                    output.research_question_clarification_revision,
                                research_question_clarification_message: output
                                    .research_question_clarification_message
                                    .clone()
                                    .ok_or_else(|| RuntimeError::ModelResponse {
                                        message: "clarification message is missing".to_owned(),
                                    })?,
                                document_research_brief_draft: output.document_research_brief_draft,
                            }
                        }
                        ResearchQuestionClarificationDecision::StartMarkdownResearchExecution => {
                            ResearchQuestionClarificationEventKind::DocumentResearchBriefReadyForExecution {
                                research_question_clarification_revision:
                                    output.research_question_clarification_revision,
                                frozen_document_research_brief:
                                    output.document_research_brief_draft.freeze()?,
                            }
                        }
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
                    replay_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )
                })
            })
            .await
    }

    /// Records a model evaluation failure.
    pub(crate) async fn record_evaluation_failure(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
        explanation: String,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<ResearchQuestionClarificationState> {
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        let scope = clarification_scope(&request_id);
        let explanation = normalize_message(explanation)?;
        let request_hash =
            canonical_content_hash(&(&owner, &request_id, "evaluation_failure", &explanation))?;
        self.append_status_event(
            &owner,
            &request_id,
            scope,
            command_id,
            recorded_at,
            request_hash,
            move |state| ResearchQuestionClarificationEventKind::ResearchQuestionEvaluationFailed {
                research_question_clarification_revision: state
                    .research_question_clarification_revision,
                failure_explanation: explanation,
            },
        )
        .await
    }

    /// Retries a failed evaluation.
    pub(crate) async fn retry_research_question_evaluation(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<ResearchQuestionClarificationState> {
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        let scope = clarification_scope(&request_id);
        let request_hash = canonical_content_hash(&(&owner, &request_id, "evaluation_retry"))?;
        self.append_status_event(
            &owner,
            &request_id,
            scope,
            command_id,
            recorded_at,
            request_hash,
            |state| ResearchQuestionClarificationEventKind::ResearchQuestionEvaluationRetried {
                research_question_clarification_revision: state
                    .research_question_clarification_revision,
            },
        )
        .await
    }

    /// Cancels a non-terminal clarification request.
    pub(crate) async fn cancel(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
        explanation: Option<String>,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
    ) -> Result<ResearchQuestionClarificationState> {
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        let scope = clarification_scope(&request_id);
        let explanation = explanation.map(normalize_message).transpose()?;
        let request_hash = canonical_content_hash(&(&owner, &request_id, "cancel", &explanation))?;
        self.append_status_event(
            &owner,
            &request_id,
            scope,
            command_id,
            recorded_at,
            request_hash,
            move |_| ResearchQuestionClarificationEventKind::DocumentResearchRequestCancelled {
                cancellation_explanation: explanation,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn append_status_event<F>(
        &self,
        owner: &SubjectId,
        request_id: &DocumentResearchRequestId,
        scope: String,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
        request_hash: String,
        build: F,
    ) -> Result<ResearchQuestionClarificationState>
    where
        F: FnOnce(&ResearchQuestionClarificationState) -> ResearchQuestionClarificationEventKind
            + Send
            + 'static,
    {
        let owner = owner.clone();
        let request_id = request_id.clone();
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    if let Some(existing) = transaction.read_command_commit(&scope, &command_id)? {
                        ensure_hash(&existing.request_hash, &request_hash)?;
                        return replay_rows(
                            transaction.read_events(EventStream::Lifecycle, &scope)?,
                            Some(&owner),
                        );
                    }
                    let state = replay_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )?;
                    let kind = build(&state);
                    if state.document_research_request_id != request_id {
                        return Err(RuntimeError::ObjectNotAvailable {
                            stage: RuntimeStage::Lifecycle,
                        });
                    }
                    append_kind(
                        transaction,
                        &scope,
                        &owner,
                        command_id,
                        recorded_at,
                        kind,
                        request_hash,
                    )?;
                    replay_rows(
                        transaction.read_events(EventStream::Lifecycle, &scope)?,
                        Some(&owner),
                    )
                })
            })
            .await
    }

    /// Loads a clarification state through complete replay.
    pub(crate) async fn load(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
    ) -> Result<ResearchQuestionClarificationState> {
        let owner = principal.subject_id.clone();
        let scope = clarification_scope(request_id);
        self.storage
            .run_blocking(move |storage| {
                replay_rows(storage.read_events(EventStream::Lifecycle, &scope)?, Some(&owner))
            })
            .await
    }
}

/// Replays clarification events.
pub fn replay_research_question_clarification(
    events: &[ResearchQuestionClarificationEvent],
) -> Result<ResearchQuestionClarificationState> {
    let mut state = None;
    for event in events {
        state = Some(reduce_research_question_clarification_event(state, event)?);
    }
    state.ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle })
}

/// Applies one clarification event.
pub fn reduce_research_question_clarification_event(
    state: Option<ResearchQuestionClarificationState>,
    event: &ResearchQuestionClarificationEvent,
) -> Result<ResearchQuestionClarificationState> {
    if event.research_question_clarification_event_schema_version
        != RESEARCH_QUESTION_CLARIFICATION_EVENT_SCHEMA_VERSION
    {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Lifecycle,
            message: "unsupported clarification event schema version".to_owned(),
        });
    }
    match (state, &event.event_kind) {
        (
            None,
            ResearchQuestionClarificationEventKind::ResearchQuestionClarificationStarted {
                document_research_conversation_id,
                document_research_request_id,
                owner_subject_id,
                original_user_question,
            },
        ) if event.research_question_clarification_event_sequence_number == 1
            && document_research_conversation_id == &event.document_research_conversation_id
            && document_research_request_id == &event.document_research_request_id =>
        {
            Ok(ResearchQuestionClarificationState {
                document_research_conversation_id: document_research_conversation_id.clone(),
                document_research_request_id: document_research_request_id.clone(),
                owner_subject_id: owner_subject_id.clone(),
                original_user_question: original_user_question.clone(),
                research_question_clarification_status:
                    DocumentResearchRequestStatus::ResearchQuestionEvaluationPending,
                research_question_clarification_revision: 0,
                research_question_clarification_dialogue: Vec::new(),
                document_research_brief_draft: None,
                frozen_document_research_brief: None,
                failure_explanation: None,
                last_research_question_clarification_event_sequence_number: 1,
                last_research_question_clarification_event_recorded_at: event
                    .research_question_clarification_event_recorded_at,
            })
        }
        (None, _) => Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Lifecycle,
            message: "clarification stream must start with started event".to_owned(),
        }),
        (Some(mut state), kind) => {
            if event.document_research_conversation_id != state.document_research_conversation_id
                || event.document_research_request_id != state.document_research_request_id
                || event.research_question_clarification_event_sequence_number
                    != state.last_research_question_clarification_event_sequence_number + 1
                || event.research_question_clarification_event_recorded_at
                    < state.last_research_question_clarification_event_recorded_at
            {
                return Err(RuntimeError::CorruptState {
                    stage: RuntimeStage::Lifecycle,
                    message: "clarification event identity, sequence or time is invalid".to_owned(),
                });
            }
            match kind {
                ResearchQuestionClarificationEventKind::ResearchQuestionClarificationStarted { .. } => {
                    return Err(RuntimeError::CorruptState {
                        stage: RuntimeStage::Lifecycle,
                        message: "clarification started event is duplicated".to_owned(),
                    });
                }
                ResearchQuestionClarificationEventKind::ResearchQuestionClarificationUserMessageSubmitted {
                    research_question_clarification_revision,
                    user_message_text,
                } => {
                    if state.research_question_clarification_status
                        != DocumentResearchRequestStatus::AwaitingResearchQuestionClarification
                        || *research_question_clarification_revision
                            != state.research_question_clarification_revision + 1
                    {
                        return Err(RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "clarification user message violates state/revision".to_owned(),
                        });
                    }
                    state.research_question_clarification_revision =
                        *research_question_clarification_revision;
                    state.research_question_clarification_dialogue.push(
                        ResearchQuestionClarificationDialogueMessage {
                            dialogue_role: DialogueRole::User,
                            dialogue_message_text: user_message_text.clone(),
                            research_question_clarification_revision:
                                *research_question_clarification_revision,
                        },
                    );
                    state.research_question_clarification_status =
                        DocumentResearchRequestStatus::ResearchQuestionEvaluationPending;
                }
                ResearchQuestionClarificationEventKind::ResearchQuestionClarificationAdditionalQuestionContextRequested {
                    research_question_clarification_revision,
                    research_question_clarification_message,
                    document_research_brief_draft,
                } => {
                    if state.research_question_clarification_status
                        != DocumentResearchRequestStatus::ResearchQuestionEvaluationPending
                        || *research_question_clarification_revision
                            != state.research_question_clarification_revision
                    {
                        return Err(RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "additional context event violates state/revision".to_owned(),
                        });
                    }
                    state.document_research_brief_draft = Some(document_research_brief_draft.clone());
                    state.research_question_clarification_dialogue.push(
                        ResearchQuestionClarificationDialogueMessage {
                            dialogue_role: DialogueRole::Assistant,
                            dialogue_message_text: research_question_clarification_message.clone(),
                            research_question_clarification_revision:
                                *research_question_clarification_revision,
                        },
                    );
                    state.research_question_clarification_status =
                        DocumentResearchRequestStatus::AwaitingResearchQuestionClarification;
                }
                ResearchQuestionClarificationEventKind::DocumentResearchBriefReadyForExecution {
                    research_question_clarification_revision,
                    frozen_document_research_brief,
                } => {
                    if state.research_question_clarification_status
                        != DocumentResearchRequestStatus::ResearchQuestionEvaluationPending
                        || *research_question_clarification_revision
                            != state.research_question_clarification_revision
                    {
                        return Err(RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "brief-ready event violates state/revision".to_owned(),
                        });
                    }
                    frozen_document_research_brief.validate()?;
                    state.frozen_document_research_brief = Some(frozen_document_research_brief.clone());
                    state.research_question_clarification_status =
                        DocumentResearchRequestStatus::DocumentResearchBriefReadyForExecution;
                }
                ResearchQuestionClarificationEventKind::ResearchQuestionEvaluationFailed {
                    research_question_clarification_revision,
                    failure_explanation,
                } => {
                    if state.research_question_clarification_status
                        != DocumentResearchRequestStatus::ResearchQuestionEvaluationPending
                        || *research_question_clarification_revision
                            != state.research_question_clarification_revision
                    {
                        return Err(RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "evaluation failure violates state/revision".to_owned(),
                        });
                    }
                    state.failure_explanation = Some(failure_explanation.clone());
                    state.research_question_clarification_status =
                        DocumentResearchRequestStatus::ResearchQuestionEvaluationFailed;
                }
                ResearchQuestionClarificationEventKind::ResearchQuestionEvaluationRetried {
                    research_question_clarification_revision,
                } => {
                    if state.research_question_clarification_status
                        != DocumentResearchRequestStatus::ResearchQuestionEvaluationFailed
                        || *research_question_clarification_revision
                            != state.research_question_clarification_revision
                    {
                        return Err(RuntimeError::CorruptState {
                            stage: RuntimeStage::Lifecycle,
                            message: "evaluation retry violates state/revision".to_owned(),
                        });
                    }
                    state.research_question_clarification_status =
                        DocumentResearchRequestStatus::ResearchQuestionEvaluationPending;
                    state.failure_explanation = None;
                }
                ResearchQuestionClarificationEventKind::DocumentResearchRequestCancelled {
                    cancellation_explanation,
                } => {
                    if state.research_question_clarification_status.is_terminal()
                        || matches!(
                            state.research_question_clarification_status,
                            DocumentResearchRequestStatus::MarkdownResearchExecutionPrepared
                                | DocumentResearchRequestStatus::MarkdownResearchExecutionRunning
                        )
                    {
                        // Prepared/running cancellation is owned by the execution trace; this
                        // clarification stream only accepts pre-execution cancellation.
                        return Err(RuntimeError::InvalidState {
                            stage: RuntimeStage::Lifecycle,
                            message: "clarification cancellation is too late".to_owned(),
                        });
                    }
                    state.failure_explanation = cancellation_explanation.clone();
                    state.research_question_clarification_status =
                        DocumentResearchRequestStatus::DocumentResearchRequestCancelled;
                }
            }
            state.last_research_question_clarification_event_sequence_number =
                event.research_question_clarification_event_sequence_number;
            state.last_research_question_clarification_event_recorded_at =
                event.research_question_clarification_event_recorded_at;
            Ok(state)
        }
    }
}

fn append_kind(
    transaction: &mut crate::storage::StorageTransaction<'_>,
    scope: &str,
    owner: &SubjectId,
    command_id: CommandId,
    recorded_at: DateTime<Utc>,
    kind: ResearchQuestionClarificationEventKind,
    request_hash: String,
) -> Result<()> {
    let event = NewEvent {
        scope: scope.to_owned(),
        owner_subject_id: owner.clone(),
        command_id: command_id.clone(),
        event_schema_version: RESEARCH_QUESTION_CLARIFICATION_EVENT_SCHEMA_VERSION,
        event_type: kind.event_type().to_owned(),
        recorded_at,
        payload_json: serde_json::to_string(&kind)?,
    };
    let command = NewCommandCommit {
        scope: scope.to_owned(),
        command_id,
        request_hash,
        result_json: serde_json::to_string(&serde_json::json!({ "scope": scope }))?,
        committed_at: recorded_at,
    };
    transaction.append_events_with_command(EventStream::Lifecycle, &command, &[event])?;
    Ok(())
}

fn replay_rows(
    rows: Vec<StoredEvent>,
    expected_owner: Option<&SubjectId>,
) -> Result<ResearchQuestionClarificationState> {
    if rows.is_empty() {
        return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle });
    }
    let request_id_text =
        rows[0].scope.strip_prefix("clarification:").ok_or_else(|| RuntimeError::CorruptState {
            stage: RuntimeStage::Lifecycle,
            message: "invalid clarification scope".to_owned(),
        })?;
    let request_id = DocumentResearchRequestId::from_value(request_id_text)?;
    let first_kind: ResearchQuestionClarificationEventKind =
        serde_json::from_str(&rows[0].payload_json).map_err(|error| {
            RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: format!("invalid clarification start payload: {error}"),
            }
        })?;
    let (conversation_id, payload_request_id, stream_owner) = match first_kind {
        ResearchQuestionClarificationEventKind::ResearchQuestionClarificationStarted {
            document_research_conversation_id,
            document_research_request_id,
            owner_subject_id,
            ..
        } => (document_research_conversation_id, document_research_request_id, owner_subject_id),
        _ => {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: "clarification stream has no start event".to_owned(),
            });
        }
    };
    if payload_request_id != request_id
        || rows[0].owner_subject_id != stream_owner
        || expected_owner.is_some_and(|owner| owner != &stream_owner)
    {
        return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle });
    }
    let mut events = Vec::with_capacity(rows.len());
    let mut previous_recorded_at = None;
    for (expected_sequence, row) in (1_i64..).zip(rows) {
        if row.scope != format!("clarification:{request_id}")
            || row.owner_subject_id != stream_owner
            || row.sequence != expected_sequence
            || previous_recorded_at.is_some_and(|previous| row.recorded_at < previous)
        {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: "clarification storage envelope is not contiguous".to_owned(),
            });
        }
        let kind: ResearchQuestionClarificationEventKind = serde_json::from_str(&row.payload_json)
            .map_err(|error| RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: format!("invalid clarification event payload: {error}"),
            })?;
        if row.event_type != kind.event_type() {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: "clarification event type does not match payload".to_owned(),
            });
        }
        events.push(ResearchQuestionClarificationEvent {
            research_question_clarification_event_schema_version: row.event_schema_version,
            document_research_conversation_id: conversation_id.clone(),
            document_research_request_id: request_id.clone(),
            research_question_clarification_event_sequence_number: u64::try_from(row.sequence)
                .map_err(|_| RuntimeError::CorruptState {
                    stage: RuntimeStage::Lifecycle,
                    message: "negative clarification sequence".to_owned(),
                })?,
            research_question_clarification_event_recorded_at: row.recorded_at,
            command_id: row.command_id,
            event_kind: kind,
        });
        previous_recorded_at = Some(row.recorded_at);
    }
    replay_research_question_clarification(&events)
}

fn clarification_scope(request_id: &DocumentResearchRequestId) -> String {
    format!("clarification:{request_id}")
}

fn normalize_message(message: String) -> Result<String> {
    let message = message.trim();
    if message.is_empty() || message.len() > MAX_RESEARCH_TEXT_BYTES || message.contains('\0') {
        return Err(RuntimeError::validation(
            RuntimeStage::Lifecycle,
            "clarification message is invalid",
        ));
    }
    Ok(message.to_owned())
}

fn ensure_hash(existing: &str, requested: &str) -> Result<()> {
    if existing == requested {
        Ok(())
    } else {
        Err(RuntimeError::Conflict {
            stage: RuntimeStage::Lifecycle,
            message: "command ID was already used with another clarification request".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::MarkdownResearchExecutionId;

    fn principal() -> ResearchPrincipal {
        ResearchPrincipal::new(
            SubjectId::from_value("subject-1").unwrap(),
            [PrincipalCapability::ExecuteMarkdownResearch],
        )
    }

    fn draft(question: &str) -> DocumentResearchBriefDraft {
        DocumentResearchBriefDraft {
            original_user_question: question.to_owned(),
            clarified_research_question: question.to_owned(),
            known_document_research_context: Vec::new(),
            document_research_assumptions: Vec::new(),
            unresolved_research_question_ambiguities: Vec::new(),
            requested_research_answer_requirements: vec!["answer".to_owned()],
        }
    }

    #[tokio::test]
    async fn clarification_round_trip_requests_context_then_freezes_brief() {
        let log = ResearchQuestionClarificationLog { storage: Storage::open_in_memory().unwrap() };
        let principal = principal();
        let request_id = DocumentResearchRequestId::generate();
        let conversation_id = DocumentResearchConversationId::generate();
        log.start(
            &principal,
            conversation_id,
            request_id.clone(),
            "question".to_owned(),
            CommandId::generate(),
            Utc::now(),
        )
        .await
        .unwrap();
        let state = log
            .apply_model_output(
                &principal,
                &request_id,
                ResearchQuestionClarificationModelOutput {
                    research_question_clarification_revision: 0,
                    research_question_clarification_decision:
                        ResearchQuestionClarificationDecision::RequestAdditionalQuestionContext,
                    research_question_clarification_message: Some("Which region?".to_owned()),
                    document_research_brief_draft: draft("question"),
                },
                CommandId::generate(),
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(
            state.research_question_clarification_status,
            DocumentResearchRequestStatus::AwaitingResearchQuestionClarification
        );
        let state = log
            .submit_user_message(
                &principal,
                &request_id,
                "Shanghai".to_owned(),
                CommandId::generate(),
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(state.research_question_clarification_revision, 1);
        let state = log
            .apply_model_output(
                &principal,
                &request_id,
                ResearchQuestionClarificationModelOutput {
                    research_question_clarification_revision: 1,
                    research_question_clarification_decision:
                        ResearchQuestionClarificationDecision::StartMarkdownResearchExecution,
                    research_question_clarification_message: None,
                    document_research_brief_draft: draft("question in Shanghai"),
                },
                CommandId::generate(),
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(state.frozen_document_research_brief.is_some());
    }

    #[test]
    fn malformed_model_output_is_rejected() {
        let output = ResearchQuestionClarificationModelOutput {
            research_question_clarification_revision: 2,
            research_question_clarification_decision:
                ResearchQuestionClarificationDecision::RequestAdditionalQuestionContext,
            research_question_clarification_message: None,
            document_research_brief_draft: draft("q"),
        };
        assert!(output.validate(0).is_err());
        let _ = MarkdownResearchExecutionId::generate();
    }
}

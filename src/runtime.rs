//! Public application facade for the traceable Markdown research runtime.
//!
//! This is the only module that a host needs to learn. Conversation,
//! clarification, corpus, Trace, Validator and SQLite details stay behind the
//! facade; callers receive typed lifecycle states and public projections.

use crate::clarification::{ResearchQuestionClarificationLog, ResearchQuestionClarificationState};
use crate::conversation::{
    DocumentResearchConversation, DocumentResearchConversationEventKind,
    DocumentResearchConversationLog, DocumentResearchRequestStatus,
};
use crate::corpus::{PublishMarkdownCorpusSnapshotInput, VersionedMarkdownCorpus};
use crate::domain::{
    AnswerCompositionStyle, MAX_RESEARCH_TEXT_BYTES, MarkdownResearchExecutionLimits,
    MarkdownResearchExecutionOverview, PreparedMarkdownResearchExecution,
    PublicMarkdownResearchAnswer, canonical_content_hash,
};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::execution_engine::MarkdownResearchExecutionEngine;
use crate::execution_trace::{
    MARKDOWN_RESEARCH_EXECUTION_TRACE_SCHEMA_VERSION, MarkdownResearchExecutionEventKind,
    MarkdownResearchExecutionTerminalState, MarkdownResearchExecutionTrace,
    ReplayedMarkdownResearchExecution,
};
use crate::identity::{
    CommandId, DocumentResearchConversationId, DocumentResearchRequestId, MarkdownCorpusSnapshotId,
    MarkdownResearchExecutionId, MarkdownResearchModelTaskId, PrincipalCapability,
    ResearchPrincipal,
};
use crate::model_gateway::{
    MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION, MarkdownResearchModelGateway,
    ResearchQuestionEvaluationTask, StrongMarkdownResearchModelResponse,
    StrongMarkdownResearchModelTask,
};
use crate::storage::{EventStream, NewCommandCommit, NewEvent, Storage};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Weak};
use tokio::sync::Mutex as AsyncMutex;

/// The result returned after creating a request and evaluating its first draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartedDocumentResearchRequest {
    /// Conversation identity.
    pub document_research_conversation_id: DocumentResearchConversationId,
    /// Request identity.
    pub document_research_request_id: DocumentResearchRequestId,
    /// Current clarification state, including any user-facing question.
    pub clarification_state: ResearchQuestionClarificationState,
}

/// Public result of a completed or resumed execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownResearchExecutionResult {
    /// One answer for each requested composition style.
    pub public_markdown_research_answers: Vec<PublicMarkdownResearchAnswer>,
    /// Safe execution overview.
    pub markdown_research_execution_overview: MarkdownResearchExecutionOverview,
}

/// A request snapshot returned by the facade without exposing raw events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentResearchRequestSnapshot {
    /// Conversation replay projection.
    pub document_research_conversation: DocumentResearchConversation,
    /// Clarification replay projection.
    pub research_question_clarification: ResearchQuestionClarificationState,
    /// Prepared execution, when one has been created.
    pub prepared_markdown_research_execution: Option<PreparedMarkdownResearchExecution>,
}

/// Stable, host-facing Runtime facade.
pub struct TraceableMarkdownResearchRuntime {
    storage: Storage,
    corpus: VersionedMarkdownCorpus,
    conversation: DocumentResearchConversationLog,
    clarification: ResearchQuestionClarificationLog,
    trace: MarkdownResearchExecutionTrace,
    gateway: Arc<dyn MarkdownResearchModelGateway>,
    engine: MarkdownResearchExecutionEngine,
    execution_locks: Arc<AsyncMutex<BTreeMap<MarkdownResearchExecutionId, Weak<AsyncMutex<()>>>>>,
}

impl std::fmt::Debug for TraceableMarkdownResearchRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TraceableMarkdownResearchRuntime")
            .field("gateway", &"<model adapter>")
            .finish_non_exhaustive()
    }
}

impl TraceableMarkdownResearchRuntime {
    /// Opens a Runtime over a SQLite database and one model Gateway Adapter.
    pub fn open(
        database_path: impl AsRef<Path>,
        gateway: Arc<dyn MarkdownResearchModelGateway>,
    ) -> Result<Self> {
        let storage = Storage::open(database_path)?;
        Ok(Self::from_storage(storage, gateway))
    }

    /// Creates an isolated in-memory Runtime, useful for deterministic tests.
    pub fn in_memory(gateway: Arc<dyn MarkdownResearchModelGateway>) -> Result<Self> {
        Ok(Self::from_storage(Storage::open_in_memory()?, gateway))
    }

    fn from_storage(storage: Storage, gateway: Arc<dyn MarkdownResearchModelGateway>) -> Self {
        let corpus = VersionedMarkdownCorpus::from_storage(storage.clone());
        let conversation = DocumentResearchConversationLog::from_storage(storage.clone());
        let clarification = ResearchQuestionClarificationLog::from_storage(storage.clone());
        let trace = MarkdownResearchExecutionTrace::from_storage(storage.clone());
        let engine = MarkdownResearchExecutionEngine::new(gateway.clone());
        Self {
            storage,
            corpus,
            conversation,
            clarification,
            trace,
            gateway,
            engine,
            execution_locks: Arc::new(AsyncMutex::new(BTreeMap::new())),
        }
    }

    /// Publishes one immutable Markdown Corpus Snapshot.
    pub async fn publish_markdown_corpus_snapshot(
        &self,
        principal: &ResearchPrincipal,
        input: PublishMarkdownCorpusSnapshotInput,
    ) -> Result<MarkdownCorpusSnapshotId> {
        self.corpus
            .publish_markdown_corpus_snapshot(principal, input, Utc::now())
            .await
            .map(|snapshot| snapshot.markdown_corpus_snapshot_id)
    }

    /// Creates a new empty Document Research Conversation.
    pub async fn create_document_research_conversation(
        &self,
        principal: &ResearchPrincipal,
    ) -> Result<DocumentResearchConversationId> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let conversation_id = DocumentResearchConversationId::generate();
        self.conversation
            .create_document_research_conversation(
                principal,
                conversation_id.clone(),
                command_id("conversation-create", &conversation_id, "create"),
                Utc::now(),
            )
            .await?;
        Ok(conversation_id)
    }

    /// Loads a conversation through complete lifecycle replay.
    pub async fn load_document_research_conversation(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
    ) -> Result<DocumentResearchConversation> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        self.conversation.load_document_research_conversation(principal, conversation_id).await
    }

    /// Starts a request and evaluates its first clarification draft.
    pub async fn start_document_research_request(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        original_user_question: impl Into<String>,
        requested_answer_composition_styles: Vec<AnswerCompositionStyle>,
    ) -> Result<StartedDocumentResearchRequest> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let original_user_question = original_user_question.into();
        let request_id = DocumentResearchRequestId::generate();
        self.conversation
            .start_document_research_request(
                principal,
                conversation_id,
                request_id.clone(),
                original_user_question.clone(),
                requested_answer_composition_styles,
                command_id("request-start", &request_id, &original_user_question),
                Utc::now(),
            )
            .await?;
        let clarification = self
            .clarification
            .start(
                principal,
                conversation_id.clone(),
                request_id.clone(),
                original_user_question,
                command_id("clarification-start", &request_id, "start"),
                Utc::now(),
            )
            .await?;
        let clarification = self
            .evaluate_clarification_and_sync(principal, conversation_id, &request_id, clarification)
            .await?;
        Ok(StartedDocumentResearchRequest {
            document_research_conversation_id: conversation_id.clone(),
            document_research_request_id: request_id,
            clarification_state: clarification,
        })
    }

    /// Submits one user clarification message and evaluates it.
    pub async fn submit_research_question_clarification_message(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        message: impl Into<String>,
    ) -> Result<ResearchQuestionClarificationState> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let message = message.into();
        let command_key = message.trim().to_owned();
        let state = self
            .clarification
            .submit_user_message(
                principal,
                request_id,
                message,
                command_id("clarification-message", request_id, &command_key),
                Utc::now(),
            )
            .await?;
        self.evaluate_clarification_and_sync(principal, conversation_id, request_id, state).await
    }

    /// Retries a failed question evaluation.
    pub async fn retry_research_question_evaluation(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
    ) -> Result<ResearchQuestionClarificationState> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let state = self
            .clarification
            .retry_research_question_evaluation(
                principal,
                request_id,
                command_id("clarification-retry", request_id, "retry"),
                Utc::now(),
            )
            .await?;
        self.evaluate_clarification_and_sync(principal, conversation_id, request_id, state).await
    }

    /// Cancels a request that has not reached a terminal lifecycle state.
    pub async fn cancel_document_research_request(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        explanation: Option<String>,
    ) -> Result<()> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let explanation = normalize_optional_explanation(explanation)?;
        let conversation = self
            .conversation
            .load_document_research_conversation(principal, conversation_id)
            .await?;
        let request = conversation
            .document_research_request(request_id)
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle })?;
        if request.document_research_request_status
            == DocumentResearchRequestStatus::DocumentResearchRequestCancelled
        {
            return Ok(());
        }
        if request.document_research_request_status.is_terminal() {
            return Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Lifecycle,
                message: "research request is already terminal".to_owned(),
            });
        }

        let execution_owned_status = matches!(
            request.document_research_request_status,
            DocumentResearchRequestStatus::MarkdownResearchExecutionPrepared
                | DocumentResearchRequestStatus::MarkdownResearchExecutionRunning
        );
        if execution_owned_status {
            let execution = self.load_execution_for_request(principal, request_id).await?;
            match execution.terminal_state {
                Some(MarkdownResearchExecutionTerminalState::Completed) => {
                    return Err(RuntimeError::InvalidState {
                        stage: RuntimeStage::Lifecycle,
                        message: "research execution is already completed".to_owned(),
                    });
                }
                Some(MarkdownResearchExecutionTerminalState::Failed) => {
                    self.sync_execution_lifecycle_status(
                        principal,
                        conversation_id,
                        request_id,
                        DocumentResearchRequestStatus::MarkdownResearchExecutionFailed,
                        Some("execution failed".to_owned()),
                    )
                    .await?;
                    return Err(RuntimeError::InvalidState {
                        stage: RuntimeStage::Execution,
                        message: "research execution has failed".to_owned(),
                    });
                }
                Some(MarkdownResearchExecutionTerminalState::Cancelled) => {
                    self.sync_execution_lifecycle_status(
                        principal,
                        conversation_id,
                        request_id,
                        DocumentResearchRequestStatus::DocumentResearchRequestCancelled,
                        execution.terminal_explanation.clone(),
                    )
                    .await?;
                    return Ok(());
                }
                None => {
                    let request_event = MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancellationRequested {
                        cancellation_explanation: explanation.clone(),
                    };
                    let terminal_event =
                        MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancelled {
                            cancellation_explanation: explanation.clone(),
                        };
                    let updated = self
                        .trace
                        .append_markdown_research_execution_events(
                            principal,
                            &execution
                                .prepared_markdown_research_execution
                                .markdown_research_execution_id,
                            command_id(
                                "execution-cancel",
                                request_id,
                                explanation.as_deref().unwrap_or("cancel"),
                            ),
                            Utc::now(),
                            vec![request_event, terminal_event],
                        )
                        .await?;
                    match updated.terminal_state {
                        Some(MarkdownResearchExecutionTerminalState::Cancelled) => {
                            self.sync_execution_lifecycle_status(
                                principal,
                                conversation_id,
                                request_id,
                                DocumentResearchRequestStatus::DocumentResearchRequestCancelled,
                                updated.terminal_explanation,
                            )
                            .await?;
                            return Ok(());
                        }
                        Some(MarkdownResearchExecutionTerminalState::Failed) => {
                            self.sync_execution_lifecycle_status(
                                principal,
                                conversation_id,
                                request_id,
                                DocumentResearchRequestStatus::MarkdownResearchExecutionFailed,
                                updated.terminal_explanation,
                            )
                            .await?;
                            return Err(RuntimeError::InvalidState {
                                stage: RuntimeStage::Execution,
                                message: "research execution has failed".to_owned(),
                            });
                        }
                        Some(MarkdownResearchExecutionTerminalState::Completed) => {
                            return Err(RuntimeError::InvalidState {
                                stage: RuntimeStage::Lifecycle,
                                message: "research execution is already completed".to_owned(),
                            });
                        }
                        None => {
                            return Err(RuntimeError::CorruptState {
                                stage: RuntimeStage::Trace,
                                message: "cancellation command did not reach a terminal state"
                                    .to_owned(),
                            });
                        }
                    }
                }
            }
        } else {
            self.clarification
                .cancel(
                    principal,
                    request_id,
                    explanation.clone(),
                    command_id(
                        "clarification-cancel",
                        request_id,
                        explanation.as_deref().unwrap_or("cancel"),
                    ),
                    Utc::now(),
                )
                .await?;
        }

        self.sync_execution_lifecycle_status(
            principal,
            conversation_id,
            request_id,
            DocumentResearchRequestStatus::DocumentResearchRequestCancelled,
            explanation,
        )
        .await
    }

    /// Freezes a Markdown Research Execution contract for a ready request.
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_markdown_research_execution(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        markdown_corpus_snapshot_id: &MarkdownCorpusSnapshotId,
        strong_markdown_research_model_reference: impl Into<String>,
        verbatim_source_evidence_extraction_model_reference: impl Into<String>,
        markdown_research_execution_limits: MarkdownResearchExecutionLimits,
        requested_answer_composition_styles: Vec<AnswerCompositionStyle>,
    ) -> Result<PreparedMarkdownResearchExecution> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let clarification = self.clarification.load(principal, request_id).await?;
        let frozen_brief = clarification.frozen_document_research_brief.clone().ok_or(
            RuntimeError::InvalidState {
                stage: RuntimeStage::Lifecycle,
                message: "research question is not ready for execution".to_owned(),
            },
        )?;
        if clarification.research_question_clarification_status
            != DocumentResearchRequestStatus::DocumentResearchBriefReadyForExecution
        {
            return Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Lifecycle,
                message: "research question is not ready for execution".to_owned(),
            });
        }
        let snapshot = self
            .corpus
            .open_markdown_corpus_snapshot(principal, markdown_corpus_snapshot_id)
            .await?;
        let mut styles = requested_answer_composition_styles;
        styles.sort();
        styles.dedup();
        let execution_id = execution_id_for_request(request_id);
        let prepared = PreparedMarkdownResearchExecution {
            markdown_research_execution_id: execution_id.clone(),
            document_research_conversation_id: conversation_id.clone(),
            document_research_request_id: request_id.clone(),
            frozen_document_research_brief: frozen_brief,
            markdown_corpus_snapshot_id: snapshot.markdown_corpus_snapshot_id.clone(),
            strong_markdown_research_model_reference: strong_markdown_research_model_reference
                .into(),
            verbatim_source_evidence_extraction_model_reference:
                verbatim_source_evidence_extraction_model_reference.into(),
            markdown_research_execution_limits,
            requested_answer_composition_styles: styles,
            markdown_research_execution_prepared_at: Utc::now(),
            markdown_research_execution_prepare_command_id: command_id(
                "execution-prepare",
                request_id,
                "prepare",
            ),
        };
        prepared.validate()?;
        let conversation = self
            .conversation
            .load_document_research_conversation(principal, conversation_id)
            .await?;
        if let Ok(existing) =
            self.trace.replay_markdown_research_execution(principal, &execution_id).await
        {
            if existing.prepared_markdown_research_execution.contract_hash()?
                != prepared.contract_hash()?
            {
                return Err(RuntimeError::Conflict {
                    stage: RuntimeStage::Lifecycle,
                    message: "execution preparation conflicts with the frozen contract".to_owned(),
                });
            }
            return Ok(existing.prepared_markdown_research_execution);
        }
        if let Some(request) = conversation.document_research_request(request_id)
            && request.document_research_request_status
                != DocumentResearchRequestStatus::DocumentResearchBriefReadyForExecution
        {
            return Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Lifecycle,
                message: "request is not in the brief-ready lifecycle state".to_owned(),
            });
        }
        self.prepare_contract_atomically(principal, conversation_id, request_id, &prepared).await?;
        Ok(prepared)
    }

    /// Executes or resumes a prepared contract and returns public projections.
    pub async fn execute_prepared_markdown_research(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
    ) -> Result<MarkdownResearchExecutionResult> {
        principal.require(PrincipalCapability::ExecuteMarkdownResearch)?;
        let execution_id = execution_id_for_request(request_id);
        let execution_lock = {
            let mut locks = self.execution_locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(&execution_id).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(AsyncMutex::new(()));
                locks.insert(execution_id, Arc::downgrade(&lock));
                lock
            }
        };
        let _execution_guard = execution_lock.lock().await;
        let state = self.load_execution_for_request(principal, request_id).await?;
        let prepared = state.prepared_markdown_research_execution.clone();
        let snapshot = self
            .corpus
            .open_markdown_corpus_snapshot(principal, &prepared.markdown_corpus_snapshot_id)
            .await?;
        let conversation = self
            .conversation
            .load_document_research_conversation(
                principal,
                &prepared.document_research_conversation_id,
            )
            .await?;
        let request = conversation
            .document_research_request(request_id)
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle })?;
        if request.document_research_request_status
            == DocumentResearchRequestStatus::MarkdownResearchExecutionFailed
        {
            return Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Lifecycle,
                message: "research execution has failed".to_owned(),
            });
        }
        if request.document_research_request_status
            == DocumentResearchRequestStatus::DocumentResearchRequestCancelled
        {
            return Err(RuntimeError::Cancelled);
        }
        if conversation.document_research_request(request_id).is_some_and(|request| {
            request.document_research_request_status
                == DocumentResearchRequestStatus::MarkdownResearchExecutionPrepared
        }) {
            self.conversation
                .change_document_research_request_status(
                    principal,
                    &prepared.document_research_conversation_id,
                    request_id,
                    DocumentResearchRequestStatus::MarkdownResearchExecutionRunning,
                    Some("execution started".to_owned()),
                    command_id("conversation-running", request_id, "running"),
                    Utc::now(),
                )
                .await?;
        }
        let state = match self
            .engine
            .execute_prepared_markdown_research(principal, &prepared, &snapshot, &self.trace)
            .await
        {
            Ok(state) => state,
            Err(error) => {
                // A retryable transport/storage failure leaves the persisted
                // checkpoint running so the same command can resume. Closed
                // model-response failures and exhausted limits are terminal
                // and must be recorded in both Trace and lifecycle state.
                let terminal_failure = matches!(
                    &error,
                    RuntimeError::ModelTransport { retryable: false, .. }
                        | RuntimeError::ModelResponse { .. }
                        | RuntimeError::LimitExceeded { .. }
                );
                if !terminal_failure {
                    return Err(error);
                }
                let mut latest = self.load_execution_for_request(principal, request_id).await?;
                if latest.terminal_state.is_none() {
                    let error_code = serde_json::to_string(&error.code())
                        .unwrap_or_else(|_| "\"execution_failed\"".to_owned())
                        .trim_matches('"')
                        .to_owned();
                    latest = self
                        .trace
                        .append_markdown_research_execution_events(
                            principal,
                            &prepared.markdown_research_execution_id,
                            command_id("execution-failed", request_id, &error_code),
                            Utc::now(),
                            vec![MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed {
                                error_code,
                                failure_explanation:
                                    "execution stopped after a non-retryable failure".to_owned(),
                            }],
                        )
                        .await?;
                }
                match latest.terminal_state {
                    Some(MarkdownResearchExecutionTerminalState::Cancelled) => {
                        self.sync_execution_lifecycle_status(
                            principal,
                            &prepared.document_research_conversation_id,
                            request_id,
                            DocumentResearchRequestStatus::DocumentResearchRequestCancelled,
                            latest.terminal_explanation,
                        )
                        .await?;
                        return Err(RuntimeError::Cancelled);
                    }
                    Some(MarkdownResearchExecutionTerminalState::Failed) => {
                        self.sync_execution_lifecycle_status(
                            principal,
                            &prepared.document_research_conversation_id,
                            request_id,
                            DocumentResearchRequestStatus::MarkdownResearchExecutionFailed,
                            latest.terminal_explanation,
                        )
                        .await?;
                        return Err(error);
                    }
                    Some(MarkdownResearchExecutionTerminalState::Completed) => latest,
                    None => return Err(error),
                }
            }
        };
        if state.terminal_state == Some(MarkdownResearchExecutionTerminalState::Completed) {
            let answers = prepared
                .requested_answer_composition_styles
                .iter()
                .copied()
                .map(|style| state.project_public_markdown_research_answer(style))
                .collect::<Result<Vec<_>>>()?;
            let conversation = self
                .conversation
                .load_document_research_conversation(
                    principal,
                    &prepared.document_research_conversation_id,
                )
                .await?;
            if conversation.document_research_request(request_id).is_some_and(|request| {
                request.document_research_request_status
                    != DocumentResearchRequestStatus::DocumentResearchRequestCompleted
            }) {
                self.conversation
                    .complete_document_research_request(
                        principal,
                        &prepared.document_research_conversation_id,
                        request_id,
                        answers.clone(),
                        command_id("conversation-complete", request_id, "complete"),
                        Utc::now(),
                    )
                    .await?;
            }
            Ok(MarkdownResearchExecutionResult {
                public_markdown_research_answers: answers,
                markdown_research_execution_overview: state
                    .project_markdown_research_execution_overview(),
            })
        } else if state.terminal_state == Some(MarkdownResearchExecutionTerminalState::Cancelled) {
            self.sync_execution_lifecycle_status(
                principal,
                &prepared.document_research_conversation_id,
                request_id,
                DocumentResearchRequestStatus::DocumentResearchRequestCancelled,
                state.terminal_explanation.clone(),
            )
            .await?;
            Err(RuntimeError::Cancelled)
        } else if state.terminal_state == Some(MarkdownResearchExecutionTerminalState::Failed) {
            self.sync_execution_lifecycle_status(
                principal,
                &prepared.document_research_conversation_id,
                request_id,
                DocumentResearchRequestStatus::MarkdownResearchExecutionFailed,
                state.terminal_explanation.clone(),
            )
            .await?;
            Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Execution,
                message: "research execution has failed".to_owned(),
            })
        } else {
            Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Execution,
                message: "execution has not reached a completed terminal state".to_owned(),
            })
        }
    }

    /// Loads a request through complete lifecycle replay.
    pub async fn load_document_research_request(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
    ) -> Result<DocumentResearchRequestSnapshot> {
        let conversation = self
            .conversation
            .load_document_research_conversation(principal, conversation_id)
            .await?;
        let clarification = self.clarification.load(principal, request_id).await?;
        let prepared = self
            .load_execution_for_request(principal, request_id)
            .await
            .ok()
            .map(|state| state.prepared_markdown_research_execution);
        Ok(DocumentResearchRequestSnapshot {
            document_research_conversation: conversation,
            research_question_clarification: clarification,
            prepared_markdown_research_execution: prepared,
        })
    }

    /// Returns a public answer projection after complete Trace replay.
    pub async fn project_public_markdown_research_answer(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
        style: AnswerCompositionStyle,
    ) -> Result<PublicMarkdownResearchAnswer> {
        let state = self.load_execution_for_request(principal, request_id).await?;
        state.project_public_markdown_research_answer(style)
    }

    /// Returns a safe execution overview after complete Trace replay.
    pub async fn project_markdown_research_execution_overview(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
    ) -> Result<MarkdownResearchExecutionOverview> {
        Ok(self
            .load_execution_for_request(principal, request_id)
            .await?
            .project_markdown_research_execution_overview())
    }

    /// Returns a paginated, raw-payload-free audit projection.
    pub async fn project_detailed_markdown_research_audit(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<crate::domain::DetailedMarkdownResearchAuditPage> {
        self.load_execution_for_request(principal, request_id)
            .await?
            .project_detailed_markdown_research_audit(cursor, page_size)
    }

    /// Evaluates a clarification and keeps the conversation lifecycle in sync
    /// even when the model adapter rejects the request.
    async fn evaluate_clarification_and_sync(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        state: ResearchQuestionClarificationState,
    ) -> Result<ResearchQuestionClarificationState> {
        let state = match self.evaluate_clarification(principal, &state).await {
            Ok(state) => state,
            Err(error) => {
                // A model transport/schema failure is persisted by
                // `evaluate_clarification`; replay that state and mirror the
                // terminal clarification status into the conversation log.
                if error.stage() == RuntimeStage::Model
                    && let Ok(failed_state) = self.clarification.load(principal, request_id).await
                    && failed_state.research_question_clarification_status
                        == DocumentResearchRequestStatus::ResearchQuestionEvaluationFailed
                {
                    let conversation = self
                        .conversation
                        .load_document_research_conversation(principal, conversation_id)
                        .await?;
                    self.sync_conversation_status(
                        principal,
                        &conversation,
                        request_id,
                        failed_state.research_question_clarification_status,
                        failed_state.failure_explanation.clone(),
                    )
                    .await?;
                }
                return Err(error);
            }
        };
        let conversation = self
            .conversation
            .load_document_research_conversation(principal, conversation_id)
            .await?;
        self.sync_conversation_status(
            principal,
            &conversation,
            request_id,
            state.research_question_clarification_status,
            state.failure_explanation.clone(),
        )
        .await?;
        Ok(state)
    }

    async fn evaluate_clarification(
        &self,
        principal: &ResearchPrincipal,
        state: &ResearchQuestionClarificationState,
    ) -> Result<ResearchQuestionClarificationState> {
        if state.research_question_clarification_status
            != DocumentResearchRequestStatus::ResearchQuestionEvaluationPending
        {
            return Ok(state.clone());
        }
        let task = StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(
            ResearchQuestionEvaluationTask {
                markdown_research_model_task_id: clarification_task_id(state),
                document_research_conversation_id: state.document_research_conversation_id.clone(),
                document_research_request_id: state.document_research_request_id.clone(),
                research_question_clarification_revision: state
                    .research_question_clarification_revision,
                original_user_question: state.original_user_question.clone(),
                research_question_clarification_dialogue: state
                    .research_question_clarification_dialogue
                    .clone(),
                document_research_brief_draft: state.document_research_brief_draft.clone(),
                allowed_completed_research_context: Vec::new(),
                markdown_research_model_task_schema_version:
                    MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
            },
        );
        let result = async {
            let response = self.gateway.execute_strong_markdown_research_task(task).await?;
            let StrongMarkdownResearchModelResponse::ResearchQuestionEvaluation(output) = response
            else {
                return Err(RuntimeError::ModelResponse {
                    message: "question evaluation returned another response kind".to_owned(),
                });
            };
            self.clarification
                .apply_model_output(
                    principal,
                    &state.document_research_request_id,
                    output,
                    command_id(
                        "clarification-evaluation",
                        &state.document_research_request_id,
                        &state.research_question_clarification_revision.to_string(),
                    ),
                    Utc::now(),
                )
                .await
        }
        .await;
        match result {
            Ok(state) => Ok(state),
            Err(error) if error.stage() == RuntimeStage::Model => {
                let failure = self
                    .clarification
                    .record_evaluation_failure(
                        principal,
                        &state.document_research_request_id,
                        "question evaluation failed".to_owned(),
                        command_id(
                            "clarification-failure",
                            &state.document_research_request_id,
                            &state.research_question_clarification_revision.to_string(),
                        ),
                        Utc::now(),
                    )
                    .await;
                match failure {
                    Ok(_) => Err(error),
                    Err(persistence_error) => Err(persistence_error),
                }
            }
            Err(error) => Err(error),
        }
    }

    async fn sync_conversation_status(
        &self,
        principal: &ResearchPrincipal,
        conversation: &DocumentResearchConversation,
        request_id: &DocumentResearchRequestId,
        target_status: DocumentResearchRequestStatus,
        explanation: Option<String>,
    ) -> Result<()> {
        let Some(request) = conversation.document_research_request(request_id) else {
            return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle });
        };
        if request.document_research_request_status == target_status {
            return Ok(());
        }
        self.conversation
            .change_document_research_request_status(
                principal,
                &conversation.document_research_conversation_id,
                request_id,
                target_status,
                explanation,
                command_id("conversation-status", request_id, &format!("{target_status:?}")),
                Utc::now(),
            )
            .await
            .map(|_| ())
    }

    async fn sync_execution_lifecycle_status(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        target_status: DocumentResearchRequestStatus,
        explanation: Option<String>,
    ) -> Result<()> {
        let conversation = self
            .conversation
            .load_document_research_conversation(principal, conversation_id)
            .await?;
        let Some(request) = conversation.document_research_request(request_id) else {
            return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle });
        };
        if request.document_research_request_status == target_status {
            return Ok(());
        }
        if request.document_research_request_status.is_terminal() {
            return Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Lifecycle,
                message: "research request reached another terminal state".to_owned(),
            });
        }
        let command_key = format!("{target_status:?}|{}", explanation.as_deref().unwrap_or(""));
        self.conversation
            .change_document_research_request_status(
                principal,
                conversation_id,
                request_id,
                target_status,
                explanation,
                command_id("conversation-terminal", request_id, &command_key),
                Utc::now(),
            )
            .await
            .map(|_| ())
    }

    async fn load_execution_for_request(
        &self,
        principal: &ResearchPrincipal,
        request_id: &DocumentResearchRequestId,
    ) -> Result<ReplayedMarkdownResearchExecution> {
        self.trace
            .replay_markdown_research_execution(principal, &execution_id_for_request(request_id))
            .await
    }

    async fn prepare_contract_atomically(
        &self,
        principal: &ResearchPrincipal,
        conversation_id: &DocumentResearchConversationId,
        request_id: &DocumentResearchRequestId,
        prepared: &PreparedMarkdownResearchExecution,
    ) -> Result<()> {
        let owner = principal.subject_id.clone();
        let request_id = request_id.clone();
        let conversation_scope = format!("conversation:{conversation_id}");
        let execution_scope = format!("execution:{}", prepared.markdown_research_execution_id);
        let status_command_id = command_id("conversation-prepare", &request_id, "prepare");
        let execution_command_id = command_id("execution-contract", &request_id, "contract");
        let recorded_at = Utc::now();
        let status_kind =
            DocumentResearchConversationEventKind::DocumentResearchRequestStatusChanged {
                document_research_request_id: request_id.clone(),
                document_research_request_status:
                    DocumentResearchRequestStatus::MarkdownResearchExecutionPrepared,
                status_change_explanation: Some("execution contract frozen".to_owned()),
            };
        let execution_kind = MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
            prepared_markdown_research_execution: Box::new(prepared.clone()),
        };
        let prepared = prepared.clone();
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    let lifecycle_state = crate::conversation::replay_conversation_rows(
                        transaction.read_events(EventStream::Lifecycle, &conversation_scope)?,
                        Some(&owner),
                    )?;
                    let lifecycle_request =
                        lifecycle_state.document_research_request(&request_id).ok_or(
                            RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Lifecycle },
                        )?;
                    if lifecycle_request.document_research_request_status
                        != DocumentResearchRequestStatus::DocumentResearchBriefReadyForExecution
                        && lifecycle_request.document_research_request_status
                            != DocumentResearchRequestStatus::MarkdownResearchExecutionPrepared
                    {
                        return Err(RuntimeError::InvalidState {
                            stage: RuntimeStage::Lifecycle,
                            message: "request is not in the brief-ready lifecycle state".to_owned(),
                        });
                    }
                    if lifecycle_request.document_research_request_status
                        == DocumentResearchRequestStatus::DocumentResearchBriefReadyForExecution
                    {
                        let request_hash = canonical_content_hash(&(&owner, &status_kind))?;
                        if let Some(existing) =
                            transaction.read_command_commit(&conversation_scope, &status_command_id)?
                        {
                            if existing.request_hash != request_hash {
                                return Err(RuntimeError::Conflict {
                                    stage: RuntimeStage::Lifecycle,
                                    message: "conversation prepare command conflicts with another request"
                                        .to_owned(),
                                });
                            }
                        } else {
                            crate::conversation::append_kind(
                                transaction,
                                &conversation_scope,
                                &owner,
                                status_command_id.clone(),
                                recorded_at,
                                status_kind.clone(),
                                request_hash,
                            )?;
                        }
                    }
                    let request_hash = canonical_content_hash(&(
                        &owner,
                        &prepared.markdown_research_execution_id,
                        std::slice::from_ref(&execution_kind),
                    ))?;
                    if let Some(existing) =
                        transaction.read_command_commit(&execution_scope, &execution_command_id)?
                    {
                        if existing.request_hash != request_hash {
                            return Err(RuntimeError::Conflict {
                                stage: RuntimeStage::Execution,
                                message: "execution contract command conflicts with another request"
                                    .to_owned(),
                            });
                        }
                    } else {
                        let event = NewEvent {
                            scope: execution_scope.clone(),
                            owner_subject_id: owner.clone(),
                            command_id: execution_command_id.clone(),
                            event_schema_version: MARKDOWN_RESEARCH_EXECUTION_TRACE_SCHEMA_VERSION,
                            event_type: execution_kind.event_type().to_owned(),
                            recorded_at,
                            payload_json: serde_json::to_string(&execution_kind)?,
                        };
                        let command = NewCommandCommit {
                            scope: execution_scope.clone(),
                            command_id: execution_command_id.clone(),
                            request_hash,
                            result_json: serde_json::to_string(&serde_json::json!({
                                "markdown_research_execution_id": prepared
                                    .markdown_research_execution_id
                                    .clone(),
                            }))?,
                            committed_at: recorded_at,
                        };
                        transaction.append_events_with_command(
                            EventStream::Execution,
                            &command,
                            &[event],
                        )?;
                    }
                    Ok(())
                })
            })
            .await
    }
}

fn command_id<T: AsRef<str>>(prefix: &str, request_id: &T, key: &str) -> CommandId {
    let digest = crate::domain::sha256_content_hash(
        format!("{prefix}|{}|{key}", request_id.as_ref()).as_bytes(),
    );
    CommandId::from_value(format!("{prefix}-{}", &digest[7..])).expect("runtime command ID grammar")
}

fn execution_id_for_request(request_id: &DocumentResearchRequestId) -> MarkdownResearchExecutionId {
    let digest = crate::domain::sha256_content_hash(request_id.as_str().as_bytes());
    MarkdownResearchExecutionId::from_value(format!("execution-{}", &digest[7..]))
        .expect("runtime execution ID grammar")
}

fn clarification_task_id(
    state: &ResearchQuestionClarificationState,
) -> MarkdownResearchModelTaskId {
    let digest = crate::domain::sha256_content_hash(
        format!(
            "clarification|{}|{}",
            state.document_research_request_id, state.research_question_clarification_revision
        )
        .as_bytes(),
    );
    MarkdownResearchModelTaskId::from_value(format!("clarification-task-{}", &digest[7..]))
        .expect("runtime model task ID grammar")
}

fn normalize_optional_explanation(explanation: Option<String>) -> Result<Option<String>> {
    explanation
        .map(|explanation| {
            let explanation = explanation.trim();
            if explanation.is_empty()
                || explanation.len() > MAX_RESEARCH_TEXT_BYTES
                || explanation.contains('\0')
            {
                return Err(RuntimeError::validation(
                    RuntimeStage::Lifecycle,
                    "cancellation explanation is invalid",
                ));
            }
            Ok(explanation.to_owned())
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    use super::*;
    use crate::execution_engine::tests::{DeterministicGateway, fixture_input};
    use crate::identity::{PrincipalCapability, SubjectId};
    use async_trait::async_trait;
    use rusqlite::{Connection, params};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration as StdDuration;
    use tempfile::tempdir;
    use tokio::sync::Semaphore;

    fn principal(subject: &str) -> ResearchPrincipal {
        ResearchPrincipal::new(
            SubjectId::from_value(subject).unwrap(),
            [
                PrincipalCapability::PublishMarkdownCorpusSnapshot,
                PrincipalCapability::ExecuteMarkdownResearch,
            ],
        )
    }

    async fn prepare_fixture_runtime(
        runtime: &TraceableMarkdownResearchRuntime,
        principal: &ResearchPrincipal,
    ) -> (
        MarkdownCorpusSnapshotId,
        DocumentResearchConversationId,
        DocumentResearchRequestId,
        PreparedMarkdownResearchExecution,
    ) {
        let snapshot =
            runtime.publish_markdown_corpus_snapshot(principal, fixture_input()).await.unwrap();
        let conversation_id =
            runtime.create_document_research_conversation(principal).await.unwrap();
        let started = runtime
            .start_document_research_request(
                principal,
                &conversation_id,
                "原问题",
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        let prepared = runtime
            .prepare_markdown_research_execution(
                principal,
                &conversation_id,
                &started.document_research_request_id,
                &snapshot,
                "fixture-strong",
                "fixture-cheap",
                MarkdownResearchExecutionLimits::default(),
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        (snapshot, conversation_id, started.document_research_request_id, prepared)
    }

    fn drop_update_trigger(connection: &Connection) {
        connection
            .execute_batch("DROP TRIGGER IF EXISTS execution_events_append_only_update;")
            .unwrap();
    }

    fn mutate_execution_event_payload(
        path: &std::path::Path,
        execution_id: &MarkdownResearchExecutionId,
        event_type: &str,
        mutate: impl FnOnce(&mut Value),
    ) {
        let connection = Connection::open(path).unwrap();
        drop_update_trigger(&connection);
        let scope = format!("execution:{execution_id}");
        let (sequence, payload): (i64, String) = connection
            .query_row(
                "SELECT sequence, payload_json FROM execution_events
                 WHERE scope = ?1 AND event_type = ?2 ORDER BY sequence LIMIT 1",
                params![scope, event_type],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let mut value: Value = serde_json::from_str(&payload).unwrap();
        mutate(&mut value);
        connection
            .execute(
                "UPDATE execution_events SET payload_json = ?1 WHERE scope = ?2 AND sequence = ?3",
                params![serde_json::to_string(&value).unwrap(), scope, sequence],
            )
            .unwrap();
    }

    fn mutate_execution_event_owner(
        path: &std::path::Path,
        execution_id: &MarkdownResearchExecutionId,
        sequence: i64,
        owner_subject_id: &str,
    ) {
        let connection = Connection::open(path).unwrap();
        drop_update_trigger(&connection);
        connection
            .execute(
                "UPDATE execution_events SET owner_subject_id = ?1
                 WHERE scope = ?2 AND sequence = ?3",
                params![owner_subject_id, format!("execution:{execution_id}"), sequence],
            )
            .unwrap();
    }

    fn mutate_snapshot_hash(path: &std::path::Path, snapshot_id: &MarkdownCorpusSnapshotId) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("DROP TRIGGER IF EXISTS markdown_corpus_snapshots_immutable_update;")
            .unwrap();
        connection
            .execute(
                "UPDATE markdown_corpus_snapshots
                 SET markdown_corpus_snapshot_hash = ?1
                 WHERE markdown_corpus_snapshot_id = ?2",
                params![
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                    snapshot_id.as_str()
                ],
            )
            .unwrap();
    }

    /// Gateway that fails once on the first execution-only strong task.
    #[derive(Default)]
    struct FailOnceExecutionGateway {
        failed: AtomicBool,
        delegate: DeterministicGateway,
    }

    #[async_trait]
    impl MarkdownResearchModelGateway for FailOnceExecutionGateway {
        async fn execute_strong_markdown_research_task(
            &self,
            task: StrongMarkdownResearchModelTask,
        ) -> Result<StrongMarkdownResearchModelResponse> {
            if !matches!(task, StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(_))
                && !self.failed.swap(true, Ordering::SeqCst)
            {
                return Err(RuntimeError::ModelTransport {
                    message: "transient fixture transport failure".to_owned(),
                    retryable: true,
                });
            }
            self.delegate.execute_strong_markdown_research_task(task).await
        }

        async fn extract_verbatim_source_evidence_candidates(
            &self,
            task: crate::model_gateway::VerbatimSourceEvidenceExtractionTask,
        ) -> Result<crate::model_gateway::VerbatimSourceEvidenceCandidateSet> {
            self.delegate.extract_verbatim_source_evidence_candidates(task).await
        }
    }

    struct BlockingCountingGateway {
        blocked_first_execution_task: AtomicBool,
        duplicate_execution_task_observed: AtomicBool,
        entered: Semaphore,
        release: Semaphore,
        observed_execution_task_ids: Mutex<BTreeSet<String>>,
        delegate: DeterministicGateway,
    }

    impl Default for BlockingCountingGateway {
        fn default() -> Self {
            Self {
                blocked_first_execution_task: AtomicBool::new(false),
                duplicate_execution_task_observed: AtomicBool::new(false),
                entered: Semaphore::new(0),
                release: Semaphore::new(0),
                observed_execution_task_ids: Mutex::new(BTreeSet::new()),
                delegate: DeterministicGateway,
            }
        }
    }

    impl BlockingCountingGateway {
        fn record_execution_task(&self, task_id: &MarkdownResearchModelTaskId) {
            let inserted = self
                .observed_execution_task_ids
                .lock()
                .expect("task observation lock")
                .insert(task_id.as_str().to_owned());
            if !inserted {
                self.duplicate_execution_task_observed.store(true, Ordering::SeqCst);
            }
        }

        async fn block_first_execution_task(&self) {
            if !self.blocked_first_execution_task.swap(true, Ordering::SeqCst) {
                self.entered.add_permits(1);
                self.release.acquire().await.expect("release semaphore remains open").forget();
            }
        }
    }

    #[async_trait]
    impl MarkdownResearchModelGateway for BlockingCountingGateway {
        async fn execute_strong_markdown_research_task(
            &self,
            task: StrongMarkdownResearchModelTask,
        ) -> Result<StrongMarkdownResearchModelResponse> {
            if !matches!(task, StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(_)) {
                self.record_execution_task(task.markdown_research_model_task_id());
                self.block_first_execution_task().await;
            }
            self.delegate.execute_strong_markdown_research_task(task).await
        }

        async fn extract_verbatim_source_evidence_candidates(
            &self,
            task: crate::model_gateway::VerbatimSourceEvidenceExtractionTask,
        ) -> Result<crate::model_gateway::VerbatimSourceEvidenceCandidateSet> {
            self.record_execution_task(&task.markdown_research_model_task_id);
            self.delegate.extract_verbatim_source_evidence_candidates(task).await
        }
    }

    struct BlockingNonRetryableGateway {
        blocked: AtomicBool,
        entered: Semaphore,
        release: Semaphore,
        delegate: DeterministicGateway,
    }

    impl Default for BlockingNonRetryableGateway {
        fn default() -> Self {
            Self {
                blocked: AtomicBool::new(false),
                entered: Semaphore::new(0),
                release: Semaphore::new(0),
                delegate: DeterministicGateway,
            }
        }
    }

    #[async_trait]
    impl MarkdownResearchModelGateway for BlockingNonRetryableGateway {
        async fn execute_strong_markdown_research_task(
            &self,
            task: StrongMarkdownResearchModelTask,
        ) -> Result<StrongMarkdownResearchModelResponse> {
            if matches!(task, StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(_)) {
                return self.delegate.execute_strong_markdown_research_task(task).await;
            }
            if !self.blocked.swap(true, Ordering::SeqCst) {
                self.entered.add_permits(1);
                self.release.acquire().await.expect("release semaphore remains open").forget();
            }
            Err(RuntimeError::ModelTransport {
                message: "closed fixture transport failure".to_owned(),
                retryable: false,
            })
        }

        async fn extract_verbatim_source_evidence_candidates(
            &self,
            task: crate::model_gateway::VerbatimSourceEvidenceExtractionTask,
        ) -> Result<crate::model_gateway::VerbatimSourceEvidenceCandidateSet> {
            self.delegate.extract_verbatim_source_evidence_candidates(task).await
        }
    }

    async fn wait_until_execution_is_blocked(semaphore: &Semaphore) {
        tokio::time::timeout(StdDuration::from_secs(5), semaphore.acquire())
            .await
            .expect("execution reached blocking gateway")
            .expect("gateway semaphore remains open")
            .forget();
    }

    fn terminal_audit_item_count(
        items: &[crate::domain::DetailedMarkdownResearchAuditItem],
    ) -> usize {
        items
            .iter()
            .filter(|item| {
                matches!(
                    item.markdown_research_execution_event_type.as_str(),
                    "markdown_research_execution_completed"
                        | "markdown_research_execution_failed"
                        | "markdown_research_execution_cancelled"
                )
            })
            .count()
    }

    #[tokio::test]
    async fn runtime_facade_runs_lifecycle_prepare_execute_and_restart() {
        let gateway = Arc::new(DeterministicGateway);
        let runtime = TraceableMarkdownResearchRuntime::in_memory(gateway).unwrap();
        let principal = ResearchPrincipal::new(
            SubjectId::from_value("subject-runtime").unwrap(),
            [
                PrincipalCapability::PublishMarkdownCorpusSnapshot,
                PrincipalCapability::ExecuteMarkdownResearch,
            ],
        );
        let snapshot =
            runtime.publish_markdown_corpus_snapshot(&principal, fixture_input()).await.unwrap();
        let conversation_id =
            runtime.create_document_research_conversation(&principal).await.unwrap();
        let started = runtime
            .start_document_research_request(
                &principal,
                &conversation_id,
                "原问题",
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        assert_eq!(
            started.clarification_state.research_question_clarification_status,
            DocumentResearchRequestStatus::DocumentResearchBriefReadyForExecution
        );
        let prepared = runtime
            .prepare_markdown_research_execution(
                &principal,
                &conversation_id,
                &started.document_research_request_id,
                &snapshot,
                "fixture-strong",
                "fixture-cheap",
                MarkdownResearchExecutionLimits::default(),
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        let result = runtime
            .execute_prepared_markdown_research(&principal, &started.document_research_request_id)
            .await
            .unwrap();
        assert_eq!(result.public_markdown_research_answers.len(), 1);
        let same_prepared = runtime
            .prepare_markdown_research_execution(
                &principal,
                &conversation_id,
                &started.document_research_request_id,
                &snapshot,
                "fixture-strong",
                "fixture-cheap",
                MarkdownResearchExecutionLimits::default(),
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        assert_eq!(prepared.contract_hash().unwrap(), same_prepared.contract_hash().unwrap());
        let overview = runtime
            .project_markdown_research_execution_overview(
                &principal,
                &started.document_research_request_id,
            )
            .await
            .unwrap();
        assert_eq!(overview.verbatim_source_evidence_count, 1);
    }

    #[tokio::test]
    async fn b11_file_backed_runtime_reopens_after_drop_and_replays_completed_execution() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("runtime.sqlite");
        let owner = principal("b11-file-owner");
        let gateway = Arc::new(DeterministicGateway);
        let (snapshot_id, conversation_id, request_id, prepared) = {
            let runtime =
                TraceableMarkdownResearchRuntime::open(&database_path, gateway.clone()).unwrap();
            let (snapshot_id, conversation_id, request_id, prepared) =
                prepare_fixture_runtime(&runtime, &owner).await;
            runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
            (snapshot_id, conversation_id, request_id, prepared)
        };

        let reopened =
            TraceableMarkdownResearchRuntime::open(&database_path, gateway.clone()).unwrap();
        let result =
            reopened.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
        assert_eq!(result.public_markdown_research_answers.len(), 1);
        let request = reopened
            .load_document_research_request(&owner, &conversation_id, &request_id)
            .await
            .unwrap();
        assert_eq!(
            request
                .prepared_markdown_research_execution
                .as_ref()
                .map(|value| value.markdown_corpus_snapshot_id.clone()),
            Some(snapshot_id)
        );
        assert_eq!(
            request
                .prepared_markdown_research_execution
                .as_ref()
                .and_then(|value| value.markdown_research_execution_id.as_str().into()),
            Some(prepared.markdown_research_execution_id.as_str())
        );
    }

    #[tokio::test]
    async fn b11_duplicate_prepare_and_execute_are_idempotent() {
        let owner = principal("b11-idempotent-owner");
        let runtime =
            TraceableMarkdownResearchRuntime::in_memory(Arc::new(DeterministicGateway)).unwrap();
        let (snapshot_id, conversation_id, request_id, prepared) =
            prepare_fixture_runtime(&runtime, &owner).await;
        let first = runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
        let audit_before = runtime
            .project_detailed_markdown_research_audit(&owner, &request_id, None, 200)
            .await
            .unwrap();
        let second = runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
        let audit_after = runtime
            .project_detailed_markdown_research_audit(&owner, &request_id, None, 200)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(audit_before.items, audit_after.items);

        let same_prepared = runtime
            .prepare_markdown_research_execution(
                &owner,
                &conversation_id,
                &request_id,
                &snapshot_id,
                "fixture-strong",
                "fixture-cheap",
                MarkdownResearchExecutionLimits::default(),
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        assert_eq!(prepared.contract_hash().unwrap(), same_prepared.contract_hash().unwrap());
    }

    #[tokio::test]
    async fn concurrent_execute_calls_share_one_model_workflow_and_one_terminal_event() {
        let owner = principal("concurrent-execute-owner");
        let gateway = Arc::new(BlockingCountingGateway::default());
        let runtime =
            Arc::new(TraceableMarkdownResearchRuntime::in_memory(gateway.clone()).unwrap());
        let (_snapshot_id, conversation_id, request_id, _prepared) =
            prepare_fixture_runtime(&runtime, &owner).await;

        let first_runtime = runtime.clone();
        let first_owner = owner.clone();
        let first_request_id = request_id.clone();
        let first = tokio::spawn(async move {
            first_runtime.execute_prepared_markdown_research(&first_owner, &first_request_id).await
        });
        wait_until_execution_is_blocked(&gateway.entered).await;

        let second_runtime = runtime.clone();
        let second_owner = owner.clone();
        let second_request_id = request_id.clone();
        let second = tokio::spawn(async move {
            second_runtime
                .execute_prepared_markdown_research(&second_owner, &second_request_id)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished(), "duplicate execute must wait for the active workflow");

        gateway.release.add_permits(1);
        let first_result = tokio::time::timeout(StdDuration::from_secs(10), first)
            .await
            .expect("first execution completes")
            .expect("first execution task joins")
            .expect("first execution succeeds");
        let second_result = tokio::time::timeout(StdDuration::from_secs(10), second)
            .await
            .expect("second execution completes")
            .expect("second execution task joins")
            .expect("second execution succeeds");

        assert_eq!(first_result, second_result);
        assert!(!gateway.duplicate_execution_task_observed.load(Ordering::SeqCst));
        let audit = runtime
            .project_detailed_markdown_research_audit(&owner, &request_id, None, 200)
            .await
            .unwrap();
        assert_eq!(terminal_audit_item_count(&audit.items), 1);
        let request = runtime
            .load_document_research_request(&owner, &conversation_id, &request_id)
            .await
            .unwrap();
        assert_eq!(
            request
                .document_research_conversation
                .document_research_request(&request_id)
                .unwrap()
                .document_research_request_status,
            DocumentResearchRequestStatus::DocumentResearchRequestCompleted
        );
    }

    #[tokio::test]
    async fn cancellation_wins_race_with_non_retryable_model_failure_consistently() {
        let owner = principal("cancel-failure-race-owner");
        let gateway = Arc::new(BlockingNonRetryableGateway::default());
        let runtime =
            Arc::new(TraceableMarkdownResearchRuntime::in_memory(gateway.clone()).unwrap());
        let (_snapshot_id, conversation_id, request_id, _prepared) =
            prepare_fixture_runtime(&runtime, &owner).await;

        let execute_runtime = runtime.clone();
        let execute_owner = owner.clone();
        let execute_request_id = request_id.clone();
        let execute = tokio::spawn(async move {
            execute_runtime
                .execute_prepared_markdown_research(&execute_owner, &execute_request_id)
                .await
        });
        wait_until_execution_is_blocked(&gateway.entered).await;

        runtime
            .cancel_document_research_request(
                &owner,
                &conversation_id,
                &request_id,
                Some("user cancellation won".to_owned()),
            )
            .await
            .unwrap();
        gateway.release.add_permits(1);
        let execute_error = tokio::time::timeout(StdDuration::from_secs(5), execute)
            .await
            .expect("cancelled execution exits")
            .expect("execution task joins")
            .unwrap_err();
        assert!(matches!(execute_error, RuntimeError::Cancelled));

        runtime
            .cancel_document_research_request(
                &owner,
                &conversation_id,
                &request_id,
                Some("retry must be idempotent".to_owned()),
            )
            .await
            .unwrap();
        let retry_error =
            runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap_err();
        assert!(matches!(retry_error, RuntimeError::Cancelled));

        let state = runtime.load_execution_for_request(&owner, &request_id).await.unwrap();
        assert_eq!(state.terminal_state, Some(MarkdownResearchExecutionTerminalState::Cancelled));
        assert_eq!(state.terminal_explanation.as_deref(), Some("user cancellation won"));
        let request = runtime
            .load_document_research_request(&owner, &conversation_id, &request_id)
            .await
            .unwrap();
        let lifecycle_request =
            request.document_research_conversation.document_research_request(&request_id).unwrap();
        assert_eq!(
            lifecycle_request.document_research_request_status,
            DocumentResearchRequestStatus::DocumentResearchRequestCancelled
        );
        assert_eq!(
            lifecycle_request.terminal_explanation.as_deref(),
            Some("user cancellation won")
        );
        let audit = runtime
            .project_detailed_markdown_research_audit(&owner, &request_id, None, 200)
            .await
            .unwrap();
        assert_eq!(terminal_audit_item_count(&audit.items), 1);
        assert!(!audit.items.iter().any(|item| {
            item.markdown_research_execution_event_type == "markdown_research_execution_failed"
        }));
    }

    #[tokio::test]
    async fn competing_terminal_trace_commands_observe_one_atomic_winner() {
        let owner = principal("terminal-command-race-owner");
        let runtime =
            TraceableMarkdownResearchRuntime::in_memory(Arc::new(DeterministicGateway)).unwrap();
        let (_snapshot_id, _conversation_id, request_id, prepared) =
            prepare_fixture_runtime(&runtime, &owner).await;
        let failed = runtime.trace.append_markdown_research_execution_events(
            &owner,
            &prepared.markdown_research_execution_id,
            command_id("terminal-race", &request_id, "failed"),
            Utc::now(),
            vec![MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed {
                error_code: "fixture_failure".to_owned(),
                failure_explanation: "fixture terminal race".to_owned(),
            }],
        );
        let cancelled = runtime.trace.append_markdown_research_execution_events(
            &owner,
            &prepared.markdown_research_execution_id,
            command_id("terminal-race", &request_id, "cancelled"),
            Utc::now(),
            vec![
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancellationRequested {
                    cancellation_explanation: Some("fixture cancellation".to_owned()),
                },
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancelled {
                    cancellation_explanation: Some("fixture cancellation".to_owned()),
                },
            ],
        );
        let (failed_observation, cancelled_observation) = tokio::join!(failed, cancelled);
        let failed_observation = failed_observation.unwrap();
        let cancelled_observation = cancelled_observation.unwrap();
        assert_eq!(failed_observation.terminal_state, cancelled_observation.terminal_state);

        let audit = runtime
            .project_detailed_markdown_research_audit(&owner, &request_id, None, 200)
            .await
            .unwrap();
        assert_eq!(terminal_audit_item_count(&audit.items), 1);
    }

    #[tokio::test]
    async fn b11_cross_owner_commands_are_denied_without_object_disclosure() {
        let owner = principal("b11-owner");
        let foreign = principal("b11-foreign");
        let runtime =
            TraceableMarkdownResearchRuntime::in_memory(Arc::new(DeterministicGateway)).unwrap();
        let (_snapshot_id, conversation_id, request_id, _prepared) =
            prepare_fixture_runtime(&runtime, &owner).await;

        let error = runtime
            .load_document_research_request(&foreign, &conversation_id, &request_id)
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::ObjectNotAvailable { .. }));
        let error =
            runtime.execute_prepared_markdown_research(&foreign, &request_id).await.unwrap_err();
        assert!(matches!(error, RuntimeError::ObjectNotAvailable { .. }));
        let error = runtime
            .project_markdown_research_execution_overview(&foreign, &request_id)
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::ObjectNotAvailable { .. }));

        let result = runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
        assert_eq!(result.public_markdown_research_answers.len(), 1);
    }

    #[tokio::test]
    async fn b11_transient_model_failure_resumes_from_the_persisted_checkpoint() {
        let owner = principal("b11-recovery-owner");
        let runtime = TraceableMarkdownResearchRuntime::in_memory(Arc::new(
            FailOnceExecutionGateway::default(),
        ))
        .unwrap();
        let (_snapshot_id, _conversation_id, request_id, _prepared) =
            prepare_fixture_runtime(&runtime, &owner).await;

        let first_error =
            runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap_err();
        assert!(matches!(first_error, RuntimeError::ModelTransport { retryable: true, .. }));
        let resumed =
            runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
        assert_eq!(resumed.public_markdown_research_answers.len(), 1);
        assert_eq!(resumed.markdown_research_execution_overview.verbatim_source_evidence_count, 1);
    }

    #[tokio::test]
    async fn b11_trace_payload_tamper_is_rejected_during_replay() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("payload-tamper.sqlite");
        let owner = principal("b11-payload-owner");
        let gateway = Arc::new(DeterministicGateway);
        let (_snapshot_id, _conversation_id, request_id, prepared) = {
            let runtime =
                TraceableMarkdownResearchRuntime::open(&database_path, gateway.clone()).unwrap();
            prepare_fixture_runtime(&runtime, &owner).await
        };
        drop_update_trigger(&Connection::open(&database_path).unwrap());
        mutate_execution_event_payload(
            &database_path,
            &prepared.markdown_research_execution_id,
            "markdown_research_execution_started",
            |value| value["payload"] = Value::Object(serde_json::Map::new()),
        );

        let reopened = TraceableMarkdownResearchRuntime::open(&database_path, gateway).unwrap();
        let error = reopened
            .project_markdown_research_execution_overview(&owner, &request_id)
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::CorruptState { stage: RuntimeStage::Trace, .. }));
    }

    #[tokio::test]
    async fn b11_trace_owner_and_snapshot_hash_tamper_are_rejected() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("ownership-tamper.sqlite");
        let owner = principal("b11-owner-tamper");
        let gateway = Arc::new(DeterministicGateway);
        let (snapshot_id, _conversation_id, request_id, prepared) = {
            let runtime =
                TraceableMarkdownResearchRuntime::open(&database_path, gateway.clone()).unwrap();
            prepare_fixture_runtime(&runtime, &owner).await
        };
        mutate_execution_event_owner(
            &database_path,
            &prepared.markdown_research_execution_id,
            1,
            "b11-tampered-owner",
        );
        let reopened =
            TraceableMarkdownResearchRuntime::open(&database_path, gateway.clone()).unwrap();
        let error = reopened
            .project_markdown_research_execution_overview(&owner, &request_id)
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Trace }));
        drop(reopened);

        mutate_execution_event_owner(
            &database_path,
            &prepared.markdown_research_execution_id,
            1,
            owner.subject_id.as_str(),
        );

        mutate_snapshot_hash(&database_path, &snapshot_id);
        let reopened = TraceableMarkdownResearchRuntime::open(&database_path, gateway).unwrap();
        let error =
            reopened.execute_prepared_markdown_research(&owner, &request_id).await.unwrap_err();
        assert!(matches!(error, RuntimeError::CorruptState { stage: RuntimeStage::Corpus, .. }));
    }

    #[tokio::test]
    async fn b11_terminal_trace_offset_and_citation_hash_tamper_are_rejected() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("evidence-tamper.sqlite");
        let owner = principal("b11-evidence-owner");
        let gateway = Arc::new(DeterministicGateway);
        let (request_id, prepared) = {
            let runtime =
                TraceableMarkdownResearchRuntime::open(&database_path, gateway.clone()).unwrap();
            let (_snapshot_id, _conversation_id, request_id, prepared) =
                prepare_fixture_runtime(&runtime, &owner).await;
            runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
            (request_id, prepared)
        };

        mutate_execution_event_payload(
            &database_path,
            &prepared.markdown_research_execution_id,
            "verbatim_source_evidence_accepted",
            |value| {
                value["payload"]["verbatim_source_evidence"]["verbatim_source_evidence_start_byte_offset"] =
                    Value::from(1_000_000_u64);
            },
        );
        let reopened =
            TraceableMarkdownResearchRuntime::open(&database_path, gateway.clone()).unwrap();
        let error =
            reopened.execute_prepared_markdown_research(&owner, &request_id).await.unwrap_err();
        assert!(matches!(error, RuntimeError::CorruptState { .. }));
        drop(reopened);

        mutate_execution_event_payload(
            &database_path,
            &prepared.markdown_research_execution_id,
            "verbatim_source_evidence_accepted",
            |value| {
                value["payload"]["verbatim_source_evidence"]["verbatim_source_evidence_start_byte_offset"] =
                    Value::from(0_u64);
                value["payload"]["public_source_citation"]["markdown_source_document_version_content_hash"] =
                    Value::String("sha256:tampered".to_owned());
            },
        );
        let reopened = TraceableMarkdownResearchRuntime::open(&database_path, gateway).unwrap();
        let error =
            reopened.execute_prepared_markdown_research(&owner, &request_id).await.unwrap_err();
        assert!(matches!(error, RuntimeError::CorruptState { .. }));
    }

    #[tokio::test]
    async fn b11_invalid_audit_cursor_is_rejected_at_projection_seam() {
        let owner = principal("b11-cursor-owner");
        let runtime =
            TraceableMarkdownResearchRuntime::in_memory(Arc::new(DeterministicGateway)).unwrap();
        let (_snapshot_id, _conversation_id, request_id, _prepared) =
            prepare_fixture_runtime(&runtime, &owner).await;
        runtime.execute_prepared_markdown_research(&owner, &request_id).await.unwrap();
        let error = runtime
            .project_detailed_markdown_research_audit(
                &owner,
                &request_id,
                Some("tampered-cursor"),
                10,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::Validation { stage: RuntimeStage::Projection, .. }));
    }

    #[tokio::test]
    async fn cancelling_prepared_execution_updates_trace_and_lifecycle() {
        let runtime =
            TraceableMarkdownResearchRuntime::in_memory(Arc::new(DeterministicGateway)).unwrap();
        let owner = principal("subject-cancel");
        let snapshot =
            runtime.publish_markdown_corpus_snapshot(&owner, fixture_input()).await.unwrap();
        let conversation_id = runtime.create_document_research_conversation(&owner).await.unwrap();
        let started = runtime
            .start_document_research_request(
                &owner,
                &conversation_id,
                "cancel me",
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        runtime
            .prepare_markdown_research_execution(
                &owner,
                &conversation_id,
                &started.document_research_request_id,
                &snapshot,
                "fixture-strong",
                "fixture-cheap",
                MarkdownResearchExecutionLimits::default(),
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        runtime
            .cancel_document_research_request(
                &owner,
                &conversation_id,
                &started.document_research_request_id,
                Some("user stopped".to_owned()),
            )
            .await
            .unwrap();
        let conversation =
            runtime.load_document_research_conversation(&owner, &conversation_id).await.unwrap();
        assert_eq!(
            conversation
                .document_research_request(&started.document_research_request_id)
                .unwrap()
                .document_research_request_status,
            DocumentResearchRequestStatus::DocumentResearchRequestCancelled
        );
        let state = runtime
            .load_execution_for_request(&owner, &started.document_research_request_id)
            .await
            .unwrap();
        assert_eq!(state.terminal_state, Some(MarkdownResearchExecutionTerminalState::Cancelled));
    }

    #[tokio::test]
    async fn retryable_model_failure_keeps_the_conversation_resumable() {
        let runtime = TraceableMarkdownResearchRuntime::in_memory(Arc::new(
            FailOnceExecutionGateway::default(),
        ))
        .unwrap();
        let owner = principal("subject-failure");
        let snapshot =
            runtime.publish_markdown_corpus_snapshot(&owner, fixture_input()).await.unwrap();
        let conversation_id = runtime.create_document_research_conversation(&owner).await.unwrap();
        let started = runtime
            .start_document_research_request(
                &owner,
                &conversation_id,
                "fail me",
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        runtime
            .prepare_markdown_research_execution(
                &owner,
                &conversation_id,
                &started.document_research_request_id,
                &snapshot,
                "fixture-strong",
                "fixture-cheap",
                MarkdownResearchExecutionLimits::default(),
                vec![AnswerCompositionStyle::ModelKnowledgeLed],
            )
            .await
            .unwrap();
        let error = runtime
            .execute_prepared_markdown_research(&owner, &started.document_research_request_id)
            .await
            .unwrap_err();
        assert_eq!(error.code(), crate::error::RuntimeErrorCode::ModelTransport);
        let conversation =
            runtime.load_document_research_conversation(&owner, &conversation_id).await.unwrap();
        assert_eq!(
            conversation
                .document_research_request(&started.document_research_request_id)
                .unwrap()
                .document_research_request_status,
            DocumentResearchRequestStatus::MarkdownResearchExecutionRunning
        );
    }
}

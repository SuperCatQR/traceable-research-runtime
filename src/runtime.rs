use std::{
    collections::{HashMap, HashSet},
    env,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::clarification::{research_preparation_failed_event, research_run_failed_event};
use crate::{
    BraveSearchClient, CLARIFICATION_PROMPT, ClarificationError, ClarificationEvent,
    ClarificationEventKind, ClarificationEventLog, ClarificationLocks,
    ClarificationModelParseOutcome, ClarificationState, ClarificationStatus,
    EmbeddedSnapshotClient, FrozenResearchBrief, LiveResearchBackend, ModelKnowledgeDraft,
    OpenAiCompatibleModelClient, ResearchAnswerComparison, ResearchAnswerStyle,
    ResearchClaimOrigin, ResearchError, RunHeader, SnapshotReader, SnapshotRef, SnapshotWriter,
    TracePolicy, TraceWriter, clarification_cancelled_event,
    clarification_user_message_event_with_operation, events_from_clarification_model_output,
    parse_clarification_model_attempt, replay_clarification, replay_trace,
    research_run::{EvidenceSource, ResearchRunExecutor, ResearchRunOutput},
    research_run_prepared_event_with_answer_style, validate_trace_policy,
};

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ResearchInfrastructureConfig {
    brave_search_api_key: String,
    pub research_data_dir: PathBuf,
}

impl ResearchInfrastructureConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            brave_search_api_key: required_env("BRAVE_SEARCH_API_KEY")?,
            research_data_dir: env::var_os("TRACEABLE_SEARCH_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data")),
        })
    }
}

#[derive(Clone)]
pub struct ModelAccessConfig {
    api_base_url: String,
    api_key: String,
    model_id: String,
    require_public_endpoint: bool,
}

impl ModelAccessConfig {
    pub fn new(
        api_base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::with_endpoint_policy(api_base_url, api_key, model_id, false)
    }

    pub fn new_public(
        api_base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::with_endpoint_policy(api_base_url, api_key, model_id, true)
    }

    fn with_endpoint_policy(
        api_base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_id: impl Into<String>,
        require_public_endpoint: bool,
    ) -> anyhow::Result<Self> {
        let api_base_url = api_base_url.into().trim().to_owned();
        let model_id = model_id.into().trim().to_owned();
        anyhow::ensure!(
            !api_base_url.is_empty(),
            "model API base URL must not be empty"
        );
        anyhow::ensure!(!model_id.is_empty(), "model ID must not be empty");
        let parsed_url = Url::parse(&api_base_url)?;
        anyhow::ensure!(
            matches!(parsed_url.scheme(), "http" | "https"),
            "model API base URL must use HTTP or HTTPS"
        );
        parsed_url.join("chat/completions")?;
        Ok(Self {
            api_base_url,
            api_key: api_key.into(),
            model_id,
            require_public_endpoint,
        })
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Self::new(
            required_env("STRONG_MODEL_BASE_URL")?,
            required_env("STRONG_MODEL_API_KEY")?,
            required_env("STRONG_MODEL_ID")?,
        )
    }

    fn create_client(&self) -> Result<OpenAiCompatibleModelClient, ResearchError> {
        if self.require_public_endpoint {
            OpenAiCompatibleModelClient::new_public(
                &self.api_base_url,
                &self.api_key,
                &self.model_id,
            )
        } else {
            OpenAiCompatibleModelClient::new(&self.api_base_url, &self.api_key, &self.model_id)
        }
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    env::var(name).map_err(|_| anyhow::anyhow!("required environment variable {name} is not set"))
}

#[derive(Debug, thiserror::Error)]
pub enum ResearchPreparationError {
    #[error(transparent)]
    Clarification(#[from] ClarificationError),
    #[error(transparent)]
    Conversation(#[from] crate::ConversationError),
    #[error(transparent)]
    ResearchTrace(#[from] ResearchError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedResearchRun {
    pub run_id: String,
    pub brief: FrozenResearchBrief,
    pub session_id: Option<String>,
    pub turn: Option<u64>,
    pub conversation_history: Vec<crate::CompletedTurnContext>,
    policy: TracePolicy,
    answer_style: ResearchAnswerStyle,
}

enum TerminalRecovery {
    None,
    RecoveredAnswer(ResearchAnswerResponse),
    Failed(ResearchError),
}

#[derive(Debug, thiserror::Error)]
pub enum ResearchRuntimeError {
    #[error(transparent)]
    Clarification(#[from] ClarificationError),
    #[error(transparent)]
    Conversation(#[from] crate::ConversationError),
    #[error(transparent)]
    Preparation(#[from] ResearchPreparationError),
    #[error(transparent)]
    ResearchExecution(#[from] ResearchError),
    #[error(transparent)]
    JsonSerialization(#[from] serde_json::Error),
    #[error("invalid model output: {0}")]
    ModelOutput(String),
}

#[derive(Clone)]
pub struct TraceableResearchRuntime {
    infrastructure: ResearchInfrastructureConfig,
    clarification_locks: ClarificationLocks,
    conversation_locks: crate::ConversationLocks,
}

fn validate_policy(policy: &TracePolicy) -> Result<(), ClarificationError> {
    validate_trace_policy(policy).map_err(ClarificationError::InvalidEvent)
}

fn require_failed_clarification(
    clarification: &ClarificationState,
    requested_revision: u32,
) -> Result<(), ClarificationError> {
    if clarification.revision != requested_revision {
        return Err(ClarificationError::StaleRevision {
            current_revision: clarification.revision,
            requested_revision,
        });
    }
    if clarification.status != ClarificationStatus::ModelRequestFailed {
        return Err(ClarificationError::InvalidTransition {
            status: clarification.status,
            event: "intake_recovery",
        });
    }
    Ok(())
}

impl TraceableResearchRuntime {
    pub fn new(infrastructure: ResearchInfrastructureConfig) -> Self {
        Self {
            infrastructure,
            clarification_locks: ClarificationLocks::default(),
            conversation_locks: crate::ConversationLocks::default(),
        }
    }

    pub fn generate_research_run_id(&self) -> String {
        format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
            std::process::id(),
            RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
    }

    pub fn research_trace_path(&self, run_id: &str) -> PathBuf {
        self.infrastructure
            .research_data_dir
            .join("traces")
            .join(format!("{run_id}.jsonl"))
    }

    #[must_use]
    pub fn clarification_trace_path(&self, clarification_id: &str) -> PathBuf {
        self.infrastructure
            .research_data_dir
            .join("intake")
            .join(format!("{clarification_id}.jsonl"))
    }

    pub async fn create_conversation(
        &self,
    ) -> Result<crate::ResearchConversation, ResearchRuntimeError> {
        let session_id = format!("session-{}", self.generate_research_run_id());
        self.create_conversation_idempotent(&session_id, Utc::now())
            .await
    }

    pub async fn create_conversation_idempotent(
        &self,
        session_id: &str,
        created_at: chrono::DateTime<Utc>,
    ) -> Result<crate::ResearchConversation, ResearchRuntimeError> {
        let _guard = self.conversation_locks.lock(session_id).await?;
        let event =
            crate::ConversationEvent::new(crate::ConversationEventKind::ConversationStarted {
                session_id: session_id.to_owned(),
                created_at,
            });
        let log = crate::ConversationEventLog::create_idempotent(
            self.infrastructure.research_data_dir.join("sessions"),
            event,
        )?;
        Ok(log.conversation().clone())
    }

    pub async fn load_conversation(
        &self,
        session_id: &str,
    ) -> Result<crate::ResearchConversation, ResearchRuntimeError> {
        let _guard = self.conversation_locks.lock(session_id).await?;
        Ok(crate::replay_conversation(
            self.infrastructure.research_data_dir.join("sessions"),
            session_id,
        )?)
    }

    pub async fn load_clarification(
        &self,
        clarification_id: &str,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let _guard = self.clarification_locks.lock(clarification_id).await?;
        Ok(replay_clarification(
            self.infrastructure.research_data_dir.join("intake"),
            clarification_id,
        )?)
    }

    pub async fn start_research_turn(
        &self,
        session_id: &str,
        question: &str,
        model_access: &ModelAccessConfig,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let clarification_id = self.generate_research_run_id();
        let started_at = Utc::now();
        self.start_research_turn_idempotent(
            session_id,
            &clarification_id,
            &clarification_id,
            question,
            started_at,
            model_access,
        )
        .await
    }

    pub async fn start_research_turn_idempotent(
        &self,
        session_id: &str,
        clarification_id: &str,
        operation_id: &str,
        question: &str,
        started_at: chrono::DateTime<Utc>,
        model_access: &ModelAccessConfig,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let _conversation_guard = self.conversation_locks.lock(session_id).await?;
        let mut conversation_log = crate::ConversationEventLog::open(
            self.infrastructure.research_data_dir.join("sessions"),
            session_id,
        )?;
        let turn = conversation_log
            .conversation()
            .pending_turns
            .iter()
            .find(|pending| pending.clarification_id == clarification_id)
            .map(|pending| pending.turn)
            .or_else(|| {
                conversation_log
                    .conversation()
                    .completed_turns
                    .iter()
                    .find(|completed| completed.clarification_id == clarification_id)
                    .map(|completed| completed.turn)
            })
            .or_else(|| {
                conversation_log
                    .conversation()
                    .cancelled_turns
                    .iter()
                    .find(|cancelled| cancelled.clarification_id == clarification_id)
                    .map(|cancelled| cancelled.turn)
            })
            .or_else(|| {
                conversation_log
                    .conversation()
                    .failed_turns
                    .iter()
                    .find(|failed| failed.clarification_id == clarification_id)
                    .map(|failed| failed.turn)
            })
            .unwrap_or_else(|| conversation_log.conversation().next_turn_number());
        let conversation_history = conversation_log.conversation().completed_turn_history();
        let started = ClarificationEvent::new(ClarificationEventKind::ClarificationStarted {
            clarification_id: clarification_id.to_owned(),
            original_question: question.to_owned(),
            revision: 0,
            created_at: started_at,
            operation_id: Some(operation_id.to_owned()),
            session_id: Some(session_id.to_owned()),
            turn: Some(turn),
            conversation_history,
        });
        let _clarification_guard = self.clarification_locks.lock(clarification_id).await?;
        let clarification_logs_dir = self.infrastructure.research_data_dir.join("intake");
        let mut log = ClarificationEventLog::create_idempotent(&clarification_logs_dir, started)?;
        let turn_started =
            crate::ConversationEvent::new(crate::ConversationEventKind::TurnStarted {
                session_id: session_id.to_owned(),
                turn,
                clarification_id: clarification_id.to_owned(),
                user_question: question.to_owned(),
                started_at,
            });
        if let Err(error) = conversation_log.append(&turn_started) {
            return Err(error.into());
        }
        if log.clarification().status == ClarificationStatus::ModelEvaluationPending
            && let Err(error) = self.advance_clarification(&mut log, model_access).await
        {
            conversation_log.append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnCancelled {
                    session_id: session_id.to_owned(),
                    turn,
                    clarification_id: clarification_id.to_owned(),
                    cancelled_at: Utc::now(),
                },
            ))?;
            return Err(error);
        }
        Ok(log.clarification().clone())
    }

    pub async fn start_single_turn_conversation(
        &self,
        question: &str,
        model_access: &ModelAccessConfig,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let conversation = self.create_conversation().await?;
        self.start_research_turn(&conversation.session_id, question, model_access)
            .await
    }

    pub async fn submit_dialogue_message(
        &self,
        clarification_id: &str,
        revision: u32,
        message: &str,
        model_access: &ModelAccessConfig,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let operation_id = self.generate_research_run_id();
        self.submit_dialogue_message_idempotent(
            clarification_id,
            &operation_id,
            revision,
            message,
            Utc::now(),
            model_access,
        )
        .await
    }

    pub async fn submit_dialogue_message_idempotent(
        &self,
        clarification_id: &str,
        operation_id: &str,
        revision: u32,
        message: &str,
        received_at: chrono::DateTime<Utc>,
        model_access: &ModelAccessConfig,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let _guard = self.clarification_locks.lock(clarification_id).await?;
        let mut log = ClarificationEventLog::open(
            self.infrastructure.research_data_dir.join("intake"),
            clarification_id,
        )?;
        let already_applied = log.clarification().has_operation_id(operation_id);
        if !already_applied {
            log.append(&clarification_user_message_event_with_operation(
                log.clarification(),
                revision,
                message,
                Some(operation_id),
                received_at,
            )?)?;
        }
        if !already_applied
            || log.clarification().status == ClarificationStatus::ModelEvaluationPending
        {
            self.advance_clarification(&mut log, model_access).await?;
        }
        Ok(log.clarification().clone())
    }

    pub async fn retry_clarification(
        &self,
        clarification_id: &str,
        revision: u32,
        model_access: &ModelAccessConfig,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let _guard = self.clarification_locks.lock(clarification_id).await?;
        let mut log = ClarificationEventLog::open(
            self.infrastructure.research_data_dir.join("intake"),
            clarification_id,
        )?;
        require_failed_clarification(log.clarification(), revision)?;
        self.advance_clarification(&mut log, model_access).await?;
        Ok(log.clarification().clone())
    }

    /// Prepares a model-approved intake for research. This freezes execution
    /// policy after the model has decided the natural dialogue is sufficient.
    pub async fn prepare_research_run(
        &self,
        clarification_id: &str,
        policy: TracePolicy,
    ) -> Result<PreparedResearchRun, ResearchRuntimeError> {
        self.prepare_research_run_with_answer_style(
            clarification_id,
            policy,
            ResearchAnswerStyle::WebFirst,
        )
        .await
    }

    pub async fn prepare_research_run_with_answer_style(
        &self,
        clarification_id: &str,
        policy: TracePolicy,
        answer_style: ResearchAnswerStyle,
    ) -> Result<PreparedResearchRun, ResearchRuntimeError> {
        let clarification = {
            let _guard = self.clarification_locks.lock(clarification_id).await?;
            ClarificationEventLog::open(
                self.infrastructure.research_data_dir.join("intake"),
                clarification_id,
            )?
            .clarification()
            .clone()
        };
        let content_hash = clarification.content_hash.as_deref().ok_or_else(|| {
            ClarificationError::InvalidEvent("completed intake has no content_hash".into())
        })?;
        Ok(self
            .prepare_completed_clarification_run_with_answer_style(
                clarification_id,
                clarification.revision,
                content_hash,
                policy,
                answer_style,
            )
            .await?)
    }

    pub async fn cancel_clarification(
        &self,
        clarification_id: &str,
        revision: u32,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let cancelled = {
            let _guard = self.clarification_locks.lock(clarification_id).await?;
            let mut log = ClarificationEventLog::open(
                self.infrastructure.research_data_dir.join("intake"),
                clarification_id,
            )?;
            if log.clarification().revision != revision {
                return Err(ClarificationError::StaleRevision {
                    current_revision: log.clarification().revision,
                    requested_revision: revision,
                }
                .into());
            }
            if log.clarification().status != ClarificationStatus::Cancelled {
                log.append(&clarification_cancelled_event(
                    log.clarification(),
                    Utc::now(),
                ))?;
            }
            log.clarification().clone()
        };
        if let (Some(session_id), Some(turn)) = (cancelled.session_id.clone(), cancelled.turn) {
            let _conversation_guard = self.conversation_locks.lock(&session_id).await?;
            let mut conversation_log = crate::ConversationEventLog::open(
                self.infrastructure.research_data_dir.join("sessions"),
                &session_id,
            )?;
            conversation_log.append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnCancelled {
                    session_id,
                    turn,
                    clarification_id: clarification_id.to_owned(),
                    cancelled_at: Utc::now(),
                },
            ))?;
        }
        Ok(cancelled)
    }

    /// Records an automatic setup failure after a run was already prepared.
    /// This is an internal terminalization path, not a user-facing command.
    pub async fn terminalize_prepared_research_failure(
        &self,
        clarification_id: &str,
        run_id: &str,
        failure_summary: &str,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let terminal = {
            let _guard = self.clarification_locks.lock(clarification_id).await?;
            let mut log = ClarificationEventLog::open(
                self.infrastructure.research_data_dir.join("intake"),
                clarification_id,
            )?;
            match log.clarification().status {
                ClarificationStatus::ResearchPrepared => {
                    let event = research_run_failed_event(
                        log.clarification(),
                        run_id,
                        failure_summary,
                        Utc::now(),
                    )?;
                    log.append(&event)?;
                    log.clarification().clone()
                }
                ClarificationStatus::ResearchFailed => {
                    let preparation =
                        log.clarification().preparation.as_ref().ok_or_else(|| {
                            ClarificationError::InvalidEvent(
                                "research failure has no persisted preparation".into(),
                            )
                        })?;
                    if preparation.run_id != run_id {
                        return Err(ClarificationError::InvalidEvent(
                            "research failure does not match the prepared run".into(),
                        )
                        .into());
                    }
                    if log.clarification().failure.as_deref() != Some(failure_summary.trim()) {
                        return Err(ClarificationError::InvalidEvent(
                            "research failure summary differs from the persisted terminal state"
                                .into(),
                        )
                        .into());
                    }
                    log.clarification().clone()
                }
                status => {
                    return Err(ClarificationError::InvalidTransition {
                        status,
                        event: "research_run_failed",
                    }
                    .into());
                }
            }
        };

        self.record_terminalized_conversation_failure(&terminal, Some(run_id))
            .await?;
        Ok(terminal)
    }

    /// Records an automatic preparation failure before a research run exists.
    /// This is an internal terminalization path, not a user-facing command.
    pub async fn terminalize_research_preparation_failure(
        &self,
        clarification_id: &str,
        failure_summary: &str,
    ) -> Result<ClarificationState, ResearchRuntimeError> {
        let terminal = {
            let _guard = self.clarification_locks.lock(clarification_id).await?;
            let mut log = ClarificationEventLog::open(
                self.infrastructure.research_data_dir.join("intake"),
                clarification_id,
            )?;
            match log.clarification().status {
                ClarificationStatus::ResearchReady => {
                    let event = research_preparation_failed_event(
                        log.clarification(),
                        failure_summary,
                        Utc::now(),
                    )?;
                    log.append(&event)?;
                    log.clarification().clone()
                }
                ClarificationStatus::ResearchFailed => {
                    if log.clarification().preparation.is_some() {
                        return Err(ClarificationError::InvalidEvent(
                            "prepared research failure cannot be terminalized as a preparation failure"
                                .into(),
                        )
                        .into());
                    }
                    if log.clarification().failure.as_deref() != Some(failure_summary.trim()) {
                        return Err(ClarificationError::InvalidEvent(
                            "research failure summary differs from the persisted terminal state"
                                .into(),
                        )
                        .into());
                    }
                    log.clarification().clone()
                }
                status => {
                    return Err(ClarificationError::InvalidTransition {
                        status,
                        event: "research_preparation_failed",
                    }
                    .into());
                }
            }
        };

        self.record_terminalized_conversation_failure(&terminal, None)
            .await?;
        Ok(terminal)
    }

    async fn record_terminalized_conversation_failure(
        &self,
        clarification: &ClarificationState,
        run_id: Option<&str>,
    ) -> Result<(), ResearchRuntimeError> {
        let (Some(session_id), Some(turn)) =
            (clarification.session_id.as_deref(), clarification.turn)
        else {
            return Ok(());
        };
        let _conversation_guard = self.conversation_locks.lock(session_id).await?;
        let mut conversation_log = crate::ConversationEventLog::open(
            self.infrastructure.research_data_dir.join("sessions"),
            session_id,
        )?;
        conversation_log.append(&crate::ConversationEvent::new(
            crate::ConversationEventKind::TurnFailed {
                session_id: session_id.to_owned(),
                turn,
                clarification_id: clarification.clarification_id.clone(),
                run_id: run_id.map(str::to_owned),
                failed_at: Utc::now(),
            },
        ))?;
        Ok(())
    }

    async fn advance_clarification(
        &self,
        log: &mut ClarificationEventLog,
        model_access: &ModelAccessConfig,
    ) -> Result<(), ResearchRuntimeError> {
        let client = model_access.create_client()?;
        let base_input = serde_json::to_string(&serde_json::json!({
            "original_question": log.clarification().original_question,
            "conversation_history": log.clarification().conversation_history,
            "dialogue": log.clarification().dialogue,
            "current_brief": log.clarification().brief_draft,
        }))?;
        let prompt = CLARIFICATION_PROMPT;
        let mut correction = None;
        for attempt in 1..=2 {
            let user = match correction.take() {
                Some(error) => format!(
                    "{base_input}\nYour previous JSON was invalid: {error}. Return one corrected JSON object only."
                ),
                None => base_input.clone(),
            };
            let content = match client.generate_text(prompt, &user).await {
                Ok(value) => value,
                Err(error) => {
                    let event = crate::clarification_model_request_failed_event(
                        log.clarification(),
                        error.to_string(),
                        Utc::now(),
                    );
                    log.append(&event)?;
                    return Ok(());
                }
            };
            match parse_clarification_model_attempt(
                log.clarification(),
                &content,
                attempt,
                Utc::now(),
            )? {
                ClarificationModelParseOutcome::Accepted(output) => {
                    for event in events_from_clarification_model_output(
                        log.clarification(),
                        output,
                        Utc::now(),
                    )? {
                        log.append(&event)?;
                    }
                    return Ok(());
                }
                ClarificationModelParseOutcome::RetryCorrection { error } => {
                    correction = Some(error)
                }
                ClarificationModelParseOutcome::Failed(event) => {
                    log.append(&event)?;
                    return Ok(());
                }
            }
        }
        unreachable!("two model attempts always return")
    }

    /// Freezes execution policy for one model-approved dialogue, then creates
    /// its trace header. Repeating preparation reuses the persisted run id; a
    /// crash after the intake append but before trace creation is repaired on
    /// the next call.
    pub async fn prepare_completed_clarification_run(
        &self,
        clarification_id: &str,
        requested_revision: u32,
        requested_content_hash: &str,
        policy: TracePolicy,
    ) -> Result<PreparedResearchRun, ResearchPreparationError> {
        self.prepare_completed_clarification_run_with_answer_style(
            clarification_id,
            requested_revision,
            requested_content_hash,
            policy,
            ResearchAnswerStyle::WebFirst,
        )
        .await
    }

    pub async fn prepare_completed_clarification_run_with_answer_style(
        &self,
        clarification_id: &str,
        requested_revision: u32,
        requested_content_hash: &str,
        policy: TracePolicy,
        answer_style: ResearchAnswerStyle,
    ) -> Result<PreparedResearchRun, ResearchPreparationError> {
        let _guard = self.clarification_locks.lock(clarification_id).await?;
        let mut log = ClarificationEventLog::open(
            self.infrastructure.research_data_dir.join("intake"),
            clarification_id,
        )?;

        let preparation = if log.clarification().status == ClarificationStatus::ResearchPrepared {
            if requested_revision != log.clarification().revision {
                return Err(ClarificationError::StaleRevision {
                    current_revision: log.clarification().revision,
                    requested_revision,
                }
                .into());
            }
            let preparation = log
                .clarification()
                .preparation
                .clone()
                .expect("prepared intake must retain preparation");
            if requested_content_hash != preparation.brief.content_hash() {
                return Err(ClarificationError::StaleContentHash {
                    current_hash: preparation.brief.content_hash().to_owned(),
                    requested_hash: requested_content_hash.to_owned(),
                }
                .into());
            }
            preparation
        } else {
            validate_policy(&policy)?;
            let event = research_run_prepared_event_with_answer_style(
                log.clarification(),
                requested_revision,
                requested_content_hash,
                self.generate_research_run_id(),
                policy.clone(),
                answer_style,
                Utc::now(),
            )?;
            log.append(&event)?;
            log.clarification()
                .preparation
                .clone()
                .expect("appended run preparation must project preparation")
        };
        let frozen_policy = preparation.policy.clone();
        let frozen_answer_style = preparation.answer_style;

        let header = RunHeader {
            run_id: preparation.run_id.clone(),
            clarification_id: clarification_id.to_owned(),
            session_id: log.clarification().session_id.clone(),
            turn: log.clarification().turn,
            brief: preparation.brief.clone(),
            started_at: *preparation.brief.frozen_at(),
            policy: frozen_policy.clone(),
            answer_style: frozen_answer_style,
        };
        let trace_dir = self.infrastructure.research_data_dir.join("traces");
        match TraceWriter::create(&trace_dir, header.clone()) {
            Ok(writer) => drop(writer),
            Err(ResearchError::Trace(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                let replayed = replay_trace(self.research_trace_path(&header.run_id))?;
                if replayed.header != header {
                    return Err(ResearchError::Trace(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "existing trace header does not match frozen intake",
                    ))
                    .into());
                }
            }
            Err(error) => return Err(error.into()),
        }

        let session_id = log.clarification().session_id.clone();
        let turn = log.clarification().turn;
        let conversation_history = log.clarification().conversation_history.clone();
        Ok(PreparedResearchRun {
            run_id: preparation.run_id,
            brief: preparation.brief,
            session_id,
            turn,
            conversation_history,
            policy: frozen_policy,
            answer_style: frozen_answer_style,
        })
    }

    pub async fn execute_prepared_research(
        &self,
        prepared: PreparedResearchRun,
        model_access: &ModelAccessConfig,
    ) -> Result<ResearchAnswerResponse, ResearchError> {
        let header = RunHeader {
            run_id: prepared.run_id.clone(),
            clarification_id: prepared.brief.clarification_id().to_owned(),
            session_id: prepared.session_id.clone(),
            turn: prepared.turn,
            started_at: *prepared.brief.frozen_at(),
            brief: prepared.brief.clone(),
            policy: prepared.policy.clone(),
            answer_style: prepared.answer_style,
        };
        let _conversation_guard = match header.session_id.as_deref() {
            Some(session_id) => Some(
                self.conversation_locks
                    .lock(session_id)
                    .await
                    .map_err(research_setup_error)?,
            ),
            None => None,
        };
        match self.recover_completed_research(&header)? {
            TerminalRecovery::RecoveredAnswer(answer) => return Ok(answer),
            TerminalRecovery::Failed(error) => {
                self.record_conversation_turn_failure(&header)?;
                return Err(error);
            }
            TerminalRecovery::None => {}
        }
        let conversation_history = prepared.conversation_history.clone();

        let store_path = self
            .infrastructure
            .research_data_dir
            .join("snapshots.sqlite");
        let backend = LiveResearchBackend::new(
            BraveSearchClient::new(&self.infrastructure.brave_search_api_key)
                .map_err(research_setup_error)?,
            EmbeddedSnapshotClient::new(),
            model_access.create_client().map_err(research_setup_error)?,
        );
        let snapshots = SnapshotWriter::open(&store_path).map_err(research_setup_error)?;
        let (trace, replay) = TraceWriter::resume(
            self.infrastructure.research_data_dir.join("traces"),
            &header,
        )
        .map_err(research_setup_error)?;
        let reader = SnapshotReader::open(&store_path).map_err(research_setup_error)?;
        let mut research_run_executor = ResearchRunExecutor::resume(
            header.brief.clone(),
            header.policy.clone(),
            header.answer_style,
            backend,
            snapshots,
            trace,
            replay,
            &reader,
            conversation_history,
        )
        .map_err(research_setup_error)?;
        let result = match research_run_executor.execute(store_path).await {
            Ok(result) => result,
            Err(error) => {
                if matches!(
                    self.recover_completed_research(&header)?,
                    TerminalRecovery::Failed(_)
                ) {
                    self.record_conversation_turn_failure(&header)?;
                }
                return Err(error);
            }
        };
        let answer = build_research_answer_response(result)?;
        self.record_conversation_turn_completion(&header, &answer)?;
        Ok(answer)
    }

    fn recover_completed_research(
        &self,
        header: &RunHeader,
    ) -> Result<TerminalRecovery, ResearchError> {
        let path = self.research_trace_path(&header.run_id);
        if !path.exists() {
            return Ok(TerminalRecovery::None);
        }
        let replayed = replay_trace(&path).map_err(research_setup_error)?;
        if &replayed.header != header {
            return Err(research_setup_error(
                "existing trace header does not match frozen run",
            ));
        }
        if let Some(crate::TraceEvent::RunFailed {
            error_class,
            stage,
            message,
        }) = replayed.events.last().map(|envelope| &envelope.event)
        {
            return Ok(TerminalRecovery::Failed(ResearchError::PersistedFailure {
                error_class: *error_class,
                stage: *stage,
                message: message.clone(),
            }));
        }
        if replayed.run_replay.completed_round == 0 {
            if replayed.is_terminal() {
                return Err(research_setup_error(
                    "terminal trace has no completed exploration round",
                ));
            }
            return Ok(TerminalRecovery::None);
        }
        let Some(crate::TraceEvent::ComposedResearchAnswer {
            answer,
            claims,
            comparison,
        }) = replayed.events.last().map(|envelope| &envelope.event)
        else {
            return Ok(TerminalRecovery::None);
        };
        if replayed.run_replay.exploration_stop_reason.is_none() {
            return Err(research_setup_error(
                "terminal trace has no exploration stop reason",
            ));
        }
        let answer = crate::ComposedResearchAnswer {
            answer: answer.clone(),
            claims: claims.clone(),
            comparison: comparison.clone(),
        };
        let knowledge_draft = replayed
            .events
            .iter()
            .find_map(|envelope| match &envelope.event {
                crate::TraceEvent::KnowledgeDraft { draft } => Some(draft.clone()),
                _ => None,
            })
            .ok_or_else(|| research_setup_error("terminal trace has no model knowledge draft"))?;
        let reader = SnapshotReader::open(
            self.infrastructure
                .research_data_dir
                .join("snapshots.sqlite"),
        )
        .map_err(research_setup_error)?;
        let mut sources = Vec::new();
        for reference in answer.claims.iter().flat_map(|claim| &claim.snapshot_refs) {
            if let Some(snapshot) = reader.get(reference).map_err(research_setup_error)? {
                sources.push(EvidenceSource {
                    snapshot_ref: reference.clone(),
                    url: snapshot.crawl.final_url,
                    title: snapshot.title,
                });
            }
        }
        let public = build_research_answer_response(ResearchRunOutput {
            answer,
            knowledge_draft,
            answer_style: header.answer_style,
            sources,
        })?;
        self.record_conversation_turn_completion(header, &public)?;
        Ok(TerminalRecovery::RecoveredAnswer(public))
    }

    fn record_conversation_turn_failure(&self, header: &RunHeader) -> Result<(), ResearchError> {
        let (Some(session_id), Some(turn)) = (header.session_id.as_deref(), header.turn) else {
            return Ok(());
        };
        let mut conversation_log = crate::ConversationEventLog::open(
            self.infrastructure.research_data_dir.join("sessions"),
            session_id,
        )
        .map_err(research_setup_error)?;
        conversation_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnFailed {
                    session_id: session_id.to_owned(),
                    turn,
                    clarification_id: header.clarification_id.clone(),
                    run_id: Some(header.run_id.clone()),
                    failed_at: Utc::now(),
                },
            ))
            .map_err(research_setup_error)
    }

    fn record_conversation_turn_completion(
        &self,
        header: &RunHeader,
        answer: &ResearchAnswerResponse,
    ) -> Result<(), ResearchError> {
        let (Some(session_id), Some(turn)) = (header.session_id.as_deref(), header.turn) else {
            return Ok(());
        };
        let mut conversation_log = crate::ConversationEventLog::open(
            self.infrastructure.research_data_dir.join("sessions"),
            session_id,
        )
        .map_err(research_setup_error)?;
        conversation_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnCompleted {
                    session_id: session_id.to_owned(),
                    turn,
                    clarification_id: header.clarification_id.clone(),
                    run_id: header.run_id.clone(),
                    answer: answer.answer.clone(),
                    completed_at: Utc::now(),
                },
            ))
            .map_err(research_setup_error)
    }
}

fn research_setup_error(error: impl std::fmt::Display) -> ResearchError {
    ResearchError::Setup {
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchAnswerResponse {
    #[serde(default)]
    pub answer_style: ResearchAnswerStyle,
    pub answer: String,
    #[serde(default)]
    pub knowledge_draft: ModelKnowledgeDraft,
    #[serde(default)]
    pub comparison: ResearchAnswerComparison,
    pub claims: Vec<ResearchClaimResponse>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchClaimResponse {
    pub text: String,
    #[serde(default)]
    pub origin: ResearchClaimOrigin,
    #[serde(default)]
    pub rationale: String,
    pub sources: Vec<EvidenceSourceResponse>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSourceResponse {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResearchAnswerResponse {
    pub answer: String,
    pub sources: Vec<EvidenceSourceResponse>,
}

#[must_use]
pub fn project_chat_research_answer(
    complete_answer: &ResearchAnswerResponse,
) -> ChatResearchAnswerResponse {
    let mut seen_sources = HashSet::new();
    let sources = complete_answer
        .claims
        .iter()
        .flat_map(|claim| claim.sources.iter())
        .filter(|source| seen_sources.insert((source.url.clone(), source.title.clone())))
        .cloned()
        .collect();
    ChatResearchAnswerResponse {
        answer: complete_answer.answer.clone(),
        sources,
    }
}

fn build_research_answer_response(
    result: ResearchRunOutput,
) -> Result<ResearchAnswerResponse, ResearchError> {
    let sources: HashMap<SnapshotRef, EvidenceSourceResponse> = result
        .sources
        .into_iter()
        .map(|source: EvidenceSource| {
            (
                source.snapshot_ref,
                EvidenceSourceResponse {
                    url: source.url,
                    title: source.title,
                },
            )
        })
        .collect();
    let claims = result
        .answer
        .claims
        .into_iter()
        .map(|claim| {
            let sources = match claim.origin {
                ResearchClaimOrigin::ModelKnowledge => Vec::new(),
                ResearchClaimOrigin::WebEvidence => claim
                    .snapshot_refs
                    .into_iter()
                    .map(|reference| {
                        sources.get(&reference).cloned().ok_or_else(|| {
                            ResearchError::InvalidSnapshot(format!(
                                "cited snapshot missing source metadata: {}",
                                reference.as_str()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            Ok(ResearchClaimResponse {
                text: claim.text,
                origin: claim.origin,
                rationale: claim.rationale,
                sources,
            })
        })
        .collect::<Result<Vec<_>, ResearchError>>()?;
    Ok(ResearchAnswerResponse {
        answer_style: result.answer_style,
        answer: result.answer.answer,
        knowledge_draft: result.knowledge_draft,
        comparison: result.answer.comparison,
        claims,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;
    use crate::{
        ClarificationDecision, ClarificationEvent, ClarificationEventKind,
        ClarificationModelOutput, ComposedResearchAnswer, ComposedResearchClaim,
        ModelKnowledgeDraft, SnapshotRef, events_from_clarification_model_output,
        replay_clarification,
    };

    fn test_runtime(name: &str) -> TraceableResearchRuntime {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        TraceableResearchRuntime::new(ResearchInfrastructureConfig {
            brave_search_api_key: "test-key".into(),
            research_data_dir: std::env::temp_dir().join(format!(
                "traceable-search-app-{name}-{}-{unique}",
                std::process::id()
            )),
        })
    }

    fn test_model_access() -> ModelAccessConfig {
        ModelAccessConfig::new("http://127.0.0.1:1", "", "test").unwrap()
    }

    fn completed_clarification(
        runtime: &TraceableResearchRuntime,
        clarification_id: &str,
    ) -> (u32, String) {
        let started = ClarificationEvent::new(ClarificationEventKind::ClarificationStarted {
            clarification_id: clarification_id.into(),
            original_question: "Original question, byte for byte?".into(),
            revision: 0,
            created_at: Utc::now(),
            operation_id: None,
            session_id: None,
            turn: None,
            conversation_history: Vec::new(),
        });
        let mut log = ClarificationEventLog::create(
            runtime.infrastructure.research_data_dir.join("intake"),
            started,
        )
        .unwrap();
        let output = ClarificationModelOutput {
            decision: ClarificationDecision::StartResearch,
            rationale: "fixture decision rationale".into(),
            assistant_message: "我理解你的研究问题，现在开始查证。".into(),
            brief_draft: crate::ResearchBrief {
                schema_version: crate::RESEARCH_BRIEF_SCHEMA_VERSION,
                original_question: log.clarification().original_question.clone(),
                research_question: log.clarification().original_question.clone(),
                desired_output: None,
                scope: crate::ResearchScope::default(),
                source_constraints: Vec::new(),
                accepted_assumptions: Vec::new(),
            },
        };
        for event in
            events_from_clarification_model_output(log.clarification(), output, Utc::now()).unwrap()
        {
            log.append(&event).unwrap();
        }
        (
            log.clarification().revision,
            log.clarification().content_hash.clone().unwrap(),
        )
    }

    fn ready_session_clarification(
        runtime: &TraceableResearchRuntime,
        conversation: &crate::ResearchConversation,
        clarification_id: &str,
    ) {
        let started_at = Utc::now();
        let mut conversation_log = crate::ConversationEventLog::open(
            runtime.infrastructure.research_data_dir.join("sessions"),
            &conversation.session_id,
        )
        .unwrap();
        conversation_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnStarted {
                    session_id: conversation.session_id.clone(),
                    turn: 1,
                    clarification_id: clarification_id.into(),
                    user_question: "question".into(),
                    started_at,
                },
            ))
            .unwrap();
        drop(conversation_log);

        let started = ClarificationEvent::new(ClarificationEventKind::ClarificationStarted {
            clarification_id: clarification_id.into(),
            original_question: "question".into(),
            revision: 0,
            created_at: started_at,
            operation_id: None,
            session_id: Some(conversation.session_id.clone()),
            turn: Some(1),
            conversation_history: Vec::new(),
        });
        let mut clarification_log = ClarificationEventLog::create(
            runtime.infrastructure.research_data_dir.join("intake"),
            started,
        )
        .unwrap();
        let original_question = clarification_log.clarification().original_question.clone();
        let output = ClarificationModelOutput {
            decision: ClarificationDecision::StartResearch,
            rationale: "fixture decision rationale".into(),
            assistant_message: "I understand the request and am starting research.".into(),
            brief_draft: crate::ResearchBrief {
                schema_version: crate::RESEARCH_BRIEF_SCHEMA_VERSION,
                original_question: original_question.clone(),
                research_question: original_question,
                desired_output: None,
                scope: crate::ResearchScope::default(),
                source_constraints: Vec::new(),
                accepted_assumptions: Vec::new(),
            },
        };
        for event in events_from_clarification_model_output(
            clarification_log.clarification(),
            output,
            Utc::now(),
        )
        .unwrap()
        {
            clarification_log.append(&event).unwrap();
        }
    }

    fn prepared_session_clarification(
        runtime: &TraceableResearchRuntime,
        conversation: &crate::ResearchConversation,
        clarification_id: &str,
        run_id: &str,
    ) {
        ready_session_clarification(runtime, conversation, clarification_id);
        let mut clarification_log = ClarificationEventLog::open(
            runtime.infrastructure.research_data_dir.join("intake"),
            clarification_id,
        )
        .unwrap();
        let prepared = research_run_prepared_event_with_answer_style(
            clarification_log.clarification(),
            clarification_log.clarification().revision,
            clarification_log
                .clarification()
                .content_hash
                .as_deref()
                .unwrap(),
            run_id.into(),
            test_policy(),
            ResearchAnswerStyle::WebFirst,
            Utc::now(),
        )
        .unwrap();
        clarification_log.append(&prepared).unwrap();
    }

    fn test_policy() -> TracePolicy {
        TracePolicy {
            rounds: 3,
            input_budget: 1_000,
            max_snapshots: 10,
        }
    }

    #[tokio::test]
    async fn terminalizing_prepared_research_failure_is_idempotent_and_releases_the_turn() {
        let runtime = test_runtime("prepared-terminalization");
        let conversation = runtime.create_conversation().await.unwrap();
        let clarification_id = "clarification-prepared-terminalization";
        let run_id = "run-prepared-terminalization";
        let summary = "Research could not start after preparation.";
        prepared_session_clarification(&runtime, &conversation, clarification_id, run_id);

        assert!(
            runtime
                .terminalize_prepared_research_failure(clarification_id, "other-run", summary)
                .await
                .is_err()
        );
        let prepared = runtime.load_clarification(clarification_id).await.unwrap();
        assert_eq!(prepared.status, ClarificationStatus::ResearchPrepared);

        let first = runtime
            .terminalize_prepared_research_failure(clarification_id, run_id, summary)
            .await
            .unwrap();
        assert_eq!(first.status, ClarificationStatus::ResearchFailed);
        assert_eq!(first.failure.as_deref(), Some(summary));
        let second = runtime
            .terminalize_prepared_research_failure(clarification_id, run_id, summary)
            .await
            .unwrap();
        assert_eq!(second, first);

        let replayed = runtime
            .load_conversation(&conversation.session_id)
            .await
            .unwrap();
        assert!(replayed.pending_turns.is_empty());
        assert_eq!(replayed.failed_turns.len(), 1);
        let next = runtime
            .start_research_turn(
                &conversation.session_id,
                "next question",
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(next.turn, Some(2));
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn terminalizing_research_preparation_failure_is_idempotent_and_releases_the_turn() {
        let runtime = test_runtime("preparation-terminalization");
        let conversation = runtime.create_conversation().await.unwrap();
        let clarification_id = "clarification-preparation-terminalization";
        let summary = "Research could not be prepared.";
        ready_session_clarification(&runtime, &conversation, clarification_id);

        let first = runtime
            .terminalize_research_preparation_failure(clarification_id, summary)
            .await
            .unwrap();
        assert_eq!(first.status, ClarificationStatus::ResearchFailed);
        assert!(first.preparation.is_none());
        assert_eq!(first.failure.as_deref(), Some(summary));
        let second = runtime
            .terminalize_research_preparation_failure(clarification_id, summary)
            .await
            .unwrap();
        assert_eq!(second, first);

        let replayed = runtime
            .load_conversation(&conversation.session_id)
            .await
            .unwrap();
        assert!(replayed.pending_turns.is_empty());
        assert_eq!(replayed.failed_turns.len(), 1);
        let next = runtime
            .start_research_turn(
                &conversation.session_id,
                "next question",
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(next.turn, Some(2));
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn invalid_policy_is_rejected_before_run_preparation() {
        for (name, policy) in [
            (
                "too-few-rounds",
                TracePolicy {
                    rounds: 2,
                    ..test_policy()
                },
            ),
            (
                "too-many-rounds",
                TracePolicy {
                    rounds: 6,
                    ..test_policy()
                },
            ),
            (
                "zero-budget",
                TracePolicy {
                    input_budget: 0,
                    ..test_policy()
                },
            ),
            (
                "zero-snapshots",
                TracePolicy {
                    max_snapshots: 0,
                    ..test_policy()
                },
            ),
        ] {
            let runtime = test_runtime(name);
            let clarification_id = format!("clarification-{name}");
            let (revision, content_hash) = completed_clarification(&runtime, &clarification_id);

            let error = runtime
                .prepare_completed_clarification_run(
                    &clarification_id,
                    revision,
                    &content_hash,
                    policy,
                )
                .await
                .unwrap_err();

            assert!(matches!(
                error,
                ResearchPreparationError::Clarification(ClarificationError::InvalidEvent(_))
            ));
            let replayed = replay_clarification(
                runtime.infrastructure.research_data_dir.join("intake"),
                &clarification_id,
            )
            .unwrap();
            assert_eq!(replayed.status, ClarificationStatus::ResearchReady);
            fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
        }
    }

    #[tokio::test]
    async fn repeated_run_preparation_reuses_one_run_id() {
        let runtime = test_runtime("freeze-idempotent");
        let (revision, content_hash) =
            completed_clarification(&runtime, "clarification-idempotent");

        let first = runtime
            .prepare_completed_clarification_run(
                "clarification-idempotent",
                revision,
                &content_hash,
                test_policy(),
            )
            .await
            .unwrap();
        let second = runtime
            .prepare_completed_clarification_run(
                "clarification-idempotent",
                revision,
                &content_hash,
                TracePolicy {
                    rounds: 5,
                    input_budget: 99,
                    max_snapshots: 1,
                },
            )
            .await
            .unwrap();

        assert_eq!(second, first);
        assert_eq!(second.policy, test_policy());
        let replayed = replay_clarification(
            runtime.infrastructure.research_data_dir.join("intake"),
            "clarification-idempotent",
        )
        .unwrap();
        assert_eq!(replayed.preparation.unwrap().run_id, first.run_id);
        let header = RunHeader {
            run_id: first.run_id.clone(),
            clarification_id: first.brief.clarification_id().to_owned(),
            session_id: first.session_id.clone(),
            turn: first.turn,
            brief: first.brief.clone(),
            started_at: *first.brief.frozen_at(),
            policy: first.policy.clone(),
            answer_style: first.answer_style,
        };
        let replayed = replay_trace(runtime.research_trace_path(&first.run_id)).unwrap();
        assert_eq!(replayed.header.run_id, first.run_id);
        assert_eq!(replayed.header.brief, first.brief);
        assert!(matches!(
            runtime.recover_completed_research(&header).unwrap(),
            TerminalRecovery::None
        ));
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn prepared_run_without_trace_is_repaired_with_same_run_id() {
        let runtime = test_runtime("freeze-crash-window");
        let clarification_id = "clarification-crash-window";
        let (revision, content_hash) = completed_clarification(&runtime, clarification_id);
        let mut log = ClarificationEventLog::open(
            runtime.infrastructure.research_data_dir.join("intake"),
            clarification_id,
        )
        .unwrap();
        let prepared_event = research_run_prepared_event_with_answer_style(
            log.clarification(),
            revision,
            &content_hash,
            "run-before-crash".into(),
            test_policy(),
            ResearchAnswerStyle::WebFirst,
            Utc::now(),
        )
        .unwrap();
        log.append(&prepared_event).unwrap();
        drop(log);
        assert!(!runtime.research_trace_path("run-before-crash").exists());

        let repaired = runtime
            .prepare_completed_clarification_run(
                clarification_id,
                revision,
                &content_hash,
                test_policy(),
            )
            .await
            .unwrap();

        assert_eq!(repaired.run_id, "run-before-crash");
        assert_eq!(repaired.policy, test_policy());
        let replayed = replay_trace(runtime.research_trace_path(&repaired.run_id)).unwrap();
        assert_eq!(replayed.header.run_id, repaired.run_id);
        assert_eq!(replayed.header.brief, repaired.brief);
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn persisted_run_failure_repairs_pending_conversation_turn() {
        let runtime = test_runtime("session-persisted-failure");
        let conversation = runtime.create_conversation().await.unwrap();
        let clarification_id = "clarification-persisted-failure";
        let started_at = Utc::now();
        let mut conversation_log = crate::ConversationEventLog::open(
            runtime.infrastructure.research_data_dir.join("sessions"),
            &conversation.session_id,
        )
        .unwrap();
        conversation_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnStarted {
                    session_id: conversation.session_id.clone(),
                    turn: 1,
                    clarification_id: clarification_id.into(),
                    user_question: "question".into(),
                    started_at,
                },
            ))
            .unwrap();
        drop(conversation_log);
        let started = ClarificationEvent::new(ClarificationEventKind::ClarificationStarted {
            clarification_id: clarification_id.into(),
            original_question: "Original question, byte for byte?".into(),
            revision: 0,
            created_at: started_at,
            operation_id: None,
            session_id: Some(conversation.session_id.clone()),
            turn: Some(1),
            conversation_history: Vec::new(),
        });
        let mut clarification_log = ClarificationEventLog::create(
            runtime.infrastructure.research_data_dir.join("intake"),
            started,
        )
        .unwrap();
        let output = ClarificationModelOutput {
            decision: ClarificationDecision::StartResearch,
            rationale: "fixture decision rationale".into(),
            assistant_message: "我理解你的研究问题，现在开始查证。".into(),
            brief_draft: crate::ResearchBrief {
                schema_version: crate::RESEARCH_BRIEF_SCHEMA_VERSION,
                original_question: clarification_log.clarification().original_question.clone(),
                research_question: clarification_log.clarification().original_question.clone(),
                desired_output: None,
                scope: crate::ResearchScope::default(),
                source_constraints: Vec::new(),
                accepted_assumptions: Vec::new(),
            },
        };
        for event in events_from_clarification_model_output(
            clarification_log.clarification(),
            output,
            Utc::now(),
        )
        .unwrap()
        {
            clarification_log.append(&event).unwrap();
        }
        let prepared = runtime
            .prepare_research_run(clarification_id, test_policy())
            .await
            .unwrap();
        let header = RunHeader {
            run_id: prepared.run_id.clone(),
            clarification_id: prepared.brief.clarification_id().into(),
            session_id: prepared.session_id.clone(),
            turn: prepared.turn,
            brief: prepared.brief.clone(),
            started_at: *prepared.brief.frozen_at(),
            policy: prepared.policy.clone(),
            answer_style: prepared.answer_style,
        };
        let (mut trace, _) = TraceWriter::resume(
            runtime.infrastructure.research_data_dir.join("traces"),
            &header,
        )
        .unwrap();
        trace
            .append(&crate::TraceEvent::RunFailed {
                error_class: crate::ErrorClass::External,
                stage: crate::ResearchStage::Planning,
                message: "persisted failure".into(),
            })
            .unwrap();
        drop(trace);

        let error = runtime
            .execute_prepared_research(prepared, &test_model_access())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ResearchError::PersistedFailure {
                error_class: crate::ErrorClass::External,
                stage: crate::ResearchStage::Planning,
                ..
            }
        ));
        let replayed = runtime
            .load_conversation(&conversation.session_id)
            .await
            .unwrap();
        assert!(replayed.pending_turns.is_empty());
        assert_eq!(replayed.failed_turns.len(), 1);
        assert!(replayed.completed_turn_history().is_empty());
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn research_setup_error_cancels_reserved_conversation_turn() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runtime = TraceableResearchRuntime::new(ResearchInfrastructureConfig {
            brave_search_api_key: "test-key".into(),
            research_data_dir: std::env::temp_dir().join(format!(
                "traceable-search-app-session-setup-error-{}-{unique}",
                std::process::id()
            )),
        });
        let invalid_model_access = ModelAccessConfig {
            api_base_url: "not a url".into(),
            api_key: String::new(),
            model_id: "test".into(),
            require_public_endpoint: false,
        };
        let conversation = runtime.create_conversation().await.unwrap();

        assert!(
            runtime
                .start_research_turn(
                    &conversation.session_id,
                    "first question",
                    &invalid_model_access,
                )
                .await
                .is_err()
        );
        let after_first = runtime
            .load_conversation(&conversation.session_id)
            .await
            .unwrap();
        assert!(after_first.pending_turns.is_empty());
        assert_eq!(after_first.cancelled_turns.len(), 1);
        assert_eq!(after_first.next_turn_number(), 2);

        assert!(
            runtime
                .start_research_turn(
                    &conversation.session_id,
                    "second question",
                    &invalid_model_access,
                )
                .await
                .is_err()
        );
        let after_second = runtime
            .load_conversation(&conversation.session_id)
            .await
            .unwrap();
        assert!(after_second.pending_turns.is_empty());
        assert_eq!(after_second.cancelled_turns.len(), 2);
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn conversations_are_isolated_and_replayable() {
        let runtime = test_runtime("session-isolation");
        let first = runtime.create_conversation().await.unwrap();
        let second = runtime.create_conversation().await.unwrap();

        let mut first_log = crate::ConversationEventLog::open(
            runtime.infrastructure.research_data_dir.join("sessions"),
            &first.session_id,
        )
        .unwrap();
        first_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnStarted {
                    session_id: first.session_id.clone(),
                    turn: 1,
                    clarification_id: "clarification-first".into(),
                    user_question: "first question".into(),
                    started_at: Utc::now(),
                },
            ))
            .unwrap();
        first_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnCompleted {
                    session_id: first.session_id.clone(),
                    turn: 1,
                    clarification_id: "clarification-first".into(),
                    run_id: "run-first".into(),
                    answer: "first answer".into(),
                    completed_at: Utc::now(),
                },
            ))
            .unwrap();

        let replayed_first = runtime.load_conversation(&first.session_id).await.unwrap();
        let replayed_second = runtime.load_conversation(&second.session_id).await.unwrap();
        assert_eq!(replayed_first.completed_turn_history().len(), 1);
        assert_eq!(
            replayed_first.completed_turn_history()[0].answer,
            "first answer"
        );
        assert!(replayed_second.completed_turn_history().is_empty());
        assert_eq!(replayed_second.next_turn_number(), 1);
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn later_clarification_freezes_only_its_conversation_history() {
        let runtime = test_runtime("session-history-snapshot");
        let first = runtime.create_conversation().await.unwrap();
        let second = runtime.create_conversation().await.unwrap();
        let mut first_log = crate::ConversationEventLog::open(
            runtime.infrastructure.research_data_dir.join("sessions"),
            &first.session_id,
        )
        .unwrap();
        first_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnStarted {
                    session_id: first.session_id.clone(),
                    turn: 1,
                    clarification_id: "clarification-history-1".into(),
                    user_question: "Who was Hegel?".into(),
                    started_at: Utc::now(),
                },
            ))
            .unwrap();
        first_log
            .append(&crate::ConversationEvent::new(
                crate::ConversationEventKind::TurnCompleted {
                    session_id: first.session_id.clone(),
                    turn: 1,
                    clarification_id: "clarification-history-1".into(),
                    run_id: "run-history-1".into(),
                    answer: "Hegel was a German philosopher.".into(),
                    completed_at: Utc::now(),
                },
            ))
            .unwrap();
        drop(first_log);

        let follow_up = runtime
            .start_research_turn(
                &first.session_id,
                "Who did he influence?",
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(follow_up.turn, Some(2));
        assert_eq!(follow_up.conversation_history.len(), 1);
        assert_eq!(
            follow_up.conversation_history[0].user_question,
            "Who was Hegel?"
        );
        assert_eq!(
            follow_up.conversation_history[0].answer,
            "Hegel was a German philosopher."
        );
        assert!(
            runtime
                .start_research_turn(
                    &first.session_id,
                    "A conflicting turn",
                    &test_model_access(),
                )
                .await
                .is_err()
        );

        let isolated = runtime
            .start_research_turn(
                &second.session_id,
                "Who did he influence?",
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(isolated.turn, Some(1));
        assert!(isolated.conversation_history.is_empty());
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn cancelling_failed_clarification_allows_the_next_conversation_turn() {
        let runtime = test_runtime("session-cancel-next-turn");
        let conversation = runtime.create_conversation().await.unwrap();
        let first = runtime
            .start_research_turn(
                &conversation.session_id,
                "first question",
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(first.status, ClarificationStatus::ModelRequestFailed);
        assert_eq!(first.turn, Some(1));

        runtime
            .cancel_clarification(&first.clarification_id, first.revision)
            .await
            .unwrap();
        runtime
            .cancel_clarification(&first.clarification_id, first.revision)
            .await
            .unwrap();
        let after_cancel = runtime
            .load_conversation(&conversation.session_id)
            .await
            .unwrap();
        assert!(after_cancel.pending_turns.is_empty());
        assert_eq!(after_cancel.cancelled_turns.len(), 1);

        let second = runtime
            .start_research_turn(
                &conversation.session_id,
                "second question",
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(second.turn, Some(2));
        assert!(second.conversation_history.is_empty());
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn idempotent_start_replays_reserved_files_without_duplicate_events() {
        let runtime = test_runtime("idempotent-start");
        let conversation = runtime.create_conversation().await.unwrap();
        let started_at = Utc::now();
        let first = runtime
            .start_research_turn_idempotent(
                &conversation.session_id,
                "clarification-idempotent-start",
                "operation-idempotent-start",
                "same question",
                started_at,
                &test_model_access(),
            )
            .await
            .unwrap();
        let second = runtime
            .start_research_turn_idempotent(
                &conversation.session_id,
                "clarification-idempotent-start",
                "operation-idempotent-start",
                "same question",
                started_at,
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(first, second);
        let conversation_path = runtime
            .infrastructure
            .research_data_dir
            .join("sessions")
            .join(format!("{}.jsonl", conversation.session_id));
        let conversation_text = fs::read_to_string(conversation_path).unwrap();
        assert_eq!(conversation_text.matches("turn_started").count(), 1);
        let clarification_path = runtime.clarification_trace_path(&first.clarification_id);
        let clarification_text = fs::read_to_string(clarification_path).unwrap();
        assert_eq!(clarification_text.matches("intake_started").count(), 1);
        assert_eq!(clarification_text.matches("intake_failed").count(), 1);
        assert!(second.has_operation_id("operation-idempotent-start"));
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[tokio::test]
    async fn idempotent_message_replay_uses_operation_evidence_after_model_failure() {
        let runtime = test_runtime("idempotent-message");
        let clarification_id = "clarification-idempotent-message";
        let mut log = ClarificationEventLog::create(
            runtime.infrastructure.research_data_dir.join("intake"),
            ClarificationEvent::new(ClarificationEventKind::ClarificationStarted {
                clarification_id: clarification_id.into(),
                original_question: "question".into(),
                revision: 0,
                created_at: Utc::now(),
                operation_id: None,
                session_id: None,
                turn: None,
                conversation_history: Vec::new(),
            }),
        )
        .unwrap();
        for event in events_from_clarification_model_output(
            log.clarification(),
            ClarificationModelOutput {
                decision: ClarificationDecision::ContinueDialogue,
                rationale: "Need one more detail before research.".into(),
                assistant_message: "Please add one detail.".into(),
                brief_draft: crate::ResearchBrief {
                    schema_version: crate::RESEARCH_BRIEF_SCHEMA_VERSION,
                    original_question: "question".into(),
                    research_question: "question".into(),
                    desired_output: None,
                    scope: crate::ResearchScope::default(),
                    source_constraints: Vec::new(),
                    accepted_assumptions: Vec::new(),
                },
            },
            Utc::now(),
        )
        .unwrap()
        {
            log.append(&event).unwrap();
        }
        let received_at = Utc::now();
        let first = runtime
            .submit_dialogue_message_idempotent(
                clarification_id,
                "operation-idempotent-message",
                1,
                "extra detail",
                received_at,
                &test_model_access(),
            )
            .await
            .unwrap();
        let second = runtime
            .submit_dialogue_message_idempotent(
                clarification_id,
                "operation-idempotent-message",
                1,
                "extra detail",
                received_at,
                &test_model_access(),
            )
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(second.dialogue.len(), 3);
        assert!(second.has_operation_id("operation-idempotent-message"));
        let text = fs::read_to_string(runtime.clarification_trace_path(clarification_id)).unwrap();
        assert_eq!(text.matches("user_message_received").count(), 1);
        assert_eq!(text.matches("intake_failed").count(), 1);
        fs::remove_dir_all(&runtime.infrastructure.research_data_dir).unwrap();
    }

    #[test]
    fn chat_research_answer_contains_only_answer_and_necessary_sources() {
        let reference = SnapshotRef::from_id("abc123");
        let complete_answer = build_research_answer_response(ResearchRunOutput {
            answer: ComposedResearchAnswer {
                answer: "Grounded".into(),
                claims: vec![
                    ComposedResearchClaim {
                        text: "Model view".into(),
                        origin: ResearchClaimOrigin::ModelKnowledge,
                        snapshot_refs: Vec::new(),
                        rationale: "Fixture retains this model knowledge view.".into(),
                    },
                    ComposedResearchClaim {
                        text: "Fact".into(),
                        origin: ResearchClaimOrigin::WebEvidence,
                        snapshot_refs: vec![reference.clone()],
                        rationale: "Fixture source supports this factual claim.".into(),
                    },
                ],
                comparison: ResearchAnswerComparison {
                    agreements: Vec::new(),
                    differences: Vec::new(),
                    synthesis_rationale: "fixture".into(),
                },
            },
            knowledge_draft: ModelKnowledgeDraft {
                answer: "Model view".into(),
                claims: vec!["Model view".into()],
                uncertainty: "Fixture uncertainty".into(),
                basis_summary: "Fixture model knowledge basis.".into(),
            },
            answer_style: ResearchAnswerStyle::WebFirst,
            sources: vec![EvidenceSource {
                snapshot_ref: reference,
                url: "https://example.com/final".into(),
                title: "Example".into(),
            }],
        })
        .unwrap();
        let value = serde_json::to_value(project_chat_research_answer(&complete_answer)).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "answer": "Grounded",
                "sources": [{
                    "url": "https://example.com/final",
                    "title": "Example"
                }]
            })
        );
    }
}

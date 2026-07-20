use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{
    BriefValidationError, FrozenResearchBrief, RationaleAuditStatus, ResearchAnswerStyle,
    ResearchBrief, TracePolicy, validate_decision_rationale, validate_trace_policy,
};

/// Schema v5 records a chat-native research-intake lifecycle. It deliberately
/// does not encode UI questions, options, or a user confirmation step.
pub const CLARIFICATION_EVENT_SCHEMA_VERSION: u32 = 5;

const MAX_DIALOGUE_MESSAGE_CHARS: usize = 4_000;
const MAX_RESEARCH_FAILURE_SUMMARY_CHARS: usize = 480;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClarificationStatus {
    #[serde(rename = "MODEL_EVALUATION_PENDING")]
    ModelEvaluationPending,
    #[serde(rename = "AWAITING_USER_MESSAGE")]
    AwaitingUserMessage,
    #[serde(rename = "RESEARCH_READY")]
    ResearchReady,
    #[serde(rename = "MODEL_REQUEST_FAILED")]
    ModelRequestFailed,
    #[serde(rename = "RESEARCH_PREPARED")]
    ResearchPrepared,
    #[serde(rename = "RESEARCH_FAILED")]
    ResearchFailed,
    #[serde(rename = "CANCELLED")]
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogueRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogueMessage {
    pub role: DialogueRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClarificationDecision {
    #[serde(rename = "continue_dialogue")]
    ContinueDialogue,
    #[serde(rename = "start_research")]
    StartResearch,
}

/// One model response serves two purposes: it is the natural-language reply
/// shown in the chat and it updates the model-owned structured research brief.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationModelOutput {
    pub decision: ClarificationDecision,
    pub rationale: String,
    pub assistant_message: String,
    pub brief_draft: ResearchBrief,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationEvent {
    pub schema_version: u32,
    #[serde(flatten)]
    pub kind: ClarificationEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClarificationEventKind {
    #[serde(rename = "intake_started")]
    ClarificationStarted {
        clarification_id: String,
        original_question: String,
        revision: u32,
        created_at: DateTime<Utc>,
        /// Optional durable evidence tying this event to a workspace write.
        /// It is additive so schema-v5 logs without the field remain readable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        conversation_history: Vec<crate::CompletedTurnContext>,
    },
    UserMessageReceived {
        revision: u32,
        message: String,
        received_at: DateTime<Utc>,
        /// Optional durable evidence for an idempotent dialogue write.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
    },
    /// The brief remains model-owned. `assistant_message` is the only part of
    /// this decision that the normal chat surface presents directly.
    ModelUnderstanding {
        revision: u32,
        decision: ClarificationDecision,
        rationale: String,
        assistant_message: String,
        brief: ResearchBrief,
        content_hash: String,
        evaluated_at: DateTime<Utc>,
    },
    #[serde(rename = "run_prepared")]
    ResearchRunPrepared {
        revision: u32,
        run_id: String,
        brief: FrozenResearchBrief,
        policy: TracePolicy,
        #[serde(default)]
        answer_style: ResearchAnswerStyle,
    },
    ResearchPreparationFailed {
        revision: u32,
        message: String,
        failed_at: DateTime<Utc>,
    },
    ResearchRunFailed {
        revision: u32,
        run_id: String,
        message: String,
        failed_at: DateTime<Utc>,
    },
    Cancelled {
        revision: u32,
        cancelled_at: DateTime<Utc>,
    },
    #[serde(rename = "intake_failed")]
    ModelRequestFailed {
        revision: u32,
        message: String,
        failed_at: DateTime<Utc>,
    },
}

impl ClarificationEvent {
    #[must_use]
    pub const fn new(kind: ClarificationEventKind) -> Self {
        Self::with_schema(CLARIFICATION_EVENT_SCHEMA_VERSION, kind)
    }

    const fn with_schema(schema_version: u32, kind: ClarificationEventKind) -> Self {
        Self {
            schema_version,
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResearchPreparation {
    pub run_id: String,
    pub brief: FrozenResearchBrief,
    pub policy: TracePolicy,
    pub answer_style: ResearchAnswerStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClarificationState {
    pub clarification_id: String,
    #[serde(skip)]
    event_schema_version: u32,
    pub original_question: String,
    pub session_id: Option<String>,
    pub turn: Option<u64>,
    pub conversation_history: Vec<crate::CompletedTurnContext>,
    pub revision: u32,
    pub status: ClarificationStatus,
    pub brief_draft: Option<ResearchBrief>,
    pub content_hash: Option<String>,
    pub dialogue: Vec<DialogueMessage>,
    pub failure: Option<String>,
    pub preparation: Option<ResearchPreparation>,
    #[serde(skip)]
    current_operation_id: Option<String>,
    #[serde(skip)]
    recent_operation_ids: Vec<String>,
}

impl ClarificationState {
    #[must_use]
    pub fn latest_assistant_message(&self) -> Option<&str> {
        self.dialogue.iter().rev().find_map(|message| {
            (message.role == DialogueRole::Assistant).then_some(message.text.as_str())
        })
    }

    #[must_use]
    pub const fn rationale_audit_status(&self) -> RationaleAuditStatus {
        RationaleAuditStatus::RequiredAndValidated
    }

    /// Returns true when this replayed intake already contains evidence for
    /// the supplied workspace operation.  Evidence survives model events so a
    /// retry can distinguish "message append happened, model call pending"
    /// from "message was never appended".
    #[must_use]
    pub fn has_operation_id(&self, operation_id: &str) -> bool {
        self.current_operation_id.as_deref() == Some(operation_id)
            || self
                .recent_operation_ids
                .iter()
                .any(|candidate| candidate == operation_id)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClarificationError {
    #[error("invalid clarification_id: expected one non-empty file-name component")]
    InvalidClarificationId,
    #[error("invalid run_id: expected one non-empty file-name component")]
    InvalidRunId,
    #[error("unsupported intake event schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("invalid intake event: {0}")]
    InvalidEvent(String),
    #[error("event {event} is invalid while intake is {status:?}")]
    InvalidTransition {
        status: ClarificationStatus,
        event: &'static str,
    },
    #[error("stale intake: current revision is {current_revision}, requested {requested_revision}")]
    StaleRevision {
        current_revision: u32,
        requested_revision: u32,
    },
    #[error("stale brief: current content hash is {current_hash}, requested {requested_hash}")]
    StaleContentHash {
        current_hash: String,
        requested_hash: String,
    },
    #[error("intake log is empty")]
    EmptyLog,
    #[error("intake log line {line} is truncated")]
    TruncatedLine { line: usize },
    #[error("invalid JSON on intake log line {line}: {source}")]
    InvalidJsonLine {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Brief(#[from] BriefValidationError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type ClarificationResult<T> = std::result::Result<T, ClarificationError>;

/// Replays one append-only event. The projected state is the only mutable
/// representation of a research-intake conversation.
pub fn reduce_clarification_event(
    current: Option<&ClarificationState>,
    event: &ClarificationEvent,
) -> ClarificationResult<ClarificationState> {
    if event.schema_version != CLARIFICATION_EVENT_SCHEMA_VERSION {
        return Err(ClarificationError::UnsupportedSchemaVersion(
            event.schema_version,
        ));
    }
    match (current, &event.kind) {
        (
            None,
            ClarificationEventKind::ClarificationStarted {
                clarification_id,
                original_question,
                revision,
                operation_id,
                session_id,
                turn,
                conversation_history,
                ..
            },
        ) => {
            validate_file_id(clarification_id, true)?;
            validate_message_text(original_question, "original_question")?;
            if *revision != 0 {
                return Err(ClarificationError::InvalidEvent(
                    "intake_started revision must be 0".into(),
                ));
            }
            if session_id.is_some() != turn.is_some() || matches!(turn, Some(0)) {
                return Err(ClarificationError::InvalidEvent(
                    "session_id and positive turn must be provided together".into(),
                ));
            }
            if let Some(session_id) = session_id {
                validate_file_id(session_id, true)?;
            }
            if session_id.is_none() && !conversation_history.is_empty() {
                return Err(ClarificationError::InvalidEvent(
                    "conversation history requires a conversation turn".into(),
                ));
            }
            record_operation_id(
                ClarificationState {
                    clarification_id: clarification_id.clone(),
                    event_schema_version: event.schema_version,
                    original_question: original_question.trim().to_owned(),
                    session_id: session_id.clone(),
                    turn: *turn,
                    conversation_history: conversation_history.clone(),
                    revision: 0,
                    status: ClarificationStatus::ModelEvaluationPending,
                    brief_draft: None,
                    content_hash: None,
                    dialogue: vec![DialogueMessage {
                        role: DialogueRole::User,
                        text: original_question.trim().to_owned(),
                    }],
                    failure: None,
                    preparation: None,
                    current_operation_id: None,
                    recent_operation_ids: Vec::new(),
                },
                operation_id.as_deref(),
            )
        }
        (None, _) => Err(ClarificationError::InvalidEvent(
            "intake_started must be the first event".into(),
        )),
        (Some(clarification), ClarificationEventKind::ClarificationStarted { .. }) => {
            transition_error(clarification, "intake_started")
        }
        (Some(clarification), kind) => reduce_existing(clarification, kind),
    }
}

fn reduce_existing(
    clarification: &ClarificationState,
    event: &ClarificationEventKind,
) -> ClarificationResult<ClarificationState> {
    if matches!(
        clarification.status,
        ClarificationStatus::ResearchFailed | ClarificationStatus::Cancelled
    ) {
        return transition_error(clarification, event_name(event));
    }
    match event {
        ClarificationEventKind::UserMessageReceived {
            revision,
            message,
            operation_id,
            ..
        } => {
            if !matches!(
                clarification.status,
                ClarificationStatus::AwaitingUserMessage | ClarificationStatus::ModelRequestFailed
            ) {
                return transition_error(clarification, "user_message_received");
            }
            require_revision(clarification, *revision)?;
            validate_message_text(message, "user message")?;
            let mut next = clarification.clone();
            next.dialogue.push(DialogueMessage {
                role: DialogueRole::User,
                text: message.trim().to_owned(),
            });
            next.status = ClarificationStatus::ModelEvaluationPending;
            next.failure = None;
            record_operation_id(next, operation_id.as_deref())
        }
        ClarificationEventKind::ModelUnderstanding {
            revision,
            decision,
            rationale,
            assistant_message,
            brief,
            content_hash,
            ..
        } => apply_model_understanding(
            clarification,
            *revision,
            *decision,
            rationale,
            assistant_message,
            brief,
            content_hash,
        ),
        ClarificationEventKind::ResearchRunPrepared {
            revision,
            run_id,
            brief,
            policy,
            answer_style,
        } => apply_run_prepared(
            clarification,
            *revision,
            run_id,
            brief,
            policy,
            *answer_style,
        ),
        ClarificationEventKind::ResearchPreparationFailed {
            revision, message, ..
        } => apply_research_preparation_failed(clarification, *revision, message),
        ClarificationEventKind::ResearchRunFailed {
            revision,
            run_id,
            message,
            ..
        } => apply_research_run_failed(clarification, *revision, run_id, message),
        ClarificationEventKind::Cancelled { revision, .. } => {
            if !matches!(
                clarification.status,
                ClarificationStatus::AwaitingUserMessage
                    | ClarificationStatus::ResearchReady
                    | ClarificationStatus::ModelRequestFailed
            ) {
                return transition_error(clarification, "cancelled");
            }
            require_revision(clarification, *revision)?;
            let mut next = clarification.clone();
            next.status = ClarificationStatus::Cancelled;
            Ok(next)
        }
        ClarificationEventKind::ModelRequestFailed {
            revision, message, ..
        } => {
            if !matches!(
                clarification.status,
                ClarificationStatus::ModelEvaluationPending
                    | ClarificationStatus::ModelRequestFailed
            ) {
                return transition_error(clarification, "intake_failed");
            }
            require_revision(clarification, *revision)?;
            validate_message_text(message, "intake failure")?;
            let mut next = clarification.clone();
            next.status = ClarificationStatus::ModelRequestFailed;
            next.failure = Some(message.trim().to_owned());
            Ok(next)
        }
        ClarificationEventKind::ClarificationStarted { .. } => unreachable!(),
    }
}

fn apply_model_understanding(
    clarification: &ClarificationState,
    revision: u32,
    decision: ClarificationDecision,
    rationale: &str,
    assistant_message: &str,
    brief: &ResearchBrief,
    content_hash: &str,
) -> ClarificationResult<ClarificationState> {
    if !matches!(
        clarification.status,
        ClarificationStatus::ModelEvaluationPending | ClarificationStatus::ModelRequestFailed
    ) {
        return transition_error(clarification, "model_understanding");
    }
    let expected_revision = clarification
        .revision
        .checked_add(1)
        .ok_or_else(|| ClarificationError::InvalidEvent("revision overflow".into()))?;
    if revision != expected_revision {
        return Err(ClarificationError::InvalidEvent(format!(
            "model_understanding revision must be {expected_revision}"
        )));
    }
    validate_decision_rationale(rationale).map_err(ClarificationError::InvalidEvent)?;
    validate_message_text(assistant_message, "assistant message")?;
    let normalized = brief.clone().normalized(&clarification.original_question)?;
    if &normalized != brief {
        return Err(ClarificationError::Brief(
            BriefValidationError::NonCanonical,
        ));
    }
    let actual_hash = brief.content_hash()?;
    if actual_hash != content_hash {
        return Err(ClarificationError::InvalidEvent(
            "model_understanding content_hash does not match brief".into(),
        ));
    }
    let mut next = clarification.clone();
    next.revision = revision;
    next.brief_draft = Some(brief.clone());
    next.content_hash = Some(content_hash.to_owned());
    next.dialogue.push(DialogueMessage {
        role: DialogueRole::Assistant,
        text: assistant_message.trim().to_owned(),
    });
    next.status = match decision {
        ClarificationDecision::ContinueDialogue => ClarificationStatus::AwaitingUserMessage,
        ClarificationDecision::StartResearch => ClarificationStatus::ResearchReady,
    };
    next.failure = None;
    Ok(next)
}

fn apply_run_prepared(
    clarification: &ClarificationState,
    revision: u32,
    run_id: &str,
    brief: &FrozenResearchBrief,
    policy: &TracePolicy,
    answer_style: ResearchAnswerStyle,
) -> ClarificationResult<ClarificationState> {
    require_status(
        clarification,
        ClarificationStatus::ResearchReady,
        "run_prepared",
    )?;
    require_revision(clarification, revision)?;
    validate_file_id(run_id, false)?;
    validate_trace_policy(policy).map_err(ClarificationError::InvalidEvent)?;
    let draft = clarification.brief_draft.as_ref().ok_or_else(|| {
        ClarificationError::InvalidEvent("run_prepared requires a brief draft".into())
    })?;
    if brief.clarification_id() != clarification.clarification_id
        || brief.brief() != draft
        || Some(brief.content_hash()) != clarification.content_hash.as_deref()
    {
        return Err(ClarificationError::InvalidEvent(
            "run_prepared brief does not match the research-ready intake".into(),
        ));
    }
    let mut next = clarification.clone();
    next.status = ClarificationStatus::ResearchPrepared;
    next.preparation = Some(ResearchPreparation {
        run_id: run_id.to_owned(),
        brief: brief.clone(),
        policy: policy.clone(),
        answer_style,
    });
    Ok(next)
}

fn apply_research_run_failed(
    clarification: &ClarificationState,
    revision: u32,
    run_id: &str,
    message: &str,
) -> ClarificationResult<ClarificationState> {
    require_status(
        clarification,
        ClarificationStatus::ResearchPrepared,
        "research_run_failed",
    )?;
    require_revision(clarification, revision)?;
    require_prepared_run_id(clarification, run_id)?;
    validate_research_failure_summary(message)?;
    let mut next = clarification.clone();
    next.status = ClarificationStatus::ResearchFailed;
    next.failure = Some(message.trim().to_owned());
    Ok(next)
}

fn apply_research_preparation_failed(
    clarification: &ClarificationState,
    revision: u32,
    message: &str,
) -> ClarificationResult<ClarificationState> {
    require_status(
        clarification,
        ClarificationStatus::ResearchReady,
        "research_preparation_failed",
    )?;
    require_revision(clarification, revision)?;
    validate_research_failure_summary(message)?;
    let mut next = clarification.clone();
    next.status = ClarificationStatus::ResearchFailed;
    next.failure = Some(message.trim().to_owned());
    Ok(next)
}

/// Converts a validated model response into the one append-only decision event
/// that both drives the state machine and supplies the visible chat message.
pub fn events_from_clarification_model_output(
    clarification: &ClarificationState,
    output: ClarificationModelOutput,
    now: DateTime<Utc>,
) -> ClarificationResult<Vec<ClarificationEvent>> {
    if !matches!(
        clarification.status,
        ClarificationStatus::ModelEvaluationPending | ClarificationStatus::ModelRequestFailed
    ) {
        return transition_error(clarification, "model_output");
    }
    let output = validate_model_output(output, &clarification.original_question)?;
    let revision = clarification
        .revision
        .checked_add(1)
        .ok_or_else(|| ClarificationError::InvalidEvent("revision overflow".into()))?;
    let content_hash = output.brief_draft.content_hash()?;
    Ok(vec![ClarificationEvent::with_schema(
        clarification.event_schema_version,
        ClarificationEventKind::ModelUnderstanding {
            revision,
            decision: output.decision,
            rationale: output.rationale,
            assistant_message: output.assistant_message,
            brief: output.brief_draft,
            content_hash,
            evaluated_at: now,
        },
    )])
}

pub fn clarification_user_message_event(
    clarification: &ClarificationState,
    requested_revision: u32,
    message: &str,
    now: DateTime<Utc>,
) -> ClarificationResult<ClarificationEvent> {
    clarification_user_message_event_with_operation(
        clarification,
        requested_revision,
        message,
        None,
        now,
    )
}

pub fn clarification_user_message_event_with_operation(
    clarification: &ClarificationState,
    requested_revision: u32,
    message: &str,
    operation_id: Option<&str>,
    now: DateTime<Utc>,
) -> ClarificationResult<ClarificationEvent> {
    if !matches!(
        clarification.status,
        ClarificationStatus::AwaitingUserMessage | ClarificationStatus::ModelRequestFailed
    ) {
        return transition_error(clarification, "user_message_received");
    }
    require_revision(clarification, requested_revision)?;
    validate_message_text(message, "user message")?;
    Ok(ClarificationEvent::with_schema(
        clarification.event_schema_version,
        ClarificationEventKind::UserMessageReceived {
            revision: requested_revision,
            message: message.trim().to_owned(),
            received_at: now,
            operation_id: operation_id.map(str::to_owned),
        },
    ))
}

pub fn research_run_prepared_event_with_answer_style(
    clarification: &ClarificationState,
    requested_revision: u32,
    requested_content_hash: &str,
    run_id: String,
    policy: TracePolicy,
    answer_style: ResearchAnswerStyle,
    now: DateTime<Utc>,
) -> ClarificationResult<ClarificationEvent> {
    require_status(
        clarification,
        ClarificationStatus::ResearchReady,
        "run_prepared",
    )?;
    require_revision(clarification, requested_revision)?;
    require_content_hash_value(clarification, requested_content_hash)?;
    validate_file_id(&run_id, false)?;
    validate_trace_policy(&policy).map_err(ClarificationError::InvalidEvent)?;
    let brief = FrozenResearchBrief::new(
        clarification.brief_draft.clone().ok_or_else(|| {
            ClarificationError::InvalidEvent("run_prepared requires a brief".into())
        })?,
        &clarification.original_question,
        clarification.clarification_id.clone(),
        requested_content_hash,
        now,
    )?;
    Ok(ClarificationEvent::with_schema(
        clarification.event_schema_version,
        ClarificationEventKind::ResearchRunPrepared {
            revision: requested_revision,
            run_id,
            brief,
            policy,
            answer_style,
        },
    ))
}

pub(crate) fn research_run_failed_event(
    clarification: &ClarificationState,
    run_id: &str,
    message: &str,
    now: DateTime<Utc>,
) -> ClarificationResult<ClarificationEvent> {
    require_status(
        clarification,
        ClarificationStatus::ResearchPrepared,
        "research_run_failed",
    )?;
    require_prepared_run_id(clarification, run_id)?;
    validate_research_failure_summary(message)?;
    Ok(ClarificationEvent::with_schema(
        clarification.event_schema_version,
        ClarificationEventKind::ResearchRunFailed {
            revision: clarification.revision,
            run_id: run_id.to_owned(),
            message: message.trim().to_owned(),
            failed_at: now,
        },
    ))
}

pub(crate) fn research_preparation_failed_event(
    clarification: &ClarificationState,
    message: &str,
    now: DateTime<Utc>,
) -> ClarificationResult<ClarificationEvent> {
    require_status(
        clarification,
        ClarificationStatus::ResearchReady,
        "research_preparation_failed",
    )?;
    validate_research_failure_summary(message)?;
    Ok(ClarificationEvent::with_schema(
        clarification.event_schema_version,
        ClarificationEventKind::ResearchPreparationFailed {
            revision: clarification.revision,
            message: message.trim().to_owned(),
            failed_at: now,
        },
    ))
}

pub fn clarification_cancelled_event(
    clarification: &ClarificationState,
    now: DateTime<Utc>,
) -> ClarificationEvent {
    ClarificationEvent::with_schema(
        clarification.event_schema_version,
        ClarificationEventKind::Cancelled {
            revision: clarification.revision,
            cancelled_at: now,
        },
    )
}

pub fn clarification_model_request_failed_event(
    clarification: &ClarificationState,
    message: String,
    now: DateTime<Utc>,
) -> ClarificationEvent {
    ClarificationEvent::with_schema(
        clarification.event_schema_version,
        ClarificationEventKind::ModelRequestFailed {
            revision: clarification.revision,
            message,
            failed_at: now,
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClarificationModelParseOutcome {
    Accepted(ClarificationModelOutput),
    RetryCorrection { error: String },
    Failed(ClarificationEvent),
}

/// Attempt one requests a corrected JSON object; attempt two records a
/// recoverable model failure without manufacturing a research-ready brief.
pub fn parse_clarification_model_attempt(
    clarification: &ClarificationState,
    json: &str,
    attempt: u8,
    now: DateTime<Utc>,
) -> ClarificationResult<ClarificationModelParseOutcome> {
    if !matches!(attempt, 1 | 2) {
        return Err(ClarificationError::InvalidEvent(
            "model parse attempt must be 1 or 2".into(),
        ));
    }
    match parse_clarification_model_output(json, &clarification.original_question) {
        Ok(output) => Ok(ClarificationModelParseOutcome::Accepted(output)),
        Err(error) if attempt == 1 => Ok(ClarificationModelParseOutcome::RetryCorrection {
            error: error.to_string(),
        }),
        Err(error) => Ok(ClarificationModelParseOutcome::Failed(
            clarification_model_request_failed_event(
                clarification,
                format!("model returned invalid structured output twice: {error}"),
                now,
            ),
        )),
    }
}

pub fn parse_clarification_model_output(
    json: &str,
    original_question: &str,
) -> ClarificationResult<ClarificationModelOutput> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    validate_generate_model_json_shape(&value)?;
    validate_model_output(serde_json::from_value(value)?, original_question)
}

fn validate_model_output(
    mut output: ClarificationModelOutput,
    original_question: &str,
) -> ClarificationResult<ClarificationModelOutput> {
    output.brief_draft = output.brief_draft.normalized(original_question)?;
    validate_decision_rationale(&output.rationale).map_err(ClarificationError::InvalidEvent)?;
    validate_message_text(&output.assistant_message, "assistant message")?;
    Ok(output)
}

fn validate_generate_model_json_shape(value: &serde_json::Value) -> ClarificationResult<()> {
    exact_keys(
        value,
        &["decision", "rationale", "assistant_message", "brief_draft"],
        "model output",
    )?;
    let brief = value.get("brief_draft").ok_or_else(|| {
        ClarificationError::InvalidEvent("model output is missing brief_draft".into())
    })?;
    exact_keys(
        brief,
        &[
            "schema_version",
            "original_question",
            "research_question",
            "desired_output",
            "scope",
            "source_constraints",
            "accepted_assumptions",
        ],
        "brief_draft",
    )?;
    exact_keys(
        brief.get("scope").ok_or_else(|| {
            ClarificationError::InvalidEvent("brief_draft is missing scope".into())
        })?,
        &["time_range", "geography", "include", "exclude"],
        "brief_draft.scope",
    )
}

fn exact_keys(value: &serde_json::Value, expected: &[&str], name: &str) -> ClarificationResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| ClarificationError::InvalidEvent(format!("{name} must be an object")))?;
    let actual: HashSet<_> = object.keys().map(String::as_str).collect();
    let expected: HashSet<_> = expected.iter().copied().collect();
    if actual == expected {
        Ok(())
    } else {
        Err(ClarificationError::InvalidEvent(format!(
            "{name} does not match the fixed schema"
        )))
    }
}

fn validate_message_text(value: &str, name: &str) -> ClarificationResult<()> {
    let count = value.trim().chars().count();
    if (1..=MAX_DIALOGUE_MESSAGE_CHARS).contains(&count) {
        Ok(())
    } else {
        Err(ClarificationError::InvalidEvent(format!(
            "{name} must be 1..={MAX_DIALOGUE_MESSAGE_CHARS} characters"
        )))
    }
}

fn validate_research_failure_summary(value: &str) -> ClarificationResult<()> {
    let count = value.trim().chars().count();
    if (1..=MAX_RESEARCH_FAILURE_SUMMARY_CHARS).contains(&count) {
        Ok(())
    } else {
        Err(ClarificationError::InvalidEvent(format!(
            "research failure summary must be 1..={MAX_RESEARCH_FAILURE_SUMMARY_CHARS} characters"
        )))
    }
}

fn require_prepared_run_id(
    clarification: &ClarificationState,
    run_id: &str,
) -> ClarificationResult<()> {
    validate_file_id(run_id, false)?;
    let preparation = clarification.preparation.as_ref().ok_or_else(|| {
        ClarificationError::InvalidEvent("research run failure requires a preparation".into())
    })?;
    if preparation.run_id == run_id {
        Ok(())
    } else {
        Err(ClarificationError::InvalidEvent(
            "research run failure does not match the prepared run".into(),
        ))
    }
}

fn require_status(
    clarification: &ClarificationState,
    expected: ClarificationStatus,
    event: &'static str,
) -> ClarificationResult<()> {
    if clarification.status == expected {
        Ok(())
    } else {
        transition_error(clarification, event)
    }
}

fn require_revision(clarification: &ClarificationState, revision: u32) -> ClarificationResult<()> {
    if revision == clarification.revision {
        Ok(())
    } else {
        Err(ClarificationError::StaleRevision {
            current_revision: clarification.revision,
            requested_revision: revision,
        })
    }
}

fn record_operation_id(
    mut clarification: ClarificationState,
    operation_id: Option<&str>,
) -> ClarificationResult<ClarificationState> {
    let Some(operation_id) = operation_id else {
        return Ok(clarification);
    };
    validate_file_id(operation_id, true)?;
    if clarification.current_operation_id.as_deref() != Some(operation_id) {
        if !clarification
            .recent_operation_ids
            .iter()
            .any(|candidate| candidate == operation_id)
        {
            clarification
                .recent_operation_ids
                .push(operation_id.to_owned());
            if clarification.recent_operation_ids.len() > 8 {
                let excess = clarification.recent_operation_ids.len() - 8;
                clarification.recent_operation_ids.drain(..excess);
            }
        }
        clarification.current_operation_id = Some(operation_id.to_owned());
    }
    Ok(clarification)
}

fn require_content_hash_value(
    clarification: &ClarificationState,
    requested_content_hash: &str,
) -> ClarificationResult<()> {
    let current_hash = clarification
        .content_hash
        .as_deref()
        .ok_or_else(|| ClarificationError::InvalidEvent("intake has no content_hash".into()))?;
    if current_hash == requested_content_hash {
        Ok(())
    } else {
        Err(ClarificationError::StaleContentHash {
            current_hash: current_hash.to_owned(),
            requested_hash: requested_content_hash.to_owned(),
        })
    }
}

fn transition_error<T>(
    clarification: &ClarificationState,
    event: &'static str,
) -> ClarificationResult<T> {
    Err(ClarificationError::InvalidTransition {
        status: clarification.status,
        event,
    })
}

const fn event_name(event: &ClarificationEventKind) -> &'static str {
    match event {
        ClarificationEventKind::ClarificationStarted { .. } => "intake_started",
        ClarificationEventKind::UserMessageReceived { .. } => "user_message_received",
        ClarificationEventKind::ModelUnderstanding { .. } => "model_understanding",
        ClarificationEventKind::ResearchRunPrepared { .. } => "run_prepared",
        ClarificationEventKind::ResearchPreparationFailed { .. } => "research_preparation_failed",
        ClarificationEventKind::ResearchRunFailed { .. } => "research_run_failed",
        ClarificationEventKind::Cancelled { .. } => "cancelled",
        ClarificationEventKind::ModelRequestFailed { .. } => "intake_failed",
    }
}

fn validate_file_id(value: &str, clarification: bool) -> ClarificationResult<()> {
    let path = Path::new(value);
    let valid = !value.is_empty()
        && value != "."
        && value != ".."
        && path.file_name() == Some(OsStr::new(value))
        && path.components().count() == 1;
    if valid {
        Ok(())
    } else if clarification {
        Err(ClarificationError::InvalidClarificationId)
    } else {
        Err(ClarificationError::InvalidRunId)
    }
}

pub struct ClarificationEventLog {
    writer: BufWriter<File>,
    clarification: ClarificationState,
}

impl ClarificationEventLog {
    pub fn create(
        intake_dir: impl AsRef<Path>,
        started: ClarificationEvent,
    ) -> ClarificationResult<Self> {
        let clarification = reduce_clarification_event(None, &started)?;
        fs::create_dir_all(intake_dir.as_ref())?;
        let path = intake_path(intake_dir.as_ref(), &clarification.clarification_id)?;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut log = Self {
            writer: BufWriter::new(file),
            clarification,
        };
        log.write_event(&started)?;
        Ok(log)
    }

    /// Create or reopen a clarification for a reserved operation.  The
    /// operation metadata is optional wire evidence; semantic seed fields
    /// remain the identity check so old schema-v5 files can be resumed.
    pub fn create_idempotent(
        intake_dir: impl AsRef<Path>,
        started: ClarificationEvent,
    ) -> ClarificationResult<Self> {
        let expected = reduce_clarification_event(None, &started)?;
        let intake_dir = intake_dir.as_ref();
        fs::create_dir_all(intake_dir)?;
        let path = intake_path(intake_dir, &expected.clarification_id)?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                let mut log = Self {
                    writer: BufWriter::new(file),
                    clarification: expected,
                };
                log.write_event(&started)?;
                Ok(log)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match Self::open(intake_dir, &expected.clarification_id) {
                    Ok(log) => {
                        let first = read_first_event(&path)?;
                        if clarification_seeds_match(&first, &started) {
                            Ok(log)
                        } else {
                            Err(ClarificationError::InvalidEvent(
                                "existing clarification does not match operation seed".into(),
                            ))
                        }
                    }
                    Err(ClarificationError::EmptyLog) => {
                        // A process can die after create_new but before the
                        // first complete record.  The reserved clarification
                        // ID makes replacing this empty/torn seed safe.
                        let file = OpenOptions::new().write(true).truncate(true).open(&path)?;
                        let mut log = Self {
                            writer: BufWriter::new(file),
                            clarification: expected,
                        };
                        log.write_event(&started)?;
                        Ok(log)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn open(intake_dir: impl AsRef<Path>, clarification_id: &str) -> ClarificationResult<Self> {
        let path = intake_path(intake_dir.as_ref(), clarification_id)?;
        let (clarification, valid_bytes, truncated) = replay_path_details(&path)?;
        let Some(clarification) = clarification else {
            // Empty files and first-line torn writes are recoverable seeds.
            // Keep the file in place for create_idempotent to rewrite.
            if truncated || valid_bytes == 0 {
                OpenOptions::new().write(true).open(&path)?.set_len(0)?;
            }
            return Err(ClarificationError::EmptyLog);
        };
        if truncated {
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(valid_bytes)?;
        }
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            clarification,
        })
    }

    #[must_use]
    pub const fn clarification(&self) -> &ClarificationState {
        &self.clarification
    }

    pub fn append(&mut self, event: &ClarificationEvent) -> ClarificationResult<()> {
        let next = reduce_clarification_event(Some(&self.clarification), event)?;
        self.write_event(event)?;
        self.clarification = next;
        Ok(())
    }

    fn write_event(&mut self, event: &ClarificationEvent) -> ClarificationResult<()> {
        serde_json::to_writer(&mut self.writer, event)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

pub fn replay_clarification(
    intake_dir: impl AsRef<Path>,
    clarification_id: &str,
) -> ClarificationResult<ClarificationState> {
    let path = intake_path(intake_dir.as_ref(), clarification_id)?;
    let (state, valid_bytes, truncated) = replay_path_details(&path)?;
    if truncated {
        OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_len(valid_bytes)?;
    }
    state.ok_or(ClarificationError::EmptyLog)
}

fn replay_path_details(
    path: &Path,
) -> ClarificationResult<(Option<ClarificationState>, u64, bool)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut current = None;
    let mut line = String::new();
    let mut line_number = 0;
    let mut valid_bytes = 0_u64;
    let mut truncated = false;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        line_number += 1;
        if !line.ends_with('\n') {
            truncated = true;
            break;
        }
        let event: ClarificationEvent =
            serde_json::from_str(&line).map_err(|source| ClarificationError::InvalidJsonLine {
                line: line_number,
                source,
            })?;
        current = Some(reduce_clarification_event(current.as_ref(), &event)?);
        valid_bytes += bytes as u64;
    }
    Ok((current, valid_bytes, truncated))
}

fn read_first_event(path: &Path) -> ClarificationResult<ClarificationEvent> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;
    if bytes == 0 || !line.ends_with('\n') {
        return Err(ClarificationError::EmptyLog);
    }
    serde_json::from_str(&line)
        .map_err(|source| ClarificationError::InvalidJsonLine { line: 1, source })
}

fn clarification_seeds_match(existing: &ClarificationEvent, expected: &ClarificationEvent) -> bool {
    match (&existing.kind, &expected.kind) {
        (
            ClarificationEventKind::ClarificationStarted {
                clarification_id: existing_id,
                original_question: existing_question,
                revision: existing_revision,
                created_at: existing_created_at,
                operation_id: existing_operation,
                session_id: existing_session,
                turn: existing_turn,
                conversation_history: existing_history,
                ..
            },
            ClarificationEventKind::ClarificationStarted {
                clarification_id: expected_id,
                original_question: expected_question,
                revision: expected_revision,
                created_at: expected_created_at,
                operation_id: expected_operation,
                session_id: expected_session,
                turn: expected_turn,
                conversation_history: expected_history,
                ..
            },
        ) => {
            existing.schema_version == expected.schema_version
                && existing_id == expected_id
                && existing_question == expected_question
                && existing_revision == expected_revision
                && existing_created_at == expected_created_at
                && (existing_operation.is_none()
                    || expected_operation.is_none()
                    || existing_operation == expected_operation)
                && existing_session == expected_session
                && existing_turn == expected_turn
                && existing_history == expected_history
        }
        _ => false,
    }
}

fn intake_path(intake_dir: &Path, clarification_id: &str) -> ClarificationResult<PathBuf> {
    validate_file_id(clarification_id, true)?;
    Ok(intake_dir.join(format!("{clarification_id}.jsonl")))
}

/// Process-local per-intake serialization. The host remains responsible for
/// cross-process coordination if it is ever deployed with multiple workers.
#[derive(Clone, Default)]
pub struct ClarificationLocks {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl ClarificationLocks {
    pub async fn lock(&self, clarification_id: &str) -> ClarificationResult<OwnedMutexGuard<()>> {
        validate_file_id(clarification_id, true)?;
        let lock = {
            let mut locks = self.inner.lock().await;
            locks
                .entry(clarification_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        Ok(lock.lock_owned().await)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use chrono::TimeZone;

    use crate::{RESEARCH_BRIEF_SCHEMA_VERSION, ResearchScope};

    use super::*;

    const QUESTION: &str = "Compare Rust 2024 edition changes with Rust 2021";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 16, 10, 0, 0).unwrap()
    }

    fn started(id: &str) -> ClarificationEvent {
        ClarificationEvent::new(ClarificationEventKind::ClarificationStarted {
            clarification_id: id.into(),
            original_question: QUESTION.into(),
            revision: 0,
            created_at: now(),
            operation_id: None,
            session_id: None,
            turn: None,
            conversation_history: Vec::new(),
        })
    }

    fn brief() -> ResearchBrief {
        ResearchBrief {
            schema_version: RESEARCH_BRIEF_SCHEMA_VERSION,
            original_question: QUESTION.into(),
            research_question: QUESTION.into(),
            desired_output: None,
            scope: ResearchScope::default(),
            source_constraints: Vec::new(),
            accepted_assumptions: Vec::new(),
        }
    }

    fn model_output(decision: ClarificationDecision, message: &str) -> ClarificationModelOutput {
        ClarificationModelOutput {
            decision,
            rationale: "The reply records the current research understanding.".into(),
            assistant_message: message.into(),
            brief_draft: brief(),
        }
    }

    fn state() -> ClarificationState {
        reduce_clarification_event(None, &started("dialogue-1")).unwrap()
    }

    fn prepared_state() -> ClarificationState {
        let initial = state();
        let ready = apply_all(
            initial.clone(),
            &events_from_clarification_model_output(
                &initial,
                model_output(
                    ClarificationDecision::StartResearch,
                    "I understand the research request and am starting now.",
                ),
                now(),
            )
            .unwrap(),
        );
        let prepared = research_run_prepared_event_with_answer_style(
            &ready,
            ready.revision,
            ready.content_hash.as_deref().unwrap(),
            "run-prepared".into(),
            TracePolicy {
                rounds: 3,
                input_budget: 1_000,
                max_snapshots: 10,
            },
            ResearchAnswerStyle::WebFirst,
            now(),
        )
        .unwrap();
        reduce_clarification_event(Some(&ready), &prepared).unwrap()
    }

    fn apply_all(
        mut state: ClarificationState,
        events: &[ClarificationEvent],
    ) -> ClarificationState {
        for event in events {
            state = reduce_clarification_event(Some(&state), event).unwrap();
        }
        state
    }

    #[test]
    fn continue_dialogue_records_a_normal_assistant_reply() {
        let first = state();
        let waiting = apply_all(
            first,
            &events_from_clarification_model_output(
                &state(),
                model_output(
                    ClarificationDecision::ContinueDialogue,
                    "我理解你希望比较两个版本的变化；请继续补充你最关心的维度。",
                ),
                now(),
            )
            .unwrap(),
        );
        assert_eq!(waiting.status, ClarificationStatus::AwaitingUserMessage);
        assert_eq!(
            waiting.latest_assistant_message(),
            Some("我理解你希望比较两个版本的变化；请继续补充你最关心的维度。")
        );
        let user =
            clarification_user_message_event(&waiting, waiting.revision, "兼容性", now()).unwrap();
        let pending = reduce_clarification_event(Some(&waiting), &user).unwrap();
        assert_eq!(pending.status, ClarificationStatus::ModelEvaluationPending);
        assert_eq!(pending.dialogue.last().unwrap().role, DialogueRole::User);
    }

    #[test]
    fn model_can_make_a_brief_research_ready_without_user_confirmation() {
        let initial = state();
        let ready = apply_all(
            initial.clone(),
            &events_from_clarification_model_output(
                &initial,
                model_output(
                    ClarificationDecision::StartResearch,
                    "我理解你希望比较 Rust 2024 与 Rust 2021 的主要变化，现在开始查证。",
                ),
                now(),
            )
            .unwrap(),
        );
        assert_eq!(ready.status, ClarificationStatus::ResearchReady);
        assert!(ready.preparation.is_none());
        assert!(
            clarification_user_message_event(
                &ready,
                ready.revision,
                "Looks good; please start.",
                now(),
            )
            .is_err()
        );
    }

    #[test]
    fn run_preparation_requires_a_model_research_ready_decision() {
        let initial = state();
        let waiting = apply_all(
            initial.clone(),
            &events_from_clarification_model_output(
                &initial,
                model_output(
                    ClarificationDecision::ContinueDialogue,
                    "请继续说明预期输出。",
                ),
                now(),
            )
            .unwrap(),
        );
        assert!(
            research_run_prepared_event_with_answer_style(
                &waiting,
                waiting.revision,
                waiting.content_hash.as_deref().unwrap(),
                "run-1".into(),
                TracePolicy {
                    rounds: 3,
                    input_budget: 1_000,
                    max_snapshots: 10,
                },
                ResearchAnswerStyle::WebFirst,
                now(),
            )
            .is_err()
        );
    }

    #[test]
    fn prepared_run_failure_is_terminal_and_requires_a_matching_bounded_summary() {
        let prepared = prepared_state();
        let event = research_run_failed_event(
            &prepared,
            "run-prepared",
            "Research could not start after preparation.",
            now(),
        )
        .unwrap();
        let failed = reduce_clarification_event(Some(&prepared), &event).unwrap();
        assert_eq!(failed.status, ClarificationStatus::ResearchFailed);
        assert_eq!(
            failed.failure.as_deref(),
            Some("Research could not start after preparation.")
        );
        assert!(
            research_run_failed_event(
                &prepared,
                "other-run",
                "Research could not start after preparation.",
                now(),
            )
            .is_err()
        );
        assert!(
            research_run_failed_event(
                &prepared,
                "run-prepared",
                &"x".repeat(MAX_RESEARCH_FAILURE_SUMMARY_CHARS + 1),
                now(),
            )
            .is_err()
        );
        assert!(
            clarification_user_message_event(&failed, failed.revision, "Please continue.", now(),)
                .is_err()
        );
    }

    #[test]
    fn research_ready_preparation_failure_is_terminal_without_creating_a_run() {
        let initial = state();
        let ready = apply_all(
            initial.clone(),
            &events_from_clarification_model_output(
                &initial,
                model_output(
                    ClarificationDecision::StartResearch,
                    "I understand the research request and am starting now.",
                ),
                now(),
            )
            .unwrap(),
        );
        let event =
            research_preparation_failed_event(&ready, "Research could not be prepared.", now())
                .unwrap();
        let failed = reduce_clarification_event(Some(&ready), &event).unwrap();
        assert_eq!(failed.status, ClarificationStatus::ResearchFailed);
        assert!(failed.preparation.is_none());
        assert_eq!(
            failed.failure.as_deref(),
            Some("Research could not be prepared.")
        );
        assert!(research_preparation_failed_event(&failed, "retry", now()).is_err());
    }

    #[test]
    fn parser_requires_user_visible_message_and_new_decision_contract() {
        let valid = serde_json::json!({
            "decision": "start_research",
            "rationale": "The original question already fixes the comparison target.",
            "assistant_message": "我理解你的比较目标，现在开始研究。",
            "brief_draft": brief(),
        });
        assert!(parse_clarification_model_output(&valid.to_string(), QUESTION).is_ok());
        let mut invalid = valid;
        invalid.as_object_mut().unwrap().remove("assistant_message");
        assert!(parse_clarification_model_output(&invalid.to_string(), QUESTION).is_err());
    }

    #[test]
    fn new_log_replays_the_chat_dialogue() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("traceable-dialogue-{unique}"));
        let mut log = ClarificationEventLog::create(&dir, started("dialogue-log")).unwrap();
        for event in events_from_clarification_model_output(
            log.clarification(),
            model_output(
                ClarificationDecision::ContinueDialogue,
                "我理解你的问题，请继续补充。",
            ),
            now(),
        )
        .unwrap()
        {
            log.append(&event).unwrap();
        }
        let user =
            clarification_user_message_event(log.clarification(), 1, "重点比较兼容性", now())
                .unwrap();
        log.append(&user).unwrap();
        drop(log);
        let replayed = replay_clarification(&dir, "dialogue-log").unwrap();
        assert_eq!(replayed.dialogue.len(), 3);
        assert_eq!(replayed.dialogue[2].text, "重点比较兼容性");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn idempotent_seed_repairs_empty_and_torn_files_without_duplicate_start() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("traceable-dialogue-replay-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dialogue-replay.jsonl");
        File::create(&path).unwrap();
        let seed = started("dialogue-replay");
        let log = ClarificationEventLog::create_idempotent(&dir, seed.clone()).unwrap();
        assert_eq!(log.clarification().clarification_id, "dialogue-replay");
        let valid_len = fs::metadata(&path).unwrap().len();
        drop(log);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"schema_version\":5")
            .unwrap();
        let reopened = ClarificationEventLog::create_idempotent(&dir, seed).unwrap();
        assert_eq!(reopened.clarification().clarification_id, "dialogue-replay");
        assert_eq!(fs::metadata(&path).unwrap().len(), valid_len);
        drop(reopened);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn operation_evidence_survives_model_events_and_replay() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("traceable-dialogue-operation-{unique}"));
        let mut log = ClarificationEventLog::create_idempotent(
            &dir,
            ClarificationEvent::new(ClarificationEventKind::ClarificationStarted {
                clarification_id: "dialogue-operation".into(),
                original_question: QUESTION.into(),
                revision: 0,
                created_at: now(),
                operation_id: Some("operation-start".into()),
                session_id: None,
                turn: None,
                conversation_history: Vec::new(),
            }),
        )
        .unwrap();
        for event in events_from_clarification_model_output(
            log.clarification(),
            model_output(ClarificationDecision::ContinueDialogue, "Please add scope."),
            now(),
        )
        .unwrap()
        {
            log.append(&event).unwrap();
        }
        let waiting = log.clarification().clone();
        log.append(
            &clarification_user_message_event_with_operation(
                &waiting,
                waiting.revision,
                "Compatibility",
                Some("operation-message"),
                now(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(log.clarification().has_operation_id("operation-start"));
        assert!(log.clarification().has_operation_id("operation-message"));
        drop(log);
        let replayed = replay_clarification(&dir, "dialogue-operation").unwrap();
        assert!(replayed.has_operation_id("operation-message"));
        let _ = fs::remove_dir_all(dir);
    }
}

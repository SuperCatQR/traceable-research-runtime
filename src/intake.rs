//! Pure Research Intake state transitions plus append-only JSONL replay.

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
    BriefValidationError, ConfirmedResearchBrief, RESEARCH_BRIEF_SCHEMA_VERSION, ResearchBrief,
    ResearchScope,
};

pub const INTAKE_EVENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_TOTAL_QUESTIONS: usize = 5;

const MAX_LEGACY_QUESTIONS_PER_EVENT: usize = 3;

const MAX_QUESTION_ID_CHARS: usize = 128;
const MAX_CLARIFICATION_TEXT_CHARS: usize = 4_000;
const MAX_OPTIONS_PER_QUESTION: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IntakeStatus {
    Draft,
    NeedsInput,
    ReadyToConfirm,
    IntakeFailed,
    Confirmed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationQuestion {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationAnswer {
    pub question_id: String,
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeModelOutput {
    pub brief_draft: ResearchBrief,
    pub question: Option<ClarificationQuestion>,
    pub ready_to_confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeEvent {
    pub schema_version: u32,
    #[serde(flatten)]
    pub kind: IntakeEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IntakeEventKind {
    IntakeStarted {
        clarification_id: String,
        original_question: String,
        revision: u32,
        created_at: DateTime<Utc>,
    },
    ClarificationAsked {
        revision: u32,
        round: u32,
        questions: Vec<ClarificationQuestion>,
        asked_at: DateTime<Utc>,
    },
    UserReplied {
        revision: u32,
        answers: Vec<ClarificationAnswer>,
        replied_at: DateTime<Utc>,
    },
    BriefRevised {
        revision: u32,
        brief: ResearchBrief,
        content_hash: String,
        ready_to_confirm: bool,
        revised_at: DateTime<Utc>,
    },
    Confirmed {
        revision: u32,
        run_id: String,
        confirmed_brief: ConfirmedResearchBrief,
    },
    Cancelled {
        revision: u32,
        cancelled_at: DateTime<Utc>,
    },
    IntakeFailed {
        revision: u32,
        message: String,
        failed_at: DateTime<Utc>,
    },
}

impl IntakeEvent {
    #[must_use]
    pub const fn new(kind: IntakeEventKind) -> Self {
        Self {
            schema_version: INTAKE_EVENT_SCHEMA_VERSION,
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntakeConfirmation {
    pub run_id: String,
    pub brief: ConfirmedResearchBrief,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntakeSession {
    pub clarification_id: String,
    pub original_question: String,
    pub revision: u32,
    pub status: IntakeStatus,
    pub brief_draft: Option<ResearchBrief>,
    pub content_hash: Option<String>,
    pub questions: Vec<ClarificationQuestion>,
    pub answers: Vec<ClarificationAnswer>,
    pub clarification_rounds: u32,
    pub failure: Option<String>,
    pub confirmation: Option<IntakeConfirmation>,
    #[serde(skip)]
    pending_question_ids: Vec<String>,
}

impl IntakeSession {
    #[must_use]
    pub fn pending_questions(&self) -> Vec<&ClarificationQuestion> {
        self.pending_question_ids
            .iter()
            .filter_map(|id| self.questions.iter().find(|question| &question.id == id))
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IntakeError {
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
        status: IntakeStatus,
        event: &'static str,
    },
    #[error("stale brief: current revision is {current_revision}, requested {requested_revision}")]
    StaleBrief {
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

pub type IntakeResult<T> = std::result::Result<T, IntakeError>;

/// Applies one audit event without I/O. The returned session is the sole state projection.
pub fn reduce_intake_event(
    current: Option<&IntakeSession>,
    event: &IntakeEvent,
) -> IntakeResult<IntakeSession> {
    if event.schema_version != INTAKE_EVENT_SCHEMA_VERSION {
        return Err(IntakeError::UnsupportedSchemaVersion(event.schema_version));
    }
    match (current, &event.kind) {
        (
            None,
            IntakeEventKind::IntakeStarted {
                clarification_id,
                original_question,
                revision,
                ..
            },
        ) => {
            validate_file_id(clarification_id, true)?;
            let original_question = original_question.trim();
            if original_question.is_empty() {
                return Err(IntakeError::InvalidEvent(
                    "original_question must not be empty".into(),
                ));
            }
            if original_question.chars().count() > 10_000 {
                return Err(IntakeError::InvalidEvent(
                    "original_question exceeds 10000 characters".into(),
                ));
            }
            if *revision != 0 {
                return Err(IntakeError::InvalidEvent(
                    "intake_started revision must be 0".into(),
                ));
            }
            Ok(IntakeSession {
                clarification_id: clarification_id.clone(),
                original_question: original_question.to_owned(),
                revision: 0,
                status: IntakeStatus::Draft,
                brief_draft: None,
                content_hash: None,
                questions: Vec::new(),
                answers: Vec::new(),
                clarification_rounds: 0,
                failure: None,
                confirmation: None,
                pending_question_ids: Vec::new(),
            })
        }
        (None, _) => Err(IntakeError::InvalidEvent(
            "intake_started must be the first event".into(),
        )),
        (Some(session), IntakeEventKind::IntakeStarted { .. }) => {
            transition_error(session, "intake_started")
        }
        (Some(session), event_kind) => reduce_existing(session, event_kind),
    }
}

fn reduce_existing(
    session: &IntakeSession,
    event: &IntakeEventKind,
) -> IntakeResult<IntakeSession> {
    if matches!(
        session.status,
        IntakeStatus::Confirmed | IntakeStatus::Cancelled
    ) {
        return transition_error(session, event_name(event));
    }
    let mut next = session.clone();
    match event {
        IntakeEventKind::BriefRevised {
            revision,
            brief,
            content_hash,
            ready_to_confirm,
            ..
        } => {
            if !matches!(
                session.status,
                IntakeStatus::Draft | IntakeStatus::ReadyToConfirm | IntakeStatus::IntakeFailed
            ) {
                return transition_error(session, "brief_revised");
            }
            if *revision
                != session
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| IntakeError::InvalidEvent("revision overflow".into()))?
            {
                return Err(IntakeError::InvalidEvent(format!(
                    "brief_revised revision must be {}",
                    session.revision + 1
                )));
            }
            let normalized = brief.clone().normalized(&session.original_question)?;
            if &normalized != brief {
                return Err(IntakeError::Brief(BriefValidationError::NonCanonical));
            }
            let actual_hash = brief.content_hash()?;
            if &actual_hash != content_hash {
                return Err(IntakeError::InvalidEvent(
                    "brief_revised content_hash does not match brief".into(),
                ));
            }
            next.revision = *revision;
            next.brief_draft = Some(brief.clone());
            next.content_hash = Some(content_hash.clone());
            next.status = if *ready_to_confirm {
                IntakeStatus::ReadyToConfirm
            } else {
                IntakeStatus::Draft
            };
            next.failure = None;
            next.pending_question_ids.clear();
        }
        IntakeEventKind::ClarificationAsked {
            revision,
            round,
            questions,
            ..
        } => {
            require_status(session, IntakeStatus::Draft, "clarification_asked")?;
            require_revision(session, *revision)?;
            if session.brief_draft.is_none() {
                return Err(IntakeError::InvalidEvent(
                    "clarification_asked requires a brief draft".into(),
                ));
            }
            if *round != session.clarification_rounds + 1 {
                return Err(IntakeError::InvalidEvent(
                    "clarification round is out of sequence".into(),
                ));
            }
            validate_questions(questions, &session.questions)?;
            if session.questions.len() + questions.len() > MAX_TOTAL_QUESTIONS {
                return Err(IntakeError::InvalidEvent(
                    "total clarification question limit exceeded".into(),
                ));
            }
            next.clarification_rounds = *round;
            next.pending_question_ids = questions
                .iter()
                .map(|question| question.id.clone())
                .collect();
            next.questions.extend(questions.clone());
            next.status = IntakeStatus::NeedsInput;
        }
        IntakeEventKind::UserReplied {
            revision, answers, ..
        } => {
            require_status(session, IntakeStatus::NeedsInput, "user_replied")?;
            require_revision(session, *revision)?;
            validate_answers(answers, &session.pending_question_ids)?;
            next.answers.extend(answers.clone());
            next.pending_question_ids.clear();
            next.status = IntakeStatus::Draft;
        }
        IntakeEventKind::Confirmed {
            revision,
            run_id,
            confirmed_brief,
        } => {
            if !matches!(
                session.status,
                IntakeStatus::NeedsInput | IntakeStatus::ReadyToConfirm
            ) {
                return transition_error(session, "confirmed");
            }
            require_revision(session, *revision)?;
            validate_file_id(run_id, false)?;
            let draft = session.brief_draft.as_ref().ok_or_else(|| {
                IntakeError::InvalidEvent("confirmed requires a brief draft".into())
            })?;
            if confirmed_brief.clarification_id() != session.clarification_id
                || confirmed_brief.brief() != draft
                || Some(confirmed_brief.content_hash()) != session.content_hash.as_deref()
            {
                return Err(IntakeError::InvalidEvent(
                    "confirmed brief does not match the current intake draft".into(),
                ));
            }
            next.status = IntakeStatus::Confirmed;
            next.pending_question_ids.clear();
            next.confirmation = Some(IntakeConfirmation {
                run_id: run_id.clone(),
                brief: confirmed_brief.clone(),
            });
        }
        IntakeEventKind::Cancelled { revision, .. } => {
            if !matches!(
                session.status,
                IntakeStatus::NeedsInput
                    | IntakeStatus::ReadyToConfirm
                    | IntakeStatus::IntakeFailed
            ) {
                return transition_error(session, "cancelled");
            }
            require_revision(session, *revision)?;
            next.status = IntakeStatus::Cancelled;
            next.pending_question_ids.clear();
        }
        IntakeEventKind::IntakeFailed {
            revision, message, ..
        } => {
            if !matches!(
                session.status,
                IntakeStatus::Draft | IntakeStatus::IntakeFailed
            ) {
                return transition_error(session, "intake_failed");
            }
            require_revision(session, *revision)?;
            let message = message.trim();
            if message.is_empty() || message.chars().count() > MAX_CLARIFICATION_TEXT_CHARS {
                return Err(IntakeError::InvalidEvent(
                    "intake_failed message must be 1..=4000 characters".into(),
                ));
            }
            next.status = IntakeStatus::IntakeFailed;
            next.failure = Some(message.to_owned());
            next.pending_question_ids.clear();
        }
        IntakeEventKind::IntakeStarted { .. } => unreachable!(),
    }
    Ok(next)
}

/// Converts one valid model response into deterministic audit events.
pub fn events_for_model_output(
    session: &IntakeSession,
    output: IntakeModelOutput,
    now: DateTime<Utc>,
) -> IntakeResult<Vec<IntakeEvent>> {
    if !matches!(
        session.status,
        IntakeStatus::Draft | IntakeStatus::IntakeFailed
    ) {
        return transition_error(session, "model_output");
    }
    let output = validate_model_output(output, &session.original_question)?;
    let can_ask = session.questions.len() < MAX_TOTAL_QUESTIONS && !output.ready_to_confirm;
    let questions: Vec<_> = if can_ask {
        output.question.into_iter().collect()
    } else {
        Vec::new()
    };
    let ready_to_confirm = output.ready_to_confirm || questions.is_empty();
    let revision = session
        .revision
        .checked_add(1)
        .ok_or_else(|| IntakeError::InvalidEvent("revision overflow".into()))?;
    let content_hash = output.brief_draft.content_hash()?;
    let mut events = vec![IntakeEvent::new(IntakeEventKind::BriefRevised {
        revision,
        brief: output.brief_draft,
        content_hash,
        ready_to_confirm,
        revised_at: now,
    })];
    if !questions.is_empty() {
        events.push(IntakeEvent::new(IntakeEventKind::ClarificationAsked {
            revision,
            round: session.clarification_rounds + 1,
            questions,
            asked_at: now,
        }));
    }
    Ok(events)
}

pub fn user_reply_event(
    session: &IntakeSession,
    requested_revision: u32,
    answer: &str,
    now: DateTime<Utc>,
) -> IntakeResult<IntakeEvent> {
    require_status(session, IntakeStatus::NeedsInput, "user_replied")?;
    require_revision(session, requested_revision)?;
    let question_id = match session.pending_question_ids.as_slice() {
        [question_id] => question_id.clone(),
        _ => {
            return Err(IntakeError::InvalidEvent(
                "single reply requires exactly one pending question".into(),
            ));
        }
    };
    validate_text(answer, MAX_CLARIFICATION_TEXT_CHARS, "clarification answer")?;
    Ok(IntakeEvent::new(IntakeEventKind::UserReplied {
        revision: requested_revision,
        answers: vec![ClarificationAnswer {
            question_id,
            answer: answer.trim().to_owned(),
        }],
        replied_at: now,
    }))
}

pub fn minimal_brief_event(
    session: &IntakeSession,
    now: DateTime<Utc>,
) -> IntakeResult<IntakeEvent> {
    let brief = ResearchBrief {
        schema_version: RESEARCH_BRIEF_SCHEMA_VERSION,
        original_question: session.original_question.clone(),
        research_question: session.original_question.clone(),
        desired_output: None,
        scope: ResearchScope::default(),
        source_constraints: Vec::new(),
        accepted_assumptions: Vec::new(),
    }
    .normalized(&session.original_question)?;
    Ok(IntakeEvent::new(IntakeEventKind::BriefRevised {
        revision: session
            .revision
            .checked_add(1)
            .ok_or_else(|| IntakeError::InvalidEvent("revision overflow".into()))?,
        content_hash: brief.content_hash()?,
        brief,
        ready_to_confirm: true,
        revised_at: now,
    }))
}

pub fn confirmation_event(
    session: &IntakeSession,
    requested_revision: u32,
    requested_content_hash: &str,
    run_id: String,
    now: DateTime<Utc>,
) -> IntakeResult<IntakeEvent> {
    if !matches!(
        session.status,
        IntakeStatus::NeedsInput | IntakeStatus::ReadyToConfirm
    ) {
        return transition_error(session, "confirmed");
    }
    if requested_revision != session.revision {
        return Err(IntakeError::StaleBrief {
            current_revision: session.revision,
            requested_revision,
        });
    }
    let current_hash = session
        .content_hash
        .clone()
        .ok_or_else(|| IntakeError::InvalidEvent("ready intake has no content_hash".into()))?;
    if requested_content_hash != current_hash {
        return Err(IntakeError::StaleContentHash {
            current_hash,
            requested_hash: requested_content_hash.to_owned(),
        });
    }
    validate_file_id(&run_id, false)?;
    let brief = session
        .brief_draft
        .clone()
        .ok_or_else(|| IntakeError::InvalidEvent("ready intake has no brief draft".into()))?;
    let confirmed_brief = ConfirmedResearchBrief::new(
        brief,
        &session.original_question,
        session.clarification_id.clone(),
        requested_content_hash,
        now,
    )?;
    Ok(IntakeEvent::new(IntakeEventKind::Confirmed {
        revision: requested_revision,
        run_id,
        confirmed_brief,
    }))
}

pub fn cancellation_event(session: &IntakeSession, now: DateTime<Utc>) -> IntakeEvent {
    IntakeEvent::new(IntakeEventKind::Cancelled {
        revision: session.revision,
        cancelled_at: now,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelParseOutcome {
    Accepted(IntakeModelOutput),
    RetryCorrection { error: String },
    Failed(IntakeEvent),
}

/// Attempt 1 requests one correction; attempt 2 records a recoverable failure.
pub fn parse_model_attempt(
    session: &IntakeSession,
    json: &str,
    attempt: u8,
    now: DateTime<Utc>,
) -> IntakeResult<ModelParseOutcome> {
    if !matches!(attempt, 1 | 2) {
        return Err(IntakeError::InvalidEvent(
            "model parse attempt must be 1 or 2".into(),
        ));
    }
    match parse_model_output(json, &session.original_question) {
        Ok(output) => Ok(ModelParseOutcome::Accepted(output)),
        Err(error) if attempt == 1 => Ok(ModelParseOutcome::RetryCorrection {
            error: error.to_string(),
        }),
        Err(error) => Ok(ModelParseOutcome::Failed(IntakeEvent::new(
            IntakeEventKind::IntakeFailed {
                revision: session.revision,
                message: format!("model returned invalid structured output twice: {error}"),
                failed_at: now,
            },
        ))),
    }
}

pub fn parse_model_output(json: &str, original_question: &str) -> IntakeResult<IntakeModelOutput> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    validate_model_json_shape(&value)?;
    validate_model_output(serde_json::from_value(value)?, original_question)
}

fn validate_model_output(
    mut output: IntakeModelOutput,
    original_question: &str,
) -> IntakeResult<IntakeModelOutput> {
    output.brief_draft = output.brief_draft.normalized(original_question)?;
    if let Some(question) = &output.question {
        validate_questions(std::slice::from_ref(question), &[])?;
    }
    if output.ready_to_confirm && output.question.is_some() {
        return Err(IntakeError::InvalidEvent(
            "ready_to_confirm output must not contain a question".into(),
        ));
    }
    if !output.ready_to_confirm && output.question.is_none() {
        return Err(IntakeError::InvalidEvent(
            "non-ready model output must contain a question".into(),
        ));
    }
    Ok(output)
}

fn validate_model_json_shape(value: &serde_json::Value) -> IntakeResult<()> {
    exact_keys(
        value,
        &["brief_draft", "question", "ready_to_confirm"],
        "model output",
    )?;
    let brief = value
        .get("brief_draft")
        .ok_or_else(|| IntakeError::InvalidEvent("model output is missing brief_draft".into()))?;
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
        brief
            .get("scope")
            .ok_or_else(|| IntakeError::InvalidEvent("brief_draft is missing scope".into()))?,
        &["time_range", "geography", "include", "exclude"],
        "brief_draft.scope",
    )?;
    Ok(())
}

fn exact_keys(value: &serde_json::Value, expected: &[&str], name: &str) -> IntakeResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| IntakeError::InvalidEvent(format!("{name} must be an object")))?;
    let actual: HashSet<_> = object.keys().map(String::as_str).collect();
    let expected: HashSet<_> = expected.iter().copied().collect();
    if actual != expected {
        return Err(IntakeError::InvalidEvent(format!(
            "{name} does not match the fixed schema"
        )));
    }
    Ok(())
}

fn validate_questions(
    questions: &[ClarificationQuestion],
    prior: &[ClarificationQuestion],
) -> IntakeResult<()> {
    if questions.is_empty() || questions.len() > MAX_LEGACY_QUESTIONS_PER_EVENT {
        return Err(IntakeError::InvalidEvent(
            "clarification event must contain 1..=3 questions".into(),
        ));
    }
    let mut ids: HashSet<&str> = prior.iter().map(|question| question.id.as_str()).collect();
    for question in questions {
        validate_text(&question.id, MAX_QUESTION_ID_CHARS, "question id")?;
        validate_text(
            &question.question,
            MAX_CLARIFICATION_TEXT_CHARS,
            "clarification question",
        )?;
        if !ids.insert(&question.id) {
            return Err(IntakeError::InvalidEvent(
                "clarification question ids must be unique".into(),
            ));
        }
        if question.options.len() > MAX_OPTIONS_PER_QUESTION {
            return Err(IntakeError::InvalidEvent(
                "clarification question has too many options".into(),
            ));
        }
        for option in &question.options {
            validate_text(option, MAX_CLARIFICATION_TEXT_CHARS, "question option")?;
        }
    }
    Ok(())
}

fn validate_answers(answers: &[ClarificationAnswer], pending: &[String]) -> IntakeResult<()> {
    let mut ids = HashSet::new();
    for answer in answers {
        validate_text(
            &answer.question_id,
            MAX_QUESTION_ID_CHARS,
            "answer question_id",
        )?;
        validate_text(
            &answer.answer,
            MAX_CLARIFICATION_TEXT_CHARS,
            "clarification answer",
        )?;
        if !ids.insert(answer.question_id.as_str()) {
            return Err(IntakeError::InvalidEvent(
                "clarification answers must have unique question ids".into(),
            ));
        }
    }
    let expected: HashSet<_> = pending.iter().map(String::as_str).collect();
    if ids != expected {
        return Err(IntakeError::InvalidEvent(
            "answers must cover exactly the pending questions".into(),
        ));
    }
    Ok(())
}

fn validate_text(value: &str, max_chars: usize, name: &str) -> IntakeResult<()> {
    let count = value.trim().chars().count();
    if count == 0 || count > max_chars {
        return Err(IntakeError::InvalidEvent(format!(
            "{name} must be 1..={max_chars} characters"
        )));
    }
    Ok(())
}

fn require_status(
    session: &IntakeSession,
    expected: IntakeStatus,
    event: &'static str,
) -> IntakeResult<()> {
    if session.status != expected {
        return transition_error(session, event);
    }
    Ok(())
}

fn require_revision(session: &IntakeSession, revision: u32) -> IntakeResult<()> {
    if revision != session.revision {
        return Err(IntakeError::InvalidEvent(format!(
            "event revision must equal current revision {}",
            session.revision
        )));
    }
    Ok(())
}

fn transition_error<T>(session: &IntakeSession, event: &'static str) -> IntakeResult<T> {
    Err(IntakeError::InvalidTransition {
        status: session.status,
        event,
    })
}

const fn event_name(event: &IntakeEventKind) -> &'static str {
    match event {
        IntakeEventKind::IntakeStarted { .. } => "intake_started",
        IntakeEventKind::ClarificationAsked { .. } => "clarification_asked",
        IntakeEventKind::UserReplied { .. } => "user_replied",
        IntakeEventKind::BriefRevised { .. } => "brief_revised",
        IntakeEventKind::Confirmed { .. } => "confirmed",
        IntakeEventKind::Cancelled { .. } => "cancelled",
        IntakeEventKind::IntakeFailed { .. } => "intake_failed",
    }
}

fn validate_file_id(value: &str, clarification: bool) -> IntakeResult<()> {
    let path = Path::new(value);
    let valid = !value.is_empty()
        && value != "."
        && value != ".."
        && path.file_name() == Some(OsStr::new(value))
        && path.components().count() == 1;
    if valid {
        Ok(())
    } else if clarification {
        Err(IntakeError::InvalidClarificationId)
    } else {
        Err(IntakeError::InvalidRunId)
    }
}

pub struct IntakeLog {
    writer: BufWriter<File>,
    session: IntakeSession,
}

impl IntakeLog {
    pub fn create(intake_dir: impl AsRef<Path>, started: IntakeEvent) -> IntakeResult<Self> {
        let session = reduce_intake_event(None, &started)?;
        fs::create_dir_all(intake_dir.as_ref())?;
        let path = intake_path(intake_dir.as_ref(), &session.clarification_id)?;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut log = Self {
            writer: BufWriter::new(file),
            session,
        };
        log.write_event(&started)?;
        Ok(log)
    }

    pub fn open(intake_dir: impl AsRef<Path>, clarification_id: &str) -> IntakeResult<Self> {
        let path = intake_path(intake_dir.as_ref(), clarification_id)?;
        let session = replay_path(&path)?;
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            session,
        })
    }

    #[must_use]
    pub const fn session(&self) -> &IntakeSession {
        &self.session
    }

    pub fn append(&mut self, event: &IntakeEvent) -> IntakeResult<()> {
        let next = reduce_intake_event(Some(&self.session), event)?;
        self.write_event(event)?;
        self.session = next;
        Ok(())
    }

    fn write_event(&mut self, event: &IntakeEvent) -> IntakeResult<()> {
        serde_json::to_writer(&mut self.writer, event)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

pub fn replay_intake(
    intake_dir: impl AsRef<Path>,
    clarification_id: &str,
) -> IntakeResult<IntakeSession> {
    replay_path(&intake_path(intake_dir.as_ref(), clarification_id)?)
}

fn replay_path(path: &Path) -> IntakeResult<IntakeSession> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut current = None;
    let mut line = String::new();
    let mut line_number = 0;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        line_number += 1;
        if !line.ends_with('\n') {
            return Err(IntakeError::TruncatedLine { line: line_number });
        }
        let event: IntakeEvent =
            serde_json::from_str(&line).map_err(|source| IntakeError::InvalidJsonLine {
                line: line_number,
                source,
            })?;
        current = Some(reduce_intake_event(current.as_ref(), &event)?);
    }
    current.ok_or(IntakeError::EmptyLog)
}

fn intake_path(intake_dir: &Path, clarification_id: &str) -> IntakeResult<PathBuf> {
    validate_file_id(clarification_id, true)?;
    Ok(intake_dir.join(format!("{clarification_id}.jsonl")))
}

/// Process-local per-session serialization. Cross-process locking is deliberately absent.
#[derive(Clone, Default)]
pub struct IntakeSessionLocks {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl IntakeSessionLocks {
    pub async fn lock(&self, clarification_id: &str) -> IntakeResult<OwnedMutexGuard<()>> {
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

    use super::*;

    const QUESTION: &str = "Compare Rust 2024 edition changes with Rust 2021";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 13, 10, 0, 0).unwrap()
    }

    fn started(id: &str) -> IntakeEvent {
        IntakeEvent::new(IntakeEventKind::IntakeStarted {
            clarification_id: id.into(),
            original_question: QUESTION.into(),
            revision: 0,
            created_at: now(),
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

    fn question(id: &str) -> ClarificationQuestion {
        ClarificationQuestion {
            id: id.into(),
            question: format!("Clarify {id}?"),
            options: vec!["A".into(), "Other / no restriction".into()],
        }
    }

    fn session() -> IntakeSession {
        reduce_intake_event(None, &started("clarification-1")).unwrap()
    }

    fn apply_all(mut state: IntakeSession, events: &[IntakeEvent]) -> IntakeSession {
        for event in events {
            state = reduce_intake_event(Some(&state), event).unwrap();
        }
        state
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("traceable-search-intake-{name}-{unique}"))
    }

    #[test]
    fn clear_question_skips_questions_but_waits_for_confirmation() {
        let state = session();
        let events = events_for_model_output(
            &state,
            IntakeModelOutput {
                brief_draft: brief(),
                question: None,
                ready_to_confirm: true,
            },
            now(),
        )
        .unwrap();
        let state = apply_all(state, &events);
        assert_eq!(state.status, IntakeStatus::ReadyToConfirm);
        assert_eq!(state.revision, 1);
        assert!(state.questions.is_empty());
        assert!(state.confirmation.is_none());
    }

    #[test]
    fn ambiguous_question_needs_input() {
        let state = session();
        let events = events_for_model_output(
            &state,
            IntakeModelOutput {
                brief_draft: brief(),
                question: Some(question("database-kind")),
                ready_to_confirm: false,
            },
            now(),
        )
        .unwrap();
        let state = apply_all(state, &events);
        assert_eq!(state.status, IntakeStatus::NeedsInput);
        assert_eq!(state.clarification_rounds, 1);
        assert_eq!(state.pending_questions().len(), 1);
    }

    #[test]
    fn five_single_questions_stop_a_sixth_followup() {
        let mut state = session();
        for number in 1..=MAX_TOTAL_QUESTIONS {
            let events = events_for_model_output(
                &state,
                IntakeModelOutput {
                    brief_draft: brief(),
                    question: Some(question(&format!("q{number}"))),
                    ready_to_confirm: false,
                },
                now(),
            )
            .unwrap();
            assert_eq!(events.len(), 2);
            state = apply_all(state, &events);
            let reply = user_reply_event(&state, state.revision, "No restriction", now()).unwrap();
            state = reduce_intake_event(Some(&state), &reply).unwrap();
        }
        let events = events_for_model_output(
            &state,
            IntakeModelOutput {
                brief_draft: brief(),
                question: Some(question("q6")),
                ready_to_confirm: false,
            },
            now(),
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        state = apply_all(state, &events);
        assert_eq!(state.status, IntakeStatus::ReadyToConfirm);
        assert_eq!(state.questions.len(), MAX_TOTAL_QUESTIONS);
        assert_eq!(state.clarification_rounds as usize, MAX_TOTAL_QUESTIONS);
    }

    #[test]
    fn single_reply_rejects_blank_or_legacy_multiple_pending_questions() {
        let base = session();
        let draft = brief();
        let content_hash = draft.content_hash().unwrap();
        let draft_state = reduce_intake_event(
            Some(&base),
            &IntakeEvent::new(IntakeEventKind::BriefRevised {
                revision: 1,
                brief: draft,
                content_hash,
                ready_to_confirm: false,
                revised_at: now(),
            }),
        )
        .unwrap();
        let legacy = reduce_intake_event(
            Some(&draft_state),
            &IntakeEvent::new(IntakeEventKind::ClarificationAsked {
                revision: 1,
                round: 1,
                questions: vec![question("legacy-1"), question("legacy-2")],
                asked_at: now(),
            }),
        )
        .unwrap();
        assert!(matches!(
            user_reply_event(&legacy, legacy.revision, "A", now()),
            Err(IntakeError::InvalidEvent(_))
        ));

        let one = apply_all(
            base.clone(),
            &events_for_model_output(
                &base,
                IntakeModelOutput {
                    brief_draft: brief(),
                    question: Some(question("q1")),
                    ready_to_confirm: false,
                },
                now(),
            )
            .unwrap(),
        );
        assert!(matches!(
            user_reply_event(&one, one.revision, "   ", now()),
            Err(IntakeError::InvalidEvent(_))
        ));
    }

    #[test]
    fn needs_input_can_be_confirmed() {
        let base = session();
        let state = apply_all(
            base.clone(),
            &events_for_model_output(
                &base,
                IntakeModelOutput {
                    brief_draft: brief(),
                    question: Some(question("q1")),
                    ready_to_confirm: false,
                },
                now(),
            )
            .unwrap(),
        );
        let event = confirmation_event(
            &state,
            state.revision,
            state.content_hash.as_deref().unwrap(),
            "run-needs-input".into(),
            now(),
        )
        .unwrap();
        let confirmed = reduce_intake_event(Some(&state), &event).unwrap();
        assert_eq!(confirmed.status, IntakeStatus::Confirmed);
        assert!(confirmed.pending_questions().is_empty());
    }

    #[test]
    fn all_three_recoverable_states_can_cancel() {
        let base = session();
        let needs = apply_all(
            base.clone(),
            &events_for_model_output(
                &base,
                IntakeModelOutput {
                    brief_draft: brief(),
                    question: Some(question("q1")),
                    ready_to_confirm: false,
                },
                now(),
            )
            .unwrap(),
        );
        let ready = apply_all(
            base.clone(),
            &events_for_model_output(
                &base,
                IntakeModelOutput {
                    brief_draft: brief(),
                    question: None,
                    ready_to_confirm: true,
                },
                now(),
            )
            .unwrap(),
        );
        let failed = reduce_intake_event(
            Some(&base),
            &IntakeEvent::new(IntakeEventKind::IntakeFailed {
                revision: 0,
                message: "model unavailable".into(),
                failed_at: now(),
            }),
        )
        .unwrap();
        for state in [needs, ready, failed] {
            let cancelled =
                reduce_intake_event(Some(&state), &cancellation_event(&state, now())).unwrap();
            assert_eq!(cancelled.status, IntakeStatus::Cancelled);
        }
    }

    #[test]
    fn stale_revision_and_hash_are_rejected() {
        let base = session();
        let state = apply_all(
            base.clone(),
            &events_for_model_output(
                &base,
                IntakeModelOutput {
                    brief_draft: brief(),
                    question: None,
                    ready_to_confirm: true,
                },
                now(),
            )
            .unwrap(),
        );
        assert!(matches!(
            confirmation_event(
                &state,
                0,
                state.content_hash.as_deref().unwrap(),
                "run-1".into(),
                now()
            ),
            Err(IntakeError::StaleBrief { .. })
        ));
        assert!(matches!(
            confirmation_event(&state, 1, "sha256:stale", "run-1".into(), now()),
            Err(IntakeError::StaleContentHash { .. })
        ));
    }

    #[test]
    fn second_bad_model_json_enters_recoverable_failure() {
        let state = session();
        assert!(matches!(
            parse_model_attempt(&state, "not json", 1, now()).unwrap(),
            ModelParseOutcome::RetryCorrection { .. }
        ));
        let ModelParseOutcome::Failed(event) =
            parse_model_attempt(&state, "still not json", 2, now()).unwrap()
        else {
            panic!("second invalid output must fail intake");
        };
        let state = reduce_intake_event(Some(&state), &event).unwrap();
        assert_eq!(state.status, IntakeStatus::IntakeFailed);
        let recovered =
            reduce_intake_event(Some(&state), &minimal_brief_event(&state, now()).unwrap())
                .unwrap();
        assert_eq!(recovered.status, IntakeStatus::ReadyToConfirm);
    }

    #[test]
    fn confirmed_event_replays_same_run_id_before_trace_exists() {
        let dir = temp_dir("crash-window");
        let mut log = IntakeLog::create(&dir, started("clarification-1")).unwrap();
        let events = events_for_model_output(
            log.session(),
            IntakeModelOutput {
                brief_draft: brief(),
                question: None,
                ready_to_confirm: true,
            },
            now(),
        )
        .unwrap();
        for event in &events {
            log.append(event).unwrap();
        }
        let confirmation = confirmation_event(
            log.session(),
            log.session().revision,
            log.session().content_hash.as_deref().unwrap(),
            "run-fixed".into(),
            now(),
        )
        .unwrap();
        log.append(&confirmation).unwrap();
        drop(log);

        let replayed = replay_intake(&dir, "clarification-1").unwrap();
        assert_eq!(replayed.status, IntakeStatus::Confirmed);
        assert_eq!(replayed.confirmation.unwrap().run_id, "run-fixed");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn online_and_replayed_states_match() {
        let dir = temp_dir("replay");
        let mut log = IntakeLog::create(&dir, started("clarification-1")).unwrap();
        for event in events_for_model_output(
            log.session(),
            IntakeModelOutput {
                brief_draft: brief(),
                question: Some(question("q1")),
                ready_to_confirm: false,
            },
            now(),
        )
        .unwrap()
        {
            log.append(&event).unwrap();
        }
        let online = log.session().clone();
        drop(log);
        assert_eq!(replay_intake(&dir, "clarification-1").unwrap(), online);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn truncated_last_line_is_locatable() {
        let dir = temp_dir("truncated");
        fs::create_dir_all(&dir).unwrap();
        let line = serde_json::to_string(&started("clarification-1")).unwrap();
        fs::write(dir.join("clarification-1.jsonl"), line).unwrap();
        assert!(matches!(
            replay_intake(&dir, "clarification-1"),
            Err(IntakeError::TruncatedLine { line: 1 })
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_event_version_is_rejected() {
        let mut event = started("clarification-1");
        event.schema_version = 99;
        assert!(matches!(
            reduce_intake_event(None, &event),
            Err(IntakeError::UnsupportedSchemaVersion(99))
        ));
    }

    #[test]
    fn terminal_state_rejects_further_events() {
        let base = session();
        let ready = apply_all(
            base.clone(),
            &events_for_model_output(
                &base,
                IntakeModelOutput {
                    brief_draft: brief(),
                    question: None,
                    ready_to_confirm: true,
                },
                now(),
            )
            .unwrap(),
        );
        let cancelled =
            reduce_intake_event(Some(&ready), &cancellation_event(&ready, now())).unwrap();
        assert!(matches!(
            reduce_intake_event(
                Some(&cancelled),
                &minimal_brief_event(&cancelled, now()).unwrap()
            ),
            Err(IntakeError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn path_traversal_is_rejected_before_io() {
        assert!(matches!(
            replay_intake("unused", "../escape"),
            Err(IntakeError::InvalidClarificationId)
        ));
        assert!(matches!(
            reduce_intake_event(None, &started("../escape")),
            Err(IntakeError::InvalidClarificationId)
        ));
    }

    #[test]
    fn model_json_schema_rejects_unknown_fields() {
        let value = serde_json::json!({
            "brief_draft": brief(),
            "questions": [],
            "ready_to_confirm": true,
            "surprise": true
        });
        assert!(parse_model_output(&value.to_string(), QUESTION).is_err());
    }
}

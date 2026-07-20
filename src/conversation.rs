use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedMutexGuard};

pub const CONVERSATION_EVENT_SCHEMA_VERSION: u32 = 2;

pub type ConversationResult<T> = std::result::Result<T, ConversationError>;

#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("invalid conversation id")]
    InvalidSessionId,
    #[error("invalid clarification id")]
    InvalidClarificationId,
    #[error("invalid run id")]
    InvalidRunId,
    #[error("invalid conversation event: {0}")]
    InvalidEvent(String),
    #[error("conversation log is empty")]
    EmptyLog,
    #[error("conversation log line {line} is truncated")]
    TruncatedLine { line: usize },
    #[error("invalid JSON on conversation log line {line}: {source}")]
    InvalidJsonLine {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationEvent {
    pub schema_version: u32,
    #[serde(flatten)]
    pub kind: ConversationEventKind,
}

impl ConversationEvent {
    #[must_use]
    pub const fn new(kind: ConversationEventKind) -> Self {
        Self {
            schema_version: CONVERSATION_EVENT_SCHEMA_VERSION,
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ConversationEventKind {
    #[serde(rename = "session_started")]
    ConversationStarted {
        session_id: String,
        created_at: DateTime<Utc>,
    },
    TurnStarted {
        session_id: String,
        turn: u64,
        clarification_id: String,
        user_question: String,
        started_at: DateTime<Utc>,
    },
    TurnCompleted {
        session_id: String,
        turn: u64,
        clarification_id: String,
        run_id: String,
        answer: String,
        completed_at: DateTime<Utc>,
    },
    TurnCancelled {
        session_id: String,
        turn: u64,
        clarification_id: String,
        cancelled_at: DateTime<Utc>,
    },
    TurnFailed {
        session_id: String,
        turn: u64,
        clarification_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        failed_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedResearchTurn {
    pub turn: u64,
    pub clarification_id: String,
    pub user_question: String,
    pub run_id: String,
    pub answer: String,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnansweredResearchTurn {
    pub turn: u64,
    pub clarification_id: String,
    pub user_question: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchConversation {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub completed_turns: Vec<CompletedResearchTurn>,
    pub pending_turns: Vec<UnansweredResearchTurn>,
    pub cancelled_turns: Vec<UnansweredResearchTurn>,
    pub failed_turns: Vec<UnansweredResearchTurn>,
}

impl ResearchConversation {
    #[must_use]
    pub fn completed_turn_history(&self) -> Vec<CompletedTurnContext> {
        self.completed_turns
            .iter()
            .map(|turn| CompletedTurnContext {
                turn: turn.turn,
                user_question: turn.user_question.clone(),
                answer: turn.answer.clone(),
            })
            .collect()
    }

    #[must_use]
    pub fn next_turn_number(&self) -> u64 {
        self.completed_turns
            .iter()
            .map(|turn| turn.turn)
            .chain(self.pending_turns.iter().map(|turn| turn.turn))
            .chain(self.cancelled_turns.iter().map(|turn| turn.turn))
            .chain(self.failed_turns.iter().map(|turn| turn.turn))
            .max()
            .unwrap_or(0)
            + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedTurnContext {
    pub turn: u64,
    pub user_question: String,
    pub answer: String,
}

pub fn reduce_conversation_event(
    current: Option<&ResearchConversation>,
    event: &ConversationEvent,
) -> ConversationResult<ResearchConversation> {
    if event.schema_version != CONVERSATION_EVENT_SCHEMA_VERSION {
        return Err(ConversationError::InvalidEvent(format!(
            "unsupported schema version {}",
            event.schema_version
        )));
    }
    match (&event.kind, current) {
        (
            ConversationEventKind::ConversationStarted {
                session_id,
                created_at,
            },
            None,
        ) => {
            validate_id(session_id, IdKind::Session)?;
            Ok(ResearchConversation {
                session_id: session_id.clone(),
                created_at: *created_at,
                completed_turns: Vec::new(),
                pending_turns: Vec::new(),
                cancelled_turns: Vec::new(),
                failed_turns: Vec::new(),
            })
        }
        (ConversationEventKind::ConversationStarted { .. }, Some(_)) => Err(
            ConversationError::InvalidEvent("duplicate session_started".into()),
        ),
        (ConversationEventKind::TurnStarted { .. }, None)
        | (ConversationEventKind::TurnCompleted { .. }, None)
        | (ConversationEventKind::TurnCancelled { .. }, None)
        | (ConversationEventKind::TurnFailed { .. }, None) => Err(ConversationError::InvalidEvent(
            "first event must be session_started".into(),
        )),
        (
            ConversationEventKind::TurnStarted {
                session_id,
                turn,
                clarification_id,
                user_question,
                started_at,
            },
            Some(conversation),
        ) => {
            require_session(conversation, session_id)?;
            validate_id(clarification_id, IdKind::Clarification)?;
            if user_question.trim().is_empty() {
                return Err(ConversationError::InvalidEvent(
                    "turn question must not be empty".into(),
                ));
            }
            if let Some(pending) = conversation.pending_turns.first() {
                if pending.turn == *turn
                    && pending.clarification_id == *clarification_id
                    && pending.user_question == *user_question
                    && pending.started_at == *started_at
                {
                    return Ok(conversation.clone());
                }
                return Err(ConversationError::InvalidEvent(
                    "conversation already has an unfinished turn".into(),
                ));
            }
            if *turn != conversation.next_turn_number() {
                return Err(ConversationError::InvalidEvent(format!(
                    "turn must be {}, got {turn}",
                    conversation.next_turn_number()
                )));
            }
            if conversation
                .pending_turns
                .iter()
                .any(|pending| pending.clarification_id == *clarification_id)
                || conversation
                    .completed_turns
                    .iter()
                    .any(|completed| completed.clarification_id == *clarification_id)
            {
                return Err(ConversationError::InvalidEvent(
                    "clarification id already belongs to this conversation".into(),
                ));
            }
            let mut next = conversation.clone();
            next.pending_turns.push(UnansweredResearchTurn {
                turn: *turn,
                clarification_id: clarification_id.clone(),
                user_question: user_question.clone(),
                started_at: *started_at,
            });
            Ok(next)
        }
        (
            ConversationEventKind::TurnCompleted {
                session_id,
                turn,
                clarification_id,
                run_id,
                answer,
                completed_at,
            },
            Some(conversation),
        ) => {
            require_session(conversation, session_id)?;
            validate_id(clarification_id, IdKind::Clarification)?;
            validate_id(run_id, IdKind::Run)?;
            if answer.trim().is_empty() {
                return Err(ConversationError::InvalidEvent(
                    "completed answer must not be empty".into(),
                ));
            }
            if let Some(completed) = conversation
                .completed_turns
                .iter()
                .find(|completed| completed.run_id == *run_id)
            {
                if completed.turn == *turn
                    && completed.clarification_id == *clarification_id
                    && completed.answer == *answer
                {
                    return Ok(conversation.clone());
                }
                return Err(ConversationError::InvalidEvent(
                    "run id already completed with different data".into(),
                ));
            }
            let position = conversation
                .pending_turns
                .iter()
                .position(|pending| {
                    pending.turn == *turn && pending.clarification_id == *clarification_id
                })
                .ok_or_else(|| {
                    ConversationError::InvalidEvent(
                        "turn_completed has no matching pending turn".into(),
                    )
                })?;
            let mut next = conversation.clone();
            let pending = next.pending_turns.remove(position);
            next.completed_turns.push(CompletedResearchTurn {
                turn: *turn,
                clarification_id: clarification_id.clone(),
                user_question: pending.user_question,
                run_id: run_id.clone(),
                answer: answer.clone(),
                completed_at: *completed_at,
            });
            next.completed_turns.sort_by_key(|completed| completed.turn);
            Ok(next)
        }
        (
            ConversationEventKind::TurnCancelled {
                session_id,
                turn,
                clarification_id,
                cancelled_at: _,
            },
            Some(conversation),
        ) => {
            require_session(conversation, session_id)?;
            validate_id(clarification_id, IdKind::Clarification)?;
            let Some(position) = conversation.pending_turns.iter().position(|pending| {
                pending.turn == *turn && pending.clarification_id == *clarification_id
            }) else {
                if conversation.cancelled_turns.iter().any(|cancelled| {
                    cancelled.turn == *turn && cancelled.clarification_id == *clarification_id
                }) {
                    return Ok(conversation.clone());
                }
                return Err(ConversationError::InvalidEvent(
                    "turn_cancelled has no matching pending turn".into(),
                ));
            };
            let mut next = conversation.clone();
            let pending = next.pending_turns.remove(position);
            next.cancelled_turns.push(pending);
            Ok(next)
        }
        (
            ConversationEventKind::TurnFailed {
                session_id,
                turn,
                clarification_id,
                run_id,
                failed_at: _,
            },
            Some(conversation),
        ) => {
            require_session(conversation, session_id)?;
            validate_id(clarification_id, IdKind::Clarification)?;
            if let Some(run_id) = run_id {
                validate_id(run_id, IdKind::Run)?;
            }
            let Some(position) = conversation.pending_turns.iter().position(|pending| {
                pending.turn == *turn && pending.clarification_id == *clarification_id
            }) else {
                if conversation.failed_turns.iter().any(|failed| {
                    failed.turn == *turn && failed.clarification_id == *clarification_id
                }) {
                    return Ok(conversation.clone());
                }
                return Err(ConversationError::InvalidEvent(
                    "turn_failed has no matching pending turn".into(),
                ));
            };
            let mut next = conversation.clone();
            let pending = next.pending_turns.remove(position);
            next.failed_turns.push(pending);
            Ok(next)
        }
    }
}

fn require_session(
    conversation: &ResearchConversation,
    session_id: &str,
) -> ConversationResult<()> {
    validate_id(session_id, IdKind::Session)?;
    if conversation.session_id == session_id {
        Ok(())
    } else {
        Err(ConversationError::InvalidEvent(
            "conversation id mismatch".into(),
        ))
    }
}

#[derive(Clone, Copy)]
enum IdKind {
    Session,
    Clarification,
    Run,
}

fn validate_id(value: &str, kind: IdKind) -> ConversationResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(match kind {
            IdKind::Session => ConversationError::InvalidSessionId,
            IdKind::Clarification => ConversationError::InvalidClarificationId,
            IdKind::Run => ConversationError::InvalidRunId,
        })
    }
}

pub struct ConversationEventLog {
    writer: BufWriter<File>,
    conversation: ResearchConversation,
}

impl ConversationEventLog {
    pub fn create(
        sessions_dir: impl AsRef<Path>,
        started: ConversationEvent,
    ) -> ConversationResult<Self> {
        let conversation = reduce_conversation_event(None, &started)?;
        create_private_dir(sessions_dir.as_ref())?;
        let path = session_path(sessions_dir.as_ref(), &conversation.session_id)?;
        let file = create_private_file(&path)?;
        let mut log = Self {
            writer: BufWriter::new(file),
            conversation,
        };
        log.write_event(&started)?;
        Ok(log)
    }

    /// Create a conversation with a caller-reserved identifier, or reopen the
    /// exact same conversation after a process exit.  The event log is the
    /// source of truth; an existing file is accepted only when its semantic
    /// session identity matches the requested seed.
    pub fn create_idempotent(
        sessions_dir: impl AsRef<Path>,
        started: ConversationEvent,
    ) -> ConversationResult<Self> {
        let expected = reduce_conversation_event(None, &started)?;
        let sessions_dir = sessions_dir.as_ref();
        create_private_dir(sessions_dir)?;
        let path = session_path(sessions_dir, &expected.session_id)?;
        match create_private_file(&path) {
            Ok(file) => {
                let mut log = Self {
                    writer: BufWriter::new(file),
                    conversation: expected,
                };
                log.write_event(&started)?;
                Ok(log)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match Self::open(sessions_dir, &expected.session_id) {
                    Ok(log) => {
                        if log.conversation.session_id == expected.session_id
                            && log.conversation.created_at == expected.created_at
                        {
                            Ok(log)
                        } else {
                            Err(ConversationError::InvalidEvent(
                                "existing conversation does not match operation seed".into(),
                            ))
                        }
                    }
                    Err(ConversationError::EmptyLog) => {
                        // A process can die after create_new but before the
                        // first complete JSONL record.  The reserved ID makes
                        // replacing this empty/torn seed unambiguous.
                        let file = OpenOptions::new().write(true).truncate(true).open(&path)?;
                        let mut log = Self {
                            writer: BufWriter::new(file),
                            conversation: expected,
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

    pub fn open(sessions_dir: impl AsRef<Path>, session_id: &str) -> ConversationResult<Self> {
        let path = session_path(sessions_dir.as_ref(), session_id)?;
        let (conversation, valid_bytes, truncated) = replay_path(&path)?;
        if truncated {
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(valid_bytes)?;
        }
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            conversation,
        })
    }

    #[must_use]
    pub const fn conversation(&self) -> &ResearchConversation {
        &self.conversation
    }

    pub fn append(&mut self, event: &ConversationEvent) -> ConversationResult<()> {
        let next = reduce_conversation_event(Some(&self.conversation), event)?;
        if next == self.conversation {
            return Ok(());
        }
        self.write_event(event)?;
        self.conversation = next;
        Ok(())
    }

    fn write_event(&mut self, event: &ConversationEvent) -> ConversationResult<()> {
        serde_json::to_writer(&mut self.writer, event)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

pub fn replay_conversation(
    sessions_dir: impl AsRef<Path>,
    session_id: &str,
) -> ConversationResult<ResearchConversation> {
    replay_path(&session_path(sessions_dir.as_ref(), session_id)?)
        .map(|(conversation, _, _)| conversation)
}

fn replay_path(path: &Path) -> ConversationResult<(ResearchConversation, u64, bool)> {
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
        let event: ConversationEvent =
            serde_json::from_str(&line).map_err(|source| ConversationError::InvalidJsonLine {
                line: line_number,
                source,
            })?;
        current = Some(reduce_conversation_event(current.as_ref(), &event)?);
        valid_bytes += bytes as u64;
    }
    current
        .map(|conversation| (conversation, valid_bytes, truncated))
        .ok_or(ConversationError::EmptyLog)
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn session_path(sessions_dir: &Path, session_id: &str) -> ConversationResult<PathBuf> {
    validate_id(session_id, IdKind::Session)?;
    Ok(sessions_dir.join(format!("{session_id}.jsonl")))
}

#[derive(Clone, Default)]
pub struct ConversationLocks {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl ConversationLocks {
    pub async fn lock(&self, session_id: &str) -> ConversationResult<OwnedMutexGuard<()>> {
        validate_id(session_id, IdKind::Session)?;
        let lock = {
            let mut locks = self.inner.lock().await;
            locks
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        Ok(lock.lock_owned().await)
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::from(SystemTime::UNIX_EPOCH)
    }

    fn started() -> ConversationEvent {
        ConversationEvent::new(ConversationEventKind::ConversationStarted {
            session_id: "conversation-1".into(),
            created_at: now(),
        })
    }

    #[test]
    fn completed_turns_form_ordered_history() {
        let mut conversation = reduce_conversation_event(None, &started()).unwrap();
        for turn in 1..=2 {
            conversation = reduce_conversation_event(
                Some(&conversation),
                &ConversationEvent::new(ConversationEventKind::TurnStarted {
                    session_id: "conversation-1".into(),
                    turn,
                    clarification_id: format!("clarification-{turn}"),
                    user_question: format!("question {turn}"),
                    started_at: now(),
                }),
            )
            .unwrap();
            conversation = reduce_conversation_event(
                Some(&conversation),
                &ConversationEvent::new(ConversationEventKind::TurnCompleted {
                    session_id: "conversation-1".into(),
                    turn,
                    clarification_id: format!("clarification-{turn}"),
                    run_id: format!("run-{turn}"),
                    answer: format!("answer {turn}"),
                    completed_at: now(),
                }),
            )
            .unwrap();
        }
        assert_eq!(conversation.completed_turn_history().len(), 2);
        assert_eq!(
            conversation.completed_turn_history()[1].user_question,
            "question 2"
        );
        assert!(conversation.pending_turns.is_empty());
    }

    #[test]
    fn incomplete_turn_is_not_history() {
        let conversation = reduce_conversation_event(None, &started()).unwrap();
        let conversation = reduce_conversation_event(
            Some(&conversation),
            &ConversationEvent::new(ConversationEventKind::TurnStarted {
                session_id: "conversation-1".into(),
                turn: 1,
                clarification_id: "clarification-1".into(),
                user_question: "question".into(),
                started_at: now(),
            }),
        )
        .unwrap();
        assert!(conversation.completed_turn_history().is_empty());
        assert_eq!(conversation.pending_turns.len(), 1);
    }

    #[test]
    fn completion_is_idempotent_for_identical_run() {
        let conversation = reduce_conversation_event(None, &started()).unwrap();
        let conversation = reduce_conversation_event(
            Some(&conversation),
            &ConversationEvent::new(ConversationEventKind::TurnStarted {
                session_id: "conversation-1".into(),
                turn: 1,
                clarification_id: "clarification-1".into(),
                user_question: "question".into(),
                started_at: now(),
            }),
        )
        .unwrap();
        let completed = ConversationEvent::new(ConversationEventKind::TurnCompleted {
            session_id: "conversation-1".into(),
            turn: 1,
            clarification_id: "clarification-1".into(),
            run_id: "run-1".into(),
            answer: "answer".into(),
            completed_at: now(),
        });
        let conversation = reduce_conversation_event(Some(&conversation), &completed).unwrap();
        assert_eq!(
            reduce_conversation_event(Some(&conversation), &completed).unwrap(),
            conversation
        );
    }

    #[test]
    fn log_round_trips_and_rejects_wrong_turn() {
        let dir = test_dir("round-trip");
        let mut log = ConversationEventLog::create(&dir, started()).unwrap();
        let error = log
            .append(&ConversationEvent::new(
                ConversationEventKind::TurnStarted {
                    session_id: "conversation-1".into(),
                    turn: 2,
                    clarification_id: "clarification-2".into(),
                    user_question: "question".into(),
                    started_at: now(),
                },
            ))
            .unwrap_err();
        assert!(matches!(error, ConversationError::InvalidEvent(_)));
        assert_eq!(
            replay_conversation(&dir, "conversation-1").unwrap(),
            *log.conversation()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_pending_turn_is_allowed_and_cancellation_releases_conversation() {
        let conversation = reduce_conversation_event(None, &started()).unwrap();
        let started_turn = ConversationEvent::new(ConversationEventKind::TurnStarted {
            session_id: "conversation-1".into(),
            turn: 1,
            clarification_id: "clarification-1".into(),
            user_question: "question 1".into(),
            started_at: now(),
        });
        let conversation = reduce_conversation_event(Some(&conversation), &started_turn).unwrap();
        assert_eq!(
            reduce_conversation_event(Some(&conversation), &started_turn).unwrap(),
            conversation
        );
        let conflict = ConversationEvent::new(ConversationEventKind::TurnStarted {
            session_id: "conversation-1".into(),
            turn: 2,
            clarification_id: "clarification-2".into(),
            user_question: "question 2".into(),
            started_at: now(),
        });
        assert!(reduce_conversation_event(Some(&conversation), &conflict).is_err());

        let cancelled = ConversationEvent::new(ConversationEventKind::TurnCancelled {
            session_id: "conversation-1".into(),
            turn: 1,
            clarification_id: "clarification-1".into(),
            cancelled_at: now(),
        });
        let conversation = reduce_conversation_event(Some(&conversation), &cancelled).unwrap();
        assert_eq!(
            reduce_conversation_event(Some(&conversation), &cancelled).unwrap(),
            conversation
        );
        assert_eq!(conversation.next_turn_number(), 2);
        assert!(reduce_conversation_event(Some(&conversation), &conflict).is_ok());
    }

    #[test]
    fn pre_run_failure_releases_a_turn_without_inventing_a_run_id() {
        let conversation = reduce_conversation_event(None, &started()).unwrap();
        let started_turn = ConversationEvent::new(ConversationEventKind::TurnStarted {
            session_id: "conversation-1".into(),
            turn: 1,
            clarification_id: "clarification-1".into(),
            user_question: "question".into(),
            started_at: now(),
        });
        let conversation = reduce_conversation_event(Some(&conversation), &started_turn).unwrap();
        let failed = ConversationEvent::new(ConversationEventKind::TurnFailed {
            session_id: "conversation-1".into(),
            turn: 1,
            clarification_id: "clarification-1".into(),
            run_id: None,
            failed_at: now(),
        });
        let conversation = reduce_conversation_event(Some(&conversation), &failed).unwrap();
        assert!(conversation.pending_turns.is_empty());
        assert_eq!(conversation.failed_turns.len(), 1);
        assert_eq!(conversation.next_turn_number(), 2);
        assert_eq!(
            reduce_conversation_event(Some(&conversation), &failed).unwrap(),
            conversation
        );
    }

    #[test]
    fn open_repairs_only_a_truncated_tail() {
        let dir = test_dir("truncated-tail");
        drop(ConversationEventLog::create(&dir, started()).unwrap());
        let path = dir.join("conversation-1.jsonl");
        let valid_len = fs::metadata(&path).unwrap().len();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"schema_version\":1").unwrap();
        drop(file);

        let log = ConversationEventLog::open(&dir, "conversation-1").unwrap();
        assert_eq!(log.conversation().session_id, "conversation-1");
        assert_eq!(fs::metadata(&path).unwrap().len(), valid_len);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn idempotent_create_repairs_empty_and_reopens_torn_seed() {
        let dir = test_dir("idempotent-seed");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("conversation-1.jsonl");
        File::create(&path).unwrap();
        let seed = started();
        let mut log = ConversationEventLog::create_idempotent(&dir, seed.clone()).unwrap();
        log.append(&ConversationEvent::new(
            ConversationEventKind::TurnStarted {
                session_id: "conversation-1".into(),
                turn: 1,
                clarification_id: "clarification-1".into(),
                user_question: "question".into(),
                started_at: now(),
            },
        ))
        .unwrap();
        let valid_len = fs::metadata(&path).unwrap().len();
        assert_eq!(log.conversation().pending_turns.len(), 1);
        drop(log);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"schema_version\":2")
            .unwrap();
        let reopened = ConversationEventLog::create_idempotent(&dir, seed).unwrap();
        assert_eq!(reopened.conversation().session_id, "conversation-1");
        assert_eq!(reopened.conversation().pending_turns.len(), 1);
        assert_eq!(fs::metadata(&path).unwrap().len(), valid_len);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn idempotent_create_rejects_a_different_seed_for_reserved_id() {
        let dir = test_dir("idempotent-mismatch");
        let first = ConversationEventLog::create_idempotent(&dir, started()).unwrap();
        drop(first);
        let different = ConversationEvent::new(ConversationEventKind::ConversationStarted {
            session_id: "conversation-1".into(),
            created_at: now() + chrono::Duration::seconds(1),
        });
        assert!(matches!(
            ConversationEventLog::create_idempotent(&dir, different),
            Err(ConversationError::InvalidEvent(_))
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn identifiers_are_portable_ascii_file_names() {
        let too_long = "x".repeat(129);
        for invalid in ["", "../x", "a:b", "with space", "中文", &too_long] {
            assert!(matches!(
                validate_id(invalid, IdKind::Session),
                Err(ConversationError::InvalidSessionId)
            ));
        }
        assert!(validate_id("session_ABC-123", IdKind::Session).is_ok());
    }

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "traceable-conversation-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}

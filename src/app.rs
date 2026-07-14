use std::{
    collections::HashMap,
    env,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::Utc;
use serde::Serialize;

use crate::{
    Claim, ConfirmedResearchBrief, CrawlClient, INTAKE_PROMPT, IntakeError, IntakeEvent,
    IntakeEventKind, IntakeLog, IntakeSession, IntakeSessionLocks, IntakeStatus, LiveBackend,
    MAX_TOTAL_QUESTIONS, ModelParseOutcome, ReplayedRunHeader, RunHeader, SearchError,
    SearxngClient, SnapshotReader, SnapshotRef, SnapshotWriter, StrongClient, TracePolicy,
    TraceWriter, cancellation_event, confirmation_event, events_for_model_output,
    minimal_brief_event,
    orchestration::{AnswerSource, ResearchResult, ResearchSession},
    parse_model_attempt, replay_run_header, user_reply_event,
};

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct AppConfig {
    search_base_url: String,
    crawl_base_url: String,
    crawl_token: String,
    model_base_url: String,
    model_api_key: String,
    model: String,
    pub data_dir: PathBuf,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            search_base_url: required_env("SEARCH_BASE_URL")?,
            crawl_base_url: required_env("CRAWL4AI_BASE_URL")?,
            crawl_token: env::var("CRAWL4AI_TOKEN").unwrap_or_default(),
            model_base_url: required_env("STRONG_MODEL_BASE_URL")?,
            model_api_key: required_env("STRONG_MODEL_API_KEY")?,
            model: required_env("STRONG_MODEL_ID")?,
            data_dir: env::var_os("TRACEABLE_SEARCH_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data")),
        })
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    env::var(name).map_err(|_| anyhow::anyhow!("required environment variable {name} is not set"))
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareRunError {
    #[error(transparent)]
    Intake(#[from] IntakeError),
    #[error(transparent)]
    Trace(#[from] SearchError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRun {
    pub run_id: String,
    pub brief: ConfirmedResearchBrief,
    policy: TracePolicy,
}

#[derive(Debug, thiserror::Error)]
pub enum IntakeCommandError {
    #[error(transparent)]
    Intake(#[from] IntakeError),
    #[error(transparent)]
    Prepare(#[from] PrepareRunError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid model output: {0}")]
    ModelOutput(String),
}

#[derive(Clone)]
pub struct ResearchService {
    config: AppConfig,
    intake_locks: IntakeSessionLocks,
}

fn require_failed_intake(
    session: &IntakeSession,
    requested_revision: u32,
) -> Result<(), IntakeError> {
    if session.revision != requested_revision {
        return Err(IntakeError::StaleBrief {
            current_revision: session.revision,
            requested_revision,
        });
    }
    if session.status != IntakeStatus::IntakeFailed {
        return Err(IntakeError::InvalidTransition {
            status: session.status,
            event: "intake_recovery",
        });
    }
    Ok(())
}

impl ResearchService {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            intake_locks: IntakeSessionLocks::default(),
        }
    }

    pub fn new_run_id(&self) -> String {
        format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
            std::process::id(),
            RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
    }

    pub fn trace_path(&self, run_id: &str) -> PathBuf {
        self.config
            .data_dir
            .join("traces")
            .join(format!("{run_id}.jsonl"))
    }

    pub async fn start_intake(&self, question: &str) -> Result<IntakeSession, IntakeCommandError> {
        let clarification_id = self.new_run_id();
        let _guard = self.intake_locks.lock(&clarification_id).await?;
        let started = IntakeEvent::new(IntakeEventKind::IntakeStarted {
            clarification_id,
            original_question: question.to_owned(),
            revision: 0,
            created_at: Utc::now(),
        });
        let mut log = IntakeLog::create(self.config.data_dir.join("intake"), started)?;
        self.advance_intake(&mut log).await?;
        Ok(log.session().clone())
    }

    pub async fn reply_intake(
        &self,
        clarification_id: &str,
        revision: u32,
        answer: &str,
    ) -> Result<IntakeSession, IntakeCommandError> {
        let _guard = self.intake_locks.lock(clarification_id).await?;
        let mut log = IntakeLog::open(self.config.data_dir.join("intake"), clarification_id)?;
        log.append(&user_reply_event(
            log.session(),
            revision,
            answer,
            Utc::now(),
        )?)?;
        self.advance_intake(&mut log).await?;
        Ok(log.session().clone())
    }

    pub async fn retry_intake(
        &self,
        clarification_id: &str,
        revision: u32,
    ) -> Result<IntakeSession, IntakeCommandError> {
        let _guard = self.intake_locks.lock(clarification_id).await?;
        let mut log = IntakeLog::open(self.config.data_dir.join("intake"), clarification_id)?;
        require_failed_intake(log.session(), revision)?;
        self.advance_intake(&mut log).await?;
        Ok(log.session().clone())
    }

    pub async fn use_minimal_brief(
        &self,
        clarification_id: &str,
        revision: u32,
    ) -> Result<IntakeSession, IntakeCommandError> {
        let _guard = self.intake_locks.lock(clarification_id).await?;
        let mut log = IntakeLog::open(self.config.data_dir.join("intake"), clarification_id)?;
        require_failed_intake(log.session(), revision)?;
        log.append(&minimal_brief_event(log.session(), Utc::now())?)?;
        Ok(log.session().clone())
    }

    pub async fn confirm_intake(
        &self,
        clarification_id: &str,
        revision: u32,
        content_hash: &str,
        policy: TracePolicy,
    ) -> Result<PreparedRun, IntakeCommandError> {
        Ok(self
            .prepare_confirmed_run(clarification_id, revision, content_hash, policy)
            .await?)
    }

    pub async fn cancel_intake(
        &self,
        clarification_id: &str,
        revision: u32,
    ) -> Result<IntakeSession, IntakeCommandError> {
        let _guard = self.intake_locks.lock(clarification_id).await?;
        let mut log = IntakeLog::open(self.config.data_dir.join("intake"), clarification_id)?;
        if log.session().revision != revision {
            return Err(IntakeError::StaleBrief {
                current_revision: log.session().revision,
                requested_revision: revision,
            }
            .into());
        }
        log.append(&cancellation_event(log.session(), Utc::now()))?;
        Ok(log.session().clone())
    }

    async fn advance_intake(&self, log: &mut IntakeLog) -> Result<(), IntakeCommandError> {
        let client = StrongClient::new(
            &self.config.model_base_url,
            &self.config.model_api_key,
            &self.config.model,
        )?;
        let base_input = serde_json::to_string(&serde_json::json!({
            "session": log.session(),
            "remaining_questions": MAX_TOTAL_QUESTIONS.saturating_sub(log.session().questions.len()),
        }))?;
        let mut correction = None;
        for attempt in 1..=2 {
            let user = match correction.take() {
                Some(error) => format!(
                    "{base_input}\nYour previous JSON was invalid: {error}. Return one corrected JSON object only."
                ),
                None => base_input.clone(),
            };
            let value: serde_json::Value = match client.complete_json(INTAKE_PROMPT, &user).await {
                Ok(value) => value,
                Err(error) => {
                    let event = IntakeEvent::new(IntakeEventKind::IntakeFailed {
                        revision: log.session().revision,
                        message: error.to_string(),
                        failed_at: Utc::now(),
                    });
                    log.append(&event)?;
                    return Ok(());
                }
            };
            match parse_model_attempt(log.session(), &value.to_string(), attempt, Utc::now())? {
                ModelParseOutcome::Accepted(output) => {
                    for event in events_for_model_output(log.session(), output, Utc::now())? {
                        log.append(&event)?;
                    }
                    return Ok(());
                }
                ModelParseOutcome::RetryCorrection { error } => correction = Some(error),
                ModelParseOutcome::Failed(event) => {
                    log.append(&event)?;
                    return Err(IntakeCommandError::ModelOutput(
                        log.session()
                            .failure
                            .clone()
                            .unwrap_or_else(|| "invalid model output".into()),
                    ));
                }
            }
        }
        unreachable!("two model attempts always return")
    }

    /// Freezes one intake exactly once, then creates its v3 trace header.
    /// Repeating confirmation reuses the persisted run id; a crash after the
    /// intake append but before trace creation is repaired on the next call.
    pub async fn prepare_confirmed_run(
        &self,
        clarification_id: &str,
        requested_revision: u32,
        requested_content_hash: &str,
        policy: TracePolicy,
    ) -> Result<PreparedRun, PrepareRunError> {
        let _guard = self.intake_locks.lock(clarification_id).await?;
        let mut log = IntakeLog::open(self.config.data_dir.join("intake"), clarification_id)?;

        let confirmation = if log.session().status == IntakeStatus::Confirmed {
            if requested_revision != log.session().revision {
                return Err(IntakeError::StaleBrief {
                    current_revision: log.session().revision,
                    requested_revision,
                }
                .into());
            }
            let confirmation = log
                .session()
                .confirmation
                .clone()
                .expect("confirmed intake must retain confirmation");
            if requested_content_hash != confirmation.brief.content_hash() {
                return Err(IntakeError::StaleContentHash {
                    current_hash: confirmation.brief.content_hash().to_owned(),
                    requested_hash: requested_content_hash.to_owned(),
                }
                .into());
            }
            confirmation
        } else {
            let event = confirmation_event(
                log.session(),
                requested_revision,
                requested_content_hash,
                self.new_run_id(),
                Utc::now(),
            )?;
            log.append(&event)?;
            log.session()
                .confirmation
                .clone()
                .expect("appended confirmed event must project confirmation")
        };

        let header = RunHeader {
            run_id: confirmation.run_id.clone(),
            clarification_id: clarification_id.to_owned(),
            brief: confirmation.brief.clone(),
            started_at: *confirmation.brief.confirmed_at(),
            policy: policy.clone(),
        };
        let trace_dir = self.config.data_dir.join("traces");
        match TraceWriter::create(&trace_dir, header.clone()) {
            Ok(writer) => drop(writer),
            Err(SearchError::Trace(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match replay_run_header(self.trace_path(&header.run_id))? {
                    ReplayedRunHeader::V3(existing) if existing.as_ref() == &header => {}
                    _ => {
                        return Err(SearchError::Trace(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "existing trace header does not match confirmed intake",
                        ))
                        .into());
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }

        Ok(PreparedRun {
            run_id: confirmation.run_id,
            brief: confirmation.brief,
            policy,
        })
    }

    pub async fn run(&self, prepared: PreparedRun) -> Result<PublicAnswer, SearchError> {
        let store_path = self.config.data_dir.join("snapshots.sqlite");
        let backend = LiveBackend::new(
            SearxngClient::new(&self.config.search_base_url).map_err(setup_error)?,
            CrawlClient::new(&self.config.crawl_base_url, self.config.crawl_token.clone())
                .map_err(setup_error)?,
            StrongClient::new(
                &self.config.model_base_url,
                self.config.model_api_key.clone(),
                self.config.model.clone(),
            )
            .map_err(setup_error)?,
        );
        let snapshots = SnapshotWriter::open(&store_path).map_err(setup_error)?;
        let header = RunHeader {
            run_id: prepared.run_id,
            clarification_id: prepared.brief.clarification_id().to_owned(),
            started_at: *prepared.brief.confirmed_at(),
            brief: prepared.brief,
            policy: prepared.policy,
        };
        let (trace, replay) = TraceWriter::resume(self.config.data_dir.join("traces"), &header)
            .map_err(setup_error)?;
        let reader = SnapshotReader::open(&store_path).map_err(setup_error)?;
        let rounds = header.policy.rounds;
        let mut session = ResearchSession::resume(
            header.brief,
            rounds,
            backend,
            snapshots,
            trace,
            replay,
            &reader,
        )
        .map_err(setup_error)?;
        public_answer(session.run(store_path).await?)
    }
}

fn setup_error(error: impl std::fmt::Display) -> SearchError {
    SearchError::Setup {
        message: error.to_string(),
    }
}

#[derive(Debug, Serialize)]
pub struct PublicAnswer {
    pub answer: String,
    pub claims: Vec<PublicClaim>,
}
#[derive(Debug, Serialize)]
pub struct PublicClaim {
    pub text: String,
    pub sources: Vec<PublicSource>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicSource {
    pub url: String,
    pub title: String,
}

fn public_answer(result: ResearchResult) -> Result<PublicAnswer, SearchError> {
    let sources: HashMap<SnapshotRef, PublicSource> = result
        .sources
        .into_iter()
        .map(|source: AnswerSource| {
            (
                source.snapshot_ref,
                PublicSource {
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
        .map(
            |Claim {
                 text,
                 snapshot_refs,
             }| {
                let sources = snapshot_refs
                    .into_iter()
                    .map(|reference| {
                        sources.get(&reference).cloned().ok_or_else(|| {
                            SearchError::InvalidSnapshot(format!(
                                "cited snapshot missing source metadata: {}",
                                reference.as_str()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(PublicClaim { text, sources })
            },
        )
        .collect::<Result<Vec<_>, SearchError>>()?;
    Ok(PublicAnswer {
        answer: result.answer.answer,
        claims,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;
    use crate::{
        Answer, IntakeEvent, IntakeEventKind, SnapshotRef, minimal_brief_event, replay_intake,
    };

    fn test_service(name: &str) -> ResearchService {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        ResearchService::new(AppConfig {
            search_base_url: "http://127.0.0.1:1".into(),
            crawl_base_url: "http://127.0.0.1:1".into(),
            crawl_token: String::new(),
            model_base_url: "http://127.0.0.1:1".into(),
            model_api_key: String::new(),
            model: "test".into(),
            data_dir: std::env::temp_dir().join(format!(
                "traceable-search-app-{name}-{}-{unique}",
                std::process::id()
            )),
        })
    }

    fn ready_intake(service: &ResearchService, clarification_id: &str) -> (u32, String) {
        let started = IntakeEvent::new(IntakeEventKind::IntakeStarted {
            clarification_id: clarification_id.into(),
            original_question: "Original question, byte for byte?".into(),
            revision: 0,
            created_at: Utc::now(),
        });
        let mut log = IntakeLog::create(service.config.data_dir.join("intake"), started).unwrap();
        let revised = minimal_brief_event(log.session(), Utc::now()).unwrap();
        log.append(&revised).unwrap();
        (
            log.session().revision,
            log.session().content_hash.clone().unwrap(),
        )
    }

    /// Drives an intake into INTAKE_FAILED and returns (current_revision, a
    /// valid recovery brief). IntakeFailed does not bump the revision, so the
    /// returned revision is still 0.
    fn failed_intake(service: &ResearchService, clarification_id: &str) -> u32 {
        let started = IntakeEvent::new(IntakeEventKind::IntakeStarted {
            clarification_id: clarification_id.into(),
            original_question: "Original question, byte for byte?".into(),
            revision: 0,
            created_at: Utc::now(),
        });
        let mut log = IntakeLog::create(service.config.data_dir.join("intake"), started).unwrap();
        log.append(&IntakeEvent::new(IntakeEventKind::IntakeFailed {
            revision: 0,
            message: "model returned invalid structured output twice".into(),
            failed_at: Utc::now(),
        }))
        .unwrap();
        assert_eq!(log.session().status, IntakeStatus::IntakeFailed);
        log.session().revision
    }

    #[tokio::test]
    async fn failed_intake_recovers_with_server_generated_minimal_brief() {
        let service = test_service("intake-failed-recovery");
        let clarification_id = "clarification-failed-recovery";
        let revision = failed_intake(&service, clarification_id);

        let session = service
            .use_minimal_brief(clarification_id, revision)
            .await
            .unwrap();

        assert_eq!(session.status, IntakeStatus::ReadyToConfirm);
        assert_eq!(session.revision, revision + 1);
        assert!(session.failure.is_none());
        assert!(session.content_hash.is_some());

        let replayed =
            replay_intake(service.config.data_dir.join("intake"), clarification_id).unwrap();
        assert_eq!(replayed.status, IntakeStatus::ReadyToConfirm);
        fs::remove_dir_all(&service.config.data_dir).unwrap();
    }

    #[tokio::test]
    async fn failed_intake_recovery_rejects_stale_revision() {
        let service = test_service("intake-failed-stale");
        let clarification_id = "clarification-failed-stale";
        let revision = failed_intake(&service, clarification_id);

        let error = service
            .use_minimal_brief(clarification_id, revision + 7)
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                IntakeCommandError::Intake(IntakeError::StaleBrief { .. })
            ),
            "expected StaleBrief, got {error:?}"
        );
        fs::remove_dir_all(&service.config.data_dir).unwrap();
    }

    fn test_policy() -> TracePolicy {
        TracePolicy {
            rounds: 3,
            input_budget: 1_000,
            max_snapshots: 10,
        }
    }

    #[tokio::test]
    async fn repeated_confirmation_reuses_one_run_id() {
        let service = test_service("confirm-idempotent");
        let (revision, content_hash) = ready_intake(&service, "clarification-idempotent");

        let first = service
            .prepare_confirmed_run(
                "clarification-idempotent",
                revision,
                &content_hash,
                test_policy(),
            )
            .await
            .unwrap();
        let second = service
            .prepare_confirmed_run(
                "clarification-idempotent",
                revision,
                &content_hash,
                test_policy(),
            )
            .await
            .unwrap();

        assert_eq!(second, first);
        let replayed = replay_intake(
            service.config.data_dir.join("intake"),
            "clarification-idempotent",
        )
        .unwrap();
        assert_eq!(replayed.confirmation.unwrap().run_id, first.run_id);
        assert!(matches!(
            replay_run_header(service.trace_path(&first.run_id)).unwrap(),
            ReplayedRunHeader::V3(header)
                if header.run_id == first.run_id && header.brief == first.brief
        ));
        fs::remove_dir_all(&service.config.data_dir).unwrap();
    }

    #[tokio::test]
    async fn confirmation_without_trace_is_repaired_with_same_run_id() {
        let service = test_service("confirm-crash-window");
        let clarification_id = "clarification-crash-window";
        let (revision, content_hash) = ready_intake(&service, clarification_id);
        let mut log =
            IntakeLog::open(service.config.data_dir.join("intake"), clarification_id).unwrap();
        let confirmed = confirmation_event(
            log.session(),
            revision,
            &content_hash,
            "run-before-crash".into(),
            Utc::now(),
        )
        .unwrap();
        log.append(&confirmed).unwrap();
        drop(log);
        assert!(!service.trace_path("run-before-crash").exists());

        let repaired = service
            .prepare_confirmed_run(clarification_id, revision, &content_hash, test_policy())
            .await
            .unwrap();

        assert_eq!(repaired.run_id, "run-before-crash");
        assert!(matches!(
            replay_run_header(service.trace_path(&repaired.run_id)).unwrap(),
            ReplayedRunHeader::V3(header)
                if header.run_id == repaired.run_id && header.brief == repaired.brief
        ));
        fs::remove_dir_all(&service.config.data_dir).unwrap();
    }

    #[test]
    fn public_answer_hides_snapshot_refs() {
        let reference = SnapshotRef::from_id("abc123");
        let value = serde_json::to_value(
            public_answer(ResearchResult {
                answer: Answer {
                    answer: "Grounded".into(),
                    claims: vec![Claim {
                        text: "Fact".into(),
                        snapshot_refs: vec![reference.clone()],
                    }],
                },
                sources: vec![AnswerSource {
                    snapshot_ref: reference,
                    url: "https://example.com/final".into(),
                    title: "Example".into(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["claims"][0]["sources"][0]["title"], "Example");
        assert!(!value.to_string().contains("snapshot_ref"));
    }
}

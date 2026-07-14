use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

use crate::{
    ClarificationQuestion, IntakeError, IntakeSession, IntakeStatus, PublicAnswer, PublicError,
    ResearchBrief, ResearchService, TracePolicy,
    app::{IntakeCommandError, PrepareRunError},
};

const INDEX_HTML: &str = include_str!("web/index.html");
const DEFAULT_ROUNDS: u32 = 3;
const MIN_ROUNDS: u32 = 3;
const MAX_ROUNDS: u32 = 5;
const DEFAULT_INPUT_BUDGET: u32 = crate::orchestration::MAX_STRONG_INPUT_TOKENS as u32;
const DEFAULT_MAX_SNAPSHOTS: u32 = crate::orchestration::MAX_SNAPSHOTS as u32;

const fn default_rounds() -> u32 {
    DEFAULT_ROUNDS
}

fn validate_rounds(rounds: u32) -> Result<(), &'static str> {
    if (MIN_ROUNDS..=MAX_ROUNDS).contains(&rounds) {
        Ok(())
    } else {
        Err("rounds must be between 3 and 5")
    }
}

#[derive(Clone)]
pub struct WebState {
    service: ResearchService,
    job: Arc<RwLock<Option<Job>>>,
}

#[derive(Debug)]
struct Job {
    run_id: String,
    status: JobStatus,
}

#[derive(Debug)]
enum JobStatus {
    Running,
    Completed(PublicAnswer),
    Failed(PublicError),
}

#[derive(Serialize)]
struct StatusResponse<'a> {
    run_id: &'a str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a PublicAnswer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a PublicError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartIntakeRequest {
    question: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyIntakeRequest {
    revision: u32,
    answer: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmIntakeRequest {
    revision: u32,
    content_hash: String,
    #[serde(default = "default_rounds")]
    rounds: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionIntakeRequest {
    revision: u32,
}

#[derive(Serialize)]
struct IntakeMessage<'a> {
    role: &'static str,
    kind: &'static str,
    text: &'a str,
}

fn intake_messages(session: &IntakeSession) -> Vec<IntakeMessage<'_>> {
    let mut messages = vec![IntakeMessage {
        role: "user",
        kind: "original_question",
        text: &session.original_question,
    }];
    for question in &session.questions {
        messages.push(IntakeMessage {
            role: "assistant",
            kind: "clarification",
            text: &question.question,
        });
        if let Some(answer) = session
            .answers
            .iter()
            .find(|answer| answer.question_id == question.id)
        {
            messages.push(IntakeMessage {
                role: "user",
                kind: "answer",
                text: &answer.answer,
            });
        }
    }
    messages
}

#[derive(Serialize)]
struct IntakeResponse<'a> {
    clarification_id: &'a str,
    original_question: &'a str,
    revision: u32,
    status: IntakeStatus,
    brief_draft: Option<&'a ResearchBrief>,
    question: Option<&'a ClarificationQuestion>,
    questions_asked: usize,
    messages: Vec<IntakeMessage<'a>>,
    content_hash: Option<&'a str>,
    failure: Option<&'a str>,
}

impl<'a> From<&'a IntakeSession> for IntakeResponse<'a> {
    fn from(session: &'a IntakeSession) -> Self {
        Self {
            clarification_id: &session.clarification_id,
            original_question: &session.original_question,
            revision: session.revision,
            status: session.status,
            brief_draft: session.brief_draft.as_ref(),
            question: session.pending_questions().into_iter().next(),
            questions_asked: session.questions.len(),
            messages: intake_messages(session),
            content_hash: session.content_hash.as_deref(),
            failure: session.failure.as_deref(),
        }
    }
}

fn intake_response(status: StatusCode, session: &IntakeSession) -> Response {
    (status, Json(IntakeResponse::from(session))).into_response()
}

#[derive(Serialize)]
struct ConfirmResponse {
    run_id: String,
}

#[derive(Serialize)]
struct ApiError {
    error: &'static str,
    message: String,
}

impl WebState {
    pub fn new(service: ResearchService) -> Self {
        Self {
            service,
            job: Arc::new(RwLock::new(None)),
        }
    }
}

pub fn router(service: ResearchService) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/research/intakes", post(start_intake))
        .route(
            "/api/research/intakes/{clarification_id}/reply",
            post(reply_intake),
        )
        .route(
            "/api/research/intakes/{clarification_id}/retry",
            post(retry_intake),
        )
        .route(
            "/api/research/intakes/{clarification_id}/minimal-brief",
            post(use_minimal_brief),
        )
        .route(
            "/api/research/intakes/{clarification_id}/confirm",
            post(confirm_intake),
        )
        .route(
            "/api/research/intakes/{clarification_id}/cancel",
            post(cancel_intake),
        )
        .route("/api/research/{run_id}", get(research_status))
        .route("/api/research/{run_id}/events", get(research_events))
        .with_state(WebState::new(service))
}

async fn start_intake(
    State(state): State<WebState>,
    Json(request): Json<StartIntakeRequest>,
) -> Response {
    match state.service.start_intake(&request.question).await {
        Ok(session) => intake_response(StatusCode::CREATED, &session),
        Err(error) => intake_error_response(&error),
    }
}

async fn reply_intake(
    State(state): State<WebState>,
    Path(clarification_id): Path<String>,
    Json(request): Json<ReplyIntakeRequest>,
) -> Response {
    match state
        .service
        .reply_intake(&clarification_id, request.revision, &request.answer)
        .await
    {
        Ok(session) => intake_response(StatusCode::OK, &session),
        Err(error) => intake_error_response(&error),
    }
}

async fn retry_intake(
    State(state): State<WebState>,
    Path(clarification_id): Path<String>,
    Json(request): Json<RevisionIntakeRequest>,
) -> Response {
    match state
        .service
        .retry_intake(&clarification_id, request.revision)
        .await
    {
        Ok(session) => intake_response(StatusCode::OK, &session),
        Err(error) => intake_error_response(&error),
    }
}

async fn use_minimal_brief(
    State(state): State<WebState>,
    Path(clarification_id): Path<String>,
    Json(request): Json<RevisionIntakeRequest>,
) -> Response {
    match state
        .service
        .use_minimal_brief(&clarification_id, request.revision)
        .await
    {
        Ok(session) => intake_response(StatusCode::OK, &session),
        Err(error) => intake_error_response(&error),
    }
}

async fn confirm_intake(
    State(state): State<WebState>,
    Path(clarification_id): Path<String>,
    Json(request): Json<ConfirmIntakeRequest>,
) -> Response {
    if let Err(message) = validate_rounds(request.rounds) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_request", message);
    }
    let policy = TracePolicy {
        rounds: request.rounds,
        input_budget: DEFAULT_INPUT_BUDGET,
        max_snapshots: DEFAULT_MAX_SNAPSHOTS,
    };
    let prepared = match state
        .service
        .confirm_intake(
            &clarification_id,
            request.revision,
            &request.content_hash,
            policy,
        )
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => return intake_error_response(&error),
    };
    let run_id = prepared.run_id.clone();
    {
        let mut job = state.job.write().await;
        if matches!(
            job.as_ref().map(|job| &job.status),
            Some(JobStatus::Running)
        ) {
            return api_error(
                StatusCode::CONFLICT,
                "research_running",
                "a research run is already active",
            );
        }
        *job = Some(Job {
            run_id: run_id.clone(),
            status: JobStatus::Running,
        });
    }
    let task_state = state.clone();
    let task_run_id = run_id.clone();
    tokio::spawn(async move {
        let status = match task_state.service.run(prepared).await {
            Ok(answer) => JobStatus::Completed(answer),
            Err(error) => JobStatus::Failed(PublicError::from(&error)),
        };
        let mut job = task_state.job.write().await;
        if let Some(job) = job.as_mut().filter(|job| job.run_id == task_run_id) {
            job.status = status;
        }
    });
    (StatusCode::ACCEPTED, Json(ConfirmResponse { run_id })).into_response()
}

async fn cancel_intake(
    State(state): State<WebState>,
    Path(clarification_id): Path<String>,
    Json(request): Json<RevisionIntakeRequest>,
) -> Response {
    match state
        .service
        .cancel_intake(&clarification_id, request.revision)
        .await
    {
        Ok(session) => intake_response(StatusCode::OK, &session),
        Err(error) => intake_error_response(&error),
    }
}

fn intake_error_response(error: &IntakeCommandError) -> Response {
    let (status, code) = match error {
        IntakeCommandError::Intake(
            IntakeError::StaleBrief { .. } | IntakeError::StaleContentHash { .. },
        )
        | IntakeCommandError::Prepare(PrepareRunError::Intake(
            IntakeError::StaleBrief { .. } | IntakeError::StaleContentHash { .. },
        )) => (StatusCode::CONFLICT, "stale_brief"),
        IntakeCommandError::Intake(IntakeError::InvalidTransition { .. })
        | IntakeCommandError::Prepare(PrepareRunError::Intake(IntakeError::InvalidTransition {
            ..
        })) => (StatusCode::CONFLICT, "invalid_transition"),
        IntakeCommandError::Intake(IntakeError::Io(source))
        | IntakeCommandError::Prepare(PrepareRunError::Intake(IntakeError::Io(source)))
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            (StatusCode::NOT_FOUND, "intake_not_found")
        }
        IntakeCommandError::Intake(
            IntakeError::InvalidEvent(_)
            | IntakeError::InvalidClarificationId
            | IntakeError::Brief(_),
        )
        | IntakeCommandError::Prepare(PrepareRunError::Intake(
            IntakeError::InvalidEvent(_)
            | IntakeError::InvalidClarificationId
            | IntakeError::Brief(_),
        )) => (StatusCode::BAD_REQUEST, "invalid_request"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "intake_failed"),
    };
    api_error(status, code, error.to_string())
}

fn api_error(status: StatusCode, error: &'static str, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            error,
            message: message.into(),
        }),
    )
        .into_response()
}

async fn research_status(State(state): State<WebState>, Path(run_id): Path<String>) -> Response {
    let job = state.job.read().await;
    let Some(job) = job.as_ref().filter(|job| job.run_id == run_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let response = match &job.status {
        JobStatus::Running => StatusResponse {
            run_id: &job.run_id,
            status: "running",
            result: None,
            error: None,
        },
        JobStatus::Completed(answer) => StatusResponse {
            run_id: &job.run_id,
            status: "completed",
            result: Some(answer),
            error: None,
        },
        JobStatus::Failed(error) => StatusResponse {
            run_id: &job.run_id,
            status: "failed",
            result: None,
            error: Some(error),
        },
    };
    Json(response).into_response()
}

async fn research_events(State(state): State<WebState>, Path(run_id): Path<String>) -> Response {
    {
        let job = state.job.read().await;
        if job.as_ref().is_none_or(|job| job.run_id != run_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    let trace_path = state.service.trace_path(&run_id);
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    tokio::spawn(async move {
        let mut sent = 0usize;
        loop {
            if let Ok(content) = tokio::fs::read_to_string(&trace_path).await {
                let lines: Vec<_> = content.lines().collect();
                for line in &lines[sent..] {
                    if sender
                        .send(Ok::<_, Infallible>(
                            Event::default().event("trace").data(*line),
                        ))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                sent = lines.len();
            }
            let terminal = {
                let job = state.job.read().await;
                job.as_ref()
                    .filter(|job| job.run_id == run_id)
                    .is_none_or(|job| !matches!(job.status, JobStatus::Running))
            };
            if terminal {
                let _ = sender
                    .send(Ok(Event::default()
                        .event("done")
                        .data(format!(r#"{{"run_id":"{run_id}"}}"#))))
                    .await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
    Sse::new(ReceiverStream::new(receiver).map(|event| event))
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::SystemTime};

    use chrono::Utc;

    use super::*;
    use crate::{AppConfig, IntakeEvent, IntakeEventKind, IntakeLog, minimal_brief_event};

    fn test_state(name: &str) -> (WebState, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data_dir = std::env::temp_dir().join(format!(
            "traceable-search-web-{name}-{}-{unique}",
            std::process::id()
        ));
        let service = ResearchService::new(AppConfig::for_test(data_dir.clone()));
        (WebState::new(service), data_dir)
    }

    fn ready_intake(data_dir: &std::path::Path, clarification_id: &str) -> (u32, String) {
        let started = IntakeEvent::new(IntakeEventKind::IntakeStarted {
            clarification_id: clarification_id.into(),
            original_question: "What changed in Rust 2024?".into(),
            revision: 0,
            created_at: Utc::now(),
        });
        let mut log = IntakeLog::create(data_dir.join("intake"), started).unwrap();
        let revised = minimal_brief_event(log.session(), Utc::now()).unwrap();
        log.append(&revised).unwrap();
        (
            log.session().revision,
            log.session().content_hash.clone().unwrap(),
        )
    }

    #[test]
    fn rounds_contract_has_one_default_and_inclusive_bounds() {
        let request: ConfirmIntakeRequest = serde_json::from_value(serde_json::json!({
            "revision": 1,
            "content_hash": "hash"
        }))
        .unwrap();
        assert_eq!(request.rounds, DEFAULT_ROUNDS);
        assert!(validate_rounds(2).is_err());
        assert!(validate_rounds(3).is_ok());
        assert!(validate_rounds(5).is_ok());
        assert!(validate_rounds(6).is_err());
        assert_eq!(DEFAULT_INPUT_BUDGET, 1_000_000);
        assert_eq!(DEFAULT_MAX_SNAPSHOTS, 300);
        assert!(INDEX_HTML.contains("min=\"3\" max=\"5\" value=\"3\""));
    }

    #[tokio::test]
    async fn intake_endpoints_return_contract_statuses() {
        let (state, data_dir) = test_state("statuses");

        for rounds in [2, 6] {
            let bad_rounds = confirm_intake(
                State(state.clone()),
                Path("missing".into()),
                Json(ConfirmIntakeRequest {
                    revision: 0,
                    content_hash: "hash".into(),
                    rounds,
                }),
            )
            .await;
            assert_eq!(bad_rounds.status(), StatusCode::BAD_REQUEST);
        }

        let bad = start_intake(
            State(state.clone()),
            Json(StartIntakeRequest {
                question: "  ".into(),
            }),
        )
        .await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        let missing = reply_intake(
            State(state.clone()),
            Path("missing".into()),
            Json(ReplyIntakeRequest {
                revision: 0,
                answer: "answer".into(),
            }),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let created = start_intake(
            State(state.clone()),
            Json(StartIntakeRequest {
                question: "What changed in Rust 2024?".into(),
            }),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(created.into_body(), usize::MAX)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let keys = response
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "brief_draft",
                "clarification_id",
                "content_hash",
                "failure",
                "messages",
                "original_question",
                "question",
                "questions_asked",
                "revision",
                "status",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            response["messages"],
            serde_json::json!([{
                "role": "user",
                "kind": "original_question",
                "text": "What changed in Rust 2024?"
            }])
        );

        let clarification_id = "ready-for-confirm";
        let (revision, content_hash) = ready_intake(&data_dir, clarification_id);
        let stale = confirm_intake(
            State(state.clone()),
            Path(clarification_id.into()),
            Json(ConfirmIntakeRequest {
                revision,
                content_hash: "stale".into(),
                rounds: 3,
            }),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);

        let accepted = confirm_intake(
            State(state.clone()),
            Path(clarification_id.into()),
            Json(ConfirmIntakeRequest {
                revision,
                content_hash,
                rounds: 3,
            }),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);

        for _ in 0..100 {
            if !matches!(
                state.job.read().await.as_ref().map(|job| &job.status),
                Some(JobStatus::Running)
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!matches!(
            state.job.read().await.as_ref().map(|job| &job.status),
            Some(JobStatus::Running)
        ));
        fs::remove_dir_all(data_dir).unwrap();
    }
}

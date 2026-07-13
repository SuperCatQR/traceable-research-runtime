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
    PublicAnswer, PublicError, ResearchService,
    orchestration::{DEFAULT_EXPLORE_ROUNDS, MAX_EXPLORE_ROUNDS, MIN_EXPLORE_ROUNDS},
};

const INDEX_HTML: &str = include_str!("web/index.html");

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

#[derive(Deserialize)]
struct CreateRequest {
    question: String,
    #[serde(default = "default_rounds")]
    rounds: u32,
}

const fn default_rounds() -> u32 {
    DEFAULT_EXPLORE_ROUNDS
}

#[derive(Serialize)]
struct CreateResponse {
    run_id: String,
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
        .route("/api/research", post(create_research))
        .route("/api/research/{run_id}", get(research_status))
        .route("/api/research/{run_id}/events", get(research_events))
        .with_state(WebState::new(service))
}

async fn create_research(
    State(state): State<WebState>,
    Json(request): Json<CreateRequest>,
) -> Response {
    let question = request.question.trim().to_owned();
    if question.is_empty() {
        return (StatusCode::BAD_REQUEST, "question must not be empty").into_response();
    }
    if !(MIN_EXPLORE_ROUNDS..=MAX_EXPLORE_ROUNDS).contains(&request.rounds) {
        return (StatusCode::BAD_REQUEST, "rounds must be between 3 and 5").into_response();
    }
    let rounds = request.rounds;
    let mut job = state.job.write().await;
    if job
        .as_ref()
        .is_some_and(|job| matches!(job.status, JobStatus::Running))
    {
        return (StatusCode::CONFLICT, "a research job is already running").into_response();
    }
    let run_id = state.service.new_run_id();
    *job = Some(Job {
        run_id: run_id.clone(),
        status: JobStatus::Running,
    });
    drop(job);

    let background = state.clone();
    let background_run_id = run_id.clone();
    tokio::spawn(async move {
        let result = background
            .service
            .run(&question, rounds, background_run_id.clone())
            .await;
        let mut job = background.job.write().await;
        if let Some(current) = job.as_mut().filter(|job| job.run_id == background_run_id) {
            current.status = match result {
                Ok(answer) => JobStatus::Completed(answer),
                Err(error) => JobStatus::Failed(PublicError::from(&error)),
            };
        }
    });

    (StatusCode::ACCEPTED, Json(CreateResponse { run_id })).into_response()
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

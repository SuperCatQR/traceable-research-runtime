//! OpenAI-compatible HTTP Adapter for the closed Markdown research model Seam.
//!
//! The Adapter owns endpoint/auth/retry details and returns only the typed
//! responses accepted by `model_gateway`. It never writes prompts, API keys or
//! full model bodies to logs or errors.

use crate::error::{Result, RuntimeError};
use crate::model_gateway::{
    MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES, MarkdownResearchModelGateway,
    StrongMarkdownResearchModelResponse, StrongMarkdownResearchModelTask,
    VerbatimSourceEvidenceCandidateSet, VerbatimSourceEvidenceExtractionTask,
};
use async_trait::async_trait;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Configuration for an OpenAI-compatible Chat Completions endpoint.
#[derive(Clone)]
pub struct OpenAiCompatibleMarkdownResearchModelGatewayConfig {
    /// Base endpoint, for example `https://example.test/v1/` or a complete
    /// `/chat/completions` URL.
    pub endpoint: String,
    /// Bearer token. It is held in memory only and redacted from `Debug`.
    pub api_key: String,
    /// Model used for strong tasks.
    pub strong_model: String,
    /// Model used for cheap verbatim extraction tasks.
    pub cheap_model: String,
    /// Per-attempt HTTP timeout.
    pub request_timeout: Duration,
    /// Total attempts, including the first request. 429 and 5xx are retried.
    pub max_attempts: u32,
    /// Prompt/schema version recorded by the host configuration.
    pub prompt_schema_version: u32,
}

impl Default for OpenAiCompatibleMarkdownResearchModelGatewayConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8000/v1/".to_owned(),
            api_key: String::new(),
            strong_model: "strong".to_owned(),
            cheap_model: "cheap".to_owned(),
            request_timeout: Duration::from_secs(60),
            max_attempts: 3,
            prompt_schema_version: 1,
        }
    }
}

impl fmt::Debug for OpenAiCompatibleMarkdownResearchModelGatewayConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleMarkdownResearchModelGatewayConfig")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .field("strong_model", &self.strong_model)
            .field("cheap_model", &self.cheap_model)
            .field("request_timeout", &self.request_timeout)
            .field("max_attempts", &self.max_attempts)
            .field("prompt_schema_version", &self.prompt_schema_version)
            .finish()
    }
}

/// Production HTTP Adapter satisfying the model Gateway Interface.
pub struct OpenAiCompatibleMarkdownResearchModelGateway {
    client: Client,
    endpoint: Url,
    api_key: String,
    strong_model: String,
    cheap_model: String,
    max_attempts: u32,
    prompt_schema_version: u32,
}

impl fmt::Debug for OpenAiCompatibleMarkdownResearchModelGateway {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleMarkdownResearchModelGateway")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .field("strong_model", &self.strong_model)
            .field("cheap_model", &self.cheap_model)
            .field("max_attempts", &self.max_attempts)
            .field("prompt_schema_version", &self.prompt_schema_version)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleMarkdownResearchModelGateway {
    /// Builds an Adapter and validates endpoint/retry configuration.
    pub fn new(config: OpenAiCompatibleMarkdownResearchModelGatewayConfig) -> Result<Self> {
        if config.max_attempts == 0 || config.max_attempts > 8 {
            return Err(RuntimeError::validation(
                crate::error::RuntimeStage::Model,
                "model max_attempts must be between 1 and 8",
            ));
        }
        if config.prompt_schema_version == 0 {
            return Err(RuntimeError::validation(
                crate::error::RuntimeStage::Model,
                "prompt schema version must be positive",
            ));
        }
        if config.request_timeout.is_zero() || config.request_timeout > Duration::from_secs(600) {
            return Err(RuntimeError::validation(
                crate::error::RuntimeStage::Model,
                "model request_timeout must be between 1ms and 600s",
            ));
        }
        let mut endpoint = Url::parse(&config.endpoint).map_err(|_| {
            RuntimeError::validation(
                crate::error::RuntimeStage::Model,
                "model endpoint is not a valid URL",
            )
        })?;
        if !endpoint.path().ends_with("/chat/completions") {
            let path = endpoint.path().trim_end_matches('/').to_owned() + "/chat/completions";
            endpoint.set_path(&path);
        }
        let client = Client::builder().timeout(config.request_timeout).build().map_err(|_| {
            RuntimeError::Internal { message: "cannot construct model HTTP client".to_owned() }
        })?;
        Ok(Self {
            client,
            endpoint,
            api_key: config.api_key,
            strong_model: config.strong_model,
            cheap_model: config.cheap_model,
            max_attempts: config.max_attempts,
            prompt_schema_version: config.prompt_schema_version,
        })
    }

    async fn complete_json(
        &self,
        model: &str,
        task_kind: &str,
        task_json: String,
    ) -> Result<String> {
        let body = ChatCompletionRequest {
            model,
            messages: [
                ChatMessage {
                    role: "system",
                    content: system_instruction(task_kind, self.prompt_schema_version),
                },
                ChatMessage { role: "user", content: task_json },
            ],
            response_format: ResponseFormat { kind: "json_object" },
        };
        let mut attempt = 0_u32;
        'attempts: loop {
            attempt += 1;
            let mut request = self.client.post(self.endpoint.clone()).json(&body);
            if !self.api_key.is_empty() {
                request = request.bearer_auth(&self.api_key);
            }
            let response = request.send().await;
            match response {
                Ok(mut response) => {
                    let status = response.status();
                    if is_retryable_status(status) && attempt < self.max_attempts {
                        tokio::time::sleep(retry_delay(attempt)).await;
                        continue;
                    }
                    if !status.is_success() {
                        return Err(RuntimeError::ModelTransport {
                            message: format!("model endpoint returned HTTP {status}"),
                            retryable: is_retryable_status(status),
                        });
                    }
                    if response.content_length().is_some_and(|length| {
                        length > MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES as u64
                    }) {
                        return Err(RuntimeError::ModelResponse {
                            message: "model endpoint response exceeds the configured size limit"
                                .to_owned(),
                        });
                    }
                    let initial_capacity = response
                        .content_length()
                        .and_then(|length| usize::try_from(length).ok())
                        .unwrap_or_default()
                        .min(MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES);
                    let mut body = Vec::with_capacity(initial_capacity);
                    loop {
                        match response.chunk().await {
                            Ok(Some(chunk)) => {
                                if body.len().saturating_add(chunk.len())
                                    > MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES
                                {
                                    return Err(RuntimeError::ModelResponse {
                                        message: "model endpoint response exceeds the configured size limit"
                                            .to_owned(),
                                    });
                                }
                                body.extend_from_slice(&chunk);
                            }
                            Ok(None) => break,
                            Err(error) => {
                                if is_retryable_transport_error(&error)
                                    && attempt < self.max_attempts
                                {
                                    tokio::time::sleep(retry_delay(attempt)).await;
                                    continue 'attempts;
                                }
                                return Err(model_transport_error(&error));
                            }
                        }
                    }
                    let envelope: ChatCompletionResponse =
                        serde_json::from_slice(&body).map_err(|_| RuntimeError::ModelResponse {
                            message: "model endpoint returned an invalid JSON envelope".to_owned(),
                        })?;
                    let content = envelope
                        .choices
                        .into_iter()
                        .next()
                        .and_then(|choice| choice.message.content)
                        .filter(|content| !content.trim().is_empty())
                        .ok_or_else(|| RuntimeError::ModelResponse {
                            message: "model endpoint returned no JSON content".to_owned(),
                        })?;
                    return Ok(content);
                }
                Err(error) => {
                    if is_retryable_transport_error(&error) && attempt < self.max_attempts {
                        tokio::time::sleep(retry_delay(attempt)).await;
                        continue;
                    }
                    return Err(model_transport_error(&error));
                }
            }
        }
    }
}

#[async_trait]
impl MarkdownResearchModelGateway for OpenAiCompatibleMarkdownResearchModelGateway {
    async fn execute_strong_markdown_research_task(
        &self,
        task: StrongMarkdownResearchModelTask,
    ) -> Result<StrongMarkdownResearchModelResponse> {
        task.validate()?;
        let task_kind = task.kind();
        let task_json = serde_json::to_string(&task)?;
        let response_json = self
            .complete_json(self.strong_model.as_str(), &format!("{task_kind:?}"), task_json)
            .await?;
        task.decode_response_json(&response_json)
    }

    async fn extract_verbatim_source_evidence_candidates(
        &self,
        task: VerbatimSourceEvidenceExtractionTask,
    ) -> Result<VerbatimSourceEvidenceCandidateSet> {
        task.validate()?;
        let task_json = serde_json::to_string(&task)?;
        let response_json = self
            .complete_json(
                self.cheap_model.as_str(),
                "verbatim_source_evidence_extraction",
                task_json,
            )
            .await?;
        task.decode_response_json(&response_json)
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}

fn model_transport_error(error: &reqwest::Error) -> RuntimeError {
    RuntimeError::ModelTransport {
        message: if error.is_timeout() {
            "model endpoint timed out".to_owned()
        } else if error.is_connect() {
            "model endpoint connection failed".to_owned()
        } else {
            "model endpoint request failed".to_owned()
        },
        retryable: is_retryable_transport_error(error),
    }
}

fn retry_delay(completed_attempts: u32) -> Duration {
    let exponent = completed_attempts.saturating_sub(1).min(7);
    INITIAL_RETRY_DELAY.saturating_mul(1_u32 << exponent).min(MAX_RETRY_DELAY)
}

fn system_instruction(task_kind: &str, prompt_schema_version: u32) -> String {
    // This string is static and contains no user/document data. All untrusted
    // task data is sent as the separate JSON data message.
    format!(
        "You are a structured-output adapter for task kind {task_kind}. Return only the closed JSON response envelope required by schema version {prompt_schema_version}. Do not invent opaque IDs."
    )
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: [ChatMessage; 2],
    response_format: ResponseFormat<'a>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Debug, Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessageResponse,
}

#[derive(Debug, Deserialize)]
struct ChatMessageResponse {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode, header::AUTHORIZATION},
        response::IntoResponse,
        routing::post,
    };
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Mutex as AsyncMutex,
    };

    #[derive(Debug)]
    struct CapturedRequest {
        headers: HeaderMap,
        body: Vec<u8>,
    }

    #[derive(Clone)]
    struct FixtureState {
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
        statuses: Arc<AsyncMutex<VecDeque<StatusCode>>>,
        response_body: Arc<String>,
    }

    impl FixtureState {
        fn new(statuses: impl IntoIterator<Item = StatusCode>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                statuses: Arc::new(AsyncMutex::new(statuses.into_iter().collect())),
                response_body: Arc::new(
                    r#"{"choices":[{"message":{"content":"{\"ok\":true}"}}]}"#.to_owned(),
                ),
            }
        }

        fn with_response_body(mut self, response_body: impl Into<String>) -> Self {
            self.response_body = Arc::new(response_body.into());
            self
        }
    }

    async fn handler(
        State(state): State<FixtureState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl IntoResponse {
        state.requests.lock().unwrap().push(CapturedRequest { headers, body: body.to_vec() });
        let status = state
            .statuses
            .lock()
            .await
            .pop_front()
            .expect("fixture must define one status per completed request");
        (status, state.response_body.as_str().to_owned()).into_response()
    }

    async fn spawn_fixture(
        state: FixtureState,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/chat/completions", post(handler)).with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (address, server)
    }

    async fn write_slow_response_body(mut socket: tokio::net::TcpStream, response_delay: Duration) {
        let mut request = [0_u8; 2048];
        let Ok(bytes_read) = socket.read(&mut request).await else {
            return;
        };
        if bytes_read == 0 {
            return;
        }
        const RESPONSE_BODY: &[u8] = br#"{"choices":[{"message":{"content":"{\"ok\":true}"}}]}"#;
        let response_head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            RESPONSE_BODY.len()
        );
        if socket.write_all(response_head.as_bytes()).await.is_err()
            || socket.flush().await.is_err()
        {
            return;
        }
        tokio::time::sleep(response_delay).await;
        let _ = socket.write_all(RESPONSE_BODY).await;
    }

    async fn spawn_slow_body_fixture(
        response_delay: Duration,
    ) -> (std::net::SocketAddr, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);
        let server = tokio::spawn(async move {
            let (first_socket, _) = listener.accept().await.unwrap();
            server_request_count.fetch_add(1, Ordering::SeqCst);
            let first_response =
                tokio::spawn(write_slow_response_body(first_socket, response_delay));

            let (second_socket, _) = listener.accept().await.unwrap();
            server_request_count.fetch_add(1, Ordering::SeqCst);
            let second_response =
                tokio::spawn(write_slow_response_body(second_socket, response_delay));

            let _ = tokio::join!(first_response, second_response);
        });
        (address, request_count, server)
    }

    async fn write_oversized_chunked_response(mut socket: tokio::net::TcpStream) {
        let mut request = [0_u8; 2048];
        let Ok(bytes_read) = socket.read(&mut request).await else {
            return;
        };
        if bytes_read == 0
            || socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                )
                .await
                .is_err()
        {
            return;
        }
        const CHUNK_BYTES: usize = 64 * 1024;
        let chunk = vec![b'x'; CHUNK_BYTES];
        for _ in 0..=(MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES / CHUNK_BYTES) {
            let chunk_header = format!("{CHUNK_BYTES:x}\r\n");
            if socket.write_all(chunk_header.as_bytes()).await.is_err()
                || socket.write_all(&chunk).await.is_err()
                || socket.write_all(b"\r\n").await.is_err()
            {
                return;
            }
        }
        let _ = socket.flush().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    }

    async fn spawn_oversized_chunked_fixture() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            write_oversized_chunked_response(socket).await;
        });
        (address, server)
    }

    #[tokio::test]
    async fn retries_429_until_success() {
        let state = FixtureState::new([StatusCode::TOO_MANY_REQUESTS, StatusCode::OK]);
        let (address, server) = spawn_fixture(state.clone()).await;
        let config = OpenAiCompatibleMarkdownResearchModelGatewayConfig {
            endpoint: format!("http://{address}/v1/"),
            max_attempts: 2,
            ..Default::default()
        };
        let adapter = OpenAiCompatibleMarkdownResearchModelGateway::new(config).unwrap();
        let result = adapter.complete_json("m", "test", "{}".to_owned()).await;
        server.abort();
        // The fixture returns JSON content; the transport itself is what this
        // test is asserting, while task-schema decoding is covered by the
        // closed Gateway tests.
        assert!(result.is_ok());
        assert_eq!(state.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn non_retryable_4xx_maps_to_non_retryable_transport_error() {
        let state = FixtureState::new([StatusCode::BAD_REQUEST]);
        let (address, server) = spawn_fixture(state.clone()).await;
        let config = OpenAiCompatibleMarkdownResearchModelGatewayConfig {
            endpoint: format!("http://{address}/v1/"),
            max_attempts: 3,
            ..Default::default()
        };
        let adapter = OpenAiCompatibleMarkdownResearchModelGateway::new(config).unwrap();
        let error = adapter.complete_json("m", "test", "{}".to_owned()).await.unwrap_err();
        server.abort();
        assert!(matches!(error, RuntimeError::ModelTransport { retryable: false, .. }));
        assert_eq!(state.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn retries_real_http_5xx_responses_until_success() {
        let state = FixtureState::new([
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::OK,
        ]);
        let (address, server) = spawn_fixture(state.clone()).await;
        let config = OpenAiCompatibleMarkdownResearchModelGatewayConfig {
            endpoint: format!("http://{address}/v1/"),
            max_attempts: 3,
            ..Default::default()
        };
        let adapter = OpenAiCompatibleMarkdownResearchModelGateway::new(config).unwrap();
        let result = adapter.complete_json("m", "test", "{}".to_owned()).await;
        server.abort();
        assert!(result.is_ok());
        assert_eq!(state.requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn malformed_success_body_is_not_retried() {
        let state = FixtureState::new([StatusCode::OK]).with_response_body("not-json");
        let (address, server) = spawn_fixture(state.clone()).await;
        let config = OpenAiCompatibleMarkdownResearchModelGatewayConfig {
            endpoint: format!("http://{address}/v1/"),
            max_attempts: 3,
            ..Default::default()
        };
        let adapter = OpenAiCompatibleMarkdownResearchModelGateway::new(config).unwrap();
        let error = adapter.complete_json("m", "test", "{}".to_owned()).await.unwrap_err();
        server.abort();
        assert!(matches!(error, RuntimeError::ModelResponse { .. }));
        assert_eq!(state.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn response_body_timeout_is_retried_and_reported_without_endpoint_details() {
        let (address, request_count, server) =
            spawn_slow_body_fixture(Duration::from_millis(250)).await;
        let adapter = OpenAiCompatibleMarkdownResearchModelGateway::new(
            OpenAiCompatibleMarkdownResearchModelGatewayConfig {
                endpoint: format!("http://{address}/v1/"),
                request_timeout: Duration::from_millis(50),
                max_attempts: 2,
                ..Default::default()
            },
        )
        .unwrap();

        let error = adapter.complete_json("m", "test", "{}".to_owned()).await.unwrap_err();
        server.await.unwrap();

        assert!(matches!(error, RuntimeError::ModelTransport { retryable: true, .. }));
        assert_eq!(error.to_string(), "model transport failed: model endpoint timed out");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn rejects_oversized_chunked_body_before_the_stream_finishes() {
        let (address, server) = spawn_oversized_chunked_fixture().await;
        let adapter = OpenAiCompatibleMarkdownResearchModelGateway::new(
            OpenAiCompatibleMarkdownResearchModelGatewayConfig {
                endpoint: format!("http://{address}/v1/"),
                request_timeout: Duration::from_secs(20),
                max_attempts: 1,
                ..Default::default()
            },
        )
        .unwrap();

        let error = tokio::time::timeout(
            Duration::from_secs(10),
            adapter.complete_json("m", "test", "{}".to_owned()),
        )
        .await
        .expect("size limit stops the response before end-of-stream")
        .unwrap_err();
        server.abort();

        assert!(matches!(error, RuntimeError::ModelResponse { .. }));
        assert!(error.to_string().contains("size limit"));
    }

    #[tokio::test]
    async fn bearer_key_is_header_only_and_redacted_from_debug_and_errors() {
        const API_KEY: &str = "secret-token-unique-marker";
        let state = FixtureState::new([StatusCode::UNAUTHORIZED])
            .with_response_body(format!("server reflected {API_KEY}"));
        let (address, server) = spawn_fixture(state.clone()).await;
        let config = OpenAiCompatibleMarkdownResearchModelGatewayConfig {
            endpoint: format!("http://{address}/v1/"),
            api_key: API_KEY.to_owned(),
            max_attempts: 1,
            ..Default::default()
        };
        let config_debug = format!("{config:?}");
        let adapter = OpenAiCompatibleMarkdownResearchModelGateway::new(config).unwrap();
        let adapter_debug = format!("{adapter:?}");

        let error = adapter
            .complete_json("strong-model", "test", r#"{"question":"safe"}"#.to_owned())
            .await
            .unwrap_err();
        server.abort();

        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.headers.get(AUTHORIZATION).and_then(|value| value.to_str().ok()),
            Some("Bearer secret-token-unique-marker")
        );
        let request_json: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(request_json["model"], "strong-model");
        assert!(!String::from_utf8_lossy(&request.body).contains(API_KEY));

        assert!(config_debug.contains("<redacted>"));
        assert!(adapter_debug.contains("<redacted>"));
        for diagnostic in [config_debug, adapter_debug, error.to_string(), format!("{error:?}")] {
            assert!(!diagnostic.contains(API_KEY));
        }
    }

    #[test]
    fn rejects_invalid_timeout_configuration() {
        let error = OpenAiCompatibleMarkdownResearchModelGateway::new(
            OpenAiCompatibleMarkdownResearchModelGatewayConfig {
                request_timeout: Duration::ZERO,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::Validation { .. }));
    }
}

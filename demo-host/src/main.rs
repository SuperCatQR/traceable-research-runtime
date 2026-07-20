mod catalog;
mod security;
mod workspace_api;

use std::{collections::HashSet, env, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
        uri::Authority,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use tokio::sync::Semaphore;
use tower_http::services::{ServeDir, ServeFile};
use traceable_search::{ResearchInfrastructureConfig, TraceableResearchRuntime};
use url::Url;

use catalog::DemoCatalog;
use security::CredentialCipher;

pub(crate) struct DemoHostState {
    pub(crate) research_runtime: TraceableResearchRuntime,
    pub(crate) research_slots: Semaphore,
    pub(crate) catalog: DemoCatalog,
    pub(crate) credential_cipher: CredentialCipher,
    pub(crate) secure_cookies: bool,
    pub(crate) allow_private_model_endpoints: bool,
    trusted_hosts: HashSet<String>,
    trusted_origins: HashSet<String>,
}

const MAX_BODY_BYTES: usize = 16 * 1024;
const WORKSPACE_ENTRY_CACHE_POLICY: HeaderValue = HeaderValue::from_static("no-store");
const HASHED_ASSET_CACHE_POLICY: HeaderValue =
    HeaderValue::from_static("public, max-age=31536000, immutable");

#[derive(Debug)]
pub(crate) struct PublicHttpError {
    status: StatusCode,
    code: &'static str,
    public_message: &'static str,
    retryable: bool,
}

#[derive(Serialize)]
struct ErrorResponse {
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

impl PublicHttpError {
    pub(crate) fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "request_not_allowed",
            public_message: "Request host or origin is not allowed",
            retryable: false,
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "authentication_required",
            public_message: "请先登录",
            retryable: false,
        }
    }

    pub(crate) fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            public_message: "未找到请求的内容",
            retryable: false,
        }
    }

    pub(crate) fn conflict(code: &'static str, public_message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            public_message,
            retryable: false,
        }
    }

    pub(crate) fn bounded_bad_request(code: &'static str, public_message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            public_message,
            retryable: false,
        }
    }

    pub(crate) fn internal_failure(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "demo API request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            public_message: "服务暂时不可用",
            retryable: true,
        }
    }
}

impl IntoResponse for PublicHttpError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                code: self.code,
                message: self.public_message,
                retryable: self.retryable,
            }),
        )
            .into_response()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "traceable_search_demo_host=info,tower_http=info".into()),
        )
        .init();

    let static_dir =
        PathBuf::from(env::var("DEMO_STATIC_DIR").unwrap_or_else(|_| "web/dist".into()));
    let index = static_dir.join("index.html");
    let infrastructure = ResearchInfrastructureConfig::from_env()?;
    let catalog_path = env::var_os("DEMO_CATALOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| infrastructure.research_data_dir.join("demo-catalog.sqlite"));
    let max_concurrent_research = env::var("DEMO_MAX_CONCURRENT_RESEARCH")
        .unwrap_or_else(|_| "2".into())
        .parse::<usize>()?;
    anyhow::ensure!(
        max_concurrent_research > 0,
        "DEMO_MAX_CONCURRENT_RESEARCH must be positive"
    );
    let address: SocketAddr = env::var("DEMO_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    anyhow::ensure!(
        address.ip().is_loopback() || env_flag("DEMO_ALLOW_NETWORK_BIND"),
        "DEMO_BIND must use loopback unless DEMO_ALLOW_NETWORK_BIND=true"
    );
    let state = Arc::new(DemoHostState {
        research_runtime: TraceableResearchRuntime::new(infrastructure),
        research_slots: Semaphore::new(max_concurrent_research),
        catalog: DemoCatalog::open(catalog_path)?,
        credential_cipher: CredentialCipher::from_env()?,
        secure_cookies: env_flag("DEMO_SECURE_COOKIES"),
        allow_private_model_endpoints: env_flag("DEMO_ALLOW_PRIVATE_MODEL_ENDPOINTS"),
        trusted_hosts: configured_trusted_hosts()?,
        trusted_origins: configured_trusted_origins(address.port())?,
    });
    // Recovery is detached so host startup and read-only conversation routes do
    // not wait on an external model or research backend after a process crash.
    workspace_api::start_automatic_execution_recovery(state.clone());
    let app = build_app(state, static_dir, index);

    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "demo host listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn build_routes(state: Arc<DemoHostState>) -> Router {
    let api = Router::new()
        .merge(workspace_api::routes())
        .route("/health", get(health_check))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_trusted_request,
        ))
        .fallback(api_not_found)
        .with_state(state);
    Router::new().nest("/api", api)
}

#[cfg(test)]
fn build_api_router(state: Arc<DemoHostState>) -> Router {
    build_routes(state).layer(middleware::from_fn(apply_static_cache_policy))
}

fn build_app(state: Arc<DemoHostState>, static_dir: PathBuf, index: PathBuf) -> Router {
    build_routes(state)
        .fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(index)))
        .layer(middleware::from_fn(apply_static_cache_policy))
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn configured_trusted_hosts() -> anyhow::Result<HashSet<String>> {
    let mut hosts = HashSet::from([
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "[::1]".to_owned(),
    ]);
    if let Ok(configured) = env::var("DEMO_TRUSTED_HOSTS") {
        for host in configured
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
        {
            let authority: Authority = host.parse()?;
            anyhow::ensure!(
                authority.port_u16().is_none(),
                "DEMO_TRUSTED_HOSTS entries must not include a port"
            );
            hosts.insert(authority.host().to_owned());
        }
    }
    Ok(hosts)
}

fn configured_trusted_origins(local_port: u16) -> anyhow::Result<HashSet<String>> {
    let mut origins = HashSet::from([
        "http://localhost".to_owned(),
        "http://127.0.0.1".to_owned(),
        "http://[::1]".to_owned(),
        format!("http://localhost:{local_port}"),
        format!("http://127.0.0.1:{local_port}"),
        format!("http://[::1]:{local_port}"),
    ]);
    if let Ok(configured) = env::var("DEMO_TRUSTED_ORIGINS") {
        for value in configured
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let origin = Url::parse(value)?;
            anyhow::ensure!(
                matches!(origin.scheme(), "http" | "https")
                    && origin.path() == "/"
                    && origin.query().is_none()
                    && origin.fragment().is_none(),
                "DEMO_TRUSTED_ORIGINS entries must be HTTP(S) origins"
            );
            origins.insert(origin.origin().ascii_serialization());
        }
    }
    Ok(origins)
}

async fn health_check() -> &'static str {
    "ok"
}

async fn api_not_found() -> PublicHttpError {
    PublicHttpError::not_found()
}

async fn apply_static_cache_policy(request: Request, next: Next) -> Response {
    let requested_path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let content_is_html = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    let cache_policy = if requested_path.starts_with("/api") || content_is_html {
        Some(WORKSPACE_ENTRY_CACHE_POLICY)
    } else if response.status().is_success() && requested_path.starts_with("/assets/") {
        Some(HASHED_ASSET_CACHE_POLICY)
    } else {
        None
    };
    if let Some(cache_policy) = cache_policy {
        response.headers_mut().insert(CACHE_CONTROL, cache_policy);
    }
    response
}

async fn require_trusted_request(
    State(state): State<Arc<DemoHostState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if !host_is_trusted(&headers, &state.trusted_hosts)
        || !origin_is_trusted(&headers, &state.trusted_origins)
    {
        return PublicHttpError::forbidden().into_response();
    }
    next.run(request).await
}

fn host_is_trusted(headers: &HeaderMap, trusted_hosts: &HashSet<String>) -> bool {
    headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Authority>().ok())
        .is_some_and(|authority| trusted_hosts.contains(authority.host()))
}

fn origin_is_trusted(headers: &HeaderMap, trusted_origins: &HashSet<String>) -> bool {
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    origin
        .to_str()
        .ok()
        .is_some_and(|origin| trusted_origins.contains(origin))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header::SET_COOKIE},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::catalog::{
        CatalogError, DurableIdempotencyClaim, DurableIdempotencyCompletion,
        NewDurableIdempotencyClaim,
    };

    struct TestApp {
        app: Router,
        state: Arc<DemoHostState>,
        _directory: TempDir,
    }

    fn test_app() -> TestApp {
        static ENVIRONMENT_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
            std::sync::OnceLock::new();
        let directory = TempDir::new().unwrap();
        let data_dir = directory.path().join("research");
        let infrastructure = {
            let _environment = ENVIRONMENT_LOCK
                .get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap();
            // Rust 2024 makes process-environment mutation explicit. Keep the
            // existing environment-backed constructor serialized and restore
            // the variables before another Router fixture can be assembled.
            let previous_brave_key = std::env::var_os("BRAVE_SEARCH_API_KEY");
            let previous_data_dir = std::env::var_os("TRACEABLE_SEARCH_DATA_DIR");
            unsafe {
                std::env::set_var("BRAVE_SEARCH_API_KEY", "fixture-key");
                std::env::set_var("TRACEABLE_SEARCH_DATA_DIR", &data_dir);
            }
            let infrastructure = ResearchInfrastructureConfig::from_env().unwrap();
            unsafe {
                match previous_brave_key {
                    Some(value) => std::env::set_var("BRAVE_SEARCH_API_KEY", value),
                    None => std::env::remove_var("BRAVE_SEARCH_API_KEY"),
                }
                match previous_data_dir {
                    Some(value) => std::env::set_var("TRACEABLE_SEARCH_DATA_DIR", value),
                    None => std::env::remove_var("TRACEABLE_SEARCH_DATA_DIR"),
                }
            }
            infrastructure
        };
        let state = Arc::new(DemoHostState {
            research_runtime: TraceableResearchRuntime::new(infrastructure),
            research_slots: Semaphore::new(1),
            catalog: DemoCatalog::open(directory.path().join("catalog.sqlite")).unwrap(),
            credential_cipher: CredentialCipher::from_key_bytes(&[7_u8; 32]).unwrap(),
            secure_cookies: false,
            allow_private_model_endpoints: true,
            trusted_hosts: HashSet::from(["example.test".into()]),
            trusted_origins: HashSet::from(["http://example.test".into()]),
        });
        TestApp {
            app: build_api_router(state.clone()),
            state,
            _directory: directory,
        }
    }

    fn request(
        method: &str,
        path: &str,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "example.test")
            .header("origin", "http://example.test");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        if body.is_some() {
            builder = builder.header(CONTENT_TYPE, "application/json");
        }
        builder
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap()
    }

    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), MAX_BODY_BYTES)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    fn cookie_from(response: &Response) -> String {
        response.headers()[SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn register(app: &Router, email: &str) -> String {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/register",
                Some(json!({
                    "email": email,
                    "password": "long-enough-password",
                    "display_name": "Researcher"
                })),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        cookie_from(&response)
    }

    async fn spawn_scripted_model_fixture(
        contents: Vec<Value>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        #[derive(Clone)]
        struct ScriptedModel {
            contents: Arc<std::sync::Mutex<std::collections::VecDeque<Value>>>,
        }

        async fn complete(State(state): State<ScriptedModel>) -> impl IntoResponse {
            let content = state.contents.lock().unwrap().pop_front().unwrap();
            Json(json!({
                "choices": [{"message": {"content": content.to_string()}}]
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = ScriptedModel {
            contents: Arc::new(std::sync::Mutex::new(contents.into())),
        };
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(complete))
                    .with_state(state),
            )
            .await
            .unwrap();
        });
        (format!("http://{address}/v1/"), handle)
    }

    async fn spawn_model_fixture() -> (String, tokio::task::JoinHandle<()>) {
        spawn_scripted_model_fixture(vec![json!({"ok": true})]).await
    }

    async fn spawn_blocked_model_fixture(
        content: Value,
    ) -> (
        String,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
        tokio::task::JoinHandle<()>,
    ) {
        #[derive(Clone)]
        struct BlockedModel {
            content: Value,
            started: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }

        async fn complete(State(state): State<BlockedModel>) -> impl IntoResponse {
            state.started.notify_one();
            state.release.notified().await;
            Json(json!({
                "choices": [{"message": {"content": state.content.to_string()}}]
            }))
        }

        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let state = BlockedModel {
            content,
            started: started.clone(),
            release: release.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(complete))
                    .with_state(state),
            )
            .await
            .unwrap();
        });
        (format!("http://{address}/v1/"), started, release, handle)
    }

    async fn create_profile(
        app: &Router,
        cookie: &str,
        model_base_url: &str,
        display_name: &str,
        idempotency_key: &str,
    ) -> Value {
        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/model-profiles",
                json!({
                    "display_name": display_name,
                    "api_base_url": model_base_url,
                    "api_key": "top-secret-key",
                    "model_id": "fixture-model",
                    "make_default": true
                }),
                cookie,
                idempotency_key,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    }

    async fn create_conversation(app: &Router, cookie: &str, profile_id: &str) -> Value {
        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/conversations",
                json!({"model_profile_id": profile_id}),
                cookie,
                "conversation-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    }

    fn continue_dialogue_output(original_question: &str, assistant_message: &str) -> Value {
        json!({
            "decision": "continue_dialogue",
            "rationale": "A narrower time range will materially improve retrieval.",
            "assistant_message": assistant_message,
            "brief_draft": {
                "schema_version": 1,
                "original_question": original_question,
                "research_question": original_question,
                "desired_output": null,
                "scope": {
                    "time_range": null,
                    "geography": null,
                    "include": [],
                    "exclude": []
                },
                "source_constraints": [],
                "accepted_assumptions": []
            }
        })
    }

    fn request_with_idempotency(
        method: &str,
        path: &str,
        body: Value,
        cookie: &str,
        idempotency_key: &str,
    ) -> Request<Body> {
        let mut request = request(method, path, Some(body), Some(cookie));
        request
            .headers_mut()
            .insert("idempotency-key", idempotency_key.parse().unwrap());
        request
    }

    fn protected_endpoint_cases() -> Vec<(&'static str, &'static str, Option<Value>)> {
        vec![
            ("POST", "/api/auth/logout", None),
            ("GET", "/api/auth/me", None),
            ("GET", "/api/model-profiles", None),
            (
                "POST",
                "/api/model-profiles",
                Some(json!({
                    "display_name": "Primary",
                    "api_base_url": "http://127.0.0.1:9/v1/",
                    "api_key": "secret",
                    "model_id": "model"
                })),
            ),
            (
                "PATCH",
                "/api/model-profiles/missing",
                Some(json!({"display_name": "X"})),
            ),
            ("POST", "/api/model-profiles/missing/default", None),
            ("POST", "/api/model-profiles/missing/verify", None),
            ("DELETE", "/api/model-profiles/missing", None),
            ("GET", "/api/archives/model-profiles", None),
            ("POST", "/api/model-profiles/missing/restore", None),
            ("GET", "/api/conversations", None),
            ("POST", "/api/conversations", Some(json!({}))),
            ("GET", "/api/conversations/missing", None),
            (
                "PATCH",
                "/api/conversations/missing",
                Some(json!({"title": "X"})),
            ),
            ("DELETE", "/api/conversations/missing", None),
            ("GET", "/api/archives/conversations", None),
            ("POST", "/api/conversations/missing/restore", None),
            (
                "POST",
                "/api/conversations/missing/turns",
                Some(json!({"question": "Q", "answer_style": "web_first"})),
            ),
            (
                "POST",
                "/api/conversations/missing/turns/missing/messages",
                Some(json!({"revision": 1, "message": "More"})),
            ),
            (
                "GET",
                "/api/conversations/missing/turns/missing/trace/summary",
                None,
            ),
            (
                "GET",
                "/api/conversations/missing/turns/missing/trace/audit",
                None,
            ),
        ]
    }

    #[tokio::test]
    async fn every_protected_endpoint_rejects_anonymous_requests_with_json() {
        let test = test_app();
        let app = test.app;
        for (method, path, body) in protected_endpoint_cases() {
            let response = app
                .clone()
                .oneshot(request(method, path, body, None))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(
                response.headers()[CACHE_CONTROL],
                "no-store",
                "{method} {path}"
            );
            assert_eq!(
                response.headers()[CONTENT_TYPE],
                "application/json",
                "{method} {path}"
            );
            let error = json_body(response).await;
            assert_eq!(error["code"], "authentication_required", "{method} {path}");
            assert_eq!(error["retryable"], false, "{method} {path}");
            assert!(error["message"].is_string(), "{method} {path}");
        }
    }

    #[tokio::test]
    async fn authentication_routes_reject_malformed_json_consistently() {
        let test = test_app();
        let app = test.app;
        for path in ["/api/auth/register", "/api/auth/login"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("host", "example.test")
                        .header("origin", "http://example.test")
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from("{"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(json_body(response).await["code"], "invalid_json", "{path}");
        }
    }

    #[tokio::test]
    async fn every_json_endpoint_uses_the_stable_invalid_json_error() {
        let test = test_app();
        let app = test.app;
        let cookie = register(&app, "invalid-json@example.com").await;
        for (method, path, needs_idempotency) in [
            ("POST", "/api/model-profiles", true),
            ("PATCH", "/api/model-profiles/missing", false),
            ("POST", "/api/conversations", true),
            ("PATCH", "/api/conversations/missing", false),
            ("POST", "/api/conversations/missing/restore", false),
            ("POST", "/api/conversations/missing/turns", true),
            (
                "POST",
                "/api/conversations/missing/turns/missing/messages",
                true,
            ),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", "example.test")
                .header("origin", "http://example.test")
                .header("cookie", &cookie)
                .header(CONTENT_TYPE, "application/json");
            if needs_idempotency {
                request = request.header("idempotency-key", "invalid-json-1");
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::from("{")).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{method} {path}"
            );
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            assert_eq!(
                json_body(response).await["code"],
                "invalid_json",
                "{method} {path}"
            );
        }

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/conversations/missing/turns",
                json!({"question": "Missing the required answer style"}),
                &cookie,
                "missing-answer-style-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["code"], "invalid_json");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations/missing/restore")
                    .header("host", "example.test")
                    .header("origin", "http://example.test")
                    .header("cookie", &cookie)
                    .header(CONTENT_TYPE, "text/plain")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["code"], "invalid_json");

        let response = app
            .oneshot(request_with_idempotency(
                "POST",
                "/api/conversations",
                json!({"title": "Undeclared create field"}),
                &cookie,
                "unknown-field-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["code"], "invalid_json");
    }

    #[tokio::test]
    async fn oversized_json_body_is_rejected_with_the_public_error_shape() {
        let test = test_app();
        let app = test.app;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("host", "example.test")
                    .header("origin", "http://example.test")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        "{{\"email\":\"{}@example.com\",\"password\":\"password-long-enough\"}}",
                        "x".repeat(MAX_BODY_BYTES)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(json_body(response).await["code"], "invalid_json");
    }

    #[tokio::test]
    async fn untrusted_host_and_origin_return_structured_forbidden_errors() {
        let test = test_app();
        let app = test.app;
        for (host, origin) in [
            ("attacker.example", "http://example.test"),
            ("example.test", "https://attacker.example"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/auth/me")
                        .header("host", host)
                        .header("origin", origin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            let error = json_body(response).await;
            assert_eq!(error["code"], "request_not_allowed");
            assert_eq!(error["retryable"], false);
        }
    }

    #[tokio::test]
    async fn app_shell_is_not_cached_and_hashed_assets_are_immutable() {
        let test = test_app();
        let static_dir = test._directory.path().join("static");
        let assets_dir = static_dir.join("assets");
        std::fs::create_dir_all(&assets_dir).unwrap();
        let index = static_dir.join("index.html");
        std::fs::write(&index, "<!doctype html><title>Workspace</title>").unwrap();
        std::fs::write(assets_dir.join("app-1234abcd.js"), "export {};").unwrap();
        let app = build_app(test.state, static_dir, index);

        for path in ["/", "/conversations/example"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store", "{path}");
            assert!(
                response.headers()[CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html")
            );
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/app-1234abcd.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/unknown")
                    .header("host", "example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(json_body(response).await["code"], "not_found");
    }

    #[tokio::test]
    async fn model_profile_router_contract_covers_crud_verification_and_idempotency() {
        let test = test_app();
        let app = test.app;
        let cookie = register(&app, "model-owner@example.com").await;
        let other_cookie = register(&app, "model-other@example.com").await;
        let (model_base_url, fixture) = spawn_model_fixture().await;
        let create_body = json!({
            "display_name": "Primary",
            "api_base_url": model_base_url,
            "api_key": "top-secret-key",
            "model_id": "fixture-model",
            "make_default": true
        });

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/model-profiles",
                Some(create_body.clone()),
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(response).await["code"],
            "idempotency_key_required"
        );

        let invalid_body = json!({
            "display_name": "Invalid endpoint",
            "api_base_url": "not-a-url",
            "api_key": "top-secret-key",
            "model_id": "fixture-model"
        });
        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/model-profiles",
                invalid_body.clone(),
                &cookie,
                "model-error-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let first_error = json_body(response).await;
        assert_eq!(first_error["code"], "invalid_model_endpoint");
        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/model-profiles",
                invalid_body,
                &cookie,
                "model-error-1",
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, first_error);

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/model-profiles",
                create_body.clone(),
                &cookie,
                "model-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let original = json_body(response).await;
        let profile_id = original["profile_id"].as_str().unwrap();
        assert_eq!(original["has_api_key"], true);
        assert_eq!(original["is_default"], true);
        let serialized = original.to_string();
        assert!(!serialized.contains("top-secret-key"));
        assert!(!serialized.contains("ciphertext"));
        assert!(!serialized.contains("nonce"));

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/model-profiles",
                None,
                Some(&other_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, json!([]));

        for (method, suffix, body) in [
            ("PATCH", "", Some(json!({"display_name": "Forbidden"}))),
            ("POST", "/default", None),
            ("POST", "/verify", None),
            ("DELETE", "", None),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    method,
                    &format!("/api/model-profiles/{profile_id}{suffix}"),
                    body,
                    Some(&other_cookie),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {suffix}"
            );
            assert_eq!(json_body(response).await["code"], "not_found");
        }

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/model-profiles",
                create_body.clone(),
                &cookie,
                "model-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, original);

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/model-profiles",
                json!({
                    "display_name": "Different",
                    "api_base_url": model_base_url,
                    "api_key": "top-secret-key",
                    "model_id": "fixture-model"
                }),
                &cookie,
                "model-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(json_body(response).await["code"], "idempotency_key_reused");

        let response = app
            .clone()
            .oneshot(request("GET", "/api/model-profiles", None, Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(json_body(response).await.as_array().unwrap().len(), 1);

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/model-profiles/{profile_id}/verify"),
                None,
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/model-profiles/{profile_id}"),
                Some(json!({"display_name": "Renamed"})),
                Some(&cookie),
            ))
            .await
            .unwrap();
        let updated = json_body(response).await;
        assert_eq!(updated["display_name"], "Renamed");
        assert_eq!(updated["verified_at"], Value::Null);
        assert_eq!(updated["revision"], 2);

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/model-profiles/{profile_id}/default"),
                None,
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/api/model-profiles/{profile_id}"),
                None,
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/archives/model-profiles",
                None,
                Some(&cookie),
            ))
            .await
            .unwrap();
        let archived = json_body(response).await;
        assert_eq!(archived[0]["profile_id"], profile_id);

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/archives/model-profiles",
                None,
                Some(&other_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, json!([]));

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/model-profiles/{profile_id}/restore"),
                None,
                Some(&other_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["code"], "not_found");

        let response = app
            .oneshot(request(
                "POST",
                &format!("/api/model-profiles/{profile_id}/restore"),
                None,
                Some(&cookie),
            ))
            .await
            .unwrap();
        let restored = json_body(response).await;
        assert_eq!(restored["profile_id"], profile_id);
        assert_eq!(restored["display_name"], "Renamed");
        fixture.abort();
    }

    #[tokio::test]
    async fn model_profile_verification_rejects_a_concurrent_edit() {
        let test = test_app();
        let app = test.app;
        let cookie = register(&app, "verify-race@example.com").await;
        let (model_base_url, started, release, fixture) =
            spawn_blocked_model_fixture(json!({"ok": true})).await;
        let profile = create_profile(
            &app,
            &cookie,
            &model_base_url,
            "Verify race",
            "verify-race-profile",
        )
        .await;
        let profile_id = profile["profile_id"].as_str().unwrap().to_owned();

        let verify_app = app.clone();
        let verify_cookie = cookie.clone();
        let verify_profile_id = profile_id.clone();
        let verification = tokio::spawn(async move {
            verify_app
                .oneshot(request(
                    "POST",
                    &format!("/api/model-profiles/{verify_profile_id}/verify"),
                    None,
                    Some(&verify_cookie),
                ))
                .await
                .unwrap()
        });
        started.notified().await;

        let response = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/model-profiles/{profile_id}"),
                Some(json!({"display_name": "Edited during verification"})),
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["revision"], 2);

        release.notify_one();
        let response = verification.await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(json_body(response).await["code"], "model_profile_changed");

        let response = app
            .oneshot(request("GET", "/api/model-profiles", None, Some(&cookie)))
            .await
            .unwrap();
        let profiles = json_body(response).await;
        assert_eq!(profiles[0]["verified_at"], Value::Null);
        fixture.abort();
    }

    #[tokio::test]
    async fn conversation_router_contract_covers_crud_archive_restore_and_ownership() {
        let test = test_app();
        let app = test.app;
        let owner_cookie = register(&app, "conversation-owner@example.com").await;
        let other_cookie = register(&app, "other-owner@example.com").await;
        let (model_base_url, fixture) = spawn_model_fixture().await;
        let profile = create_profile(
            &app,
            &owner_cookie,
            &model_base_url,
            "Conversation model",
            "conversation-model-1",
        )
        .await;
        let conversation =
            create_conversation(&app, &owner_cookie, profile["profile_id"].as_str().unwrap()).await;
        let conversation_id = conversation["conversation_id"].as_str().unwrap();
        assert_eq!(conversation["turns"], json!([]));

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/conversations",
                json!({"model_profile_id": profile["profile_id"]}),
                &owner_cookie,
                "conversation-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, conversation);

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/conversations",
                None,
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await.as_array().unwrap().len(), 1);

        for (method, suffix, body) in [
            ("GET", "", None),
            ("PATCH", "", Some(json!({"title": "Forbidden"}))),
            ("DELETE", "", None),
            ("POST", "/restore", None),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    method,
                    &format!("/api/conversations/{conversation_id}{suffix}"),
                    body,
                    Some(&other_cookie),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {suffix}"
            );
            assert_eq!(json_body(response).await["code"], "not_found");
        }

        let response = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/conversations/{conversation_id}"),
                Some(json!({"title": "Renamed conversation"})),
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await["title"], "Renamed conversation");

        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/api/conversations/{conversation_id}"),
                None,
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/archives/conversations",
                None,
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        let archived = json_body(response).await;
        assert_eq!(archived[0]["conversation_id"], conversation_id);
        assert_eq!(archived[0]["model_profile_available"], true);

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/archives/conversations",
                None,
                Some(&other_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, json!([]));

        let other_profile = create_profile(
            &app,
            &other_cookie,
            &model_base_url,
            "Other account model",
            "other-conversation-model-1",
        )
        .await;
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/conversations/{conversation_id}/restore"),
                Some(json!({"model_profile_id": other_profile["profile_id"]})),
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["code"], "not_found");

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/conversations/{conversation_id}/restore"),
                None,
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        let restored = json_body(response).await;
        assert_eq!(restored["conversation_id"], conversation_id);
        assert_eq!(restored["title"], "Renamed conversation");

        let response = app
            .oneshot(request(
                "GET",
                &format!("/api/conversations/{conversation_id}"),
                None,
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await["turns"], json!([]));
        fixture.abort();
    }

    #[tokio::test]
    async fn turn_and_dialogue_router_contract_covers_replay_revision_and_locked_resources() {
        let question = "Compare the current approaches";
        let test = test_app();
        let app = test.app;
        let cookie = register(&app, "turn-owner@example.com").await;
        let other_cookie = register(&app, "turn-other@example.com").await;
        let (model_base_url, fixture) = spawn_scripted_model_fixture(vec![
            continue_dialogue_output(question, "Which time range should I use?"),
            continue_dialogue_output(question, "I still need the target geography."),
        ])
        .await;
        let profile =
            create_profile(&app, &cookie, &model_base_url, "Turn model", "turn-model-1").await;
        let profile_id = profile["profile_id"].as_str().unwrap();
        let conversation = create_conversation(&app, &cookie, profile_id).await;
        let conversation_id = conversation["conversation_id"].as_str().unwrap();
        let body = json!({"question": question, "answer_style": "web_first"});

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &format!("/api/conversations/{conversation_id}/turns"),
                body.clone(),
                &cookie,
                "turn-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let turn = json_body(response).await;
        let turn_id = turn["turn_id"].as_str().unwrap();
        assert_eq!(turn["status"], "clarifying");
        assert_eq!(turn["dialogue"]["status"], "awaiting_message");
        assert_eq!(turn["dialogue"]["revision"], 1);
        assert_eq!(turn["answer"], Value::Null);
        let serialized = turn.to_string();
        for forbidden in ["run_id", "brief", "api_base_url", "api_key", "rationale"] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &format!("/api/conversations/{conversation_id}/turns"),
                json!({"question": "Foreign", "answer_style": "web_first"}),
                &other_cookie,
                "foreign-turn-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["code"], "not_found");

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &format!("/api/conversations/{conversation_id}/turns/{turn_id}/messages"),
                json!({"revision": 1, "message": "Foreign"}),
                &other_cookie,
                "foreign-message-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["code"], "not_found");

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &format!("/api/conversations/{conversation_id}/turns"),
                body,
                &cookie,
                "turn-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, turn);

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &format!("/api/conversations/{conversation_id}/turns"),
                json!({"question": "Another question", "answer_style": "web_first"}),
                &cookie,
                "turn-create-2",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            json_body(response).await["code"],
            "conversation_has_active_turn"
        );

        let response = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/conversations/{conversation_id}"),
                Some(json!({"title": "Rename while clarifying"})),
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await["title"],
            "Rename while clarifying"
        );

        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/api/conversations/{conversation_id}"),
                None,
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            json_body(response).await["code"],
            "conversation_has_active_turn"
        );

        let response = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/model-profiles/{profile_id}"),
                Some(json!({"display_name": "Blocked edit"})),
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            json_body(response).await["code"],
            "model_profile_in_use_by_active_turn"
        );

        let message_path = format!("/api/conversations/{conversation_id}/turns/{turn_id}/messages");
        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &message_path,
                json!({"revision": 0, "message": "Use the last five years"}),
                &cookie,
                "message-create-stale",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            json_body(response).await["code"],
            "dialogue_revision_conflict"
        );

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &message_path,
                json!({"revision": 1, "message": "Use the last five years"}),
                &cookie,
                "message-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let updated = json_body(response).await;
        assert_eq!(updated["dialogue"]["revision"], 2);
        assert_eq!(updated["dialogue"]["messages"].as_array().unwrap().len(), 4);

        let response = app
            .oneshot(request_with_idempotency(
                "POST",
                &message_path,
                json!({"revision": 1, "message": "Use the last five years"}),
                &cookie,
                "message-create-1",
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, updated);
        fixture.abort();
    }

    #[tokio::test]
    async fn concurrent_turn_retry_reports_in_progress_then_replays_completion() {
        let question = "Hold this model request";
        let test = test_app();
        let app = test.app;
        let cookie = register(&app, "concurrent-turn@example.com").await;
        let (model_base_url, started, release, fixture) = spawn_blocked_model_fixture(
            continue_dialogue_output(question, "Please add a date range."),
        )
        .await;
        let profile = create_profile(
            &app,
            &cookie,
            &model_base_url,
            "Concurrent model",
            "concurrent-model-1",
        )
        .await;
        let conversation =
            create_conversation(&app, &cookie, profile["profile_id"].as_str().unwrap()).await;
        let conversation_id = conversation["conversation_id"].as_str().unwrap().to_owned();
        let path = format!("/api/conversations/{conversation_id}/turns");
        let body = json!({"question": question, "answer_style": "web_first"});
        let first_app = app.clone();
        let first_cookie = cookie.clone();
        let first_path = path.clone();
        let first_body = body.clone();
        let first = tokio::spawn(async move {
            first_app
                .oneshot(request_with_idempotency(
                    "POST",
                    &first_path,
                    first_body,
                    &first_cookie,
                    "concurrent-turn-1",
                ))
                .await
                .unwrap()
        });
        started.notified().await;

        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &path,
                body.clone(),
                &cookie,
                "concurrent-turn-1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let in_progress = json_body(response).await;
        assert_eq!(in_progress["code"], "idempotency_request_in_progress");
        assert_eq!(in_progress["retryable"], true);

        release.notify_one();
        let first_response = first.await.unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);
        let completed = json_body(first_response).await;
        let response = app
            .oneshot(request_with_idempotency(
                "POST",
                &path,
                body,
                &cookie,
                "concurrent-turn-1",
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await, completed);
        fixture.abort();
    }

    #[tokio::test]
    async fn trace_router_contract_validates_filters_pagination_ownership_and_projection() {
        let question = "Trace this request";
        let test = test_app();
        let app = test.app;
        let owner_cookie = register(&app, "trace-owner@example.com").await;
        let other_cookie = register(&app, "trace-other@example.com").await;
        let (model_base_url, fixture) =
            spawn_scripted_model_fixture(vec![continue_dialogue_output(
                question,
                "Please narrow the request.",
            )])
            .await;
        let profile = create_profile(
            &app,
            &owner_cookie,
            &model_base_url,
            "Trace model",
            "trace-model-1",
        )
        .await;
        let conversation =
            create_conversation(&app, &owner_cookie, profile["profile_id"].as_str().unwrap()).await;
        let conversation_id = conversation["conversation_id"].as_str().unwrap();
        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                &format!("/api/conversations/{conversation_id}/turns"),
                json!({"question": question, "answer_style": "web_first"}),
                &owner_cookie,
                "trace-turn-1",
            ))
            .await
            .unwrap();
        let turn = json_body(response).await;
        let turn_id = turn["turn_id"].as_str().unwrap();
        let summary_path =
            format!("/api/conversations/{conversation_id}/turns/{turn_id}/trace/summary");
        let audit_path =
            format!("/api/conversations/{conversation_id}/turns/{turn_id}/trace/audit");

        for path in [&summary_path, &audit_path] {
            let response = app
                .clone()
                .oneshot(request("GET", path, None, Some(&other_cookie)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(json_body(response).await["code"], "not_found");
        }

        let response = app
            .clone()
            .oneshot(request("GET", &summary_path, None, Some(&owner_cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let summary = json_body(response).await;
        assert_eq!(summary["model_id"], "fixture-model");
        assert_eq!(
            summary["understanding"]["message"],
            "Please narrow the request."
        );
        let serialized = summary.to_string();
        for forbidden in ["run_id", "api_key", "api_base_url", "audit_status"] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }

        for (query, code) in [
            ("?stage=private", "invalid_trace_stage"),
            ("?cursor=999", "invalid_trace_cursor"),
            ("?cursor=abc", "invalid_trace_cursor"),
            ("?cursor=-1", "invalid_trace_cursor"),
            ("?limit=0", "invalid_trace_limit"),
            ("?limit=abc", "invalid_trace_limit"),
            ("?limit=-1", "invalid_trace_limit"),
            ("?limit=101", "invalid_trace_limit"),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    "GET",
                    &format!("{audit_path}{query}"),
                    None,
                    Some(&owner_cookie),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
            assert_eq!(json_body(response).await["code"], code, "{query}");
        }

        let response = app
            .oneshot(request(
                "GET",
                &format!("{audit_path}?stage=dialogue&cursor=0&limit=1"),
                None,
                Some(&owner_cookie),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let audit = json_body(response).await;
        assert_eq!(audit["entries"].as_array().unwrap().len(), 1);
        assert_eq!(audit["entries"][0]["stage"], "dialogue");
        assert!(audit["next_cursor"].is_number());
        let serialized = audit.to_string();
        for forbidden in ["run_id", "api_key", "api_base_url", "brief_draft"] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
        fixture.abort();
    }

    #[tokio::test]
    async fn authentication_router_contract_sets_and_revokes_a_secure_session() {
        let test = test_app();
        let app = test.app;
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/register",
                Some(json!({
                    "email": "researcher@example.com",
                    "password": "long-enough-password",
                    "display_name": "Researcher"
                })),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        let set_cookie = response.headers()[SET_COOKIE].to_str().unwrap().to_owned();
        assert!(set_cookie.contains("traceable_login="));
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        let cookie = set_cookie.split(';').next().unwrap().to_owned();
        let account = json_body(response).await;
        assert_eq!(account["email"], "researcher@example.com");

        let response = app
            .clone()
            .oneshot(request("GET", "/api/auth/me", None, Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["display_name"], "Researcher");

        let response = app
            .clone()
            .oneshot(request("POST", "/api/auth/logout", None, Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");

        let response = app
            .clone()
            .oneshot(request("GET", "/api/auth/me", None, Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let error = json_body(response).await;
        assert_eq!(error["code"], "authentication_required");
        assert_eq!(error["retryable"], false);
        assert!(error["message"].is_string());

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/login",
                Some(json!({
                    "email": "researcher@example.com",
                    "password": "wrong-password"
                })),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(response).await["code"], "invalid_credentials");

        let response = app
            .oneshot(request(
                "POST",
                "/api/auth/login",
                Some(json!({
                    "email": "researcher@example.com",
                    "password": "long-enough-password"
                })),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("traceable_login=")
        );
    }

    #[tokio::test]
    async fn durable_conversation_takeover_reuses_operation_resource_id() {
        let test = test_app();
        let app = test.app;
        let state = test.state;
        let cookie = register(&app, "takeover-owner@example.com").await;
        let (model_base_url, fixture) = spawn_model_fixture().await;
        let profile = create_profile(
            &app,
            &cookie,
            &model_base_url,
            "Takeover model",
            "takeover-profile-1",
        )
        .await;
        let profile_id = profile["profile_id"].as_str().unwrap().to_owned();
        let body = json!({"model_profile_id": profile_id});
        let request_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&body).unwrap()));
        let user = state
            .catalog
            .user_account_by_email("takeover-owner@example.com")
            .unwrap()
            .unwrap();
        let old_now = chrono::Utc::now().timestamp().saturating_sub(301);
        let old_lease = match state
            .catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id: &user.user_id,
                method: "POST",
                resource_scope: "conversations",
                key: "takeover-conversation",
                request_hash: &request_hash,
                serialization_key: None,
                now: old_now,
                expires_at: old_now + 24 * 60 * 60,
            })
            .unwrap()
        {
            DurableIdempotencyClaim::Claimed(lease) => lease,
            other => panic!("expected stale durable lease, got {other:?}"),
        };
        let response = app
            .clone()
            .oneshot(request_with_idempotency(
                "POST",
                "/api/conversations",
                json!({"model_profile_id": profile["profile_id"]}),
                &cookie,
                "takeover-conversation",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let conversation = json_body(response).await;
        assert_eq!(conversation["conversation_id"], old_lease.operation_id);

        let stale_completion = state.catalog.complete_durable_idempotency(
            DurableIdempotencyCompletion {
                user_id: &user.user_id,
                method: "POST",
                resource_scope: "conversations",
                key: "takeover-conversation",
                operation_id: &old_lease.operation_id,
                operation_created_at: old_lease.operation_created_at,
                claim_token: &old_lease.claim_token,
                status_code: 200,
            },
            "{}",
        );
        assert!(matches!(stale_completion, Err(CatalogError::NotFound)));
        fixture.abort();
    }

    #[test]
    fn local_host_and_same_origin_are_required() {
        let trusted_hosts = HashSet::from([
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
            "[::1]".to_owned(),
        ]);
        let trusted_origins = configured_trusted_origins(8080).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:8080".parse().unwrap());
        assert!(host_is_trusted(&headers, &trusted_hosts));
        assert!(origin_is_trusted(&headers, &trusted_origins));

        headers.insert("origin", "http://localhost:8080".parse().unwrap());
        assert!(origin_is_trusted(&headers, &trusted_origins));
        headers.insert("origin", "http://127.0.0.1:8080".parse().unwrap());
        assert!(origin_is_trusted(&headers, &trusted_origins));
        headers.insert("origin", "http://127.0.0.1:8090".parse().unwrap());
        assert!(!origin_is_trusted(&headers, &trusted_origins));
        headers.insert("origin", "https://attacker.example".parse().unwrap());
        assert!(!origin_is_trusted(&headers, &trusted_origins));
        headers.insert("host", "attacker.example".parse().unwrap());
        assert!(!host_is_trusted(&headers, &trusted_hosts));
    }
}

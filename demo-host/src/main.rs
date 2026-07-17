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
    pub(crate) fn invalid_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            public_message: "请求无效",
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
    let api = Router::new()
        .merge(workspace_api::routes())
        .route("/health", get(health_check))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_trusted_request,
        ))
        .with_state(state);
    let app = Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(static_dir).not_found_service(ServeFile::new(index)))
        .layer(middleware::from_fn(apply_static_cache_policy));

    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "demo host listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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

async fn apply_static_cache_policy(request: Request, next: Next) -> Response {
    let requested_path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let content_is_html = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    let cache_policy = if content_is_html {
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
        return StatusCode::FORBIDDEN.into_response();
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
    use super::*;

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

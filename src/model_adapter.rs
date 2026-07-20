//! OpenAI-compatible model HTTP adapter.

use std::{net::SocketAddr, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use crate::{ResearchError, Result, web_snapshot::resolve_public_url};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

fn model_http_client(
    pin: Option<(&str, &[SocketAddr])>,
    require_direct_connection: bool,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("traceable-search/", env!("CARGO_PKG_VERSION")));
    if require_direct_connection {
        builder = builder.no_proxy();
    }
    if let Some((domain, addresses)) = pin {
        builder = builder.resolve_to_addrs(domain, addresses);
    }
    builder.build().map_err(|error| ResearchError::ModelCall {
        message: format!("model HTTP client setup failed: {error}"),
    })
}

/// Minimal OpenAI-compatible JSON client. Secrets stay in caller-owned runtime
/// configuration and are never read from or written to the repository.
pub struct OpenAiCompatibleModelClient {
    transport: ModelTransport,
    chat_completions_endpoint: Url,
    model_api_key: String,
    model_id: String,
}

enum ModelTransport {
    Unrestricted(reqwest::Client),
    PublicOnly,
}

impl OpenAiCompatibleModelClient {
    pub fn new(
        api_base_url: &str,
        model_api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Self> {
        Self::with_transport(
            api_base_url,
            model_api_key,
            model_id,
            ModelTransport::Unrestricted(model_http_client(None, false)?),
        )
    }

    /// Creates a client that resolves and pins a public endpoint immediately
    /// before every request. The request URL keeps its hostname for Host, TLS
    /// SNI, and certificate validation while the connection uses only checked
    /// addresses.
    pub fn new_public(
        api_base_url: &str,
        model_api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Self> {
        Self::with_transport(
            api_base_url,
            model_api_key,
            model_id,
            ModelTransport::PublicOnly,
        )
    }

    fn with_transport(
        api_base_url: &str,
        model_api_key: impl Into<String>,
        model_id: impl Into<String>,
        transport: ModelTransport,
    ) -> Result<Self> {
        let chat_completions_endpoint = Url::parse(api_base_url)
            .and_then(|base| base.join("chat/completions"))
            .map_err(|error| ResearchError::ModelCall {
                message: format!("invalid model endpoint: {error}"),
            })?;
        Ok(Self {
            transport,
            chat_completions_endpoint,
            model_api_key: model_api_key.into(),
            model_id: model_id.into(),
        })
    }

    pub async fn generate_text(&self, system_prompt: &str, user_prompt: &str) -> Result<String> {
        let (http_client, endpoint) = match &self.transport {
            ModelTransport::Unrestricted(client) => {
                (client.clone(), self.chat_completions_endpoint.clone())
            }
            ModelTransport::PublicOnly => {
                let (endpoint, pin) =
                    resolve_public_url(self.chat_completions_endpoint.as_str()).await?;
                let client = model_http_client(
                    pin.as_ref()
                        .map(|(domain, addresses)| (domain.as_str(), addresses.as_slice())),
                    true,
                )?;
                (client, endpoint)
            }
        };
        let mut request = http_client.post(endpoint).json(&ChatRequest {
            model: &self.model_id,
            messages: [
                ChatMessage {
                    role: "system",
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user",
                    content: user_prompt,
                },
            ],
            response_format: ResponseFormat {
                kind: "json_object",
            },
        });
        if !self.model_api_key.is_empty() {
            request = request.bearer_auth(&self.model_api_key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| ResearchError::ModelCall {
                message: error.to_string(),
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| ResearchError::ModelCall {
                message: error.to_string(),
            })?;
        if !status.is_success() {
            return Err(ResearchError::ModelCall {
                message: format!("model returned HTTP {status}"),
            });
        }
        let completion: ChatResponse =
            serde_json::from_str(&body).map_err(|error| ResearchError::ModelOutput {
                message: format!("invalid completion envelope: {error}"),
            })?;
        let content = completion
            .choices
            .first()
            .ok_or_else(|| ResearchError::ModelOutput {
                message: "completion has no choice".into(),
            })?
            .message
            .content
            .trim();
        Ok(content.to_owned())
    }

    pub async fn generate_structured_output<T: DeserializeOwned>(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<T> {
        let content = self.generate_text(system_prompt, user_prompt).await?;
        serde_json::from_str(&content).map_err(|error| ResearchError::ModelOutput {
            message: format!("invalid JSON content: {error}"),
        })
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 2],
    response_format: ResponseFormat<'a>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Router, http::StatusCode, routing::post};

    use super::*;

    #[tokio::test]
    async fn model_client_does_not_follow_redirects() {
        let private_hits = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/", listener.local_addr().unwrap());
        let private_url = format!("{base_url}private");
        let app = Router::new()
            .route(
                "/chat/completions",
                post({
                    let private_url = private_url.clone();
                    move || async move {
                        (
                            StatusCode::TEMPORARY_REDIRECT,
                            [(axum::http::header::LOCATION, private_url)],
                        )
                    }
                }),
            )
            .route(
                "/private",
                post({
                    let private_hits = private_hits.clone();
                    move || async move {
                        private_hits.fetch_add(1, Ordering::SeqCst);
                        r#"{"choices":[{"message":{"content":"{}"}}]}"#
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = OpenAiCompatibleModelClient::new(&base_url, "secret", "fixture").unwrap();

        let error = client.generate_text("system", "user").await.unwrap_err();

        server.abort();
        assert!(matches!(
            error,
            ResearchError::ModelCall { ref message }
                if message == "model returned HTTP 307 Temporary Redirect"
        ));
        assert_eq!(private_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn public_model_client_rejects_private_targets_before_connecting() {
        let request_hits = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().route(
            "/chat/completions",
            post({
                let request_hits = request_hits.clone();
                move || async move {
                    request_hits.fetch_add(1, Ordering::SeqCst);
                    r#"{"choices":[{"message":{"content":"{}"}}]}"#
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = OpenAiCompatibleModelClient::new_public(
            &format!("http://localhost:{port}/"),
            "secret",
            "fixture",
        )
        .unwrap();

        let error = client.generate_text("system", "user").await.unwrap_err();

        server.abort();
        assert!(matches!(
            error,
            ResearchError::Ssrf { ref reason, .. }
                if reason.contains("non-public address")
        ));
        assert_eq!(request_hits.load(Ordering::SeqCst), 0);
    }
}

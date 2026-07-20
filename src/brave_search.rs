//! Brave Search API adapter.

use std::time::Duration;

use serde::Deserialize;
use url::Url;

use crate::{
    ResearchError, Result, SearchBoundaryContractFailure, SearchEngine, SearchEngineAttempt,
    SearchEngineAttemptOutcome, SearchEngineUnavailability, SearchResult, WebSearchCompletion,
    WebSearchExecution, WebSearchFailureReason,
};

const SEARCH_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const SEARCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Direct Brave Web Search API client. The API key is kept server-side and is
/// never included in a browser response or trace event.
pub struct BraveSearchClient {
    http_client: reqwest::Client,
    search_endpoint: Url,
    api_key: String,
}

impl BraveSearchClient {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Self::with_endpoint(SEARCH_ENDPOINT, api_key)
    }

    fn with_endpoint(endpoint: &str, api_key: impl Into<String>) -> Result<Self> {
        let search_endpoint = Url::parse(endpoint).map_err(|error| ResearchError::Search {
            message: format!("invalid Brave Search endpoint: {error}"),
        })?;
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(ResearchError::Search {
                message: "Brave Search API key is empty".into(),
            });
        }
        Ok(Self {
            http_client: reqwest::Client::builder()
                .timeout(SEARCH_REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("traceable-search/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|error| ResearchError::Search {
                    message: format!("Brave Search HTTP client setup failed: {error}"),
                })?,
            search_endpoint,
            api_key,
        })
    }

    pub async fn search_web(&self, query: &str) -> WebSearchExecution {
        if query.trim().is_empty() {
            return WebSearchExecution {
                attempts: Vec::new(),
                completion: WebSearchCompletion::Failed {
                    reason: WebSearchFailureReason::InvalidQuery,
                },
            };
        }

        let attempt = self.search_once(query).await;
        let completion = match (&attempt.attempt.outcome, &attempt.results) {
            (SearchEngineAttemptOutcome::Completed { .. }, Some(results)) => {
                WebSearchCompletion::Completed {
                    selected_engine: SearchEngine::Brave,
                    results: results.clone(),
                }
            }
            (SearchEngineAttemptOutcome::ContractRejected { .. }, _) => {
                WebSearchCompletion::Failed {
                    reason: WebSearchFailureReason::SearchProviderContractRejected,
                }
            }
            _ => WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::SearchProviderUnavailable,
            },
        };
        WebSearchExecution {
            attempts: vec![attempt.attempt],
            completion,
        }
    }

    async fn search_once(&self, query: &str) -> BraveSearchResult {
        let language = query_language(query);
        let mut search_url = self.search_endpoint.clone();
        search_url.query_pairs_mut().extend_pairs([
            ("q", query),
            ("count", "10"),
            ("search_lang", language.search_lang),
            ("ui_lang", language.ui_lang),
        ]);
        let response = self
            .http_client
            .get(search_url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.api_key)
            .send()
            .await;

        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return BraveSearchResult::unavailable(
                    if error.is_timeout() {
                        SearchEngineUnavailability::RequestTimeout
                    } else {
                        SearchEngineUnavailability::TransportFailure
                    },
                    None,
                );
            }
        };
        let status = response.status();
        let http_status = Some(status.as_u16());
        if status == reqwest::StatusCode::REQUEST_TIMEOUT {
            return BraveSearchResult::unavailable(
                SearchEngineUnavailability::RequestTimeout,
                http_status,
            );
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return BraveSearchResult::unavailable(
                SearchEngineUnavailability::RateLimited,
                http_status,
            );
        }
        if status.is_server_error() {
            return BraveSearchResult::unavailable(
                SearchEngineUnavailability::ServerError,
                http_status,
            );
        }
        if !status.is_success() {
            return BraveSearchResult::contract_rejected(
                SearchBoundaryContractFailure::UnexpectedHttpStatus,
                http_status,
            );
        }
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                return BraveSearchResult::unavailable(
                    if error.is_timeout() {
                        SearchEngineUnavailability::RequestTimeout
                    } else {
                        SearchEngineUnavailability::TransportFailure
                    },
                    http_status,
                );
            }
        };
        match parse_brave_results(query, &body) {
            Ok(results) => BraveSearchResult::completed(results, http_status),
            Err(reason) => BraveSearchResult::contract_rejected(reason, http_status),
        }
    }
}

struct BraveSearchResult {
    attempt: SearchEngineAttempt,
    results: Option<Vec<SearchResult>>,
}

impl BraveSearchResult {
    fn completed(results: Vec<SearchResult>, http_status: Option<u16>) -> Self {
        Self {
            attempt: SearchEngineAttempt {
                engine: SearchEngine::Brave,
                outcome: SearchEngineAttemptOutcome::Completed {
                    valid_result_count: results.len() as u32,
                },
                http_status,
            },
            results: Some(results),
        }
    }

    fn unavailable(reason: SearchEngineUnavailability, http_status: Option<u16>) -> Self {
        Self {
            attempt: SearchEngineAttempt {
                engine: SearchEngine::Brave,
                outcome: SearchEngineAttemptOutcome::Unavailable { reason },
                http_status,
            },
            results: None,
        }
    }

    fn contract_rejected(reason: SearchBoundaryContractFailure, http_status: Option<u16>) -> Self {
        Self {
            attempt: SearchEngineAttempt {
                engine: SearchEngine::Brave,
                outcome: SearchEngineAttemptOutcome::ContractRejected { reason },
                http_status,
            },
            results: None,
        }
    }
}

struct QueryLanguage {
    search_lang: &'static str,
    ui_lang: &'static str,
}

fn query_language(query: &str) -> QueryLanguage {
    if query
        .chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
    {
        QueryLanguage {
            search_lang: "zh-hans",
            ui_lang: "zh-CN",
        }
    } else {
        QueryLanguage {
            search_lang: "en",
            ui_lang: "en-US",
        }
    }
}

#[derive(Deserialize)]
struct BraveEnvelope {
    web: BraveWeb,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveWebResult>,
}

#[derive(Deserialize)]
struct BraveWebResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

fn parse_brave_results(
    query: &str,
    body: &[u8],
) -> std::result::Result<Vec<SearchResult>, SearchBoundaryContractFailure> {
    let raw: BraveEnvelope =
        serde_json::from_slice(body).map_err(|_| SearchBoundaryContractFailure::InvalidResponse)?;
    Ok(raw
        .web
        .results
        .into_iter()
        .filter(|item| {
            Url::parse(&item.url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
        })
        .take(10)
        .enumerate()
        .map(|(index, item)| {
            SearchResult::new(
                SearchEngine::Brave,
                query,
                item.title,
                item.description,
                item.url,
                index as u32 + 1,
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Router, extract::Query, http::StatusCode, response::IntoResponse, routing::get};
    use serde::Deserialize;

    use super::*;

    #[derive(Clone)]
    struct FixtureState {
        status: StatusCode,
        body: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Deserialize)]
    struct FixtureQuery {
        q: String,
        count: String,
        search_lang: String,
    }

    async fn fixture_search(
        Query(query): Query<FixtureQuery>,
        axum::extract::State(state): axum::extract::State<FixtureState>,
    ) -> impl IntoResponse {
        state
            .seen
            .lock()
            .unwrap()
            .push(format!("{}|{}|{}", query.q, query.count, query.search_lang));
        (state.status, state.body)
    }

    async fn client(
        body: &str,
        status: StatusCode,
    ) -> (BraveSearchClient, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let state = FixtureState {
            status,
            body: body.into(),
            seen: seen.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/search", get(fixture_search))
                    .with_state(state),
            )
            .await
            .unwrap();
        });
        (
            BraveSearchClient::with_endpoint(&endpoint, "test-key").unwrap(),
            seen,
        )
    }

    #[tokio::test]
    async fn maps_brave_results_and_sends_api_contract() {
        let body = r#"{"web":{"results":[{"title":"Skip","url":"ftp://example.com/x","description":"bad"},{"title":"React","url":"https://react.dev/","description":"library"}]}}"#;
        let (client, seen) = client(body, StatusCode::OK).await;
        let execution = client.search_web("React framework").await;
        assert!(
            matches!(execution.completion, WebSearchCompletion::Completed { selected_engine: SearchEngine::Brave, ref results } if results.len() == 1)
        );
        assert_eq!(execution.attempts[0].engine, SearchEngine::Brave);
        assert_eq!(seen.lock().unwrap().as_slice(), ["React framework|10|en"]);
    }

    #[tokio::test]
    async fn maps_rate_limit_to_unavailable_without_fallback() {
        let (client, _) = client("{}", StatusCode::TOO_MANY_REQUESTS).await;
        let execution = client.search_web("query").await;
        assert_eq!(execution.attempts.len(), 1);
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::SearchProviderUnavailable
            }
        ));
        assert!(matches!(
            execution.attempts[0].outcome,
            SearchEngineAttemptOutcome::Unavailable {
                reason: SearchEngineUnavailability::RateLimited
            }
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_payloads_without_fallback() {
        let (client, _) = client("not json", StatusCode::OK).await;
        let execution = client.search_web("query").await;
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::SearchProviderContractRejected
            }
        ));
        assert!(matches!(
            execution.attempts[0].outcome,
            SearchEngineAttemptOutcome::ContractRejected {
                reason: SearchBoundaryContractFailure::InvalidResponse
            }
        ));
    }

    #[tokio::test]
    async fn empty_queries_do_not_call_provider() {
        let (client, seen) = client("{}", StatusCode::OK).await;
        let execution = client.search_web(" ").await;
        assert!(execution.attempts.is_empty());
        assert!(seen.lock().unwrap().is_empty());
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::InvalidQuery
            }
        ));
    }
}

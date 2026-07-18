//! P3 external adapters: SearXNG/Bing, crawl4ai, an OpenAI-compatible strong model,
//! and the public-HTTP SSRF boundary shared by every page fetch.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use chrono::Utc;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::{Host, Url};

use crate::{
    CrawlBodyKind, CrawlMeta, ResearchError, Result, SearchBoundaryContractFailure, SearchEngine,
    SearchEngineAttempt, SearchEngineAttemptOutcome, SearchEngineUnavailability, SearchResult,
    Snapshot, WebSearchCompletion, WebSearchExecution, WebSearchFailureReason,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const SEARCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 5;
const MAX_PAGE_BYTES: usize = 4_000_000;
const MAX_ARCHIVED_BODY_BYTES: usize = 4 * 1024 * 1024;
const BLOCKED_DOMAINS: &[&str] = &["talk-doubao.com.cn"];

/// Accept only public HTTP(S) URLs. Every DNS answer is checked: a hostname with
/// even one private/special address is rejected rather than gambling on which
/// address the fetcher chooses.
pub async fn validate_public_web_url(raw: &str) -> Result<Url> {
    resolve_public_url(raw).await.map(|(url, _)| url)
}

async fn resolve_public_url(raw: &str) -> Result<(Url, Option<(String, Vec<SocketAddr>)>)> {
    let url = Url::parse(raw).map_err(|error| ResearchError::Ssrf {
        url: raw.to_owned(),
        reason: format!("invalid URL: {error}"),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ssrf(raw, "only http and https are allowed"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ssrf(raw, "embedded credentials are forbidden"));
    }

    let host = url.host().ok_or_else(|| ssrf(raw, "URL has no host"))?;
    if let Host::Domain(domain) = host
        && BLOCKED_DOMAINS
            .iter()
            .any(|blocked| domain == *blocked || domain.ends_with(&format!(".{blocked}")))
    {
        return Err(ssrf(raw, "domain is blocked"));
    }
    let pin = match host {
        Host::Ipv4(ip) => {
            ensure_public(raw, IpAddr::V4(ip))?;
            None
        }
        Host::Ipv6(ip) => {
            ensure_public(raw, IpAddr::V6(ip))?;
            None
        }
        Host::Domain(domain) => {
            let port = url.port_or_known_default().expect("http(s) has a port");
            let addresses = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|error| ssrf(raw, &format!("DNS lookup failed: {error}")))?
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(ssrf(raw, "DNS returned no address"));
            }
            for address in &addresses {
                ensure_public(raw, address.ip())?;
            }
            Some((domain.to_owned(), addresses))
        }
    };
    Ok((url, pin))
}

fn ensure_public(raw: &str, ip: IpAddr) -> Result<()> {
    if is_public_ip(ip) {
        Ok(())
    } else {
        Err(ssrf(
            raw,
            &format!("host resolved to non-public address {ip}"),
        ))
    }
}

fn ssrf(url: &str, reason: &str) -> ResearchError {
    ResearchError::Ssrf {
        url: url.to_owned(),
        reason: reason.to_owned(),
    }
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224)
        }
        IpAddr::V6(ip) => {
            let octets = ip.octets();
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (octets[0] & 0xfe) == 0xfc
                || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
                || (octets[0..4] == [0x20, 0x01, 0x0d, 0xb8]))
        }
    }
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .user_agent(concat!("traceable-search/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| ResearchError::Search {
            message: format!("HTTP client setup failed: {error}"),
        })
}

fn pinned_page_client(raw: &str, pin: Option<(&str, &[SocketAddr])>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .user_agent(concat!("traceable-search/", env!("CARGO_PKG_VERSION")));
    if let Some((domain, addresses)) = pin {
        builder = builder.resolve_to_addrs(domain, addresses);
    }
    builder.build().map_err(|error| ResearchError::Fetch {
        url: raw.to_owned(),
        reason: format!("HTTP client setup failed: {error}"),
    })
}

#[derive(Debug)]
struct FetchedPage {
    final_url: String,
    status: u16,
    html: String,
}

async fn fetch_public_page(raw: &str) -> Result<FetchedPage> {
    let mut current = raw.to_owned();
    for redirect_count in 0..=MAX_REDIRECTS {
        let (url, pin) = resolve_public_url(&current).await?;
        let client = pinned_page_client(
            url.as_str(),
            pin.as_ref()
                .map(|(domain, addresses)| (domain.as_str(), addresses.as_slice())),
        )?;
        let response =
            client
                .get(url.clone())
                .send()
                .await
                .map_err(|error| ResearchError::Fetch {
                    url: url.to_string(),
                    reason: error.to_string(),
                })?;
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(ResearchError::Fetch {
                    url: url.to_string(),
                    reason: format!("more than {MAX_REDIRECTS} redirects"),
                });
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| ResearchError::Fetch {
                    url: url.to_string(),
                    reason: "redirect has no Location header".into(),
                })?
                .to_str()
                .map_err(|error| ResearchError::Fetch {
                    url: url.to_string(),
                    reason: format!("invalid redirect Location header: {error}"),
                })?;
            current = url
                .join(location)
                .map_err(|error| ResearchError::Fetch {
                    url: url.to_string(),
                    reason: format!("invalid redirect target: {error}"),
                })?
                .to_string();
            continue;
        }
        let status = response.status().as_u16();
        let html = read_page_body(response, url.as_str()).await?;
        return Ok(FetchedPage {
            final_url: url.to_string(),
            status,
            html,
        });
    }
    unreachable!("redirect loop either returns or errors")
}

async fn read_page_body(mut response: reqwest::Response, url: &str) -> Result<String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ResearchError::Fetch {
            url: url.to_owned(),
            reason: format!("failed to read response body: {error}"),
        })?
    {
        if chunk.len() > MAX_PAGE_BYTES - body.len() {
            return Err(ResearchError::Fetch {
                url: url.to_owned(),
                reason: format!("response body exceeds {MAX_PAGE_BYTES} bytes"),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn sanitize_for_offline_crawl(html: &str) -> String {
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(std::collections::HashSet::from([
            "a",
            "article",
            "b",
            "blockquote",
            "br",
            "caption",
            "code",
            "dd",
            "details",
            "div",
            "dl",
            "dt",
            "em",
            "figcaption",
            "figure",
            "footer",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "header",
            "hr",
            "i",
            "li",
            "main",
            "mark",
            "nav",
            "ol",
            "p",
            "pre",
            "s",
            "section",
            "small",
            "span",
            "strong",
            "sub",
            "summary",
            "sup",
            "table",
            "tbody",
            "td",
            "tfoot",
            "th",
            "thead",
            "time",
            "tr",
            "u",
            "ul",
        ]))
        .tag_attributes(std::collections::HashMap::new())
        .generic_attributes(std::collections::HashSet::new())
        .url_schemes(std::collections::HashSet::new());
    // ponytail: safe static structure only; add validated resource rewriting if media becomes required.
    builder.clean(html).to_string()
}

/// Google-first client for a self-hosted SearXNG instance. It never contacts
/// a search engine outside that controlled endpoint.
pub struct SearxngSearchClient {
    http_client: reqwest::Client,
    search_endpoint: Url,
}

impl SearxngSearchClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let search_endpoint = Url::parse(base_url)
            .and_then(|base| base.join("search"))
            .map_err(|error| ResearchError::Search {
                message: format!("invalid SearXNG endpoint: {error}"),
            })?;
        Ok(Self {
            http_client: reqwest::Client::builder()
                .timeout(SEARCH_REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                .user_agent(concat!("traceable-search/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|error| ResearchError::Search {
                    message: format!("SearXNG HTTP client setup failed: {error}"),
                })?,
            search_endpoint,
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
        let google = self
            .search_searxng_engine(query, SearchEngine::Google)
            .await;
        let google_attempt = google.attempt;
        match (google_attempt.outcome.clone(), google.results) {
            (SearchEngineAttemptOutcome::Completed { .. }, Some(results)) => WebSearchExecution {
                attempts: vec![google_attempt],
                completion: WebSearchCompletion::Completed {
                    selected_engine: SearchEngine::Google,
                    results,
                },
            },
            (SearchEngineAttemptOutcome::ContractRejected { .. }, _) => WebSearchExecution {
                attempts: vec![google_attempt],
                completion: WebSearchCompletion::Failed {
                    reason: WebSearchFailureReason::PrimarySearchContractRejected,
                },
            },
            _ => {
                let bing = self.search_searxng_engine(query, SearchEngine::Bing).await;
                let bing_attempt = bing.attempt;
                let completion = match (bing_attempt.outcome.clone(), bing.results) {
                    (SearchEngineAttemptOutcome::Completed { .. }, Some(results)) => {
                        WebSearchCompletion::Completed {
                            selected_engine: SearchEngine::Bing,
                            results,
                        }
                    }
                    _ => WebSearchCompletion::Failed {
                        reason: WebSearchFailureReason::FallbackSearchFailed,
                    },
                };
                WebSearchExecution {
                    attempts: vec![google_attempt, bing_attempt],
                    completion,
                }
            }
        }
    }

    async fn search_searxng_engine(
        &self,
        query: &str,
        engine: SearchEngine,
    ) -> SearxngEngineSearch {
        let mut search_url = self.search_endpoint.clone();
        search_url.query_pairs_mut().extend_pairs([
            ("q", query),
            ("format", "json"),
            ("categories", "general"),
            ("language", query_language(query)),
            ("engines", search_engine_name(engine)),
        ]);
        let response = match self.http_client.get(search_url).send().await {
            Ok(response) => response,
            Err(error) => {
                return SearxngEngineSearch::unavailable(
                    engine,
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
            return SearxngEngineSearch::unavailable(
                engine,
                SearchEngineUnavailability::RequestTimeout,
                http_status,
            );
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return SearxngEngineSearch::unavailable(
                engine,
                SearchEngineUnavailability::RateLimited,
                http_status,
            );
        }
        if status.is_server_error() {
            return SearxngEngineSearch::unavailable(
                engine,
                SearchEngineUnavailability::ServerError,
                http_status,
            );
        }
        if status != reqwest::StatusCode::OK {
            return SearxngEngineSearch::contract_rejected(
                engine,
                SearchBoundaryContractFailure::UnexpectedHttpStatus,
                http_status,
            );
        }
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                return SearxngEngineSearch::unavailable(
                    engine,
                    if error.is_timeout() {
                        SearchEngineUnavailability::RequestTimeout
                    } else {
                        SearchEngineUnavailability::TransportFailure
                    },
                    http_status,
                );
            }
        };
        match parse_searxng_engine_results(engine, query, &body) {
            Ok(ParsedSearxngEngineResults::Completed(results)) => {
                SearxngEngineSearch::completed(engine, results, http_status)
            }
            Ok(ParsedSearxngEngineResults::Unresponsive) => SearxngEngineSearch::unavailable(
                engine,
                SearchEngineUnavailability::EngineUnresponsive,
                http_status,
            ),
            Err(reason) => SearxngEngineSearch::contract_rejected(engine, reason, http_status),
        }
    }
}

struct SearxngEngineSearch {
    attempt: SearchEngineAttempt,
    results: Option<Vec<SearchResult>>,
}

impl SearxngEngineSearch {
    fn completed(
        engine: SearchEngine,
        results: Vec<SearchResult>,
        http_status: Option<u16>,
    ) -> Self {
        Self {
            attempt: SearchEngineAttempt {
                engine,
                outcome: SearchEngineAttemptOutcome::Completed {
                    valid_result_count: results.len() as u32,
                },
                http_status,
            },
            results: Some(results),
        }
    }

    fn unavailable(
        engine: SearchEngine,
        reason: SearchEngineUnavailability,
        http_status: Option<u16>,
    ) -> Self {
        Self {
            attempt: SearchEngineAttempt {
                engine,
                outcome: SearchEngineAttemptOutcome::Unavailable { reason },
                http_status,
            },
            results: None,
        }
    }

    fn contract_rejected(
        engine: SearchEngine,
        reason: SearchBoundaryContractFailure,
        http_status: Option<u16>,
    ) -> Self {
        Self {
            attempt: SearchEngineAttempt {
                engine,
                outcome: SearchEngineAttemptOutcome::ContractRejected { reason },
                http_status,
            },
            results: None,
        }
    }
}

const fn search_engine_name(engine: SearchEngine) -> &'static str {
    match engine {
        SearchEngine::Google => "google",
        SearchEngine::Bing => "bing",
    }
}

fn query_language(query: &str) -> &'static str {
    if query
        .chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
    {
        "zh-CN"
    } else {
        "en-US"
    }
}

#[derive(Deserialize)]
struct SearxngEnvelope {
    results: Vec<SearxngResult>,
    unresponsive_engines: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct SearxngResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    engines: Option<Vec<String>>,
}

#[derive(Debug, PartialEq, Eq)]
enum ParsedSearxngEngineResults {
    Completed(Vec<SearchResult>),
    Unresponsive,
}

fn parse_searxng_engine_results(
    engine: SearchEngine,
    query: &str,
    body: &[u8],
) -> std::result::Result<ParsedSearxngEngineResults, SearchBoundaryContractFailure> {
    let raw: SearxngEnvelope =
        serde_json::from_slice(body).map_err(|_| SearchBoundaryContractFailure::InvalidResponse)?;
    let expected_engine = search_engine_name(engine);
    if raw
        .results
        .iter()
        .any(|result| !searxng_result_proves_engine(result, expected_engine))
        || raw
            .unresponsive_engines
            .iter()
            .any(|(name, _)| name != expected_engine)
    {
        return Err(SearchBoundaryContractFailure::EngineSelectionViolation);
    }
    let results: Vec<_> = raw
        .results
        .into_iter()
        .filter(|item| {
            Url::parse(&item.url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
        })
        .take(10)
        .enumerate()
        .map(|(index, item)| {
            SearchResult::new(
                engine,
                query,
                item.title,
                item.content,
                item.url,
                index as u32 + 1,
            )
        })
        .collect();
    if !results.is_empty() {
        return Ok(ParsedSearxngEngineResults::Completed(results));
    }
    if raw
        .unresponsive_engines
        .iter()
        .any(|(name, _)| name == expected_engine)
    {
        Ok(ParsedSearxngEngineResults::Unresponsive)
    } else {
        Ok(ParsedSearxngEngineResults::Completed(Vec::new()))
    }
}

fn searxng_result_proves_engine(result: &SearxngResult, expected_engine: &str) -> bool {
    let direct_engine_is_valid = result
        .engine
        .as_deref()
        .is_some_and(|engine| engine == expected_engine);
    let merged_engines_are_valid = result.engines.as_ref().is_some_and(|engines| {
        !engines.is_empty() && engines.iter().all(|engine| engine == expected_engine)
    });
    match (&result.engine, &result.engines) {
        (Some(_), Some(_)) => direct_engine_is_valid && merged_engines_are_valid,
        (Some(_), None) => direct_engine_is_valid,
        (None, Some(_)) => merged_engines_are_valid,
        (None, None) => false,
    }
}

/// Thin client for the crawl4ai `/crawl` API. The service endpoint may be on a
/// private network; only the untrusted page URL is subject to the SSRF guard.
pub struct Crawl4AiSnapshotClient {
    http_client: reqwest::Client,
    crawl_endpoint: Url,
    api_token: String,
}

impl Crawl4AiSnapshotClient {
    pub fn new(base_url: &str, api_token: impl Into<String>) -> Result<Self> {
        let crawl_endpoint = Url::parse(base_url)
            .and_then(|base| base.join("crawl"))
            .map_err(|error| ResearchError::Fetch {
                url: base_url.to_owned(),
                reason: format!("invalid crawl4ai endpoint: {error}"),
            })?;
        Ok(Self {
            http_client: http_client()?,
            crawl_endpoint,
            api_token: api_token.into(),
        })
    }

    pub async fn capture_web_snapshot(&self, raw_url: &str) -> Result<Snapshot> {
        let fetched = fetch_public_page(raw_url).await?;
        let offline_url = format!("raw:{}", sanitize_for_offline_crawl(&fetched.html));
        let mut request = self
            .http_client
            .post(self.crawl_endpoint.clone())
            .json(&CrawlRequest {
                urls: [offline_url.as_str()],
            });
        if !self.api_token.is_empty() {
            request = request.bearer_auth(&self.api_token);
        }
        let response = request.send().await.map_err(|error| ResearchError::Fetch {
            url: raw_url.to_owned(),
            reason: error.to_string(),
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(ResearchError::Fetch {
                url: raw_url.to_owned(),
                reason: format!("crawl4ai returned HTTP {status}"),
            });
        }
        let envelope: CrawlEnvelope =
            response
                .json()
                .await
                .map_err(|error| ResearchError::Fetch {
                    url: raw_url.to_owned(),
                    reason: format!("invalid crawl4ai response: {error}"),
                })?;
        snapshot_from_crawl(raw_url, fetched.final_url, fetched.status, envelope)
    }
}

#[derive(Serialize)]
struct CrawlRequest<'a> {
    urls: [&'a str; 1],
}

#[derive(Deserialize)]
struct CrawlEnvelope {
    success: bool,
    #[serde(default)]
    results: Vec<CrawlResult>,
}

#[derive(Deserialize)]
struct CrawlResult {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error_message: String,
    #[serde(default)]
    metadata: serde_json::Value,
    #[serde(default)]
    markdown: CrawlMarkdown,
}

#[derive(Default, Deserialize)]
struct CrawlMarkdown {
    #[serde(default)]
    raw_markdown: String,
    #[serde(default)]
    fit_markdown: String,
}

fn snapshot_from_crawl(
    requested_url: &str,
    final_url: String,
    http_status: u16,
    mut envelope: CrawlEnvelope,
) -> Result<Snapshot> {
    let result = envelope
        .results
        .drain(..)
        .next()
        .ok_or_else(|| ResearchError::Fetch {
            url: requested_url.to_owned(),
            reason: "crawl4ai returned no result".into(),
        })?;
    if !envelope.success || !result.success {
        return Err(ResearchError::Fetch {
            url: requested_url.to_owned(),
            reason: if result.error_message.is_empty() {
                "crawl4ai reported failure".into()
            } else {
                result.error_message
            },
        });
    }
    let (selected_body, body_kind) = if result.markdown.raw_markdown.trim().is_empty() {
        (&result.markdown.fit_markdown, CrawlBodyKind::FitMarkdown)
    } else {
        (&result.markdown.raw_markdown, CrawlBodyKind::RawMarkdown)
    };
    if selected_body.trim().is_empty() {
        return Err(ResearchError::Fetch {
            url: requested_url.to_owned(),
            reason: "crawl4ai returned empty markdown".into(),
        });
    }
    let raw_markdown_bytes = result.markdown.raw_markdown.len();
    let fit_markdown_bytes = result.markdown.fit_markdown.len();
    let title = result
        .metadata
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let (body, truncated) = truncate_utf8(selected_body, MAX_ARCHIVED_BODY_BYTES);
    Ok(Snapshot::new(
        requested_url.to_owned(),
        title,
        body,
        CrawlMeta {
            final_url,
            http_status,
            fetched_at: Utc::now(),
            metadata: result.metadata,
            raw_markdown_bytes,
            fit_markdown_bytes,
            body_kind: Some(body_kind),
            truncated,
        },
    ))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

/// Minimal OpenAI-compatible JSON client. Secrets stay in caller-owned runtime
/// configuration and are never read from or written to the repository.
pub struct OpenAiCompatibleModelClient {
    http_client: reqwest::Client,
    chat_completions_endpoint: Url,
    model_api_key: String,
    model_id: String,
}

impl OpenAiCompatibleModelClient {
    pub fn new(
        api_base_url: &str,
        model_api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Self> {
        let chat_completions_endpoint = Url::parse(api_base_url)
            .and_then(|base| base.join("chat/completions"))
            .map_err(|error| ResearchError::ModelCall {
                message: format!("invalid model endpoint: {error}"),
            })?;
        Ok(Self {
            http_client: http_client()?,
            chat_completions_endpoint,
            model_api_key: model_api_key.into(),
            model_id: model_id.into(),
        })
    }

    pub async fn generate_text(&self, system_prompt: &str, user_prompt: &str) -> Result<String> {
        let mut request = self
            .http_client
            .post(self.chat_completions_endpoint.clone())
            .json(&ChatRequest {
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
                // Upstream bodies may echo secrets or prompts; status is diagnostic enough.
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
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use axum::{
        Router,
        extract::{OriginalUri, State},
        http::StatusCode,
        response::IntoResponse,
        routing::get,
    };

    use super::*;

    #[derive(Clone)]
    struct SearchFixtureState {
        requests: Arc<Mutex<Vec<String>>>,
        responses: Arc<Mutex<VecDeque<(StatusCode, String)>>>,
    }

    async fn scripted_search(
        State(state): State<SearchFixtureState>,
        OriginalUri(uri): OriginalUri,
    ) -> impl IntoResponse {
        state
            .requests
            .lock()
            .unwrap()
            .push(uri.query().unwrap_or_default().to_owned());
        state.responses.lock().unwrap().pop_front().unwrap()
    }

    async fn search_fixture(
        responses: Vec<(StatusCode, &str)>,
    ) -> (
        SearxngSearchClient,
        Arc<Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = SearchFixtureState {
            requests: requests.clone(),
            responses: Arc::new(Mutex::new(
                responses
                    .into_iter()
                    .map(|(status, body)| (status, body.to_owned()))
                    .collect(),
            )),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/search", get(scripted_search))
            .with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            SearxngSearchClient::new(&base_url).unwrap(),
            requests,
            server,
        )
    }

    #[tokio::test]
    async fn google_success_uses_one_explicit_single_engine_request() {
        let (client, requests, server) = search_fixture(vec![(
            StatusCode::OK,
            r#"{"results":[{"title":"Rust","url":"https://example.com/","content":"language","engine":"google","engines":["google"]}],"unresponsive_engines":[]}"#,
        )])
        .await;

        let execution = client.search_web("rust").await;
        server.abort();
        assert_eq!(
            execution.attempts,
            [crate::SearchEngineAttempt {
                engine: crate::SearchEngine::Google,
                outcome: crate::SearchEngineAttemptOutcome::Completed {
                    valid_result_count: 1,
                },
                http_status: Some(200),
            }]
        );
        let crate::WebSearchCompletion::Completed {
            selected_engine,
            results,
        } = execution.completion
        else {
            panic!("Google success did not complete the search");
        };
        assert_eq!(selected_engine, crate::SearchEngine::Google);
        assert_eq!(results[0].search_engine, crate::SearchEngine::Google);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let pairs = url::form_urlencoded::parse(requests[0].as_bytes()).collect::<Vec<_>>();
        assert_eq!(
            pairs
                .iter()
                .filter(|(name, _)| name == "engines")
                .map(|(_, value)| value.as_ref())
                .collect::<Vec<_>>(),
            ["google"]
        );
    }

    #[tokio::test]
    async fn google_unavailable_falls_back_to_one_explicit_bing_request() {
        let (client, requests, server) = search_fixture(vec![
            (
                StatusCode::OK,
                r#"{"results":[],"unresponsive_engines":[["google","upstream timeout"]]}"#,
            ),
            (
                StatusCode::OK,
                r#"{"results":[{"title":"Rust","url":"https://example.com/bing","content":"language","engine":"bing","engines":["bing"]}],"unresponsive_engines":[]}"#,
            ),
        ])
        .await;

        let execution = client.search_web("rust").await;

        server.abort();
        assert_eq!(execution.attempts.len(), 2);
        assert_eq!(execution.attempts[0].engine, SearchEngine::Google);
        assert_eq!(
            execution.attempts[0].outcome,
            SearchEngineAttemptOutcome::Unavailable {
                reason: SearchEngineUnavailability::EngineUnresponsive,
            }
        );
        assert_eq!(execution.attempts[1].engine, SearchEngine::Bing);
        let WebSearchCompletion::Completed {
            selected_engine,
            results,
        } = execution.completion
        else {
            panic!("Bing fallback did not complete the search");
        };
        assert_eq!(selected_engine, SearchEngine::Bing);
        assert_eq!(results[0].search_engine, SearchEngine::Bing);
        let requested_engines = requests
            .lock()
            .unwrap()
            .iter()
            .map(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .find_map(|(name, value)| (name == "engines").then(|| value.into_owned()))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(requested_engines, ["google", "bing"]);
    }

    #[tokio::test]
    async fn google_proven_empty_is_completed_without_bing() {
        let (client, requests, server) = search_fixture(vec![(
            StatusCode::OK,
            r#"{"results":[],"unresponsive_engines":[]}"#,
        )])
        .await;

        let execution = client.search_web("rare exact query").await;

        server.abort();
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert_eq!(
            execution.attempts[0].outcome,
            SearchEngineAttemptOutcome::Completed {
                valid_result_count: 0,
            }
        );
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Completed {
                selected_engine: SearchEngine::Google,
                ref results,
            } if results.is_empty()
        ));
    }

    #[tokio::test]
    async fn google_invalid_urls_are_a_completed_empty_result_without_bing() {
        let (client, requests, server) = search_fixture(vec![(
            StatusCode::OK,
            r#"{"results":[{"title":"FTP","url":"ftp://example.com/file","engine":"google","engines":["google"]}],"unresponsive_engines":[]}"#,
        )])
        .await;

        let execution = client.search_web("rare query").await;

        server.abort();
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Completed {
                selected_engine: SearchEngine::Google,
                ref results,
            } if results.is_empty()
        ));
    }

    #[tokio::test]
    async fn google_valid_results_win_over_same_engine_unresponsive_metadata() {
        let (client, requests, server) = search_fixture(vec![(
            StatusCode::OK,
            r#"{"results":[{"title":"Result","url":"https://example.com/result","engine":"google","engines":["google"]}],"unresponsive_engines":[["google","late response"]]}"#,
        )])
        .await;

        let execution = client.search_web("query").await;

        server.abort();
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Completed {
                selected_engine: SearchEngine::Google,
                ref results,
            } if results.len() == 1
        ));
    }

    #[tokio::test]
    async fn unavailable_http_statuses_fallback_to_a_successful_empty_bing_result() {
        for (status, expected_reason) in [
            (
                StatusCode::REQUEST_TIMEOUT,
                SearchEngineUnavailability::RequestTimeout,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                SearchEngineUnavailability::RateLimited,
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                SearchEngineUnavailability::ServerError,
            ),
        ] {
            let (client, requests, server) = search_fixture(vec![
                (status, "unavailable"),
                (
                    StatusCode::OK,
                    r#"{"results":[],"unresponsive_engines":[]}"#,
                ),
            ])
            .await;

            let execution = client.search_web("query").await;

            server.abort();
            assert_eq!(requests.lock().unwrap().len(), 2);
            assert_eq!(
                execution.attempts[0].outcome,
                SearchEngineAttemptOutcome::Unavailable {
                    reason: expected_reason,
                }
            );
            assert!(matches!(
                execution.completion,
                WebSearchCompletion::Completed {
                    selected_engine: SearchEngine::Bing,
                    ref results,
                } if results.is_empty()
            ));
        }
    }

    #[tokio::test]
    async fn client_errors_and_invalid_payloads_reject_without_bing() {
        for (status, body, expected_reason) in [
            (
                StatusCode::BAD_REQUEST,
                "bad request",
                SearchBoundaryContractFailure::UnexpectedHttpStatus,
            ),
            (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                SearchBoundaryContractFailure::UnexpectedHttpStatus,
            ),
            (
                StatusCode::NOT_FOUND,
                "not found",
                SearchBoundaryContractFailure::UnexpectedHttpStatus,
            ),
            (
                StatusCode::OK,
                "not json",
                SearchBoundaryContractFailure::InvalidResponse,
            ),
            (
                StatusCode::OK,
                r#"{"results":[{"title":"Wrong","url":"https://example.com/","engine":"bing","engines":["bing"]}],"unresponsive_engines":[]}"#,
                SearchBoundaryContractFailure::EngineSelectionViolation,
            ),
        ] {
            let (client, requests, server) = search_fixture(vec![(status, body)]).await;

            let execution = client.search_web("query").await;

            server.abort();
            assert_eq!(requests.lock().unwrap().len(), 1);
            assert_eq!(
                execution.attempts[0].outcome,
                SearchEngineAttemptOutcome::ContractRejected {
                    reason: expected_reason,
                }
            );
            assert!(matches!(
                execution.completion,
                WebSearchCompletion::Failed {
                    reason: WebSearchFailureReason::PrimarySearchContractRejected,
                }
            ));
        }
    }

    #[tokio::test]
    async fn transport_failure_attempts_google_then_bing_and_fails() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let client = SearxngSearchClient::new(&base_url).unwrap();

        let execution = client.search_web("query").await;

        assert_eq!(execution.attempts.len(), 2);
        assert!(execution.attempts.iter().all(|attempt| matches!(
            attempt.outcome,
            SearchEngineAttemptOutcome::Unavailable {
                reason: SearchEngineUnavailability::TransportFailure,
            }
        )));
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::FallbackSearchFailed,
            }
        ));
    }

    #[tokio::test]
    async fn google_contract_rejection_does_not_hide_behind_bing() {
        let (client, requests, server) =
            search_fixture(vec![(StatusCode::FORBIDDEN, "forbidden")]).await;

        let execution = client.search_web("rust").await;

        server.abort();
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(matches!(
            execution.attempts[0].outcome,
            SearchEngineAttemptOutcome::ContractRejected {
                reason: SearchBoundaryContractFailure::UnexpectedHttpStatus,
            }
        ));
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::PrimarySearchContractRejected,
            }
        ));
    }

    #[tokio::test]
    async fn google_rate_limit_falls_back_and_bing_failure_is_terminal() {
        let (client, requests, server) = search_fixture(vec![
            (StatusCode::TOO_MANY_REQUESTS, "rate limited"),
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        ])
        .await;

        let execution = client.search_web("rust").await;

        server.abort();
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert!(matches!(
            execution.attempts[0].outcome,
            SearchEngineAttemptOutcome::Unavailable {
                reason: SearchEngineUnavailability::RateLimited,
            }
        ));
        assert!(matches!(
            execution.attempts[1].outcome,
            SearchEngineAttemptOutcome::Unavailable {
                reason: SearchEngineUnavailability::ServerError,
            }
        ));
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::FallbackSearchFailed,
            }
        ));
    }

    #[tokio::test]
    async fn empty_query_never_reaches_searxng() {
        let (client, requests, server) = search_fixture(Vec::new()).await;

        let execution = client.search_web("   ").await;

        server.abort();
        assert!(requests.lock().unwrap().is_empty());
        assert!(execution.attempts.is_empty());
        assert!(matches!(
            execution.completion,
            WebSearchCompletion::Failed {
                reason: WebSearchFailureReason::InvalidQuery,
            }
        ));
    }

    #[tokio::test]
    async fn blocked_phishing_domain_is_rejected_before_dns() {
        for url in [
            "https://talk-doubao.com.cn/",
            "https://www.talk-doubao.com.cn/login",
        ] {
            let error = validate_public_web_url(url).await.unwrap_err();
            assert!(error.to_string().contains("domain is blocked"));
        }
        assert!(!BLOCKED_DOMAINS.contains(&"talk.doubao.com.cn"));
    }

    #[tokio::test]
    async fn literal_private_target_is_rejected_at_fetch_boundary() {
        let error = fetch_public_page("http://127.0.0.1:9/").await.unwrap_err();
        assert!(error.to_string().contains("non-public address 127.0.0.1"));
    }

    #[tokio::test]
    async fn page_body_reader_accepts_limit_and_rejects_one_more_byte() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/limit", get(|| async { vec![b'a'; MAX_PAGE_BYTES] }))
            .route(
                "/oversize",
                get(|| async { vec![b'a'; MAX_PAGE_BYTES + 1] }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let response = reqwest::get(format!("{base_url}/limit")).await.unwrap();
        assert_eq!(
            read_page_body(response, "fixture").await.unwrap().len(),
            MAX_PAGE_BYTES
        );
        let response = reqwest::get(format!("{base_url}/oversize")).await.unwrap();
        let error = read_page_body(response, "fixture").await.unwrap_err();

        server.abort();
        assert!(error.to_string().contains("exceeds 4000000 bytes"));
    }

    #[test]
    fn offline_html_cannot_load_subresources_or_scripts() {
        let safe = sanitize_for_offline_crawl(
            r#"<h1>Keep</h1><script>alert(1)</script><img src="http://127.0.0.1/x"><iframe src="https://example.com"></iframe><p style="background:url(https://example.com/x)">Text</p>"#,
        );
        assert!(safe.contains("<h1>Keep</h1>"));
        for forbidden in ["<script", "<img", "<iframe", "src=", "style="] {
            assert!(!safe.contains(forbidden), "retained {forbidden}: {safe}");
        }
    }

    #[test]
    fn special_addresses_are_not_public() {
        for raw in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.168.1.1",
            "198.18.0.1",
            "203.0.113.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(raw.parse().unwrap()), "accepted {raw}");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn searxng_language_follows_query_script() {
        assert_eq!(query_language("黑格尔哲学"), "zh-CN");
        assert_eq!(query_language("Hegel philosophy"), "en-US");
    }

    #[test]
    fn searxng_contract_maps_and_filters_results() {
        let json = br#"{"results":[{"title":"Skip","url":"ftp://example.com/x","engine":"google","engines":["google"]},{"title":"Alpha","url":"https://example.com/a","content":"One","engine":"google","engines":["google"]},{"title":"Beta","url":"http://example.com/b","engine":"google","engines":["google"]}],"unresponsive_engines":[]}"#;
        let ParsedSearxngEngineResults::Completed(results) =
            parse_searxng_engine_results(SearchEngine::Google, "query", json).unwrap()
        else {
            panic!("valid Google results were marked unresponsive");
        };
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].rank, 1);
        assert_eq!(results[1].rank, 2);
        assert_eq!(results[0].snippet, "One");
        assert!(results[1].snippet.is_empty());
        assert_eq!(
            results[0].search_result_id,
            crate::search_result_id("query", "https://example.com/a")
        );
    }

    #[test]
    fn searxng_contract_keeps_at_most_ten_results_with_contiguous_ranks() {
        let results = (0..12)
            .map(|index| {
                serde_json::json!({
                    "title": format!("Result {index}"),
                    "url": format!("https://example.com/{index}"),
                    "engine": "google",
                    "engines": ["google"],
                })
            })
            .collect::<Vec<_>>();
        let body = serde_json::to_vec(&serde_json::json!({
            "results": results,
            "unresponsive_engines": [],
        }))
        .unwrap();

        let ParsedSearxngEngineResults::Completed(results) =
            parse_searxng_engine_results(SearchEngine::Google, "query", &body).unwrap()
        else {
            panic!("valid Google results were marked unresponsive");
        };

        assert_eq!(results.len(), 10);
        assert_eq!(
            results.iter().map(|result| result.rank).collect::<Vec<_>>(),
            (1..=10).collect::<Vec<_>>()
        );
    }

    #[test]
    fn searxng_contract_accepts_proven_empty_and_rejects_unproven_payloads() {
        assert_eq!(
            parse_searxng_engine_results(
                SearchEngine::Google,
                "query",
                br#"{"results":[],"unresponsive_engines":[]}"#,
            )
            .map(|parsed| matches!(parsed, ParsedSearxngEngineResults::Completed(results) if results.is_empty())),
            Ok(true)
        );
        assert_eq!(
            parse_searxng_engine_results(SearchEngine::Google, "query", b"not json"),
            Err(SearchBoundaryContractFailure::InvalidResponse)
        );
        assert_eq!(
            parse_searxng_engine_results(SearchEngine::Google, "query", br#"{"results":[]}"#,),
            Err(SearchBoundaryContractFailure::InvalidResponse)
        );
        assert_eq!(
            parse_searxng_engine_results(
                SearchEngine::Google,
                "query",
                br#"{"results":[{"title":"Hidden","url":"ftp://example.com/x","engine":"bing","engines":["bing"]}],"unresponsive_engines":[]}"#,
            ),
            Err(SearchBoundaryContractFailure::EngineSelectionViolation)
        );
    }

    #[test]
    fn crawl_snapshot_records_meta_and_truncates_on_utf8_boundary() {
        let mut raw = "a".repeat(MAX_ARCHIVED_BODY_BYTES - 1);
        raw.push('界');
        let snapshot = snapshot_from_crawl(
            "https://example.com/original",
            "https://example.com/final".into(),
            200,
            CrawlEnvelope {
                success: true,
                results: vec![CrawlResult {
                    success: true,
                    error_message: String::new(),
                    metadata: serde_json::json!({"title": "Example", "language": "en"}),
                    markdown: CrawlMarkdown {
                        raw_markdown: raw,
                        fit_markdown: "fit".into(),
                    },
                }],
            },
        )
        .unwrap();

        assert_eq!(snapshot.title, "Example");
        assert_eq!(snapshot.body.len(), MAX_ARCHIVED_BODY_BYTES - 1);
        assert!(snapshot.crawl.truncated);
        assert_eq!(
            snapshot.crawl.raw_markdown_bytes,
            MAX_ARCHIVED_BODY_BYTES + 2
        );
        assert_eq!(snapshot.crawl.fit_markdown_bytes, 3);
        assert_eq!(snapshot.crawl.body_kind, Some(CrawlBodyKind::RawMarkdown));
        assert_eq!(snapshot.crawl.metadata["language"], "en");
    }

    #[test]
    fn parses_openai_json_content() {
        let response: ChatResponse =
            serde_json::from_str(r#"{"choices":[{"message":{"content":"{\"queries\":[]}"}}]}"#)
                .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&response.choices[0].message.content).unwrap();
        assert_eq!(value, serde_json::json!({"queries": []}));
    }
}

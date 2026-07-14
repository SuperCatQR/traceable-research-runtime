//! P3 external adapters: SearXNG/Bing, crawl4ai, an OpenAI-compatible strong model,
//! and the public-HTTP SSRF boundary shared by every page fetch.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use chrono::Utc;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::{Host, Url};

use crate::{CrawlBodyKind, CrawlMeta, Result, SearchError, SearchResult, Snapshot};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const SEARCH_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(5),
    Duration::from_secs(9),
];
const MAX_REDIRECTS: usize = 5;
const MAX_PAGE_BYTES: usize = 4_000_000;
const MAX_ARCHIVED_BODY_BYTES: usize = 4 * 1024 * 1024;
const BLOCKED_DOMAINS: &[&str] = &["talk-doubao.com.cn"];

/// Accept only public HTTP(S) URLs. Every DNS answer is checked: a hostname with
/// even one private/special address is rejected rather than gambling on which
/// address the fetcher chooses.
pub async fn validate_public_url(raw: &str) -> Result<Url> {
    resolve_public_url(raw).await.map(|(url, _)| url)
}

async fn resolve_public_url(raw: &str) -> Result<(Url, Option<(String, Vec<SocketAddr>)>)> {
    let url = Url::parse(raw).map_err(|error| SearchError::Ssrf {
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

fn ssrf(url: &str, reason: &str) -> SearchError {
    SearchError::Ssrf {
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
        .map_err(|error| SearchError::Search {
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
    builder.build().map_err(|error| SearchError::Fetch {
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
                .map_err(|error| SearchError::Fetch {
                    url: url.to_string(),
                    reason: error.to_string(),
                })?;
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(SearchError::Fetch {
                    url: url.to_string(),
                    reason: format!("more than {MAX_REDIRECTS} redirects"),
                });
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| SearchError::Fetch {
                    url: url.to_string(),
                    reason: "redirect has no Location header".into(),
                })?
                .to_str()
                .map_err(|error| SearchError::Fetch {
                    url: url.to_string(),
                    reason: format!("invalid redirect Location header: {error}"),
                })?;
            current = url
                .join(location)
                .map_err(|error| SearchError::Fetch {
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
    while let Some(chunk) = response.chunk().await.map_err(|error| SearchError::Fetch {
        url: url.to_owned(),
        reason: format!("failed to read response body: {error}"),
    })? {
        if chunk.len() > MAX_PAGE_BYTES - body.len() {
            return Err(SearchError::Fetch {
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

/// Thin client for a self-hosted SearXNG instance configured with Bing only.
pub struct SearxngClient {
    client: reqwest::Client,
    endpoint: Url,
}

impl SearxngClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let endpoint = Url::parse(base_url)
            .and_then(|base| base.join("search"))
            .map_err(|error| SearchError::Search {
                message: format!("invalid SearXNG endpoint: {error}"),
            })?;
        Ok(Self {
            client: http_client()?,
            endpoint,
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>> {
        self.search_with_delays(query, &SEARCH_RETRY_DELAYS).await
    }

    async fn search_with_delays(
        &self,
        query: &str,
        retry_delays: &[Duration],
    ) -> Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Err(SearchError::Search {
                message: "query is empty".into(),
            });
        }
        let mut endpoint = self.endpoint.clone();
        endpoint.query_pairs_mut().extend_pairs([
            ("q", query),
            ("format", "json"),
            ("categories", "general"),
            ("language", query_language(query)),
        ]);

        for attempt in 0..=retry_delays.len() {
            let response = self
                .client
                .get(endpoint.clone())
                .send()
                .await
                .map_err(|error| SearchError::Search {
                    message: error.to_string(),
                })?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|error| SearchError::Search {
                    message: error.to_string(),
                })?;
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || body_reports_rate_limit(&body) {
                if let Some(delay) = retry_delays.get(attempt) {
                    tokio::time::sleep(*delay).await;
                    continue;
                }
                return Err(SearchError::Search {
                    message: "SearXNG rate limit persisted after 4 retries".into(),
                });
            }
            if !status.is_success() {
                return Err(SearchError::Search {
                    message: format!("SearXNG returned HTTP {status}"),
                });
            }
            return parse_searxng_results(query, &body);
        }
        unreachable!("search retry loop always returns")
    }
}

fn body_reports_rate_limit(body: &[u8]) -> bool {
    if body.len() > 1024 {
        return false;
    }
    let body = String::from_utf8_lossy(body).to_ascii_lowercase();
    body.contains("ratelimit") || body.contains("rate limit") || body.contains("too many requests")
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
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

fn parse_searxng_results(query: &str, body: &[u8]) -> Result<Vec<SearchResult>> {
    let raw: SearxngEnvelope =
        serde_json::from_slice(body).map_err(|error| SearchError::Search {
            message: format!("invalid SearXNG JSON: {error}"),
        })?;
    let results: Vec<_> = raw
        .results
        .into_iter()
        .filter(|item| {
            Url::parse(&item.url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
        })
        .take(10)
        .enumerate()
        .map(|(index, item)| {
            SearchResult::new(query, item.title, item.content, item.url, index as u32 + 1)
        })
        .collect();
    if results.is_empty() {
        Err(SearchError::Search {
            message: "SearXNG/Bing returned no valid result".into(),
        })
    } else {
        Ok(results)
    }
}

/// Thin client for the crawl4ai `/crawl` API. The service endpoint may be on a
/// private network; only the untrusted page URL is subject to the SSRF guard.
pub struct CrawlClient {
    client: reqwest::Client,
    endpoint: Url,
    token: String,
}

impl CrawlClient {
    pub fn new(base_url: &str, token: impl Into<String>) -> Result<Self> {
        let endpoint = Url::parse(base_url)
            .and_then(|base| base.join("crawl"))
            .map_err(|error| SearchError::Fetch {
                url: base_url.to_owned(),
                reason: format!("invalid crawl4ai endpoint: {error}"),
            })?;
        Ok(Self {
            client: http_client()?,
            endpoint,
            token: token.into(),
        })
    }

    pub async fn crawl(&self, raw_url: &str) -> Result<Snapshot> {
        let fetched = fetch_public_page(raw_url).await?;
        let offline_url = format!("raw:{}", sanitize_for_offline_crawl(&fetched.html));
        let mut request = self.client.post(self.endpoint.clone()).json(&CrawlRequest {
            urls: [offline_url.as_str()],
        });
        if !self.token.is_empty() {
            request = request.bearer_auth(&self.token);
        }
        let response = request.send().await.map_err(|error| SearchError::Fetch {
            url: raw_url.to_owned(),
            reason: error.to_string(),
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(SearchError::Fetch {
                url: raw_url.to_owned(),
                reason: format!("crawl4ai returned HTTP {status}"),
            });
        }
        let envelope: CrawlEnvelope =
            response.json().await.map_err(|error| SearchError::Fetch {
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
        .ok_or_else(|| SearchError::Fetch {
            url: requested_url.to_owned(),
            reason: "crawl4ai returned no result".into(),
        })?;
    if !envelope.success || !result.success {
        return Err(SearchError::Fetch {
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
        return Err(SearchError::Fetch {
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
pub struct StrongClient {
    client: reqwest::Client,
    endpoint: Url,
    api_key: String,
    model: String,
}

impl StrongClient {
    pub fn new(
        base_url: &str,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let endpoint = Url::parse(base_url)
            .and_then(|base| base.join("chat/completions"))
            .map_err(|error| SearchError::ModelCall {
                message: format!("invalid model endpoint: {error}"),
            })?;
        Ok(Self {
            client: http_client()?,
            endpoint,
            api_key: api_key.into(),
            model: model.into(),
        })
    }

    pub async fn complete_text(&self, system: &str, user: &str) -> Result<String> {
        let mut request = self.client.post(self.endpoint.clone()).json(&ChatRequest {
            model: &self.model,
            messages: [
                ChatMessage {
                    role: "system",
                    content: system,
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            response_format: ResponseFormat {
                kind: "json_object",
            },
        });
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| SearchError::ModelCall {
                message: error.to_string(),
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| SearchError::ModelCall {
                message: error.to_string(),
            })?;
        if !status.is_success() {
            return Err(SearchError::ModelCall {
                // Upstream bodies may echo secrets or prompts; status is diagnostic enough.
                message: format!("model returned HTTP {status}"),
            });
        }
        let completion: ChatResponse =
            serde_json::from_str(&body).map_err(|error| SearchError::ModelOutput {
                message: format!("invalid completion envelope: {error}"),
            })?;
        let content = completion
            .choices
            .first()
            .ok_or_else(|| SearchError::ModelOutput {
                message: "completion has no choice".into(),
            })?
            .message
            .content
            .trim();
        Ok(content.to_owned())
    }

    pub async fn complete_json<T: DeserializeOwned>(&self, system: &str, user: &str) -> Result<T> {
        let content = self.complete_text(system, user).await?;
        serde_json::from_str(&content).map_err(|error| SearchError::ModelOutput {
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

    use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};

    use super::*;

    async fn retrying_search(State(attempts): State<Arc<AtomicUsize>>) -> impl IntoResponse {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            (StatusCode::OK, "ratelimit")
        } else {
            (
                StatusCode::OK,
                r#"{"results":[{"title":"Rust","url":"https://example.com/","content":"language"}]}"#,
            )
        }
    }

    async fn rate_limited_search(State(attempts): State<Arc<AtomicUsize>>) -> impl IntoResponse {
        attempts.fetch_add(1, Ordering::SeqCst);
        (StatusCode::TOO_MANY_REQUESTS, "too many requests")
    }

    async fn search_fixture(
        handler: axum::routing::MethodRouter<Arc<AtomicUsize>>,
    ) -> (SearxngClient, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/search", handler)
            .with_state(attempts.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (SearxngClient::new(&base_url).unwrap(), attempts, server)
    }

    #[tokio::test]
    async fn search_retries_rate_limit_body_then_succeeds() {
        let (client, attempts, server) = search_fixture(get(retrying_search)).await;

        let results = client
            .search_with_delays("rust", &[Duration::ZERO; 4])
            .await
            .unwrap();

        server.abort();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn search_stops_after_four_rate_limit_retries() {
        let (client, attempts, server) = search_fixture(get(rate_limited_search)).await;

        let error = client
            .search_with_delays("rust", &[Duration::ZERO; 4])
            .await
            .unwrap_err();

        server.abort();
        assert!(error.to_string().contains("persisted after 4 retries"));
        assert_eq!(attempts.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn blocked_phishing_domain_is_rejected_before_dns() {
        for url in [
            "https://talk-doubao.com.cn/",
            "https://www.talk-doubao.com.cn/login",
        ] {
            let error = validate_public_url(url).await.unwrap_err();
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
        let json = br#"{"results":[{"title":"Skip","url":"ftp://example.com/x"},{"title":"Alpha","url":"https://example.com/a","content":"One"},{"title":"Beta","url":"http://example.com/b"}]}"#;
        let results = parse_searxng_results("query", json).unwrap();
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
    fn searxng_rejects_bad_json_and_empty_results() {
        assert!(parse_searxng_results("query", b"not json").is_err());
        assert!(parse_searxng_results("query", br#"{"results":[]}"#).is_err());
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

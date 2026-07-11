//! P3 external adapters: Bing RSS, crawl4ai, an OpenAI-compatible strong model,
//! and the public-HTTP SSRF boundary shared by every page fetch.

use std::{net::IpAddr, time::Duration};

use chrono::Utc;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::{Host, Url};

use crate::{CrawlMeta, Result, SearchError, SearchResult, Snapshot};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Accept only public HTTP(S) URLs. Every DNS answer is checked: a hostname with
/// even one private/special address is rejected rather than gambling on which
/// address the downstream fetcher chooses.
pub async fn validate_public_url(raw: &str) -> Result<Url> {
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
    match host {
        Host::Ipv4(ip) => ensure_public(raw, IpAddr::V4(ip))?,
        Host::Ipv6(ip) => ensure_public(raw, IpAddr::V6(ip))?,
        Host::Domain(domain) => {
            let port = url.port_or_known_default().expect("http(s) has a port");
            let addresses = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|error| ssrf(raw, &format!("DNS lookup failed: {error}")))?;
            let mut found = false;
            for address in addresses {
                found = true;
                ensure_public(raw, address.ip())?;
            }
            if !found {
                return Err(ssrf(raw, "DNS returned no address"));
            }
        }
    }
    Ok(url)
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
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("traceable-search/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| SearchError::Search {
            message: format!("HTTP client setup failed: {error}"),
        })
}

/// Bing first-page search through its stable RSS representation; no API key or
/// HTML selector is required.
pub struct BingClient {
    client: reqwest::Client,
    endpoint: Url,
}

impl BingClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            endpoint: Url::parse("https://www.bing.com/search").expect("constant URL is valid"),
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Err(SearchError::Search {
                message: "query is empty".into(),
            });
        }
        let mut url = self.endpoint.clone();
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("format", "rss")
            .append_pair("count", "10");
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(search_transport)?;
        let status = response.status();
        let body = response.text().await.map_err(search_transport)?;
        if !status.is_success() {
            return Err(SearchError::Search {
                message: format!("Bing returned HTTP {status}"),
            });
        }
        parse_bing_rss(query, &body)
    }
}

fn search_transport(error: reqwest::Error) -> SearchError {
    SearchError::Search {
        message: error.to_string(),
    }
}

#[derive(Deserialize)]
struct Rss {
    channel: RssChannel,
}

#[derive(Deserialize)]
struct RssChannel {
    #[serde(rename = "item", default)]
    items: Vec<RssItem>,
}

#[derive(Deserialize)]
struct RssItem {
    title: String,
    link: String,
    #[serde(default)]
    description: String,
}

fn parse_bing_rss(query: &str, body: &str) -> Result<Vec<SearchResult>> {
    let rss: Rss = quick_xml::de::from_str(body).map_err(|error| SearchError::Search {
        message: format!("invalid Bing RSS: {error}"),
    })?;
    let results: Vec<_> = rss
        .channel
        .items
        .into_iter()
        .take(10)
        .enumerate()
        .filter(|(_, item)| Url::parse(&item.link).is_ok())
        .map(|(index, item)| {
            SearchResult::new(
                query,
                item.title,
                item.description,
                item.link,
                index as u32 + 1,
            )
        })
        .collect();
    if results.is_empty() {
        Err(SearchError::Search {
            message: "Bing returned no valid result".into(),
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
        let requested = validate_public_url(raw_url).await?;
        let mut request = self.client.post(self.endpoint.clone()).json(&CrawlRequest {
            urls: [requested.as_str()],
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
        snapshot_from_crawl(raw_url, envelope).await
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
    url: String,
    #[serde(default)]
    redirected_url: String,
    #[serde(default)]
    status_code: u16,
    #[serde(default)]
    error_message: String,
    #[serde(default)]
    metadata: CrawlMetadata,
    #[serde(default)]
    markdown: CrawlMarkdown,
}

#[derive(Default, Deserialize)]
struct CrawlMetadata {
    #[serde(default)]
    title: String,
}

#[derive(Default, Deserialize)]
struct CrawlMarkdown {
    #[serde(default)]
    raw_markdown: String,
    #[serde(default)]
    fit_markdown: String,
}

async fn snapshot_from_crawl(requested_url: &str, mut envelope: CrawlEnvelope) -> Result<Snapshot> {
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
    let body = if result.raw_body().trim().is_empty() {
        return Err(SearchError::Fetch {
            url: requested_url.to_owned(),
            reason: "crawl4ai returned empty markdown".into(),
        });
    } else {
        result.raw_body().to_owned()
    };
    let final_url = if result.redirected_url.is_empty() {
        result.url
    } else {
        result.redirected_url
    };
    validate_public_url(&final_url).await?;
    Ok(Snapshot::new(
        requested_url.to_owned(),
        result.metadata.title,
        body,
        CrawlMeta {
            final_url,
            http_status: result.status_code,
            fetched_at: Utc::now(),
        },
    ))
}

impl CrawlResult {
    fn raw_body(&self) -> &str {
        if self.markdown.raw_markdown.trim().is_empty() {
            &self.markdown.fit_markdown
        } else {
            &self.markdown.raw_markdown
        }
    }
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

    pub async fn complete_json<T: DeserializeOwned>(&self, system: &str, user: &str) -> Result<T> {
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
                message: format!("model returned HTTP {status}: {}", truncate(&body, 300)),
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
        serde_json::from_str(content).map_err(|error| SearchError::ModelOutput {
            message: format!("invalid JSON content: {error}"),
        })
    }
}

fn truncate(value: &str, max: usize) -> &str {
    value.get(..max).unwrap_or(value)
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
    use super::*;

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
    fn parses_bing_fixture_and_derives_ids() {
        let rss = r#"<rss><channel><item><title>Alpha</title><link>https://example.com/a</link><description>One</description></item><item><title>Beta</title><link>https://example.com/b</link><description>Two</description></item></channel></rss>"#;
        let results = parse_bing_rss("query", rss).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].rank, 1);
        assert_eq!(
            results[0].search_result_id,
            crate::search_result_id("query", "https://example.com/a")
        );
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

//! Safe public-page capture and deterministic in-process snapshot extraction.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Utc;
use htmd::{Element, HtmlToMarkdown, element_handler::Handlers};
use url::{Host, Url};

use crate::{CrawlBodyKind, CrawlMeta, ResearchError, Result, Snapshot};

const MAX_ARCHIVED_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 4_000_000;
const MAX_REDIRECTS: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const BLOCKED_DOMAINS: &[&str] = &["talk-doubao.com.cn"];

#[derive(Debug, Clone)]
struct FetchedPage {
    final_url: String,
    status: u16,
    html: String,
}

type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<FetchedPage>> + Send + 'a>>;

trait PageFetcher: Send + Sync {
    fn fetch<'a>(&'a self, raw_url: &'a str) -> FetchFuture<'a>;
}

struct PublicHttpFetcher;

impl PageFetcher for PublicHttpFetcher {
    fn fetch<'a>(&'a self, raw_url: &'a str) -> FetchFuture<'a> {
        Box::pin(async move { fetch_public_page(raw_url).await })
    }
}

/// Accept only public HTTP(S) URLs. Every DNS answer must be public.
pub async fn validate_public_web_url(raw: &str) -> Result<Url> {
    resolve_public_url(raw).await.map(|(url, _)| url)
}

pub(crate) async fn resolve_public_url(
    raw: &str,
) -> Result<(Url, Option<(String, Vec<SocketAddr>)>)> {
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

/// Captures a public page and turns it into an immutable, content-addressed Snapshot.
pub struct EmbeddedSnapshotClient {
    page_fetcher: Box<dyn PageFetcher>,
}

impl EmbeddedSnapshotClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            page_fetcher: Box::new(PublicHttpFetcher),
        }
    }

    pub async fn capture_web_snapshot(&self, raw_url: &str) -> Result<Snapshot> {
        let fetched = self.page_fetcher.fetch(raw_url).await?;
        snapshot_from_fetched_page(raw_url, fetched)
    }

    #[cfg(test)]
    fn with_page_fetcher(page_fetcher: impl PageFetcher + 'static) -> Self {
        Self {
            page_fetcher: Box::new(page_fetcher),
        }
    }
}

impl Default for EmbeddedSnapshotClient {
    fn default() -> Self {
        Self::new()
    }
}

fn snapshot_from_fetched_page(requested_url: &str, fetched: FetchedPage) -> Result<Snapshot> {
    let title = extract_title(&fetched.html).map_err(|error| ResearchError::Fetch {
        url: requested_url.to_owned(),
        reason: format!("embedded title extraction failed: {error}"),
    })?;
    let safe_html = sanitize_for_snapshot(&fetched.html);
    let markdown = htmd::convert(&safe_html)
        .map_err(|error| ResearchError::Fetch {
            url: requested_url.to_owned(),
            reason: format!("embedded HTML-to-Markdown conversion failed: {error}"),
        })?
        .trim()
        .to_owned();
    if markdown.is_empty() {
        return Err(ResearchError::Fetch {
            url: requested_url.to_owned(),
            reason: "embedded extraction returned empty markdown".into(),
        });
    }
    let markdown_bytes = markdown.len();
    let (body, truncated) = truncate_utf8(&markdown, MAX_ARCHIVED_BODY_BYTES);
    Ok(Snapshot::new(
        requested_url.to_owned(),
        title,
        body,
        CrawlMeta {
            final_url: fetched.final_url,
            http_status: fetched.status,
            fetched_at: Utc::now(),
            metadata: serde_json::json!({
                "extractor": "embedded_html_to_markdown",
                "schema_version": 1,
            }),
            raw_markdown_bytes: markdown_bytes,
            fit_markdown_bytes: 0,
            body_kind: Some(CrawlBodyKind::RawMarkdown),
            truncated,
        },
    ))
}

fn extract_title(html: &str) -> std::io::Result<String> {
    let title = Arc::new(Mutex::new(String::new()));
    let captured_title = Arc::clone(&title);
    let converter = HtmlToMarkdown::builder()
        .add_handler(
            vec!["title"],
            move |handlers: &dyn Handlers, element: Element<'_>| {
                let candidate = handlers
                    .walk_children(element.node)
                    .content
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !candidate.is_empty()
                    && let Ok(mut current) = captured_title.lock()
                    && current.is_empty()
                {
                    *current = candidate;
                }
                None
            },
        )
        .build();
    converter.convert(html)?;
    let title = title
        .lock()
        .map_err(|_| std::io::Error::other("title capture lock poisoned"))?
        .clone();
    Ok(title)
}

fn sanitize_for_snapshot(html: &str) -> String {
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(HashSet::from([
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
        .tag_attributes(HashMap::new())
        .generic_attributes(HashSet::new())
        .url_schemes(HashSet::new());
    builder.clean(html).to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};

    struct FixturePageFetcher {
        page: FetchedPage,
    }

    impl PageFetcher for FixturePageFetcher {
        fn fetch<'a>(&'a self, _raw_url: &'a str) -> FetchFuture<'a> {
            let page = self.page.clone();
            Box::pin(async move { Ok(page) })
        }
    }

    #[tokio::test]
    async fn embedded_snapshot_converts_safe_html_to_markdown() {
        let client = EmbeddedSnapshotClient::with_page_fetcher(FixturePageFetcher {
            page: FetchedPage {
                final_url: "https://example.com/final".into(),
                status: 200,
                html: "<h1>Research</h1><script>alert(1)</script><p>Hello world.</p>".into(),
            },
        });

        let snapshot = client
            .capture_web_snapshot("https://example.com/original")
            .await
            .unwrap();

        assert_eq!(snapshot.body, "# Research\n\nHello world.");
    }

    #[tokio::test]
    async fn embedded_snapshot_preserves_the_page_title() {
        let client = EmbeddedSnapshotClient::with_page_fetcher(FixturePageFetcher {
            page: FetchedPage {
                final_url: "https://example.com/final".into(),
                status: 200,
                html: "<html><head><title>  Example 研究  </title></head><body><p>Evidence</p></body></html>".into(),
            },
        });

        let snapshot = client
            .capture_web_snapshot("https://example.com/original")
            .await
            .unwrap();

        assert_eq!(snapshot.title, "Example 研究");
    }

    #[tokio::test]
    async fn embedded_snapshot_rejects_literal_private_targets_before_connecting() {
        let error = EmbeddedSnapshotClient::new()
            .capture_web_snapshot("http://127.0.0.1:9/")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("non-public address 127.0.0.1"));
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
    fn sanitized_html_cannot_load_subresources_or_scripts() {
        let safe = sanitize_for_snapshot(
            r#"<h1>Keep</h1><script>alert(1)</script><img src="http://127.0.0.1/x"><iframe src="https://example.com"></iframe><p style="background:url(https://example.com/x)">Text</p>"#,
        );
        assert!(safe.contains("<h1>Keep</h1>"));
        for forbidden in ["<script", "<img", "<iframe", "src=", "style="] {
            assert!(!safe.contains(forbidden), "retained {forbidden}: {safe}");
        }
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

    #[tokio::test]
    async fn embedded_snapshot_truncates_on_a_utf8_boundary() {
        let mut source = "a".repeat(MAX_ARCHIVED_BODY_BYTES - 1);
        source.push('界');
        let client = EmbeddedSnapshotClient::with_page_fetcher(FixturePageFetcher {
            page: FetchedPage {
                final_url: "https://example.com/final".into(),
                status: 200,
                html: format!("<p>{source}</p>"),
            },
        });

        let snapshot = client
            .capture_web_snapshot("https://example.com/original")
            .await
            .unwrap();

        assert_eq!(snapshot.body.len(), MAX_ARCHIVED_BODY_BYTES - 1);
        assert!(snapshot.crawl.truncated);
        assert_eq!(
            snapshot.crawl.raw_markdown_bytes,
            MAX_ARCHIVED_BODY_BYTES + 2
        );
    }
}

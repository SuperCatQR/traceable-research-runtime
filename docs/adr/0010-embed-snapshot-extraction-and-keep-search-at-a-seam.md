# Embed snapshot extraction and keep search at a seam

- Status: Accepted
- Date: 2026-07-18

## Context

The runtime already fetched public pages in Rust with DNS pinning, SSRF checks, bounded redirects, and response-size limits. It then sent sanitized offline HTML to crawl4ai only to obtain Markdown. This made deployment and live testing depend on a Python/Playwright container without using its browser or remote-crawl capabilities. Search remains a true external dependency, now provided by the Brave Search API.

## Decision

`EmbeddedSnapshotClient` owns public-page validation, fetching, sanitization, title extraction, HTML-to-Markdown conversion, UTF-8-safe truncation, and Snapshot construction behind `capture_web_snapshot(url)`. crawl4ai is no longer a runtime dependency.

Search remains at the one-method `WebSearch` seam. `BraveSearchClient` is the production Adapter and deterministic fixtures provide the test Adapter. Each provider attempt and its outcome are recorded in `WebSearchExecution` and Trace.

The repository provides a declarative Compose topology for the App and its persistent data volume. The search provider is accessed over HTTPS with a server-side API key; environment-specific replacement and rollback automation remains outside the repository.

## Compatibility

Research Trace remains v7. Existing Snapshot SQLite rows and serialized `crawl` metadata remain readable; `CrawlMeta` and `CrawlBodyKind` are retained as legacy wire names. Newly captured bodies can produce different content hashes because the Markdown implementation changed, but existing immutable Snapshots are not rewritten.

## Consequences

- Routine Core and Host tests do not require crawl4ai, Python, Playwright, or a browser image.
- Self-hosted deployment has one runtime container; Brave Search is consumed as
  a managed HTTPS API rather than a local SearXNG service.
- JavaScript-rendered capture is not added; a future browser Adapter must be optional and must preserve the same Snapshot interface.
- A managed search API was selected after comparing Brave, Tavily, Exa, and proxy-based Google results; Brave is the primary provider and the current Trace seam records it explicitly.

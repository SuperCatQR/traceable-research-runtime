//! traceable-search — an auditable web-research pipeline exposed as an MCP service.
//!
//! Design is frozen in `docs/web-search-architecture.md`. The build lands in phases:
//! P1 domain types + `error_class`, P2 dual persistence (snapshot.sqlite + trace JSONL),
//! P3 external adapters (Bing / crawl4ai / strong model) behind an SSRF guard,
//! P4 orchestration (fixed 3-round explore + synthesize; three pure functions),
//! P5 the `rmcp` server surface, P6 the six program validations + E2E.
//!
//! The crate is split lib + bin so the three pure functions (`plan_queries`,
//! `select_sources`, `synthesize_answer`) stay fixture-testable without a runtime.

pub mod error;
pub mod types;

// Flat public surface: downstream phases import from the crate root, not deep
// module paths. The frozen P1 contract is exactly what's re-exported here.
pub use error::{ErrorClass, Result, SearchError};
pub use types::{
    content_hash, search_result_id, snapshot_id, snapshot_ref, Answer, Claim, CrawlMeta, Excerpt,
    Query, SearchResult, Snapshot, SnapshotRef,
};

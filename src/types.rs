//! Frozen domain types and the content-addressing rules they depend on.
//!
//! Every id here is derived, never assigned: the same inputs must always
//! produce the same id so a snapshot can be re-verified against its recorded
//! `content_hash` (§6 validation 4). The known-answer tests at the bottom lock
//! the exact formulas — separator, hash algorithm, prefix, and truncation — so
//! a later edit that drifts the contract fails loudly instead of silently
//! renumbering everything downstream.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Content-addressing rules (§3.1 / §3.3). Free functions: pure, referenced
// from both construction and re-verification, so they don't belong to a type.
// ---------------------------------------------------------------------------

/// `sha1(query|url)[:12]` — stable id for one Bing hit within a run (§3.1).
#[must_use]
pub fn search_result_id(query: &str, url: &str) -> String {
    let mut h = Sha1::new();
    h.update(query.as_bytes());
    h.update(b"|");
    h.update(url.as_bytes());
    hex::encode(h.finalize())[..12].to_string()
}

/// `"sha256:" + sha256(text)` — the body fingerprint every snapshot is keyed
/// and re-verified by (§3.3). Prefix names the algorithm so the store is not
/// locked to one hash forever.
#[must_use]
pub fn content_hash(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// `sha1(final_url|content_hash)[:16]` — snapshot id binding the landing URL to
/// its exact body (§3.3). Two fetches of the same URL with different bodies get
/// different ids; identical bodies collapse to one.
#[must_use]
pub fn snapshot_id(final_url: &str, content_hash: &str) -> String {
    let mut h = Sha1::new();
    h.update(final_url.as_bytes());
    h.update(b"|");
    h.update(content_hash.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

/// `"snapshot:web/<snapshot_id>"` — the reference form carried through
/// selection, Claims, and the trace (§3.3).
#[must_use]
pub fn snapshot_ref(snapshot_id: &str) -> String {
    format!("snapshot:web/{snapshot_id}")
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// A reference to a stored snapshot: `"snapshot:web/<id>"`. Newtype so a raw
/// URL or arbitrary string can't be passed where a snapshot reference is
/// expected; serializes transparently as the bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotRef(pub String);

impl SnapshotRef {
    /// Build the reference for a snapshot id (§3.3).
    #[must_use]
    pub fn from_id(id: &str) -> Self {
        Self(snapshot_ref(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One query the strong model emits per explore round, with the evidence gap
/// it is meant to close (§5.1 schema; §7 query rationale).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub query: String,
    pub gap: String,
}

/// One Bing first-page hit (§3 web_search). `search_result_id` is derived from
/// the issuing query + URL, so archiving can be gated to this run (§6 v2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub search_result_id: String,
    pub title: String,
    pub snippet: String,
    pub url: String,
    pub rank: u32,
}

impl SearchResult {
    /// Build a hit, deriving `search_result_id` from the query that produced it.
    #[must_use]
    pub fn new(query: &str, title: String, snippet: String, url: String, rank: u32) -> Self {
        Self {
            search_result_id: search_result_id(query, &url),
            title,
            snippet,
            url,
            rank,
        }
    }
}

/// Fetch provenance recorded alongside a snapshot (§8 抓取凭证和时间).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlMeta {
    /// URL after redirects — the SSRF guard re-checks this (§10).
    pub final_url: String,
    pub http_status: u16,
    pub fetched_at: DateTime<Utc>,
}

/// An immutable archived page — the sole source of truth for the final answer
/// (§5 snapshot_body). Body is never mutated; `content_hash` and `snapshot_id`
/// are derived from it so any later drift is detectable (§6 v4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub snapshot_id: String,
    pub snapshot_ref: SnapshotRef,
    /// The URL we asked for, before redirects — kept for the audit trail.
    pub requested_url: String,
    pub title: String,
    pub body: String,
    pub content_hash: String,
    pub crawl: CrawlMeta,
}

impl Snapshot {
    /// Archive a fetched page, deriving `content_hash` → `snapshot_id` →
    /// `snapshot_ref` from the body and landing URL (§3.3). This is the only
    /// place those three are minted, so they always agree.
    #[must_use]
    pub fn new(requested_url: String, title: String, body: String, crawl: CrawlMeta) -> Self {
        let content_hash = content_hash(&body);
        let snapshot_id = snapshot_id(&crawl.final_url, &content_hash);
        let snapshot_ref = SnapshotRef::from_id(&snapshot_id);
        Self {
            snapshot_id,
            snapshot_ref,
            requested_url,
            title,
            body,
            content_hash,
            crawl,
        }
    }
}

/// Deterministic navigation material (title + first paragraph + URL) the strong
/// model reads to pick pages. Explicitly *not* evidence — Claims cite the
/// snapshot body, never the excerpt (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excerpt {
    pub snapshot_ref: SnapshotRef,
    pub title: String,
    pub excerpt: String,
}

/// A single fact with the snapshot(s) it rests on. Every Claim must cite at
/// least one `snapshot_ref` that was actually fed to the final call (§5.3 / §6
/// v6); enforcement lives in P4, the shape is frozen here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub text: String,
    pub snapshot_refs: Vec<SnapshotRef>,
}

/// The final answer: prose plus the Claims that source it (§5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub answer: String,
    pub claims: Vec<Claim>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known-answer vectors — computed independently and pinned. If a formula
    // drifts (separator, prefix, truncation length), these break loudly.
    const Q: &str = "2024 nobel physics";
    const U: &str = "https://example.com/page";
    const FINAL_URL: &str = "https://example.com/final";

    #[test]
    fn search_result_id_is_pinned() {
        assert_eq!(search_result_id(Q, U), "6b044c73d025");
    }

    #[test]
    fn content_hash_is_pinned() {
        assert_eq!(
            content_hash("hello world"),
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn snapshot_id_and_ref_are_pinned() {
        let ch = content_hash("hello world");
        let id = snapshot_id(FINAL_URL, &ch);
        assert_eq!(id, "d0790216187faeb6");
        assert_eq!(snapshot_ref(&id), "snapshot:web/d0790216187faeb6");
    }

    #[test]
    fn snapshot_new_derives_agreeing_ids() {
        let crawl = CrawlMeta {
            final_url: FINAL_URL.to_string(),
            http_status: 200,
            fetched_at: Utc::now(),
        };
        let snap = Snapshot::new(U.to_string(), "T".into(), "hello world".into(), crawl);
        assert_eq!(snap.content_hash, content_hash("hello world"));
        assert_eq!(snap.snapshot_id, "d0790216187faeb6");
        assert_eq!(snap.snapshot_ref.as_str(), "snapshot:web/d0790216187faeb6");
        // requested_url and final_url differ; both are retained for audit.
        assert_eq!(snap.requested_url, U);
        assert_eq!(snap.crawl.final_url, FINAL_URL);
    }

    #[test]
    fn search_result_new_derives_id() {
        let r = SearchResult::new(Q, "T".into(), "snip".into(), U.to_string(), 1);
        assert_eq!(r.search_result_id, "6b044c73d025");
    }

    #[test]
    fn snapshot_ref_serializes_transparently() {
        let r = SnapshotRef::from_id("d0790216187faeb6");
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            "\"snapshot:web/d0790216187faeb6\""
        );
    }

    #[test]
    fn answer_json_roundtrips() {
        let ans = Answer {
            answer: "42".into(),
            claims: vec![Claim {
                text: "the answer".into(),
                snapshot_refs: vec![SnapshotRef::from_id("d0790216187faeb6")],
            }],
        };
        let json = serde_json::to_string(&ans).unwrap();
        let back: Answer = serde_json::from_str(&json).unwrap();
        assert_eq!(ans, back);
    }
}

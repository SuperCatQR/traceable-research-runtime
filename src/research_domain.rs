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
/// selection, composed claims, and the trace (§3.3).
#[must_use]
pub fn snapshot_ref(snapshot_id: &str) -> String {
    format!("snapshot:web/{snapshot_id}")
}

// ---------------------------------------------------------------------------
// Research Intake domain types
// ---------------------------------------------------------------------------

pub const RESEARCH_BRIEF_SCHEMA_VERSION: u32 = 1;
pub const MAX_QUESTION_CHARS: usize = 10_000;
pub const MAX_BRIEF_STRING_CHARS: usize = 4_000;
pub const MAX_BRIEF_ARRAY_ITEMS: usize = 32;
pub const MAX_BRIEF_ARRAY_ITEM_CHARS: usize = 2_000;
pub const MIN_DECISION_RATIONALE_CHARS: usize = 8;
pub const MAX_DECISION_RATIONALE_CHARS: usize = 480;

pub fn validate_decision_rationale(value: &str) -> std::result::Result<(), String> {
    let char_count = value.trim().chars().count();
    if !(MIN_DECISION_RATIONALE_CHARS..=MAX_DECISION_RATIONALE_CHARS).contains(&char_count) {
        return Err(format!(
            "decision rationale must contain {MIN_DECISION_RATIONALE_CHARS}..={MAX_DECISION_RATIONALE_CHARS} characters"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RationaleAuditStatus {
    LegacyUnverified,
    RequiredAndValidated,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResearchScope {
    pub time_range: Option<String>,
    pub geography: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchBrief {
    pub schema_version: u32,
    pub original_question: String,
    pub research_question: String,
    pub desired_output: Option<String>,
    pub scope: ResearchScope,
    pub source_constraints: Vec<String>,
    pub accepted_assumptions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BriefValidationError {
    #[error("unsupported research brief schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("{field} exceeds {max} characters")]
    TooLong { field: &'static str, max: usize },
    #[error("{field} exceeds {max} items")]
    TooManyItems { field: &'static str, max: usize },
    #[error("{field}[{index}] must not be empty")]
    EmptyArrayItem { field: &'static str, index: usize },
    #[error("original_question cannot be changed")]
    OriginalQuestionChanged,
    #[error("research brief is not canonical")]
    NonCanonical,
    #[error("research brief content hash mismatch: expected {expected}, got {actual}")]
    ContentHashMismatch { expected: String, actual: String },
}

impl ResearchBrief {
    /// Trim and validate a draft while pinning its immutable original question.
    pub fn normalized(
        mut self,
        expected_original_question: &str,
    ) -> std::result::Result<Self, BriefValidationError> {
        if self.schema_version != RESEARCH_BRIEF_SCHEMA_VERSION {
            return Err(BriefValidationError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }

        let expected_original = normalize_required(
            "original_question",
            expected_original_question,
            MAX_QUESTION_CHARS,
        )?;
        let supplied_original = normalize_required(
            "original_question",
            &self.original_question,
            MAX_QUESTION_CHARS,
        )?;
        if supplied_original != expected_original {
            return Err(BriefValidationError::OriginalQuestionChanged);
        }

        self.original_question = expected_original;
        self.research_question = normalize_required(
            "research_question",
            &self.research_question,
            MAX_QUESTION_CHARS,
        )?;
        self.desired_output = normalize_optional("desired_output", self.desired_output)?;
        self.scope.time_range = normalize_optional("scope.time_range", self.scope.time_range)?;
        self.scope.geography = normalize_optional("scope.geography", self.scope.geography)?;
        self.scope.include = normalize_list("scope.include", self.scope.include)?;
        self.scope.exclude = normalize_list("scope.exclude", self.scope.exclude)?;
        self.source_constraints = normalize_list("source_constraints", self.source_constraints)?;
        self.accepted_assumptions =
            normalize_list("accepted_assumptions", self.accepted_assumptions)?;
        Ok(self)
    }

    pub fn content_hash(&self) -> std::result::Result<String, BriefValidationError> {
        let normalized = self.clone().normalized(&self.original_question)?;
        Ok(hash_normalized_brief(&normalized))
    }
}

fn normalize_required(
    field: &'static str,
    value: &str,
    max: usize,
) -> std::result::Result<String, BriefValidationError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(BriefValidationError::Empty(field));
    }
    if value.chars().count() > max {
        return Err(BriefValidationError::TooLong { field, max });
    }
    Ok(value.to_owned())
}

fn normalize_optional(
    field: &'static str,
    value: Option<String>,
) -> std::result::Result<Option<String>, BriefValidationError> {
    value
        .map(|value| {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else if value.chars().count() > MAX_BRIEF_STRING_CHARS {
                Err(BriefValidationError::TooLong {
                    field,
                    max: MAX_BRIEF_STRING_CHARS,
                })
            } else {
                Ok(Some(value.to_owned()))
            }
        })
        .unwrap_or(Ok(None))
}

fn normalize_list(
    field: &'static str,
    values: Vec<String>,
) -> std::result::Result<Vec<String>, BriefValidationError> {
    if values.len() > MAX_BRIEF_ARRAY_ITEMS {
        return Err(BriefValidationError::TooManyItems {
            field,
            max: MAX_BRIEF_ARRAY_ITEMS,
        });
    }
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value.trim();
            if value.is_empty() {
                return Err(BriefValidationError::EmptyArrayItem { field, index });
            }
            if value.chars().count() > MAX_BRIEF_ARRAY_ITEM_CHARS {
                return Err(BriefValidationError::TooLong {
                    field,
                    max: MAX_BRIEF_ARRAY_ITEM_CHARS,
                });
            }
            Ok(value.to_owned())
        })
        .collect()
}

fn hash_normalized_brief(brief: &ResearchBrief) -> String {
    let json = serde_json::to_vec(brief).expect("ResearchBrief serialization cannot fail");
    let mut hash = Sha256::new();
    hash.update(json);
    format!("sha256:{}", hex::encode(hash.finalize()))
}

/// A frozen brief has no mutable fields or setters. Construction verifies the
/// model-approved content hash; deserialization repeats that check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrozenResearchBrief {
    brief: ResearchBrief,
    clarification_id: String,
    content_hash: String,
    frozen_at: DateTime<Utc>,
}

impl FrozenResearchBrief {
    pub fn new(
        brief: ResearchBrief,
        expected_original_question: &str,
        clarification_id: String,
        expected_content_hash: &str,
        frozen_at: DateTime<Utc>,
    ) -> std::result::Result<Self, BriefValidationError> {
        let brief = brief.normalized(expected_original_question)?;
        let actual = hash_normalized_brief(&brief);
        if actual != expected_content_hash {
            return Err(BriefValidationError::ContentHashMismatch {
                expected: expected_content_hash.to_owned(),
                actual,
            });
        }
        if clarification_id.trim().is_empty() {
            return Err(BriefValidationError::Empty("clarification_id"));
        }
        Ok(Self {
            brief,
            clarification_id,
            content_hash: expected_content_hash.to_owned(),
            frozen_at,
        })
    }

    #[must_use]
    pub fn brief(&self) -> &ResearchBrief {
        &self.brief
    }

    #[must_use]
    pub fn clarification_id(&self) -> &str {
        &self.clarification_id
    }

    #[must_use]
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    #[must_use]
    pub const fn frozen_at(&self) -> &DateTime<Utc> {
        &self.frozen_at
    }
}

#[derive(Deserialize)]
struct FrozenResearchBriefWire {
    brief: ResearchBrief,
    clarification_id: String,
    content_hash: String,
    frozen_at: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for FrozenResearchBrief {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = FrozenResearchBriefWire::deserialize(deserializer)?;
        let normalized = wire
            .brief
            .clone()
            .normalized(&wire.brief.original_question)
            .map_err(serde::de::Error::custom)?;
        if normalized != wire.brief {
            return Err(serde::de::Error::custom(BriefValidationError::NonCanonical));
        }
        Self::new(
            wire.brief.clone(),
            &wire.brief.original_question,
            wire.clarification_id,
            &wire.content_hash,
            wire.frozen_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Existing research domain types
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
pub struct SearchQuery {
    pub query: String,
    pub gap: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchEngine {
    Google,
    Bing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchEngineUnavailability {
    TransportFailure,
    RequestTimeout,
    RateLimited,
    ServerError,
    EngineUnresponsive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchBoundaryContractFailure {
    EmptyQuery,
    UnexpectedHttpStatus,
    InvalidResponse,
    EngineSelectionViolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SearchEngineAttemptOutcome {
    Completed {
        valid_result_count: u32,
    },
    Unavailable {
        reason: SearchEngineUnavailability,
    },
    ContractRejected {
        reason: SearchBoundaryContractFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchEngineAttempt {
    pub engine: SearchEngine,
    pub outcome: SearchEngineAttemptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchFailureReason {
    InvalidQuery,
    PrimarySearchContractRejected,
    FallbackSearchFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationStopReason {
    CompletedRounds,
    InputBudget,
    SnapshotLimit,
    NoNewUrls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WebSearchCompletion {
    Completed {
        selected_engine: SearchEngine,
        results: Vec<SearchResult>,
    },
    Failed {
        reason: WebSearchFailureReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchExecution {
    pub attempts: Vec<SearchEngineAttempt>,
    pub completion: WebSearchCompletion,
}

/// One SearXNG first-page hit (§3 web_search). `search_result_id` is derived from
/// the issuing query + URL, so archiving can be gated to this run (§6 v2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub search_engine: SearchEngine,
    pub search_result_id: String,
    pub title: String,
    pub snippet: String,
    pub url: String,
    pub rank: u32,
}

impl SearchResult {
    /// Build a hit, deriving `search_result_id` from the query that produced it.
    #[must_use]
    pub fn new(
        search_engine: SearchEngine,
        query: &str,
        title: String,
        snippet: String,
        url: String,
        rank: u32,
    ) -> Self {
        Self {
            search_engine,
            search_result_id: search_result_id(query, &url),
            title,
            snippet,
            url,
            rank,
        }
    }
}

/// Which crawl4ai markdown representation became the archived body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrawlBodyKind {
    RawMarkdown,
    FitMarkdown,
}

/// Fetch provenance recorded alongside a snapshot (§8 抓取凭证和时间).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlMeta {
    /// URL after redirects — the SSRF guard re-checks this (§10).
    pub final_url: String,
    pub http_status: u16,
    pub fetched_at: DateTime<Utc>,
    /// Page metadata returned by crawl4ai.
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub raw_markdown_bytes: usize,
    #[serde(default)]
    pub fit_markdown_bytes: usize,
    pub body_kind: Option<CrawlBodyKind>,
    /// True when the selected body exceeded the archive limit.
    #[serde(default)]
    pub truncated: bool,
}

impl CrawlMeta {
    /// Minimal provenance for non-crawl4ai fixtures and legacy callers.
    #[must_use]
    pub fn basic(final_url: String, http_status: u16, fetched_at: DateTime<Utc>) -> Self {
        Self {
            final_url,
            http_status,
            fetched_at,
            metadata: serde_json::Value::Null,
            raw_markdown_bytes: 0,
            fit_markdown_bytes: 0,
            body_kind: None,
            truncated: false,
        }
    }
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
/// model reads to pick pages. Explicitly *not* evidence — composed claims cite the
/// snapshot body, never the excerpt (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotNavigationExcerpt {
    pub snapshot_ref: SnapshotRef,
    pub content_hash: String,
    pub title: String,
    pub excerpt: String,
}

/// Controls the balance between model knowledge and newly retrieved Web evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchAnswerStyle {
    #[default]
    WebFirst,
    KnowledgeFirst,
}

impl ResearchAnswerStyle {
    #[must_use]
    pub const fn knowledge_weight_percent(self) -> u8 {
        match self {
            Self::WebFirst => 20,
            Self::KnowledgeFirst => 80,
        }
    }

    #[must_use]
    pub const fn web_weight_percent(self) -> u8 {
        100 - self.knowledge_weight_percent()
    }
}

/// A model-only answer generated before it can inspect this run's Web evidence.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelKnowledgeDraft {
    pub answer: String,
    pub claims: Vec<String>,
    pub uncertainty: String,
    #[serde(default)]
    pub basis_summary: String,
}

/// The declared provenance of a final answer claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchClaimOrigin {
    ModelKnowledge,
    #[default]
    WebEvidence,
}

/// One final claim with an explicit provenance contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposedResearchClaim {
    pub text: String,
    #[serde(default)]
    pub origin: ResearchClaimOrigin,
    #[serde(default)]
    pub snapshot_refs: Vec<SnapshotRef>,
    #[serde(default)]
    pub rationale: String,
}

/// The model's comparison of independent knowledge and selected Web evidence.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResearchAnswerComparison {
    #[serde(default)]
    pub agreements: Vec<String>,
    #[serde(default)]
    pub differences: Vec<String>,
    pub synthesis_rationale: String,
}

/// The weighted final answer after reflection over both answer sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposedResearchAnswer {
    pub answer: String,
    pub claims: Vec<ComposedResearchClaim>,
    #[serde(default)]
    pub comparison: ResearchAnswerComparison,
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
        let crawl = CrawlMeta::basic(FINAL_URL.to_string(), 200, Utc::now());
        let snap = Snapshot::new(U.to_string(), "T".into(), "hello world".into(), crawl);
        assert_eq!(snap.content_hash, content_hash("hello world"));
        assert_eq!(snap.snapshot_id, "d0790216187faeb6");
        assert_eq!(snap.snapshot_ref.as_str(), "snapshot:web/d0790216187faeb6");
        // requested_url and final_url differ; both are retained for audit.
        assert_eq!(snap.requested_url, U);
        assert_eq!(snap.crawl.final_url, FINAL_URL);
    }

    #[test]
    fn search_result_identity_is_stable_across_engines_but_provenance_is_not() {
        let google = SearchResult::new(
            SearchEngine::Google,
            Q,
            "T".into(),
            "snip".into(),
            U.to_string(),
            1,
        );
        let bing = SearchResult::new(
            SearchEngine::Bing,
            Q,
            "T".into(),
            "snip".into(),
            U.to_string(),
            1,
        );
        assert_eq!(google.search_result_id, "6b044c73d025");
        assert_eq!(google.search_result_id, bing.search_result_id);
        assert_eq!(
            serde_json::to_value([google, bing]).unwrap(),
            serde_json::json!([
                {
                    "search_engine": "google",
                    "search_result_id": "6b044c73d025",
                    "title": "T",
                    "snippet": "snip",
                    "url": U,
                    "rank": 1
                },
                {
                    "search_engine": "bing",
                    "search_result_id": "6b044c73d025",
                    "title": "T",
                    "snippet": "snip",
                    "url": U,
                    "rank": 1
                }
            ])
        );
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
    fn answer_styles_have_stable_weights_and_default() {
        assert_eq!(
            ResearchAnswerStyle::default(),
            ResearchAnswerStyle::WebFirst
        );
        assert_eq!(ResearchAnswerStyle::WebFirst.knowledge_weight_percent(), 20);
        assert_eq!(ResearchAnswerStyle::WebFirst.web_weight_percent(), 80);
        assert_eq!(
            ResearchAnswerStyle::KnowledgeFirst.knowledge_weight_percent(),
            80
        );
        assert_eq!(ResearchAnswerStyle::KnowledgeFirst.web_weight_percent(), 20);
    }

    #[test]
    fn decision_rationale_length_is_bounded_for_auditable_summaries() {
        assert!(validate_decision_rationale("short").is_err());
        assert!(validate_decision_rationale("A concise and reviewable reason.").is_ok());
        assert!(
            validate_decision_rationale(&"x".repeat(MAX_DECISION_RATIONALE_CHARS + 1)).is_err()
        );
    }

    fn intake_brief() -> ResearchBrief {
        ResearchBrief {
            schema_version: RESEARCH_BRIEF_SCHEMA_VERSION,
            original_question: "Which database is best?".into(),
            research_question: "Compare PostgreSQL and SQLite for a single-user local application"
                .into(),
            desired_output: Some("A concise trade-off table".into()),
            scope: ResearchScope {
                time_range: None,
                geography: None,
                include: vec!["operational simplicity".into()],
                exclude: vec![],
            },
            source_constraints: vec![],
            accepted_assumptions: vec!["One developer".into()],
        }
    }

    #[test]
    fn research_brief_hash_is_pinned_and_fields_affect_it() {
        let brief = intake_brief();
        assert_eq!(
            brief.content_hash().unwrap(),
            "sha256:52f7593b95ea27fa1fd70382aa7dbaaa97f2ac9aa7dadda30c74789a5efbd289"
        );
        let mut changed = brief.clone();
        changed.research_question.push('?');
        assert_ne!(
            brief.content_hash().unwrap(),
            changed.content_hash().unwrap()
        );
    }

    #[test]
    fn research_brief_normalizes_empty_optional_constraints() {
        let mut brief = intake_brief();
        brief.original_question = "  Which database is best?  ".into();
        brief.desired_output = Some("  ".into());
        let normalized = brief.normalized(" Which database is best? ").unwrap();
        assert_eq!(normalized.original_question, "Which database is best?");
        assert_eq!(normalized.desired_output, None);
        assert!(normalized.source_constraints.is_empty());
    }

    #[test]
    fn research_brief_rejects_changed_original_and_boundaries() {
        let mut changed = intake_brief();
        changed.original_question = "A different question".into();
        assert_eq!(
            changed.normalized("Which database is best?").unwrap_err(),
            BriefValidationError::OriginalQuestionChanged
        );

        let mut empty = intake_brief();
        empty.research_question = "  ".into();
        assert_eq!(
            empty.normalized("Which database is best?").unwrap_err(),
            BriefValidationError::Empty("research_question")
        );

        let mut too_long = intake_brief();
        too_long.desired_output = Some("x".repeat(MAX_BRIEF_STRING_CHARS + 1));
        assert!(matches!(
            too_long.normalized("Which database is best?"),
            Err(BriefValidationError::TooLong {
                field: "desired_output",
                ..
            })
        ));

        let mut too_many = intake_brief();
        too_many.scope.include = vec!["x".into(); MAX_BRIEF_ARRAY_ITEMS + 1];
        assert!(matches!(
            too_many.normalized("Which database is best?"),
            Err(BriefValidationError::TooManyItems {
                field: "scope.include",
                ..
            })
        ));
    }

    #[test]
    fn frozen_research_brief_roundtrips_and_rejects_wrong_hash() {
        let brief = intake_brief();
        let hash = brief.content_hash().unwrap();
        let frozen_at = DateTime::parse_from_rfc3339("2026-07-11T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let frozen = FrozenResearchBrief::new(
            brief.clone(),
            &brief.original_question,
            "clarification-1".into(),
            &hash,
            frozen_at,
        )
        .unwrap();
        let json = serde_json::to_string(&frozen).unwrap();
        let back: FrozenResearchBrief = serde_json::from_str(&json).unwrap();
        assert_eq!(frozen, back);
        assert_eq!(back.brief(), &brief);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("frozen_at").is_some());
        assert!(value.get("confirmed_at").is_none());

        assert!(matches!(
            FrozenResearchBrief::new(
                brief.clone(),
                &brief.original_question,
                "clarification-1".into(),
                "sha256:wrong",
                frozen_at,
            ),
            Err(BriefValidationError::ContentHashMismatch { .. })
        ));
    }
}

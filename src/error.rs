//! Typed errors for the research pipeline, each carrying an `error_class`.
//!
//! The design (§8.1) splits every failure into one of two runtime-attribution
//! buckets: `external` (a dependency or the model jittered) versus `internal`
//! (one of our own invariants broke). Choosing Rust lets us fold that split
//! into the type system so every call site matches it exhaustively — the
//! classification can never silently go missing the way a stray Python
//! `except` can.

use serde::{Deserialize, Serialize};

/// Runtime attribution for a failure (§8.1).
///
/// `External` = an outside dependency or the model misbehaved (crawl4ai
/// reporting empty "success", Bing ranking drift, strong returning bad JSON).
/// `Internal` = one of our own invariants broke (e.g. a stored snapshot no
/// longer hashes to its recorded `content_hash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorClass {
    External,
    Internal,
}

/// Fatal boundary at which a pipeline error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    Setup,
    Planning,
    Search,
    Archive,
    Selection,
    Synthesis,
    Trace,
}

/// Domain errors raised while researching a question.
///
/// Binary environment loading stays in `anyhow`; runtime setup and pipeline
/// failures retain this typed form. Every variant maps to a specific program
/// validation (§6) or failure mode (§8.1), and `error_class` pins its attribution.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Bing search itself failed: network, scrape parse, or an empty result
    /// set for the query.
    #[error("search failed: {message}")]
    Search { message: String },

    /// crawl4ai could not produce a valid snapshot for `url` — the fetch
    /// failed, or `success=true` but the body was empty / below the minimum
    /// length (§6 validation 3; §5 "success=true 不等于正文正确").
    #[error("fetch failed for {url}: {reason}")]
    Fetch { url: String, reason: String },

    /// The SSRF guard rejected a URL before or after redirects (§10: public
    /// HTTP(S) only). The blocked URL originated outside, so this is external.
    #[error("SSRF guard blocked {url}: {reason}")]
    Ssrf { url: String, reason: String },

    /// The strong model endpoint could not be reached or rejected the request.
    #[error("model call failed: {message}")]
    ModelCall { message: String },

    /// The strong model returned output that failed JSON/schema validation —
    /// a malformed query batch (§6 validation 1), selection, or answer.
    #[error("model output rejected: {message}")]
    ModelOutput { message: String },

    /// The model referenced an id that does not belong to this run: a
    /// `search_result_id` for archiving, or a `snapshot_ref` for selection or
    /// a Claim (§6 validations 2, 5, 6).
    #[error("reference not in this run: {reference}")]
    RefNotInRun { reference: String },

    /// Nothing usable to answer from: no search results, every fetch failed,
    /// or the model judged the sources insufficient (§6 据实拒答 cases). A
    /// legitimate refusal driven by the outside world, hence external.
    #[error("no usable source to answer from")]
    NoUsableSource,

    /// Runtime construction or configuration failed before the pipeline began.
    #[error("setup failed: {message}")]
    Setup { message: String },

    /// A snapshot read back from storage no longer hashes to its recorded
    /// `content_hash` (§6 validation 4). Our own store corrupted the body, so
    /// this is internal — not a dependency's fault.
    #[error("content hash mismatch for {reference}: expected {expected}, got {actual}")]
    HashMismatch {
        reference: String,
        expected: String,
        actual: String,
    },

    /// A caller supplied a `Snapshot` whose public fields no longer satisfy
    /// the frozen content-addressing formula.
    #[error("invalid snapshot identity: {0}")]
    InvalidSnapshot(String),

    /// The snapshot store (rusqlite) failed on open, schema, or a query — our
    /// own persistence layer misbehaved, so internal (§8.1). `#[from]` lets
    /// every `?` on a SQL call fold into this without a manual map at each site.
    #[error("snapshot store failed: {0}")]
    Storage(#[from] rusqlite::Error),

    /// Appending to the append-only trace log failed — file I/O, or serializing
    /// one of our own events. Our audit layer, so internal (§8.1).
    // ponytail: `#[from] io::Error` claims *every* crate io::Error as a trace
    // failure. True in P2 (io only arises in trace.rs); if a non-trace io path
    // appears later (e.g. reading a local fixture), split this variant then.
    #[error("trace write failed: {0}")]
    Trace(#[from] std::io::Error),

    /// A fatal error annotated at its orchestration boundary.
    #[error("{source}")]
    Staged {
        stage: PipelineStage,
        #[source]
        source: Box<SearchError>,
    },

    /// Recording `run_failed` itself failed. Both failures remain inspectable.
    #[error("failed to record pipeline error ({original}): {trace}")]
    FailureTrace {
        original: Box<SearchError>,
        trace: Box<SearchError>,
    },
}

impl SearchError {
    /// Attach the first (most specific) fatal pipeline boundary.
    #[must_use]
    pub fn at(self, stage: PipelineStage) -> Self {
        if matches!(self, Self::Staged { .. } | Self::FailureTrace { .. }) {
            self
        } else {
            Self::Staged {
                stage,
                source: Box::new(self),
            }
        }
    }

    #[must_use]
    pub fn stage(&self) -> Option<PipelineStage> {
        match self {
            Self::Staged { stage, .. } => Some(*stage),
            Self::FailureTrace { .. } | Self::Trace(_) => Some(PipelineStage::Trace),
            _ => None,
        }
    }

    /// Runtime attribution bucket for this error (§8.1). Exhaustive match, so
    /// any future variant forces an explicit classification here.
    #[must_use]
    pub fn error_class(&self) -> ErrorClass {
        match self {
            Self::Search { .. }
            | Self::Fetch { .. }
            | Self::Ssrf { .. }
            | Self::ModelCall { .. }
            | Self::ModelOutput { .. }
            | Self::RefNotInRun { .. }
            | Self::NoUsableSource => ErrorClass::External,
            Self::Setup { .. }
            | Self::HashMismatch { .. }
            | Self::InvalidSnapshot(_)
            | Self::Storage(_)
            | Self::Trace(_)
            | Self::FailureTrace { .. } => ErrorClass::Internal,
            Self::Staged { source, .. } => source.error_class(),
        }
    }
}

/// Pipeline result alias — every fallible pipeline function returns this.
pub type Result<T> = std::result::Result<T, SearchError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_dependency_jitter_as_external_and_our_bug_as_internal() {
        assert_eq!(
            SearchError::Fetch {
                url: "https://example.com".into(),
                reason: "timeout".into(),
            }
            .error_class(),
            ErrorClass::External
        );
        assert_eq!(
            SearchError::HashMismatch {
                reference: "snapshot:web/x".into(),
                expected: "sha256:a".into(),
                actual: "sha256:b".into(),
            }
            .error_class(),
            ErrorClass::Internal
        );
    }

    #[test]
    fn error_class_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ErrorClass::External).unwrap(),
            "\"external\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorClass::Internal).unwrap(),
            "\"internal\""
        );
    }
}

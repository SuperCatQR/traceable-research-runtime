//! traceable-search — an auditable web-research pipeline.
//!
//! Design is frozen in `docs/web-search-architecture.md`. The build lands in phases:
//! P1 domain types + `error_class`, P2 dual persistence (snapshot.sqlite + trace JSONL),
//! P3 external adapters (Bing / crawl4ai / strong model) behind an SSRF guard,
//! P4 orchestration (fixed 3-round explore + synthesize; three pure functions),
//! P5 program validations + E2E.
//!
//! The crate is split lib + bin so the three pure functions (`plan_queries`,
//! `select_sources`, `synthesize_answer`) stay fixture-testable without a runtime.

pub mod adapters;
pub mod app;
pub mod backend;
pub mod error;
pub mod intake;
pub mod orchestration;
pub mod snapshot;
pub mod trace;
pub mod types;

// Flat public surface: downstream phases import from the crate root, not deep
// module paths.
pub use adapters::{CrawlClient, SearxngClient, StrongClient, validate_public_url};
pub use app::{AppConfig, PublicAnswer, PublicClaim, PublicSource, ResearchService};
pub use backend::{INTAKE_PROMPT, LiveBackend, PLAN_PROMPT, SELECT_PROMPT, SYNTHESIZE_PROMPT};
pub use error::{ErrorClass, PipelineStage, Result, SearchError};
pub use intake::{
    ClarificationAnswer, ClarificationQuestion, INTAKE_EVENT_SCHEMA_VERSION, IntakeError,
    IntakeEvent, IntakeEventKind, IntakeLog, IntakeModelOutput, IntakeResult, IntakeSession,
    IntakeSessionLocks, IntakeStatus, MAX_TOTAL_QUESTIONS, ModelParseOutcome, cancellation_event,
    confirmation_event, events_for_model_output, minimal_brief_event, parse_model_attempt,
    parse_model_output, reduce_intake_event, replay_intake, user_reply_event,
};
pub use snapshot::{SnapshotReader, SnapshotWriter};
pub use trace::{
    ReplayedRunHeader, RunHeader, RunReplay, SourceSelection, TRACE_SCHEMA_VERSION, TraceEvent,
    TracePolicy, TraceWriter, replay_run_header,
};
pub use types::{
    Answer, BriefValidationError, Claim, ConfirmedResearchBrief, CrawlBodyKind, CrawlMeta, Excerpt,
    Query, RESEARCH_BRIEF_SCHEMA_VERSION, ResearchBrief, ResearchScope, SearchResult, Snapshot,
    SnapshotRef, content_hash, search_result_id, snapshot_id, snapshot_ref,
};

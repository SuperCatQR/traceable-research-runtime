//! traceable-search — an auditable web-research pipeline.
//!
//! The current design is documented in `docs/web-search-architecture.md`: a model-led
//! conversation freezes an internal research brief, then a bounded research run records
//! snapshots and review-safe trace events.
//!
//! The crate is library-only; pure functions remain fixture-testable without a transport runtime.

pub mod brave_search;
pub mod clarification;
pub mod conversation;
pub mod live_research_backend;
pub mod model_adapter;
pub mod research_domain;
pub mod research_error;
pub mod research_run;
pub mod research_trace;
pub mod runtime;
pub mod snapshot_store;
pub mod web_search;
pub mod web_snapshot;

// Flat public surface: downstream phases import from the crate root, not deep
// module paths.
pub use brave_search::BraveSearchClient;
pub use clarification::{
    CLARIFICATION_EVENT_SCHEMA_VERSION, ClarificationDecision, ClarificationError,
    ClarificationEvent, ClarificationEventKind, ClarificationEventLog, ClarificationLocks,
    ClarificationModelOutput, ClarificationModelParseOutcome, ClarificationResult,
    ClarificationState, ClarificationStatus, DialogueMessage, DialogueRole,
    clarification_cancelled_event, clarification_model_request_failed_event,
    clarification_user_message_event, clarification_user_message_event_with_operation,
    events_from_clarification_model_output, parse_clarification_model_attempt,
    parse_clarification_model_output, reduce_clarification_event, replay_clarification,
    research_run_prepared_event_with_answer_style,
};
pub use conversation::{
    CONVERSATION_EVENT_SCHEMA_VERSION, CompletedResearchTurn, CompletedTurnContext,
    ConversationError, ConversationEvent, ConversationEventKind, ConversationEventLog,
    ConversationLocks, ConversationResult, ResearchConversation, UnansweredResearchTurn,
    reduce_conversation_event, replay_conversation,
};
pub use live_research_backend::{
    CLARIFICATION_PROMPT, EVIDENCE_SELECTION_PROMPT, LiveResearchBackend,
    MODEL_KNOWLEDGE_DRAFT_PROMPT, REFLECTIVE_COMPOSITION_PROMPT, SEARCH_QUERY_PLANNING_PROMPT,
};
pub use model_adapter::OpenAiCompatibleModelClient;
pub use research_domain::{
    BriefValidationError, ComposedResearchAnswer, ComposedResearchClaim, CrawlBodyKind, CrawlMeta,
    ExplorationStopReason, FrozenResearchBrief, MAX_DECISION_RATIONALE_CHARS,
    MIN_DECISION_RATIONALE_CHARS, ModelKnowledgeDraft, RESEARCH_BRIEF_SCHEMA_VERSION,
    RationaleAuditStatus, ResearchAnswerComparison, ResearchAnswerStyle, ResearchBrief,
    ResearchClaimOrigin, ResearchScope, SearchBoundaryContractFailure, SearchEngine,
    SearchEngineAttempt, SearchEngineAttemptOutcome, SearchEngineUnavailability, SearchQuery,
    SearchResult, Snapshot, SnapshotNavigationExcerpt, SnapshotRef, WebSearchCompletion,
    WebSearchExecution, WebSearchFailureReason, content_hash, search_result_id, snapshot_id,
    snapshot_ref, validate_decision_rationale,
};
pub use research_error::{ErrorClass, ResearchError, ResearchStage, Result};
pub use research_trace::{
    ReplayedTrace, RunHeader, RunReplay, SourceSelection, TRACE_SCHEMA_VERSION, TraceEvent,
    TraceEventEnvelope, TracePolicy, TraceWriter, replay_trace, validate_trace_event_for_schema,
    validate_trace_policy,
};
pub use runtime::{
    ChatResearchAnswerResponse, EvidenceSourceResponse, ModelAccessConfig, PreparedResearchRun,
    ResearchAnswerResponse, ResearchClaimResponse, ResearchInfrastructureConfig,
    ResearchPreparationError, ResearchRuntimeError, TraceableResearchRuntime,
    project_chat_research_answer,
};
pub use snapshot_store::{SnapshotReader, SnapshotWriter};
pub use web_search::WebSearch;
pub use web_snapshot::{EmbeddedSnapshotClient, validate_public_web_url};

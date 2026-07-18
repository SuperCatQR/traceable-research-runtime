//! Traceable Markdown Document Research Runtime.
//!
//! The crate exposes a small application Interface over a versioned Markdown
//! corpus. Domain state is submitted through validated commands and can only
//! be recovered or projected after append-only events have been replayed.

mod clarification;
mod conversation;
mod corpus;
mod domain;
pub mod error;
mod execution_engine;
mod execution_trace;
mod identity;
mod integrity;
mod live_model_gateway;
mod model_gateway;
mod runtime;
mod storage;

// Keep lifecycle event payloads, append/replay logs, corpus readers, the
// execution Trace, SQLite storage and integrity implementation behind the
// Runtime seam.  The root exports only command inputs/results, projections,
// identity/authorization values, and the model Adapter seam.
pub use clarification::{
    DialogueRole, DocumentResearchBriefDraft, ResearchQuestionClarificationDecision,
    ResearchQuestionClarificationDialogueMessage, ResearchQuestionClarificationModelOutput,
    ResearchQuestionClarificationState,
};
pub use conversation::{
    DocumentResearchConversation, DocumentResearchRequestState, DocumentResearchRequestStatus,
};
pub use corpus::{
    MarkdownCorpusNavigationNodeInput, MarkdownSourceDocumentInput,
    PublishMarkdownCorpusSnapshotInput,
};
pub use domain::{
    AnswerCompositionStyle, DetailedMarkdownResearchAuditItem, DetailedMarkdownResearchAuditPage,
    EvidenceLinkedResearchClaim, EvidenceLinkedResearchClaimCitationStatus,
    EvidenceLinkedResearchClaimsAnswer, FrozenDocumentResearchBrief,
    MarkdownResearchExecutionLimits, MarkdownResearchExecutionOverview,
    MarkdownResearchModelTaskKind, ModelKnowledgeOnlyAnswer, PreparedMarkdownResearchExecution,
    PublicMarkdownResearchAnswer, PublicResearchCoverageGap, PublicSourceCitation,
    ResearchClaimEvidenceRelationship, ResearchClaimEvidenceRelationshipType, ResearchCoverageGap,
    ResearchCoverageGapPriority, ResearchCoverageGapResolutionStatus, ResourceExhaustionOutcome,
    SourceAttributedAnswerComposition, SourceAttributedAnswerSegment,
    SourceAttributedAnswerSegmentSourceType,
};
pub use error::{Result, RuntimeError, RuntimeErrorCode, RuntimeStage};
pub use execution_trace::{
    MarkdownCorpusNavigationBranchCloseReason,
    MarkdownCorpusNavigationBranchDocumentRelevanceReport, MarkdownCorpusNavigationBranchSelection,
    MarkdownCorpusNavigationNodeSelectionStatus, MarkdownSourceFollowUpAction,
    ResearchDocumentReadRequest,
};
pub use identity::{
    CommandId, DocumentResearchBranchTaskId, DocumentResearchConversationId,
    DocumentResearchRequestId, EvidenceLinkedResearchClaimId,
    MarkdownCorpusNavigationCandidateSetId, MarkdownCorpusNavigationNodeId,
    MarkdownCorpusSnapshotId, MarkdownResearchExecutionId, MarkdownResearchModelTaskId,
    MarkdownSourceDocumentId, MarkdownSourceDocumentVersionId, MarkdownSourceSegmentId, OpaqueId,
    PrincipalCapability, PublicSourceCitationId, ResearchCoverageGapId,
    ResearchDocumentReadRequestId, ResearchPrincipal, SubjectId,
    VerbatimSourceEvidenceExtractionRequestId, VerbatimSourceEvidenceId,
};
pub use live_model_gateway::{
    OpenAiCompatibleMarkdownResearchModelGateway,
    OpenAiCompatibleMarkdownResearchModelGatewayConfig,
};
pub use model_gateway::{
    AcceptedVerbatimSourceEvidenceModelContext, AllowedCompletedResearchContext,
    AuthorizedMarkdownSourceSegmentInput, EvidenceLinkedResearchClaimGenerationResponse,
    EvidenceLinkedResearchClaimGenerationTask, EvidenceLinkedResearchClaimsAnswerGenerationTask,
    FixtureMarkdownResearchModelCall, FixtureMarkdownResearchModelGateway,
    MarkdownCorpusNavigationBranchDocumentRelevanceReportTask,
    MarkdownCorpusNavigationBranchSelectionResponse, MarkdownCorpusNavigationBranchSelectionTask,
    MarkdownCorpusNavigationNodeCandidate, MarkdownResearchModelGateway,
    MarkdownSourceDocumentAbstractModelCandidate, MarkdownSourceReviewDecision,
    MarkdownSourceReviewTask, MarkdownSourceSegmentMetadata,
    ModelKnowledgeOnlyAnswerGenerationTask, ResearchDocumentReadRequestTask,
    ResearchQuestionEvaluationTask, ScriptedStrongMarkdownResearchModelResponse,
    ScriptedVerbatimSourceEvidenceExtractionResponse, SourceAttributedAnswerCompositionTask,
    StrongMarkdownResearchModelResponse, StrongMarkdownResearchModelTask,
    VerbatimSourceEvidenceCandidate, VerbatimSourceEvidenceCandidateSet,
    VerbatimSourceEvidenceExtractionTask,
};
pub use runtime::{
    DocumentResearchRequestSnapshot, MarkdownResearchExecutionResult,
    StartedDocumentResearchRequest, TraceableMarkdownResearchRuntime,
};

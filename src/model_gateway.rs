//! Closed model tasks, response validation and deterministic fixture adapter.

use crate::clarification::{
    DocumentResearchBriefDraft, ResearchQuestionClarificationDialogueMessage,
    ResearchQuestionClarificationModelOutput,
};
use crate::corpus::{
    MAX_MARKDOWN_DOCUMENT_ABSTRACT_BYTES, MAX_MARKDOWN_DOCUMENT_TITLE_BYTES,
    MAX_MARKDOWN_SOURCE_SEGMENT_BYTES, MAX_NAVIGATION_LINKS_PER_NODE,
};
use crate::domain::{
    ANSWER_PROJECTION_SCHEMA_VERSION, AnswerCompositionStyle, EvidenceLinkedResearchClaim,
    EvidenceLinkedResearchClaimsAnswer, FrozenDocumentResearchBrief, MAX_CLAIM_TEXT_BYTES,
    MAX_EVIDENCE_QUOTE_BYTES, MAX_RESEARCH_TEXT_BYTES, MarkdownResearchModelTaskKind,
    ModelKnowledgeOnlyAnswer, PublicSourceCitation, ResearchCoverageGap,
    SourceAttributedAnswerComposition, SourceAttributedAnswerSegmentSourceType,
    VerbatimSourceEvidence, sha256_content_hash,
};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::execution_trace::{
    MarkdownCorpusNavigationBranchCloseReason,
    MarkdownCorpusNavigationBranchDocumentRelevanceReport, MarkdownCorpusNavigationBranchSelection,
    MarkdownSourceFollowUpAction, ResearchDocumentReadRequest,
};
use crate::identity::{
    DocumentResearchBranchTaskId, DocumentResearchConversationId, DocumentResearchRequestId,
    EvidenceLinkedResearchClaimId, MarkdownCorpusNavigationCandidateSetId,
    MarkdownCorpusNavigationNodeId, MarkdownCorpusSnapshotId, MarkdownResearchExecutionId,
    MarkdownResearchModelTaskId, MarkdownSourceDocumentId, MarkdownSourceDocumentVersionId,
    MarkdownSourceSegmentId, ResearchDocumentReadRequestId,
    VerbatimSourceEvidenceExtractionRequestId, VerbatimSourceEvidenceId,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tokio::sync::Mutex;

/// Closed schema version shared by every model task in this module.
pub const MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION: u32 = 1;
/// Maximum serialized input size for one logical model task.
pub const MAX_MARKDOWN_RESEARCH_MODEL_TASK_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of values accepted in one model-facing collection.
pub const MAX_MARKDOWN_RESEARCH_MODEL_ITEMS: usize = 128;
/// Maximum serialized response size accepted from an Adapter.
pub const MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Public-safe context from a completed request that a task is allowed to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedCompletedResearchContext {
    /// Human-readable label for the prior result.
    pub completed_research_context_label: String,
    /// Public answer or user-approved context, never a current-corpus object.
    pub completed_research_context_text: String,
}

/// One direct navigation child exposed to the branch-selection task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationNodeCandidate {
    /// Opaque node ID from the frozen candidate set.
    pub markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Navigation label.
    pub markdown_corpus_navigation_node_label: String,
    /// Navigation summary.
    pub markdown_corpus_navigation_node_summary: String,
}

/// One title/abstract candidate exposed to one branch-report task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownSourceDocumentAbstractModelCandidate {
    /// Opaque source document ID.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Source document title.
    pub markdown_source_document_title: String,
    /// Source document abstract.
    pub markdown_source_document_abstract: String,
}

/// Body-free segment metadata exposed to a read-request task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownSourceSegmentMetadata {
    /// Source document ID.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Frozen source document version ID.
    pub markdown_source_document_version_id: MarkdownSourceDocumentVersionId,
    /// Candidate segment ID.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// Nearest section heading, when present.
    pub markdown_source_segment_section_heading: Option<String>,
    /// Inclusive byte offset in the canonical document body.
    pub markdown_source_segment_start_byte_offset_in_document: u64,
    /// Exclusive byte offset in the canonical document body.
    pub markdown_source_segment_end_byte_offset_in_document: u64,
    /// Hash of the exact segment bytes.
    pub markdown_source_segment_hash: String,
}

/// An owned copy of exactly one segment authorized for model access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedMarkdownSourceSegmentInput {
    /// Source document ID.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Frozen source document version ID.
    pub markdown_source_document_version_id: MarkdownSourceDocumentVersionId,
    /// Authorized segment ID.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// Hash of the exact segment bytes.
    pub markdown_source_segment_hash: String,
    /// Inclusive segment offset in the canonical document body.
    pub markdown_source_segment_start_byte_offset_in_document: u64,
    /// Exact authorized Markdown text.
    pub canonical_markdown_source_segment_text: String,
}

impl AuthorizedMarkdownSourceSegmentInput {
    fn validate(&self) -> Result<()> {
        validate_task_text(
            "markdown_source_segment_hash",
            &self.markdown_source_segment_hash,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        if self.canonical_markdown_source_segment_text.is_empty()
            || self.canonical_markdown_source_segment_text.len() > MAX_MARKDOWN_SOURCE_SEGMENT_BYTES
            || self.canonical_markdown_source_segment_text.contains('\0')
        {
            return Err(task_validation(
                "authorized Markdown source segment is empty, invalid, or too long",
            ));
        }
        if sha256_content_hash(self.canonical_markdown_source_segment_text.as_bytes())
            != self.markdown_source_segment_hash
        {
            return Err(task_validation(
                "authorized Markdown source segment hash does not match its text",
            ));
        }
        Ok(())
    }
}

/// Accepted evidence projected without an internal source path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedVerbatimSourceEvidenceModelContext {
    /// Runtime-owned evidence ID.
    pub verbatim_source_evidence_id: VerbatimSourceEvidenceId,
    /// Source document ID.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Source segment ID.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// Exact accepted quote.
    pub verbatim_source_evidence_quote: String,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
}

impl From<&VerbatimSourceEvidence> for AcceptedVerbatimSourceEvidenceModelContext {
    fn from(evidence: &VerbatimSourceEvidence) -> Self {
        Self {
            verbatim_source_evidence_id: evidence.verbatim_source_evidence_id.clone(),
            markdown_source_document_id: evidence.markdown_source_document_id.clone(),
            markdown_source_segment_id: evidence.markdown_source_segment_id.clone(),
            verbatim_source_evidence_quote: evidence.verbatim_source_evidence_quote.clone(),
            document_research_branch_task_id: evidence.document_research_branch_task_id.clone(),
            markdown_research_execution_id: evidence.markdown_research_execution_id.clone(),
        }
    }
}

/// Strong-model task for research-question evaluation and clarification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQuestionEvaluationTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning conversation.
    pub document_research_conversation_id: DocumentResearchConversationId,
    /// Owning request.
    pub document_research_request_id: DocumentResearchRequestId,
    /// Revision the response must evaluate.
    pub research_question_clarification_revision: u64,
    /// Original user question.
    pub original_user_question: String,
    /// User/model clarification dialogue accepted so far.
    pub research_question_clarification_dialogue: Vec<ResearchQuestionClarificationDialogueMessage>,
    /// Latest normalized draft, when a prior evaluation produced one.
    pub document_research_brief_draft: Option<DocumentResearchBriefDraft>,
    /// Explicitly allowed completed-request history.
    pub allowed_completed_research_context: Vec<AllowedCompletedResearchContext>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task that cannot represent current-corpus input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelKnowledgeOnlyAnswerGenerationTask {
    /// Stable logical model task ID and runtime-owned answer ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Explicitly allowed completed-request history.
    pub allowed_completed_research_context: Vec<AllowedCompletedResearchContext>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task that selects from one complete direct-child set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationBranchSelectionTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Locked snapshot ID.
    pub markdown_corpus_snapshot_id: MarkdownCorpusSnapshotId,
    /// Candidate-set ID persisted before dispatch.
    pub markdown_corpus_navigation_candidate_set_id: MarkdownCorpusNavigationCandidateSetId,
    /// Parent node whose complete direct children are listed.
    pub parent_markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Complete direct-child candidate set.
    pub markdown_corpus_navigation_node_candidates: Vec<MarkdownCorpusNavigationNodeCandidate>,
    /// Evidence IDs that authorized scope expansion; no evidence quote is exposed here.
    pub triggering_verbatim_source_evidence_ids: Vec<VerbatimSourceEvidenceId>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task limited to one branch's full title/abstract set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationBranchDocumentRelevanceReportTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Selected navigation node.
    pub markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Complete title/abstract set for this branch.
    pub markdown_source_document_candidates: Vec<MarkdownSourceDocumentAbstractModelCandidate>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task that can select only reported document/segment metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchDocumentReadRequestTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Runtime-owned ID the response must echo.
    pub research_document_read_request_id: ResearchDocumentReadRequestId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Reports committed before this task was frozen.
    pub committed_branch_document_reports:
        Vec<MarkdownCorpusNavigationBranchDocumentRelevanceReport>,
    /// Candidate segment metadata; no body text is present.
    pub candidate_markdown_source_segments: Vec<MarkdownSourceSegmentMetadata>,
    /// Accepted evidence visible to this phase.
    pub accepted_verbatim_source_evidence: Vec<AcceptedVerbatimSourceEvidenceModelContext>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task that receives exactly one authorized source segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownSourceReviewTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Read authorization that exposed the segment.
    pub research_document_read_request_id: ResearchDocumentReadRequestId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// The only current-corpus body available to this task.
    pub authorized_markdown_source_segment: AuthorizedMarkdownSourceSegmentInput,
    /// Accepted evidence visible to this phase.
    pub accepted_verbatim_source_evidence: Vec<AcceptedVerbatimSourceEvidenceModelContext>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task that can derive claims only from accepted evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLinkedResearchClaimGenerationTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Accepted evidence and no current-corpus body.
    pub accepted_verbatim_source_evidence: Vec<AcceptedVerbatimSourceEvidenceModelContext>,
    /// Coverage gaps visible during claim generation.
    pub research_coverage_gaps: Vec<ResearchCoverageGap>,
    /// Runtime-owned IDs the model may use for proposed claims.
    pub authorized_evidence_linked_research_claim_ids: Vec<EvidenceLinkedResearchClaimId>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task whose only factual input is the committed claim set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLinkedResearchClaimsAnswerGenerationTask {
    /// Stable logical model task ID and runtime-owned answer ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Only factual input made visible to this task.
    pub committed_evidence_linked_research_claims: Vec<EvidenceLinkedResearchClaim>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// Strong-model task where the model-only and evidence-only routes finally meet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAttributedAnswerCompositionTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Requested composition style.
    pub requested_answer_composition_style: AnswerCompositionStyle,
    /// Isolated model-only answer.
    pub model_knowledge_only_answer: ModelKnowledgeOnlyAnswer,
    /// Committed claims used by the evidence-only answer.
    pub committed_evidence_linked_research_claims: Vec<EvidenceLinkedResearchClaim>,
    /// Answer generated only from committed claims.
    pub evidence_linked_research_claims_answer: EvidenceLinkedResearchClaimsAnswer,
    /// Public-safe citations available for final attribution.
    pub public_source_citations: Vec<PublicSourceCitation>,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

/// The nine closed strong-model tasks supported by the fixed research workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "markdown_research_model_task_kind",
    content = "task",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StrongMarkdownResearchModelTask {
    /// Evaluate or clarify the research question.
    ResearchQuestionEvaluation(ResearchQuestionEvaluationTask),
    /// Generate the isolated model-knowledge answer.
    ModelKnowledgeOnlyAnswerGeneration(ModelKnowledgeOnlyAnswerGenerationTask),
    /// Select from one complete direct-child navigation set.
    MarkdownCorpusNavigationBranchSelection(MarkdownCorpusNavigationBranchSelectionTask),
    /// Report relevance for one branch's title/abstract candidates.
    MarkdownCorpusNavigationBranchDocumentRelevanceReport(
        MarkdownCorpusNavigationBranchDocumentRelevanceReportTask,
    ),
    /// Select one reported document/segment pair to read.
    ResearchDocumentReadRequest(ResearchDocumentReadRequestTask),
    /// Review one authorized source segment.
    MarkdownSourceReview(MarkdownSourceReviewTask),
    /// Generate evidence-linked claims.
    EvidenceLinkedResearchClaimGeneration(EvidenceLinkedResearchClaimGenerationTask),
    /// Generate the claims-only answer.
    EvidenceLinkedResearchClaimsAnswerGeneration(EvidenceLinkedResearchClaimsAnswerGenerationTask),
    /// Compose one source-attributed answer style.
    SourceAttributedAnswerComposition(SourceAttributedAnswerCompositionTask),
}

impl StrongMarkdownResearchModelTask {
    /// Returns the stable logical task ID.
    #[must_use]
    pub fn markdown_research_model_task_id(&self) -> &MarkdownResearchModelTaskId {
        match self {
            Self::ResearchQuestionEvaluation(task) => &task.markdown_research_model_task_id,
            Self::ModelKnowledgeOnlyAnswerGeneration(task) => &task.markdown_research_model_task_id,
            Self::MarkdownCorpusNavigationBranchSelection(task) => {
                &task.markdown_research_model_task_id
            }
            Self::MarkdownCorpusNavigationBranchDocumentRelevanceReport(task) => {
                &task.markdown_research_model_task_id
            }
            Self::ResearchDocumentReadRequest(task) => &task.markdown_research_model_task_id,
            Self::MarkdownSourceReview(task) => &task.markdown_research_model_task_id,
            Self::EvidenceLinkedResearchClaimGeneration(task) => {
                &task.markdown_research_model_task_id
            }
            Self::EvidenceLinkedResearchClaimsAnswerGeneration(task) => {
                &task.markdown_research_model_task_id
            }
            Self::SourceAttributedAnswerComposition(task) => &task.markdown_research_model_task_id,
        }
    }

    /// Returns the stable task kind used for accounting and fixture matching.
    #[must_use]
    pub const fn kind(&self) -> MarkdownResearchModelTaskKind {
        match self {
            Self::ResearchQuestionEvaluation(_) => {
                MarkdownResearchModelTaskKind::ResearchQuestionEvaluation
            }
            Self::ModelKnowledgeOnlyAnswerGeneration(_) => {
                MarkdownResearchModelTaskKind::ModelKnowledgeOnlyAnswerGeneration
            }
            Self::MarkdownCorpusNavigationBranchSelection(_) => {
                MarkdownResearchModelTaskKind::MarkdownCorpusNavigationBranchSelection
            }
            Self::MarkdownCorpusNavigationBranchDocumentRelevanceReport(_) => {
                MarkdownResearchModelTaskKind::MarkdownCorpusNavigationBranchDocumentRelevanceReport
            }
            Self::ResearchDocumentReadRequest(_) => {
                MarkdownResearchModelTaskKind::ResearchDocumentReadRequest
            }
            Self::MarkdownSourceReview(_) => MarkdownResearchModelTaskKind::MarkdownSourceReview,
            Self::EvidenceLinkedResearchClaimGeneration(_) => {
                MarkdownResearchModelTaskKind::EvidenceLinkedResearchClaimGeneration
            }
            Self::EvidenceLinkedResearchClaimsAnswerGeneration(_) => {
                MarkdownResearchModelTaskKind::EvidenceLinkedResearchClaimsAnswerGeneration
            }
            Self::SourceAttributedAnswerComposition(_) => {
                MarkdownResearchModelTaskKind::SourceAttributedAnswerComposition
            }
        }
    }

    /// Returns branch ownership when this is a branch-local task.
    #[must_use]
    pub fn document_research_branch_task_id(&self) -> Option<&DocumentResearchBranchTaskId> {
        match self {
            Self::MarkdownCorpusNavigationBranchDocumentRelevanceReport(task) => {
                Some(&task.document_research_branch_task_id)
            }
            Self::ResearchDocumentReadRequest(task) => Some(&task.document_research_branch_task_id),
            Self::MarkdownSourceReview(task) => Some(&task.document_research_branch_task_id),
            _ => None,
        }
    }

    /// Validates caller-supplied task shape before an Adapter sees it.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version() != MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION {
            return Err(task_validation("unsupported Markdown research model task schema version"));
        }
        match self {
            Self::ResearchQuestionEvaluation(task) => validate_question_evaluation_task(task),
            Self::ModelKnowledgeOnlyAnswerGeneration(task) => validate_brief_and_history(
                &task.frozen_document_research_brief,
                &task.allowed_completed_research_context,
            ),
            Self::MarkdownCorpusNavigationBranchSelection(task) => {
                task.frozen_document_research_brief.validate()?;
                validate_count_with_limit(
                    "markdown_corpus_navigation_node_candidates",
                    task.markdown_corpus_navigation_node_candidates.len(),
                    MAX_NAVIGATION_LINKS_PER_NODE,
                )?;
                validate_count(
                    "triggering_verbatim_source_evidence_ids",
                    task.triggering_verbatim_source_evidence_ids.len(),
                )?;
                ensure_unique_ids(
                    "navigation candidate",
                    task.markdown_corpus_navigation_node_candidates
                        .iter()
                        .map(|candidate| candidate.markdown_corpus_navigation_node_id.as_str()),
                )?;
                ensure_unique_ids(
                    "scope expansion evidence",
                    task.triggering_verbatim_source_evidence_ids.iter().map(|id| id.as_str()),
                )?;
                for candidate in &task.markdown_corpus_navigation_node_candidates {
                    validate_task_text(
                        "markdown_corpus_navigation_node_label",
                        &candidate.markdown_corpus_navigation_node_label,
                        MAX_RESEARCH_TEXT_BYTES,
                    )?;
                    validate_task_text(
                        "markdown_corpus_navigation_node_summary",
                        &candidate.markdown_corpus_navigation_node_summary,
                        MAX_RESEARCH_TEXT_BYTES,
                    )?;
                }
                Ok(())
            }
            Self::MarkdownCorpusNavigationBranchDocumentRelevanceReport(task) => {
                task.frozen_document_research_brief.validate()?;
                validate_count_with_limit(
                    "markdown_source_document_candidates",
                    task.markdown_source_document_candidates.len(),
                    MAX_NAVIGATION_LINKS_PER_NODE,
                )?;
                ensure_unique_ids(
                    "document candidate",
                    task.markdown_source_document_candidates
                        .iter()
                        .map(|candidate| candidate.markdown_source_document_id.as_str()),
                )?;
                for candidate in &task.markdown_source_document_candidates {
                    validate_task_text(
                        "markdown_source_document_title",
                        &candidate.markdown_source_document_title,
                        MAX_MARKDOWN_DOCUMENT_TITLE_BYTES,
                    )?;
                    validate_task_text(
                        "markdown_source_document_abstract",
                        &candidate.markdown_source_document_abstract,
                        MAX_MARKDOWN_DOCUMENT_ABSTRACT_BYTES,
                    )?;
                }
                Ok(())
            }
            Self::ResearchDocumentReadRequest(task) => validate_read_request_task(task),
            Self::MarkdownSourceReview(task) => validate_source_review_task(task),
            Self::EvidenceLinkedResearchClaimGeneration(task) => {
                task.frozen_document_research_brief.validate()?;
                validate_evidence_contexts(
                    &task.accepted_verbatim_source_evidence,
                    &task.markdown_research_execution_id,
                )?;
                validate_count("research_coverage_gaps", task.research_coverage_gaps.len())?;
                validate_count(
                    "authorized_evidence_linked_research_claim_ids",
                    task.authorized_evidence_linked_research_claim_ids.len(),
                )?;
                ensure_unique_ids(
                    "authorized claim",
                    task.authorized_evidence_linked_research_claim_ids.iter().map(|id| id.as_str()),
                )?;
                for gap in &task.research_coverage_gaps {
                    gap.validate_shape()?;
                }
                Ok(())
            }
            Self::EvidenceLinkedResearchClaimsAnswerGeneration(task) => {
                task.frozen_document_research_brief.validate()?;
                validate_committed_claims(
                    &task.committed_evidence_linked_research_claims,
                    &task.markdown_research_execution_id,
                )
            }
            Self::SourceAttributedAnswerComposition(task) => validate_composition_task(task),
        }?;
        validate_serialized_task_size(self)
    }

    /// Decodes and validates one closed-schema response for this exact task.
    pub fn decode_response_json(
        &self,
        response_json: &str,
    ) -> Result<StrongMarkdownResearchModelResponse> {
        self.validate()?;
        reject_oversized_response(response_json)?;
        let envelope: StrongModelResponseEnvelope = decode_closed_json(response_json)?;
        if envelope.markdown_research_model_task_id != *self.markdown_research_model_task_id()
            || envelope.markdown_research_model_task_kind != self.kind()
            || envelope.document_research_branch_task_id.as_ref()
                != self.document_research_branch_task_id()
            || envelope.markdown_research_model_task_schema_version
                != MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION
        {
            return Err(model_response(
                "model response envelope does not match the dispatched task",
            ));
        }
        decode_and_validate_strong_response(self, envelope.response)
    }

    fn schema_version(&self) -> u32 {
        match self {
            Self::ResearchQuestionEvaluation(task) => {
                task.markdown_research_model_task_schema_version
            }
            Self::ModelKnowledgeOnlyAnswerGeneration(task) => {
                task.markdown_research_model_task_schema_version
            }
            Self::MarkdownCorpusNavigationBranchSelection(task) => {
                task.markdown_research_model_task_schema_version
            }
            Self::MarkdownCorpusNavigationBranchDocumentRelevanceReport(task) => {
                task.markdown_research_model_task_schema_version
            }
            Self::ResearchDocumentReadRequest(task) => {
                task.markdown_research_model_task_schema_version
            }
            Self::MarkdownSourceReview(task) => task.markdown_research_model_task_schema_version,
            Self::EvidenceLinkedResearchClaimGeneration(task) => {
                task.markdown_research_model_task_schema_version
            }
            Self::EvidenceLinkedResearchClaimsAnswerGeneration(task) => {
                task.markdown_research_model_task_schema_version
            }
            Self::SourceAttributedAnswerComposition(task) => {
                task.markdown_research_model_task_schema_version
            }
        }
    }
}

/// Response containing one decision for every presented navigation candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationBranchSelectionResponse {
    /// Candidate-set ID being answered.
    pub markdown_corpus_navigation_candidate_set_id: MarkdownCorpusNavigationCandidateSetId,
    /// One selection decision per candidate.
    pub markdown_corpus_navigation_branch_selections: Vec<MarkdownCorpusNavigationBranchSelection>,
}

/// Strong-model decision after reviewing the single authorized segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownSourceReviewDecision {
    /// Read request being reviewed.
    pub research_document_read_request_id: ResearchDocumentReadRequestId,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Source document being reviewed.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Exact authorized segment being reviewed.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// One closed follow-up action.
    pub markdown_source_follow_up_action: MarkdownSourceFollowUpAction,
    /// Required only when extraction is requested.
    pub verbatim_source_evidence_extraction_goal: Option<String>,
    /// Required only for scope expansion and limited to accepted evidence IDs.
    pub triggering_verbatim_source_evidence_ids: Vec<VerbatimSourceEvidenceId>,
    /// Required only when closing the current branch.
    pub markdown_corpus_navigation_branch_close_reason:
        Option<MarkdownCorpusNavigationBranchCloseReason>,
    /// Review-safe summary without hidden reasoning.
    pub markdown_source_review_summary: String,
}

/// Response containing runtime-ID-bound evidence-linked claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLinkedResearchClaimGenerationResponse {
    /// Claims proposed from the task's accepted evidence only.
    pub evidence_linked_research_claims: Vec<EvidenceLinkedResearchClaim>,
}

/// Typed result of one strong-model task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "markdown_research_model_task_kind",
    content = "response",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StrongMarkdownResearchModelResponse {
    /// Question evaluation output.
    ResearchQuestionEvaluation(ResearchQuestionClarificationModelOutput),
    /// Isolated model-only answer.
    ModelKnowledgeOnlyAnswerGeneration(ModelKnowledgeOnlyAnswer),
    /// Navigation decisions.
    MarkdownCorpusNavigationBranchSelection(MarkdownCorpusNavigationBranchSelectionResponse),
    /// Branch document relevance report.
    MarkdownCorpusNavigationBranchDocumentRelevanceReport(
        MarkdownCorpusNavigationBranchDocumentRelevanceReport,
    ),
    /// One proposed read request.
    ResearchDocumentReadRequest(ResearchDocumentReadRequest),
    /// One bounded source-review decision.
    MarkdownSourceReview(MarkdownSourceReviewDecision),
    /// Proposed evidence-linked claims.
    EvidenceLinkedResearchClaimGeneration(EvidenceLinkedResearchClaimGenerationResponse),
    /// Claims-only answer.
    EvidenceLinkedResearchClaimsAnswerGeneration(EvidenceLinkedResearchClaimsAnswer),
    /// Source-attributed final composition.
    SourceAttributedAnswerComposition(SourceAttributedAnswerComposition),
}

/// Cheap-model task containing exactly one authorized source segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbatimSourceEvidenceExtractionTask {
    /// Stable logical model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Runtime-owned extraction request ID.
    pub verbatim_source_evidence_extraction_request_id: VerbatimSourceEvidenceExtractionRequestId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Clarified question, without the rest of the frozen brief.
    pub clarified_research_question: String,
    /// Precise extraction goal.
    pub verbatim_source_evidence_extraction_goal: String,
    /// The only corpus body visible to the cheap model.
    pub authorized_markdown_source_segment: AuthorizedMarkdownSourceSegmentInput,
    /// Closed task schema version.
    pub markdown_research_model_task_schema_version: u32,
}

impl VerbatimSourceEvidenceExtractionTask {
    /// Validates that the extraction task contains one bounded authorized segment.
    pub fn validate(&self) -> Result<()> {
        if self.markdown_research_model_task_schema_version
            != MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION
        {
            return Err(task_validation("unsupported Markdown research model task schema version"));
        }
        validate_task_text(
            "clarified_research_question",
            &self.clarified_research_question,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        validate_task_text(
            "verbatim_source_evidence_extraction_goal",
            &self.verbatim_source_evidence_extraction_goal,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        self.authorized_markdown_source_segment.validate()?;
        validate_serialized_task_size(self)
    }

    /// Decodes and validates a candidate set for this exact authorized segment.
    pub fn decode_response_json(
        &self,
        response_json: &str,
    ) -> Result<VerbatimSourceEvidenceCandidateSet> {
        self.validate()?;
        reject_oversized_response(response_json)?;
        let envelope: ExtractionModelResponseEnvelope = decode_closed_json(response_json)?;
        if envelope.markdown_research_model_task_id != self.markdown_research_model_task_id
            || envelope.document_research_branch_task_id != self.document_research_branch_task_id
            || envelope.markdown_research_model_task_schema_version
                != MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION
        {
            return Err(model_response(
                "extraction response envelope does not match the dispatched task",
            ));
        }
        validate_extraction_response(self, envelope.response)
    }
}

/// One quote candidate with offsets relative to the authorized segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbatimSourceEvidenceCandidate {
    /// Inclusive UTF-8 byte offset relative to the segment.
    pub verbatim_source_evidence_start_byte_offset_in_segment: u64,
    /// Exclusive UTF-8 byte offset relative to the segment.
    pub verbatim_source_evidence_end_byte_offset_in_segment: u64,
    /// Exact quote at the relative offsets.
    pub verbatim_source_evidence_quote: String,
}

/// Cheap-model response bound to the one authorized segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbatimSourceEvidenceCandidateSet {
    /// Extraction request being answered.
    pub verbatim_source_evidence_extraction_request_id: VerbatimSourceEvidenceExtractionRequestId,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Authorized source document.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Authorized source document version.
    pub markdown_source_document_version_id: MarkdownSourceDocumentVersionId,
    /// Authorized source segment.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// Hash of the exact authorized segment.
    pub markdown_source_segment_hash: String,
    /// Bounded quote candidates; the empty set is valid.
    pub verbatim_source_evidence_candidates: Vec<VerbatimSourceEvidenceCandidate>,
}

/// Deep model seam used by the execution engine.
#[async_trait]
pub trait MarkdownResearchModelGateway: Send + Sync {
    /// Executes one closed strong-model task and returns a validated typed response.
    async fn execute_strong_markdown_research_task(
        &self,
        task: StrongMarkdownResearchModelTask,
    ) -> Result<StrongMarkdownResearchModelResponse>;

    /// Extracts candidate quotes from exactly one authorized segment.
    async fn extract_verbatim_source_evidence_candidates(
        &self,
        task: VerbatimSourceEvidenceExtractionTask,
    ) -> Result<VerbatimSourceEvidenceCandidateSet>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrongModelResponseEnvelope {
    markdown_research_model_task_id: MarkdownResearchModelTaskId,
    markdown_research_model_task_kind: MarkdownResearchModelTaskKind,
    document_research_branch_task_id: Option<DocumentResearchBranchTaskId>,
    markdown_research_model_task_schema_version: u32,
    response: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionModelResponseEnvelope {
    markdown_research_model_task_id: MarkdownResearchModelTaskId,
    document_research_branch_task_id: DocumentResearchBranchTaskId,
    markdown_research_model_task_schema_version: u32,
    response: VerbatimSourceEvidenceCandidateSet,
}

fn decode_and_validate_strong_response(
    task: &StrongMarkdownResearchModelTask,
    response: Value,
) -> Result<StrongMarkdownResearchModelResponse> {
    match task {
        StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(task) => {
            let response: ResearchQuestionClarificationModelOutput = decode_closed_value(response)?;
            response
                .validate(task.research_question_clarification_revision)
                .map_err(|error| invalid_model_value("question evaluation", error))?;
            if response.document_research_brief_draft.original_user_question
                != task.original_user_question
            {
                return Err(model_response(
                    "question evaluation response changed the original user question",
                ));
            }
            Ok(StrongMarkdownResearchModelResponse::ResearchQuestionEvaluation(response))
        }
        StrongMarkdownResearchModelTask::ModelKnowledgeOnlyAnswerGeneration(task) => {
            let response: ModelKnowledgeOnlyAnswer = decode_closed_value(response)?;
            if response.model_knowledge_only_answer_id != task.markdown_research_model_task_id
                || response.markdown_research_execution_id != task.markdown_research_execution_id
            {
                return Err(model_response(
                    "model-knowledge-only answer contains an unauthorized ID",
                ));
            }
            response
                .validate_shape()
                .map_err(|error| invalid_model_value("model-knowledge-only answer", error))?;
            Ok(StrongMarkdownResearchModelResponse::ModelKnowledgeOnlyAnswerGeneration(response))
        }
        StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchSelection(task) => {
            let response: MarkdownCorpusNavigationBranchSelectionResponse =
                decode_closed_value(response)?;
            validate_navigation_selection_response(task, &response)?;
            Ok(StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchSelection(
                response,
            ))
        }
        StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
            task,
        ) => {
            let response: MarkdownCorpusNavigationBranchDocumentRelevanceReport =
                decode_closed_value(response)?;
            validate_branch_report_response(task, &response)?;
            Ok(StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchDocumentRelevanceReport(response))
        }
        StrongMarkdownResearchModelTask::ResearchDocumentReadRequest(task) => {
            let response: ResearchDocumentReadRequest = decode_closed_value(response)?;
            validate_read_request_response(task, &response)?;
            Ok(StrongMarkdownResearchModelResponse::ResearchDocumentReadRequest(response))
        }
        StrongMarkdownResearchModelTask::MarkdownSourceReview(task) => {
            let response: MarkdownSourceReviewDecision = decode_closed_value(response)?;
            validate_source_review_response(task, &response)?;
            Ok(StrongMarkdownResearchModelResponse::MarkdownSourceReview(response))
        }
        StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimGeneration(task) => {
            let response: EvidenceLinkedResearchClaimGenerationResponse =
                decode_closed_value(response)?;
            validate_claim_generation_response(task, &response)?;
            Ok(StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimGeneration(response))
        }
        StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimsAnswerGeneration(task) => {
            let response: EvidenceLinkedResearchClaimsAnswer = decode_closed_value(response)?;
            validate_claims_answer_response(task, &response)?;
            Ok(StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimsAnswerGeneration(
                response,
            ))
        }
        StrongMarkdownResearchModelTask::SourceAttributedAnswerComposition(task) => {
            let response: SourceAttributedAnswerComposition = decode_closed_value(response)?;
            validate_composition_response(task, &response)?;
            Ok(StrongMarkdownResearchModelResponse::SourceAttributedAnswerComposition(response))
        }
    }
}

fn validate_question_evaluation_task(task: &ResearchQuestionEvaluationTask) -> Result<()> {
    validate_task_text(
        "original_user_question",
        &task.original_user_question,
        MAX_RESEARCH_TEXT_BYTES,
    )?;
    validate_count(
        "research_question_clarification_dialogue",
        task.research_question_clarification_dialogue.len(),
    )?;
    for message in &task.research_question_clarification_dialogue {
        validate_task_text(
            "dialogue_message_text",
            &message.dialogue_message_text,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        if message.research_question_clarification_revision
            > task.research_question_clarification_revision
        {
            return Err(task_validation("clarification dialogue contains a future revision"));
        }
    }
    if let Some(draft) = &task.document_research_brief_draft {
        draft
            .clone()
            .freeze()
            .map_err(|error| task_validation(format!("invalid brief draft: {error}")))?;
    }
    validate_history(&task.allowed_completed_research_context)
}

fn validate_brief_and_history(
    brief: &FrozenDocumentResearchBrief,
    history: &[AllowedCompletedResearchContext],
) -> Result<()> {
    brief.validate()?;
    validate_history(history)
}

fn validate_history(history: &[AllowedCompletedResearchContext]) -> Result<()> {
    validate_count("allowed_completed_research_context", history.len())?;
    for context in history {
        validate_task_text(
            "completed_research_context_label",
            &context.completed_research_context_label,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        validate_task_text(
            "completed_research_context_text",
            &context.completed_research_context_text,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
    }
    Ok(())
}

fn validate_read_request_task(task: &ResearchDocumentReadRequestTask) -> Result<()> {
    task.frozen_document_research_brief.validate()?;
    validate_count(
        "committed_branch_document_reports",
        task.committed_branch_document_reports.len(),
    )?;
    validate_count(
        "candidate_markdown_source_segments",
        task.candidate_markdown_source_segments.len(),
    )?;
    validate_evidence_contexts(
        &task.accepted_verbatim_source_evidence,
        &task.markdown_research_execution_id,
    )?;

    let mut segment_ids = BTreeSet::new();
    for segment in &task.candidate_markdown_source_segments {
        if segment.markdown_source_segment_end_byte_offset_in_document
            <= segment.markdown_source_segment_start_byte_offset_in_document
            || !segment_ids.insert(segment.markdown_source_segment_id.as_str())
        {
            return Err(task_validation(
                "candidate segment metadata contains an invalid range or duplicate ID",
            ));
        }
        validate_task_text(
            "markdown_source_segment_hash",
            &segment.markdown_source_segment_hash,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        if let Some(heading) = &segment.markdown_source_segment_section_heading {
            validate_task_text(
                "markdown_source_segment_section_heading",
                heading,
                MAX_RESEARCH_TEXT_BYTES,
            )?;
        }
    }

    let mut report_branches = BTreeSet::new();
    for report in &task.committed_branch_document_reports {
        if !report_branches.insert(report.document_research_branch_task_id.as_str()) {
            return Err(task_validation("duplicate committed branch report"));
        }
        validate_count_with_limit(
            "candidate_markdown_source_document_ids",
            report.candidate_markdown_source_document_ids.len(),
            MAX_NAVIGATION_LINKS_PER_NODE,
        )?;
        validate_count(
            "selected_markdown_source_document_ids",
            report.selected_markdown_source_document_ids.len(),
        )?;
        ensure_unique_ids(
            "reported candidate document",
            report.candidate_markdown_source_document_ids.iter().map(|id| id.as_str()),
        )?;
        ensure_unique_ids(
            "reported selected document",
            report.selected_markdown_source_document_ids.iter().map(|id| id.as_str()),
        )?;
        let candidates: BTreeSet<_> =
            report.candidate_markdown_source_document_ids.iter().map(|id| id.as_str()).collect();
        if report
            .selected_markdown_source_document_ids
            .iter()
            .any(|id| !candidates.contains(id.as_str()))
        {
            return Err(task_validation(
                "committed report selects a document outside its candidates",
            ));
        }
        validate_task_text(
            "markdown_corpus_navigation_branch_document_report_summary",
            &report.markdown_corpus_navigation_branch_document_report_summary,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
    }
    let selected_for_branch: BTreeSet<_> = task
        .committed_branch_document_reports
        .iter()
        .filter(|report| {
            report.document_research_branch_task_id == task.document_research_branch_task_id
        })
        .flat_map(|report| report.selected_markdown_source_document_ids.iter())
        .map(|id| id.as_str())
        .collect();
    if task
        .candidate_markdown_source_segments
        .iter()
        .any(|segment| !selected_for_branch.contains(segment.markdown_source_document_id.as_str()))
    {
        return Err(task_validation(
            "candidate segment does not belong to a document selected for this branch",
        ));
    }
    Ok(())
}

fn validate_source_review_task(task: &MarkdownSourceReviewTask) -> Result<()> {
    task.frozen_document_research_brief.validate()?;
    task.authorized_markdown_source_segment.validate()?;
    validate_evidence_contexts(
        &task.accepted_verbatim_source_evidence,
        &task.markdown_research_execution_id,
    )
}

fn validate_composition_task(task: &SourceAttributedAnswerCompositionTask) -> Result<()> {
    task.frozen_document_research_brief.validate()?;
    task.model_knowledge_only_answer
        .validate_shape()
        .map_err(|error| task_validation(format!("invalid model-only answer input: {error}")))?;
    task.evidence_linked_research_claims_answer
        .validate_shape()
        .map_err(|error| task_validation(format!("invalid claims answer input: {error}")))?;
    if task.model_knowledge_only_answer.markdown_research_execution_id
        != task.markdown_research_execution_id
        || task.evidence_linked_research_claims_answer.markdown_research_execution_id
            != task.markdown_research_execution_id
    {
        return Err(task_validation("composition inputs do not belong to the task execution"));
    }
    validate_committed_claims(
        &task.committed_evidence_linked_research_claims,
        &task.markdown_research_execution_id,
    )?;
    let claims: BTreeSet<_> = task
        .committed_evidence_linked_research_claims
        .iter()
        .map(|claim| claim.evidence_linked_research_claim_id.as_str())
        .collect();
    if task
        .evidence_linked_research_claims_answer
        .supporting_evidence_linked_research_claim_ids
        .iter()
        .any(|id| !claims.contains(id.as_str()))
    {
        return Err(task_validation("claims answer input references an uncommitted claim"));
    }
    validate_count("public_source_citations", task.public_source_citations.len())?;
    ensure_unique_ids(
        "public citation",
        task.public_source_citations
            .iter()
            .map(|citation| citation.public_source_citation_id.as_str()),
    )?;
    for citation in &task.public_source_citations {
        validate_task_text(
            "markdown_source_document_title",
            &citation.markdown_source_document_title,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        validate_task_text(
            "public_source_citation_quote",
            &citation.public_source_citation_quote,
            MAX_EVIDENCE_QUOTE_BYTES,
        )?;
        validate_task_text(
            "markdown_source_document_version_content_hash",
            &citation.markdown_source_document_version_content_hash,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
    }
    Ok(())
}

fn validate_evidence_contexts(
    evidence: &[AcceptedVerbatimSourceEvidenceModelContext],
    execution_id: &MarkdownResearchExecutionId,
) -> Result<()> {
    validate_count("accepted_verbatim_source_evidence", evidence.len())?;
    ensure_unique_ids(
        "accepted evidence",
        evidence.iter().map(|item| item.verbatim_source_evidence_id.as_str()),
    )?;
    for item in evidence {
        if item.markdown_research_execution_id != *execution_id {
            return Err(task_validation("accepted evidence belongs to another execution"));
        }
        validate_task_text(
            "verbatim_source_evidence_quote",
            &item.verbatim_source_evidence_quote,
            MAX_EVIDENCE_QUOTE_BYTES,
        )?;
    }
    Ok(())
}

fn validate_committed_claims(
    claims: &[EvidenceLinkedResearchClaim],
    execution_id: &MarkdownResearchExecutionId,
) -> Result<()> {
    validate_count("committed_evidence_linked_research_claims", claims.len())?;
    ensure_unique_ids(
        "committed claim",
        claims.iter().map(|claim| claim.evidence_linked_research_claim_id.as_str()),
    )?;
    for claim in claims {
        if claim.markdown_research_execution_id != *execution_id {
            return Err(task_validation("committed claim belongs to another execution"));
        }
        claim
            .validate_shape()
            .map_err(|error| task_validation(format!("invalid committed claim: {error}")))?;
        validate_claim_text_lists(claim, false)?;
    }
    Ok(())
}

fn validate_navigation_selection_response(
    task: &MarkdownCorpusNavigationBranchSelectionTask,
    response: &MarkdownCorpusNavigationBranchSelectionResponse,
) -> Result<()> {
    if response.markdown_corpus_navigation_candidate_set_id
        != task.markdown_corpus_navigation_candidate_set_id
    {
        return Err(model_response("navigation response references another candidate set"));
    }
    validate_response_count_with_limit(
        "markdown_corpus_navigation_branch_selections",
        response.markdown_corpus_navigation_branch_selections.len(),
        MAX_NAVIGATION_LINKS_PER_NODE,
    )?;
    let expected: BTreeSet<_> = task
        .markdown_corpus_navigation_node_candidates
        .iter()
        .map(|candidate| candidate.markdown_corpus_navigation_node_id.as_str())
        .collect();
    let actual: BTreeSet<_> = response
        .markdown_corpus_navigation_branch_selections
        .iter()
        .map(|selection| selection.markdown_corpus_navigation_node_id.as_str())
        .collect();
    if actual.len() != response.markdown_corpus_navigation_branch_selections.len()
        || actual != expected
    {
        return Err(model_response(
            "navigation response must cover every candidate ID exactly once",
        ));
    }
    for selection in &response.markdown_corpus_navigation_branch_selections {
        validate_response_text(
            "markdown_corpus_navigation_node_relevance_explanation",
            &selection.markdown_corpus_navigation_node_relevance_explanation,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        validate_response_text(
            "expected_research_information_to_resolve_question",
            &selection.expected_research_information_to_resolve_question,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        if selection.markdown_corpus_navigation_branch_priority == 0 {
            return Err(model_response("navigation branch priority must be positive"));
        }
    }
    Ok(())
}

fn validate_branch_report_response(
    task: &MarkdownCorpusNavigationBranchDocumentRelevanceReportTask,
    response: &MarkdownCorpusNavigationBranchDocumentRelevanceReport,
) -> Result<()> {
    if response.document_research_branch_task_id != task.document_research_branch_task_id
        || response.markdown_corpus_navigation_node_id != task.markdown_corpus_navigation_node_id
    {
        return Err(model_response(
            "branch report contains an unauthorized branch or navigation ID",
        ));
    }
    validate_response_count_with_limit(
        "candidate_markdown_source_document_ids",
        response.candidate_markdown_source_document_ids.len(),
        MAX_NAVIGATION_LINKS_PER_NODE,
    )?;
    validate_response_count(
        "selected_markdown_source_document_ids",
        response.selected_markdown_source_document_ids.len(),
    )?;
    let expected: BTreeSet<_> = task
        .markdown_source_document_candidates
        .iter()
        .map(|candidate| candidate.markdown_source_document_id.as_str())
        .collect();
    let candidates: BTreeSet<_> =
        response.candidate_markdown_source_document_ids.iter().map(|id| id.as_str()).collect();
    let selected: BTreeSet<_> =
        response.selected_markdown_source_document_ids.iter().map(|id| id.as_str()).collect();
    if candidates != expected
        || candidates.len() != response.candidate_markdown_source_document_ids.len()
        || selected.len() != response.selected_markdown_source_document_ids.len()
        || !selected.is_subset(&expected)
    {
        return Err(model_response(
            "branch report contains missing, duplicate, or fabricated document IDs",
        ));
    }
    validate_response_text(
        "markdown_corpus_navigation_branch_document_report_summary",
        &response.markdown_corpus_navigation_branch_document_report_summary,
        MAX_RESEARCH_TEXT_BYTES,
    )
}

fn validate_read_request_response(
    task: &ResearchDocumentReadRequestTask,
    response: &ResearchDocumentReadRequest,
) -> Result<()> {
    if response.research_document_read_request_id != task.research_document_read_request_id
        || response.document_research_branch_task_id != task.document_research_branch_task_id
    {
        return Err(model_response("read request contains an unauthorized request or branch ID"));
    }
    let pair_is_candidate = task.candidate_markdown_source_segments.iter().any(|candidate| {
        candidate.markdown_source_document_id == response.markdown_source_document_id
            && candidate.markdown_source_segment_id == response.markdown_source_segment_id
    });
    let document_was_selected = task.committed_branch_document_reports.iter().any(|report| {
        report.document_research_branch_task_id == task.document_research_branch_task_id
            && report
                .selected_markdown_source_document_ids
                .contains(&response.markdown_source_document_id)
    });
    if !pair_is_candidate || !document_was_selected {
        return Err(model_response(
            "read request contains a document or segment outside committed candidates",
        ));
    }
    validate_response_text(
        "unresolved_research_question",
        &response.unresolved_research_question,
        MAX_RESEARCH_TEXT_BYTES,
    )?;
    validate_response_text(
        "expected_research_information_to_resolve_question",
        &response.expected_research_information_to_resolve_question,
        MAX_RESEARCH_TEXT_BYTES,
    )?;
    validate_response_text(
        "markdown_source_document_selection_explanation",
        &response.markdown_source_document_selection_explanation,
        MAX_RESEARCH_TEXT_BYTES,
    )
}

fn validate_source_review_response(
    task: &MarkdownSourceReviewTask,
    response: &MarkdownSourceReviewDecision,
) -> Result<()> {
    let segment = &task.authorized_markdown_source_segment;
    if response.research_document_read_request_id != task.research_document_read_request_id
        || response.document_research_branch_task_id != task.document_research_branch_task_id
        || response.markdown_source_document_id != segment.markdown_source_document_id
        || response.markdown_source_segment_id != segment.markdown_source_segment_id
    {
        return Err(model_response(
            "source review contains an unauthorized request, branch, document, or segment ID",
        ));
    }
    validate_response_text(
        "markdown_source_review_summary",
        &response.markdown_source_review_summary,
        MAX_RESEARCH_TEXT_BYTES,
    )?;
    match response.markdown_source_follow_up_action {
        MarkdownSourceFollowUpAction::ExtractVerbatimSourceEvidence => {
            let goal = response
                .verbatim_source_evidence_extraction_goal
                .as_deref()
                .ok_or_else(|| model_response("evidence extraction action needs a goal"))?;
            validate_response_text(
                "verbatim_source_evidence_extraction_goal",
                goal,
                MAX_RESEARCH_TEXT_BYTES,
            )?;
            if !response.triggering_verbatim_source_evidence_ids.is_empty()
                || response.markdown_corpus_navigation_branch_close_reason.is_some()
            {
                return Err(model_response(
                    "evidence extraction action contains fields for another action",
                ));
            }
        }
        MarkdownSourceFollowUpAction::ReadAdditionalMarkdownSourceSegment => {
            require_no_action_details(response)?;
        }
        MarkdownSourceFollowUpAction::ExpandMarkdownCorpusNavigationScope => {
            if response.verbatim_source_evidence_extraction_goal.is_some()
                || response.markdown_corpus_navigation_branch_close_reason.is_some()
                || response.triggering_verbatim_source_evidence_ids.is_empty()
            {
                return Err(model_response(
                    "scope expansion needs evidence IDs and no other action fields",
                ));
            }
            validate_response_count(
                "triggering_verbatim_source_evidence_ids",
                response.triggering_verbatim_source_evidence_ids.len(),
            )?;
            let allowed: BTreeSet<_> = task
                .accepted_verbatim_source_evidence
                .iter()
                .map(|evidence| evidence.verbatim_source_evidence_id.as_str())
                .collect();
            let actual: BTreeSet<_> = response
                .triggering_verbatim_source_evidence_ids
                .iter()
                .map(|id| id.as_str())
                .collect();
            if actual.len() != response.triggering_verbatim_source_evidence_ids.len()
                || !actual.is_subset(&allowed)
            {
                return Err(model_response(
                    "scope expansion contains duplicate or unaccepted evidence IDs",
                ));
            }
        }
        MarkdownSourceFollowUpAction::CloseMarkdownCorpusNavigationBranch => {
            if response.verbatim_source_evidence_extraction_goal.is_some()
                || !response.triggering_verbatim_source_evidence_ids.is_empty()
                || response.markdown_corpus_navigation_branch_close_reason.is_none()
            {
                return Err(model_response("branch close action needs only a close reason"));
            }
        }
    }
    Ok(())
}

fn require_no_action_details(response: &MarkdownSourceReviewDecision) -> Result<()> {
    if response.verbatim_source_evidence_extraction_goal.is_some()
        || !response.triggering_verbatim_source_evidence_ids.is_empty()
        || response.markdown_corpus_navigation_branch_close_reason.is_some()
    {
        return Err(model_response(
            "read-additional-segment action contains fields for another action",
        ));
    }
    Ok(())
}

fn validate_claim_generation_response(
    task: &EvidenceLinkedResearchClaimGenerationTask,
    response: &EvidenceLinkedResearchClaimGenerationResponse,
) -> Result<()> {
    validate_response_count(
        "evidence_linked_research_claims",
        response.evidence_linked_research_claims.len(),
    )?;
    if response.evidence_linked_research_claims.is_empty() {
        return Err(model_response("claim generation response must contain at least one claim"));
    }
    let authorized_claims: BTreeSet<_> =
        task.authorized_evidence_linked_research_claim_ids.iter().map(|id| id.as_str()).collect();
    let accepted_evidence: BTreeSet<_> = task
        .accepted_verbatim_source_evidence
        .iter()
        .map(|evidence| evidence.verbatim_source_evidence_id.as_str())
        .collect();
    let mut actual_claims = BTreeSet::new();
    for claim in &response.evidence_linked_research_claims {
        if !authorized_claims.contains(claim.evidence_linked_research_claim_id.as_str())
            || !actual_claims.insert(claim.evidence_linked_research_claim_id.as_str())
            || claim.markdown_research_execution_id != task.markdown_research_execution_id
        {
            return Err(model_response(
                "claim generation contains a fabricated, duplicate, or foreign claim ID",
            ));
        }
        claim
            .validate_shape()
            .map_err(|error| invalid_model_value("evidence-linked claim", error))?;
        validate_claim_text_lists(claim, true)?;
        if claim.research_claim_evidence_relationships.iter().any(|relationship| {
            !accepted_evidence.contains(relationship.verbatim_source_evidence_id.as_str())
        }) {
            return Err(model_response(
                "claim generation references evidence outside the accepted set",
            ));
        }
    }
    Ok(())
}

fn validate_claim_text_lists(claim: &EvidenceLinkedResearchClaim, response: bool) -> Result<()> {
    let validate_count_fn = if response { validate_response_count } else { validate_count };
    validate_count_fn(
        "research_claim_evidence_relationships",
        claim.research_claim_evidence_relationships.len(),
    )?;
    validate_count_fn(
        "evidence_linked_research_claim_applicability_conditions",
        claim.evidence_linked_research_claim_applicability_conditions.len(),
    )?;
    validate_count_fn(
        "evidence_linked_research_claim_exceptions",
        claim.evidence_linked_research_claim_exceptions.len(),
    )?;
    for text in claim
        .evidence_linked_research_claim_applicability_conditions
        .iter()
        .chain(claim.evidence_linked_research_claim_exceptions.iter())
    {
        if response {
            validate_response_text("claim condition or exception", text, MAX_CLAIM_TEXT_BYTES)?;
        } else {
            validate_task_text("claim condition or exception", text, MAX_CLAIM_TEXT_BYTES)?;
        }
    }
    Ok(())
}

fn validate_claims_answer_response(
    task: &EvidenceLinkedResearchClaimsAnswerGenerationTask,
    response: &EvidenceLinkedResearchClaimsAnswer,
) -> Result<()> {
    if response.evidence_linked_research_claims_answer_id != task.markdown_research_model_task_id
        || response.markdown_research_execution_id != task.markdown_research_execution_id
    {
        return Err(model_response(
            "claims answer contains an unauthorized answer or execution ID",
        ));
    }
    response.validate_shape().map_err(|error| invalid_model_value("claims answer", error))?;
    validate_response_count(
        "supporting_evidence_linked_research_claim_ids",
        response.supporting_evidence_linked_research_claim_ids.len(),
    )?;
    let allowed: BTreeSet<_> = task
        .committed_evidence_linked_research_claims
        .iter()
        .map(|claim| claim.evidence_linked_research_claim_id.as_str())
        .collect();
    if response
        .supporting_evidence_linked_research_claim_ids
        .iter()
        .any(|id| !allowed.contains(id.as_str()))
    {
        return Err(model_response("claims answer references an uncommitted claim ID"));
    }
    Ok(())
}

fn validate_composition_response(
    task: &SourceAttributedAnswerCompositionTask,
    response: &SourceAttributedAnswerComposition,
) -> Result<()> {
    if response.source_attributed_answer_composition_style
        != task.requested_answer_composition_style
        || response.model_knowledge_only_answer_id
            != task.model_knowledge_only_answer.model_knowledge_only_answer_id
        || response.evidence_linked_research_claims_answer_id
            != task.evidence_linked_research_claims_answer.evidence_linked_research_claims_answer_id
        || response.answer_projection_schema_version != ANSWER_PROJECTION_SCHEMA_VERSION
    {
        return Err(model_response(
            "answer composition contains an unauthorized style, answer ID, or schema version",
        ));
    }
    response.validate_shape().map_err(|error| invalid_model_value("answer composition", error))?;
    validate_response_count(
        "source_attributed_answer_segments",
        response.source_attributed_answer_segments.len(),
    )?;
    let claims: BTreeSet<_> = task
        .committed_evidence_linked_research_claims
        .iter()
        .map(|claim| claim.evidence_linked_research_claim_id.as_str())
        .collect();
    let citations: BTreeSet<_> = task
        .public_source_citations
        .iter()
        .map(|citation| citation.public_source_citation_id.as_str())
        .collect();
    for segment in &response.source_attributed_answer_segments {
        if segment
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .any(|id| !claims.contains(id.as_str()))
            || segment
                .supporting_public_source_citation_ids
                .iter()
                .any(|id| !citations.contains(id.as_str()))
        {
            return Err(model_response(
                "answer segment contains a fabricated claim or citation ID",
            ));
        }
        let has_claims = !segment.supporting_evidence_linked_research_claim_ids.is_empty();
        let has_citations = !segment.supporting_public_source_citation_ids.is_empty();
        let has_notice = segment.model_knowledge_unverified_notice.is_some();
        let valid_source_shape = match segment.source_attributed_answer_segment_source_type {
            SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaims => {
                has_claims && has_citations && !has_notice
            }
            SourceAttributedAnswerSegmentSourceType::ModelKnowledgeOnly => {
                !has_claims && !has_citations && has_notice
            }
            SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaimsAndModelKnowledge => {
                has_claims && has_citations && has_notice
            }
        };
        if !valid_source_shape {
            return Err(model_response(
                "answer segment fields do not match its declared source type",
            ));
        }
    }
    Ok(())
}

fn validate_extraction_response(
    task: &VerbatimSourceEvidenceExtractionTask,
    response: VerbatimSourceEvidenceCandidateSet,
) -> Result<VerbatimSourceEvidenceCandidateSet> {
    let segment = &task.authorized_markdown_source_segment;
    if response.verbatim_source_evidence_extraction_request_id
        != task.verbatim_source_evidence_extraction_request_id
        || response.document_research_branch_task_id != task.document_research_branch_task_id
        || response.markdown_source_document_id != segment.markdown_source_document_id
        || response.markdown_source_document_version_id
            != segment.markdown_source_document_version_id
        || response.markdown_source_segment_id != segment.markdown_source_segment_id
        || response.markdown_source_segment_hash != segment.markdown_source_segment_hash
    {
        return Err(model_response(
            "extraction response contains an unauthorized request, branch, document, version, segment, or hash",
        ));
    }
    validate_response_count(
        "verbatim_source_evidence_candidates",
        response.verbatim_source_evidence_candidates.len(),
    )?;
    let text = segment.canonical_markdown_source_segment_text.as_bytes();
    let mut ranges = BTreeSet::new();
    for candidate in &response.verbatim_source_evidence_candidates {
        let Ok(start) =
            usize::try_from(candidate.verbatim_source_evidence_start_byte_offset_in_segment)
        else {
            return Err(model_response("evidence candidate start offset is too large"));
        };
        let Ok(end) =
            usize::try_from(candidate.verbatim_source_evidence_end_byte_offset_in_segment)
        else {
            return Err(model_response("evidence candidate end offset is too large"));
        };
        if start >= end
            || end > text.len()
            || !segment.canonical_markdown_source_segment_text.is_char_boundary(start)
            || !segment.canonical_markdown_source_segment_text.is_char_boundary(end)
            || !ranges.insert((start, end))
        {
            return Err(model_response(
                "evidence candidate contains invalid or duplicate relative offsets",
            ));
        }
        validate_response_text(
            "verbatim_source_evidence_quote",
            &candidate.verbatim_source_evidence_quote,
            MAX_EVIDENCE_QUOTE_BYTES,
        )?;
        if &text[start..end] != candidate.verbatim_source_evidence_quote.as_bytes() {
            return Err(model_response(
                "evidence candidate quote does not match the authorized segment offsets",
            ));
        }
    }
    Ok(response)
}

fn decode_closed_json<T: DeserializeOwned>(json: &str) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|error| model_response(format!("response violates closed JSON schema: {error}")))
}

fn decode_closed_value<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|error| model_response(format!("response violates task schema: {error}")))
}

fn reject_oversized_response(response_json: &str) -> Result<()> {
    if response_json.len() > MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES {
        return Err(model_response(format!(
            "model response exceeds {MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_task_text(name: &str, value: &str, maximum_bytes: usize) -> Result<()> {
    validate_text(name, value, maximum_bytes).map_err(task_validation)
}

fn validate_response_text(name: &str, value: &str, maximum_bytes: usize) -> Result<()> {
    validate_text(name, value, maximum_bytes).map_err(model_response)
}

fn validate_text(name: &str, value: &str, maximum_bytes: usize) -> std::result::Result<(), String> {
    if value.trim().is_empty() || value.len() > maximum_bytes {
        return Err(format!(
            "{name} must contain 1..={maximum_bytes} bytes of non-whitespace text"
        ));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(format!("{name} contains an unsupported control character"));
    }
    Ok(())
}

fn validate_count(name: &str, count: usize) -> Result<()> {
    validate_count_with_limit(name, count, MAX_MARKDOWN_RESEARCH_MODEL_ITEMS)
}

fn validate_count_with_limit(name: &str, count: usize, maximum_items: usize) -> Result<()> {
    if count > maximum_items {
        return Err(task_validation(format!("{name} exceeds {maximum_items} items")));
    }
    Ok(())
}

fn validate_response_count(name: &str, count: usize) -> Result<()> {
    validate_response_count_with_limit(name, count, MAX_MARKDOWN_RESEARCH_MODEL_ITEMS)
}

fn validate_response_count_with_limit(
    name: &str,
    count: usize,
    maximum_items: usize,
) -> Result<()> {
    if count > maximum_items {
        return Err(model_response(format!("{name} exceeds {maximum_items} items")));
    }
    Ok(())
}

fn validate_serialized_task_size<T: Serialize>(task: &T) -> Result<()> {
    let bytes = serde_json::to_vec(task).map_err(|error| {
        task_validation(format!("cannot serialize Markdown research model task: {error}"))
    })?;
    if bytes.len() > MAX_MARKDOWN_RESEARCH_MODEL_TASK_INPUT_BYTES {
        return Err(task_validation(format!(
            "model task input exceeds {MAX_MARKDOWN_RESEARCH_MODEL_TASK_INPUT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn ensure_unique_ids<'a>(name: &str, identifiers: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut unique = BTreeSet::new();
    if identifiers.into_iter().any(|identifier| !unique.insert(identifier)) {
        return Err(task_validation(format!("duplicate {name} ID")));
    }
    Ok(())
}

fn task_validation(message: impl Into<String>) -> RuntimeError {
    RuntimeError::validation(RuntimeStage::Model, message)
}

fn model_response(message: impl Into<String>) -> RuntimeError {
    RuntimeError::ModelResponse { message: message.into() }
}

fn invalid_model_value(context: &str, error: RuntimeError) -> RuntimeError {
    model_response(format!("invalid {context}: {error}"))
}

/// One raw strong-model script consumed by the Fixture Adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedStrongMarkdownResearchModelResponse {
    /// Stable task ID used as a concurrency-independent lookup key.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Exact strong task kind expected by the script.
    pub markdown_research_model_task_kind: MarkdownResearchModelTaskKind,
    /// Exact branch ownership expected by the script.
    pub document_research_branch_task_id: Option<DocumentResearchBranchTaskId>,
    /// Raw response envelope decoded through the production contract path.
    pub response_json: String,
}

/// One raw cheap-extraction script consumed by the Fixture Adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedVerbatimSourceEvidenceExtractionResponse {
    /// Stable task ID used as a concurrency-independent lookup key.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Exact branch ownership expected by the script.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Raw response envelope decoded through the production contract path.
    pub response_json: String,
}

/// A task recorded by the Fixture Adapter before its script is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureMarkdownResearchModelCall {
    /// Strong-model task input.
    Strong(StrongMarkdownResearchModelTask),
    /// Cheap-model task input.
    VerbatimSourceEvidenceExtraction(VerbatimSourceEvidenceExtractionTask),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StrongFixtureKey {
    task_id: MarkdownResearchModelTaskId,
    task_kind: MarkdownResearchModelTaskKind,
    branch_task_id: Option<DocumentResearchBranchTaskId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ExtractionFixtureKey {
    task_id: MarkdownResearchModelTaskId,
    branch_task_id: DocumentResearchBranchTaskId,
}

#[derive(Debug, Default)]
struct FixtureState {
    strong_responses: BTreeMap<StrongFixtureKey, VecDeque<String>>,
    extraction_responses: BTreeMap<ExtractionFixtureKey, VecDeque<String>>,
    calls: Vec<FixtureMarkdownResearchModelCall>,
}

/// Deterministic Adapter whose scripts are matched by stable task and branch IDs.
#[derive(Debug, Default)]
pub struct FixtureMarkdownResearchModelGateway {
    state: Mutex<FixtureState>,
}

impl FixtureMarkdownResearchModelGateway {
    /// Builds a Fixture Adapter and rejects duplicate script keys immediately.
    pub fn new(
        strong_responses: Vec<ScriptedStrongMarkdownResearchModelResponse>,
        extraction_responses: Vec<ScriptedVerbatimSourceEvidenceExtractionResponse>,
    ) -> Result<Self> {
        let mut state = FixtureState::default();
        let mut strong_metadata = BTreeMap::new();
        for script in strong_responses {
            let key = StrongFixtureKey {
                task_id: script.markdown_research_model_task_id,
                task_kind: script.markdown_research_model_task_kind,
                branch_task_id: script.document_research_branch_task_id,
            };
            let metadata = (key.task_kind, key.branch_task_id.clone());
            if strong_metadata
                .insert(key.task_id.clone(), metadata.clone())
                .is_some_and(|existing| existing != metadata)
            {
                return Err(task_validation(
                    "one strong-model fixture task ID has conflicting kind or branch metadata",
                ));
            }
            state.strong_responses.entry(key).or_default().push_back(script.response_json);
        }
        let mut extraction_metadata = BTreeMap::new();
        for script in extraction_responses {
            let key = ExtractionFixtureKey {
                task_id: script.markdown_research_model_task_id,
                branch_task_id: script.document_research_branch_task_id,
            };
            if strong_metadata.contains_key(&key.task_id) {
                return Err(task_validation(
                    "one fixture task ID cannot be both a strong and extraction task",
                ));
            }
            if extraction_metadata
                .insert(key.task_id.clone(), key.branch_task_id.clone())
                .is_some_and(|existing| existing != key.branch_task_id)
            {
                return Err(task_validation(
                    "one extraction fixture task ID has conflicting branch metadata",
                ));
            }
            state.extraction_responses.entry(key).or_default().push_back(script.response_json);
        }
        Ok(Self { state: Mutex::new(state) })
    }

    /// Returns a snapshot of every typed input received so far.
    pub async fn recorded_calls(&self) -> Vec<FixtureMarkdownResearchModelCall> {
        self.state.lock().await.calls.clone()
    }

    /// Fails when any configured script was not consumed.
    pub async fn assert_all_scripts_consumed(&self) -> Result<()> {
        let state = self.state.lock().await;
        if state.strong_responses.is_empty() && state.extraction_responses.is_empty() {
            Ok(())
        } else {
            let strong_count: usize = state.strong_responses.values().map(VecDeque::len).sum();
            let extraction_count: usize =
                state.extraction_responses.values().map(VecDeque::len).sum();
            Err(model_response(format!(
                "fixture has {} strong and {} extraction scripts remaining",
                strong_count, extraction_count
            )))
        }
    }
}

#[async_trait]
impl MarkdownResearchModelGateway for FixtureMarkdownResearchModelGateway {
    async fn execute_strong_markdown_research_task(
        &self,
        task: StrongMarkdownResearchModelTask,
    ) -> Result<StrongMarkdownResearchModelResponse> {
        task.validate()?;
        let key = StrongFixtureKey {
            task_id: task.markdown_research_model_task_id().clone(),
            task_kind: task.kind(),
            branch_task_id: task.document_research_branch_task_id().cloned(),
        };
        let response_json = {
            let mut state = self.state.lock().await;
            state.calls.push(FixtureMarkdownResearchModelCall::Strong(task.clone()));
            let mut responses = state.strong_responses.remove(&key).ok_or_else(|| {
                model_response("no fixture script matches the strong task ID, kind, and branch ID")
            })?;
            let response = responses.pop_front().ok_or_else(|| {
                model_response("the matching strong-model fixture response queue is exhausted")
            })?;
            if !responses.is_empty() {
                state.strong_responses.insert(key, responses);
            }
            response
        };
        task.decode_response_json(&response_json)
    }

    async fn extract_verbatim_source_evidence_candidates(
        &self,
        task: VerbatimSourceEvidenceExtractionTask,
    ) -> Result<VerbatimSourceEvidenceCandidateSet> {
        task.validate()?;
        let key = ExtractionFixtureKey {
            task_id: task.markdown_research_model_task_id.clone(),
            branch_task_id: task.document_research_branch_task_id.clone(),
        };
        let response_json = {
            let mut state = self.state.lock().await;
            state.calls.push(FixtureMarkdownResearchModelCall::VerbatimSourceEvidenceExtraction(
                task.clone(),
            ));
            let mut responses = state.extraction_responses.remove(&key).ok_or_else(|| {
                model_response("no fixture script matches the extraction task and branch IDs")
            })?;
            let response = responses.pop_front().ok_or_else(|| {
                model_response("the matching extraction fixture response queue is exhausted")
            })?;
            if !responses.is_empty() {
                state.extraction_responses.insert(key, responses);
            }
            response
        };
        task.decode_response_json(&response_json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clarification::{
        ResearchQuestionClarificationDecision, ResearchQuestionClarificationModelOutput,
    };
    use crate::domain::{
        EvidenceLinkedResearchClaimCitationStatus, ResearchClaimEvidenceRelationship,
        ResearchClaimEvidenceRelationshipType, SourceAttributedAnswerSegment,
    };
    use crate::error::RuntimeErrorCode;
    use crate::execution_trace::MarkdownCorpusNavigationNodeSelectionStatus;
    use crate::identity::PublicSourceCitationId;
    use serde_json::json;
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct TestContext {
        execution_id: MarkdownResearchExecutionId,
        branch_id: DocumentResearchBranchTaskId,
        navigation_node_id: MarkdownCorpusNavigationNodeId,
        document_id: MarkdownSourceDocumentId,
        document_version_id: MarkdownSourceDocumentVersionId,
        segment_id: MarkdownSourceSegmentId,
        read_request_id: ResearchDocumentReadRequestId,
        claim_id: EvidenceLinkedResearchClaimId,
        citation_id: PublicSourceCitationId,
        model_answer_id: MarkdownResearchModelTaskId,
        claims_answer_id: MarkdownResearchModelTaskId,
        candidate_set_id: MarkdownCorpusNavigationCandidateSetId,
        snapshot_id: MarkdownCorpusSnapshotId,
        authorized_segment: AuthorizedMarkdownSourceSegmentInput,
        evidence: AcceptedVerbatimSourceEvidenceModelContext,
        claim: EvidenceLinkedResearchClaim,
        model_answer: ModelKnowledgeOnlyAnswer,
        claims_answer: EvidenceLinkedResearchClaimsAnswer,
        citation: PublicSourceCitation,
        report: MarkdownCorpusNavigationBranchDocumentRelevanceReport,
    }

    impl TestContext {
        fn new() -> Self {
            let execution_id = MarkdownResearchExecutionId::generate();
            let branch_id = DocumentResearchBranchTaskId::generate();
            let navigation_node_id = MarkdownCorpusNavigationNodeId::generate();
            let document_id = MarkdownSourceDocumentId::generate();
            let document_version_id = MarkdownSourceDocumentVersionId::generate();
            let segment_id = MarkdownSourceSegmentId::generate();
            let read_request_id = ResearchDocumentReadRequestId::generate();
            let evidence_id = VerbatimSourceEvidenceId::generate();
            let claim_id = EvidenceLinkedResearchClaimId::generate();
            let citation_id = PublicSourceCitationId::generate();
            let model_answer_id = MarkdownResearchModelTaskId::generate();
            let claims_answer_id = MarkdownResearchModelTaskId::generate();
            let candidate_set_id = MarkdownCorpusNavigationCandidateSetId::generate();
            let snapshot_id = MarkdownCorpusSnapshotId::generate();
            let segment_text = "authoritative rule".to_owned();
            let segment_hash = sha256_content_hash(segment_text.as_bytes());
            let authorized_segment = AuthorizedMarkdownSourceSegmentInput {
                markdown_source_document_id: document_id.clone(),
                markdown_source_document_version_id: document_version_id.clone(),
                markdown_source_segment_id: segment_id.clone(),
                markdown_source_segment_hash: segment_hash,
                markdown_source_segment_start_byte_offset_in_document: 40,
                canonical_markdown_source_segment_text: segment_text.clone(),
            };
            let evidence = AcceptedVerbatimSourceEvidenceModelContext {
                verbatim_source_evidence_id: evidence_id.clone(),
                markdown_source_document_id: document_id.clone(),
                markdown_source_segment_id: segment_id.clone(),
                verbatim_source_evidence_quote: segment_text,
                document_research_branch_task_id: branch_id.clone(),
                markdown_research_execution_id: execution_id.clone(),
            };
            let claim = EvidenceLinkedResearchClaim {
                evidence_linked_research_claim_id: claim_id.clone(),
                evidence_linked_research_claim_text: "The rule applies.".to_owned(),
                research_claim_evidence_relationships: vec![
                    ResearchClaimEvidenceRelationship {
                        verbatim_source_evidence_id: evidence_id.clone(),
                        research_claim_evidence_relationship_type:
                            ResearchClaimEvidenceRelationshipType::SupportsEvidenceLinkedResearchClaim,
                    },
                ],
                evidence_linked_research_claim_applicability_conditions: vec![],
                evidence_linked_research_claim_exceptions: vec![],
                evidence_linked_research_claim_citation_status:
                    EvidenceLinkedResearchClaimCitationStatus::AllCitationsLinkedToVerbatimSourceEvidence,
                markdown_research_execution_id: execution_id.clone(),
            };
            let model_answer = ModelKnowledgeOnlyAnswer {
                model_knowledge_only_answer_id: model_answer_id.clone(),
                model_knowledge_only_answer_text: "Unverified model background.".to_owned(),
                markdown_research_execution_id: execution_id.clone(),
            };
            let claims_answer = EvidenceLinkedResearchClaimsAnswer {
                evidence_linked_research_claims_answer_id: claims_answer_id.clone(),
                evidence_linked_research_claims_answer_text: "The evidence-backed answer."
                    .to_owned(),
                supporting_evidence_linked_research_claim_ids: vec![claim_id.clone()],
                markdown_research_execution_id: execution_id.clone(),
            };
            let citation = PublicSourceCitation {
                public_source_citation_id: citation_id.clone(),
                markdown_source_document_id: document_id.clone(),
                markdown_source_document_title: "Rules".to_owned(),
                markdown_source_segment_section_heading: Some("Scope".to_owned()),
                public_source_citation_quote: "authoritative rule".to_owned(),
                markdown_source_document_version_content_hash: "sha256:document".to_owned(),
            };
            let report = MarkdownCorpusNavigationBranchDocumentRelevanceReport {
                document_research_branch_task_id: branch_id.clone(),
                markdown_corpus_navigation_node_id: navigation_node_id.clone(),
                candidate_markdown_source_document_ids: vec![document_id.clone()],
                selected_markdown_source_document_ids: vec![document_id.clone()],
                markdown_corpus_navigation_branch_document_report_summary: "One relevant document."
                    .to_owned(),
            };
            Self {
                execution_id,
                branch_id,
                navigation_node_id,
                document_id,
                document_version_id,
                segment_id,
                read_request_id,
                claim_id,
                citation_id,
                model_answer_id,
                claims_answer_id,
                candidate_set_id,
                snapshot_id,
                authorized_segment,
                evidence,
                claim,
                model_answer,
                claims_answer,
                citation,
                report,
            }
        }
    }

    fn brief() -> FrozenDocumentResearchBrief {
        FrozenDocumentResearchBrief::freeze(
            "What is the rule?",
            "What is the applicable rule?",
            vec![],
            vec![],
            vec![],
            vec!["Cite the answer.".to_owned()],
        )
        .expect("valid brief")
    }

    fn brief_draft() -> DocumentResearchBriefDraft {
        DocumentResearchBriefDraft {
            original_user_question: "What is the rule?".to_owned(),
            clarified_research_question: "What is the applicable rule?".to_owned(),
            known_document_research_context: vec![],
            document_research_assumptions: vec![],
            unresolved_research_question_ambiguities: vec![],
            requested_research_answer_requirements: vec!["Cite the answer.".to_owned()],
        }
    }

    fn strong_cases(context: &TestContext) -> Vec<(StrongMarkdownResearchModelTask, Value)> {
        let question_task = ResearchQuestionEvaluationTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            document_research_conversation_id: DocumentResearchConversationId::generate(),
            document_research_request_id: DocumentResearchRequestId::generate(),
            research_question_clarification_revision: 0,
            original_user_question: "What is the rule?".to_owned(),
            research_question_clarification_dialogue: vec![],
            document_research_brief_draft: None,
            allowed_completed_research_context: vec![],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };
        let question_response = ResearchQuestionClarificationModelOutput {
            research_question_clarification_revision: 0,
            research_question_clarification_decision:
                ResearchQuestionClarificationDecision::StartMarkdownResearchExecution,
            research_question_clarification_message: None,
            document_research_brief_draft: brief_draft(),
        };

        let model_task = ModelKnowledgeOnlyAnswerGenerationTask {
            markdown_research_model_task_id: context.model_answer_id.clone(),
            markdown_research_execution_id: context.execution_id.clone(),
            frozen_document_research_brief: brief(),
            allowed_completed_research_context: vec![],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };

        let navigation_task = MarkdownCorpusNavigationBranchSelectionTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            markdown_research_execution_id: context.execution_id.clone(),
            frozen_document_research_brief: brief(),
            markdown_corpus_snapshot_id: context.snapshot_id.clone(),
            markdown_corpus_navigation_candidate_set_id: context.candidate_set_id.clone(),
            parent_markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId::generate(),
            markdown_corpus_navigation_node_candidates: vec![
                MarkdownCorpusNavigationNodeCandidate {
                    markdown_corpus_navigation_node_id: context.navigation_node_id.clone(),
                    markdown_corpus_navigation_node_label: "Rules".to_owned(),
                    markdown_corpus_navigation_node_summary: "Applicable rules".to_owned(),
                },
            ],
            triggering_verbatim_source_evidence_ids: vec![],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };
        let navigation_response = MarkdownCorpusNavigationBranchSelectionResponse {
            markdown_corpus_navigation_candidate_set_id: context.candidate_set_id.clone(),
            markdown_corpus_navigation_branch_selections: vec![
                MarkdownCorpusNavigationBranchSelection {
                    markdown_corpus_navigation_node_id: context.navigation_node_id.clone(),
                    markdown_corpus_navigation_node_selection_status:
                        MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch,
                    markdown_corpus_navigation_node_relevance_explanation: "Directly relevant."
                        .to_owned(),
                    expected_research_information_to_resolve_question: "The applicable rule."
                        .to_owned(),
                    markdown_corpus_navigation_branch_priority: 1,
                },
            ],
        };

        let report_task = MarkdownCorpusNavigationBranchDocumentRelevanceReportTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            markdown_research_execution_id: context.execution_id.clone(),
            document_research_branch_task_id: context.branch_id.clone(),
            markdown_corpus_navigation_node_id: context.navigation_node_id.clone(),
            frozen_document_research_brief: brief(),
            markdown_source_document_candidates: vec![
                MarkdownSourceDocumentAbstractModelCandidate {
                    markdown_source_document_id: context.document_id.clone(),
                    markdown_source_document_title: "Rules".to_owned(),
                    markdown_source_document_abstract: "Applicable rules.".to_owned(),
                },
            ],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };

        let read_task = ResearchDocumentReadRequestTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            research_document_read_request_id: context.read_request_id.clone(),
            markdown_research_execution_id: context.execution_id.clone(),
            document_research_branch_task_id: context.branch_id.clone(),
            frozen_document_research_brief: brief(),
            committed_branch_document_reports: vec![context.report.clone()],
            candidate_markdown_source_segments: vec![MarkdownSourceSegmentMetadata {
                markdown_source_document_id: context.document_id.clone(),
                markdown_source_document_version_id: context.document_version_id.clone(),
                markdown_source_segment_id: context.segment_id.clone(),
                markdown_source_segment_section_heading: Some("Scope".to_owned()),
                markdown_source_segment_start_byte_offset_in_document: 40,
                markdown_source_segment_end_byte_offset_in_document: 58,
                markdown_source_segment_hash: context
                    .authorized_segment
                    .markdown_source_segment_hash
                    .clone(),
            }],
            accepted_verbatim_source_evidence: vec![],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };
        let read_response = ResearchDocumentReadRequest {
            research_document_read_request_id: context.read_request_id.clone(),
            document_research_branch_task_id: context.branch_id.clone(),
            markdown_source_document_id: context.document_id.clone(),
            markdown_source_segment_id: context.segment_id.clone(),
            unresolved_research_question: "What is the applicable rule?".to_owned(),
            expected_research_information_to_resolve_question: "The rule text.".to_owned(),
            markdown_source_document_selection_explanation: "The report selected it.".to_owned(),
        };

        let review_task = MarkdownSourceReviewTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            markdown_research_execution_id: context.execution_id.clone(),
            document_research_branch_task_id: context.branch_id.clone(),
            research_document_read_request_id: context.read_request_id.clone(),
            frozen_document_research_brief: brief(),
            authorized_markdown_source_segment: context.authorized_segment.clone(),
            accepted_verbatim_source_evidence: vec![],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };
        let review_response = MarkdownSourceReviewDecision {
            research_document_read_request_id: context.read_request_id.clone(),
            document_research_branch_task_id: context.branch_id.clone(),
            markdown_source_document_id: context.document_id.clone(),
            markdown_source_segment_id: context.segment_id.clone(),
            markdown_source_follow_up_action:
                MarkdownSourceFollowUpAction::ExtractVerbatimSourceEvidence,
            verbatim_source_evidence_extraction_goal: Some("Quote the rule.".to_owned()),
            triggering_verbatim_source_evidence_ids: vec![],
            markdown_corpus_navigation_branch_close_reason: None,
            markdown_source_review_summary: "The segment contains direct evidence.".to_owned(),
        };

        let claim_task = EvidenceLinkedResearchClaimGenerationTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            markdown_research_execution_id: context.execution_id.clone(),
            frozen_document_research_brief: brief(),
            accepted_verbatim_source_evidence: vec![context.evidence.clone()],
            research_coverage_gaps: vec![],
            authorized_evidence_linked_research_claim_ids: vec![context.claim_id.clone()],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };
        let claim_response = EvidenceLinkedResearchClaimGenerationResponse {
            evidence_linked_research_claims: vec![context.claim.clone()],
        };

        let claims_answer_task = EvidenceLinkedResearchClaimsAnswerGenerationTask {
            markdown_research_model_task_id: context.claims_answer_id.clone(),
            markdown_research_execution_id: context.execution_id.clone(),
            frozen_document_research_brief: brief(),
            committed_evidence_linked_research_claims: vec![context.claim.clone()],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };

        let composition_task = SourceAttributedAnswerCompositionTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            markdown_research_execution_id: context.execution_id.clone(),
            frozen_document_research_brief: brief(),
            requested_answer_composition_style:
                AnswerCompositionStyle::EvidenceLinkedResearchClaimLed,
            model_knowledge_only_answer: context.model_answer.clone(),
            committed_evidence_linked_research_claims: vec![context.claim.clone()],
            evidence_linked_research_claims_answer: context.claims_answer.clone(),
            public_source_citations: vec![context.citation.clone()],
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };
        let composition_response = SourceAttributedAnswerComposition {
            source_attributed_answer_composition_style:
                AnswerCompositionStyle::EvidenceLinkedResearchClaimLed,
            model_knowledge_only_answer_id: context.model_answer_id.clone(),
            evidence_linked_research_claims_answer_id: context.claims_answer_id.clone(),
            source_attributed_answer_segments: vec![SourceAttributedAnswerSegment {
                source_attributed_answer_segment_text: "The rule applies.".to_owned(),
                source_attributed_answer_segment_source_type:
                    SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaims,
                supporting_evidence_linked_research_claim_ids: vec![context.claim_id.clone()],
                supporting_public_source_citation_ids: vec![context.citation_id.clone()],
                model_knowledge_unverified_notice: None,
            }],
            source_attributed_answer_composition_review_reason: "Evidence is the requested base."
                .to_owned(),
            answer_projection_schema_version: ANSWER_PROJECTION_SCHEMA_VERSION,
        };

        vec![
            (
                StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(question_task),
                serde_json::to_value(question_response).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::ModelKnowledgeOnlyAnswerGeneration(model_task),
                serde_json::to_value(&context.model_answer).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchSelection(
                    navigation_task,
                ),
                serde_json::to_value(navigation_response).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
                    report_task,
                ),
                serde_json::to_value(&context.report).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::ResearchDocumentReadRequest(read_task),
                serde_json::to_value(read_response).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::MarkdownSourceReview(review_task),
                serde_json::to_value(review_response).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimGeneration(
                    claim_task,
                ),
                serde_json::to_value(claim_response).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimsAnswerGeneration(
                    claims_answer_task,
                ),
                serde_json::to_value(&context.claims_answer).expect("serialize response"),
            ),
            (
                StrongMarkdownResearchModelTask::SourceAttributedAnswerComposition(
                    composition_task,
                ),
                serde_json::to_value(composition_response).expect("serialize response"),
            ),
        ]
    }

    fn strong_envelope(task: &StrongMarkdownResearchModelTask, response: Value) -> String {
        json!({
            "markdown_research_model_task_id": task.markdown_research_model_task_id(),
            "markdown_research_model_task_kind": task.kind(),
            "document_research_branch_task_id": task.document_research_branch_task_id(),
            "markdown_research_model_task_schema_version": MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
            "response": response,
        })
        .to_string()
    }

    fn strong_script(
        task: &StrongMarkdownResearchModelTask,
        response: Value,
    ) -> ScriptedStrongMarkdownResearchModelResponse {
        ScriptedStrongMarkdownResearchModelResponse {
            markdown_research_model_task_id: task.markdown_research_model_task_id().clone(),
            markdown_research_model_task_kind: task.kind(),
            document_research_branch_task_id: task.document_research_branch_task_id().cloned(),
            response_json: strong_envelope(task, response),
        }
    }

    fn extraction_task(context: &TestContext) -> VerbatimSourceEvidenceExtractionTask {
        VerbatimSourceEvidenceExtractionTask {
            markdown_research_model_task_id: MarkdownResearchModelTaskId::generate(),
            verbatim_source_evidence_extraction_request_id:
                VerbatimSourceEvidenceExtractionRequestId::generate(),
            markdown_research_execution_id: context.execution_id.clone(),
            document_research_branch_task_id: context.branch_id.clone(),
            clarified_research_question: "What is the applicable rule?".to_owned(),
            verbatim_source_evidence_extraction_goal: "Quote the rule.".to_owned(),
            authorized_markdown_source_segment: context.authorized_segment.clone(),
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        }
    }

    fn extraction_response(
        context: &TestContext,
        task: &VerbatimSourceEvidenceExtractionTask,
    ) -> VerbatimSourceEvidenceCandidateSet {
        VerbatimSourceEvidenceCandidateSet {
            verbatim_source_evidence_extraction_request_id: task
                .verbatim_source_evidence_extraction_request_id
                .clone(),
            document_research_branch_task_id: context.branch_id.clone(),
            markdown_source_document_id: context.document_id.clone(),
            markdown_source_document_version_id: context.document_version_id.clone(),
            markdown_source_segment_id: context.segment_id.clone(),
            markdown_source_segment_hash: context
                .authorized_segment
                .markdown_source_segment_hash
                .clone(),
            verbatim_source_evidence_candidates: vec![VerbatimSourceEvidenceCandidate {
                verbatim_source_evidence_start_byte_offset_in_segment: 0,
                verbatim_source_evidence_end_byte_offset_in_segment: context
                    .authorized_segment
                    .canonical_markdown_source_segment_text
                    .len()
                    as u64,
                verbatim_source_evidence_quote: context
                    .authorized_segment
                    .canonical_markdown_source_segment_text
                    .clone(),
            }],
        }
    }

    fn extraction_envelope(task: &VerbatimSourceEvidenceExtractionTask, response: Value) -> String {
        json!({
            "markdown_research_model_task_id": task.markdown_research_model_task_id,
            "document_research_branch_task_id": task.document_research_branch_task_id,
            "markdown_research_model_task_schema_version": MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
            "response": response,
        })
        .to_string()
    }

    fn case_by_kind(
        context: &TestContext,
        kind: MarkdownResearchModelTaskKind,
    ) -> (StrongMarkdownResearchModelTask, Value) {
        strong_cases(context)
            .into_iter()
            .find(|(task, _)| task.kind() == kind)
            .expect("case exists")
    }

    fn assert_model_response(error: RuntimeError) {
        assert_eq!(error.code(), RuntimeErrorCode::ModelResponse);
    }

    #[tokio::test]
    async fn fixture_matches_every_strong_variant_by_stable_key_not_script_order() {
        let context = TestContext::new();
        let mut cases = strong_cases(&context);
        let scripts = cases
            .iter()
            .rev()
            .map(|(task, response)| strong_script(task, response.clone()))
            .collect();
        let fixture = FixtureMarkdownResearchModelGateway::new(scripts, vec![])
            .expect("valid fixture scripts");

        for (task, _) in cases.drain(..).rev() {
            fixture.execute_strong_markdown_research_task(task).await.expect("scripted response");
        }

        assert_eq!(fixture.recorded_calls().await.len(), 9);
        fixture.assert_all_scripts_consumed().await.expect("all consumed");
    }

    #[test]
    fn phase_specific_task_types_exclude_forbidden_inputs() {
        let context = TestContext::new();
        let (model_task, _) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::ModelKnowledgeOnlyAnswerGeneration,
        );
        let model_json = serde_json::to_string(&model_task).expect("serialize task");
        for forbidden in [
            "markdown_corpus_snapshot",
            "markdown_source_document",
            "markdown_source_segment",
            "verbatim_source_evidence",
            "evidence_linked_research_claim",
        ] {
            assert!(!model_json.contains(forbidden), "unexpected field: {forbidden}");
        }
        let StrongMarkdownResearchModelTask::ModelKnowledgeOnlyAnswerGeneration(model_task) =
            model_task
        else {
            panic!("wrong test task");
        };
        let mut model_value = serde_json::to_value(model_task).expect("serialize task body");
        model_value
            .as_object_mut()
            .expect("task object")
            .insert("markdown_corpus_snapshot_id".to_owned(), json!(context.snapshot_id));
        assert!(
            serde_json::from_value::<ModelKnowledgeOnlyAnswerGenerationTask>(model_value).is_err()
        );

        let (claims_task, _) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::EvidenceLinkedResearchClaimsAnswerGeneration,
        );
        let claims_json = serde_json::to_string(&claims_task).expect("serialize task");
        for forbidden in [
            "markdown_corpus",
            "markdown_source_segment",
            "accepted_verbatim_source_evidence",
            "verbatim_source_evidence_quote",
            "internal_markdown_source_reference",
            "model_knowledge_only_answer",
        ] {
            assert!(!claims_json.contains(forbidden), "unexpected field: {forbidden}");
        }

        let cheap_task = extraction_task(&context);
        let cheap_json = serde_json::to_string(&cheap_task).expect("serialize task");
        assert_eq!(cheap_json.matches("canonical_markdown_source_segment_text").count(), 1);
        for forbidden in [
            "markdown_corpus_navigation",
            "accepted_verbatim_source_evidence",
            "markdown_source_follow_up_action",
            "evidence_linked_research_claim",
            "model_knowledge_only_answer",
        ] {
            assert!(!cheap_json.contains(forbidden), "unexpected field: {forbidden}");
        }
        let mut cheap_value = serde_json::to_value(cheap_task).expect("serialize cheap task");
        cheap_value
            .as_object_mut()
            .expect("task object")
            .insert("other_authorized_markdown_source_segments".to_owned(), json!([]));
        assert!(
            serde_json::from_value::<VerbatimSourceEvidenceExtractionTask>(cheap_value).is_err()
        );
    }

    #[test]
    fn closed_response_schema_rejects_unknown_wrong_and_oversized_values() {
        let context = TestContext::new();
        let (task, valid_response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::ModelKnowledgeOnlyAnswerGeneration,
        );

        let mut unknown_response = valid_response.clone();
        unknown_response
            .as_object_mut()
            .expect("response object")
            .insert("unknown_field".to_owned(), json!(true));
        assert_model_response(
            task.decode_response_json(&strong_envelope(&task, unknown_response))
                .expect_err("unknown response field must fail"),
        );

        let mut unknown_envelope: Value =
            serde_json::from_str(&strong_envelope(&task, valid_response.clone()))
                .expect("response envelope");
        unknown_envelope
            .as_object_mut()
            .expect("envelope object")
            .insert("unknown_envelope_field".to_owned(), json!(true));
        assert_model_response(
            task.decode_response_json(&unknown_envelope.to_string())
                .expect_err("unknown envelope field must fail"),
        );

        let mut wrong_kind: Value =
            serde_json::from_str(&strong_envelope(&task, valid_response.clone()))
                .expect("response envelope");
        wrong_kind["markdown_research_model_task_kind"] =
            json!(MarkdownResearchModelTaskKind::MarkdownSourceReview);
        assert_model_response(
            task.decode_response_json(&wrong_kind.to_string())
                .expect_err("wrong task response must fail"),
        );

        let mut long_response = valid_response;
        long_response["model_knowledge_only_answer_text"] =
            json!("x".repeat(MAX_RESEARCH_TEXT_BYTES + 1));
        assert_model_response(
            task.decode_response_json(&strong_envelope(&task, long_response))
                .expect_err("long response text must fail"),
        );

        let (claim_task, mut claims_response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::EvidenceLinkedResearchClaimGeneration,
        );
        let claim = claims_response["evidence_linked_research_claims"][0].clone();
        claims_response["evidence_linked_research_claims"] =
            Value::Array(vec![claim; MAX_MARKDOWN_RESEARCH_MODEL_ITEMS + 1]);
        assert_model_response(
            claim_task
                .decode_response_json(&strong_envelope(&claim_task, claims_response))
                .expect_err("oversized response array must fail"),
        );

        assert_model_response(
            task.decode_response_json(&"x".repeat(MAX_MARKDOWN_RESEARCH_MODEL_RESPONSE_BYTES + 1))
                .expect_err("oversized response must fail"),
        );
    }

    #[test]
    fn strong_responses_reject_fabricated_candidate_document_segment_and_claim_ids() {
        let context = TestContext::new();

        let (navigation_task, mut navigation_response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::MarkdownCorpusNavigationBranchSelection,
        );
        navigation_response["markdown_corpus_navigation_branch_selections"][0]["markdown_corpus_navigation_node_id"] =
            json!(MarkdownCorpusNavigationNodeId::generate());
        assert_model_response(
            navigation_task
                .decode_response_json(&strong_envelope(&navigation_task, navigation_response))
                .expect_err("fabricated navigation ID must fail"),
        );

        let (report_task, mut report_response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::MarkdownCorpusNavigationBranchDocumentRelevanceReport,
        );
        report_response["selected_markdown_source_document_ids"][0] =
            json!(MarkdownSourceDocumentId::generate());
        assert_model_response(
            report_task
                .decode_response_json(&strong_envelope(&report_task, report_response))
                .expect_err("fabricated document ID must fail"),
        );

        let (read_task, mut read_response) =
            case_by_kind(&context, MarkdownResearchModelTaskKind::ResearchDocumentReadRequest);
        read_response["markdown_source_segment_id"] = json!(MarkdownSourceSegmentId::generate());
        assert_model_response(
            read_task
                .decode_response_json(&strong_envelope(&read_task, read_response))
                .expect_err("fabricated segment ID must fail"),
        );

        let (claim_task, mut claim_response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::EvidenceLinkedResearchClaimGeneration,
        );
        claim_response["evidence_linked_research_claims"][0]["evidence_linked_research_claim_id"] =
            json!(EvidenceLinkedResearchClaimId::generate());
        assert_model_response(
            claim_task
                .decode_response_json(&strong_envelope(&claim_task, claim_response))
                .expect_err("fabricated generated claim ID must fail"),
        );

        let (answer_task, mut answer_response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::EvidenceLinkedResearchClaimsAnswerGeneration,
        );
        answer_response["supporting_evidence_linked_research_claim_ids"][0] =
            json!(EvidenceLinkedResearchClaimId::generate());
        assert_model_response(
            answer_task
                .decode_response_json(&strong_envelope(&answer_task, answer_response))
                .expect_err("fabricated supporting claim ID must fail"),
        );
    }

    #[tokio::test]
    async fn cheap_fixture_sees_one_segment_and_rejects_other_segment_or_control_fields() {
        let context = TestContext::new();
        let task = extraction_task(&context);
        let response = extraction_response(&context, &task);
        let script = ScriptedVerbatimSourceEvidenceExtractionResponse {
            markdown_research_model_task_id: task.markdown_research_model_task_id.clone(),
            document_research_branch_task_id: task.document_research_branch_task_id.clone(),
            response_json: extraction_envelope(
                &task,
                serde_json::to_value(&response).expect("serialize candidate set"),
            ),
        };
        let fixture =
            FixtureMarkdownResearchModelGateway::new(vec![], vec![script]).expect("valid fixture");
        let accepted = fixture
            .extract_verbatim_source_evidence_candidates(task.clone())
            .await
            .expect("valid extraction response");
        assert_eq!(accepted.markdown_source_segment_id, context.segment_id);
        assert_eq!(fixture.recorded_calls().await.len(), 1);

        let mut fabricated = serde_json::to_value(&response).expect("serialize candidate set");
        fabricated["markdown_source_segment_id"] = json!(MarkdownSourceSegmentId::generate());
        assert_model_response(
            task.decode_response_json(&extraction_envelope(&task, fabricated))
                .expect_err("other segment must fail"),
        );

        let mut control_field = serde_json::to_value(response).expect("serialize candidate set");
        control_field.as_object_mut().expect("candidate set object").insert(
            "markdown_source_follow_up_action".to_owned(),
            json!("close_markdown_corpus_navigation_branch"),
        );
        assert_model_response(
            task.decode_response_json(&extraction_envelope(&task, control_field))
                .expect_err("cheap model control action must fail"),
        );
    }

    #[tokio::test]
    async fn fixture_uses_per_task_queues_records_failures_and_reports_exhaustion() {
        let context = TestContext::new();
        let (task, mut first_response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::ModelKnowledgeOnlyAnswerGeneration,
        );
        let mut second_response = first_response.clone();
        first_response["model_knowledge_only_answer_text"] = json!("first");
        second_response["model_knowledge_only_answer_text"] = json!("second");
        let fixture = FixtureMarkdownResearchModelGateway::new(
            vec![strong_script(&task, first_response), strong_script(&task, second_response)],
            vec![],
        )
        .expect("same-key response queue is valid");
        assert!(fixture.assert_all_scripts_consumed().await.is_err());

        let first = fixture
            .execute_strong_markdown_research_task(task.clone())
            .await
            .expect("first queued response");
        let second = fixture
            .execute_strong_markdown_research_task(task.clone())
            .await
            .expect("second queued response");
        let StrongMarkdownResearchModelResponse::ModelKnowledgeOnlyAnswerGeneration(first) = first
        else {
            panic!("wrong first response variant");
        };
        let StrongMarkdownResearchModelResponse::ModelKnowledgeOnlyAnswerGeneration(second) =
            second
        else {
            panic!("wrong second response variant");
        };
        assert_eq!(first.model_knowledge_only_answer_text, "first");
        assert_eq!(second.model_knowledge_only_answer_text, "second");
        fixture.assert_all_scripts_consumed().await.expect("queue consumed");

        assert_model_response(
            fixture
                .execute_strong_markdown_research_task(task)
                .await
                .expect_err("exhausted queue must fail"),
        );
        assert_eq!(fixture.recorded_calls().await.len(), 3);
    }

    #[tokio::test]
    async fn concurrent_branch_scripts_do_not_depend_on_completion_order() {
        let first_context = TestContext::new();
        let second_context = TestContext::new();
        let (first_task, first_response) = case_by_kind(
            &first_context,
            MarkdownResearchModelTaskKind::MarkdownCorpusNavigationBranchDocumentRelevanceReport,
        );
        let (second_task, second_response) = case_by_kind(
            &second_context,
            MarkdownResearchModelTaskKind::MarkdownCorpusNavigationBranchDocumentRelevanceReport,
        );
        let fixture = Arc::new(
            FixtureMarkdownResearchModelGateway::new(
                vec![
                    strong_script(&second_task, second_response),
                    strong_script(&first_task, first_response),
                ],
                vec![],
            )
            .expect("valid branch scripts"),
        );

        let (first, second) = tokio::join!(
            fixture.execute_strong_markdown_research_task(first_task),
            fixture.execute_strong_markdown_research_task(second_task),
        );
        let StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
            first,
        ) = first.expect("first branch response")
        else {
            panic!("wrong first branch response");
        };
        let StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
            second,
        ) = second.expect("second branch response")
        else {
            panic!("wrong second branch response");
        };
        assert_eq!(first.selected_markdown_source_document_ids, vec![first_context.document_id]);
        assert_eq!(second.selected_markdown_source_document_ids, vec![second_context.document_id]);
        fixture.assert_all_scripts_consumed().await.expect("all consumed");
    }

    #[test]
    fn fixture_rejects_one_task_id_with_conflicting_metadata() {
        let context = TestContext::new();
        let (task, response) = case_by_kind(
            &context,
            MarkdownResearchModelTaskKind::ModelKnowledgeOnlyAnswerGeneration,
        );
        let mut conflicting = strong_script(&task, response.clone());
        conflicting.markdown_research_model_task_kind =
            MarkdownResearchModelTaskKind::MarkdownSourceReview;
        let error = FixtureMarkdownResearchModelGateway::new(
            vec![strong_script(&task, response), conflicting],
            vec![],
        )
        .expect_err("conflicting metadata must fail");
        assert_eq!(error.code(), RuntimeErrorCode::ValidationFailed);
    }
}

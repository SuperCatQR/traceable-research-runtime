//! Pure domain values and deterministic invariants.

use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::identity::{
    CommandId, DocumentResearchBranchTaskId, DocumentResearchConversationId,
    DocumentResearchRequestId, EvidenceLinkedResearchClaimId, MarkdownCorpusSnapshotId,
    MarkdownResearchExecutionId, MarkdownResearchModelTaskId, MarkdownSourceDocumentId,
    MarkdownSourceSegmentId, PublicSourceCitationId, ResearchCoverageGapId,
    VerbatimSourceEvidenceId,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Version of the Frozen Document Research Brief schema.
pub const DOCUMENT_RESEARCH_BRIEF_SCHEMA_VERSION: u32 = 1;
/// Version of the Markdown Research Execution Limits schema.
pub const EXECUTION_LIMITS_SCHEMA_VERSION: u32 = 1;
/// Version of the answer/source projection schema.
pub const ANSWER_PROJECTION_SCHEMA_VERSION: u32 = 1;
/// Maximum user-facing research text in bytes.
pub const MAX_RESEARCH_TEXT_BYTES: usize = 64 * 1024;
/// Maximum text for one evidence quote in bytes.
pub const MAX_EVIDENCE_QUOTE_BYTES: usize = 64 * 1024;
/// Maximum text for one claim in bytes.
pub const MAX_CLAIM_TEXT_BYTES: usize = 64 * 1024;

/// Serializes a value using RFC 8785 JSON Canonicalization Scheme.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json_canonicalizer::to_vec(value).map_err(|error| RuntimeError::Validation {
        stage: RuntimeStage::Setup,
        message: format!("cannot canonicalize value: {error}"),
    })
}

/// Computes a content-addressed SHA-256 string.
#[must_use]
pub fn sha256_content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", hex::encode(digest))
}

/// Computes a canonical JSON content hash.
pub fn canonical_content_hash<T: Serialize>(value: &T) -> Result<String> {
    Ok(sha256_content_hash(&canonical_json_bytes(value)?))
}

fn require_text(name: &str, value: &str, max_bytes: usize) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RuntimeError::validation(
            RuntimeStage::Setup,
            format!("{name} must not be empty"),
        ));
    }
    if trimmed.len() > max_bytes {
        return Err(RuntimeError::validation(
            RuntimeStage::Setup,
            format!("{name} exceeds {max_bytes} bytes"),
        ));
    }
    if trimmed
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(RuntimeError::validation(
            RuntimeStage::Setup,
            format!("{name} contains a control character"),
        ));
    }
    Ok(trimmed.to_owned())
}

/// The two supported final answer composition bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum AnswerCompositionStyle {
    /// Use model knowledge as the narrative base and evidence as correction.
    ModelKnowledgeLed,
    /// Use evidence-linked claims as the narrative base and model knowledge as supplement.
    EvidenceLinkedResearchClaimLed,
}

impl AnswerCompositionStyle {
    /// Returns the stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelKnowledgeLed => "model_knowledge_led",
            Self::EvidenceLinkedResearchClaimLed => "evidence_linked_research_claim_led",
        }
    }
}

/// Source provenance attached to each final answer segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum SourceAttributedAnswerSegmentSourceType {
    /// The segment is based on current-execution evidence-linked claims.
    EvidenceLinkedResearchClaims,
    /// The segment is model knowledge not verified by the current corpus.
    ModelKnowledgeOnly,
    /// The segment deliberately combines both sources.
    EvidenceLinkedResearchClaimsAndModelKnowledge,
}

/// The relationship a source evidence item has to a research claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ResearchClaimEvidenceRelationshipType {
    /// The evidence directly supports the claim.
    SupportsEvidenceLinkedResearchClaim,
    /// The evidence limits or qualifies the claim.
    QualifiesEvidenceLinkedResearchClaim,
    /// The evidence contradicts the claim.
    ContradictsEvidenceLinkedResearchClaim,
}

/// Priority of an unresolved research coverage gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ResearchCoverageGapPriority {
    /// A gap can change the answer and must be resolved or disclosed.
    High,
    /// A gap is useful context but does not block normal completion.
    Medium,
    /// A gap is optional context.
    Low,
}

/// Lifecycle state of a research coverage gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ResearchCoverageGapResolutionStatus {
    /// No resolution has been committed.
    Unresolved,
    /// Accepted evidence resolves the gap.
    ResolvedWithVerbatimSourceEvidence,
    /// The frozen corpus cannot answer the gap.
    UnableToResolveFromMarkdownCorpus,
    /// The gap is explicitly disclosed in the final answer.
    DisclosedInAnswer,
}

/// Frozen resource and stopping limits for one execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownResearchExecutionLimits {
    /// Maximum navigation depth.
    pub maximum_markdown_corpus_navigation_depth: u32,
    /// Maximum selected branches at one level.
    pub maximum_selected_markdown_corpus_navigation_branches_per_level: u32,
    /// Maximum active branch tasks.
    pub maximum_active_document_research_branches: u32,
    /// Maximum selected source documents.
    pub maximum_selected_markdown_source_documents: u32,
    /// Maximum source segments read.
    pub maximum_read_markdown_source_segments: u32,
    /// Maximum logical strong model tasks.
    pub maximum_strong_markdown_research_model_requests: u32,
    /// Maximum logical cheap extraction tasks.
    pub maximum_verbatim_source_evidence_extraction_model_requests: u32,
    /// Maximum estimated model input tokens.
    pub maximum_total_model_input_token_estimate: u64,
    /// Maximum execution wall-clock seconds.
    pub maximum_markdown_research_execution_duration_seconds: u64,
    /// Result policy when a limit is exhausted.
    pub resource_exhaustion_outcome: ResourceExhaustionOutcome,
    /// Frozen estimator version used for token counts.
    pub model_input_token_estimator_version: u32,
    /// Schema version.
    pub execution_limits_schema_version: u32,
}

/// Behaviour when a resource budget is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceExhaustionOutcome {
    /// Produce an answer only when usable evidence exists and disclose gaps.
    ProduceLimitedAnswerWithGapDisclosure,
    /// End the execution as failed.
    FailExecution,
}

impl Default for MarkdownResearchExecutionLimits {
    fn default() -> Self {
        Self {
            maximum_markdown_corpus_navigation_depth: 8,
            maximum_selected_markdown_corpus_navigation_branches_per_level: 4,
            maximum_active_document_research_branches: 8,
            maximum_selected_markdown_source_documents: 32,
            maximum_read_markdown_source_segments: 128,
            maximum_strong_markdown_research_model_requests: 128,
            maximum_verbatim_source_evidence_extraction_model_requests: 128,
            maximum_total_model_input_token_estimate: 1_000_000,
            maximum_markdown_research_execution_duration_seconds: 1_800,
            resource_exhaustion_outcome:
                ResourceExhaustionOutcome::ProduceLimitedAnswerWithGapDisclosure,
            model_input_token_estimator_version: 1,
            execution_limits_schema_version: EXECUTION_LIMITS_SCHEMA_VERSION,
        }
    }
}

impl MarkdownResearchExecutionLimits {
    /// Validates positive values and hard caps from the implementation plan.
    pub fn validate(&self) -> Result<()> {
        let positive = [
            (
                "maximum_markdown_corpus_navigation_depth",
                u64::from(self.maximum_markdown_corpus_navigation_depth),
                32,
            ),
            (
                "maximum_selected_markdown_corpus_navigation_branches_per_level",
                u64::from(self.maximum_selected_markdown_corpus_navigation_branches_per_level),
                32,
            ),
            (
                "maximum_active_document_research_branches",
                u64::from(self.maximum_active_document_research_branches),
                64,
            ),
            (
                "maximum_selected_markdown_source_documents",
                u64::from(self.maximum_selected_markdown_source_documents),
                1_000,
            ),
            (
                "maximum_read_markdown_source_segments",
                u64::from(self.maximum_read_markdown_source_segments),
                10_000,
            ),
            (
                "maximum_strong_markdown_research_model_requests",
                u64::from(self.maximum_strong_markdown_research_model_requests),
                10_000,
            ),
            (
                "maximum_verbatim_source_evidence_extraction_model_requests",
                u64::from(self.maximum_verbatim_source_evidence_extraction_model_requests),
                10_000,
            ),
            (
                "maximum_total_model_input_token_estimate",
                self.maximum_total_model_input_token_estimate,
                50_000_000,
            ),
            (
                "maximum_markdown_research_execution_duration_seconds",
                self.maximum_markdown_research_execution_duration_seconds,
                86_400,
            ),
        ];
        for (name, value, cap) in positive {
            if value == 0 || value > cap {
                return Err(RuntimeError::validation(
                    RuntimeStage::Setup,
                    format!("{name} must be between 1 and {cap}"),
                ));
            }
        }
        if self.execution_limits_schema_version != EXECUTION_LIMITS_SCHEMA_VERSION {
            return Err(RuntimeError::validation(
                RuntimeStage::Setup,
                "unsupported execution limits schema version",
            ));
        }
        if self.model_input_token_estimator_version == 0 {
            return Err(RuntimeError::validation(
                RuntimeStage::Setup,
                "model input token estimator version must be positive",
            ));
        }
        Ok(())
    }
}

/// A normalized research brief that can be frozen for execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenDocumentResearchBrief {
    /// The user's original question.
    pub original_user_question: String,
    /// The ambiguity-free research question.
    pub clarified_research_question: String,
    /// Publicly safe context inherited from completed requests.
    pub known_document_research_context: Vec<String>,
    /// Explicit assumptions accepted for this execution.
    pub document_research_assumptions: Vec<String>,
    /// Ambiguities that remain and must be disclosed or resolved.
    pub unresolved_research_question_ambiguities: Vec<String>,
    /// User requirements for the final answer.
    pub requested_research_answer_requirements: Vec<String>,
    /// Domain schema version.
    pub document_research_brief_schema_version: u32,
    /// Content hash of the canonical brief.
    pub document_research_brief_content_hash: String,
}

impl FrozenDocumentResearchBrief {
    /// Normalizes and freezes a draft brief, returning its deterministic hash.
    pub fn freeze(
        original_user_question: impl Into<String>,
        clarified_research_question: impl Into<String>,
        known_document_research_context: Vec<String>,
        document_research_assumptions: Vec<String>,
        unresolved_research_question_ambiguities: Vec<String>,
        requested_research_answer_requirements: Vec<String>,
    ) -> Result<Self> {
        let mut brief = Self {
            original_user_question: require_text(
                "original_user_question",
                &original_user_question.into(),
                MAX_RESEARCH_TEXT_BYTES,
            )?,
            clarified_research_question: String::new(),
            known_document_research_context: normalize_text_list(
                "known_document_research_context",
                known_document_research_context,
            )?,
            document_research_assumptions: normalize_text_list(
                "document_research_assumptions",
                document_research_assumptions,
            )?,
            unresolved_research_question_ambiguities: normalize_text_list(
                "unresolved_research_question_ambiguities",
                unresolved_research_question_ambiguities,
            )?,
            requested_research_answer_requirements: normalize_text_list(
                "requested_research_answer_requirements",
                requested_research_answer_requirements,
            )?,
            document_research_brief_schema_version: DOCUMENT_RESEARCH_BRIEF_SCHEMA_VERSION,
            document_research_brief_content_hash: String::new(),
        };
        brief.clarified_research_question = require_text(
            "clarified_research_question",
            &clarified_research_question.into(),
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        let hash_input = FrozenBriefHashInput::from_brief(&brief);
        brief.document_research_brief_content_hash = canonical_content_hash(&hash_input)?;
        Ok(brief)
    }

    /// Validates a previously frozen brief and its content hash.
    pub fn validate(&self) -> Result<()> {
        if self.document_research_brief_schema_version != DOCUMENT_RESEARCH_BRIEF_SCHEMA_VERSION {
            return Err(RuntimeError::validation(
                RuntimeStage::Lifecycle,
                "unsupported document research brief schema version",
            ));
        }
        require_text(
            "original_user_question",
            &self.original_user_question,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        require_text(
            "clarified_research_question",
            &self.clarified_research_question,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        let expected = canonical_content_hash(&FrozenBriefHashInput::from_brief(self))?;
        if expected != self.document_research_brief_content_hash {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Lifecycle,
                message: "document research brief content hash mismatch".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct FrozenBriefHashInput<'a> {
    original_user_question: &'a str,
    clarified_research_question: &'a str,
    known_document_research_context: &'a [String],
    document_research_assumptions: &'a [String],
    unresolved_research_question_ambiguities: &'a [String],
    requested_research_answer_requirements: &'a [String],
    document_research_brief_schema_version: u32,
}

impl<'a> FrozenBriefHashInput<'a> {
    fn from_brief(brief: &'a FrozenDocumentResearchBrief) -> Self {
        Self {
            original_user_question: &brief.original_user_question,
            clarified_research_question: &brief.clarified_research_question,
            known_document_research_context: &brief.known_document_research_context,
            document_research_assumptions: &brief.document_research_assumptions,
            unresolved_research_question_ambiguities: &brief
                .unresolved_research_question_ambiguities,
            requested_research_answer_requirements: &brief.requested_research_answer_requirements,
            document_research_brief_schema_version: brief.document_research_brief_schema_version,
        }
    }
}

fn normalize_text_list(name: &str, values: Vec<String>) -> Result<Vec<String>> {
    if values.len() > 128 {
        return Err(RuntimeError::validation(
            RuntimeStage::Setup,
            format!("{name} contains too many items"),
        ));
    }
    values.into_iter().map(|value| require_text(name, &value, MAX_RESEARCH_TEXT_BYTES)).collect()
}

/// A verified verbatim quote from the canonical Markdown body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbatimSourceEvidence {
    /// Stable evidence ID generated by the program.
    pub verbatim_source_evidence_id: VerbatimSourceEvidenceId,
    /// Public-safe projection reference, populated only at projection time.
    pub internal_markdown_source_reference: String,
    /// Absolute canonical body start offset, inclusive.
    pub verbatim_source_evidence_start_byte_offset: u64,
    /// Absolute canonical body end offset, exclusive.
    pub verbatim_source_evidence_end_byte_offset: u64,
    /// Exact UTF-8 quote at the offsets.
    pub verbatim_source_evidence_quote: String,
    /// Hash of the source segment from which the quote was extracted.
    pub markdown_source_segment_hash: String,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Owning source document.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Owning source segment.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
}

impl VerbatimSourceEvidence {
    /// Validates structural fields; quote membership is checked by the Corpus validator.
    pub fn validate_shape(&self) -> Result<()> {
        if self.verbatim_source_evidence_end_byte_offset
            <= self.verbatim_source_evidence_start_byte_offset
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Execution,
                "verbatim source evidence offsets must be a non-empty range",
            ));
        }
        if self.verbatim_source_evidence_quote.len() > MAX_EVIDENCE_QUOTE_BYTES {
            return Err(RuntimeError::validation(
                RuntimeStage::Execution,
                "verbatim source evidence quote is too long",
            ));
        }
        if self.verbatim_source_evidence_quote.is_empty()
            || self.verbatim_source_evidence_quote.contains('\0')
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Execution,
                "verbatim source evidence quote is invalid",
            ));
        }
        Ok(())
    }
}

/// A relationship between one accepted evidence item and a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchClaimEvidenceRelationship {
    /// Referenced evidence ID.
    pub verbatim_source_evidence_id: VerbatimSourceEvidenceId,
    /// Semantic relationship proposed by the strong model.
    pub research_claim_evidence_relationship_type: ResearchClaimEvidenceRelationshipType,
}

/// A current-execution claim interpreted from accepted evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLinkedResearchClaim {
    /// Program-generated claim ID.
    pub evidence_linked_research_claim_id: EvidenceLinkedResearchClaimId,
    /// Claim text.
    pub evidence_linked_research_claim_text: String,
    /// Evidence relationships.
    pub research_claim_evidence_relationships: Vec<ResearchClaimEvidenceRelationship>,
    /// Conditions under which the claim applies.
    pub evidence_linked_research_claim_applicability_conditions: Vec<String>,
    /// Exceptions identified by the strong model.
    pub evidence_linked_research_claim_exceptions: Vec<String>,
    /// Must be all-citations-linked when committed.
    pub evidence_linked_research_claim_citation_status: EvidenceLinkedResearchClaimCitationStatus,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
}

/// Citation status for an Evidence-Linked Research Claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLinkedResearchClaimCitationStatus {
    /// Every relationship points to accepted evidence.
    AllCitationsLinkedToVerbatimSourceEvidence,
}

impl EvidenceLinkedResearchClaim {
    /// Validates claim shape and duplicate relationship IDs.
    pub fn validate_shape(&self) -> Result<()> {
        require_text(
            "evidence_linked_research_claim_text",
            &self.evidence_linked_research_claim_text,
            MAX_CLAIM_TEXT_BYTES,
        )?;
        if self.research_claim_evidence_relationships.is_empty() {
            return Err(RuntimeError::validation(
                RuntimeStage::Execution,
                "an evidence-linked research claim needs evidence relationships",
            ));
        }
        let mut ids = BTreeSet::new();
        for relationship in &self.research_claim_evidence_relationships {
            if !ids.insert(relationship.verbatim_source_evidence_id.as_str()) {
                return Err(RuntimeError::validation(
                    RuntimeStage::Execution,
                    "duplicate evidence relationship",
                ));
            }
        }
        if self.evidence_linked_research_claim_citation_status
            != EvidenceLinkedResearchClaimCitationStatus::AllCitationsLinkedToVerbatimSourceEvidence
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Execution,
                "unsupported evidence-linked research claim citation status",
            ));
        }
        Ok(())
    }
}

/// A model-only answer that never receives the current Markdown corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelKnowledgeOnlyAnswer {
    /// Program-generated answer ID.
    pub model_knowledge_only_answer_id: MarkdownResearchModelTaskId,
    /// Answer text.
    pub model_knowledge_only_answer_text: String,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
}

impl ModelKnowledgeOnlyAnswer {
    /// Validates the model-only answer shape.
    pub fn validate_shape(&self) -> Result<()> {
        require_text(
            "model_knowledge_only_answer_text",
            &self.model_knowledge_only_answer_text,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        Ok(())
    }
}

/// An answer generated only from committed evidence-linked claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLinkedResearchClaimsAnswer {
    /// Program-generated answer ID.
    pub evidence_linked_research_claims_answer_id: MarkdownResearchModelTaskId,
    /// Answer text.
    pub evidence_linked_research_claims_answer_text: String,
    /// Claims used as the only factual input.
    pub supporting_evidence_linked_research_claim_ids: Vec<EvidenceLinkedResearchClaimId>,
    /// Owning execution.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
}

impl EvidenceLinkedResearchClaimsAnswer {
    /// Validates answer shape and duplicate claim references.
    pub fn validate_shape(&self) -> Result<()> {
        require_text(
            "evidence_linked_research_claims_answer_text",
            &self.evidence_linked_research_claims_answer_text,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        let unique: BTreeSet<_> = self
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .map(|id| id.as_str())
            .collect();
        if unique.len() != self.supporting_evidence_linked_research_claim_ids.len() {
            return Err(RuntimeError::validation(
                RuntimeStage::Execution,
                "duplicate claim reference in evidence-linked answer",
            ));
        }
        Ok(())
    }
}

/// A public citation derived from accepted evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicSourceCitation {
    /// Program-generated public citation ID.
    pub public_source_citation_id: PublicSourceCitationId,
    /// Stable source document ID.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Source document title.
    pub markdown_source_document_title: String,
    /// Section heading, if any.
    pub markdown_source_segment_section_heading: Option<String>,
    /// Exact public quote.
    pub public_source_citation_quote: String,
    /// Version hash, not an internal path or URI.
    pub markdown_source_document_version_content_hash: String,
}

/// One source-attributed segment in a composed answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAttributedAnswerSegment {
    /// Segment text.
    pub source_attributed_answer_segment_text: String,
    /// Provenance category.
    pub source_attributed_answer_segment_source_type: SourceAttributedAnswerSegmentSourceType,
    /// Supporting current-execution claim IDs.
    pub supporting_evidence_linked_research_claim_ids: Vec<EvidenceLinkedResearchClaimId>,
    /// Supporting public citation IDs.
    pub supporting_public_source_citation_ids: Vec<PublicSourceCitationId>,
    /// Explicit marker required for model-only content.
    pub model_knowledge_unverified_notice: Option<String>,
}

impl SourceAttributedAnswerSegment {
    /// Validates source-type-specific references and disclosure requirements.
    pub fn validate_shape(&self) -> Result<()> {
        require_text(
            "source_attributed_answer_segment_text",
            &self.source_attributed_answer_segment_text,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        let claims: BTreeSet<_> = self
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .map(|id| id.as_str())
            .collect();
        if claims.len() != self.supporting_evidence_linked_research_claim_ids.len() {
            return Err(RuntimeError::validation(
                RuntimeStage::Projection,
                "duplicate claim IDs in answer segment",
            ));
        }
        let citations: BTreeSet<_> =
            self.supporting_public_source_citation_ids.iter().map(|id| id.as_str()).collect();
        if citations.len() != self.supporting_public_source_citation_ids.len() {
            return Err(RuntimeError::validation(
                RuntimeStage::Projection,
                "duplicate citation IDs in answer segment",
            ));
        }
        let has_claims = !self.supporting_evidence_linked_research_claim_ids.is_empty();
        let has_model_notice = self
            .model_knowledge_unverified_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("model") || notice.contains("模型"));
        match self.source_attributed_answer_segment_source_type {
            SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaims if !has_claims => {
                Err(RuntimeError::validation(
                    RuntimeStage::Projection,
                    "evidence-linked answer segment needs a claim",
                ))
            }
            SourceAttributedAnswerSegmentSourceType::ModelKnowledgeOnly
            | SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaimsAndModelKnowledge
                if !has_model_notice => Err(RuntimeError::validation(
                RuntimeStage::Projection,
                "model-knowledge answer segment needs an unverified notice",
            )),
            _ => Ok(()),
        }
    }
}

/// A composed answer for one requested style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAttributedAnswerComposition {
    /// Requested composition style.
    pub source_attributed_answer_composition_style: AnswerCompositionStyle,
    /// Model-only input ID.
    pub model_knowledge_only_answer_id: MarkdownResearchModelTaskId,
    /// Evidence answer input ID.
    pub evidence_linked_research_claims_answer_id: MarkdownResearchModelTaskId,
    /// Ordered output segments.
    pub source_attributed_answer_segments: Vec<SourceAttributedAnswerSegment>,
    /// Review-safe reason for the composition.
    pub source_attributed_answer_composition_review_reason: String,
    /// Schema version.
    pub answer_projection_schema_version: u32,
}

impl SourceAttributedAnswerComposition {
    /// Validates every output segment and projection version.
    pub fn validate_shape(&self) -> Result<()> {
        if self.answer_projection_schema_version != ANSWER_PROJECTION_SCHEMA_VERSION {
            return Err(RuntimeError::validation(
                RuntimeStage::Projection,
                "unsupported answer projection schema version",
            ));
        }
        if self.source_attributed_answer_segments.is_empty() {
            return Err(RuntimeError::validation(
                RuntimeStage::Projection,
                "answer composition needs at least one segment",
            ));
        }
        for segment in &self.source_attributed_answer_segments {
            segment.validate_shape()?;
        }
        require_text(
            "source_attributed_answer_composition_review_reason",
            &self.source_attributed_answer_composition_review_reason,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        Ok(())
    }
}

/// A public answer projection for one requested style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicMarkdownResearchAnswer {
    /// Requested style.
    pub source_attributed_answer_composition_style: AnswerCompositionStyle,
    /// Public answer segments.
    pub source_attributed_answer_segments: Vec<SourceAttributedAnswerSegment>,
    /// Public citations referenced by segments.
    pub public_source_citations: Vec<PublicSourceCitation>,
    /// Publicly disclosed high-priority gaps.
    pub disclosed_research_coverage_gaps: Vec<PublicResearchCoverageGap>,
    /// Schema version.
    pub answer_projection_schema_version: u32,
}

/// A public projection of a Research Coverage Gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicResearchCoverageGap {
    /// Public gap identifier.
    pub research_coverage_gap_id: ResearchCoverageGapId,
    /// Unresolved question.
    pub unresolved_research_question: String,
    /// Priority.
    pub research_coverage_gap_priority: ResearchCoverageGapPriority,
    /// Public status.
    pub research_coverage_gap_resolution_status: ResearchCoverageGapResolutionStatus,
    /// Safe explanation.
    pub research_coverage_gap_resolution_explanation: String,
}

/// A coverage gap tracked by the execution engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchCoverageGap {
    /// Program-generated gap ID.
    pub research_coverage_gap_id: ResearchCoverageGapId,
    /// Question still needing an answer.
    pub unresolved_research_question: String,
    /// Priority.
    pub research_coverage_gap_priority: ResearchCoverageGapPriority,
    /// Current status.
    pub research_coverage_gap_resolution_status: ResearchCoverageGapResolutionStatus,
    /// Evidence IDs that resolved it.
    pub research_coverage_gap_resolution_verbatim_source_evidence_ids:
        Vec<VerbatimSourceEvidenceId>,
    /// Safe explanation.
    pub research_coverage_gap_resolution_explanation: String,
}

impl ResearchCoverageGap {
    /// Validates the status/evidence relationship.
    pub fn validate_shape(&self) -> Result<()> {
        require_text(
            "unresolved_research_question",
            &self.unresolved_research_question,
            MAX_RESEARCH_TEXT_BYTES,
        )?;
        if self.research_coverage_gap_resolution_status
            == ResearchCoverageGapResolutionStatus::ResolvedWithVerbatimSourceEvidence
            && self.research_coverage_gap_resolution_verbatim_source_evidence_ids.is_empty()
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Execution,
                "resolved coverage gap needs evidence IDs",
            ));
        }
        Ok(())
    }
}

/// A frozen execution contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarkdownResearchExecution {
    /// Stable execution ID.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Owning conversation.
    pub document_research_conversation_id: DocumentResearchConversationId,
    /// Owning request.
    pub document_research_request_id: DocumentResearchRequestId,
    /// Frozen semantic brief.
    pub frozen_document_research_brief: FrozenDocumentResearchBrief,
    /// Snapshot content addressed by this execution.
    pub markdown_corpus_snapshot_id: MarkdownCorpusSnapshotId,
    /// Strong model reference (no credentials).
    pub strong_markdown_research_model_reference: String,
    /// Cheap extraction model reference (no credentials).
    pub verbatim_source_evidence_extraction_model_reference: String,
    /// Frozen limits.
    pub markdown_research_execution_limits: MarkdownResearchExecutionLimits,
    /// One or both requested answer styles, sorted and deduplicated.
    pub requested_answer_composition_styles: Vec<AnswerCompositionStyle>,
    /// Preparation time.
    pub markdown_research_execution_prepared_at: DateTime<Utc>,
    /// Preparation command ID.
    pub markdown_research_execution_prepare_command_id: CommandId,
}

impl PreparedMarkdownResearchExecution {
    /// Validates all frozen fields and style uniqueness.
    pub fn validate(&self) -> Result<()> {
        self.frozen_document_research_brief.validate()?;
        self.markdown_research_execution_limits.validate()?;
        require_text(
            "strong_markdown_research_model_reference",
            &self.strong_markdown_research_model_reference,
            512,
        )?;
        require_text(
            "verbatim_source_evidence_extraction_model_reference",
            &self.verbatim_source_evidence_extraction_model_reference,
            512,
        )?;
        if self.requested_answer_composition_styles.is_empty()
            || self.requested_answer_composition_styles.len() > 2
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Lifecycle,
                "one or two answer composition styles are required",
            ));
        }
        let mut styles = self.requested_answer_composition_styles.clone();
        styles.sort();
        styles.dedup();
        if styles.len() != self.requested_answer_composition_styles.len()
            || styles != self.requested_answer_composition_styles
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Lifecycle,
                "answer composition styles must be sorted and unique",
            ));
        }
        Ok(())
    }

    /// Returns a stable hash used by command idempotency checks.
    pub fn contract_hash(&self) -> Result<String> {
        canonical_content_hash(&PreparedExecutionContractHashInput::from(self))
    }
}

#[derive(Serialize)]
struct PreparedExecutionContractHashInput<'a> {
    markdown_research_execution_id: &'a MarkdownResearchExecutionId,
    document_research_conversation_id: &'a DocumentResearchConversationId,
    document_research_request_id: &'a DocumentResearchRequestId,
    frozen_document_research_brief: &'a FrozenDocumentResearchBrief,
    markdown_corpus_snapshot_id: &'a MarkdownCorpusSnapshotId,
    strong_markdown_research_model_reference: &'a str,
    verbatim_source_evidence_extraction_model_reference: &'a str,
    markdown_research_execution_limits: &'a MarkdownResearchExecutionLimits,
    requested_answer_composition_styles: &'a [AnswerCompositionStyle],
}

impl<'a> From<&'a PreparedMarkdownResearchExecution> for PreparedExecutionContractHashInput<'a> {
    fn from(prepared: &'a PreparedMarkdownResearchExecution) -> Self {
        Self {
            markdown_research_execution_id: &prepared.markdown_research_execution_id,
            document_research_conversation_id: &prepared.document_research_conversation_id,
            document_research_request_id: &prepared.document_research_request_id,
            frozen_document_research_brief: &prepared.frozen_document_research_brief,
            markdown_corpus_snapshot_id: &prepared.markdown_corpus_snapshot_id,
            strong_markdown_research_model_reference: &prepared
                .strong_markdown_research_model_reference,
            verbatim_source_evidence_extraction_model_reference: &prepared
                .verbatim_source_evidence_extraction_model_reference,
            markdown_research_execution_limits: &prepared.markdown_research_execution_limits,
            requested_answer_composition_styles: &prepared.requested_answer_composition_styles,
        }
    }
}

/// A public overview of an execution, intentionally smaller than the trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownResearchExecutionOverview {
    /// Clarified question.
    pub clarified_research_question: String,
    /// Selected navigation labels.
    pub selected_markdown_corpus_navigation_node_labels: Vec<String>,
    /// Branch report summaries.
    pub markdown_corpus_navigation_branch_document_report_summaries: Vec<String>,
    /// Number of reads.
    pub markdown_source_segment_read_count: u64,
    /// Number of accepted evidence items.
    pub verbatim_source_evidence_count: u64,
    /// Selected source document IDs.
    pub selected_markdown_source_document_ids: Vec<MarkdownSourceDocumentId>,
    /// Current coverage gaps.
    pub research_coverage_gaps: Vec<PublicResearchCoverageGap>,
    /// Stop reason.
    pub markdown_research_execution_stop_reason: String,
    /// Requested styles.
    pub requested_answer_composition_styles: Vec<AnswerCompositionStyle>,
}

/// One whitelist item in a detailed audit page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailedMarkdownResearchAuditItem {
    /// Event sequence represented by this item.
    pub markdown_research_execution_event_sequence_number: u64,
    /// Safe event type.
    pub markdown_research_execution_event_type: String,
    /// Safe summary, never a raw prompt or hidden reasoning.
    pub markdown_research_execution_audit_summary: String,
}

/// A paginated audit projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailedMarkdownResearchAuditPage {
    /// Schema version.
    pub detailed_markdown_research_audit_schema_version: u32,
    /// Whitelist items.
    pub items: Vec<DetailedMarkdownResearchAuditItem>,
    /// Opaque cursor for the next page.
    pub next_cursor: Option<String>,
}

/// A model task kind used for deterministic fixture matching and audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownResearchModelTaskKind {
    /// Evaluate the user's research question.
    ResearchQuestionEvaluation,
    /// Produce the isolated model-only answer.
    ModelKnowledgeOnlyAnswerGeneration,
    /// Select navigation branches.
    MarkdownCorpusNavigationBranchSelection,
    /// Report document relevance within one branch.
    MarkdownCorpusNavigationBranchDocumentRelevanceReport,
    /// Propose a source document/segment read.
    ResearchDocumentReadRequest,
    /// Review one authorized segment.
    MarkdownSourceReview,
    /// Generate evidence-linked claims.
    EvidenceLinkedResearchClaimGeneration,
    /// Generate an answer from committed claims only.
    EvidenceLinkedResearchClaimsAnswerGeneration,
    /// Compose one source-attributed answer.
    SourceAttributedAnswerComposition,
    /// Extract verbatim evidence candidates from one authorized segment.
    VerbatimSourceEvidenceExtraction,
}

/// A stable dispatch checkpoint for one logical model task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownResearchModelDispatchCheckpoint {
    /// Stable model task ID.
    pub markdown_research_model_task_id: MarkdownResearchModelTaskId,
    /// Task kind.
    pub markdown_research_model_task_kind: MarkdownResearchModelTaskKind,
    /// Optional branch task ownership.
    pub document_research_branch_task_id: Option<DocumentResearchBranchTaskId>,
    /// Serialized input content hash.
    pub markdown_research_model_task_input_hash: String,
    /// Frozen token estimator version.
    pub model_input_token_estimator_version: u32,
    /// Estimated input tokens charged once.
    pub estimated_input_tokens: u64,
    /// Command that dispatched the task.
    pub markdown_research_execution_command_id: CommandId,
}

impl MarkdownResearchModelDispatchCheckpoint {
    /// Validates a dispatch checkpoint before it is persisted.
    pub fn validate(&self) -> Result<()> {
        if self.estimated_input_tokens == 0
            || self.model_input_token_estimator_version == 0
            || !self.markdown_research_model_task_input_hash.strip_prefix("sha256:").is_some_and(
                |digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            )
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Model,
                "model dispatch checkpoint has invalid accounting fields",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{MarkdownResearchExecutionId, MarkdownResearchModelTaskId};

    #[test]
    fn canonical_hash_is_stable_for_structured_values() {
        #[derive(Serialize)]
        struct Value {
            z: u32,
            a: &'static str,
        }
        let first = canonical_content_hash(&Value { z: 1, a: "x" }).unwrap();
        let second = canonical_content_hash(&Value { z: 1, a: "x" }).unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
    }

    #[test]
    fn defaults_validate_and_invalid_limits_are_rejected() {
        let defaults = MarkdownResearchExecutionLimits::default();
        defaults.validate().unwrap();
        let mut invalid = defaults;
        invalid.maximum_read_markdown_source_segments = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn frozen_brief_detects_tampering() {
        let mut brief = FrozenDocumentResearchBrief::freeze(
            "original",
            "clarified",
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec!["answer requirement".to_owned()],
        )
        .unwrap();
        brief.clarified_research_question = "changed".to_owned();
        assert!(brief.validate().is_err());
    }

    #[test]
    fn source_types_require_model_disclosure() {
        let segment = SourceAttributedAnswerSegment {
            source_attributed_answer_segment_text: "background".to_owned(),
            source_attributed_answer_segment_source_type:
                SourceAttributedAnswerSegmentSourceType::ModelKnowledgeOnly,
            supporting_evidence_linked_research_claim_ids: Vec::new(),
            supporting_public_source_citation_ids: Vec::new(),
            model_knowledge_unverified_notice: None,
        };
        assert!(segment.validate_shape().is_err());
    }

    #[test]
    fn prepared_execution_requires_sorted_unique_styles() {
        let execution = PreparedMarkdownResearchExecution {
            markdown_research_execution_id: MarkdownResearchExecutionId::generate(),
            document_research_conversation_id: DocumentResearchConversationId::generate(),
            document_research_request_id: DocumentResearchRequestId::generate(),
            frozen_document_research_brief: FrozenDocumentResearchBrief::freeze(
                "q",
                "q",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
            markdown_corpus_snapshot_id: MarkdownCorpusSnapshotId::generate(),
            strong_markdown_research_model_reference: "strong-v1".to_owned(),
            verbatim_source_evidence_extraction_model_reference: "cheap-v1".to_owned(),
            markdown_research_execution_limits: MarkdownResearchExecutionLimits::default(),
            requested_answer_composition_styles: vec![AnswerCompositionStyle::ModelKnowledgeLed],
            markdown_research_execution_prepared_at: Utc::now(),
            markdown_research_execution_prepare_command_id: CommandId::generate(),
        };
        execution.validate().unwrap();
        let _ = MarkdownResearchModelTaskId::generate();
        let _ = MarkdownResearchExecutionId::generate();
    }
}

//! Append-only Markdown Research Execution Trace, replay and whitelist projections.

use crate::domain::{
    AnswerCompositionStyle, DetailedMarkdownResearchAuditItem, DetailedMarkdownResearchAuditPage,
    EvidenceLinkedResearchClaim, EvidenceLinkedResearchClaimsAnswer,
    MarkdownResearchExecutionOverview, MarkdownResearchModelDispatchCheckpoint,
    ModelKnowledgeOnlyAnswer, PreparedMarkdownResearchExecution, PublicMarkdownResearchAnswer,
    PublicResearchCoverageGap, PublicSourceCitation, ResearchCoverageGap,
    ResearchCoverageGapPriority, ResearchCoverageGapResolutionStatus,
    SourceAttributedAnswerComposition, VerbatimSourceEvidence, canonical_content_hash,
};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::identity::{
    CommandId, DocumentResearchBranchTaskId, MarkdownCorpusNavigationCandidateSetId,
    MarkdownCorpusNavigationNodeId, MarkdownResearchExecutionId, MarkdownResearchModelTaskId,
    MarkdownSourceDocumentId, MarkdownSourceSegmentId, ResearchDocumentReadRequestId,
    ResearchPrincipal, SubjectId, VerbatimSourceEvidenceExtractionRequestId,
};
use crate::model_gateway::VerbatimSourceEvidenceCandidateSet;
use crate::storage::{EventStream, NewCommandCommit, NewEvent, Storage, StoredEvent};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Current execution trace schema.
pub const MARKDOWN_RESEARCH_EXECUTION_TRACE_SCHEMA_VERSION: u32 = 1;
/// Maximum audit page size.
pub const MAX_DETAILED_AUDIT_PAGE_SIZE: usize = 200;
/// Maximum serialized size of one detailed audit page.
pub const MAX_DETAILED_AUDIT_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// Selection status for one navigation candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownCorpusNavigationNodeSelectionStatus {
    /// Selected for bounded research.
    SelectedForMarkdownResearch,
    /// Preserved for possible later scope expansion.
    DeferredForLaterMarkdownResearch,
    /// Explicitly excluded from current scope.
    ExcludedFromCurrentMarkdownResearchScope,
}

/// One validated navigation selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationBranchSelection {
    /// Candidate node ID.
    pub markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Selection status.
    pub markdown_corpus_navigation_node_selection_status:
        MarkdownCorpusNavigationNodeSelectionStatus,
    /// Safe relevance explanation.
    pub markdown_corpus_navigation_node_relevance_explanation: String,
    /// Expected information.
    pub expected_research_information_to_resolve_question: String,
    /// Stable priority, lower first.
    pub markdown_corpus_navigation_branch_priority: u32,
}

/// One committed branch document report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationBranchDocumentRelevanceReport {
    /// Branch task ID.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Navigation node ID.
    pub markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Full candidate document IDs visible to the task.
    pub candidate_markdown_source_document_ids: Vec<MarkdownSourceDocumentId>,
    /// Documents recommended by the model; all must be candidates.
    pub selected_markdown_source_document_ids: Vec<MarkdownSourceDocumentId>,
    /// Safe report summary.
    pub markdown_corpus_navigation_branch_document_report_summary: String,
}

/// A persisted authorization request for one source segment read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchDocumentReadRequest {
    /// Stable read request ID.
    pub research_document_read_request_id: ResearchDocumentReadRequestId,
    /// Owning branch task.
    pub document_research_branch_task_id: DocumentResearchBranchTaskId,
    /// Selected document.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Selected segment.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// Unresolved question.
    pub unresolved_research_question: String,
    /// Expected information.
    pub expected_research_information_to_resolve_question: String,
    /// Selection explanation.
    pub markdown_source_document_selection_explanation: String,
}

/// Strong-model follow-up action after reviewing one source segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownSourceFollowUpAction {
    /// Request the cheap evidence extractor.
    ExtractVerbatimSourceEvidence,
    /// Read another segment from the same document.
    ReadAdditionalMarkdownSourceSegment,
    /// Return to the snapshot root and discover more scope.
    ExpandMarkdownCorpusNavigationScope,
    /// Close the current branch.
    CloseMarkdownCorpusNavigationBranch,
}

/// Branch closure reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownCorpusNavigationBranchCloseReason {
    /// Branch is irrelevant.
    MarkdownCorpusNavigationBranchNotRelevantToResearchQuestion,
    /// Branch duplicates another active branch.
    DuplicatesExistingMarkdownCorpusNavigationBranch,
    /// Every relevant segment has been reviewed.
    AllRelevantMarkdownSourceSegmentsReviewed,
    /// Frozen limits prevent more work.
    MarkdownResearchExecutionLimitsExhausted,
}

/// One complete append-only execution fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "markdown_research_execution_event_type", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum MarkdownResearchExecutionEventKind {
    /// Execution contract was prepared and frozen.
    MarkdownResearchExecutionStarted {
        /// Frozen contract.
        prepared_markdown_research_execution: Box<PreparedMarkdownResearchExecution>,
    },
    /// Engine entered running state; timestamp is the duration origin.
    MarkdownResearchExecutionRunning,
    /// Cancellation request won before a response was committed.
    MarkdownResearchExecutionCancellationRequested {
        /// Safe cancellation reason.
        cancellation_explanation: Option<String>,
    },
    /// A logical strong model task consumed its budget once.
    StrongMarkdownResearchModelRequestDispatched {
        /// Dispatch checkpoint.
        markdown_research_model_dispatch_checkpoint: MarkdownResearchModelDispatchCheckpoint,
    },
    /// Isolated model-only answer was generated.
    ModelKnowledgeOnlyAnswerGenerated {
        /// Answer.
        model_knowledge_only_answer: ModelKnowledgeOnlyAnswer,
    },
    /// Complete direct child candidate set was persisted before selection.
    MarkdownCorpusNavigationChildCandidatesPresented {
        /// Candidate set ID.
        markdown_corpus_navigation_candidate_set_id: MarkdownCorpusNavigationCandidateSetId,
        /// Parent node.
        parent_markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
        /// Complete direct children.
        child_markdown_corpus_navigation_node_ids: Vec<MarkdownCorpusNavigationNodeId>,
    },
    /// Strong model selections were validated against a candidate set.
    MarkdownCorpusNavigationBranchesSelected {
        /// Candidate set being selected.
        markdown_corpus_navigation_candidate_set_id: MarkdownCorpusNavigationCandidateSetId,
        /// Selections, one per candidate.
        markdown_corpus_navigation_branch_selections: Vec<MarkdownCorpusNavigationBranchSelection>,
    },
    /// One branch title/abstract report was committed.
    MarkdownCorpusNavigationBranchDocumentReportCommitted {
        /// Report.
        markdown_corpus_navigation_branch_document_relevance_report:
            MarkdownCorpusNavigationBranchDocumentRelevanceReport,
    },
    /// One branch report failed explicitly.
    MarkdownCorpusNavigationBranchDocumentReportFailed {
        /// Branch task ID.
        document_research_branch_task_id: DocumentResearchBranchTaskId,
        /// Navigation node ID.
        markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
        /// Safe failure explanation.
        failure_explanation: String,
    },
    /// A source read authorization was committed.
    ResearchDocumentReadRequestCreated {
        /// Request.
        research_document_read_request: ResearchDocumentReadRequest,
    },
    /// One authorized source segment was read.
    MarkdownSourceSegmentRead {
        /// Read request ID.
        research_document_read_request_id: ResearchDocumentReadRequestId,
        /// Segment hash observed during read.
        markdown_source_segment_hash: String,
    },
    /// Strong model proposed a bounded follow-up.
    MarkdownSourceFollowUpDecided {
        /// Read request ID.
        research_document_read_request_id: ResearchDocumentReadRequestId,
        /// Follow-up action.
        markdown_source_follow_up_action: MarkdownSourceFollowUpAction,
        /// Extraction goal when the action requests cheap evidence extraction.
        #[serde(default)]
        verbatim_source_evidence_extraction_goal: Option<String>,
        /// Accepted evidence IDs that authorize scope expansion.
        #[serde(default)]
        triggering_verbatim_source_evidence_ids: Vec<crate::identity::VerbatimSourceEvidenceId>,
        /// Close reason when the action closes a branch.
        #[serde(default)]
        markdown_corpus_navigation_branch_close_reason:
            Option<MarkdownCorpusNavigationBranchCloseReason>,
        /// Safe review summary, never hidden reasoning.
        #[serde(default)]
        markdown_source_review_summary: Option<String>,
    },
    /// A cheap extraction task was dispatched.
    VerbatimSourceEvidenceExtractionRequested {
        /// Extraction request ID.
        verbatim_source_evidence_extraction_request_id: VerbatimSourceEvidenceExtractionRequestId,
        /// Read authorization being used.
        research_document_read_request_id: ResearchDocumentReadRequestId,
        /// Accounting checkpoint.
        markdown_research_model_dispatch_checkpoint: MarkdownResearchModelDispatchCheckpoint,
    },
    /// Complete cheap-model candidate set persisted before any candidate is accepted.
    VerbatimSourceEvidenceCandidatesPresented {
        /// Extraction request that produced the candidate set.
        verbatim_source_evidence_extraction_request_id: VerbatimSourceEvidenceExtractionRequestId,
        /// The complete response, still containing only segment-relative candidates.
        verbatim_source_evidence_candidate_set: VerbatimSourceEvidenceCandidateSet,
    },
    /// A verified evidence quote was accepted.
    VerbatimSourceEvidenceAccepted {
        /// Extraction request ID.
        verbatim_source_evidence_extraction_request_id: VerbatimSourceEvidenceExtractionRequestId,
        /// Accepted evidence.
        verbatim_source_evidence: VerbatimSourceEvidence,
        /// Public citation derived from it.
        public_source_citation: PublicSourceCitation,
    },
    /// A candidate quote failed deterministic validation.
    VerbatimSourceEvidenceRejected {
        /// Extraction request ID.
        verbatim_source_evidence_extraction_request_id: VerbatimSourceEvidenceExtractionRequestId,
        /// Safe rejection explanation.
        rejection_explanation: String,
    },
    /// Scope expansion was requested and tied to accepted evidence.
    MarkdownCorpusNavigationScopeExpansionRequested {
        /// Triggering accepted evidence IDs.
        triggering_verbatim_source_evidence_ids: Vec<crate::identity::VerbatimSourceEvidenceId>,
    },
    /// A branch was closed.
    MarkdownCorpusNavigationBranchClosed {
        /// Branch task ID.
        document_research_branch_task_id: DocumentResearchBranchTaskId,
        /// Closure reason.
        markdown_corpus_navigation_branch_close_reason: MarkdownCorpusNavigationBranchCloseReason,
    },
    /// One coverage gap was created or changed.
    ResearchCoverageGapUpdated {
        /// Complete current gap value.
        research_coverage_gap: ResearchCoverageGap,
    },
    /// Claims were committed after reference integrity validation.
    EvidenceLinkedResearchClaimsCommitted {
        /// Committed claims.
        evidence_linked_research_claims: Vec<EvidenceLinkedResearchClaim>,
    },
    /// Answer generated from committed claims only.
    EvidenceLinkedResearchClaimsAnswerGenerated {
        /// Answer.
        evidence_linked_research_claims_answer: EvidenceLinkedResearchClaimsAnswer,
    },
    /// One requested answer composition was committed.
    SourceAttributedAnswerComposed {
        /// Composition.
        source_attributed_answer_composition: SourceAttributedAnswerComposition,
    },
    /// All requested compositions and stopping conditions were satisfied.
    MarkdownResearchExecutionCompleted {
        /// Safe stop reason.
        markdown_research_execution_stop_reason: String,
    },
    /// Execution ended in a typed failure.
    MarkdownResearchExecutionFailed {
        /// Stable error code.
        error_code: String,
        /// Safe explanation.
        failure_explanation: String,
    },
    /// Execution ended after cancellation.
    MarkdownResearchExecutionCancelled {
        /// Safe explanation.
        cancellation_explanation: Option<String>,
    },
}

impl MarkdownResearchExecutionEventKind {
    /// Returns the stable event type string.
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::MarkdownResearchExecutionStarted { .. } => "markdown_research_execution_started",
            Self::MarkdownResearchExecutionRunning => "markdown_research_execution_running",
            Self::MarkdownResearchExecutionCancellationRequested { .. } => {
                "markdown_research_execution_cancellation_requested"
            }
            Self::StrongMarkdownResearchModelRequestDispatched { .. } => {
                "strong_markdown_research_model_request_dispatched"
            }
            Self::ModelKnowledgeOnlyAnswerGenerated { .. } => {
                "model_knowledge_only_answer_generated"
            }
            Self::MarkdownCorpusNavigationChildCandidatesPresented { .. } => {
                "markdown_corpus_navigation_child_candidates_presented"
            }
            Self::MarkdownCorpusNavigationBranchesSelected { .. } => {
                "markdown_corpus_navigation_branches_selected"
            }
            Self::MarkdownCorpusNavigationBranchDocumentReportCommitted { .. } => {
                "markdown_corpus_navigation_branch_document_report_committed"
            }
            Self::MarkdownCorpusNavigationBranchDocumentReportFailed { .. } => {
                "markdown_corpus_navigation_branch_document_report_failed"
            }
            Self::ResearchDocumentReadRequestCreated { .. } => {
                "research_document_read_request_created"
            }
            Self::MarkdownSourceSegmentRead { .. } => "markdown_source_segment_read",
            Self::MarkdownSourceFollowUpDecided { .. } => "markdown_source_follow_up_decided",
            Self::VerbatimSourceEvidenceExtractionRequested { .. } => {
                "verbatim_source_evidence_extraction_requested"
            }
            Self::VerbatimSourceEvidenceCandidatesPresented { .. } => {
                "verbatim_source_evidence_candidates_presented"
            }
            Self::VerbatimSourceEvidenceAccepted { .. } => "verbatim_source_evidence_accepted",
            Self::VerbatimSourceEvidenceRejected { .. } => "verbatim_source_evidence_rejected",
            Self::MarkdownCorpusNavigationScopeExpansionRequested { .. } => {
                "markdown_corpus_navigation_scope_expansion_requested"
            }
            Self::MarkdownCorpusNavigationBranchClosed { .. } => {
                "markdown_corpus_navigation_branch_closed"
            }
            Self::ResearchCoverageGapUpdated { .. } => "research_coverage_gap_updated",
            Self::EvidenceLinkedResearchClaimsCommitted { .. } => {
                "evidence_linked_research_claims_committed"
            }
            Self::EvidenceLinkedResearchClaimsAnswerGenerated { .. } => {
                "evidence_linked_research_claims_answer_generated"
            }
            Self::SourceAttributedAnswerComposed { .. } => "source_attributed_answer_composed",
            Self::MarkdownResearchExecutionCompleted { .. } => {
                "markdown_research_execution_completed"
            }
            Self::MarkdownResearchExecutionFailed { .. } => "markdown_research_execution_failed",
            Self::MarkdownResearchExecutionCancelled { .. } => {
                "markdown_research_execution_cancelled"
            }
        }
    }
}

/// Versioned event envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownResearchExecutionEvent {
    /// Trace schema version.
    pub markdown_research_execution_trace_schema_version: u32,
    /// Execution ID.
    pub markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Contiguous one-based sequence.
    pub markdown_research_execution_event_sequence_number: u64,
    /// UTC event time.
    pub markdown_research_execution_event_recorded_at: DateTime<Utc>,
    /// Command ID.
    pub markdown_research_execution_command_id: CommandId,
    /// Event payload.
    pub event_kind: MarkdownResearchExecutionEventKind,
}

/// Terminal state of a replayed execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownResearchExecutionTerminalState {
    /// Completed normally or with disclosed limits.
    Completed,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
}

/// Complete validated execution state after replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayedMarkdownResearchExecution {
    /// Frozen execution.
    pub prepared_markdown_research_execution: PreparedMarkdownResearchExecution,
    /// Owning subject recovered from storage envelope.
    pub owner_subject_id: SubjectId,
    /// Full validated event stream.
    pub events: Vec<MarkdownResearchExecutionEvent>,
    /// Running origin.
    pub markdown_research_execution_running_at: Option<DateTime<Utc>>,
    /// Persisted cancellation request.
    pub cancellation_requested: bool,
    /// Logical strong task count.
    pub strong_markdown_research_model_request_count: u64,
    /// Logical extraction task count.
    pub verbatim_source_evidence_extraction_model_request_count: u64,
    /// Total estimated tokens from dispatch checkpoints.
    pub total_model_input_token_estimate: u64,
    /// Unique logical model tasks already charged.
    pub dispatched_markdown_research_model_task_ids: BTreeSet<MarkdownResearchModelTaskId>,
    /// Unique extraction requests already dispatched.
    pub verbatim_source_evidence_extraction_request_ids:
        BTreeSet<VerbatimSourceEvidenceExtractionRequestId>,
    /// Candidate sets persisted before evidence acceptance, keyed by extraction request.
    pub verbatim_source_evidence_candidate_sets:
        BTreeMap<VerbatimSourceEvidenceExtractionRequestId, VerbatimSourceEvidenceCandidateSet>,
    /// Isolated model-only answer.
    pub model_knowledge_only_answer: Option<ModelKnowledgeOnlyAnswer>,
    /// Candidate sets by ID.
    pub navigation_candidate_sets:
        BTreeMap<MarkdownCorpusNavigationCandidateSetId, Vec<MarkdownCorpusNavigationNodeId>>,
    /// Branch selections.
    pub navigation_branch_selections: Vec<MarkdownCorpusNavigationBranchSelection>,
    /// Branch reports.
    pub branch_document_reports: Vec<MarkdownCorpusNavigationBranchDocumentRelevanceReport>,
    /// Explicitly failed branches.
    pub failed_document_research_branch_task_ids: BTreeSet<DocumentResearchBranchTaskId>,
    /// Read authorizations by ID.
    pub research_document_read_requests:
        BTreeMap<ResearchDocumentReadRequestId, ResearchDocumentReadRequest>,
    /// Accepted evidence.
    pub accepted_verbatim_source_evidence: Vec<VerbatimSourceEvidence>,
    /// Public citations.
    pub public_source_citations: Vec<PublicSourceCitation>,
    /// Current gap values.
    pub research_coverage_gaps: BTreeMap<String, ResearchCoverageGap>,
    /// Committed claims.
    pub evidence_linked_research_claims: Vec<EvidenceLinkedResearchClaim>,
    /// Claims-only answer.
    pub evidence_linked_research_claims_answer: Option<EvidenceLinkedResearchClaimsAnswer>,
    /// Compositions by style.
    pub source_attributed_answer_compositions:
        BTreeMap<AnswerCompositionStyle, SourceAttributedAnswerComposition>,
    /// Terminal state.
    pub terminal_state: Option<MarkdownResearchExecutionTerminalState>,
    /// Safe terminal reason.
    pub terminal_explanation: Option<String>,
}

impl ReplayedMarkdownResearchExecution {
    fn from_started(
        prepared: PreparedMarkdownResearchExecution,
        owner_subject_id: SubjectId,
        event: MarkdownResearchExecutionEvent,
    ) -> Result<Self> {
        prepared.validate().map_err(|_| trace_corrupt("prepared execution contract is invalid"))?;
        Ok(Self {
            prepared_markdown_research_execution: prepared,
            owner_subject_id,
            events: vec![event],
            markdown_research_execution_running_at: None,
            cancellation_requested: false,
            strong_markdown_research_model_request_count: 0,
            verbatim_source_evidence_extraction_model_request_count: 0,
            total_model_input_token_estimate: 0,
            dispatched_markdown_research_model_task_ids: BTreeSet::new(),
            verbatim_source_evidence_extraction_request_ids: BTreeSet::new(),
            verbatim_source_evidence_candidate_sets: BTreeMap::new(),
            model_knowledge_only_answer: None,
            navigation_candidate_sets: BTreeMap::new(),
            navigation_branch_selections: Vec::new(),
            branch_document_reports: Vec::new(),
            failed_document_research_branch_task_ids: BTreeSet::new(),
            research_document_read_requests: BTreeMap::new(),
            accepted_verbatim_source_evidence: Vec::new(),
            public_source_citations: Vec::new(),
            research_coverage_gaps: BTreeMap::new(),
            evidence_linked_research_claims: Vec::new(),
            evidence_linked_research_claims_answer: None,
            source_attributed_answer_compositions: BTreeMap::new(),
            terminal_state: None,
            terminal_explanation: None,
        })
    }

    /// Projects one public answer only after successful full replay.
    pub fn project_public_markdown_research_answer(
        &self,
        style: AnswerCompositionStyle,
    ) -> Result<PublicMarkdownResearchAnswer> {
        if self.terminal_state != Some(MarkdownResearchExecutionTerminalState::Completed) {
            return Err(RuntimeError::InvalidState {
                stage: RuntimeStage::Projection,
                message: "public answer is only available for a completed execution".to_owned(),
            });
        }
        let composition = self
            .source_attributed_answer_compositions
            .get(&style)
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Projection })?;
        let cited_ids: BTreeSet<_> = composition
            .source_attributed_answer_segments
            .iter()
            .flat_map(|segment| segment.supporting_public_source_citation_ids.iter())
            .map(|id| id.as_str())
            .collect();
        let citations = self
            .public_source_citations
            .iter()
            .filter(|citation| cited_ids.contains(citation.public_source_citation_id.as_str()))
            .cloned()
            .collect();
        let gaps = self
            .research_coverage_gaps
            .values()
            .filter(|gap| {
                gap.research_coverage_gap_priority == ResearchCoverageGapPriority::High
                    && gap.research_coverage_gap_resolution_status
                        != ResearchCoverageGapResolutionStatus::ResolvedWithVerbatimSourceEvidence
            })
            .map(public_gap)
            .collect();
        Ok(PublicMarkdownResearchAnswer {
            source_attributed_answer_composition_style: style,
            source_attributed_answer_segments: composition
                .source_attributed_answer_segments
                .clone(),
            public_source_citations: citations,
            disclosed_research_coverage_gaps: gaps,
            answer_projection_schema_version: crate::domain::ANSWER_PROJECTION_SCHEMA_VERSION,
        })
    }

    /// Projects a safe execution overview.
    pub fn project_markdown_research_execution_overview(
        &self,
    ) -> MarkdownResearchExecutionOverview {
        let selected_documents: BTreeSet<_> = self
            .branch_document_reports
            .iter()
            .flat_map(|report| report.selected_markdown_source_document_ids.iter().cloned())
            .collect();
        MarkdownResearchExecutionOverview {
            clarified_research_question: self
                .prepared_markdown_research_execution
                .frozen_document_research_brief
                .clarified_research_question
                .clone(),
            selected_markdown_corpus_navigation_node_labels: self
                .navigation_branch_selections
                .iter()
                .filter(|selection| {
                    selection.markdown_corpus_navigation_node_selection_status
                        == MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                })
                .map(|selection| selection.markdown_corpus_navigation_node_id.to_string())
                .collect(),
            markdown_corpus_navigation_branch_document_report_summaries: self
                .branch_document_reports
                .iter()
                .map(|report| {
                    report.markdown_corpus_navigation_branch_document_report_summary.clone()
                })
                .collect(),
            markdown_source_segment_read_count: self
                .events
                .iter()
                .filter(|event| {
                    matches!(
                        event.event_kind,
                        MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead { .. }
                    )
                })
                .count() as u64,
            verbatim_source_evidence_count: self.accepted_verbatim_source_evidence.len() as u64,
            selected_markdown_source_document_ids: selected_documents.into_iter().collect(),
            research_coverage_gaps: self.research_coverage_gaps.values().map(public_gap).collect(),
            markdown_research_execution_stop_reason: self
                .terminal_explanation
                .clone()
                .unwrap_or_else(|| "execution_not_terminal".to_owned()),
            requested_answer_composition_styles: self
                .prepared_markdown_research_execution
                .requested_answer_composition_styles
                .clone(),
        }
    }

    /// Projects one whitelist audit page using an opaque sequence cursor.
    pub fn project_detailed_markdown_research_audit(
        &self,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<DetailedMarkdownResearchAuditPage> {
        if page_size == 0 || page_size > MAX_DETAILED_AUDIT_PAGE_SIZE {
            return Err(RuntimeError::validation(
                RuntimeStage::Projection,
                "audit page size must be 1..=200",
            ));
        }
        let after_sequence = cursor.map(decode_audit_cursor).transpose()?.unwrap_or(0);
        let remaining: Vec<_> = self
            .events
            .iter()
            .filter(|event| {
                event.markdown_research_execution_event_sequence_number > after_sequence
            })
            .collect();
        let remaining_count = remaining.len();
        let items: Vec<_> = remaining
            .into_iter()
            .take(page_size)
            .map(|event| DetailedMarkdownResearchAuditItem {
                markdown_research_execution_event_sequence_number: event
                    .markdown_research_execution_event_sequence_number,
                markdown_research_execution_event_type: event.event_kind.event_type().to_owned(),
                markdown_research_execution_audit_summary: audit_summary(&event.event_kind),
            })
            .collect();
        build_detailed_audit_page(&items, remaining_count)
    }
}

fn build_detailed_audit_page(
    available_items: &[DetailedMarkdownResearchAuditItem],
    remaining_count: usize,
) -> Result<DetailedMarkdownResearchAuditPage> {
    let mut lower = 0usize;
    let mut upper = available_items.len();
    while lower < upper {
        let candidate_count = lower + (upper - lower).div_ceil(2);
        let candidate =
            detailed_audit_page_candidate(available_items, candidate_count, remaining_count)?;
        if serde_json::to_vec(&candidate)?.len() <= MAX_DETAILED_AUDIT_PAGE_BYTES {
            lower = candidate_count;
        } else {
            upper = candidate_count - 1;
        }
    }
    if lower == 0 && !available_items.is_empty() {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Projection,
            message: "one detailed audit item exceeds the serialized page cap".to_owned(),
        });
    }
    detailed_audit_page_candidate(available_items, lower, remaining_count)
}

fn detailed_audit_page_candidate(
    available_items: &[DetailedMarkdownResearchAuditItem],
    item_count: usize,
    remaining_count: usize,
) -> Result<DetailedMarkdownResearchAuditPage> {
    let items = available_items[..item_count].to_vec();
    let has_more = remaining_count > item_count;
    let next_cursor = if has_more {
        let sequence = items
            .last()
            .ok_or_else(|| RuntimeError::CorruptState {
                stage: RuntimeStage::Projection,
                message: "audit pagination cannot advance within the serialized page cap"
                    .to_owned(),
            })?
            .markdown_research_execution_event_sequence_number;
        Some(encode_audit_cursor(sequence))
    } else {
        None
    };
    Ok(DetailedMarkdownResearchAuditPage {
        detailed_markdown_research_audit_schema_version: 1,
        items,
        next_cursor,
    })
}

/// Persistent Trace Module.
#[derive(Debug, Clone)]
pub struct MarkdownResearchExecutionTrace {
    storage: Storage,
}

impl MarkdownResearchExecutionTrace {
    /// Opens a file-backed Trace.
    #[allow(dead_code)]
    pub(crate) fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { storage: Storage::open(database_path)? })
    }

    /// Creates the Module from shared Runtime storage.
    pub(crate) fn from_storage(storage: Storage) -> Self {
        Self { storage }
    }

    /// Appends an atomic checkpoint event batch after reducing it against current state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn append_markdown_research_execution_events(
        &self,
        principal: &ResearchPrincipal,
        execution_id: &MarkdownResearchExecutionId,
        command_id: CommandId,
        recorded_at: DateTime<Utc>,
        event_kinds: Vec<MarkdownResearchExecutionEventKind>,
    ) -> Result<ReplayedMarkdownResearchExecution> {
        let owner = principal.subject_id.clone();
        let execution_id = execution_id.clone();
        let request_hash = canonical_content_hash(&(&owner, &execution_id, &event_kinds))?;
        let scope = execution_scope(&execution_id);
        self.storage
            .run_blocking(move |storage| {
                storage.transact(|transaction| {
                    if let Some(existing) = transaction.read_command_commit(&scope, &command_id)? {
                        if existing.request_hash != request_hash {
                            return Err(RuntimeError::Conflict {
                                stage: RuntimeStage::Trace,
                                message: "execution command ID conflicts with another request"
                                    .to_owned(),
                            });
                        }
                        return replay_execution_rows(
                            transaction.read_events(EventStream::Execution, &scope)?,
                            Some(&owner),
                        );
                    }
                    if event_kinds.is_empty() {
                        return Err(RuntimeError::validation(
                            RuntimeStage::Trace,
                            "checkpoint event batch must not be empty",
                        ));
                    }
                    let existing_rows = transaction.read_events(EventStream::Execution, &scope)?;
                    let mut state = if existing_rows.is_empty() {
                        None
                    } else {
                        Some(replay_execution_rows(existing_rows, Some(&owner))?)
                    };
                    if let Some(terminal) =
                        state.take_if(|current| current.terminal_state.is_some())
                    {
                        return Ok(terminal);
                    }
                    let first_sequence =
                        state.as_ref().map_or(1, |current| current.events.len() as u64 + 1);
                    let mut envelopes = Vec::with_capacity(event_kinds.len());
                    for (index, kind) in event_kinds.into_iter().enumerate() {
                        let event = MarkdownResearchExecutionEvent {
                            markdown_research_execution_trace_schema_version:
                                MARKDOWN_RESEARCH_EXECUTION_TRACE_SCHEMA_VERSION,
                            markdown_research_execution_id: execution_id.clone(),
                            markdown_research_execution_event_sequence_number: first_sequence
                                + index as u64,
                            markdown_research_execution_event_recorded_at: recorded_at,
                            markdown_research_execution_command_id: command_id.clone(),
                            event_kind: kind,
                        };
                        state = Some(reduce_markdown_research_execution_event(
                            state,
                            &owner,
                            event.clone(),
                        )?);
                        envelopes.push(event);
                    }
                    let events: Vec<_> = envelopes
                        .iter()
                        .map(|event| {
                            Ok(NewEvent {
                                scope: scope.clone(),
                                owner_subject_id: owner.clone(),
                                command_id: command_id.clone(),
                                event_schema_version:
                                    MARKDOWN_RESEARCH_EXECUTION_TRACE_SCHEMA_VERSION,
                                event_type: event.event_kind.event_type().to_owned(),
                                recorded_at,
                                payload_json: serde_json::to_string(&event.event_kind)?,
                            })
                        })
                        .collect::<Result<_>>()?;
                    let command = NewCommandCommit {
                        scope: scope.clone(),
                        command_id,
                        request_hash,
                        result_json: serde_json::to_string(&serde_json::json!({
                            "markdown_research_execution_id": execution_id,
                        }))?,
                        committed_at: recorded_at,
                    };
                    transaction.append_events_with_command(
                        EventStream::Execution,
                        &command,
                        &events,
                    )?;
                    replay_execution_rows(
                        transaction.read_events(EventStream::Execution, &scope)?,
                        Some(&owner),
                    )
                })
            })
            .await
    }

    /// Fully replays and validates one execution.
    pub async fn replay_markdown_research_execution(
        &self,
        principal: &ResearchPrincipal,
        execution_id: &MarkdownResearchExecutionId,
    ) -> Result<ReplayedMarkdownResearchExecution> {
        let owner = principal.subject_id.clone();
        let scope = execution_scope(execution_id);
        self.storage
            .run_blocking(move |storage| {
                replay_execution_rows(
                    storage.read_events(EventStream::Execution, &scope)?,
                    Some(&owner),
                )
            })
            .await
    }
}

/// Replays decoded execution events through the same reducer used before append.
pub fn replay_markdown_research_execution_events(
    owner_subject_id: SubjectId,
    events: &[MarkdownResearchExecutionEvent],
) -> Result<ReplayedMarkdownResearchExecution> {
    let mut state = None;
    for event in events {
        state = Some(reduce_markdown_research_execution_event(
            state,
            &owner_subject_id,
            event.clone(),
        )?);
    }
    state.ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Trace })
}

/// Reduces one execution event and validates observable cross-event relationships.
pub fn reduce_markdown_research_execution_event(
    state: Option<ReplayedMarkdownResearchExecution>,
    owner_subject_id: &SubjectId,
    event: MarkdownResearchExecutionEvent,
) -> Result<ReplayedMarkdownResearchExecution> {
    if event.markdown_research_execution_trace_schema_version
        != MARKDOWN_RESEARCH_EXECUTION_TRACE_SCHEMA_VERSION
    {
        return Err(trace_corrupt("unsupported execution trace schema version"));
    }
    match (state, &event.event_kind) {
        (
            None,
            MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
                prepared_markdown_research_execution,
            },
        ) if event.markdown_research_execution_event_sequence_number == 1
            && prepared_markdown_research_execution.markdown_research_execution_id
                == event.markdown_research_execution_id =>
        {
            ReplayedMarkdownResearchExecution::from_started(
                (**prepared_markdown_research_execution).clone(),
                owner_subject_id.clone(),
                event,
            )
        }
        (None, _) => Err(trace_corrupt(
            "execution stream must start with markdown_research_execution_started",
        )),
        (Some(mut state), kind) => {
            let previous =
                state.events.last().ok_or_else(|| trace_corrupt("missing prior event"))?;
            if event.markdown_research_execution_id
                != state.prepared_markdown_research_execution.markdown_research_execution_id
                || event.markdown_research_execution_event_sequence_number
                    != previous.markdown_research_execution_event_sequence_number + 1
                || event.markdown_research_execution_event_recorded_at
                    < previous.markdown_research_execution_event_recorded_at
                || state.terminal_state.is_some()
            {
                return Err(trace_corrupt(
                    "execution event identity, sequence, time or terminal ordering is invalid",
                ));
            }
            if let Some(running_at) = state.markdown_research_execution_running_at
                && !matches!(
                    kind,
                    MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed { .. }
                        | MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancelled { .. }
                        | MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancellationRequested { .. }
                        | MarkdownResearchExecutionEventKind::ResearchCoverageGapUpdated { .. }
                )
            {
                let limit_seconds = i64::try_from(
                    state
                        .prepared_markdown_research_execution
                        .markdown_research_execution_limits
                        .maximum_markdown_research_execution_duration_seconds,
                )
                .map_err(|_| trace_corrupt("frozen execution duration is invalid"))?;
                if event.markdown_research_execution_event_recorded_at
                    > running_at + chrono::TimeDelta::seconds(limit_seconds)
                {
                    return Err(trace_corrupt("persisted work exceeds the frozen duration limit"));
                }
            }
            match kind {
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted { .. } => {
                    return Err(trace_corrupt("execution started event is duplicated"));
                }
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionRunning => {
                    if state.markdown_research_execution_running_at.is_some() {
                        return Err(trace_corrupt("execution running event is duplicated"));
                    }
                    state.markdown_research_execution_running_at =
                        Some(event.markdown_research_execution_event_recorded_at);
                }
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancellationRequested {
                    ..
                } => {
                    if state.cancellation_requested {
                        return Err(trace_corrupt("cancellation request is duplicated"));
                    }
                    state.cancellation_requested = true;
                }
                MarkdownResearchExecutionEventKind::StrongMarkdownResearchModelRequestDispatched {
                    markdown_research_model_dispatch_checkpoint,
                } => {
                    require_running(&state)?;
                    markdown_research_model_dispatch_checkpoint
                        .validate()
                        .map_err(|_| trace_corrupt("strong model dispatch checkpoint is invalid"))?;
                    if markdown_research_model_dispatch_checkpoint
                        .markdown_research_model_task_kind
                        == crate::domain::MarkdownResearchModelTaskKind::VerbatimSourceEvidenceExtraction
                        || markdown_research_model_dispatch_checkpoint
                            .markdown_research_execution_command_id
                            != event.markdown_research_execution_command_id
                        || markdown_research_model_dispatch_checkpoint
                            .model_input_token_estimator_version
                            != state
                                .prepared_markdown_research_execution
                                .markdown_research_execution_limits
                                .model_input_token_estimator_version
                        || !state.dispatched_markdown_research_model_task_ids.insert(
                        markdown_research_model_dispatch_checkpoint
                            .markdown_research_model_task_id
                            .clone(),
                    )
                    {
                        return Err(trace_corrupt("logical model task ID is duplicated"));
                    }
                    state.strong_markdown_research_model_request_count += 1;
                    charge_tokens(&mut state, markdown_research_model_dispatch_checkpoint)?;
                    validate_replayed_limits(&state)?;
                }
                MarkdownResearchExecutionEventKind::ModelKnowledgeOnlyAnswerGenerated {
                    model_knowledge_only_answer,
                } => {
                    require_running(&state)?;
                    model_knowledge_only_answer
                        .validate_shape()
                        .map_err(|_| trace_corrupt("model-only answer payload is invalid"))?;
                    if state.model_knowledge_only_answer.is_some()
                        || model_knowledge_only_answer.markdown_research_execution_id
                            != event.markdown_research_execution_id
                    {
                        return Err(trace_corrupt("model-only answer is duplicated or mis-owned"));
                    }
                    state.model_knowledge_only_answer = Some(model_knowledge_only_answer.clone());
                }
                MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationChildCandidatesPresented {
                    markdown_corpus_navigation_candidate_set_id,
                    child_markdown_corpus_navigation_node_ids,
                    ..
                } => {
                    require_running(&state)?;
                    if state.model_knowledge_only_answer.is_none()
                        || child_markdown_corpus_navigation_node_ids.is_empty()
                        || has_duplicate_ids(child_markdown_corpus_navigation_node_ids)
                        || state
                            .navigation_candidate_sets
                            .insert(
                                markdown_corpus_navigation_candidate_set_id.clone(),
                                child_markdown_corpus_navigation_node_ids.clone(),
                            )
                            .is_some()
                    {
                        return Err(trace_corrupt("navigation candidate set is invalid or duplicated"));
                    }
                }
                MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchesSelected {
                    markdown_corpus_navigation_candidate_set_id,
                    markdown_corpus_navigation_branch_selections,
                } => {
                    if state.events.iter().any(|prior| {
                        matches!(
                            &prior.event_kind,
                            MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchesSelected {
                                markdown_corpus_navigation_candidate_set_id: prior_id,
                                ..
                            } if prior_id == markdown_corpus_navigation_candidate_set_id
                        )
                    }) {
                        return Err(trace_corrupt("navigation candidate set was selected twice"));
                    }
                    let candidates = state
                        .navigation_candidate_sets
                        .get(markdown_corpus_navigation_candidate_set_id)
                        .ok_or_else(|| trace_corrupt("branch selection has no candidate set"))?;
                    let selected_ids: BTreeSet<_> = markdown_corpus_navigation_branch_selections
                        .iter()
                        .map(|selection| &selection.markdown_corpus_navigation_node_id)
                        .collect();
                    if selected_ids != candidates.iter().collect()
                        || markdown_corpus_navigation_branch_selections.len() != candidates.len()
                    {
                        return Err(trace_corrupt(
                            "branch selections must cover exactly the candidate set",
                        ));
                    }
                    let selected_in_set = markdown_corpus_navigation_branch_selections
                        .iter()
                        .filter(|selection| {
                            selection.markdown_corpus_navigation_node_selection_status
                                == MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                        })
                        .count();
                    if selected_in_set
                        > state
                            .prepared_markdown_research_execution
                            .markdown_research_execution_limits
                            .maximum_selected_markdown_corpus_navigation_branches_per_level
                            as usize
                    {
                        return Err(trace_corrupt(
                            "persisted branch selection exceeds the frozen per-level limit",
                        ));
                    }
                    state
                        .navigation_branch_selections
                        .extend(markdown_corpus_navigation_branch_selections.clone());
                    let selected_nodes: BTreeSet<_> = state
                        .navigation_branch_selections
                        .iter()
                        .filter(|selection| {
                            selection.markdown_corpus_navigation_node_selection_status
                                == MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                        })
                        .map(|selection| selection.markdown_corpus_navigation_node_id.as_str())
                        .collect();
                    if selected_nodes.len()
                        > state
                            .prepared_markdown_research_execution
                            .markdown_research_execution_limits
                            .maximum_active_document_research_branches
                            as usize
                    {
                        return Err(trace_corrupt(
                            "persisted branch selection exceeds the frozen active-branch limit",
                        ));
                    }
                }
                MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchDocumentReportCommitted {
                    markdown_corpus_navigation_branch_document_relevance_report: report,
                } => {
                    if state.branch_document_reports.iter().any(|existing| {
                        existing.document_research_branch_task_id
                            == report.document_research_branch_task_id
                            || existing.markdown_corpus_navigation_node_id
                                == report.markdown_corpus_navigation_node_id
                    }) || !state.navigation_branch_selections.iter().any(|selection| {
                        selection.markdown_corpus_navigation_node_id
                            == report.markdown_corpus_navigation_node_id
                            && selection.markdown_corpus_navigation_node_selection_status
                                == MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                    }) || !report
                        .selected_markdown_source_document_ids
                        .iter()
                        .all(|id| report.candidate_markdown_source_document_ids.contains(id))
                    {
                        return Err(trace_corrupt(
                            "branch report is not owned by a selected candidate set",
                        ));
                    }
                    state.branch_document_reports.push(report.clone());
                    let selected_documents: BTreeSet<_> = state
                        .branch_document_reports
                        .iter()
                        .flat_map(|report| {
                            report.selected_markdown_source_document_ids.iter().map(|id| id.as_str())
                        })
                        .collect();
                    if selected_documents.len()
                        > state
                            .prepared_markdown_research_execution
                            .markdown_research_execution_limits
                            .maximum_selected_markdown_source_documents
                            as usize
                    {
                        return Err(trace_corrupt(
                            "persisted document selection exceeds the frozen document limit",
                        ));
                    }
                }
                MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchDocumentReportFailed {
                    document_research_branch_task_id,
                    markdown_corpus_navigation_node_id,
                    ..
                } => {
                    if !state.navigation_branch_selections.iter().any(|selection| {
                        &selection.markdown_corpus_navigation_node_id
                            == markdown_corpus_navigation_node_id
                            && selection.markdown_corpus_navigation_node_selection_status
                                == MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                    }) || !state
                        .failed_document_research_branch_task_ids
                        .insert(document_research_branch_task_id.clone())
                    {
                        return Err(trace_corrupt("failed branch report is invalid or duplicated"));
                    }
                }
                MarkdownResearchExecutionEventKind::ResearchDocumentReadRequestCreated {
                    research_document_read_request: request,
                } => {
                    let selected = state.branch_document_reports.iter().any(|report| {
                        report.document_research_branch_task_id
                            == request.document_research_branch_task_id
                            && report
                                .selected_markdown_source_document_ids
                                .contains(&request.markdown_source_document_id)
                    });
                    if request.research_document_read_request_id.as_str().is_empty()
                        || request.document_research_branch_task_id.as_str().is_empty()
                        || request.markdown_source_document_id.as_str().is_empty()
                        || request.markdown_source_segment_id.as_str().is_empty()
                        || !selected
                        || state
                            .research_document_read_requests
                            .insert(request.research_document_read_request_id.clone(), request.clone())
                            .is_some()
                    {
                        return Err(trace_corrupt(
                            "read request references a document outside its committed report",
                        ));
                    }
                }
                MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead {
                    research_document_read_request_id,
                    markdown_source_segment_hash,
                } => {
                    if !is_sha256_hash(markdown_source_segment_hash)
                        || !state
                            .research_document_read_requests
                            .contains_key(research_document_read_request_id)
                        || has_prior_segment_read(&state, research_document_read_request_id)
                    {
                        return Err(trace_corrupt("source read has no authorization or hash"));
                    }
                    let read_count = state
                        .events
                        .iter()
                        .filter(|event| {
                            matches!(
                                event.event_kind,
                                MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead { .. }
                            )
                        })
                        .count()
                        + 1;
                    if read_count
                        > state
                            .prepared_markdown_research_execution
                            .markdown_research_execution_limits
                            .maximum_read_markdown_source_segments
                            as usize
                    {
                        return Err(trace_corrupt(
                            "persisted source reads exceed the frozen segment limit",
                        ));
                    }
                }
                MarkdownResearchExecutionEventKind::MarkdownSourceFollowUpDecided {
                    research_document_read_request_id,
                    markdown_source_follow_up_action,
                    verbatim_source_evidence_extraction_goal,
                    triggering_verbatim_source_evidence_ids,
                    markdown_corpus_navigation_branch_close_reason,
                    ..
                } => {
                    if !has_prior_segment_read(&state, research_document_read_request_id) {
                        return Err(trace_corrupt("source review has no prior segment read"));
                    }
                    validate_follow_up_shape(
                        *markdown_source_follow_up_action,
                        verbatim_source_evidence_extraction_goal.as_deref(),
                        triggering_verbatim_source_evidence_ids,
                        *markdown_corpus_navigation_branch_close_reason,
                    )?;
                }
                MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceExtractionRequested {
                    research_document_read_request_id,
                    markdown_research_model_dispatch_checkpoint,
                    verbatim_source_evidence_extraction_request_id,
                } => {
                    if !has_prior_segment_read(&state, research_document_read_request_id)
                    {
                        return Err(trace_corrupt("extraction has no prior authorized segment read"));
                    }
                    markdown_research_model_dispatch_checkpoint
                        .validate()
                        .map_err(|_| trace_corrupt("extraction dispatch checkpoint is invalid"))?;
                    let read_request = state
                        .research_document_read_requests
                        .get(research_document_read_request_id)
                        .ok_or_else(|| trace_corrupt("extraction read request disappeared"))?;
                    if markdown_research_model_dispatch_checkpoint
                        .markdown_research_model_task_kind
                        != crate::domain::MarkdownResearchModelTaskKind::VerbatimSourceEvidenceExtraction
                        || markdown_research_model_dispatch_checkpoint
                            .document_research_branch_task_id
                            .as_ref()
                            != Some(&read_request.document_research_branch_task_id)
                        || markdown_research_model_dispatch_checkpoint
                            .markdown_research_execution_command_id
                            != event.markdown_research_execution_command_id
                        || markdown_research_model_dispatch_checkpoint
                            .model_input_token_estimator_version
                            != state
                                .prepared_markdown_research_execution
                                .markdown_research_execution_limits
                                .model_input_token_estimator_version
                        || !state.verbatim_source_evidence_extraction_request_ids.insert(
                        verbatim_source_evidence_extraction_request_id.clone(),
                    )
                        || !state.dispatched_markdown_research_model_task_ids.insert(
                        markdown_research_model_dispatch_checkpoint
                            .markdown_research_model_task_id
                            .clone(),
                    )
                    {
                        return Err(trace_corrupt("extraction request or model task ID is duplicated"));
                    }
                    state.verbatim_source_evidence_extraction_model_request_count += 1;
                    charge_tokens(&mut state, markdown_research_model_dispatch_checkpoint)?;
                    validate_replayed_limits(&state)?;
                }
                MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceCandidatesPresented {
                    verbatim_source_evidence_extraction_request_id,
                    verbatim_source_evidence_candidate_set,
                } => {
                    let read_request_id = state
                        .events
                        .iter()
                        .rev()
                        .find_map(|prior| match &prior.event_kind {
                            MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceExtractionRequested {
                                verbatim_source_evidence_extraction_request_id: prior_id,
                                research_document_read_request_id,
                                ..
                            } if prior_id == verbatim_source_evidence_extraction_request_id => {
                                Some(research_document_read_request_id)
                            }
                            _ => None,
                        })
                        .ok_or_else(|| trace_corrupt("candidate set has no extraction request"))?;
                    let read_request = state
                        .research_document_read_requests
                        .get(read_request_id)
                        .ok_or_else(|| trace_corrupt("candidate set has no authorized read"))?;
                    if state
                        .verbatim_source_evidence_candidate_sets
                        .contains_key(verbatim_source_evidence_extraction_request_id)
                        || verbatim_source_evidence_candidate_set
                            .verbatim_source_evidence_extraction_request_id
                            != *verbatim_source_evidence_extraction_request_id
                        || verbatim_source_evidence_candidate_set.document_research_branch_task_id
                            != read_request.document_research_branch_task_id
                        || verbatim_source_evidence_candidate_set.markdown_source_document_id
                            != read_request.markdown_source_document_id
                        || verbatim_source_evidence_candidate_set.markdown_source_segment_id
                            != read_request.markdown_source_segment_id
                        || !is_sha256_hash(
                            &verbatim_source_evidence_candidate_set.markdown_source_segment_hash,
                        )
                        || has_duplicate_candidate_coordinates(verbatim_source_evidence_candidate_set)
                        || verbatim_source_evidence_candidate_set
                            .verbatim_source_evidence_candidates
                            .iter()
                            .any(|candidate| {
                                candidate.verbatim_source_evidence_quote.is_empty()
                                    || candidate.verbatim_source_evidence_end_byte_offset_in_segment
                                        <= candidate
                                            .verbatim_source_evidence_start_byte_offset_in_segment
                                    || candidate.verbatim_source_evidence_end_byte_offset_in_segment
                                        - candidate
                                            .verbatim_source_evidence_start_byte_offset_in_segment
                                        != u64::try_from(
                                            candidate.verbatim_source_evidence_quote.len(),
                                        )
                                        .unwrap_or_default()
                            })
                    {
                        return Err(trace_corrupt("persisted evidence candidate set is invalid or duplicated"));
                    }
                    state.verbatim_source_evidence_candidate_sets.insert(
                        verbatim_source_evidence_extraction_request_id.clone(),
                        verbatim_source_evidence_candidate_set.clone(),
                    );
                }
                MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceAccepted {
                    verbatim_source_evidence_extraction_request_id,
                    verbatim_source_evidence,
                    public_source_citation,
                } => {
                    if !has_prior_extraction(&state, verbatim_source_evidence_extraction_request_id)
                        || !state
                            .verbatim_source_evidence_candidate_sets
                            .contains_key(verbatim_source_evidence_extraction_request_id)
                        || has_prior_candidate_outcome(
                            &state,
                            verbatim_source_evidence_extraction_request_id,
                        )
                        || state.accepted_verbatim_source_evidence.iter().any(|existing| {
                            existing.verbatim_source_evidence_id
                                == verbatim_source_evidence.verbatim_source_evidence_id
                        })
                    {
                        return Err(trace_corrupt(
                            "accepted evidence has no extraction request or is duplicated",
                        ));
                    }
                    verbatim_source_evidence
                        .validate_shape()
                        .map_err(|_| trace_corrupt("accepted evidence payload is invalid"))?;
                    let candidate_set = state
                        .verbatim_source_evidence_candidate_sets
                        .get(verbatim_source_evidence_extraction_request_id)
                        .expect("candidate set checked above");
                    let matching_candidate = candidate_set
                        .verbatim_source_evidence_candidates
                        .iter()
                        .any(|candidate| {
                            candidate.verbatim_source_evidence_quote
                                == verbatim_source_evidence.verbatim_source_evidence_quote
                                && candidate.verbatim_source_evidence_end_byte_offset_in_segment
                                    > candidate.verbatim_source_evidence_start_byte_offset_in_segment
                                && candidate.verbatim_source_evidence_end_byte_offset_in_segment
                                    - candidate.verbatim_source_evidence_start_byte_offset_in_segment
                                    == u64::try_from(
                                        verbatim_source_evidence
                                            .verbatim_source_evidence_quote
                                            .len(),
                                    )
                                    .unwrap_or_default()
                        });
                    let citation_matches = public_source_citation.public_source_citation_quote
                        == verbatim_source_evidence.verbatim_source_evidence_quote
                        && public_source_citation.markdown_source_document_id
                            == verbatim_source_evidence.markdown_source_document_id
                        && is_sha256_hash(
                            &public_source_citation
                                .markdown_source_document_version_content_hash,
                        )
                        && !public_source_citation.markdown_source_document_title.is_empty();
                    if !matching_candidate
                        || !citation_matches
                        || verbatim_source_evidence.markdown_research_execution_id
                            != state
                                .prepared_markdown_research_execution
                                .markdown_research_execution_id
                        || verbatim_source_evidence.document_research_branch_task_id
                            != candidate_set.document_research_branch_task_id
                        || verbatim_source_evidence.markdown_source_document_id
                            != candidate_set.markdown_source_document_id
                        || verbatim_source_evidence.markdown_source_segment_id
                            != candidate_set.markdown_source_segment_id
                        || !is_sha256_hash(&verbatim_source_evidence.markdown_source_segment_hash)
                        || verbatim_source_evidence.markdown_source_segment_hash
                            != candidate_set.markdown_source_segment_hash
                        || verbatim_source_evidence.verbatim_source_evidence_end_byte_offset
                            <= verbatim_source_evidence.verbatim_source_evidence_start_byte_offset
                        || verbatim_source_evidence.verbatim_source_evidence_end_byte_offset
                            - verbatim_source_evidence.verbatim_source_evidence_start_byte_offset
                            != u64::try_from(
                                verbatim_source_evidence.verbatim_source_evidence_quote.len(),
                            )
                            .unwrap_or_default()
                        || state.public_source_citations.iter().any(|existing| {
                            existing.public_source_citation_id
                                == public_source_citation.public_source_citation_id
                        })
                    {
                        return Err(trace_corrupt("accepted evidence has no matching candidate"));
                    }
                    state
                        .accepted_verbatim_source_evidence
                        .push(verbatim_source_evidence.clone());
                    state.public_source_citations.push(public_source_citation.clone());
                }
                MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceRejected {
                    verbatim_source_evidence_extraction_request_id,
                    ..
                } => {
                    if !has_prior_extraction(&state, verbatim_source_evidence_extraction_request_id)
                        || !state
                            .verbatim_source_evidence_candidate_sets
                            .contains_key(verbatim_source_evidence_extraction_request_id)
                        || has_prior_candidate_outcome(
                            &state,
                            verbatim_source_evidence_extraction_request_id,
                        )
                    {
                        return Err(trace_corrupt("rejected evidence has no extraction request"));
                    }
                }
                MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationScopeExpansionRequested {
                    triggering_verbatim_source_evidence_ids,
                } => {
                    if triggering_verbatim_source_evidence_ids.is_empty()
                        || !triggering_verbatim_source_evidence_ids.iter().all(|id| {
                            state.accepted_verbatim_source_evidence.iter().any(|evidence| {
                                &evidence.verbatim_source_evidence_id == id
                            })
                        })
                    {
                        return Err(trace_corrupt(
                            "scope expansion must be triggered by accepted evidence",
                        ));
                    }
                }
                MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchClosed { .. } => {}
                MarkdownResearchExecutionEventKind::ResearchCoverageGapUpdated {
                    research_coverage_gap,
                } => {
                    research_coverage_gap
                        .validate_shape()
                        .map_err(|_| trace_corrupt("coverage gap payload is invalid"))?;
                    state.research_coverage_gaps.insert(
                        research_coverage_gap.research_coverage_gap_id.to_string(),
                        research_coverage_gap.clone(),
                    );
                }
                MarkdownResearchExecutionEventKind::EvidenceLinkedResearchClaimsCommitted {
                    evidence_linked_research_claims,
                } => commit_claims(&mut state, &event, evidence_linked_research_claims)?,
                MarkdownResearchExecutionEventKind::EvidenceLinkedResearchClaimsAnswerGenerated {
                    evidence_linked_research_claims_answer,
                } => commit_claims_answer(&mut state, evidence_linked_research_claims_answer)?,
                MarkdownResearchExecutionEventKind::SourceAttributedAnswerComposed {
                    source_attributed_answer_composition,
                } => commit_composition(&mut state, source_attributed_answer_composition)?,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCompleted {
                    markdown_research_execution_stop_reason,
                } => {
                    validate_completion(&state)?;
                    state.terminal_state = Some(MarkdownResearchExecutionTerminalState::Completed);
                    state.terminal_explanation =
                        Some(markdown_research_execution_stop_reason.clone());
                }
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed {
                    failure_explanation,
                    ..
                } => {
                    state.terminal_state = Some(MarkdownResearchExecutionTerminalState::Failed);
                    state.terminal_explanation = Some(failure_explanation.clone());
                }
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancelled {
                    cancellation_explanation,
                } => {
                    if !state.cancellation_requested {
                        return Err(trace_corrupt(
                            "cancelled terminal requires a persisted cancellation request",
                        ));
                    }
                    state.terminal_state = Some(MarkdownResearchExecutionTerminalState::Cancelled);
                    state.terminal_explanation = cancellation_explanation.clone();
                }
            }
            state.events.push(event);
            Ok(state)
        }
    }
}

fn commit_claims(
    state: &mut ReplayedMarkdownResearchExecution,
    event: &MarkdownResearchExecutionEvent,
    claims: &[EvidenceLinkedResearchClaim],
) -> Result<()> {
    if !state.evidence_linked_research_claims.is_empty() || claims.is_empty() {
        return Err(trace_corrupt("claims commit is empty or duplicated"));
    }
    let accepted: BTreeSet<_> = state
        .accepted_verbatim_source_evidence
        .iter()
        .map(|evidence| evidence.verbatim_source_evidence_id.as_str())
        .collect();
    let mut claim_ids = BTreeSet::new();
    for claim in claims {
        claim.validate_shape().map_err(|_| trace_corrupt("committed claim payload is invalid"))?;
        if !claim_ids.insert(claim.evidence_linked_research_claim_id.clone())
            || claim.markdown_research_execution_id != event.markdown_research_execution_id
            || !claim.research_claim_evidence_relationships.iter().all(|relationship| {
                accepted.contains(relationship.verbatim_source_evidence_id.as_str())
            })
        {
            return Err(trace_corrupt("claim references unaccepted evidence"));
        }
    }
    state.evidence_linked_research_claims = claims.to_vec();
    Ok(())
}

fn commit_claims_answer(
    state: &mut ReplayedMarkdownResearchExecution,
    answer: &EvidenceLinkedResearchClaimsAnswer,
) -> Result<()> {
    if state.evidence_linked_research_claims.is_empty()
        || state.evidence_linked_research_claims_answer.is_some()
    {
        return Err(trace_corrupt("claims answer is out of order or duplicated"));
    }
    answer.validate_shape().map_err(|_| trace_corrupt("claims answer payload is invalid"))?;
    if answer.markdown_research_execution_id
        != state.prepared_markdown_research_execution.markdown_research_execution_id
        || answer.supporting_evidence_linked_research_claim_ids.is_empty()
    {
        return Err(trace_corrupt("claims answer is not owned by this execution"));
    }
    let committed: BTreeSet<_> = state
        .evidence_linked_research_claims
        .iter()
        .map(|claim| claim.evidence_linked_research_claim_id.as_str())
        .collect();
    if !answer
        .supporting_evidence_linked_research_claim_ids
        .iter()
        .all(|id| committed.contains(id.as_str()))
    {
        return Err(trace_corrupt("claims answer references an uncommitted claim"));
    }
    state.evidence_linked_research_claims_answer = Some(answer.clone());
    Ok(())
}

fn commit_composition(
    state: &mut ReplayedMarkdownResearchExecution,
    composition: &SourceAttributedAnswerComposition,
) -> Result<()> {
    if state.model_knowledge_only_answer.is_none()
        || state.evidence_linked_research_claims_answer.is_none()
        || !state
            .prepared_markdown_research_execution
            .requested_answer_composition_styles
            .contains(&composition.source_attributed_answer_composition_style)
    {
        return Err(trace_corrupt("answer composition is out of order or unrequested"));
    }
    composition
        .validate_shape()
        .map_err(|_| trace_corrupt("answer composition payload is invalid"))?;
    if composition.model_knowledge_only_answer_id
        != state
            .model_knowledge_only_answer
            .as_ref()
            .expect("model-only answer checked above")
            .model_knowledge_only_answer_id
        || composition.evidence_linked_research_claims_answer_id
            != state
                .evidence_linked_research_claims_answer
                .as_ref()
                .expect("claims answer checked above")
                .evidence_linked_research_claims_answer_id
    {
        return Err(trace_corrupt("composition is not bound to its committed answer inputs"));
    }
    let claim_ids: BTreeSet<_> = state
        .evidence_linked_research_claims
        .iter()
        .map(|claim| claim.evidence_linked_research_claim_id.as_str())
        .collect();
    let citation_ids: BTreeSet<_> = state
        .public_source_citations
        .iter()
        .map(|citation| citation.public_source_citation_id.as_str())
        .collect();
    for segment in &composition.source_attributed_answer_segments {
        if segment
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .any(|id| !claim_ids.contains(id.as_str()))
            || segment
                .supporting_public_source_citation_ids
                .iter()
                .any(|id| !citation_ids.contains(id.as_str()))
        {
            return Err(trace_corrupt("composition references an unknown claim or citation"));
        }
    }
    if state
        .source_attributed_answer_compositions
        .insert(composition.source_attributed_answer_composition_style, composition.clone())
        .is_some()
    {
        return Err(trace_corrupt("answer style is duplicated"));
    }
    Ok(())
}

fn validate_follow_up_shape(
    action: MarkdownSourceFollowUpAction,
    extraction_goal: Option<&str>,
    triggering_evidence_ids: &[crate::identity::VerbatimSourceEvidenceId],
    close_reason: Option<MarkdownCorpusNavigationBranchCloseReason>,
) -> Result<()> {
    let has_goal = extraction_goal.is_some_and(|goal| !goal.trim().is_empty());
    match action {
        MarkdownSourceFollowUpAction::ExtractVerbatimSourceEvidence
            if !has_goal || !triggering_evidence_ids.is_empty() || close_reason.is_some() =>
        {
            Err(trace_corrupt("extraction follow-up fields are invalid"))
        }
        MarkdownSourceFollowUpAction::ExpandMarkdownCorpusNavigationScope
            if extraction_goal.is_some()
                || triggering_evidence_ids.is_empty()
                || close_reason.is_some() =>
        {
            Err(trace_corrupt("scope-expansion follow-up fields are invalid"))
        }
        MarkdownSourceFollowUpAction::CloseMarkdownCorpusNavigationBranch
            if extraction_goal.is_some()
                || !triggering_evidence_ids.is_empty()
                || close_reason.is_none() =>
        {
            Err(trace_corrupt("branch-close follow-up fields are invalid"))
        }
        MarkdownSourceFollowUpAction::ReadAdditionalMarkdownSourceSegment
            if extraction_goal.is_some()
                || !triggering_evidence_ids.is_empty()
                || close_reason.is_some() =>
        {
            Err(trace_corrupt("additional-read follow-up fields are invalid"))
        }
        _ => Ok(()),
    }
}

fn replay_execution_rows(
    rows: Vec<StoredEvent>,
    expected_owner: Option<&SubjectId>,
) -> Result<ReplayedMarkdownResearchExecution> {
    if rows.is_empty() {
        return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Trace });
    }
    let execution_id_text = rows[0]
        .scope
        .strip_prefix("execution:")
        .ok_or_else(|| trace_corrupt("invalid execution scope"))?;
    let execution_id = MarkdownResearchExecutionId::from_value(execution_id_text)?;
    let stream_owner = rows[0].owner_subject_id.clone();
    if rows.iter().any(|row| row.owner_subject_id != stream_owner) {
        return Err(trace_corrupt("execution storage envelope changes owner within one stream"));
    }
    if expected_owner.is_some_and(|owner| owner != &stream_owner) {
        return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Trace });
    }
    let mut events = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        if row.scope != execution_scope(&execution_id)
            || row.owner_subject_id != stream_owner
            || row.sequence != index as i64 + 1
        {
            return Err(trace_corrupt("execution storage envelope is not contiguous"));
        }
        let kind: MarkdownResearchExecutionEventKind = serde_json::from_str(&row.payload_json)
            .map_err(|error| trace_corrupt(&format!("invalid execution event payload: {error}")))?;
        if row.event_type != kind.event_type() {
            return Err(trace_corrupt("execution event type does not match payload"));
        }
        events.push(MarkdownResearchExecutionEvent {
            markdown_research_execution_trace_schema_version: row.event_schema_version,
            markdown_research_execution_id: execution_id.clone(),
            markdown_research_execution_event_sequence_number: row.sequence as u64,
            markdown_research_execution_event_recorded_at: row.recorded_at,
            markdown_research_execution_command_id: row.command_id,
            event_kind: kind,
        });
    }
    replay_markdown_research_execution_events(stream_owner, &events)
}

fn execution_scope(execution_id: &MarkdownResearchExecutionId) -> String {
    format!("execution:{execution_id}")
}

fn require_running(state: &ReplayedMarkdownResearchExecution) -> Result<()> {
    if state.markdown_research_execution_running_at.is_some() {
        Ok(())
    } else {
        Err(trace_corrupt("execution work occurred before running event"))
    }
}

fn charge_tokens(
    state: &mut ReplayedMarkdownResearchExecution,
    checkpoint: &MarkdownResearchModelDispatchCheckpoint,
) -> Result<()> {
    state.total_model_input_token_estimate = state
        .total_model_input_token_estimate
        .checked_add(checkpoint.estimated_input_tokens)
        .ok_or_else(|| trace_corrupt("model token estimate overflow"))?;
    Ok(())
}

fn has_prior_extraction(
    state: &ReplayedMarkdownResearchExecution,
    request_id: &VerbatimSourceEvidenceExtractionRequestId,
) -> bool {
    state.events.iter().any(|event| {
        matches!(
            &event.event_kind,
            MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceExtractionRequested {
                verbatim_source_evidence_extraction_request_id,
                ..
            } if verbatim_source_evidence_extraction_request_id == request_id
        )
    })
}

fn has_prior_candidate_outcome(
    state: &ReplayedMarkdownResearchExecution,
    request_id: &VerbatimSourceEvidenceExtractionRequestId,
) -> bool {
    state.events.iter().any(|event| {
        matches!(
            &event.event_kind,
            MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceAccepted {
                verbatim_source_evidence_extraction_request_id,
                ..
            }
            | MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceRejected {
                verbatim_source_evidence_extraction_request_id,
                ..
            } if verbatim_source_evidence_extraction_request_id == request_id
        )
    })
}

fn has_duplicate_candidate_coordinates(candidate_set: &VerbatimSourceEvidenceCandidateSet) -> bool {
    let mut coordinates = BTreeSet::new();
    candidate_set.verbatim_source_evidence_candidates.iter().any(|candidate| {
        !coordinates.insert((
            candidate.verbatim_source_evidence_start_byte_offset_in_segment,
            candidate.verbatim_source_evidence_end_byte_offset_in_segment,
        ))
    })
}

fn has_prior_segment_read(
    state: &ReplayedMarkdownResearchExecution,
    request_id: &ResearchDocumentReadRequestId,
) -> bool {
    state.events.iter().any(|event| {
        matches!(
            &event.event_kind,
            MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead {
                research_document_read_request_id,
                ..
            } if research_document_read_request_id == request_id
        )
    })
}

fn validate_replayed_limits(state: &ReplayedMarkdownResearchExecution) -> Result<()> {
    let limits = &state.prepared_markdown_research_execution.markdown_research_execution_limits;
    if state.strong_markdown_research_model_request_count
        > u64::from(limits.maximum_strong_markdown_research_model_requests)
        || state.verbatim_source_evidence_extraction_model_request_count
            > u64::from(limits.maximum_verbatim_source_evidence_extraction_model_requests)
        || state.total_model_input_token_estimate > limits.maximum_total_model_input_token_estimate
    {
        return Err(trace_corrupt("persisted model dispatch exceeds frozen execution limits"));
    }
    Ok(())
}

fn validate_completion(state: &ReplayedMarkdownResearchExecution) -> Result<()> {
    if state.model_knowledge_only_answer.is_none()
        || state.evidence_linked_research_claims_answer.is_none()
    {
        return Err(trace_corrupt("completed execution requires both answer inputs"));
    }
    let requested: BTreeSet<_> = state
        .prepared_markdown_research_execution
        .requested_answer_composition_styles
        .iter()
        .copied()
        .collect();
    let composed: BTreeSet<_> =
        state.source_attributed_answer_compositions.keys().copied().collect();
    if requested != composed {
        return Err(trace_corrupt(
            "completed execution must contain every requested answer style exactly once",
        ));
    }
    let selected_nodes: BTreeSet<_> = state
        .navigation_branch_selections
        .iter()
        .filter(|selection| {
            selection.markdown_corpus_navigation_node_selection_status
                == MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
        })
        .map(|selection| &selection.markdown_corpus_navigation_node_id)
        .collect();
    let reported_nodes: BTreeSet<_> = state
        .branch_document_reports
        .iter()
        .map(|report| &report.markdown_corpus_navigation_node_id)
        .collect();
    let failed_nodes: BTreeSet<_> = state
        .events
        .iter()
        .filter_map(|event| {
            match &event.event_kind {
            MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchDocumentReportFailed {
                markdown_corpus_navigation_node_id,
                ..
            } => Some(markdown_corpus_navigation_node_id),
            _ => None,
        }
        })
        .collect();
    if selected_nodes
        .iter()
        .any(|node| !reported_nodes.contains(node) && !failed_nodes.contains(node))
    {
        return Err(trace_corrupt(
            "completed execution has a selected branch without report or explicit failure",
        ));
    }
    let unresolved_high_gap = state.research_coverage_gaps.values().any(|gap| {
        gap.research_coverage_gap_priority == ResearchCoverageGapPriority::High
            && !matches!(
                gap.research_coverage_gap_resolution_status,
                ResearchCoverageGapResolutionStatus::ResolvedWithVerbatimSourceEvidence
                    | ResearchCoverageGapResolutionStatus::UnableToResolveFromMarkdownCorpus
                    | ResearchCoverageGapResolutionStatus::DisclosedInAnswer
            )
    });
    if unresolved_high_gap {
        return Err(trace_corrupt(
            "completed execution contains an undisclosed high-priority coverage gap",
        ));
    }
    Ok(())
}

fn has_duplicate_ids<T: Ord + Clone>(values: &[T]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().any(|value| !seen.insert(value.clone()))
}

fn is_sha256_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn trace_corrupt(message: &str) -> RuntimeError {
    RuntimeError::CorruptState { stage: RuntimeStage::Trace, message: message.to_owned() }
}

fn public_gap(gap: &ResearchCoverageGap) -> PublicResearchCoverageGap {
    PublicResearchCoverageGap {
        research_coverage_gap_id: gap.research_coverage_gap_id.clone(),
        unresolved_research_question: gap.unresolved_research_question.clone(),
        research_coverage_gap_priority: gap.research_coverage_gap_priority,
        research_coverage_gap_resolution_status: gap.research_coverage_gap_resolution_status,
        research_coverage_gap_resolution_explanation: gap
            .research_coverage_gap_resolution_explanation
            .clone(),
    }
}

fn audit_summary(kind: &MarkdownResearchExecutionEventKind) -> String {
    match kind {
        MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted { .. } => {
            "Frozen execution contract recorded".to_owned()
        }
        MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceAccepted {
            verbatim_source_evidence,
            ..
        } => format!(
            "Accepted verbatim evidence {} ({} bytes)",
            verbatim_source_evidence.verbatim_source_evidence_id,
            verbatim_source_evidence.verbatim_source_evidence_quote.len()
        ),
        MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed {
            error_code, ..
        } => format!("Execution failed with {error_code}"),
        _ => kind.event_type().replace('_', " "),
    }
}

fn encode_audit_cursor(sequence: u64) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sequence.to_be_bytes())
}

fn decode_audit_cursor(cursor: &str) -> Result<u64> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| RuntimeError::validation(RuntimeStage::Projection, "invalid audit cursor"))?;
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        RuntimeError::validation(RuntimeStage::Projection, "invalid audit cursor length")
    })?;
    Ok(u64::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MarkdownResearchExecutionLimits;
    use crate::identity::{
        DocumentResearchConversationId, DocumentResearchRequestId, MarkdownCorpusSnapshotId,
    };

    fn prepared() -> PreparedMarkdownResearchExecution {
        PreparedMarkdownResearchExecution {
            markdown_research_execution_id: MarkdownResearchExecutionId::generate(),
            document_research_conversation_id: DocumentResearchConversationId::generate(),
            document_research_request_id: DocumentResearchRequestId::generate(),
            frozen_document_research_brief: crate::domain::FrozenDocumentResearchBrief::freeze(
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
        }
    }

    fn event(
        prepared: &PreparedMarkdownResearchExecution,
        sequence: u64,
        kind: MarkdownResearchExecutionEventKind,
    ) -> MarkdownResearchExecutionEvent {
        MarkdownResearchExecutionEvent {
            markdown_research_execution_trace_schema_version:
                MARKDOWN_RESEARCH_EXECUTION_TRACE_SCHEMA_VERSION,
            markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
            markdown_research_execution_event_sequence_number: sequence,
            markdown_research_execution_event_recorded_at: prepared
                .markdown_research_execution_prepared_at,
            markdown_research_execution_command_id: CommandId::generate(),
            event_kind: kind,
        }
    }

    #[test]
    fn failed_stream_accepts_a_legal_answer_prefix() {
        let prepared = prepared();
        let events = vec![
            event(
                &prepared,
                1,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
                    prepared_markdown_research_execution: Box::new(prepared.clone()),
                },
            ),
            event(
                &prepared,
                2,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed {
                    error_code: "model_transport".to_owned(),
                    failure_explanation: "model failed".to_owned(),
                },
            ),
        ];
        let replay = replay_markdown_research_execution_events(
            SubjectId::from_value("subject-1").unwrap(),
            &events,
        )
        .unwrap();
        assert_eq!(replay.terminal_state, Some(MarkdownResearchExecutionTerminalState::Failed));
    }

    #[test]
    fn completion_requires_both_answers_and_requested_compositions() {
        let prepared = prepared();
        let events = vec![
            event(
                &prepared,
                1,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
                    prepared_markdown_research_execution: Box::new(prepared.clone()),
                },
            ),
            event(
                &prepared,
                2,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionRunning,
            ),
            event(
                &prepared,
                3,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCompleted {
                    markdown_research_execution_stop_reason: "done".to_owned(),
                },
            ),
        ];
        assert!(
            replay_markdown_research_execution_events(
                SubjectId::from_value("subject-1").unwrap(),
                &events,
            )
            .is_err()
        );
    }

    #[test]
    fn audit_cursor_pages_without_raw_payloads() {
        let prepared = prepared();
        let events = vec![
            event(
                &prepared,
                1,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
                    prepared_markdown_research_execution: Box::new(prepared.clone()),
                },
            ),
            event(
                &prepared,
                2,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionRunning,
            ),
            event(
                &prepared,
                3,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancellationRequested {
                    cancellation_explanation: Some("cancel".to_owned()),
                },
            ),
            event(
                &prepared,
                4,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancelled {
                    cancellation_explanation: Some("cancel".to_owned()),
                },
            ),
        ];
        let replay = replay_markdown_research_execution_events(
            SubjectId::from_value("subject-1").unwrap(),
            &events,
        )
        .unwrap();
        let first = replay.project_detailed_markdown_research_audit(None, 1).unwrap();
        let second = replay
            .project_detailed_markdown_research_audit(first.next_cursor.as_deref(), 1)
            .unwrap();
        assert_eq!(second.items[0].markdown_research_execution_event_sequence_number, 2);
    }

    #[test]
    fn audit_page_respects_the_serialized_byte_cap() {
        let items: Vec<_> = (1..=MAX_DETAILED_AUDIT_PAGE_SIZE)
            .map(|sequence| DetailedMarkdownResearchAuditItem {
                markdown_research_execution_event_sequence_number: sequence as u64,
                markdown_research_execution_event_type: "large_safe_summary".to_owned(),
                markdown_research_execution_audit_summary: "x".repeat(32 * 1024),
            })
            .collect();

        let page = build_detailed_audit_page(&items, items.len()).unwrap();

        assert!(page.items.len() < items.len());
        assert!(page.next_cursor.is_some());
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_DETAILED_AUDIT_PAGE_BYTES);
    }

    #[tokio::test]
    async fn persists_and_replays_atomic_checkpoint_batches() {
        let trace = MarkdownResearchExecutionTrace { storage: Storage::open_in_memory().unwrap() };
        let principal = ResearchPrincipal::new(
            SubjectId::from_value("subject-1").unwrap(),
            [crate::identity::PrincipalCapability::ExecuteMarkdownResearch],
        );
        let prepared = prepared();
        let kinds = vec![
            MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
                prepared_markdown_research_execution: Box::new(prepared.clone()),
            },
            MarkdownResearchExecutionEventKind::MarkdownResearchExecutionRunning,
        ];
        let replay = trace
            .append_markdown_research_execution_events(
                &principal,
                &prepared.markdown_research_execution_id,
                CommandId::generate(),
                Utc::now(),
                kinds,
            )
            .await
            .unwrap();
        assert_eq!(replay.events.len(), 2);
    }
}

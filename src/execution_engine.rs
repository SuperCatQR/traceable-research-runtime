//! Fixed, replay-first Markdown Research Execution orchestration.
//!
//! The engine is deliberately the only module that decides the next research
//! action. Model adapters return proposals; this module validates them, appends
//! an atomic checkpoint, and only then makes the proposal visible to the next
//! step. It owns no transport or persistence implementation beyond the deep
//! Trace and Corpus interfaces.

use crate::corpus::MarkdownCorpusSnapshot;
use crate::domain::{
    MarkdownResearchModelDispatchCheckpoint, PreparedMarkdownResearchExecution,
    ResearchCoverageGap, ResearchCoverageGapPriority, ResearchCoverageGapResolutionStatus,
    ResourceExhaustionOutcome, canonical_content_hash,
};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::execution_trace::{
    MarkdownCorpusNavigationBranchCloseReason,
    MarkdownCorpusNavigationBranchDocumentRelevanceReport, MarkdownCorpusNavigationBranchSelection,
    MarkdownResearchExecutionEventKind, MarkdownResearchExecutionTrace,
    MarkdownSourceFollowUpAction, ReplayedMarkdownResearchExecution, ResearchDocumentReadRequest,
};
use crate::identity::{
    CommandId, DocumentResearchBranchTaskId, EvidenceLinkedResearchClaimId,
    MarkdownCorpusNavigationCandidateSetId, MarkdownCorpusNavigationNodeId,
    MarkdownResearchExecutionId, MarkdownResearchModelTaskId, MarkdownSourceDocumentId,
    PublicSourceCitationId, ResearchCoverageGapId, ResearchDocumentReadRequestId,
    ResearchPrincipal, VerbatimSourceEvidenceExtractionRequestId, VerbatimSourceEvidenceId,
};
use crate::integrity::{
    MarkdownSourceEvidenceIntegrityValidator, PersistedAuthorizedMarkdownSourceRead,
    PersistedVerbatimSourceEvidenceCandidateSet, ProgramAssignedVerbatimSourceEvidenceIds,
    ValidateSourceAttributedAnswerCompositionInput, ValidateVerbatimSourceEvidenceCandidateInput,
};
use crate::model_gateway::{
    AcceptedVerbatimSourceEvidenceModelContext, AuthorizedMarkdownSourceSegmentInput,
    EvidenceLinkedResearchClaimGenerationResponse, EvidenceLinkedResearchClaimGenerationTask,
    EvidenceLinkedResearchClaimsAnswerGenerationTask, MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
    MarkdownCorpusNavigationBranchDocumentRelevanceReportTask,
    MarkdownCorpusNavigationBranchSelectionTask, MarkdownCorpusNavigationNodeCandidate,
    MarkdownResearchModelGateway, MarkdownSourceReviewDecision, MarkdownSourceReviewTask,
    MarkdownSourceSegmentMetadata, ModelKnowledgeOnlyAnswerGenerationTask,
    ResearchDocumentReadRequestTask, SourceAttributedAnswerCompositionTask,
    StrongMarkdownResearchModelResponse, StrongMarkdownResearchModelTask,
    VerbatimSourceEvidenceCandidateSet, VerbatimSourceEvidenceExtractionTask,
};
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// A model-independent fixed workflow engine.
#[derive(Clone)]
pub(crate) struct MarkdownResearchExecutionEngine {
    gateway: Arc<dyn MarkdownResearchModelGateway>,
}

impl MarkdownResearchExecutionEngine {
    /// Creates an engine around one model Gateway Adapter.
    pub(crate) fn new(gateway: Arc<dyn MarkdownResearchModelGateway>) -> Self {
        Self { gateway }
    }

    /// Executes or resumes one frozen execution against one immutable snapshot.
    pub(crate) async fn execute_prepared_markdown_research(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        snapshot: &MarkdownCorpusSnapshot,
        trace: &MarkdownResearchExecutionTrace,
    ) -> Result<ReplayedMarkdownResearchExecution> {
        principal.require(crate::identity::PrincipalCapability::ExecuteMarkdownResearch)?;
        prepared.validate()?;
        if snapshot.owner_subject_id != principal.subject_id
            || snapshot.markdown_corpus_snapshot_id != prepared.markdown_corpus_snapshot_id
        {
            return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus });
        }

        let mut state = match trace
            .replay_markdown_research_execution(principal, &prepared.markdown_research_execution_id)
            .await
        {
            Ok(state) => state,
            Err(RuntimeError::ObjectNotAvailable { .. }) => {
                let initial = vec![
                    MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
                        prepared_markdown_research_execution: Box::new(prepared.clone()),
                    },
                    MarkdownResearchExecutionEventKind::MarkdownResearchExecutionRunning,
                ];
                trace
                    .append_markdown_research_execution_events(
                        principal,
                        &prepared.markdown_research_execution_id,
                        command_id(prepared, "execution-start"),
                        Utc::now(),
                        initial,
                    )
                    .await?
            }
            Err(error) => return Err(error),
        };

        if state.prepared_markdown_research_execution.contract_hash()?
            != prepared.contract_hash()?
        {
            return Err(RuntimeError::Conflict {
                stage: RuntimeStage::Execution,
                message: "execution contract conflicts with the frozen Trace".to_owned(),
            });
        }
        if state.terminal_state.is_some() {
            return Ok(state);
        }
        if state.markdown_research_execution_running_at.is_none() {
            state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state,
                    "execution-running",
                    vec![MarkdownResearchExecutionEventKind::MarkdownResearchExecutionRunning],
                )
                .await?;
        }

        let reader = snapshot.reader();
        let validator =
            MarkdownSourceEvidenceIntegrityValidator::for_locked_markdown_corpus_snapshot(
                &principal.subject_id,
                &prepared.markdown_research_execution_id,
                &prepared.markdown_corpus_snapshot_id,
                &prepared.requested_answer_composition_styles,
                snapshot,
            )?;

        if let Err(error) =
            self.ensure_model_knowledge_answer(principal, prepared, trace, &mut state).await
        {
            if let RuntimeError::LimitExceeded { message } = error {
                return self
                    .finish_limit_exhaustion(principal, prepared, trace, state, &message, false)
                    .await;
            }
            return Err(error);
        }
        if state.cancellation_requested || state.terminal_state.is_some() {
            return Ok(state);
        }

        if let Err(error) = self
            .explore_navigation_and_sources(
                principal, prepared, snapshot, trace, &reader, &validator, &mut state,
            )
            .await
        {
            if let RuntimeError::LimitExceeded { message } = error {
                state = self
                    .finish_limit_exhaustion(principal, prepared, trace, state, &message, true)
                    .await?;
                if state.terminal_state.is_some() {
                    return Ok(state);
                }
            } else {
                return Err(error);
            }
        }

        let accepted = validator.validate_persisted_evidence(
            &state.accepted_verbatim_source_evidence,
            &state.public_source_citations,
        )?;
        if accepted.is_empty() {
            let failed = MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed {
                error_code: "no_verbatim_source_evidence".to_owned(),
                failure_explanation: "no authorized source evidence was accepted".to_owned(),
            };
            return self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state,
                    "execution-no-evidence",
                    vec![failed],
                )
                .await;
        }

        if let Err(error) = self
            .generate_claims_and_answers(
                principal, prepared, trace, &validator, &mut state, accepted,
            )
            .await
        {
            if let RuntimeError::LimitExceeded { message } = error {
                return self
                    .finish_limit_exhaustion(principal, prepared, trace, state, &message, false)
                    .await;
            }
            return Err(error);
        }
        if state.cancellation_requested || state.terminal_state.is_some() {
            return Ok(state);
        }

        if state.terminal_state.is_none() {
            state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state,
                    "execution-complete",
                    vec![MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCompleted {
                        markdown_research_execution_stop_reason: "all requested answers composed"
                            .to_owned(),
                    }],
                )
                .await?;
        }
        Ok(state)
    }

    async fn finish_limit_exhaustion(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        trace: &MarkdownResearchExecutionTrace,
        state: ReplayedMarkdownResearchExecution,
        limit_message: &str,
        can_continue_with_evidence: bool,
    ) -> Result<ReplayedMarkdownResearchExecution> {
        let has_usable_evidence = !state.accepted_verbatim_source_evidence.is_empty();
        let disclose_and_continue = can_continue_with_evidence
            && has_usable_evidence
            && prepared.markdown_research_execution_limits.resource_exhaustion_outcome
                == ResourceExhaustionOutcome::ProduceLimitedAnswerWithGapDisclosure;
        if disclose_and_continue {
            let gap_id = research_coverage_gap_id(prepared, limit_message);
            let gap_already_recorded = state.research_coverage_gaps.contains_key(gap_id.as_str());
            let gap = ResearchCoverageGap {
                research_coverage_gap_id: gap_id,
                unresolved_research_question: prepared
                    .frozen_document_research_brief
                    .clarified_research_question
                    .clone(),
                research_coverage_gap_priority: ResearchCoverageGapPriority::High,
                research_coverage_gap_resolution_status:
                    ResearchCoverageGapResolutionStatus::DisclosedInAnswer,
                research_coverage_gap_resolution_verbatim_source_evidence_ids: Vec::new(),
                research_coverage_gap_resolution_explanation: format!(
                    "Research scope was limited because {limit_message}."
                ),
            };
            let reported_nodes: BTreeSet<_> = state
                .branch_document_reports
                .iter()
                .map(|report| report.markdown_corpus_navigation_node_id.as_str())
                .collect();
            let failed_nodes: BTreeSet<_> = state
                .events
                .iter()
                .filter_map(|event| match &event.event_kind {
                    MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchDocumentReportFailed {
                        markdown_corpus_navigation_node_id,
                        ..
                    } => Some(markdown_corpus_navigation_node_id.as_str()),
                    _ => None,
                })
                .collect();
            let pending_nodes: BTreeSet<_> = state
                .navigation_branch_selections
                .iter()
                .filter(|selection| {
                    selection.markdown_corpus_navigation_node_selection_status
                        == crate::execution_trace::MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                        && !reported_nodes
                            .contains(selection.markdown_corpus_navigation_node_id.as_str())
                        && !failed_nodes
                            .contains(selection.markdown_corpus_navigation_node_id.as_str())
                })
                .map(|selection| selection.markdown_corpus_navigation_node_id.clone())
                .collect();
            let mut events: Vec<_> = pending_nodes
                .into_iter()
                .map(|markdown_corpus_navigation_node_id| {
                    MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchDocumentReportFailed {
                        document_research_branch_task_id: branch_task_id(
                            prepared,
                            &markdown_corpus_navigation_node_id,
                        ),
                        markdown_corpus_navigation_node_id,
                        failure_explanation: format!(
                            "branch report was not completed because {limit_message}"
                        ),
                    }
                })
                .collect();
            if !gap_already_recorded {
                events.push(MarkdownResearchExecutionEventKind::ResearchCoverageGapUpdated {
                    research_coverage_gap: gap.clone(),
                });
            }
            if events.is_empty() {
                return Ok(state);
            }
            let command_key = if gap_already_recorded {
                format!(
                    "coverage-gap-limit-{}-{}",
                    gap.research_coverage_gap_id,
                    canonical_content_hash(&events)?
                )
            } else {
                format!("coverage-gap-limit-{}", gap.research_coverage_gap_id)
            };
            return self
                .append_events(principal, prepared, trace, state, &command_key, events)
                .await;
        }
        self.append_events(
            principal,
            prepared,
            trace,
            state,
            "execution-limit-exhausted",
            vec![MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed {
                error_code: "markdown_research_execution_limit_exceeded".to_owned(),
                failure_explanation: if has_usable_evidence {
                    format!("execution stopped because {limit_message}")
                } else {
                    format!("execution stopped without usable evidence because {limit_message}")
                },
            }],
        )
        .await
    }

    async fn ensure_model_knowledge_answer(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        trace: &MarkdownResearchExecutionTrace,
        state: &mut ReplayedMarkdownResearchExecution,
    ) -> Result<()> {
        if state.model_knowledge_only_answer.is_some() {
            return Ok(());
        }
        let task_id = model_task_id(prepared, "model-knowledge-only");
        let task = StrongMarkdownResearchModelTask::ModelKnowledgeOnlyAnswerGeneration(
            ModelKnowledgeOnlyAnswerGenerationTask {
                markdown_research_model_task_id: task_id.clone(),
                markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
                frozen_document_research_brief: prepared.frozen_document_research_brief.clone(),
                allowed_completed_research_context: Vec::new(),
                markdown_research_model_task_schema_version:
                    MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
            },
        );
        self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
        let response = self.gateway.execute_strong_markdown_research_task(task).await?;
        let StrongMarkdownResearchModelResponse::ModelKnowledgeOnlyAnswerGeneration(answer) =
            response
        else {
            return Err(RuntimeError::ModelResponse {
                message: "model-only task returned another response kind".to_owned(),
            });
        };
        *state = self
            .append_events(
                principal,
                prepared,
                trace,
                state.clone(),
                "model-knowledge-only-answer",
                vec![MarkdownResearchExecutionEventKind::ModelKnowledgeOnlyAnswerGenerated {
                    model_knowledge_only_answer: answer,
                }],
            )
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn explore_navigation_and_sources(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        snapshot: &MarkdownCorpusSnapshot,
        trace: &MarkdownResearchExecutionTrace,
        reader: &crate::corpus::MarkdownCorpusSnapshotReader<'_>,
        validator: &MarkdownSourceEvidenceIntegrityValidator<'_>,
        state: &mut ReplayedMarkdownResearchExecution,
    ) -> Result<()> {
        let root = snapshot.root_markdown_corpus_navigation_node_id.clone();
        let mut expansion_round = 0usize;
        let mut selected_at_depth =
            BTreeMap::<u32, BTreeSet<MarkdownCorpusNavigationNodeId>>::new();
        loop {
            let expansion_requests = scope_expansion_requests(state);
            let triggering_evidence_ids = if expansion_round == 0 {
                Vec::new()
            } else {
                expansion_requests.get(expansion_round - 1).cloned().ok_or_else(|| {
                    RuntimeError::CorruptState {
                        stage: RuntimeStage::Trace,
                        message: "scope expansion round has no persisted trigger".to_owned(),
                    }
                })?
            };
            let mut pending = vec![(root.clone(), 0_u32)];
            let mut visited = BTreeSet::new();
            while let Some((parent_id, depth)) = pending.pop() {
                if depth
                    >= prepared
                        .markdown_research_execution_limits
                        .maximum_markdown_corpus_navigation_depth
                    || !visited.insert(parent_id.clone())
                {
                    continue;
                }
                if state.cancellation_requested || state.terminal_state.is_some() {
                    return Ok(());
                }
                let children =
                    reader.list_direct_child_markdown_corpus_navigation_nodes(&parent_id)?;
                if children.is_empty() {
                    continue;
                }
                let candidate_set_id =
                    candidate_set_id(prepared, &parent_id, depth, expansion_round);
                let child_ids: Vec<_> = children
                    .iter()
                    .map(|child| child.markdown_corpus_navigation_node_id.clone())
                    .collect();
                if !state.navigation_candidate_sets.contains_key(&candidate_set_id) {
                    *state = self
                        .append_events(
                            principal,
                            prepared,
                            trace,
                            state.clone(),
                            &format!(
                                "navigation-candidates-{expansion_round}-{depth}-{parent_id}"
                            ),
                            vec![MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationChildCandidatesPresented {
                                markdown_corpus_navigation_candidate_set_id: candidate_set_id.clone(),
                                parent_markdown_corpus_navigation_node_id: parent_id.clone(),
                                child_markdown_corpus_navigation_node_ids: child_ids.clone(),
                            }],
                        )
                        .await?;
                }
                let selection_triggers = if expansion_round > 0 && parent_id == root {
                    triggering_evidence_ids.as_slice()
                } else {
                    &[]
                };
                let selections = self
                    .select_navigation_branches(
                        principal,
                        prepared,
                        trace,
                        reader,
                        state,
                        &candidate_set_id,
                        &parent_id,
                        children,
                        selection_triggers,
                    )
                    .await?;
                let depth_selected = selected_at_depth.entry(depth + 1).or_default();
                depth_selected.extend(
                    selections
                        .iter()
                        .filter(|selection| {
                            selection.markdown_corpus_navigation_node_selection_status
                                == crate::execution_trace::MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                        })
                        .map(|selection| selection.markdown_corpus_navigation_node_id.clone()),
                );
                if depth_selected.len()
                    > prepared
                        .markdown_research_execution_limits
                        .maximum_selected_markdown_corpus_navigation_branches_per_level
                        as usize
                {
                    return Err(RuntimeError::LimitExceeded {
                        message:
                            "maximum_selected_markdown_corpus_navigation_branches_per_level exhausted"
                                .to_owned(),
                    });
                }
                if selected_branch_count(state)
                    > u64::from(
                        prepared
                            .markdown_research_execution_limits
                            .maximum_active_document_research_branches,
                    )
                {
                    return Err(RuntimeError::LimitExceeded {
                        message: "maximum_active_document_research_branches exhausted".to_owned(),
                    });
                }
                for selection in selections {
                    if selection.markdown_corpus_navigation_node_selection_status
                        == crate::execution_trace::MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                    {
                        pending.push((
                            selection.markdown_corpus_navigation_node_id.clone(),
                            depth + 1,
                        ));
                        self.process_branch(
                            principal,
                            prepared,
                            snapshot,
                            trace,
                            reader,
                            validator,
                            state,
                            &selection.markdown_corpus_navigation_node_id,
                            &selection,
                        )
                        .await?;
                    }
                }
            }
            let expansion_count = scope_expansion_requests(state).len();
            if expansion_round >= expansion_count {
                break;
            }
            expansion_round += 1;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn select_navigation_branches(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        trace: &MarkdownResearchExecutionTrace,
        _reader: &crate::corpus::MarkdownCorpusSnapshotReader<'_>,
        state: &mut ReplayedMarkdownResearchExecution,
        candidate_set_id: &MarkdownCorpusNavigationCandidateSetId,
        parent_id: &MarkdownCorpusNavigationNodeId,
        children: Vec<&crate::corpus::MarkdownCorpusNavigationNode>,
        triggering_evidence_ids: &[VerbatimSourceEvidenceId],
    ) -> Result<Vec<MarkdownCorpusNavigationBranchSelection>> {
        if let Some(existing) = state.events.iter().find_map(|event| match &event.event_kind {
            MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchesSelected {
                markdown_corpus_navigation_candidate_set_id,
                markdown_corpus_navigation_branch_selections,
            } if markdown_corpus_navigation_candidate_set_id == candidate_set_id => {
                Some(markdown_corpus_navigation_branch_selections.clone())
            }
            _ => None,
        }) {
            return Ok(existing);
        }
        let task = StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchSelection(
            MarkdownCorpusNavigationBranchSelectionTask {
                markdown_research_model_task_id: model_task_id(
                    prepared,
                    &format!("navigation-selection-{candidate_set_id}"),
                ),
                markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
                frozen_document_research_brief: prepared.frozen_document_research_brief.clone(),
                markdown_corpus_snapshot_id: prepared.markdown_corpus_snapshot_id.clone(),
                markdown_corpus_navigation_candidate_set_id: candidate_set_id.clone(),
                parent_markdown_corpus_navigation_node_id: parent_id.clone(),
                markdown_corpus_navigation_node_candidates: children
                    .iter()
                    .map(|child| MarkdownCorpusNavigationNodeCandidate {
                        markdown_corpus_navigation_node_id: child
                            .markdown_corpus_navigation_node_id
                            .clone(),
                        markdown_corpus_navigation_node_label: child
                            .markdown_corpus_navigation_node_label
                            .clone(),
                        markdown_corpus_navigation_node_summary: child
                            .markdown_corpus_navigation_node_summary
                            .clone(),
                    })
                    .collect(),
                triggering_verbatim_source_evidence_ids: triggering_evidence_ids.to_vec(),
                markdown_research_model_task_schema_version:
                    MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
            },
        );
        self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
        let response = self.gateway.execute_strong_markdown_research_task(task).await?;
        let StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchSelection(response) =
            response
        else {
            return Err(RuntimeError::ModelResponse {
                message: "navigation task returned another response kind".to_owned(),
            });
        };
        let selections = response.markdown_corpus_navigation_branch_selections;
        *state = self
            .append_events(
                principal,
                prepared,
                trace,
                state.clone(),
                &format!("navigation-selection-{candidate_set_id}"),
                vec![
                    MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchesSelected {
                        markdown_corpus_navigation_candidate_set_id: candidate_set_id.clone(),
                        markdown_corpus_navigation_branch_selections: selections.clone(),
                    },
                ],
            )
            .await?;
        Ok(selections)
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_branch(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        snapshot: &MarkdownCorpusSnapshot,
        trace: &MarkdownResearchExecutionTrace,
        reader: &crate::corpus::MarkdownCorpusSnapshotReader<'_>,
        validator: &MarkdownSourceEvidenceIntegrityValidator<'_>,
        state: &mut ReplayedMarkdownResearchExecution,
        node_id: &MarkdownCorpusNavigationNodeId,
        selection: &MarkdownCorpusNavigationBranchSelection,
    ) -> Result<()> {
        let branch_task_id = branch_task_id(prepared, node_id);
        let branch_finished =
            state.failed_document_research_branch_task_ids.contains(&branch_task_id)
                || state.events.iter().any(|event| {
                    matches!(
                        &event.event_kind,
                        MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchClosed {
                            document_research_branch_task_id,
                            ..
                        } if document_research_branch_task_id == &branch_task_id
                    )
                });
        if branch_finished {
            return Ok(());
        }
        let report = if let Some(report) = state
            .branch_document_reports
            .iter()
            .find(|report| report.document_research_branch_task_id == branch_task_id)
        {
            report.clone()
        } else {
            let candidates = reader.list_branch_document_abstracts(node_id)?;
            let task = StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
                MarkdownCorpusNavigationBranchDocumentRelevanceReportTask {
                    markdown_research_model_task_id: model_task_id(
                        prepared,
                        &format!("branch-report-{branch_task_id}"),
                    ),
                    markdown_research_execution_id:
                        prepared.markdown_research_execution_id.clone(),
                    document_research_branch_task_id: branch_task_id.clone(),
                    markdown_corpus_navigation_node_id: node_id.clone(),
                    frozen_document_research_brief: prepared.frozen_document_research_brief.clone(),
                    markdown_source_document_candidates: candidates
                        .iter()
                        .map(|candidate| crate::model_gateway::MarkdownSourceDocumentAbstractModelCandidate {
                            markdown_source_document_id:
                                candidate.markdown_source_document_id.clone(),
                            markdown_source_document_title:
                                candidate.markdown_source_document_title.to_owned(),
                            markdown_source_document_abstract:
                                candidate.markdown_source_document_abstract.to_owned(),
                        })
                        .collect(),
                    markdown_research_model_task_schema_version:
                        MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
                },
            );
            self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
            let response = self.gateway.execute_strong_markdown_research_task(task).await?;
            let StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchDocumentRelevanceReport(report) = response
            else {
                return Err(RuntimeError::ModelResponse {
                    message: "branch report task returned another response kind".to_owned(),
                });
            };
            let mut selected_documents: BTreeSet<_> = state
                .branch_document_reports
                .iter()
                .flat_map(|existing| existing.selected_markdown_source_document_ids.iter().cloned())
                .collect();
            selected_documents.extend(report.selected_markdown_source_document_ids.iter().cloned());
            if selected_documents.len()
                > prepared
                    .markdown_research_execution_limits
                    .maximum_selected_markdown_source_documents as usize
            {
                return Err(RuntimeError::LimitExceeded {
                    message: "maximum_selected_markdown_source_documents exhausted".to_owned(),
                });
            }
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    &format!("branch-report-{branch_task_id}"),
                    vec![MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchDocumentReportCommitted {
                        markdown_corpus_navigation_branch_document_relevance_report: report.clone(),
                    }],
                )
                .await?;
            report
        };

        for document_id in &report.selected_markdown_source_document_ids {
            self.process_document(
                principal,
                prepared,
                snapshot,
                trace,
                reader,
                validator,
                state,
                &branch_task_id,
                document_id,
                selection,
                &report,
            )
            .await?;
        }

        let closed = state.events.iter().any(|event| {
            matches!(
                &event.event_kind,
                MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchClosed {
                    document_research_branch_task_id,
                    ..
                } if document_research_branch_task_id == &branch_task_id
            )
        });
        if !closed {
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    &format!("branch-close-{branch_task_id}"),
                    vec![MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchClosed {
                        document_research_branch_task_id: branch_task_id.clone(),
                        markdown_corpus_navigation_branch_close_reason:
                            MarkdownCorpusNavigationBranchCloseReason::AllRelevantMarkdownSourceSegmentsReviewed,
                    }],
                )
                .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_document(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        snapshot: &MarkdownCorpusSnapshot,
        trace: &MarkdownResearchExecutionTrace,
        reader: &crate::corpus::MarkdownCorpusSnapshotReader<'_>,
        validator: &MarkdownSourceEvidenceIntegrityValidator<'_>,
        state: &mut ReplayedMarkdownResearchExecution,
        branch_task_id: &DocumentResearchBranchTaskId,
        document_id: &MarkdownSourceDocumentId,
        _selection: &MarkdownCorpusNavigationBranchSelection,
        _report: &MarkdownCorpusNavigationBranchDocumentRelevanceReport,
    ) -> Result<()> {
        let document = snapshot
            .markdown_source_document_versions
            .iter()
            .find(|document| &document.markdown_source_document_id == document_id)
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus })?;
        let mut iteration = 0_u32;
        loop {
            if state.cancellation_requested || state.terminal_state.is_some() {
                return Ok(());
            }
            let already_read: BTreeSet<_> = state
                .events
                .iter()
                .filter_map(|event| match &event.event_kind {
                    MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead {
                        research_document_read_request_id,
                        ..
                    } => state
                        .research_document_read_requests
                        .get(research_document_read_request_id)
                        .filter(|request| {
                            &request.document_research_branch_task_id == branch_task_id
                                && &request.markdown_source_document_id == document_id
                        })
                        .map(|request| request.markdown_source_segment_id.clone()),
                    _ => None,
                })
                .collect();
            let segments: Vec<_> = document
                .markdown_source_segments
                .iter()
                .filter(|segment| !already_read.contains(&segment.markdown_source_segment_id))
                .collect();
            if segments.is_empty()
                || read_segment_count(state)
                    >= u64::from(
                        prepared
                            .markdown_research_execution_limits
                            .maximum_read_markdown_source_segments,
                    )
            {
                if segments.is_empty() {
                    return Ok(());
                }
                return Err(RuntimeError::LimitExceeded {
                    message: "maximum_read_markdown_source_segments exhausted".to_owned(),
                });
            }
            let read_request_id = read_request_id(prepared, branch_task_id, document_id, iteration);
            let read_request = if let Some(request) =
                state.research_document_read_requests.get(&read_request_id)
            {
                request.clone()
            } else {
                let task = StrongMarkdownResearchModelTask::ResearchDocumentReadRequest(
                    ResearchDocumentReadRequestTask {
                        markdown_research_model_task_id: model_task_id(
                            prepared,
                            &format!("read-request-{read_request_id}"),
                        ),
                        research_document_read_request_id: read_request_id.clone(),
                        markdown_research_execution_id: prepared
                            .markdown_research_execution_id
                            .clone(),
                        document_research_branch_task_id: branch_task_id.clone(),
                        frozen_document_research_brief: prepared
                            .frozen_document_research_brief
                            .clone(),
                        committed_branch_document_reports: state.branch_document_reports.clone(),
                        candidate_markdown_source_segments: segments
                            .iter()
                            .map(|segment| MarkdownSourceSegmentMetadata {
                                markdown_source_document_id: document_id.clone(),
                                markdown_source_document_version_id: document
                                    .markdown_source_document_version_id
                                    .clone(),
                                markdown_source_segment_id: segment
                                    .markdown_source_segment_id
                                    .clone(),
                                markdown_source_segment_section_heading: segment
                                    .markdown_source_segment_section_heading
                                    .clone(),
                                markdown_source_segment_start_byte_offset_in_document: segment
                                    .markdown_source_segment_start_byte_offset_in_document,
                                markdown_source_segment_end_byte_offset_in_document: segment
                                    .markdown_source_segment_end_byte_offset_in_document,
                                markdown_source_segment_hash: segment
                                    .markdown_source_segment_hash
                                    .clone(),
                            })
                            .collect(),
                        accepted_verbatim_source_evidence: accepted_evidence_contexts(state),
                        markdown_research_model_task_schema_version:
                            MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
                    },
                );
                self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
                let response = self.gateway.execute_strong_markdown_research_task(task).await?;
                let StrongMarkdownResearchModelResponse::ResearchDocumentReadRequest(request) =
                    response
                else {
                    return Err(RuntimeError::ModelResponse {
                        message: "read-request task returned another response kind".to_owned(),
                    });
                };
                *state = self
                    .append_events(
                        principal,
                        prepared,
                        trace,
                        state.clone(),
                        &format!("read-request-{read_request_id}"),
                        vec![MarkdownResearchExecutionEventKind::ResearchDocumentReadRequestCreated {
                            research_document_read_request: request.clone(),
                        }],
                    )
                    .await?;
                request
            };
            let authorized = reader.read_authorized_markdown_source_segment(
                &read_request.markdown_source_document_id,
                &read_request.markdown_source_segment_id,
            )?;
            let has_read = state.events.iter().any(|event| {
                matches!(
                    &event.event_kind,
                    MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead {
                        research_document_read_request_id,
                        ..
                    } if research_document_read_request_id == &read_request.research_document_read_request_id
                )
            });
            if !has_read {
                *state = self
                    .append_events(
                        principal,
                        prepared,
                        trace,
                        state.clone(),
                        &format!("segment-read-{}", read_request.research_document_read_request_id),
                        vec![MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead {
                            research_document_read_request_id: read_request
                                .research_document_read_request_id
                                .clone(),
                            markdown_source_segment_hash: authorized
                                .markdown_source_segment_hash
                                .to_owned(),
                        }],
                    )
                    .await?;
            }
            let review = self
                .review_segment(
                    principal,
                    prepared,
                    trace,
                    reader,
                    state,
                    &read_request,
                    &authorized,
                    &document.markdown_source_document_version_id,
                )
                .await?;
            match review.markdown_source_follow_up_action {
                MarkdownSourceFollowUpAction::ExtractVerbatimSourceEvidence => {
                    self.extract_evidence(
                        principal,
                        prepared,
                        snapshot,
                        trace,
                        reader,
                        validator,
                        state,
                        &read_request,
                        &authorized,
                        review
                            .verbatim_source_evidence_extraction_goal
                            .as_deref()
                            .unwrap_or("extract the exact source passage relevant to the question"),
                    )
                    .await?;
                    return Ok(());
                }
                MarkdownSourceFollowUpAction::ReadAdditionalMarkdownSourceSegment => {
                    iteration = iteration.saturating_add(1);
                }
                MarkdownSourceFollowUpAction::ExpandMarkdownCorpusNavigationScope => {
                    if !review.triggering_verbatim_source_evidence_ids.is_empty() {
                        *state = self
                            .append_events(
                                principal,
                                prepared,
                                trace,
                                state.clone(),
                                &format!("scope-expand-{branch_task_id}"),
                                vec![MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationScopeExpansionRequested {
                                    triggering_verbatim_source_evidence_ids:
                                        review.triggering_verbatim_source_evidence_ids.clone(),
                                }],
                            )
                            .await?;
                    }
                    return Ok(());
                }
                MarkdownSourceFollowUpAction::CloseMarkdownCorpusNavigationBranch => return Ok(()),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn review_segment(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        trace: &MarkdownResearchExecutionTrace,
        _reader: &crate::corpus::MarkdownCorpusSnapshotReader<'_>,
        state: &mut ReplayedMarkdownResearchExecution,
        read_request: &ResearchDocumentReadRequest,
        authorized: &crate::corpus::AuthorizedMarkdownSourceSegment<'_>,
        document_version_id: &crate::identity::MarkdownSourceDocumentVersionId,
    ) -> Result<MarkdownSourceReviewDecision> {
        if let Some(decision) = state.events.iter().find_map(|event| match &event.event_kind {
            MarkdownResearchExecutionEventKind::MarkdownSourceFollowUpDecided {
                research_document_read_request_id,
                markdown_source_follow_up_action,
                verbatim_source_evidence_extraction_goal,
                triggering_verbatim_source_evidence_ids,
                markdown_corpus_navigation_branch_close_reason,
                markdown_source_review_summary,
            } if research_document_read_request_id
                == &read_request.research_document_read_request_id =>
            {
                Some(MarkdownSourceReviewDecision {
                    research_document_read_request_id: read_request
                        .research_document_read_request_id
                        .clone(),
                    document_research_branch_task_id: read_request
                        .document_research_branch_task_id
                        .clone(),
                    markdown_source_document_id: read_request.markdown_source_document_id.clone(),
                    markdown_source_segment_id: read_request.markdown_source_segment_id.clone(),
                    markdown_source_follow_up_action: *markdown_source_follow_up_action,
                    verbatim_source_evidence_extraction_goal:
                        verbatim_source_evidence_extraction_goal.clone(),
                    triggering_verbatim_source_evidence_ids:
                        triggering_verbatim_source_evidence_ids.clone(),
                    markdown_corpus_navigation_branch_close_reason:
                        *markdown_corpus_navigation_branch_close_reason,
                    markdown_source_review_summary: markdown_source_review_summary
                        .clone()
                        .unwrap_or_else(|| "replayed source review decision".to_owned()),
                })
            }
            _ => None,
        }) {
            return Ok(decision);
        }
        let task =
            StrongMarkdownResearchModelTask::MarkdownSourceReview(MarkdownSourceReviewTask {
                markdown_research_model_task_id: model_task_id(
                    prepared,
                    &format!("source-review-{}", read_request.research_document_read_request_id),
                ),
                markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
                document_research_branch_task_id: read_request
                    .document_research_branch_task_id
                    .clone(),
                research_document_read_request_id: read_request
                    .research_document_read_request_id
                    .clone(),
                frozen_document_research_brief: prepared.frozen_document_research_brief.clone(),
                authorized_markdown_source_segment: AuthorizedMarkdownSourceSegmentInput {
                    markdown_source_document_id: authorized.markdown_source_document_id.clone(),
                    markdown_source_document_version_id: document_version_id.clone(),
                    markdown_source_segment_id: authorized.markdown_source_segment_id.clone(),
                    markdown_source_segment_hash: authorized
                        .markdown_source_segment_hash
                        .to_owned(),
                    markdown_source_segment_start_byte_offset_in_document: authorized
                        .markdown_source_segment_start_byte_offset_in_document,
                    canonical_markdown_source_segment_text: authorized
                        .canonical_markdown_source_segment_text
                        .to_owned(),
                },
                accepted_verbatim_source_evidence: accepted_evidence_contexts(state),
                markdown_research_model_task_schema_version:
                    MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
            });
        self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
        let response = self.gateway.execute_strong_markdown_research_task(task).await?;
        let StrongMarkdownResearchModelResponse::MarkdownSourceReview(decision) = response else {
            return Err(RuntimeError::ModelResponse {
                message: "source-review task returned another response kind".to_owned(),
            });
        };
        *state = self
            .append_events(
                principal,
                prepared,
                trace,
                state.clone(),
                &format!("source-review-{}", read_request.research_document_read_request_id),
                vec![MarkdownResearchExecutionEventKind::MarkdownSourceFollowUpDecided {
                    research_document_read_request_id: read_request
                        .research_document_read_request_id
                        .clone(),
                    markdown_source_follow_up_action: decision.markdown_source_follow_up_action,
                    verbatim_source_evidence_extraction_goal: decision
                        .verbatim_source_evidence_extraction_goal
                        .clone(),
                    triggering_verbatim_source_evidence_ids: decision
                        .triggering_verbatim_source_evidence_ids
                        .clone(),
                    markdown_corpus_navigation_branch_close_reason: decision
                        .markdown_corpus_navigation_branch_close_reason,
                    markdown_source_review_summary: Some(
                        decision.markdown_source_review_summary.clone(),
                    ),
                }],
            )
            .await?;
        Ok(decision)
    }

    #[allow(clippy::too_many_arguments)]
    async fn extract_evidence(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        snapshot: &MarkdownCorpusSnapshot,
        trace: &MarkdownResearchExecutionTrace,
        _reader: &crate::corpus::MarkdownCorpusSnapshotReader<'_>,
        validator: &MarkdownSourceEvidenceIntegrityValidator<'_>,
        state: &mut ReplayedMarkdownResearchExecution,
        read_request: &ResearchDocumentReadRequest,
        authorized: &crate::corpus::AuthorizedMarkdownSourceSegment<'_>,
        extraction_goal: &str,
    ) -> Result<()> {
        let extraction_id =
            extraction_request_id(prepared, &read_request.research_document_read_request_id);
        let task_id = model_task_id(prepared, &format!("evidence-extraction-{extraction_id}"));
        let version_id = snapshot
            .markdown_source_document_versions
            .iter()
            .find(|document| {
                document.markdown_source_document_id == read_request.markdown_source_document_id
            })
            .map(|document| document.markdown_source_document_version_id.clone())
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus })?;
        let extraction_task = VerbatimSourceEvidenceExtractionTask {
            markdown_research_model_task_id: task_id.clone(),
            verbatim_source_evidence_extraction_request_id: extraction_id.clone(),
            markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
            document_research_branch_task_id: read_request.document_research_branch_task_id.clone(),
            clarified_research_question: prepared
                .frozen_document_research_brief
                .clarified_research_question
                .clone(),
            verbatim_source_evidence_extraction_goal: extraction_goal.to_owned(),
            authorized_markdown_source_segment: AuthorizedMarkdownSourceSegmentInput {
                markdown_source_document_id: read_request.markdown_source_document_id.clone(),
                markdown_source_document_version_id: version_id.clone(),
                markdown_source_segment_id: read_request.markdown_source_segment_id.clone(),
                markdown_source_segment_hash: authorized.markdown_source_segment_hash.to_owned(),
                markdown_source_segment_start_byte_offset_in_document: authorized
                    .markdown_source_segment_start_byte_offset_in_document,
                canonical_markdown_source_segment_text: authorized
                    .canonical_markdown_source_segment_text
                    .to_owned(),
            },
            markdown_research_model_task_schema_version:
                MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
        };
        if !state.verbatim_source_evidence_extraction_request_ids.contains(&extraction_id) {
            let dispatch_key = format!("dispatch-{task_id}");
            let checkpoint = dispatch_checkpoint(
                &extraction_task,
                &task_id,
                crate::domain::MarkdownResearchModelTaskKind::VerbatimSourceEvidenceExtraction,
                Some(&read_request.document_research_branch_task_id),
                command_id(prepared, &dispatch_key),
                prepared.markdown_research_execution_limits.model_input_token_estimator_version,
            )?;
            ensure_dispatch_budget(prepared, state, &checkpoint, true)?;
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    &dispatch_key,
                    vec![MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceExtractionRequested {
                        verbatim_source_evidence_extraction_request_id: extraction_id.clone(),
                        research_document_read_request_id:
                            read_request.research_document_read_request_id.clone(),
                        markdown_research_model_dispatch_checkpoint: checkpoint,
                    }],
                )
                .await?;
        }
        let candidate_set = if let Some(existing) =
            state.verbatim_source_evidence_candidate_sets.get(&extraction_id)
        {
            existing.clone()
        } else {
            let response =
                self.gateway.extract_verbatim_source_evidence_candidates(extraction_task).await?;
            let envelope = VerbatimSourceEvidenceCandidateSet { ..response.clone() };
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    &format!("evidence-candidates-{extraction_id}"),
                    vec![MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceCandidatesPresented {
                        verbatim_source_evidence_extraction_request_id: extraction_id.clone(),
                        verbatim_source_evidence_candidate_set: envelope.clone(),
                    }],
                )
                .await?;
            envelope
        };
        if candidate_set.verbatim_source_evidence_candidates.is_empty() {
            if !has_extraction_outcome(state, &extraction_id) {
                *state = self
                    .append_events(
                        principal,
                        prepared,
                        trace,
                        state.clone(),
                        &format!("evidence-rejected-{extraction_id}"),
                        vec![MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceRejected {
                            verbatim_source_evidence_extraction_request_id: extraction_id,
                            rejection_explanation: "cheap model returned no exact source candidate"
                                .to_owned(),
                        }],
                    )
                    .await?;
            }
            return Ok(());
        }

        let accepted = validator.validate_persisted_evidence(
            &state.accepted_verbatim_source_evidence,
            &state.public_source_citations,
        )?;
        let persisted_set = PersistedVerbatimSourceEvidenceCandidateSet {
            owner_subject_id: principal.subject_id.clone(),
            markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
            markdown_corpus_snapshot_id: prepared.markdown_corpus_snapshot_id.clone(),
            markdown_corpus_snapshot_hash: snapshot.markdown_corpus_snapshot_hash.clone(),
            research_document_read_request_id: read_request
                .research_document_read_request_id
                .clone(),
            markdown_source_document_version_content_hash: snapshot
                .markdown_source_document_versions
                .iter()
                .find(|document| {
                    document.markdown_source_document_id == read_request.markdown_source_document_id
                })
                .map(|document| document.markdown_source_document_version_content_hash.clone())
                .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus })?,
            verbatim_source_evidence_candidate_set: candidate_set.clone(),
        };
        // Keep generated IDs alive while the validator borrows them. A
        // candidate set may contain several legal quotes; choose the first
        // valid candidate in canonical coordinate order so model ordering
        // cannot affect the persisted result. One extraction request still
        // has one terminal candidate outcome and therefore one evidence ID.
        let mut candidates = candidate_set.verbatim_source_evidence_candidates.clone();
        candidates.sort_by(|left, right| {
            (
                left.verbatim_source_evidence_start_byte_offset_in_segment,
                left.verbatim_source_evidence_end_byte_offset_in_segment,
                &left.verbatim_source_evidence_quote,
            )
                .cmp(&(
                    right.verbatim_source_evidence_start_byte_offset_in_segment,
                    right.verbatim_source_evidence_end_byte_offset_in_segment,
                    &right.verbatim_source_evidence_quote,
                ))
        });
        let evidence_id = verbatim_source_evidence_id(prepared, &extraction_id);
        let citation_id = public_source_citation_id(prepared, &extraction_id);
        let authorized_read = PersistedAuthorizedMarkdownSourceRead {
            owner_subject_id: &principal.subject_id,
            markdown_research_execution_id: &prepared.markdown_research_execution_id,
            markdown_corpus_snapshot_id: &prepared.markdown_corpus_snapshot_id,
            markdown_corpus_snapshot_hash: &snapshot.markdown_corpus_snapshot_hash,
            research_document_read_request: read_request,
            authorized_markdown_source_segment: *authorized,
            observed_markdown_source_segment_hash: authorized.markdown_source_segment_hash,
        };
        let ids = ProgramAssignedVerbatimSourceEvidenceIds {
            verbatim_source_evidence_id: &evidence_id,
            public_source_citation_id: &citation_id,
        };
        let mut first_rejection = None;
        for candidate in &candidates {
            match validator.validate_verbatim_source_evidence_candidate(
                ValidateVerbatimSourceEvidenceCandidateInput {
                    expected_document_research_branch_task_id: &read_request
                        .document_research_branch_task_id,
                    expected_verbatim_source_evidence_extraction_request_id: &extraction_id,
                    authorized_markdown_source_read: authorized_read,
                    persisted_verbatim_source_evidence_candidate_set: &persisted_set,
                    verbatim_source_evidence_candidate: candidate,
                    program_assigned_ids: ids,
                    previously_accepted_verbatim_source_evidence: &accepted,
                },
            ) {
                Ok(validated) => {
                    *state = self
                        .append_events(
                            principal,
                            prepared,
                            trace,
                            state.clone(),
                            &format!("evidence-accepted-{extraction_id}"),
                            vec![MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceAccepted {
                                verbatim_source_evidence_extraction_request_id: extraction_id.clone(),
                                verbatim_source_evidence: validated.verbatim_source_evidence,
                                public_source_citation: validated.public_source_citation,
                            }],
                        )
                        .await?;
                    return Ok(());
                }
                Err(error) => {
                    if first_rejection.is_none() {
                        first_rejection = Some(safe_model_rejection(&error));
                    }
                }
            }
        }
        if !has_extraction_outcome(state, &extraction_id) {
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    &format!("evidence-rejected-{extraction_id}"),
                    vec![MarkdownResearchExecutionEventKind::VerbatimSourceEvidenceRejected {
                        verbatim_source_evidence_extraction_request_id: extraction_id,
                        rejection_explanation: first_rejection.unwrap_or_else(|| {
                            "cheap model returned no valid source candidate".to_owned()
                        }),
                    }],
                )
                .await?;
        }
        Ok(())
    }

    async fn generate_claims_and_answers(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        trace: &MarkdownResearchExecutionTrace,
        validator: &MarkdownSourceEvidenceIntegrityValidator<'_>,
        state: &mut ReplayedMarkdownResearchExecution,
        accepted: Vec<crate::integrity::ValidatedVerbatimSourceEvidence>,
    ) -> Result<()> {
        if state.evidence_linked_research_claims.is_empty() {
            let authorized_ids =
                (0..16).map(|index| evidence_linked_research_claim_id(prepared, index)).collect();
            let task = StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimGeneration(
                EvidenceLinkedResearchClaimGenerationTask {
                    markdown_research_model_task_id: model_task_id(prepared, "claims"),
                    markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
                    frozen_document_research_brief: prepared.frozen_document_research_brief.clone(),
                    accepted_verbatim_source_evidence: accepted
                        .iter()
                        .map(|item| {
                            AcceptedVerbatimSourceEvidenceModelContext::from(
                                &item.verbatim_source_evidence,
                            )
                        })
                        .collect(),
                    research_coverage_gaps: state
                        .research_coverage_gaps
                        .values()
                        .cloned()
                        .collect(),
                    authorized_evidence_linked_research_claim_ids: authorized_ids,
                    markdown_research_model_task_schema_version:
                        MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
                },
            );
            self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
            let response = self.gateway.execute_strong_markdown_research_task(task).await?;
            let StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimGeneration(
                EvidenceLinkedResearchClaimGenerationResponse { evidence_linked_research_claims },
            ) = response
            else {
                return Err(RuntimeError::ModelResponse {
                    message: "claim task returned another response kind".to_owned(),
                });
            };
            validator.validate_evidence_linked_research_claims(
                &evidence_linked_research_claims,
                &accepted,
            )?;
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    "claims-committed",
                    vec![
                        MarkdownResearchExecutionEventKind::EvidenceLinkedResearchClaimsCommitted {
                            evidence_linked_research_claims,
                        },
                    ],
                )
                .await?;
        }

        if state.evidence_linked_research_claims_answer.is_none() {
            let task =
                StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimsAnswerGeneration(
                    EvidenceLinkedResearchClaimsAnswerGenerationTask {
                        markdown_research_model_task_id: model_task_id(prepared, "claims-answer"),
                        markdown_research_execution_id: prepared
                            .markdown_research_execution_id
                            .clone(),
                        frozen_document_research_brief: prepared
                            .frozen_document_research_brief
                            .clone(),
                        committed_evidence_linked_research_claims: state
                            .evidence_linked_research_claims
                            .clone(),
                        markdown_research_model_task_schema_version:
                            MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
                    },
                );
            self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
            let response = self.gateway.execute_strong_markdown_research_task(task).await?;
            let StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimsAnswerGeneration(
                answer,
            ) = response
            else {
                return Err(RuntimeError::ModelResponse {
                    message: "claims-answer task returned another response kind".to_owned(),
                });
            };
            validator.validate_evidence_linked_research_claims_answer(
                &answer,
                &state.evidence_linked_research_claims,
            )?;
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    "claims-answer",
                    vec![MarkdownResearchExecutionEventKind::EvidenceLinkedResearchClaimsAnswerGenerated {
                        evidence_linked_research_claims_answer: answer,
                    }],
                )
                .await?;
        }

        let model_answer = state.model_knowledge_only_answer.clone().ok_or_else(|| {
            RuntimeError::CorruptState {
                stage: RuntimeStage::Trace,
                message: "model-only answer disappeared before composition".to_owned(),
            }
        })?;
        let claims_answer =
            state.evidence_linked_research_claims_answer.clone().ok_or_else(|| {
                RuntimeError::CorruptState {
                    stage: RuntimeStage::Trace,
                    message: "claims answer disappeared before composition".to_owned(),
                }
            })?;
        for style in &prepared.requested_answer_composition_styles {
            if state.source_attributed_answer_compositions.contains_key(style) {
                continue;
            }
            let task = StrongMarkdownResearchModelTask::SourceAttributedAnswerComposition(
                SourceAttributedAnswerCompositionTask {
                    markdown_research_model_task_id: model_task_id(
                        prepared,
                        &format!("composition-{}", style.as_str()),
                    ),
                    markdown_research_execution_id: prepared.markdown_research_execution_id.clone(),
                    frozen_document_research_brief: prepared.frozen_document_research_brief.clone(),
                    requested_answer_composition_style: *style,
                    model_knowledge_only_answer: model_answer.clone(),
                    committed_evidence_linked_research_claims: state
                        .evidence_linked_research_claims
                        .clone(),
                    evidence_linked_research_claims_answer: claims_answer.clone(),
                    public_source_citations: state.public_source_citations.clone(),
                    markdown_research_model_task_schema_version:
                        MARKDOWN_RESEARCH_MODEL_TASK_SCHEMA_VERSION,
                },
            );
            self.ensure_strong_dispatch(principal, prepared, trace, state, &task).await?;
            let response = self.gateway.execute_strong_markdown_research_task(task).await?;
            let StrongMarkdownResearchModelResponse::SourceAttributedAnswerComposition(composition) =
                response
            else {
                return Err(RuntimeError::ModelResponse {
                    message: "composition task returned another response kind".to_owned(),
                });
            };
            validator.validate_source_attributed_answer_composition(
                ValidateSourceAttributedAnswerCompositionInput {
                    model_knowledge_only_answer: &model_answer,
                    evidence_linked_research_claims_answer: &claims_answer,
                    committed_evidence_linked_research_claims: &state
                        .evidence_linked_research_claims,
                    accepted_verbatim_source_evidence: &accepted,
                    source_attributed_answer_composition: &composition,
                },
            )?;
            *state = self
                .append_events(
                    principal,
                    prepared,
                    trace,
                    state.clone(),
                    &format!("composition-{}", style.as_str()),
                    vec![MarkdownResearchExecutionEventKind::SourceAttributedAnswerComposed {
                        source_attributed_answer_composition: composition,
                    }],
                )
                .await?;
        }
        Ok(())
    }

    async fn ensure_strong_dispatch(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        trace: &MarkdownResearchExecutionTrace,
        state: &mut ReplayedMarkdownResearchExecution,
        task: &StrongMarkdownResearchModelTask,
    ) -> Result<()> {
        if state
            .dispatched_markdown_research_model_task_ids
            .contains(task.markdown_research_model_task_id())
        {
            return Ok(());
        }
        let checkpoint = dispatch_checkpoint(
            task,
            task.markdown_research_model_task_id(),
            task.kind(),
            task.document_research_branch_task_id(),
            command_id(prepared, &format!("dispatch-{}", task.markdown_research_model_task_id())),
            prepared.markdown_research_execution_limits.model_input_token_estimator_version,
        )?;
        ensure_dispatch_budget(prepared, state, &checkpoint, false)?;
        *state = self
            .append_events(
                principal,
                prepared,
                trace,
                state.clone(),
                &format!("dispatch-{}", task.markdown_research_model_task_id()),
                vec![MarkdownResearchExecutionEventKind::StrongMarkdownResearchModelRequestDispatched {
                    markdown_research_model_dispatch_checkpoint: checkpoint,
                }],
            )
            .await?;
        Ok(())
    }

    async fn append_events(
        &self,
        principal: &ResearchPrincipal,
        prepared: &PreparedMarkdownResearchExecution,
        trace: &MarkdownResearchExecutionTrace,
        state: ReplayedMarkdownResearchExecution,
        key: &str,
        event_kinds: Vec<MarkdownResearchExecutionEventKind>,
    ) -> Result<ReplayedMarkdownResearchExecution> {
        if event_kinds.is_empty() {
            return Ok(state);
        }
        let latest = match trace
            .replay_markdown_research_execution(principal, &prepared.markdown_research_execution_id)
            .await
        {
            Ok(latest) => latest,
            Err(RuntimeError::ObjectNotAvailable { .. }) => state.clone(),
            Err(error) => return Err(error),
        };
        if latest.cancellation_requested
            && !event_kinds.iter().any(|kind| {
                matches!(
                    kind,
                    MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancellationRequested { .. }
                        | MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancelled { .. }
                )
            })
        {
            return Ok(latest);
        }
        if event_kinds.iter().any(|kind| {
            !matches!(
                kind,
                MarkdownResearchExecutionEventKind::MarkdownResearchExecutionFailed { .. }
                    | MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancelled { .. }
                    | MarkdownResearchExecutionEventKind::MarkdownResearchExecutionCancellationRequested { .. }
                    | MarkdownResearchExecutionEventKind::ResearchCoverageGapUpdated { .. }
            )
        }) {
            ensure_execution_duration(prepared, &latest)?;
        }
        trace
            .append_markdown_research_execution_events(
                principal,
                &prepared.markdown_research_execution_id,
                command_id(prepared, key),
                next_recorded_at(&latest),
                event_kinds,
            )
            .await
    }
}

fn command_id(prepared: &PreparedMarkdownResearchExecution, key: &str) -> CommandId {
    deterministic_id("engine-command", &prepared.markdown_research_execution_id, key, |value| {
        CommandId::from_value(value)
    })
}

fn model_task_id(
    prepared: &PreparedMarkdownResearchExecution,
    key: &str,
) -> MarkdownResearchModelTaskId {
    deterministic_id("engine-task", &prepared.markdown_research_execution_id, key, |value| {
        MarkdownResearchModelTaskId::from_value(value)
    })
}

fn candidate_set_id(
    prepared: &PreparedMarkdownResearchExecution,
    parent: &MarkdownCorpusNavigationNodeId,
    depth: u32,
    expansion_round: usize,
) -> MarkdownCorpusNavigationCandidateSetId {
    deterministic_id(
        "engine-candidates",
        &prepared.markdown_research_execution_id,
        &format!("{parent}-{depth}-{expansion_round}"),
        MarkdownCorpusNavigationCandidateSetId::from_value,
    )
}

fn branch_task_id(
    prepared: &PreparedMarkdownResearchExecution,
    node: &MarkdownCorpusNavigationNodeId,
) -> DocumentResearchBranchTaskId {
    deterministic_id(
        "engine-branch",
        &prepared.markdown_research_execution_id,
        node.as_str(),
        DocumentResearchBranchTaskId::from_value,
    )
}

fn read_request_id(
    prepared: &PreparedMarkdownResearchExecution,
    branch: &DocumentResearchBranchTaskId,
    document: &MarkdownSourceDocumentId,
    iteration: u32,
) -> ResearchDocumentReadRequestId {
    deterministic_id(
        "engine-read",
        &prepared.markdown_research_execution_id,
        &format!("{branch}-{document}-{iteration}"),
        ResearchDocumentReadRequestId::from_value,
    )
}

fn extraction_request_id(
    prepared: &PreparedMarkdownResearchExecution,
    read_request: &ResearchDocumentReadRequestId,
) -> VerbatimSourceEvidenceExtractionRequestId {
    deterministic_id(
        "engine-extraction",
        &prepared.markdown_research_execution_id,
        read_request.as_str(),
        VerbatimSourceEvidenceExtractionRequestId::from_value,
    )
}

fn verbatim_source_evidence_id(
    prepared: &PreparedMarkdownResearchExecution,
    extraction: &VerbatimSourceEvidenceExtractionRequestId,
) -> VerbatimSourceEvidenceId {
    deterministic_id(
        "engine-evidence",
        &prepared.markdown_research_execution_id,
        extraction.as_str(),
        VerbatimSourceEvidenceId::from_value,
    )
}

fn public_source_citation_id(
    prepared: &PreparedMarkdownResearchExecution,
    extraction: &VerbatimSourceEvidenceExtractionRequestId,
) -> PublicSourceCitationId {
    deterministic_id(
        "engine-citation",
        &prepared.markdown_research_execution_id,
        extraction.as_str(),
        PublicSourceCitationId::from_value,
    )
}

fn evidence_linked_research_claim_id(
    prepared: &PreparedMarkdownResearchExecution,
    index: usize,
) -> EvidenceLinkedResearchClaimId {
    deterministic_id(
        "engine-claim",
        &prepared.markdown_research_execution_id,
        &index.to_string(),
        EvidenceLinkedResearchClaimId::from_value,
    )
}

fn research_coverage_gap_id(
    prepared: &PreparedMarkdownResearchExecution,
    key: &str,
) -> ResearchCoverageGapId {
    deterministic_id(
        "research-coverage-gap",
        &prepared.markdown_research_execution_id,
        key,
        ResearchCoverageGapId::from_value,
    )
}

fn deterministic_id<T>(
    prefix: &str,
    execution_id: &MarkdownResearchExecutionId,
    key: &str,
    parse: impl FnOnce(String) -> Result<T>,
) -> T {
    let digest = crate::domain::sha256_content_hash(
        format!("{prefix}|{}|{key}", execution_id.as_str()).as_bytes(),
    );
    let value = format!("{prefix}-{}", &digest[7..]);
    parse(value).expect("deterministic engine IDs satisfy the opaque ID grammar")
}

fn next_recorded_at(state: &ReplayedMarkdownResearchExecution) -> DateTime<Utc> {
    let now = Utc::now();
    state
        .events
        .last()
        .map(|event| {
            let minimum =
                event.markdown_research_execution_event_recorded_at + Duration::nanoseconds(1);
            now.max(minimum)
        })
        .unwrap_or(now)
}

fn dispatch_checkpoint<T: serde::Serialize>(
    task: &T,
    task_id: &MarkdownResearchModelTaskId,
    task_kind: crate::domain::MarkdownResearchModelTaskKind,
    branch_task_id: Option<&DocumentResearchBranchTaskId>,
    command_id: CommandId,
    model_input_token_estimator_version: u32,
) -> Result<MarkdownResearchModelDispatchCheckpoint> {
    let serialized = crate::domain::canonical_json_bytes(task)?;
    let estimated_input_tokens = u64::try_from((serialized.len().saturating_add(3)) / 4)
        .map_err(|_| RuntimeError::LimitExceeded {
            message: "model task input token estimate overflow".to_owned(),
        })?
        .max(1);
    Ok(MarkdownResearchModelDispatchCheckpoint {
        markdown_research_model_task_id: task_id.clone(),
        markdown_research_model_task_kind: task_kind,
        document_research_branch_task_id: branch_task_id.cloned(),
        markdown_research_model_task_input_hash: canonical_content_hash(task)?,
        model_input_token_estimator_version,
        estimated_input_tokens,
        markdown_research_execution_command_id: command_id,
    })
}

fn ensure_dispatch_budget(
    prepared: &PreparedMarkdownResearchExecution,
    state: &ReplayedMarkdownResearchExecution,
    checkpoint: &MarkdownResearchModelDispatchCheckpoint,
    extraction: bool,
) -> Result<()> {
    ensure_execution_duration(prepared, state)?;
    let limits = &prepared.markdown_research_execution_limits;
    let request_count = if extraction {
        state.verbatim_source_evidence_extraction_model_request_count
    } else {
        state.strong_markdown_research_model_request_count
    };
    let request_limit = if extraction {
        u64::from(limits.maximum_verbatim_source_evidence_extraction_model_requests)
    } else {
        u64::from(limits.maximum_strong_markdown_research_model_requests)
    };
    if request_count >= request_limit {
        return Err(RuntimeError::LimitExceeded {
            message: if extraction {
                "maximum_verbatim_source_evidence_extraction_model_requests exhausted".to_owned()
            } else {
                "maximum_strong_markdown_research_model_requests exhausted".to_owned()
            },
        });
    }
    let next_total = state
        .total_model_input_token_estimate
        .checked_add(checkpoint.estimated_input_tokens)
        .ok_or_else(|| RuntimeError::LimitExceeded {
            message: "maximum_total_model_input_token_estimate overflow".to_owned(),
        })?;
    if next_total > limits.maximum_total_model_input_token_estimate {
        return Err(RuntimeError::LimitExceeded {
            message: "maximum_total_model_input_token_estimate exhausted".to_owned(),
        });
    }
    Ok(())
}

fn ensure_execution_duration(
    prepared: &PreparedMarkdownResearchExecution,
    state: &ReplayedMarkdownResearchExecution,
) -> Result<()> {
    let Some(started_at) = state.markdown_research_execution_running_at else {
        return Ok(());
    };
    let limit = i64::try_from(
        prepared
            .markdown_research_execution_limits
            .maximum_markdown_research_execution_duration_seconds,
    )
    .map_err(|_| RuntimeError::LimitExceeded {
        message: "maximum_markdown_research_execution_duration_seconds is invalid".to_owned(),
    })?;
    if Utc::now() > started_at + Duration::seconds(limit) {
        return Err(RuntimeError::LimitExceeded {
            message: "maximum_markdown_research_execution_duration_seconds exhausted".to_owned(),
        });
    }
    Ok(())
}

fn accepted_evidence_contexts(
    state: &ReplayedMarkdownResearchExecution,
) -> Vec<AcceptedVerbatimSourceEvidenceModelContext> {
    state
        .accepted_verbatim_source_evidence
        .iter()
        .map(AcceptedVerbatimSourceEvidenceModelContext::from)
        .collect()
}

fn read_segment_count(state: &ReplayedMarkdownResearchExecution) -> u64 {
    state
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.event_kind,
                MarkdownResearchExecutionEventKind::MarkdownSourceSegmentRead { .. }
            )
        })
        .count() as u64
}

fn selected_branch_count(state: &ReplayedMarkdownResearchExecution) -> u64 {
    state
        .navigation_branch_selections
        .iter()
        .filter(|selection| {
            selection.markdown_corpus_navigation_node_selection_status
                == crate::execution_trace::MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
        })
        .map(|selection| selection.markdown_corpus_navigation_node_id.as_str())
        .collect::<BTreeSet<_>>()
        .len() as u64
}

fn scope_expansion_requests(
    state: &ReplayedMarkdownResearchExecution,
) -> Vec<Vec<VerbatimSourceEvidenceId>> {
    state
        .events
        .iter()
        .filter_map(|event| {
            match &event.event_kind {
            MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationScopeExpansionRequested {
                triggering_verbatim_source_evidence_ids,
            } => Some(triggering_verbatim_source_evidence_ids.clone()),
            _ => None,
        }
        })
        .collect()
}

fn has_extraction_outcome(
    state: &ReplayedMarkdownResearchExecution,
    extraction_id: &VerbatimSourceEvidenceExtractionRequestId,
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
            } if verbatim_source_evidence_extraction_request_id == extraction_id
        )
    })
}

fn safe_model_rejection(error: &RuntimeError) -> String {
    match error {
        RuntimeError::Validation { .. }
        | RuntimeError::ModelResponse { .. }
        | RuntimeError::Conflict { .. } => {
            "model evidence candidate rejected by deterministic validation".to_owned()
        }
        _ => "model evidence candidate could not be accepted".to_owned(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::corpus::{
        MarkdownCorpusNavigationNodeInput, MarkdownSourceDocumentInput,
        PublishMarkdownCorpusSnapshotInput, build_markdown_corpus_snapshot,
    };
    use crate::domain::{
        ANSWER_PROJECTION_SCHEMA_VERSION, AnswerCompositionStyle, EvidenceLinkedResearchClaim,
        EvidenceLinkedResearchClaimCitationStatus, EvidenceLinkedResearchClaimsAnswer,
        FrozenDocumentResearchBrief, ModelKnowledgeOnlyAnswer, ResearchClaimEvidenceRelationship,
        ResearchClaimEvidenceRelationshipType, SourceAttributedAnswerComposition,
        SourceAttributedAnswerSegment, SourceAttributedAnswerSegmentSourceType,
    };
    use crate::execution_trace::{
        MarkdownCorpusNavigationNodeSelectionStatus, MarkdownResearchExecutionTerminalState,
        MarkdownResearchExecutionTrace,
    };
    use crate::identity::{
        DocumentResearchConversationId, DocumentResearchRequestId, MarkdownCorpusNavigationNodeId,
        MarkdownSourceDocumentId, PrincipalCapability, SubjectId,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    pub(crate) struct DeterministicGateway;

    #[async_trait]
    impl MarkdownResearchModelGateway for DeterministicGateway {
        async fn execute_strong_markdown_research_task(
            &self,
            task: StrongMarkdownResearchModelTask,
        ) -> Result<StrongMarkdownResearchModelResponse> {
            match task {
                StrongMarkdownResearchModelTask::ModelKnowledgeOnlyAnswerGeneration(task) => {
                    Ok(StrongMarkdownResearchModelResponse::ModelKnowledgeOnlyAnswerGeneration(
                        ModelKnowledgeOnlyAnswer {
                            model_knowledge_only_answer_id: task.markdown_research_model_task_id,
                            model_knowledge_only_answer_text: "模型背景说明".to_owned(),
                            markdown_research_execution_id: task.markdown_research_execution_id,
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchSelection(task) => {
                    Ok(StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchSelection(
                        crate::model_gateway::MarkdownCorpusNavigationBranchSelectionResponse {
                            markdown_corpus_navigation_candidate_set_id:
                                task.markdown_corpus_navigation_candidate_set_id,
                            markdown_corpus_navigation_branch_selections: task
                                .markdown_corpus_navigation_node_candidates
                                .into_iter()
                                .enumerate()
                                .map(|(index, candidate)| MarkdownCorpusNavigationBranchSelection {
                                    markdown_corpus_navigation_node_id:
                                        candidate.markdown_corpus_navigation_node_id,
                                    markdown_corpus_navigation_node_selection_status:
                                        MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch,
                                    markdown_corpus_navigation_node_relevance_explanation:
                                        "fixture relevance".to_owned(),
                                    expected_research_information_to_resolve_question:
                                        "fixture source information".to_owned(),
                                    markdown_corpus_navigation_branch_priority:
                                        (index + 1) as u32,
                                })
                                .collect(),
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchDocumentRelevanceReport(task) => {
                    let ids = task
                        .markdown_source_document_candidates
                        .iter()
                        .map(|candidate| candidate.markdown_source_document_id.clone())
                        .collect::<Vec<_>>();
                    Ok(StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
                        MarkdownCorpusNavigationBranchDocumentRelevanceReport {
                            document_research_branch_task_id: task.document_research_branch_task_id,
                            markdown_corpus_navigation_node_id: task.markdown_corpus_navigation_node_id,
                            candidate_markdown_source_document_ids: ids.clone(),
                            selected_markdown_source_document_ids: ids,
                            markdown_corpus_navigation_branch_document_report_summary:
                                "fixture report".to_owned(),
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::ResearchDocumentReadRequest(task) => {
                    let segment = task
                        .candidate_markdown_source_segments
                        .first()
                        .ok_or_else(|| RuntimeError::ModelResponse {
                            message: "fixture has no segment".to_owned(),
                        })?;
                    Ok(StrongMarkdownResearchModelResponse::ResearchDocumentReadRequest(
                        ResearchDocumentReadRequest {
                            research_document_read_request_id:
                                task.research_document_read_request_id,
                            document_research_branch_task_id:
                                task.document_research_branch_task_id,
                            markdown_source_document_id:
                                segment.markdown_source_document_id.clone(),
                            markdown_source_segment_id: segment.markdown_source_segment_id.clone(),
                            unresolved_research_question: "fixture unresolved question".to_owned(),
                            expected_research_information_to_resolve_question:
                                "fixture expected information".to_owned(),
                            markdown_source_document_selection_explanation:
                                "fixture selected document".to_owned(),
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::MarkdownSourceReview(task) => {
                    let segment = task.authorized_markdown_source_segment;
                    Ok(StrongMarkdownResearchModelResponse::MarkdownSourceReview(
                        MarkdownSourceReviewDecision {
                            research_document_read_request_id:
                                task.research_document_read_request_id,
                            document_research_branch_task_id:
                                task.document_research_branch_task_id,
                            markdown_source_document_id: segment.markdown_source_document_id,
                            markdown_source_segment_id: segment.markdown_source_segment_id,
                            markdown_source_follow_up_action:
                                MarkdownSourceFollowUpAction::ExtractVerbatimSourceEvidence,
                            verbatim_source_evidence_extraction_goal:
                                Some("extract fixture evidence".to_owned()),
                            triggering_verbatim_source_evidence_ids: Vec::new(),
                            markdown_corpus_navigation_branch_close_reason: None,
                            markdown_source_review_summary: "fixture review".to_owned(),
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimGeneration(task) => {
                    let evidence = task
                        .accepted_verbatim_source_evidence
                        .first()
                        .ok_or_else(|| RuntimeError::ModelResponse {
                            message: "fixture has no evidence".to_owned(),
                        })?;
                    let claim_id = task
                        .authorized_evidence_linked_research_claim_ids
                        .first()
                        .cloned()
                        .ok_or_else(|| RuntimeError::ModelResponse {
                            message: "fixture has no claim ID".to_owned(),
                        })?;
                    Ok(StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimGeneration(
                        EvidenceLinkedResearchClaimGenerationResponse {
                            evidence_linked_research_claims: vec![EvidenceLinkedResearchClaim {
                                evidence_linked_research_claim_id: claim_id,
                                evidence_linked_research_claim_text:
                                    "fixture claim from source".to_owned(),
                                research_claim_evidence_relationships: vec![
                                    ResearchClaimEvidenceRelationship {
                                        verbatim_source_evidence_id:
                                            evidence.verbatim_source_evidence_id.clone(),
                                        research_claim_evidence_relationship_type:
                                            ResearchClaimEvidenceRelationshipType::SupportsEvidenceLinkedResearchClaim,
                                    },
                                ],
                                evidence_linked_research_claim_applicability_conditions:
                                    Vec::new(),
                                evidence_linked_research_claim_exceptions: Vec::new(),
                                evidence_linked_research_claim_citation_status:
                                    EvidenceLinkedResearchClaimCitationStatus::AllCitationsLinkedToVerbatimSourceEvidence,
                                markdown_research_execution_id:
                                    task.markdown_research_execution_id,
                            }],
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimsAnswerGeneration(task) => {
                    let claim = task
                        .committed_evidence_linked_research_claims
                        .first()
                        .ok_or_else(|| RuntimeError::ModelResponse {
                            message: "fixture has no claim".to_owned(),
                        })?;
                    Ok(StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimsAnswerGeneration(
                        EvidenceLinkedResearchClaimsAnswer {
                            evidence_linked_research_claims_answer_id:
                                task.markdown_research_model_task_id,
                            evidence_linked_research_claims_answer_text:
                                "fixture claims answer".to_owned(),
                            supporting_evidence_linked_research_claim_ids:
                                vec![claim.evidence_linked_research_claim_id.clone()],
                            markdown_research_execution_id: task.markdown_research_execution_id,
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::SourceAttributedAnswerComposition(task) => {
                    let claim = task
                        .committed_evidence_linked_research_claims
                        .first()
                        .ok_or_else(|| RuntimeError::ModelResponse {
                            message: "fixture has no claim".to_owned(),
                        })?;
                    let citation = task
                        .public_source_citations
                        .first()
                        .ok_or_else(|| RuntimeError::ModelResponse {
                            message: "fixture has no citation".to_owned(),
                        })?;
                    Ok(StrongMarkdownResearchModelResponse::SourceAttributedAnswerComposition(
                        SourceAttributedAnswerComposition {
                            source_attributed_answer_composition_style:
                                task.requested_answer_composition_style,
                            model_knowledge_only_answer_id:
                                task.model_knowledge_only_answer.model_knowledge_only_answer_id,
                            evidence_linked_research_claims_answer_id: task
                                .evidence_linked_research_claims_answer
                                .evidence_linked_research_claims_answer_id,
                            source_attributed_answer_segments: vec![
                                SourceAttributedAnswerSegment {
                                    source_attributed_answer_segment_text:
                                        "fixture cited answer".to_owned(),
                                    source_attributed_answer_segment_source_type:
                                        SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaims,
                                    supporting_evidence_linked_research_claim_ids: vec![
                                        claim.evidence_linked_research_claim_id.clone(),
                                    ],
                                    supporting_public_source_citation_ids: vec![
                                        citation.public_source_citation_id.clone(),
                                    ],
                                    model_knowledge_unverified_notice: None,
                                },
                                SourceAttributedAnswerSegment {
                                    source_attributed_answer_segment_text:
                                        "fixture model context".to_owned(),
                                    source_attributed_answer_segment_source_type:
                                        SourceAttributedAnswerSegmentSourceType::ModelKnowledgeOnly,
                                    supporting_evidence_linked_research_claim_ids: Vec::new(),
                                    supporting_public_source_citation_ids: Vec::new(),
                                    model_knowledge_unverified_notice: Some(
                                        crate::integrity::MODEL_KNOWLEDGE_UNVERIFIED_NOTICE
                                            .to_owned(),
                                    ),
                                },
                            ],
                            source_attributed_answer_composition_review_reason:
                                "fixture composition".to_owned(),
                            answer_projection_schema_version: ANSWER_PROJECTION_SCHEMA_VERSION,
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(task) => {
                    let draft = task.document_research_brief_draft.unwrap_or_else(|| {
                        crate::clarification::DocumentResearchBriefDraft {
                            original_user_question: task.original_user_question.clone(),
                            clarified_research_question: task.original_user_question.clone(),
                            known_document_research_context: Vec::new(),
                            document_research_assumptions: Vec::new(),
                            unresolved_research_question_ambiguities: Vec::new(),
                            requested_research_answer_requirements: Vec::new(),
                        }
                    });
                    Ok(StrongMarkdownResearchModelResponse::ResearchQuestionEvaluation(
                        crate::clarification::ResearchQuestionClarificationModelOutput {
                            research_question_clarification_revision:
                                task.research_question_clarification_revision,
                            research_question_clarification_decision:
                                crate::clarification::ResearchQuestionClarificationDecision::StartMarkdownResearchExecution,
                            research_question_clarification_message: None,
                            document_research_brief_draft: draft,
                        },
                    ))
                }
            }
        }

        async fn extract_verbatim_source_evidence_candidates(
            &self,
            task: VerbatimSourceEvidenceExtractionTask,
        ) -> Result<VerbatimSourceEvidenceCandidateSet> {
            let quote = "原文证据";
            let start = task
                .authorized_markdown_source_segment
                .canonical_markdown_source_segment_text
                .find(quote)
                .ok_or_else(|| RuntimeError::ModelResponse {
                    message: "fixture quote not found".to_owned(),
                })?;
            Ok(VerbatimSourceEvidenceCandidateSet {
                verbatim_source_evidence_extraction_request_id: task
                    .verbatim_source_evidence_extraction_request_id,
                document_research_branch_task_id: task.document_research_branch_task_id,
                markdown_source_document_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_document_id,
                markdown_source_document_version_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_document_version_id,
                markdown_source_segment_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_segment_id,
                markdown_source_segment_hash: task
                    .authorized_markdown_source_segment
                    .markdown_source_segment_hash,
                verbatim_source_evidence_candidates: vec![
                    crate::model_gateway::VerbatimSourceEvidenceCandidate {
                        verbatim_source_evidence_start_byte_offset_in_segment: start as u64,
                        verbatim_source_evidence_end_byte_offset_in_segment: (start + quote.len())
                            as u64,
                        verbatim_source_evidence_quote: quote.to_owned(),
                    },
                ],
            })
        }
    }

    #[derive(Default)]
    struct LoopGateway {
        additional_segment_decisions: AtomicUsize,
        expansion_decisions: AtomicUsize,
        expansion_navigation_rounds: AtomicUsize,
        extraction_requests: AtomicUsize,
        repeat_only_prior_branch_on_expansion: bool,
    }

    #[async_trait]
    impl MarkdownResearchModelGateway for LoopGateway {
        async fn execute_strong_markdown_research_task(
            &self,
            task: StrongMarkdownResearchModelTask,
        ) -> Result<StrongMarkdownResearchModelResponse> {
            match task {
                StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchSelection(task) => {
                    let expansion = !task.triggering_verbatim_source_evidence_ids.is_empty();
                    if expansion {
                        self.expansion_navigation_rounds.fetch_add(1, Ordering::SeqCst);
                    }
                    let selections = task
                        .markdown_corpus_navigation_node_candidates
                        .into_iter()
                        .enumerate()
                        .map(|(index, candidate)| {
                            let selected = if expansion {
                                if self.repeat_only_prior_branch_on_expansion {
                                    candidate.markdown_corpus_navigation_node_id.as_str() == "alpha"
                                } else {
                                    candidate.markdown_corpus_navigation_node_id.as_str() == "beta"
                                }
                            } else {
                                candidate.markdown_corpus_navigation_node_id.as_str() == "alpha"
                            };
                            MarkdownCorpusNavigationBranchSelection {
                                markdown_corpus_navigation_node_id:
                                    candidate.markdown_corpus_navigation_node_id,
                                markdown_corpus_navigation_node_selection_status: if selected {
                                    MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch
                                } else if expansion {
                                    MarkdownCorpusNavigationNodeSelectionStatus::ExcludedFromCurrentMarkdownResearchScope
                                } else {
                                    MarkdownCorpusNavigationNodeSelectionStatus::DeferredForLaterMarkdownResearch
                                },
                                markdown_corpus_navigation_node_relevance_explanation:
                                    "loop fixture navigation decision".to_owned(),
                                expected_research_information_to_resolve_question:
                                    "loop fixture evidence".to_owned(),
                                markdown_corpus_navigation_branch_priority: (index + 1) as u32,
                            }
                        })
                        .collect();
                    Ok(StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchSelection(
                        crate::model_gateway::MarkdownCorpusNavigationBranchSelectionResponse {
                            markdown_corpus_navigation_candidate_set_id:
                                task.markdown_corpus_navigation_candidate_set_id,
                            markdown_corpus_navigation_branch_selections: selections,
                        },
                    ))
                }
                StrongMarkdownResearchModelTask::MarkdownSourceReview(task) => {
                    let segment = task.authorized_markdown_source_segment;
                    let document_id = segment.markdown_source_document_id.as_str();
                    let segment_text = segment.canonical_markdown_source_segment_text.as_str();
                    let (action, goal, triggers) = if document_id == "doc-a"
                        && segment_text.contains("Read the next segment")
                    {
                        self.additional_segment_decisions.fetch_add(1, Ordering::SeqCst);
                        (
                            MarkdownSourceFollowUpAction::ReadAdditionalMarkdownSourceSegment,
                            None,
                            Vec::new(),
                        )
                    } else if document_id == "doc-expand" {
                        self.expansion_decisions.fetch_add(1, Ordering::SeqCst);
                        let trigger = task
                            .accepted_verbatim_source_evidence
                            .first()
                            .ok_or_else(|| RuntimeError::ModelResponse {
                                message: "expansion fixture needs accepted evidence".to_owned(),
                            })?
                            .verbatim_source_evidence_id
                            .clone();
                        (
                            MarkdownSourceFollowUpAction::ExpandMarkdownCorpusNavigationScope,
                            None,
                            vec![trigger],
                        )
                    } else {
                        (
                            MarkdownSourceFollowUpAction::ExtractVerbatimSourceEvidence,
                            Some("extract loop fixture evidence".to_owned()),
                            Vec::new(),
                        )
                    };
                    Ok(StrongMarkdownResearchModelResponse::MarkdownSourceReview(
                        MarkdownSourceReviewDecision {
                            research_document_read_request_id: task
                                .research_document_read_request_id,
                            document_research_branch_task_id: task.document_research_branch_task_id,
                            markdown_source_document_id: segment.markdown_source_document_id,
                            markdown_source_segment_id: segment.markdown_source_segment_id,
                            markdown_source_follow_up_action: action,
                            verbatim_source_evidence_extraction_goal: goal,
                            triggering_verbatim_source_evidence_ids: triggers,
                            markdown_corpus_navigation_branch_close_reason: None,
                            markdown_source_review_summary: "loop fixture review".to_owned(),
                        },
                    ))
                }
                other => DeterministicGateway.execute_strong_markdown_research_task(other).await,
            }
        }

        async fn extract_verbatim_source_evidence_candidates(
            &self,
            task: VerbatimSourceEvidenceExtractionTask,
        ) -> Result<VerbatimSourceEvidenceCandidateSet> {
            self.extraction_requests.fetch_add(1, Ordering::SeqCst);
            let text =
                &task.authorized_markdown_source_segment.canonical_markdown_source_segment_text;
            let quote = if text.contains("Alpha evidence is exact.") {
                "Alpha evidence is exact."
            } else if text.contains("Beta evidence is exact.") {
                "Beta evidence is exact."
            } else {
                return Err(RuntimeError::ModelResponse {
                    message: "loop fixture quote not found".to_owned(),
                });
            };
            let start = text.find(quote).expect("quote checked above");
            Ok(VerbatimSourceEvidenceCandidateSet {
                verbatim_source_evidence_extraction_request_id: task
                    .verbatim_source_evidence_extraction_request_id,
                document_research_branch_task_id: task.document_research_branch_task_id,
                markdown_source_document_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_document_id,
                markdown_source_document_version_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_document_version_id,
                markdown_source_segment_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_segment_id,
                markdown_source_segment_hash: task
                    .authorized_markdown_source_segment
                    .markdown_source_segment_hash,
                verbatim_source_evidence_candidates: vec![
                    crate::model_gateway::VerbatimSourceEvidenceCandidate {
                        verbatim_source_evidence_start_byte_offset_in_segment: start as u64,
                        verbatim_source_evidence_end_byte_offset_in_segment: (start + quote.len())
                            as u64,
                        verbatim_source_evidence_quote: quote.to_owned(),
                    },
                ],
            })
        }
    }

    #[derive(Default)]
    struct LimitGateway;

    #[async_trait]
    impl MarkdownResearchModelGateway for LimitGateway {
        async fn execute_strong_markdown_research_task(
            &self,
            task: StrongMarkdownResearchModelTask,
        ) -> Result<StrongMarkdownResearchModelResponse> {
            DeterministicGateway.execute_strong_markdown_research_task(task).await
        }

        async fn extract_verbatim_source_evidence_candidates(
            &self,
            task: VerbatimSourceEvidenceExtractionTask,
        ) -> Result<VerbatimSourceEvidenceCandidateSet> {
            let text =
                &task.authorized_markdown_source_segment.canonical_markdown_source_segment_text;
            let quote = text.trim();
            let start = text.find(quote).ok_or_else(|| RuntimeError::ModelResponse {
                message: "limit fixture quote not found".to_owned(),
            })?;
            Ok(VerbatimSourceEvidenceCandidateSet {
                verbatim_source_evidence_extraction_request_id: task
                    .verbatim_source_evidence_extraction_request_id,
                document_research_branch_task_id: task.document_research_branch_task_id,
                markdown_source_document_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_document_id,
                markdown_source_document_version_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_document_version_id,
                markdown_source_segment_id: task
                    .authorized_markdown_source_segment
                    .markdown_source_segment_id,
                markdown_source_segment_hash: task
                    .authorized_markdown_source_segment
                    .markdown_source_segment_hash,
                verbatim_source_evidence_candidates: vec![
                    crate::model_gateway::VerbatimSourceEvidenceCandidate {
                        verbatim_source_evidence_start_byte_offset_in_segment: start as u64,
                        verbatim_source_evidence_end_byte_offset_in_segment: (start + quote.len())
                            as u64,
                        verbatim_source_evidence_quote: quote.to_owned(),
                    },
                ],
            })
        }
    }

    fn loop_fixture_input() -> PublishMarkdownCorpusSnapshotInput {
        let document = |path: &str, id: &str, body: &str| {
            MarkdownSourceDocumentInput {
            relative_path: path.to_owned(),
            markdown_source_bytes: format!(
                "---\nmarkdown_source_document_id: {id}\n---\n\n# {id}\n\nFixture abstract for {id}.\n\n{body}\n"
            )
            .into_bytes(),
        }
        };
        let node =
            |id: &str, children: &[&str], documents: &[&str]| MarkdownCorpusNavigationNodeInput {
                markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId::from_value(id)
                    .unwrap(),
                markdown_corpus_navigation_node_label: id.to_owned(),
                markdown_corpus_navigation_node_summary: format!("{id} fixture branch"),
                child_markdown_corpus_navigation_node_ids: children
                    .iter()
                    .map(|child| MarkdownCorpusNavigationNodeId::from_value(*child).unwrap())
                    .collect(),
                linked_markdown_source_document_ids: documents
                    .iter()
                    .map(|document| MarkdownSourceDocumentId::from_value(*document).unwrap())
                    .collect(),
            };
        PublishMarkdownCorpusSnapshotInput {
            markdown_source_documents: vec![
                document(
                    "a.md",
                    "doc-a",
                    "Read the next segment.\n\nAlpha evidence is exact.\n\nThis segment must remain unread after the branch closes.",
                ),
                document(
                    "expand.md",
                    "doc-expand",
                    "The accepted alpha evidence requires the beta branch.",
                ),
                document("b.md", "doc-b", "Beta evidence is exact."),
            ],
            markdown_corpus_navigation_nodes: vec![
                node("root-loop", &["alpha", "beta", "gamma"], &[]),
                node("alpha", &[], &["doc-a", "doc-expand"]),
                node("beta", &[], &["doc-b"]),
                node("gamma", &[], &[]),
            ],
            root_markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId::from_value(
                "root-loop",
            )
            .unwrap(),
        }
    }

    fn limited_prepared(
        snapshot: &crate::corpus::MarkdownCorpusSnapshot,
        execution_id: &str,
        outcome: ResourceExhaustionOutcome,
    ) -> PreparedMarkdownResearchExecution {
        let limits = crate::domain::MarkdownResearchExecutionLimits {
            maximum_read_markdown_source_segments: 1,
            resource_exhaustion_outcome: outcome,
            ..Default::default()
        };
        PreparedMarkdownResearchExecution {
            markdown_research_execution_id: MarkdownResearchExecutionId::from_value(execution_id)
                .unwrap(),
            document_research_conversation_id: DocumentResearchConversationId::from_value(format!(
                "conversation-{execution_id}"
            ))
            .unwrap(),
            document_research_request_id: DocumentResearchRequestId::from_value(format!(
                "request-{execution_id}"
            ))
            .unwrap(),
            frozen_document_research_brief: FrozenDocumentResearchBrief::freeze(
                "Find bounded evidence",
                "Find bounded evidence",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
            markdown_corpus_snapshot_id: snapshot.markdown_corpus_snapshot_id.clone(),
            strong_markdown_research_model_reference: "fixture-strong".to_owned(),
            verbatim_source_evidence_extraction_model_reference: "fixture-cheap".to_owned(),
            markdown_research_execution_limits: limits,
            requested_answer_composition_styles: vec![AnswerCompositionStyle::ModelKnowledgeLed],
            markdown_research_execution_prepared_at: Utc::now(),
            markdown_research_execution_prepare_command_id: CommandId::from_value(format!(
                "prepare-{execution_id}"
            ))
            .unwrap(),
        }
    }

    pub(crate) fn fixture_input() -> PublishMarkdownCorpusSnapshotInput {
        PublishMarkdownCorpusSnapshotInput {
                markdown_source_documents: vec![MarkdownSourceDocumentInput {
                    relative_path: "source.md".to_owned(),
                    markdown_source_bytes:
                        "---\nmarkdown_source_document_id: doc-1\n---\n\n# Fixture\n\nFixture abstract.\n\n原文证据：规则适用于本例。\n"
                            .as_bytes()
                            .to_vec(),
                }],
                markdown_corpus_navigation_nodes: vec![
                    MarkdownCorpusNavigationNodeInput {
                        markdown_corpus_navigation_node_id:
                            MarkdownCorpusNavigationNodeId::from_value("root").unwrap(),
                        markdown_corpus_navigation_node_label: "Root".to_owned(),
                        markdown_corpus_navigation_node_summary: "Root".to_owned(),
                        child_markdown_corpus_navigation_node_ids: vec![
                            MarkdownCorpusNavigationNodeId::from_value("child").unwrap(),
                        ],
                        linked_markdown_source_document_ids: Vec::new(),
                    },
                    MarkdownCorpusNavigationNodeInput {
                        markdown_corpus_navigation_node_id:
                            MarkdownCorpusNavigationNodeId::from_value("child").unwrap(),
                        markdown_corpus_navigation_node_label: "Child".to_owned(),
                        markdown_corpus_navigation_node_summary: "Child".to_owned(),
                        child_markdown_corpus_navigation_node_ids: Vec::new(),
                        linked_markdown_source_document_ids: vec![
                            MarkdownSourceDocumentId::from_value("doc-1").unwrap(),
                        ],
                    },
                ],
                root_markdown_corpus_navigation_node_id:
                    MarkdownCorpusNavigationNodeId::from_value("root").unwrap(),
            }
    }

    pub(crate) fn fixture_snapshot(
        subject_id: &SubjectId,
    ) -> crate::corpus::MarkdownCorpusSnapshot {
        build_markdown_corpus_snapshot(subject_id.clone(), fixture_input(), Utc::now()).unwrap()
    }

    #[tokio::test]
    async fn fixed_engine_completes_one_evidence_path_and_replays_terminal() {
        let subject_id = SubjectId::from_value("subject-engine").unwrap();
        let snapshot = fixture_snapshot(&subject_id);
        let prepared = PreparedMarkdownResearchExecution {
            markdown_research_execution_id: MarkdownResearchExecutionId::from_value(
                "execution-engine",
            )
            .unwrap(),
            document_research_conversation_id: DocumentResearchConversationId::from_value(
                "conversation-engine",
            )
            .unwrap(),
            document_research_request_id: DocumentResearchRequestId::from_value("request-engine")
                .unwrap(),
            frozen_document_research_brief: FrozenDocumentResearchBrief::freeze(
                "原问题",
                "澄清问题",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
            markdown_corpus_snapshot_id: snapshot.markdown_corpus_snapshot_id.clone(),
            strong_markdown_research_model_reference: "fixture-strong".to_owned(),
            verbatim_source_evidence_extraction_model_reference: "fixture-cheap".to_owned(),
            markdown_research_execution_limits: Default::default(),
            requested_answer_composition_styles: vec![
                AnswerCompositionStyle::ModelKnowledgeLed,
                AnswerCompositionStyle::EvidenceLinkedResearchClaimLed,
            ],
            markdown_research_execution_prepared_at: Utc::now(),
            markdown_research_execution_prepare_command_id: CommandId::from_value("prepare-engine")
                .unwrap(),
        };
        let principal =
            ResearchPrincipal::new(subject_id, [PrincipalCapability::ExecuteMarkdownResearch]);
        let trace = MarkdownResearchExecutionTrace::from_storage(
            crate::storage::Storage::open_in_memory().unwrap(),
        );
        let engine = MarkdownResearchExecutionEngine::new(Arc::new(DeterministicGateway));
        let state = engine
            .execute_prepared_markdown_research(&principal, &prepared, &snapshot, &trace)
            .await
            .unwrap();
        assert_eq!(state.terminal_state, Some(MarkdownResearchExecutionTerminalState::Completed));
        assert_eq!(state.accepted_verbatim_source_evidence.len(), 1);
        assert_eq!(state.source_attributed_answer_compositions.len(), 2);
        let replayed = engine
            .execute_prepared_markdown_research(&principal, &prepared, &snapshot, &trace)
            .await
            .unwrap();
        assert_eq!(replayed.events.len(), state.events.len());
    }

    #[tokio::test]
    async fn fixed_engine_executes_additional_segment_and_evidence_backed_scope_expansion_loops() {
        let subject_id = SubjectId::from_value("subject-loop-engine").unwrap();
        let snapshot =
            build_markdown_corpus_snapshot(subject_id.clone(), loop_fixture_input(), Utc::now())
                .unwrap();
        let prepared = PreparedMarkdownResearchExecution {
            markdown_research_execution_id: MarkdownResearchExecutionId::from_value(
                "execution-loop-engine",
            )
            .unwrap(),
            document_research_conversation_id: DocumentResearchConversationId::from_value(
                "conversation-loop-engine",
            )
            .unwrap(),
            document_research_request_id: DocumentResearchRequestId::from_value(
                "request-loop-engine",
            )
            .unwrap(),
            frozen_document_research_brief: FrozenDocumentResearchBrief::freeze(
                "Find both evidence statements",
                "Find both evidence statements",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
            markdown_corpus_snapshot_id: snapshot.markdown_corpus_snapshot_id.clone(),
            strong_markdown_research_model_reference: "fixture-strong".to_owned(),
            verbatim_source_evidence_extraction_model_reference: "fixture-cheap".to_owned(),
            markdown_research_execution_limits: Default::default(),
            requested_answer_composition_styles: vec![AnswerCompositionStyle::ModelKnowledgeLed],
            markdown_research_execution_prepared_at: Utc::now(),
            markdown_research_execution_prepare_command_id: CommandId::from_value(
                "prepare-loop-engine",
            )
            .unwrap(),
        };
        let principal =
            ResearchPrincipal::new(subject_id, [PrincipalCapability::ExecuteMarkdownResearch]);
        let trace = MarkdownResearchExecutionTrace::from_storage(
            crate::storage::Storage::open_in_memory().unwrap(),
        );
        let gateway = Arc::new(LoopGateway::default());
        let engine = MarkdownResearchExecutionEngine::new(gateway.clone());
        let state = engine
            .execute_prepared_markdown_research(&principal, &prepared, &snapshot, &trace)
            .await
            .unwrap();

        assert_eq!(state.terminal_state, Some(MarkdownResearchExecutionTerminalState::Completed));
        assert_eq!(state.accepted_verbatim_source_evidence.len(), 2);
        assert_eq!(gateway.additional_segment_decisions.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.expansion_decisions.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.expansion_navigation_rounds.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.extraction_requests.load(Ordering::SeqCst), 2);
        assert_eq!(read_segment_count(&state), 4);
        assert_eq!(scope_expansion_requests(&state).len(), 1);
    }

    #[tokio::test]
    async fn repeated_navigation_selection_does_not_reopen_a_finished_branch() {
        let subject_id = SubjectId::from_value("subject-repeat-branch-engine").unwrap();
        let snapshot =
            build_markdown_corpus_snapshot(subject_id.clone(), loop_fixture_input(), Utc::now())
                .unwrap();
        let mut prepared = limited_prepared(
            &snapshot,
            "execution-repeat-branch-engine",
            ResourceExhaustionOutcome::ProduceLimitedAnswerWithGapDisclosure,
        );
        prepared.markdown_research_execution_limits = Default::default();
        prepared
            .markdown_research_execution_limits
            .maximum_selected_markdown_corpus_navigation_branches_per_level = 1;
        let principal =
            ResearchPrincipal::new(subject_id, [PrincipalCapability::ExecuteMarkdownResearch]);
        let trace = MarkdownResearchExecutionTrace::from_storage(
            crate::storage::Storage::open_in_memory().unwrap(),
        );
        let gateway = Arc::new(LoopGateway {
            repeat_only_prior_branch_on_expansion: true,
            ..Default::default()
        });
        let engine = MarkdownResearchExecutionEngine::new(gateway.clone());

        let state = engine
            .execute_prepared_markdown_research(&principal, &prepared, &snapshot, &trace)
            .await
            .unwrap();

        assert_eq!(state.terminal_state, Some(MarkdownResearchExecutionTerminalState::Completed));
        assert_eq!(state.accepted_verbatim_source_evidence.len(), 1);
        assert_eq!(read_segment_count(&state), 3);
        assert!(state.research_coverage_gaps.is_empty());
        assert_eq!(gateway.expansion_navigation_rounds.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.extraction_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn per_level_navigation_limit_accumulates_unique_nodes_across_expansion_rounds() {
        let subject_id = SubjectId::from_value("subject-global-level-limit-engine").unwrap();
        let snapshot =
            build_markdown_corpus_snapshot(subject_id.clone(), loop_fixture_input(), Utc::now())
                .unwrap();
        let mut prepared = limited_prepared(
            &snapshot,
            "execution-global-level-limit-engine",
            ResourceExhaustionOutcome::ProduceLimitedAnswerWithGapDisclosure,
        );
        prepared.markdown_research_execution_limits = Default::default();
        prepared
            .markdown_research_execution_limits
            .maximum_selected_markdown_corpus_navigation_branches_per_level = 1;
        let principal =
            ResearchPrincipal::new(subject_id, [PrincipalCapability::ExecuteMarkdownResearch]);
        let trace = MarkdownResearchExecutionTrace::from_storage(
            crate::storage::Storage::open_in_memory().unwrap(),
        );
        let gateway = Arc::new(LoopGateway::default());
        let engine = MarkdownResearchExecutionEngine::new(gateway.clone());

        let state = engine
            .execute_prepared_markdown_research(&principal, &prepared, &snapshot, &trace)
            .await
            .unwrap();

        assert_eq!(state.terminal_state, Some(MarkdownResearchExecutionTerminalState::Completed));
        assert_eq!(state.accepted_verbatim_source_evidence.len(), 1);
        assert_eq!(state.research_coverage_gaps.len(), 1);
        assert_eq!(state.failed_document_research_branch_task_ids.len(), 1);
        assert!(state.failed_document_research_branch_task_ids.contains(&branch_task_id(
            &prepared,
            &MarkdownCorpusNavigationNodeId::from_value("beta").unwrap(),
        )));
        assert_eq!(read_segment_count(&state), 3);
        assert_eq!(gateway.extraction_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn limit_exhaustion_deduplicates_repeated_pending_navigation_nodes() {
        let subject_id = SubjectId::from_value("subject-limit-dedup-engine").unwrap();
        let snapshot =
            build_markdown_corpus_snapshot(subject_id.clone(), loop_fixture_input(), Utc::now())
                .unwrap();
        let mut prepared = limited_prepared(
            &snapshot,
            "execution-limit-dedup-engine",
            ResourceExhaustionOutcome::ProduceLimitedAnswerWithGapDisclosure,
        );
        prepared.markdown_research_execution_limits = Default::default();
        let principal =
            ResearchPrincipal::new(subject_id, [PrincipalCapability::ExecuteMarkdownResearch]);
        let trace = MarkdownResearchExecutionTrace::from_storage(
            crate::storage::Storage::open_in_memory().unwrap(),
        );
        let engine = MarkdownResearchExecutionEngine::new(Arc::new(LoopGateway::default()));
        let mut state = trace
            .append_markdown_research_execution_events(
                &principal,
                &prepared.markdown_research_execution_id,
                command_id(&prepared, "execution-start"),
                Utc::now(),
                vec![
                    MarkdownResearchExecutionEventKind::MarkdownResearchExecutionStarted {
                        prepared_markdown_research_execution: Box::new(prepared.clone()),
                    },
                    MarkdownResearchExecutionEventKind::MarkdownResearchExecutionRunning,
                ],
            )
            .await
            .unwrap();
        engine
            .ensure_model_knowledge_answer(&principal, &prepared, &trace, &mut state)
            .await
            .unwrap();
        let reader = snapshot.reader();
        let validator =
            MarkdownSourceEvidenceIntegrityValidator::for_locked_markdown_corpus_snapshot(
                &principal.subject_id,
                &prepared.markdown_research_execution_id,
                &prepared.markdown_corpus_snapshot_id,
                &prepared.requested_answer_composition_styles,
                &snapshot,
            )
            .unwrap();
        engine
            .explore_navigation_and_sources(
                &principal, &prepared, &snapshot, &trace, &reader, &validator, &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.accepted_verbatim_source_evidence.len(), 2);

        let gamma = MarkdownCorpusNavigationNodeId::from_value("gamma").unwrap();
        for duplicate_round in [100_usize, 101] {
            let candidate_set_id = candidate_set_id(
                &prepared,
                &snapshot.root_markdown_corpus_navigation_node_id,
                0,
                duplicate_round,
            );
            state = engine
                .append_events(
                    &principal,
                    &prepared,
                    &trace,
                    state,
                    &format!("duplicate-pending-navigation-{duplicate_round}"),
                    vec![
                        MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationChildCandidatesPresented {
                            markdown_corpus_navigation_candidate_set_id: candidate_set_id.clone(),
                            parent_markdown_corpus_navigation_node_id: snapshot
                                .root_markdown_corpus_navigation_node_id
                                .clone(),
                            child_markdown_corpus_navigation_node_ids: vec![gamma.clone()],
                        },
                        MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchesSelected {
                            markdown_corpus_navigation_candidate_set_id: candidate_set_id,
                            markdown_corpus_navigation_branch_selections: vec![
                                MarkdownCorpusNavigationBranchSelection {
                                    markdown_corpus_navigation_node_id: gamma.clone(),
                                    markdown_corpus_navigation_node_selection_status:
                                        MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch,
                                    markdown_corpus_navigation_node_relevance_explanation:
                                        "duplicate pending fixture selection".to_owned(),
                                    expected_research_information_to_resolve_question:
                                        "pending fixture information".to_owned(),
                                    markdown_corpus_navigation_branch_priority: 1,
                                },
                            ],
                        },
                    ],
                )
                .await
                .unwrap();
        }

        let state = engine
            .finish_limit_exhaustion(
                &principal,
                &prepared,
                &trace,
                state,
                "the duplicate pending fixture exhausted its budget",
                true,
            )
            .await
            .unwrap();
        let gamma_failures = state
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.event_kind,
                    MarkdownResearchExecutionEventKind::MarkdownCorpusNavigationBranchDocumentReportFailed {
                        markdown_corpus_navigation_node_id,
                        ..
                    } if markdown_corpus_navigation_node_id == &gamma
                )
            })
            .count();
        assert_eq!(gamma_failures, 1);
        assert_eq!(state.failed_document_research_branch_task_ids.len(), 1);
        assert_eq!(state.research_coverage_gaps.len(), 1);
    }

    #[tokio::test]
    async fn frozen_resource_outcome_controls_limit_failure_or_gap_disclosure() {
        let subject_id = SubjectId::from_value("subject-limit-engine").unwrap();
        let snapshot =
            build_markdown_corpus_snapshot(subject_id.clone(), loop_fixture_input(), Utc::now())
                .unwrap();
        let principal =
            ResearchPrincipal::new(subject_id, [PrincipalCapability::ExecuteMarkdownResearch]);
        let engine = MarkdownResearchExecutionEngine::new(Arc::new(LimitGateway));

        let disclosed = limited_prepared(
            &snapshot,
            "execution-limited-answer",
            ResourceExhaustionOutcome::ProduceLimitedAnswerWithGapDisclosure,
        );
        let disclosed_trace = MarkdownResearchExecutionTrace::from_storage(
            crate::storage::Storage::open_in_memory().unwrap(),
        );
        let disclosed_state = engine
            .execute_prepared_markdown_research(&principal, &disclosed, &snapshot, &disclosed_trace)
            .await
            .unwrap();
        assert_eq!(
            disclosed_state.terminal_state,
            Some(MarkdownResearchExecutionTerminalState::Completed)
        );
        assert_eq!(read_segment_count(&disclosed_state), 1);
        assert_eq!(disclosed_state.research_coverage_gaps.len(), 1);
        assert!(disclosed_state.research_coverage_gaps.values().all(|gap| {
            gap.research_coverage_gap_resolution_status
                == ResearchCoverageGapResolutionStatus::DisclosedInAnswer
        }));
        let answer = disclosed_state
            .project_public_markdown_research_answer(AnswerCompositionStyle::ModelKnowledgeLed)
            .unwrap();
        assert_eq!(answer.disclosed_research_coverage_gaps.len(), 1);

        let failed = limited_prepared(
            &snapshot,
            "execution-limited-failure",
            ResourceExhaustionOutcome::FailExecution,
        );
        let failed_trace = MarkdownResearchExecutionTrace::from_storage(
            crate::storage::Storage::open_in_memory().unwrap(),
        );
        let failed_state = engine
            .execute_prepared_markdown_research(&principal, &failed, &snapshot, &failed_trace)
            .await
            .unwrap();
        assert_eq!(
            failed_state.terminal_state,
            Some(MarkdownResearchExecutionTerminalState::Failed)
        );
        assert!(failed_state.source_attributed_answer_compositions.is_empty());
    }
}

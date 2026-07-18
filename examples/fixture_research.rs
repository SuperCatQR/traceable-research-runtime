//! Runs one complete offline research request through the public Runtime Interface.

use async_trait::async_trait;
use std::sync::Arc;
use traceable_markdown_research_runtime::{
    AnswerCompositionStyle, DocumentResearchBriefDraft, EvidenceLinkedResearchClaim,
    EvidenceLinkedResearchClaimCitationStatus, EvidenceLinkedResearchClaimGenerationResponse,
    EvidenceLinkedResearchClaimsAnswer, MarkdownCorpusNavigationBranchDocumentRelevanceReport,
    MarkdownCorpusNavigationBranchSelection, MarkdownCorpusNavigationNodeId,
    MarkdownCorpusNavigationNodeInput, MarkdownCorpusNavigationNodeSelectionStatus,
    MarkdownResearchExecutionLimits, MarkdownResearchModelGateway, MarkdownSourceDocumentId,
    MarkdownSourceDocumentInput, MarkdownSourceFollowUpAction, MarkdownSourceReviewDecision,
    ModelKnowledgeOnlyAnswer, PrincipalCapability, PublishMarkdownCorpusSnapshotInput,
    ResearchClaimEvidenceRelationship, ResearchClaimEvidenceRelationshipType,
    ResearchDocumentReadRequest, ResearchPrincipal, ResearchQuestionClarificationDecision,
    ResearchQuestionClarificationModelOutput, Result, RuntimeError,
    SourceAttributedAnswerComposition, SourceAttributedAnswerSegment,
    SourceAttributedAnswerSegmentSourceType, StrongMarkdownResearchModelResponse,
    StrongMarkdownResearchModelTask, SubjectId, TraceableMarkdownResearchRuntime,
    VerbatimSourceEvidenceCandidate, VerbatimSourceEvidenceCandidateSet,
    VerbatimSourceEvidenceExtractionTask,
};

#[derive(Debug, Default)]
struct OfflineFixtureGateway;

#[async_trait]
impl MarkdownResearchModelGateway for OfflineFixtureGateway {
    async fn execute_strong_markdown_research_task(
        &self,
        task: StrongMarkdownResearchModelTask,
    ) -> Result<StrongMarkdownResearchModelResponse> {
        match task {
            StrongMarkdownResearchModelTask::ResearchQuestionEvaluation(task) => {
                Ok(StrongMarkdownResearchModelResponse::ResearchQuestionEvaluation(
                    ResearchQuestionClarificationModelOutput {
                        research_question_clarification_revision:
                            task.research_question_clarification_revision,
                        research_question_clarification_decision:
                            ResearchQuestionClarificationDecision::StartMarkdownResearchExecution,
                        research_question_clarification_message: None,
                        document_research_brief_draft: DocumentResearchBriefDraft {
                            original_user_question: task.original_user_question.clone(),
                            clarified_research_question: task.original_user_question,
                            known_document_research_context: Vec::new(),
                            document_research_assumptions: Vec::new(),
                            unresolved_research_question_ambiguities: Vec::new(),
                            requested_research_answer_requirements: vec![
                                "Cite the exact Markdown source passage".to_owned(),
                            ],
                        },
                    },
                ))
            }
            StrongMarkdownResearchModelTask::ModelKnowledgeOnlyAnswerGeneration(task) => {
                Ok(StrongMarkdownResearchModelResponse::ModelKnowledgeOnlyAnswerGeneration(
                    ModelKnowledgeOnlyAnswer {
                        model_knowledge_only_answer_id: task.markdown_research_model_task_id,
                        model_knowledge_only_answer_text:
                            "The model-only route has no access to the current corpus.".to_owned(),
                        markdown_research_execution_id: task.markdown_research_execution_id,
                    },
                ))
            }
            StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchSelection(task) => {
                let selections = task
                    .markdown_corpus_navigation_node_candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| MarkdownCorpusNavigationBranchSelection {
                        markdown_corpus_navigation_node_id: candidate
                            .markdown_corpus_navigation_node_id,
                        markdown_corpus_navigation_node_selection_status:
                            MarkdownCorpusNavigationNodeSelectionStatus::SelectedForMarkdownResearch,
                        markdown_corpus_navigation_node_relevance_explanation:
                            "The branch contains the requested policy evidence.".to_owned(),
                        expected_research_information_to_resolve_question:
                            "An exact statement of the retention rule.".to_owned(),
                        markdown_corpus_navigation_branch_priority: (index + 1) as u32,
                    })
                    .collect();
                Ok(StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchSelection(
                    traceable_markdown_research_runtime::MarkdownCorpusNavigationBranchSelectionResponse {
                        markdown_corpus_navigation_candidate_set_id:
                            task.markdown_corpus_navigation_candidate_set_id,
                        markdown_corpus_navigation_branch_selections: selections,
                    },
                ))
            }
            StrongMarkdownResearchModelTask::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
                task,
            ) => {
                let document_ids = task
                    .markdown_source_document_candidates
                    .iter()
                    .map(|candidate| candidate.markdown_source_document_id.clone())
                    .collect::<Vec<_>>();
                Ok(
                    StrongMarkdownResearchModelResponse::MarkdownCorpusNavigationBranchDocumentRelevanceReport(
                        MarkdownCorpusNavigationBranchDocumentRelevanceReport {
                            document_research_branch_task_id:
                                task.document_research_branch_task_id,
                            markdown_corpus_navigation_node_id:
                                task.markdown_corpus_navigation_node_id,
                            candidate_markdown_source_document_ids: document_ids.clone(),
                            selected_markdown_source_document_ids: document_ids,
                            markdown_corpus_navigation_branch_document_report_summary:
                                "The policy document directly addresses retention.".to_owned(),
                        },
                    ),
                )
            }
            StrongMarkdownResearchModelTask::ResearchDocumentReadRequest(task) => {
                let segment = task.candidate_markdown_source_segments.first().ok_or_else(|| {
                    RuntimeError::ModelResponse {
                        message: "offline fixture received no segment candidate".to_owned(),
                    }
                })?;
                Ok(StrongMarkdownResearchModelResponse::ResearchDocumentReadRequest(
                    ResearchDocumentReadRequest {
                        research_document_read_request_id: task.research_document_read_request_id,
                        document_research_branch_task_id: task.document_research_branch_task_id,
                        markdown_source_document_id: segment.markdown_source_document_id.clone(),
                        markdown_source_segment_id: segment.markdown_source_segment_id.clone(),
                        unresolved_research_question:
                            "What is the retention period?".to_owned(),
                        expected_research_information_to_resolve_question:
                            "A verbatim retention period.".to_owned(),
                        markdown_source_document_selection_explanation:
                            "The document abstract identifies the retention policy.".to_owned(),
                    },
                ))
            }
            StrongMarkdownResearchModelTask::MarkdownSourceReview(task) => {
                let segment = task.authorized_markdown_source_segment;
                Ok(StrongMarkdownResearchModelResponse::MarkdownSourceReview(
                    MarkdownSourceReviewDecision {
                        research_document_read_request_id: task.research_document_read_request_id,
                        document_research_branch_task_id: task.document_research_branch_task_id,
                        markdown_source_document_id: segment.markdown_source_document_id,
                        markdown_source_segment_id: segment.markdown_source_segment_id,
                        markdown_source_follow_up_action:
                            MarkdownSourceFollowUpAction::ExtractVerbatimSourceEvidence,
                        verbatim_source_evidence_extraction_goal: Some(
                            "Extract the exact retention-period sentence.".to_owned(),
                        ),
                        triggering_verbatim_source_evidence_ids: Vec::new(),
                        markdown_corpus_navigation_branch_close_reason: None,
                        markdown_source_review_summary:
                            "The segment contains the exact rule.".to_owned(),
                    },
                ))
            }
            StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimGeneration(task) => {
                let evidence = task.accepted_verbatim_source_evidence.first().ok_or_else(|| {
                    RuntimeError::ModelResponse {
                        message: "offline fixture received no accepted evidence".to_owned(),
                    }
                })?;
                let claim_id = task
                    .authorized_evidence_linked_research_claim_ids
                    .first()
                    .cloned()
                    .ok_or_else(|| RuntimeError::ModelResponse {
                        message: "offline fixture received no authorized claim ID".to_owned(),
                    })?;
                Ok(StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimGeneration(
                    EvidenceLinkedResearchClaimGenerationResponse {
                        evidence_linked_research_claims: vec![EvidenceLinkedResearchClaim {
                            evidence_linked_research_claim_id: claim_id,
                            evidence_linked_research_claim_text:
                                "Research records are retained for seven years.".to_owned(),
                            research_claim_evidence_relationships: vec![
                                ResearchClaimEvidenceRelationship {
                                    verbatim_source_evidence_id:
                                        evidence.verbatim_source_evidence_id.clone(),
                                    research_claim_evidence_relationship_type:
                                        ResearchClaimEvidenceRelationshipType::SupportsEvidenceLinkedResearchClaim,
                                },
                            ],
                            evidence_linked_research_claim_applicability_conditions: Vec::new(),
                            evidence_linked_research_claim_exceptions: Vec::new(),
                            evidence_linked_research_claim_citation_status:
                                EvidenceLinkedResearchClaimCitationStatus::AllCitationsLinkedToVerbatimSourceEvidence,
                            markdown_research_execution_id: task.markdown_research_execution_id,
                        }],
                    },
                ))
            }
            StrongMarkdownResearchModelTask::EvidenceLinkedResearchClaimsAnswerGeneration(task) => {
                let claim = task.committed_evidence_linked_research_claims.first().ok_or_else(
                    || RuntimeError::ModelResponse {
                        message: "offline fixture received no committed claim".to_owned(),
                    },
                )?;
                Ok(
                    StrongMarkdownResearchModelResponse::EvidenceLinkedResearchClaimsAnswerGeneration(
                        EvidenceLinkedResearchClaimsAnswer {
                            evidence_linked_research_claims_answer_id:
                                task.markdown_research_model_task_id,
                            evidence_linked_research_claims_answer_text:
                                "Research records are retained for seven years.".to_owned(),
                            supporting_evidence_linked_research_claim_ids: vec![
                                claim.evidence_linked_research_claim_id.clone(),
                            ],
                            markdown_research_execution_id: task.markdown_research_execution_id,
                        },
                    ),
                )
            }
            StrongMarkdownResearchModelTask::SourceAttributedAnswerComposition(task) => {
                let claim = task.committed_evidence_linked_research_claims.first().ok_or_else(
                    || RuntimeError::ModelResponse {
                        message: "offline fixture received no committed claim".to_owned(),
                    },
                )?;
                let citation = task.public_source_citations.first().ok_or_else(|| {
                    RuntimeError::ModelResponse {
                        message: "offline fixture received no public citation".to_owned(),
                    }
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
                        source_attributed_answer_segments: vec![SourceAttributedAnswerSegment {
                            source_attributed_answer_segment_text:
                                "Research records are retained for seven years.".to_owned(),
                            source_attributed_answer_segment_source_type:
                                SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaims,
                            supporting_evidence_linked_research_claim_ids: vec![
                                claim.evidence_linked_research_claim_id.clone(),
                            ],
                            supporting_public_source_citation_ids: vec![
                                citation.public_source_citation_id.clone(),
                            ],
                            model_knowledge_unverified_notice: None,
                        }],
                        source_attributed_answer_composition_review_reason:
                            "The answer is limited to the verified corpus claim.".to_owned(),
                        answer_projection_schema_version: 1,
                    },
                ))
            }
        }
    }

    async fn extract_verbatim_source_evidence_candidates(
        &self,
        task: VerbatimSourceEvidenceExtractionTask,
    ) -> Result<VerbatimSourceEvidenceCandidateSet> {
        let quote = "Research records are retained for seven years.";
        let start = task
            .authorized_markdown_source_segment
            .canonical_markdown_source_segment_text
            .find(quote)
            .ok_or_else(|| RuntimeError::ModelResponse {
                message: "offline fixture quote is absent from the authorized segment".to_owned(),
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
            verbatim_source_evidence_candidates: vec![VerbatimSourceEvidenceCandidate {
                verbatim_source_evidence_start_byte_offset_in_segment: start as u64,
                verbatim_source_evidence_end_byte_offset_in_segment: (start + quote.len()) as u64,
                verbatim_source_evidence_quote: quote.to_owned(),
            }],
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let runtime = TraceableMarkdownResearchRuntime::in_memory(Arc::new(OfflineFixtureGateway))?;
    let principal = ResearchPrincipal::new(
        SubjectId::from_value("example-owner")?,
        [
            PrincipalCapability::PublishMarkdownCorpusSnapshot,
            PrincipalCapability::ExecuteMarkdownResearch,
        ],
    );
    let document_id = MarkdownSourceDocumentId::from_value("retention-policy")?;
    let root_id = MarkdownCorpusNavigationNodeId::from_value("root")?;
    let policy_id = MarkdownCorpusNavigationNodeId::from_value("policy")?;
    let snapshot_id = runtime
        .publish_markdown_corpus_snapshot(
            &principal,
            PublishMarkdownCorpusSnapshotInput {
                markdown_source_documents: vec![MarkdownSourceDocumentInput {
                    relative_path: "policies/retention.md".to_owned(),
                    markdown_source_bytes: br#"---
markdown_source_document_id: retention-policy
---

# Research Retention Policy

Retention rules for completed research records.

Research records are retained for seven years.
"#
                    .to_vec(),
                }],
                markdown_corpus_navigation_nodes: vec![
                    MarkdownCorpusNavigationNodeInput {
                        markdown_corpus_navigation_node_id: root_id.clone(),
                        markdown_corpus_navigation_node_label: "Research policies".to_owned(),
                        markdown_corpus_navigation_node_summary:
                            "Controlled policies for research operations.".to_owned(),
                        child_markdown_corpus_navigation_node_ids: vec![policy_id.clone()],
                        linked_markdown_source_document_ids: Vec::new(),
                    },
                    MarkdownCorpusNavigationNodeInput {
                        markdown_corpus_navigation_node_id: policy_id,
                        markdown_corpus_navigation_node_label: "Retention".to_owned(),
                        markdown_corpus_navigation_node_summary:
                            "Retention periods and handling rules.".to_owned(),
                        child_markdown_corpus_navigation_node_ids: Vec::new(),
                        linked_markdown_source_document_ids: vec![document_id],
                    },
                ],
                root_markdown_corpus_navigation_node_id: root_id,
            },
        )
        .await?;
    let conversation_id = runtime.create_document_research_conversation(&principal).await?;
    let started = runtime
        .start_document_research_request(
            &principal,
            &conversation_id,
            "How long are completed research records retained?",
            vec![AnswerCompositionStyle::EvidenceLinkedResearchClaimLed],
        )
        .await?;
    runtime
        .prepare_markdown_research_execution(
            &principal,
            &conversation_id,
            &started.document_research_request_id,
            &snapshot_id,
            "offline-strong-fixture",
            "offline-extraction-fixture",
            MarkdownResearchExecutionLimits::default(),
            vec![AnswerCompositionStyle::EvidenceLinkedResearchClaimLed],
        )
        .await?;
    let result = runtime
        .execute_prepared_markdown_research(&principal, &started.document_research_request_id)
        .await?;

    println!("{}", serde_json::to_string_pretty(&result.public_markdown_research_answers)?);
    Ok(())
}

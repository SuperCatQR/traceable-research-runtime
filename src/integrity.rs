//! Deterministic source, claim and answer integrity validation.

use crate::corpus::{
    AuthorizedMarkdownSourceSegment, MARKDOWN_CANONICALIZATION_SCHEMA_VERSION,
    MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION, MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION,
    MARKDOWN_PARSER_SCHEMA_VERSION, MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION,
    MarkdownCorpusSnapshot, MarkdownSourceDocumentVersion, MarkdownSourceSegment,
};
use crate::domain::{
    AnswerCompositionStyle, EvidenceLinkedResearchClaim, EvidenceLinkedResearchClaimsAnswer,
    ModelKnowledgeOnlyAnswer, PublicSourceCitation, SourceAttributedAnswerComposition,
    SourceAttributedAnswerSegment, SourceAttributedAnswerSegmentSourceType, VerbatimSourceEvidence,
    canonical_content_hash, sha256_content_hash,
};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::execution_trace::ResearchDocumentReadRequest;
use crate::identity::{
    DocumentResearchBranchTaskId, MarkdownCorpusSnapshotId, MarkdownResearchExecutionId,
    MarkdownSourceDocumentId, MarkdownSourceSegmentId, PublicSourceCitationId,
    ResearchDocumentReadRequestId, SubjectId, VerbatimSourceEvidenceExtractionRequestId,
    VerbatimSourceEvidenceId,
};
use crate::model_gateway::{VerbatimSourceEvidenceCandidate, VerbatimSourceEvidenceCandidateSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Stable disclosure shown for every answer segment containing model knowledge.
pub(crate) const MODEL_KNOWLEDGE_UNVERIFIED_NOTICE: &str = "模型补充，未由当前 Markdown 文档验证";

/// Persisted extraction response envelope used to prove candidate ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedVerbatimSourceEvidenceCandidateSet {
    /// Owning subject from the persisted envelope, never supplied by the model.
    pub(crate) owner_subject_id: SubjectId,
    /// Owning execution.
    pub(crate) markdown_research_execution_id: MarkdownResearchExecutionId,
    /// Locked snapshot.
    pub(crate) markdown_corpus_snapshot_id: MarkdownCorpusSnapshotId,
    /// Locked snapshot content hash.
    pub(crate) markdown_corpus_snapshot_hash: String,
    /// Authorized read request used for the extraction.
    pub(crate) research_document_read_request_id: ResearchDocumentReadRequestId,
    /// Source document version content hash.
    pub(crate) markdown_source_document_version_content_hash: String,
    /// Complete typed Gateway response persisted before candidate acceptance.
    pub(crate) verbatim_source_evidence_candidate_set: VerbatimSourceEvidenceCandidateSet,
}

/// Persisted proof that one exact segment was authorized and read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PersistedAuthorizedMarkdownSourceRead<'a> {
    /// Owning subject recovered from the trace envelope.
    pub(crate) owner_subject_id: &'a SubjectId,
    /// Owning execution recovered from the trace stream.
    pub(crate) markdown_research_execution_id: &'a MarkdownResearchExecutionId,
    /// Locked snapshot used by the read.
    pub(crate) markdown_corpus_snapshot_id: &'a MarkdownCorpusSnapshotId,
    /// Snapshot content hash observed for the read.
    pub(crate) markdown_corpus_snapshot_hash: &'a str,
    /// Persisted read authorization.
    pub(crate) research_document_read_request: &'a ResearchDocumentReadRequest,
    /// Segment payload obtained through the snapshot reader.
    pub(crate) authorized_markdown_source_segment: AuthorizedMarkdownSourceSegment<'a>,
    /// Segment hash recorded by `markdown_source_segment_read`.
    pub(crate) observed_markdown_source_segment_hash: &'a str,
}

/// Runtime-owned identifiers assigned only after a model candidate is selected.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProgramAssignedVerbatimSourceEvidenceIds<'a> {
    /// Program-generated evidence ID.
    pub(crate) verbatim_source_evidence_id: &'a VerbatimSourceEvidenceId,
    /// Program-generated public citation ID.
    pub(crate) public_source_citation_id: &'a PublicSourceCitationId,
}

/// One accepted evidence item and its one-to-one public citation projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedVerbatimSourceEvidence {
    /// Accepted source evidence.
    pub(crate) verbatim_source_evidence: VerbatimSourceEvidence,
    /// Citation derived by the program from the locked snapshot.
    pub(crate) public_source_citation: PublicSourceCitation,
}

/// Complete input for accepting one candidate from a persisted set.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ValidateVerbatimSourceEvidenceCandidateInput<'a> {
    /// Branch currently being executed.
    pub(crate) expected_document_research_branch_task_id: &'a DocumentResearchBranchTaskId,
    /// Extraction request currently being committed.
    pub(crate) expected_verbatim_source_evidence_extraction_request_id:
        &'a VerbatimSourceEvidenceExtractionRequestId,
    /// Persisted read authorization.
    pub(crate) authorized_markdown_source_read: PersistedAuthorizedMarkdownSourceRead<'a>,
    /// Complete persisted candidate set.
    pub(crate) persisted_verbatim_source_evidence_candidate_set:
        &'a PersistedVerbatimSourceEvidenceCandidateSet,
    /// Selected candidate, which must occur exactly once in the persisted set.
    pub(crate) verbatim_source_evidence_candidate: &'a VerbatimSourceEvidenceCandidate,
    /// Runtime-assigned output identifiers.
    pub(crate) program_assigned_ids: ProgramAssignedVerbatimSourceEvidenceIds<'a>,
    /// Evidence already accepted by this execution.
    pub(crate) previously_accepted_verbatim_source_evidence:
        &'a [ValidatedVerbatimSourceEvidence],
}

/// Complete input for validating one source-attributed answer composition.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ValidateSourceAttributedAnswerCompositionInput<'a> {
    /// Independently generated model-only answer.
    pub(crate) model_knowledge_only_answer: &'a ModelKnowledgeOnlyAnswer,
    /// Claims-only answer.
    pub(crate) evidence_linked_research_claims_answer: &'a EvidenceLinkedResearchClaimsAnswer,
    /// Claims committed for the current execution.
    pub(crate) committed_evidence_linked_research_claims: &'a [EvidenceLinkedResearchClaim],
    /// Accepted evidence/citation associations for the current execution.
    pub(crate) accepted_verbatim_source_evidence: &'a [ValidatedVerbatimSourceEvidence],
    /// Proposed composition.
    pub(crate) source_attributed_answer_composition: &'a SourceAttributedAnswerComposition,
}

/// Pure validator bound to one owner, execution and immutable snapshot.
///
/// Construction verifies the complete snapshot content address once. All later
/// methods are deterministic and perform no I/O or identifier generation.
#[derive(Debug)]
pub(crate) struct MarkdownSourceEvidenceIntegrityValidator<'a> {
    owner_subject_id: &'a SubjectId,
    markdown_research_execution_id: &'a MarkdownResearchExecutionId,
    markdown_corpus_snapshot_id: &'a MarkdownCorpusSnapshotId,
    requested_answer_composition_styles: &'a [AnswerCompositionStyle],
    markdown_corpus_snapshot: &'a MarkdownCorpusSnapshot,
}

impl<'a> MarkdownSourceEvidenceIntegrityValidator<'a> {
    /// Binds the validator to a frozen execution and revalidates its snapshot.
    pub(crate) fn for_locked_markdown_corpus_snapshot(
        owner_subject_id: &'a SubjectId,
        markdown_research_execution_id: &'a MarkdownResearchExecutionId,
        markdown_corpus_snapshot_id: &'a MarkdownCorpusSnapshotId,
        requested_answer_composition_styles: &'a [AnswerCompositionStyle],
        markdown_corpus_snapshot: &'a MarkdownCorpusSnapshot,
    ) -> Result<Self> {
        if &markdown_corpus_snapshot.owner_subject_id != owner_subject_id
            || &markdown_corpus_snapshot.markdown_corpus_snapshot_id != markdown_corpus_snapshot_id
        {
            return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus });
        }
        validate_requested_answer_composition_styles(requested_answer_composition_styles)?;
        validate_locked_markdown_corpus_snapshot(markdown_corpus_snapshot)?;
        Ok(Self {
            owner_subject_id,
            markdown_research_execution_id,
            markdown_corpus_snapshot_id,
            requested_answer_composition_styles,
            markdown_corpus_snapshot,
        })
    }

    /// Converts one persisted, authorized model candidate into evidence and a citation.
    pub(crate) fn validate_verbatim_source_evidence_candidate(
        &self,
        input: ValidateVerbatimSourceEvidenceCandidateInput<'_>,
    ) -> Result<ValidatedVerbatimSourceEvidence> {
        let read_request = self.validate_authorized_markdown_source_read(
            input.authorized_markdown_source_read,
            input.expected_document_research_branch_task_id,
        )?;
        let candidate_set = input.persisted_verbatim_source_evidence_candidate_set;
        self.validate_candidate_set_envelope(
            candidate_set,
            read_request,
            input.expected_document_research_branch_task_id,
            input.expected_verbatim_source_evidence_extraction_request_id,
        )?;
        let gateway_candidate_set = &candidate_set.verbatim_source_evidence_candidate_set;

        let candidate_occurrences = gateway_candidate_set
            .verbatim_source_evidence_candidates
            .iter()
            .filter(|candidate| *candidate == input.verbatim_source_evidence_candidate)
            .count();
        if candidate_occurrences != 1 {
            return Err(model_rejection(
                "selected evidence candidate is not a unique member of the persisted candidate set",
            ));
        }
        ensure_unique_candidate_coordinates(candidate_set)?;

        let document = self.document(&gateway_candidate_set.markdown_source_document_id)?;
        let segment = self.segment(document, &gateway_candidate_set.markdown_source_segment_id)?;
        let candidate = input.verbatim_source_evidence_candidate;
        let (relative_start, relative_end) = validate_candidate_quote(candidate, segment)?;
        let relative_start = u64::try_from(relative_start)
            .map_err(|_| model_rejection("evidence candidate byte offset is not representable"))?;
        let relative_end = u64::try_from(relative_end)
            .map_err(|_| model_rejection("evidence candidate byte offset is not representable"))?;
        let absolute_start = segment
            .markdown_source_segment_start_byte_offset_in_document
            .checked_add(relative_start)
            .ok_or_else(|| model_rejection("evidence candidate byte offset overflow"))?;
        let absolute_end = segment
            .markdown_source_segment_start_byte_offset_in_document
            .checked_add(relative_end)
            .ok_or_else(|| model_rejection("evidence candidate byte offset overflow"))?;
        if absolute_end > segment.markdown_source_segment_end_byte_offset_in_document {
            return Err(model_rejection(
                "evidence candidate is outside the authorized source segment",
            ));
        }
        let absolute_start_usize = usize::try_from(absolute_start)
            .map_err(|_| model_rejection("evidence candidate byte offset is not representable"))?;
        let absolute_end_usize = usize::try_from(absolute_end)
            .map_err(|_| model_rejection("evidence candidate byte offset is not representable"))?;
        let body = &document.canonical_markdown_document_body;
        if !body.is_char_boundary(absolute_start_usize)
            || !body.is_char_boundary(absolute_end_usize)
            || body.get(absolute_start_usize..absolute_end_usize)
                != Some(candidate.verbatim_source_evidence_quote.as_str())
        {
            return Err(model_rejection(
                "evidence candidate does not exactly match the canonical Markdown body",
            ));
        }

        self.validate_previously_accepted_evidence(
            input.previously_accepted_verbatim_source_evidence,
        )?;
        if input.previously_accepted_verbatim_source_evidence.iter().any(|accepted| {
            &accepted.verbatim_source_evidence.verbatim_source_evidence_id
                == input.program_assigned_ids.verbatim_source_evidence_id
                || &accepted.public_source_citation.public_source_citation_id
                    == input.program_assigned_ids.public_source_citation_id
        }) {
            return Err(RuntimeError::Conflict {
                stage: RuntimeStage::Execution,
                message: "program-assigned evidence or citation ID is already committed".to_owned(),
            });
        }
        if input.previously_accepted_verbatim_source_evidence.iter().any(|accepted| {
            has_same_source_coordinate(
                &accepted.verbatim_source_evidence,
                &gateway_candidate_set.markdown_source_document_id,
                &gateway_candidate_set.markdown_source_segment_id,
                absolute_start,
                absolute_end,
            )
        }) {
            return Err(model_rejection(
                "evidence candidate duplicates an accepted source coordinate",
            ));
        }

        let internal_markdown_source_reference = format!(
            "markdown-source:corpus-snapshot/{}/document-version/{}#source-segment/{}",
            self.markdown_corpus_snapshot_id,
            gateway_candidate_set.markdown_source_document_version_id,
            gateway_candidate_set.markdown_source_segment_id,
        );
        let evidence = VerbatimSourceEvidence {
            verbatim_source_evidence_id: input
                .program_assigned_ids
                .verbatim_source_evidence_id
                .clone(),
            internal_markdown_source_reference,
            verbatim_source_evidence_start_byte_offset: absolute_start,
            verbatim_source_evidence_end_byte_offset: absolute_end,
            verbatim_source_evidence_quote: candidate.verbatim_source_evidence_quote.clone(),
            markdown_source_segment_hash: segment.markdown_source_segment_hash.clone(),
            document_research_branch_task_id: input
                .expected_document_research_branch_task_id
                .clone(),
            markdown_source_document_id: document.markdown_source_document_id.clone(),
            markdown_source_segment_id: segment.markdown_source_segment_id.clone(),
            markdown_research_execution_id: self.markdown_research_execution_id.clone(),
        };
        evidence
            .validate_shape()
            .map_err(|_| model_rejection("evidence candidate violates the evidence schema"))?;
        let citation = PublicSourceCitation {
            public_source_citation_id: input.program_assigned_ids.public_source_citation_id.clone(),
            markdown_source_document_id: document.markdown_source_document_id.clone(),
            markdown_source_document_title: document.markdown_source_document_title.clone(),
            markdown_source_segment_section_heading: segment
                .markdown_source_segment_section_heading
                .clone(),
            public_source_citation_quote: evidence.verbatim_source_evidence_quote.clone(),
            markdown_source_document_version_content_hash: document
                .markdown_source_document_version_content_hash
                .clone(),
        };
        Ok(ValidatedVerbatimSourceEvidence {
            verbatim_source_evidence: evidence,
            public_source_citation: citation,
        })
    }

    /// Revalidates accepted evidence/citation pairs recovered from the Trace.
    ///
    /// The Trace stores the two projections in parallel because the citation is
    /// intentionally a public projection rather than a field on the evidence
    /// value.  This method rebuilds the private validated wrapper only after
    /// checking both vectors against the locked snapshot.
    pub(crate) fn validate_persisted_evidence(
        &self,
        evidence: &[VerbatimSourceEvidence],
        citations: &[PublicSourceCitation],
    ) -> Result<Vec<ValidatedVerbatimSourceEvidence>> {
        if evidence.len() != citations.len() {
            return Err(trace_corrupt("persisted evidence and citation counts differ"));
        }
        let mut validated = Vec::with_capacity(evidence.len());
        for (evidence, citation) in evidence.iter().zip(citations) {
            let candidate = ValidatedVerbatimSourceEvidence {
                verbatim_source_evidence: evidence.clone(),
                public_source_citation: citation.clone(),
            };
            self.validate_previously_accepted_evidence(std::slice::from_ref(&candidate))?;
            validated.push(candidate);
        }
        self.validate_previously_accepted_evidence(&validated)?;
        Ok(validated)
    }

    /// Validates proposed claims against accepted evidence from this execution.
    pub(crate) fn validate_evidence_linked_research_claims(
        &self,
        evidence_linked_research_claims: &[EvidenceLinkedResearchClaim],
        accepted_verbatim_source_evidence: &[ValidatedVerbatimSourceEvidence],
    ) -> Result<()> {
        self.validate_previously_accepted_evidence(accepted_verbatim_source_evidence)?;
        if evidence_linked_research_claims.is_empty() {
            return Err(model_rejection(
                "evidence-linked research claims response must not be empty",
            ));
        }
        let mut claim_ids = BTreeSet::new();
        let accepted_evidence_ids: BTreeSet<_> = accepted_verbatim_source_evidence
            .iter()
            .map(|accepted| accepted.verbatim_source_evidence.verbatim_source_evidence_id.as_str())
            .collect();
        for claim in evidence_linked_research_claims {
            claim.validate_shape().map_err(|_| {
                model_rejection("evidence-linked research claim violates its closed schema")
            })?;
            if &claim.markdown_research_execution_id != self.markdown_research_execution_id
                || !claim_ids.insert(claim.evidence_linked_research_claim_id.as_str())
                || !claim.research_claim_evidence_relationships.iter().all(|relationship| {
                    accepted_evidence_ids
                        .contains(relationship.verbatim_source_evidence_id.as_str())
                })
            {
                return Err(model_rejection(
                    "evidence-linked research claim is not owned by current accepted evidence",
                ));
            }
        }
        Ok(())
    }

    /// Validates a claims-only answer against committed current-execution claims.
    pub(crate) fn validate_evidence_linked_research_claims_answer(
        &self,
        evidence_linked_research_claims_answer: &EvidenceLinkedResearchClaimsAnswer,
        committed_evidence_linked_research_claims: &[EvidenceLinkedResearchClaim],
    ) -> Result<()> {
        validate_committed_claims(
            self.markdown_research_execution_id,
            committed_evidence_linked_research_claims,
        )?;
        evidence_linked_research_claims_answer.validate_shape().map_err(|_| {
            model_rejection("evidence-linked research claims answer violates its closed schema")
        })?;
        if &evidence_linked_research_claims_answer.markdown_research_execution_id
            != self.markdown_research_execution_id
            || evidence_linked_research_claims_answer
                .supporting_evidence_linked_research_claim_ids
                .is_empty()
        {
            return Err(model_rejection("claims answer is not owned by current committed claims"));
        }
        let committed_ids: BTreeSet<_> = committed_evidence_linked_research_claims
            .iter()
            .map(|claim| claim.evidence_linked_research_claim_id.as_str())
            .collect();
        if !evidence_linked_research_claims_answer
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .all(|claim_id| committed_ids.contains(claim_id.as_str()))
        {
            return Err(model_rejection(
                "claims answer references a claim outside the current execution",
            ));
        }
        Ok(())
    }

    /// Validates source types, claims, citations and model-knowledge notices.
    pub(crate) fn validate_source_attributed_answer_composition(
        &self,
        input: ValidateSourceAttributedAnswerCompositionInput<'_>,
    ) -> Result<()> {
        self.validate_previously_accepted_evidence(input.accepted_verbatim_source_evidence)?;
        validate_committed_claims(
            self.markdown_research_execution_id,
            input.committed_evidence_linked_research_claims,
        )?;
        self.validate_evidence_linked_research_claims_answer(
            input.evidence_linked_research_claims_answer,
            input.committed_evidence_linked_research_claims,
        )?;
        input.model_knowledge_only_answer.validate_shape().map_err(|_| {
            model_rejection("model-knowledge-only answer violates its closed schema")
        })?;
        if &input.model_knowledge_only_answer.markdown_research_execution_id
            != self.markdown_research_execution_id
        {
            return Err(model_rejection(
                "model-knowledge-only answer belongs to another execution",
            ));
        }

        let composition = input.source_attributed_answer_composition;
        composition.validate_shape().map_err(|_| {
            model_rejection("source-attributed answer composition violates its closed schema")
        })?;
        if !self
            .requested_answer_composition_styles
            .contains(&composition.source_attributed_answer_composition_style)
            || composition.model_knowledge_only_answer_id
                != input.model_knowledge_only_answer.model_knowledge_only_answer_id
            || composition.evidence_linked_research_claims_answer_id
                != input
                    .evidence_linked_research_claims_answer
                    .evidence_linked_research_claims_answer_id
        {
            return Err(model_rejection(
                "source-attributed answer composition is not bound to its frozen inputs",
            ));
        }

        let claims_answer_ids: BTreeSet<_> = input
            .evidence_linked_research_claims_answer
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .map(|claim_id| claim_id.as_str())
            .collect();
        for segment in &composition.source_attributed_answer_segments {
            self.validate_source_attributed_answer_segment(
                segment,
                &claims_answer_ids,
                input.committed_evidence_linked_research_claims,
                input.accepted_verbatim_source_evidence,
            )?;
        }
        Ok(())
    }

    fn validate_authorized_markdown_source_read<'b>(
        &self,
        authorized_read: PersistedAuthorizedMarkdownSourceRead<'b>,
        expected_branch_task_id: &DocumentResearchBranchTaskId,
    ) -> Result<&'b ResearchDocumentReadRequest> {
        if authorized_read.owner_subject_id != self.owner_subject_id {
            return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Execution });
        }
        let read_request = authorized_read.research_document_read_request;
        if authorized_read.markdown_research_execution_id != self.markdown_research_execution_id
            || authorized_read.markdown_corpus_snapshot_id != self.markdown_corpus_snapshot_id
            || authorized_read.markdown_corpus_snapshot_hash
                != self.markdown_corpus_snapshot.markdown_corpus_snapshot_hash
            || &read_request.document_research_branch_task_id != expected_branch_task_id
            || authorized_read.authorized_markdown_source_segment.markdown_source_document_id
                != &read_request.markdown_source_document_id
            || authorized_read.authorized_markdown_source_segment.markdown_source_segment_id
                != &read_request.markdown_source_segment_id
            || authorized_read.authorized_markdown_source_segment.markdown_source_segment_hash
                != authorized_read.observed_markdown_source_segment_hash
        {
            return Err(trace_corrupt(
                "persisted authorized Markdown source read is internally inconsistent",
            ));
        }
        let document = self.document(&read_request.markdown_source_document_id)?;
        let segment = self.segment(document, &read_request.markdown_source_segment_id)?;
        if authorized_read.authorized_markdown_source_segment.markdown_source_segment_hash
            != segment.markdown_source_segment_hash
            || authorized_read
                .authorized_markdown_source_segment
                .markdown_source_segment_start_byte_offset_in_document
                != segment.markdown_source_segment_start_byte_offset_in_document
            || authorized_read
                .authorized_markdown_source_segment
                .canonical_markdown_source_segment_text
                != segment.canonical_markdown_source_segment_text
        {
            return Err(trace_corrupt(
                "persisted authorized Markdown source read drifted from the locked snapshot",
            ));
        }
        Ok(read_request)
    }

    fn validate_candidate_set_envelope(
        &self,
        candidate_set: &PersistedVerbatimSourceEvidenceCandidateSet,
        read_request: &ResearchDocumentReadRequest,
        expected_branch_task_id: &DocumentResearchBranchTaskId,
        expected_extraction_request_id: &VerbatimSourceEvidenceExtractionRequestId,
    ) -> Result<()> {
        if &candidate_set.owner_subject_id != self.owner_subject_id {
            return Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Execution });
        }
        let gateway_candidate_set = &candidate_set.verbatim_source_evidence_candidate_set;
        let document = self.document(&read_request.markdown_source_document_id)?;
        let segment = self.segment(document, &read_request.markdown_source_segment_id)?;
        if &candidate_set.markdown_research_execution_id != self.markdown_research_execution_id
            || &candidate_set.markdown_corpus_snapshot_id != self.markdown_corpus_snapshot_id
            || candidate_set.markdown_corpus_snapshot_hash
                != self.markdown_corpus_snapshot.markdown_corpus_snapshot_hash
            || candidate_set.research_document_read_request_id
                != read_request.research_document_read_request_id
            || &gateway_candidate_set.document_research_branch_task_id != expected_branch_task_id
            || &gateway_candidate_set.verbatim_source_evidence_extraction_request_id
                != expected_extraction_request_id
            || gateway_candidate_set.markdown_source_document_id
                != read_request.markdown_source_document_id
            || gateway_candidate_set.markdown_source_document_version_id
                != document.markdown_source_document_version_id
            || candidate_set.markdown_source_document_version_content_hash
                != document.markdown_source_document_version_content_hash
            || gateway_candidate_set.markdown_source_segment_id
                != read_request.markdown_source_segment_id
            || gateway_candidate_set.markdown_source_segment_hash
                != segment.markdown_source_segment_hash
            || gateway_candidate_set.verbatim_source_evidence_candidates.is_empty()
        {
            return Err(model_rejection(
                "persisted evidence candidate set is not bound to the active authorized read",
            ));
        }
        Ok(())
    }

    fn validate_previously_accepted_evidence(
        &self,
        accepted_evidence: &[ValidatedVerbatimSourceEvidence],
    ) -> Result<()> {
        let mut evidence_ids = BTreeSet::new();
        let mut citation_ids = BTreeSet::new();
        let mut source_coordinates = BTreeSet::new();
        for accepted in accepted_evidence {
            let evidence = &accepted.verbatim_source_evidence;
            if &evidence.markdown_research_execution_id != self.markdown_research_execution_id {
                return Err(trace_corrupt(
                    "accepted evidence belongs to another Markdown Research Execution",
                ));
            }
            evidence.validate_shape().map_err(|_| {
                trace_corrupt("accepted evidence violates its persisted domain shape")
            })?;
            if !evidence_ids.insert(evidence.verbatim_source_evidence_id.as_str())
                || !citation_ids
                    .insert(accepted.public_source_citation.public_source_citation_id.as_str())
                || !source_coordinates.insert((
                    evidence.markdown_source_document_id.as_str(),
                    evidence.markdown_source_segment_id.as_str(),
                    evidence.verbatim_source_evidence_start_byte_offset,
                    evidence.verbatim_source_evidence_end_byte_offset,
                ))
            {
                return Err(trace_corrupt(
                    "accepted evidence contains a duplicate ID or source coordinate",
                ));
            }
            self.validate_accepted_evidence_source(accepted)?;
        }
        Ok(())
    }

    fn validate_accepted_evidence_source(
        &self,
        accepted: &ValidatedVerbatimSourceEvidence,
    ) -> Result<()> {
        let evidence = &accepted.verbatim_source_evidence;
        let document = self.document(&evidence.markdown_source_document_id)?;
        let segment = self.segment(document, &evidence.markdown_source_segment_id)?;
        let start = usize::try_from(evidence.verbatim_source_evidence_start_byte_offset)
            .map_err(|_| trace_corrupt("accepted evidence offset is not representable"))?;
        let end = usize::try_from(evidence.verbatim_source_evidence_end_byte_offset)
            .map_err(|_| trace_corrupt("accepted evidence offset is not representable"))?;
        let segment_start =
            usize::try_from(segment.markdown_source_segment_start_byte_offset_in_document)
                .map_err(|_| trace_corrupt("source segment offset is not representable"))?;
        let segment_end =
            usize::try_from(segment.markdown_source_segment_end_byte_offset_in_document)
                .map_err(|_| trace_corrupt("source segment offset is not representable"))?;
        let body = &document.canonical_markdown_document_body;
        let expected_internal_reference = format!(
            "markdown-source:corpus-snapshot/{}/document-version/{}#source-segment/{}",
            self.markdown_corpus_snapshot_id,
            document.markdown_source_document_version_id,
            segment.markdown_source_segment_id,
        );
        let citation = &accepted.public_source_citation;
        if start < segment_start
            || end > segment_end
            || !body.is_char_boundary(start)
            || !body.is_char_boundary(end)
            || body.get(start..end) != Some(evidence.verbatim_source_evidence_quote.as_str())
            || evidence.markdown_source_segment_hash != segment.markdown_source_segment_hash
            || evidence.internal_markdown_source_reference != expected_internal_reference
            || citation.markdown_source_document_id != document.markdown_source_document_id
            || citation.markdown_source_document_title != document.markdown_source_document_title
            || citation.markdown_source_segment_section_heading
                != segment.markdown_source_segment_section_heading
            || citation.public_source_citation_quote != evidence.verbatim_source_evidence_quote
            || citation.markdown_source_document_version_content_hash
                != document.markdown_source_document_version_content_hash
        {
            return Err(trace_corrupt(
                "accepted evidence or its public citation drifted from the locked snapshot",
            ));
        }
        Ok(())
    }

    fn validate_source_attributed_answer_segment(
        &self,
        segment: &SourceAttributedAnswerSegment,
        claims_answer_ids: &BTreeSet<&str>,
        committed_claims: &[EvidenceLinkedResearchClaim],
        accepted_evidence: &[ValidatedVerbatimSourceEvidence],
    ) -> Result<()> {
        let has_claims = !segment.supporting_evidence_linked_research_claim_ids.is_empty();
        let has_citations = !segment.supporting_public_source_citation_ids.is_empty();
        let has_required_notice = segment.model_knowledge_unverified_notice.as_deref()
            == Some(MODEL_KNOWLEDGE_UNVERIFIED_NOTICE);
        match segment.source_attributed_answer_segment_source_type {
            SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaims => {
                if !has_claims
                    || !has_citations
                    || segment.model_knowledge_unverified_notice.is_some()
                {
                    return Err(model_rejection(
                        "evidence-only answer segment needs claims and citations but no model notice",
                    ));
                }
            }
            SourceAttributedAnswerSegmentSourceType::ModelKnowledgeOnly => {
                if has_claims || has_citations || !has_required_notice {
                    return Err(model_rejection(
                        "model-only answer segment must have no source references and must disclose unverified model knowledge",
                    ));
                }
                return Ok(());
            }
            SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaimsAndModelKnowledge => {
                if !has_claims || !has_citations || !has_required_notice {
                    return Err(model_rejection(
                        "mixed answer segment needs claims, citations and the unverified model-knowledge notice",
                    ));
                }
            }
        }

        if !segment
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .all(|claim_id| claims_answer_ids.contains(claim_id.as_str()))
        {
            return Err(model_rejection(
                "answer segment references a claim outside the claims-only answer",
            ));
        }
        let referenced_claims: Vec<_> = segment
            .supporting_evidence_linked_research_claim_ids
            .iter()
            .map(|claim_id| {
                committed_claims
                    .iter()
                    .find(|claim| &claim.evidence_linked_research_claim_id == claim_id)
                    .ok_or_else(|| {
                        model_rejection(
                            "answer segment references a claim outside the current execution",
                        )
                    })
            })
            .collect::<Result<_>>()?;
        let cited_evidence: Vec<_> = segment
            .supporting_public_source_citation_ids
            .iter()
            .map(|citation_id| {
                accepted_evidence
                    .iter()
                    .find(|accepted| {
                        &accepted.public_source_citation.public_source_citation_id == citation_id
                    })
                    .ok_or_else(|| {
                        model_rejection(
                            "answer segment references a citation outside the current execution",
                        )
                    })
            })
            .collect::<Result<_>>()?;

        for accepted in &cited_evidence {
            let evidence_id = &accepted.verbatim_source_evidence.verbatim_source_evidence_id;
            if !referenced_claims.iter().any(|claim| {
                claim
                    .research_claim_evidence_relationships
                    .iter()
                    .any(|relationship| &relationship.verbatim_source_evidence_id == evidence_id)
            }) {
                return Err(model_rejection(
                    "answer citation is not traceable through a referenced claim",
                ));
            }
        }
        for claim in &referenced_claims {
            if !cited_evidence.iter().any(|accepted| {
                claim.research_claim_evidence_relationships.iter().any(|relationship| {
                    relationship.verbatim_source_evidence_id
                        == accepted.verbatim_source_evidence.verbatim_source_evidence_id
                })
            }) {
                return Err(model_rejection(
                    "answer claim has no citation to its accepted evidence",
                ));
            }
        }
        Ok(())
    }

    fn document(
        &self,
        document_id: &MarkdownSourceDocumentId,
    ) -> Result<&'a MarkdownSourceDocumentVersion> {
        self.markdown_corpus_snapshot
            .markdown_source_document_versions
            .iter()
            .find(|document| &document.markdown_source_document_id == document_id)
            .ok_or_else(|| {
                trace_corrupt("persisted source reference is absent from locked snapshot")
            })
    }

    fn segment<'b>(
        &self,
        document: &'b MarkdownSourceDocumentVersion,
        segment_id: &MarkdownSourceSegmentId,
    ) -> Result<&'b MarkdownSourceSegment> {
        document
            .markdown_source_segments
            .iter()
            .find(|segment| &segment.markdown_source_segment_id == segment_id)
            .ok_or_else(|| trace_corrupt("persisted source segment is absent from locked snapshot"))
    }
}

#[derive(Serialize)]
struct DocumentVersionHashInput<'a> {
    canonical_markdown_source: &'a str,
    markdown_source_document_id: &'a MarkdownSourceDocumentId,
    markdown_source_document_schema_version: u32,
    markdown_parser_schema_version: u32,
    markdown_canonicalization_schema_version: u32,
}

#[derive(Serialize)]
struct SnapshotHashInput<'a> {
    root_markdown_corpus_navigation_node_id: &'a crate::identity::MarkdownCorpusNavigationNodeId,
    markdown_source_document_versions: &'a [MarkdownSourceDocumentVersion],
    markdown_corpus_navigation_nodes: &'a [crate::corpus::MarkdownCorpusNavigationNode],
    markdown_source_document_schema_version: u32,
    markdown_parser_schema_version: u32,
    markdown_canonicalization_schema_version: u32,
    markdown_corpus_navigation_schema_version: u32,
    markdown_corpus_snapshot_hash_schema_version: u32,
}

fn validate_locked_markdown_corpus_snapshot(snapshot: &MarkdownCorpusSnapshot) -> Result<()> {
    if snapshot.markdown_corpus_navigation_schema_version
        != MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION
        || snapshot.markdown_corpus_snapshot_hash_schema_version
            != MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION
    {
        return Err(corpus_corrupt(
            "locked Markdown Corpus Snapshot uses unsupported schema versions",
        ));
    }
    let mut document_ids = BTreeSet::new();
    let mut version_ids = BTreeSet::new();
    for document in &snapshot.markdown_source_document_versions {
        if !document_ids.insert(document.markdown_source_document_id.as_str())
            || !version_ids.insert(document.markdown_source_document_version_id.as_str())
        {
            return Err(corpus_corrupt(
                "locked Markdown Corpus Snapshot contains duplicate document identities",
            ));
        }
        validate_document_version(document)?;
    }
    let expected_snapshot_hash = canonical_content_hash(&SnapshotHashInput {
        root_markdown_corpus_navigation_node_id: &snapshot.root_markdown_corpus_navigation_node_id,
        markdown_source_document_versions: &snapshot.markdown_source_document_versions,
        markdown_corpus_navigation_nodes: &snapshot.markdown_corpus_navigation_nodes,
        markdown_source_document_schema_version: MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION,
        markdown_parser_schema_version: MARKDOWN_PARSER_SCHEMA_VERSION,
        markdown_canonicalization_schema_version: MARKDOWN_CANONICALIZATION_SCHEMA_VERSION,
        markdown_corpus_navigation_schema_version: MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION,
        markdown_corpus_snapshot_hash_schema_version: MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION,
    })
    .map_err(|_| corpus_corrupt("locked Markdown Corpus Snapshot cannot be canonicalized"))?;
    if snapshot.markdown_corpus_snapshot_hash != expected_snapshot_hash
        || !content_addressed_id_matches(
            snapshot.markdown_corpus_snapshot_id.as_str(),
            "markdown-corpus-snapshot-",
            &expected_snapshot_hash,
        )
    {
        return Err(corpus_corrupt(
            "locked Markdown Corpus Snapshot content hash or identity mismatch",
        ));
    }
    Ok(())
}

fn validate_document_version(document: &MarkdownSourceDocumentVersion) -> Result<()> {
    if document.markdown_source_document_schema_version != MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION
        || document.markdown_parser_schema_version != MARKDOWN_PARSER_SCHEMA_VERSION
        || document.markdown_canonicalization_schema_version
            != MARKDOWN_CANONICALIZATION_SCHEMA_VERSION
    {
        return Err(corpus_corrupt(
            "locked Markdown Source Document Version uses unsupported schemas",
        ));
    }
    let expected_document_hash = canonical_content_hash(&DocumentVersionHashInput {
        canonical_markdown_source: &document.canonical_markdown_source,
        markdown_source_document_id: &document.markdown_source_document_id,
        markdown_source_document_schema_version: document.markdown_source_document_schema_version,
        markdown_parser_schema_version: document.markdown_parser_schema_version,
        markdown_canonicalization_schema_version: document.markdown_canonicalization_schema_version,
    })
    .map_err(|_| corpus_corrupt("locked Markdown Source Document cannot be canonicalized"))?;
    if document.markdown_source_document_version_content_hash != expected_document_hash
        || !content_addressed_id_matches(
            document.markdown_source_document_version_id.as_str(),
            "markdown-source-document-version-",
            &expected_document_hash,
        )
    {
        return Err(corpus_corrupt(
            "locked Markdown Source Document Version content hash or identity mismatch",
        ));
    }

    let body = &document.canonical_markdown_document_body;
    let mut segment_ids = BTreeSet::new();
    let mut previous_end = 0_u64;
    for segment in &document.markdown_source_segments {
        if !segment_ids.insert(segment.markdown_source_segment_id.as_str())
            || segment.markdown_source_segment_start_byte_offset_in_document < previous_end
            || segment.markdown_source_segment_end_byte_offset_in_document
                <= segment.markdown_source_segment_start_byte_offset_in_document
        {
            return Err(corpus_corrupt(
                "locked Markdown Source Document contains duplicate or overlapping segments",
            ));
        }
        let start = usize::try_from(segment.markdown_source_segment_start_byte_offset_in_document)
            .map_err(|_| corpus_corrupt("source segment offset is not representable"))?;
        let end = usize::try_from(segment.markdown_source_segment_end_byte_offset_in_document)
            .map_err(|_| corpus_corrupt("source segment offset is not representable"))?;
        if !body.is_char_boundary(start)
            || !body.is_char_boundary(end)
            || body.get(start..end) != Some(segment.canonical_markdown_source_segment_text.as_str())
            || sha256_content_hash(segment.canonical_markdown_source_segment_text.as_bytes())
                != segment.markdown_source_segment_hash
        {
            return Err(corpus_corrupt(
                "locked Markdown Source Segment hash, text or byte range mismatch",
            ));
        }
        previous_end = segment.markdown_source_segment_end_byte_offset_in_document;
    }
    if document.markdown_source_segments.is_empty() {
        return Err(corpus_corrupt("locked Markdown Source Document has no source segments"));
    }
    Ok(())
}

fn validate_candidate_quote(
    candidate: &VerbatimSourceEvidenceCandidate,
    segment: &MarkdownSourceSegment,
) -> Result<(usize, usize)> {
    let start = usize::try_from(candidate.verbatim_source_evidence_start_byte_offset_in_segment)
        .map_err(|_| model_rejection("evidence candidate byte offset is not representable"))?;
    let end = usize::try_from(candidate.verbatim_source_evidence_end_byte_offset_in_segment)
        .map_err(|_| model_rejection("evidence candidate byte offset is not representable"))?;
    let segment_text = &segment.canonical_markdown_source_segment_text;
    if end <= start
        || !segment_text.is_char_boundary(start)
        || !segment_text.is_char_boundary(end)
        || segment_text.get(start..end) != Some(candidate.verbatim_source_evidence_quote.as_str())
    {
        return Err(model_rejection(
            "evidence candidate byte range is not an exact UTF-8 source match",
        ));
    }
    Ok((start, end))
}

fn ensure_unique_candidate_coordinates(
    candidate_set: &PersistedVerbatimSourceEvidenceCandidateSet,
) -> Result<()> {
    let mut coordinates = BTreeSet::new();
    for candidate in
        &candidate_set.verbatim_source_evidence_candidate_set.verbatim_source_evidence_candidates
    {
        let coordinate = (
            candidate.verbatim_source_evidence_start_byte_offset_in_segment,
            candidate.verbatim_source_evidence_end_byte_offset_in_segment,
        );
        if !coordinates.insert(coordinate) {
            return Err(model_rejection(
                "persisted evidence candidate set contains duplicate source coordinates",
            ));
        }
    }
    Ok(())
}

fn validate_committed_claims(
    expected_execution_id: &MarkdownResearchExecutionId,
    committed_claims: &[EvidenceLinkedResearchClaim],
) -> Result<()> {
    if committed_claims.is_empty() {
        return Err(trace_corrupt(
            "claims-only answer requires committed evidence-linked research claims",
        ));
    }
    let mut claim_ids = BTreeSet::new();
    for claim in committed_claims {
        if &claim.markdown_research_execution_id != expected_execution_id
            || !claim_ids.insert(claim.evidence_linked_research_claim_id.as_str())
            || claim.validate_shape().is_err()
        {
            return Err(trace_corrupt(
                "committed evidence-linked research claims are internally inconsistent",
            ));
        }
    }
    Ok(())
}

fn validate_requested_answer_composition_styles(
    requested_styles: &[AnswerCompositionStyle],
) -> Result<()> {
    if requested_styles.is_empty() || requested_styles.len() > 2 {
        return Err(trace_corrupt(
            "frozen execution contains an invalid answer composition style set",
        ));
    }
    let unique: BTreeSet<_> = requested_styles.iter().copied().collect();
    if unique.len() != requested_styles.len()
        || requested_styles.windows(2).any(|styles| styles[0] > styles[1])
    {
        return Err(trace_corrupt("frozen answer composition styles are not sorted and unique"));
    }
    Ok(())
}

fn has_same_source_coordinate(
    evidence: &VerbatimSourceEvidence,
    document_id: &MarkdownSourceDocumentId,
    segment_id: &MarkdownSourceSegmentId,
    start: u64,
    end: u64,
) -> bool {
    &evidence.markdown_source_document_id == document_id
        && &evidence.markdown_source_segment_id == segment_id
        && evidence.verbatim_source_evidence_start_byte_offset == start
        && evidence.verbatim_source_evidence_end_byte_offset == end
}

fn content_addressed_id_matches(id: &str, prefix: &str, content_hash: &str) -> bool {
    let Some(digest) = content_hash.strip_prefix("sha256:") else {
        return false;
    };
    digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && digest.get(..32).is_some_and(|digest_prefix| id == format!("{prefix}{digest_prefix}"))
}

fn model_rejection(message: &str) -> RuntimeError {
    RuntimeError::ModelResponse { message: message.to_owned() }
}

fn corpus_corrupt(message: &str) -> RuntimeError {
    RuntimeError::CorruptState { stage: RuntimeStage::Corpus, message: message.to_owned() }
}

fn trace_corrupt(message: &str) -> RuntimeError {
    RuntimeError::CorruptState { stage: RuntimeStage::Trace, message: message.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{
        MarkdownCorpusNavigationNodeInput, MarkdownSourceDocumentInput,
        PublishMarkdownCorpusSnapshotInput, build_markdown_corpus_snapshot,
    };
    use crate::domain::{
        ANSWER_PROJECTION_SCHEMA_VERSION, EvidenceLinkedResearchClaimCitationStatus,
        ResearchClaimEvidenceRelationship, ResearchClaimEvidenceRelationshipType,
    };
    use crate::identity::{
        EvidenceLinkedResearchClaimId, MarkdownCorpusNavigationNodeId, MarkdownResearchModelTaskId,
        MarkdownSourceDocumentVersionId,
    };
    use chrono::{TimeZone, Utc};

    static REQUESTED_STYLES: [AnswerCompositionStyle; 1] =
        [AnswerCompositionStyle::ModelKnowledgeLed];

    struct Fixture {
        owner_subject_id: SubjectId,
        markdown_research_execution_id: MarkdownResearchExecutionId,
        document_research_branch_task_id: DocumentResearchBranchTaskId,
        extraction_request_id: VerbatimSourceEvidenceExtractionRequestId,
        snapshot: MarkdownCorpusSnapshot,
        document_id: MarkdownSourceDocumentId,
        segment_id: MarkdownSourceSegmentId,
        read_request: ResearchDocumentReadRequest,
    }

    impl Fixture {
        fn validator(&self) -> MarkdownSourceEvidenceIntegrityValidator<'_> {
            MarkdownSourceEvidenceIntegrityValidator::for_locked_markdown_corpus_snapshot(
                &self.owner_subject_id,
                &self.markdown_research_execution_id,
                &self.snapshot.markdown_corpus_snapshot_id,
                &REQUESTED_STYLES,
                &self.snapshot,
            )
            .unwrap()
        }

        fn segment(&self) -> &MarkdownSourceSegment {
            self.snapshot
                .markdown_source_document_versions
                .iter()
                .find(|document| document.markdown_source_document_id == self.document_id)
                .unwrap()
                .markdown_source_segments
                .iter()
                .find(|segment| segment.markdown_source_segment_id == self.segment_id)
                .unwrap()
        }

        fn candidate(&self, quote: &str, relative_start: usize) -> VerbatimSourceEvidenceCandidate {
            VerbatimSourceEvidenceCandidate {
                verbatim_source_evidence_start_byte_offset_in_segment: relative_start as u64,
                verbatim_source_evidence_end_byte_offset_in_segment: (relative_start + quote.len())
                    as u64,
                verbatim_source_evidence_quote: quote.to_owned(),
            }
        }

        fn candidate_set(
            &self,
            candidates: Vec<VerbatimSourceEvidenceCandidate>,
        ) -> PersistedVerbatimSourceEvidenceCandidateSet {
            let document = &self.snapshot.markdown_source_document_versions[0];
            PersistedVerbatimSourceEvidenceCandidateSet {
                owner_subject_id: self.owner_subject_id.clone(),
                markdown_research_execution_id: self.markdown_research_execution_id.clone(),
                markdown_corpus_snapshot_id: self.snapshot.markdown_corpus_snapshot_id.clone(),
                markdown_corpus_snapshot_hash: self.snapshot.markdown_corpus_snapshot_hash.clone(),
                research_document_read_request_id: self
                    .read_request
                    .research_document_read_request_id
                    .clone(),
                markdown_source_document_version_content_hash: document
                    .markdown_source_document_version_content_hash
                    .clone(),
                verbatim_source_evidence_candidate_set: VerbatimSourceEvidenceCandidateSet {
                    verbatim_source_evidence_extraction_request_id: self
                        .extraction_request_id
                        .clone(),
                    document_research_branch_task_id: self.document_research_branch_task_id.clone(),
                    markdown_source_document_id: self.document_id.clone(),
                    markdown_source_document_version_id: document
                        .markdown_source_document_version_id
                        .clone(),
                    markdown_source_segment_id: self.segment_id.clone(),
                    markdown_source_segment_hash: self
                        .segment()
                        .markdown_source_segment_hash
                        .clone(),
                    verbatim_source_evidence_candidates: candidates,
                },
            }
        }

        fn validate_candidate(
            &self,
            candidate_set: &PersistedVerbatimSourceEvidenceCandidateSet,
            candidate: &VerbatimSourceEvidenceCandidate,
            evidence_id: &str,
            citation_id: &str,
            accepted: &[ValidatedVerbatimSourceEvidence],
        ) -> Result<ValidatedVerbatimSourceEvidence> {
            let authorized_segment = self
                .snapshot
                .reader()
                .read_authorized_markdown_source_segment(&self.document_id, &self.segment_id)
                .unwrap();
            let evidence_id = VerbatimSourceEvidenceId::from_value(evidence_id).unwrap();
            let citation_id = PublicSourceCitationId::from_value(citation_id).unwrap();
            self.validator().validate_verbatim_source_evidence_candidate(
                ValidateVerbatimSourceEvidenceCandidateInput {
                    expected_document_research_branch_task_id: &self
                        .document_research_branch_task_id,
                    expected_verbatim_source_evidence_extraction_request_id: &self
                        .extraction_request_id,
                    authorized_markdown_source_read: PersistedAuthorizedMarkdownSourceRead {
                        owner_subject_id: &self.owner_subject_id,
                        markdown_research_execution_id: &self.markdown_research_execution_id,
                        markdown_corpus_snapshot_id: &self.snapshot.markdown_corpus_snapshot_id,
                        markdown_corpus_snapshot_hash: &self.snapshot.markdown_corpus_snapshot_hash,
                        research_document_read_request: &self.read_request,
                        authorized_markdown_source_segment: authorized_segment,
                        observed_markdown_source_segment_hash: &self
                            .segment()
                            .markdown_source_segment_hash,
                    },
                    persisted_verbatim_source_evidence_candidate_set: candidate_set,
                    verbatim_source_evidence_candidate: candidate,
                    program_assigned_ids: ProgramAssignedVerbatimSourceEvidenceIds {
                        verbatim_source_evidence_id: &evidence_id,
                        public_source_citation_id: &citation_id,
                    },
                    previously_accepted_verbatim_source_evidence: accepted,
                },
            )
        }
    }

    fn fixture() -> Fixture {
        let owner_subject_id = SubjectId::from_value("subject-1").unwrap();
        let document_id = MarkdownSourceDocumentId::from_value("markdown-doc-1").unwrap();
        let root_id = MarkdownCorpusNavigationNodeId::from_value("root").unwrap();
        let source = format!(
            "---\nmarkdown_source_document_id: {}\n---\n\n# UTF-8 规则\n\n摘要。\n\n## 原文\n\n前缀🙂重复依据；中间；重复依据。\n",
            document_id
        );
        let snapshot = build_markdown_corpus_snapshot(
            owner_subject_id.clone(),
            PublishMarkdownCorpusSnapshotInput {
                markdown_source_documents: vec![MarkdownSourceDocumentInput {
                    relative_path: "rules.md".to_owned(),
                    markdown_source_bytes: source.into_bytes(),
                }],
                markdown_corpus_navigation_nodes: vec![MarkdownCorpusNavigationNodeInput {
                    markdown_corpus_navigation_node_id: root_id.clone(),
                    markdown_corpus_navigation_node_label: "root".to_owned(),
                    markdown_corpus_navigation_node_summary: "rules".to_owned(),
                    child_markdown_corpus_navigation_node_ids: Vec::new(),
                    linked_markdown_source_document_ids: vec![document_id.clone()],
                }],
                root_markdown_corpus_navigation_node_id: root_id,
            },
            Utc.with_ymd_and_hms(2026, 7, 18, 0, 0, 0).unwrap(),
        )
        .unwrap();
        let segment_id = snapshot.markdown_source_document_versions[0]
            .markdown_source_segments
            .iter()
            .find(|segment| segment.canonical_markdown_source_segment_text.contains("重复依据"))
            .unwrap()
            .markdown_source_segment_id
            .clone();
        let branch_id = DocumentResearchBranchTaskId::from_value("branch-1").unwrap();
        let read_request = ResearchDocumentReadRequest {
            research_document_read_request_id: ResearchDocumentReadRequestId::from_value("read-1")
                .unwrap(),
            document_research_branch_task_id: branch_id.clone(),
            markdown_source_document_id: document_id.clone(),
            markdown_source_segment_id: segment_id.clone(),
            unresolved_research_question: "规则是什么".to_owned(),
            expected_research_information_to_resolve_question: "逐字规则".to_owned(),
            markdown_source_document_selection_explanation: "相关".to_owned(),
        };
        Fixture {
            owner_subject_id,
            markdown_research_execution_id: MarkdownResearchExecutionId::from_value("execution-1")
                .unwrap(),
            document_research_branch_task_id: branch_id,
            extraction_request_id: VerbatimSourceEvidenceExtractionRequestId::from_value(
                "extract-1",
            )
            .unwrap(),
            snapshot,
            document_id,
            segment_id,
            read_request,
        }
    }

    fn claim(
        fixture: &Fixture,
        claim_id: &str,
        evidence_id: &VerbatimSourceEvidenceId,
    ) -> EvidenceLinkedResearchClaim {
        EvidenceLinkedResearchClaim {
            evidence_linked_research_claim_id: EvidenceLinkedResearchClaimId::from_value(claim_id)
                .unwrap(),
            evidence_linked_research_claim_text: "原文限定了该规则。".to_owned(),
            research_claim_evidence_relationships: vec![ResearchClaimEvidenceRelationship {
                verbatim_source_evidence_id: evidence_id.clone(),
                research_claim_evidence_relationship_type:
                    ResearchClaimEvidenceRelationshipType::QualifiesEvidenceLinkedResearchClaim,
            }],
            evidence_linked_research_claim_applicability_conditions: Vec::new(),
            evidence_linked_research_claim_exceptions: Vec::new(),
            evidence_linked_research_claim_citation_status:
                EvidenceLinkedResearchClaimCitationStatus::AllCitationsLinkedToVerbatimSourceEvidence,
            markdown_research_execution_id: fixture.markdown_research_execution_id.clone(),
        }
    }

    fn composition_input<'a>(
        model_answer: &'a ModelKnowledgeOnlyAnswer,
        claims_answer: &'a EvidenceLinkedResearchClaimsAnswer,
        claims: &'a [EvidenceLinkedResearchClaim],
        accepted_evidence: &'a [ValidatedVerbatimSourceEvidence],
        composition: &'a SourceAttributedAnswerComposition,
    ) -> ValidateSourceAttributedAnswerCompositionInput<'a> {
        ValidateSourceAttributedAnswerCompositionInput {
            model_knowledge_only_answer: model_answer,
            evidence_linked_research_claims_answer: claims_answer,
            committed_evidence_linked_research_claims: claims,
            accepted_verbatim_source_evidence: accepted_evidence,
            source_attributed_answer_composition: composition,
        }
    }

    #[test]
    fn accepts_multibyte_exact_match_and_distinguishes_repeated_quotes() {
        let fixture = fixture();
        let text = &fixture.segment().canonical_markdown_source_segment_text;
        let first_start = text.find("重复依据").unwrap();
        let second_start = text.rfind("重复依据").unwrap();
        let first_candidate = fixture.candidate("重复依据", first_start);
        let second_candidate = fixture.candidate("重复依据", second_start);
        let candidate_set =
            fixture.candidate_set(vec![first_candidate.clone(), second_candidate.clone()]);

        let first = fixture
            .validate_candidate(&candidate_set, &first_candidate, "evidence-1", "citation-1", &[])
            .unwrap();
        let second = fixture
            .validate_candidate(
                &candidate_set,
                &second_candidate,
                "evidence-2",
                "citation-2",
                std::slice::from_ref(&first),
            )
            .unwrap();
        assert_eq!(
            second.verbatim_source_evidence.verbatim_source_evidence_start_byte_offset,
            fixture.segment().markdown_source_segment_start_byte_offset_in_document
                + second_start as u64
        );
        assert_eq!(second.public_source_citation.public_source_citation_quote, "重复依据");
        assert!(
            fixture
                .validate_candidate(
                    &candidate_set,
                    &first_candidate,
                    "evidence-3",
                    "citation-3",
                    &[first],
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_non_character_boundaries_and_off_by_one_ranges() {
        let fixture = fixture();
        let text = &fixture.segment().canonical_markdown_source_segment_text;
        let emoji_start = text.find('🙂').unwrap();
        let mut non_boundary = fixture.candidate("🙂", emoji_start);
        non_boundary.verbatim_source_evidence_start_byte_offset_in_segment += 1;
        let set = fixture.candidate_set(vec![non_boundary.clone()]);
        assert!(
            fixture
                .validate_candidate(&set, &non_boundary, "evidence-1", "citation-1", &[])
                .is_err()
        );

        let quote_start = text.find("重复依据").unwrap();
        let mut off_by_one = fixture.candidate("重复依据", quote_start);
        off_by_one.verbatim_source_evidence_end_byte_offset_in_segment += 1;
        let set = fixture.candidate_set(vec![off_by_one.clone()]);
        assert!(
            fixture.validate_candidate(&set, &off_by_one, "evidence-2", "citation-2", &[]).is_err()
        );
    }

    #[test]
    fn rejects_candidates_absent_from_or_duplicated_in_the_persisted_set() {
        let fixture = fixture();
        let text = &fixture.segment().canonical_markdown_source_segment_text;
        let first_start = text.find("重复依据").unwrap();
        let second_start = text.rfind("重复依据").unwrap();
        let persisted = fixture.candidate("重复依据", first_start);
        let absent = fixture.candidate("重复依据", second_start);
        let candidate_set = fixture.candidate_set(vec![persisted.clone()]);
        assert!(
            fixture
                .validate_candidate(&candidate_set, &absent, "evidence-1", "citation-1", &[],)
                .is_err()
        );

        let duplicate_set = fixture.candidate_set(vec![persisted.clone(), persisted.clone()]);
        assert!(
            fixture
                .validate_candidate(&duplicate_set, &persisted, "evidence-2", "citation-2", &[],)
                .is_err()
        );
    }

    #[test]
    fn rejects_hash_drift_and_self_inconsistent_snapshot_as_different_error_classes() {
        let fixture = fixture();
        let start =
            fixture.segment().canonical_markdown_source_segment_text.find("重复依据").unwrap();
        let candidate = fixture.candidate("重复依据", start);
        let mut set = fixture.candidate_set(vec![candidate.clone()]);
        set.verbatim_source_evidence_candidate_set.markdown_source_segment_hash =
            "sha256:model-drift".to_owned();
        let error = fixture
            .validate_candidate(&set, &candidate, "evidence-1", "citation-1", &[])
            .unwrap_err();
        assert!(matches!(error, RuntimeError::ModelResponse { .. }));

        let mut snapshot = fixture.snapshot.clone();
        snapshot.markdown_source_document_versions[0].markdown_source_segments[0]
            .markdown_source_segment_hash = "sha256:stored-drift".to_owned();
        let error = MarkdownSourceEvidenceIntegrityValidator::for_locked_markdown_corpus_snapshot(
            &fixture.owner_subject_id,
            &fixture.markdown_research_execution_id,
            &snapshot.markdown_corpus_snapshot_id,
            &[AnswerCompositionStyle::ModelKnowledgeLed],
            &snapshot,
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::CorruptState { stage: RuntimeStage::Corpus, .. }));
    }

    #[test]
    fn rejects_cross_owner_execution_branch_snapshot_document_and_segment() {
        let fixture = fixture();
        let start =
            fixture.segment().canonical_markdown_source_segment_text.find("重复依据").unwrap();
        let candidate = fixture.candidate("重复依据", start);
        let base = fixture.candidate_set(vec![candidate.clone()]);

        let mut mutations = Vec::new();
        let mut foreign_owner = base.clone();
        foreign_owner.owner_subject_id = SubjectId::from_value("subject-2").unwrap();
        mutations.push(foreign_owner);
        let mut foreign_execution = base.clone();
        foreign_execution.markdown_research_execution_id =
            MarkdownResearchExecutionId::from_value("execution-2").unwrap();
        mutations.push(foreign_execution);
        let mut foreign_branch = base.clone();
        foreign_branch.verbatim_source_evidence_candidate_set.document_research_branch_task_id =
            DocumentResearchBranchTaskId::from_value("branch-2").unwrap();
        mutations.push(foreign_branch);
        let mut foreign_snapshot = base.clone();
        foreign_snapshot.markdown_corpus_snapshot_id =
            MarkdownCorpusSnapshotId::from_value("snapshot-2").unwrap();
        mutations.push(foreign_snapshot);
        let mut foreign_snapshot_hash = base.clone();
        foreign_snapshot_hash.markdown_corpus_snapshot_hash = "sha256:foreign".to_owned();
        mutations.push(foreign_snapshot_hash);
        let mut foreign_document = base.clone();
        foreign_document.verbatim_source_evidence_candidate_set.markdown_source_document_id =
            MarkdownSourceDocumentId::from_value("document-2").unwrap();
        mutations.push(foreign_document);
        let mut foreign_document_version = base.clone();
        foreign_document_version
            .verbatim_source_evidence_candidate_set
            .markdown_source_document_version_id =
            MarkdownSourceDocumentVersionId::from_value("document-version-2").unwrap();
        mutations.push(foreign_document_version);
        let mut foreign_document_hash = base.clone();
        foreign_document_hash.markdown_source_document_version_content_hash =
            "sha256:foreign".to_owned();
        mutations.push(foreign_document_hash);
        let mut foreign_segment = base.clone();
        foreign_segment.verbatim_source_evidence_candidate_set.markdown_source_segment_id =
            MarkdownSourceSegmentId::from_value("segment-2").unwrap();
        mutations.push(foreign_segment);
        let mut foreign_segment_hash = base.clone();
        foreign_segment_hash.verbatim_source_evidence_candidate_set.markdown_source_segment_hash =
            "sha256:foreign".to_owned();
        mutations.push(foreign_segment_hash);

        for candidate_set in mutations {
            assert!(
                fixture
                    .validate_candidate(
                        &candidate_set,
                        &candidate,
                        "evidence-1",
                        "citation-1",
                        &[],
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn claims_and_claims_answer_only_reference_current_committed_objects() {
        let fixture = fixture();
        let start =
            fixture.segment().canonical_markdown_source_segment_text.find("重复依据").unwrap();
        let candidate = fixture.candidate("重复依据", start);
        let set = fixture.candidate_set(vec![candidate.clone()]);
        let accepted =
            fixture.validate_candidate(&set, &candidate, "evidence-1", "citation-1", &[]).unwrap();
        let claim = claim(
            &fixture,
            "claim-1",
            &accepted.verbatim_source_evidence.verbatim_source_evidence_id,
        );
        let validator = fixture.validator();
        assert!(
            validator
                .validate_evidence_linked_research_claims(
                    std::slice::from_ref(&claim),
                    std::slice::from_ref(&accepted),
                )
                .is_ok()
        );

        let mut foreign_relationship = claim.clone();
        foreign_relationship.research_claim_evidence_relationships[0].verbatim_source_evidence_id =
            VerbatimSourceEvidenceId::from_value("foreign-evidence").unwrap();
        assert!(
            validator
                .validate_evidence_linked_research_claims(
                    &[foreign_relationship],
                    std::slice::from_ref(&accepted),
                )
                .is_err()
        );

        let answer_id = MarkdownResearchModelTaskId::from_value("claims-answer-1").unwrap();
        let answer = EvidenceLinkedResearchClaimsAnswer {
            evidence_linked_research_claims_answer_id: answer_id,
            evidence_linked_research_claims_answer_text: "该规则受到限定。".to_owned(),
            supporting_evidence_linked_research_claim_ids: vec![
                claim.evidence_linked_research_claim_id.clone(),
            ],
            markdown_research_execution_id: fixture.markdown_research_execution_id.clone(),
        };
        assert!(
            validator
                .validate_evidence_linked_research_claims_answer(
                    &answer,
                    std::slice::from_ref(&claim),
                )
                .is_ok()
        );
        let mut foreign_answer = answer;
        foreign_answer.supporting_evidence_linked_research_claim_ids =
            vec![EvidenceLinkedResearchClaimId::from_value("foreign-claim").unwrap()];
        assert!(
            validator
                .validate_evidence_linked_research_claims_answer(
                    &foreign_answer,
                    std::slice::from_ref(&claim),
                )
                .is_err()
        );
    }

    #[test]
    fn composition_enforces_source_type_notice_and_citation_traceability() {
        let fixture = fixture();
        let start =
            fixture.segment().canonical_markdown_source_segment_text.find("重复依据").unwrap();
        let candidate = fixture.candidate("重复依据", start);
        let set = fixture.candidate_set(vec![candidate.clone()]);
        let accepted =
            fixture.validate_candidate(&set, &candidate, "evidence-1", "citation-1", &[]).unwrap();
        let claim = claim(
            &fixture,
            "claim-1",
            &accepted.verbatim_source_evidence.verbatim_source_evidence_id,
        );
        let model_answer = ModelKnowledgeOnlyAnswer {
            model_knowledge_only_answer_id: MarkdownResearchModelTaskId::from_value(
                "model-answer-1",
            )
            .unwrap(),
            model_knowledge_only_answer_text: "模型背景。".to_owned(),
            markdown_research_execution_id: fixture.markdown_research_execution_id.clone(),
        };
        let claims_answer = EvidenceLinkedResearchClaimsAnswer {
            evidence_linked_research_claims_answer_id: MarkdownResearchModelTaskId::from_value(
                "claims-answer-1",
            )
            .unwrap(),
            evidence_linked_research_claims_answer_text: "证据结论。".to_owned(),
            supporting_evidence_linked_research_claim_ids: vec![
                claim.evidence_linked_research_claim_id.clone(),
            ],
            markdown_research_execution_id: fixture.markdown_research_execution_id.clone(),
        };
        let evidence_segment = SourceAttributedAnswerSegment {
            source_attributed_answer_segment_text: "证据结论。".to_owned(),
            source_attributed_answer_segment_source_type:
                SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaims,
            supporting_evidence_linked_research_claim_ids: vec![
                claim.evidence_linked_research_claim_id.clone(),
            ],
            supporting_public_source_citation_ids: vec![
                accepted.public_source_citation.public_source_citation_id.clone(),
            ],
            model_knowledge_unverified_notice: None,
        };
        let model_segment = SourceAttributedAnswerSegment {
            source_attributed_answer_segment_text: "模型背景。".to_owned(),
            source_attributed_answer_segment_source_type:
                SourceAttributedAnswerSegmentSourceType::ModelKnowledgeOnly,
            supporting_evidence_linked_research_claim_ids: Vec::new(),
            supporting_public_source_citation_ids: Vec::new(),
            model_knowledge_unverified_notice: Some(MODEL_KNOWLEDGE_UNVERIFIED_NOTICE.to_owned()),
        };
        let composition = SourceAttributedAnswerComposition {
            source_attributed_answer_composition_style: AnswerCompositionStyle::ModelKnowledgeLed,
            model_knowledge_only_answer_id: model_answer.model_knowledge_only_answer_id.clone(),
            evidence_linked_research_claims_answer_id: claims_answer
                .evidence_linked_research_claims_answer_id
                .clone(),
            source_attributed_answer_segments: vec![
                evidence_segment.clone(),
                model_segment.clone(),
            ],
            source_attributed_answer_composition_review_reason: "来源清晰。".to_owned(),
            answer_projection_schema_version: ANSWER_PROJECTION_SCHEMA_VERSION,
        };
        let validator = fixture.validator();
        let committed_claims = vec![claim];
        let accepted_evidence = vec![accepted.clone()];
        assert!(
            validator
                .validate_source_attributed_answer_composition(composition_input(
                    &model_answer,
                    &claims_answer,
                    &committed_claims,
                    &accepted_evidence,
                    &composition,
                ))
                .is_ok()
        );

        let mut model_with_citation = composition.clone();
        model_with_citation.source_attributed_answer_segments[1]
            .supporting_public_source_citation_ids =
            vec![accepted.public_source_citation.public_source_citation_id.clone()];
        assert!(
            validator
                .validate_source_attributed_answer_composition(composition_input(
                    &model_answer,
                    &claims_answer,
                    &committed_claims,
                    &accepted_evidence,
                    &model_with_citation,
                ))
                .is_err()
        );

        let mut valid_mixed = composition.clone();
        valid_mixed.source_attributed_answer_segments[0]
            .source_attributed_answer_segment_source_type =
            SourceAttributedAnswerSegmentSourceType::EvidenceLinkedResearchClaimsAndModelKnowledge;
        valid_mixed.source_attributed_answer_segments[0].model_knowledge_unverified_notice =
            Some(MODEL_KNOWLEDGE_UNVERIFIED_NOTICE.to_owned());
        assert!(
            validator
                .validate_source_attributed_answer_composition(composition_input(
                    &model_answer,
                    &claims_answer,
                    &committed_claims,
                    &accepted_evidence,
                    &valid_mixed,
                ))
                .is_ok()
        );

        let mut mixed_without_notice = valid_mixed;
        mixed_without_notice.source_attributed_answer_segments[0]
            .model_knowledge_unverified_notice = None;
        assert!(
            validator
                .validate_source_attributed_answer_composition(composition_input(
                    &model_answer,
                    &claims_answer,
                    &committed_claims,
                    &accepted_evidence,
                    &mixed_without_notice,
                ))
                .is_err()
        );

        let mut unknown_citation = composition;
        unknown_citation.source_attributed_answer_segments[0]
            .supporting_public_source_citation_ids =
            vec![PublicSourceCitationId::from_value("foreign-citation").unwrap()];
        assert!(
            validator
                .validate_source_attributed_answer_composition(composition_input(
                    &model_answer,
                    &claims_answer,
                    &committed_claims,
                    &accepted_evidence,
                    &unknown_citation,
                ))
                .is_err()
        );
    }
}

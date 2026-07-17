//! Fixed-round Explore orchestration and pure strong-model output validation.

use std::{collections::HashSet, future::Future, io::Write, path::Path};

use serde::Deserialize;
use url::Url;

use crate::{
    CompletedTurnContext, ComposedResearchAnswer, ComposedResearchClaim, FrozenResearchBrief,
    ModelKnowledgeDraft, ResearchAnswerComparison, ResearchAnswerStyle, ResearchClaimOrigin,
    ResearchError, ResearchStage, Result, RunReplay, SearchQuery, SearchResult, Snapshot,
    SnapshotNavigationExcerpt, SnapshotReader, SnapshotRef, SnapshotWriter, SourceSelection,
    TraceEvent, TracePolicy, TraceWriter, validate_decision_rationale,
};

pub const MIN_EXPLORE_ROUNDS: u32 = 3;
pub const DEFAULT_EXPLORE_ROUNDS: u32 = 3;
pub const MAX_EXPLORE_ROUNDS: u32 = 5;
pub const QUERIES_PER_ROUND: usize = 3;
pub const MAX_STRONG_INPUT_TOKENS: usize = 1_000_000;
pub const MAX_SNAPSHOTS: usize = 300;
pub const MAX_READ_SNAPSHOTS: usize = 100;
const MAX_QUERY_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchRunStopReason {
    CompletedRounds,
    InputBudget,
    SnapshotLimit,
    NoNewUrls,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResearchRunProgress {
    pub round: u32,
    pub estimated_input_tokens: usize,
    pub archived_snapshots: usize,
    pub stop_reason: Option<ResearchRunStopReason>,
}

/// Test seam for the three external effects. Implementations remain sequential.
pub trait ResearchExecutionBackend {
    fn generate_model_knowledge_draft(
        &mut self,
        brief: &FrozenResearchBrief,
        conversation_context: &[CompletedTurnContext],
    ) -> impl Future<Output = Result<String>>;

    fn generate_search_queries(
        &mut self,
        brief: &FrozenResearchBrief,
        conversation_context: &[CompletedTurnContext],
        captured_snapshots: &[Snapshot],
        previous_queries: &[String],
    ) -> impl Future<Output = Result<String>>;

    fn search_web(&mut self, query: &str) -> impl Future<Output = Result<Vec<SearchResult>>>;

    fn capture_web_snapshot(&mut self, url: &str) -> impl Future<Output = Result<Snapshot>>;

    fn select_evidence_snapshots(
        &mut self,
        brief: &FrozenResearchBrief,
        conversation_context: &[CompletedTurnContext],
        excerpts: &[SnapshotNavigationExcerpt],
    ) -> impl Future<Output = Result<String>>;

    fn synthesize_composed_answer(
        &mut self,
        brief: &FrozenResearchBrief,
        conversation_context: &[CompletedTurnContext],
        snapshots: &[Snapshot],
        knowledge_draft: &ModelKnowledgeDraft,
        answer_style: ResearchAnswerStyle,
    ) -> impl Future<Output = Result<String>>;
}

#[derive(Debug)]
pub struct EvidenceSource {
    pub snapshot_ref: SnapshotRef,
    pub url: String,
    pub title: String,
}

#[derive(Debug)]
pub struct ResearchRunOutput {
    pub answer: ComposedResearchAnswer,
    pub knowledge_draft: ModelKnowledgeDraft,
    pub answer_style: ResearchAnswerStyle,
    pub sources: Vec<EvidenceSource>,
}

pub struct ResearchRunExecutor<B, W: Write> {
    brief: FrozenResearchBrief,
    policy: TracePolicy,
    answer_style: ResearchAnswerStyle,
    execution_backend: B,
    snapshot_writer: SnapshotWriter,
    trace_writer: TraceWriter<W>,
    progress: ResearchRunProgress,
    captured_snapshots: Vec<Snapshot>,
    previous_queries: Vec<String>,
    captured_page_urls: HashSet<String>,
    captured_snapshot_refs: HashSet<SnapshotRef>,
    conversation_context: Vec<CompletedTurnContext>,
    model_knowledge_draft: Option<ModelKnowledgeDraft>,
}

impl<B: ResearchExecutionBackend, W: Write> ResearchRunExecutor<B, W> {
    #[must_use]
    pub fn new(
        brief: FrozenResearchBrief,
        policy: TracePolicy,
        answer_style: ResearchAnswerStyle,
        execution_backend: B,
        snapshot_writer: SnapshotWriter,
        trace_writer: TraceWriter<W>,
    ) -> Self {
        Self {
            brief,
            policy,
            answer_style,
            execution_backend,
            snapshot_writer,
            trace_writer,
            progress: ResearchRunProgress::default(),
            captured_snapshots: Vec::new(),
            previous_queries: Vec::new(),
            captured_page_urls: HashSet::new(),
            captured_snapshot_refs: HashSet::new(),
            conversation_context: Vec::new(),
            model_knowledge_draft: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resume(
        brief: FrozenResearchBrief,
        policy: TracePolicy,
        answer_style: ResearchAnswerStyle,
        execution_backend: B,
        snapshot_writer: SnapshotWriter,
        trace_writer: TraceWriter<W>,
        replay: RunReplay,
        reader: &SnapshotReader,
        conversation_context: Vec<CompletedTurnContext>,
    ) -> Result<Self> {
        let mut captured_snapshots = Vec::with_capacity(replay.archived_snapshot_refs.len());
        for reference in &replay.archived_snapshot_refs {
            captured_snapshots.push(reader.get(reference)?.ok_or_else(|| {
                ResearchError::InvalidSnapshot(format!("missing replay snapshot {reference:?}"))
            })?);
        }
        let captured_page_urls = captured_snapshots
            .iter()
            .filter_map(|snapshot| normalize_url_for_deduplication(&snapshot.crawl.final_url).ok())
            .collect();
        let captured_snapshot_refs = captured_snapshots
            .iter()
            .map(|snapshot| snapshot.snapshot_ref.clone())
            .collect();
        Ok(Self {
            brief,
            policy,
            answer_style,
            execution_backend,
            snapshot_writer,
            trace_writer,
            progress: ResearchRunProgress {
                round: replay.completed_round,
                estimated_input_tokens: estimate_snapshot_input_tokens(&captured_snapshots),
                archived_snapshots: captured_snapshots.len(),
                stop_reason: None,
            },
            captured_snapshots,
            previous_queries: replay.previous_queries,
            captured_page_urls,
            captured_snapshot_refs,
            conversation_context,
            model_knowledge_draft: replay.model_knowledge_draft,
        })
    }

    /// Runs the complete pipeline and records exactly one terminal failure event.
    pub async fn execute(&mut self, snapshot_path: impl AsRef<Path>) -> Result<ResearchRunOutput> {
        let result: Result<ResearchRunOutput> = async {
            self.execute_exploration()
                .await
                .map_err(|error| error.at(ResearchStage::Planning))?;
            let reader = SnapshotReader::open(snapshot_path)
                .map_err(|error| error.at(ResearchStage::Setup))?;
            let answer = self
                .synthesize_composed_answer(reader)
                .await
                .map_err(|error| error.at(ResearchStage::Synthesis))?;
            let cited: HashSet<_> = answer
                .claims
                .iter()
                .flat_map(|claim| claim.snapshot_refs.iter().cloned())
                .collect();
            let sources = self
                .captured_snapshots
                .iter()
                .filter(|snapshot| cited.contains(&snapshot.snapshot_ref))
                .map(|snapshot| EvidenceSource {
                    snapshot_ref: snapshot.snapshot_ref.clone(),
                    url: snapshot.crawl.final_url.clone(),
                    title: snapshot.title.clone(),
                })
                .collect();
            Ok(ResearchRunOutput {
                answer,
                knowledge_draft: self
                    .model_knowledge_draft
                    .clone()
                    .expect("knowledge draft is generated before synthesis"),
                answer_style: self.answer_style,
                sources,
            })
        }
        .await;

        match result {
            Ok(answer) => Ok(answer),
            Err(error @ ResearchError::FailureTrace { .. }) => Err(error),
            Err(error) => {
                let failure = TraceEvent::RunFailed {
                    error_class: error.error_class(),
                    stage: error.stage().unwrap_or(ResearchStage::Setup),
                    message: error.to_string(),
                };
                match self.trace_writer.append(&failure) {
                    Ok(()) => Err(error),
                    Err(trace) => Err(ResearchError::FailureTrace {
                        original: Box::new(error),
                        trace: Box::new(trace.at(ResearchStage::Trace)),
                    }),
                }
            }
        }
    }

    pub async fn execute_exploration(&mut self) -> Result<&ResearchRunProgress> {
        for round in self.progress.round + 1..=self.policy.rounds {
            self.progress.round = round;
            self.progress.estimated_input_tokens =
                estimate_snapshot_input_tokens(&self.captured_snapshots);
            if self.progress.estimated_input_tokens >= self.policy.input_budget as usize {
                self.progress.stop_reason = Some(ResearchRunStopReason::InputBudget);
                break;
            }
            if self.captured_snapshots.len() >= self.policy.max_snapshots as usize {
                self.progress.stop_reason = Some(ResearchRunStopReason::SnapshotLimit);
                break;
            }

            let input_snapshot_refs = self
                .captured_snapshots
                .iter()
                .map(|snapshot| snapshot.snapshot_ref.clone())
                .collect();
            let result = self
                .execution_backend
                .generate_search_queries(
                    &self.brief,
                    &self.conversation_context,
                    &self.captured_snapshots,
                    &self.previous_queries,
                )
                .await;
            let raw = self.record_model_call_result(
                "plan",
                round,
                input_snapshot_refs,
                result,
                ResearchStage::Planning,
            )?;
            let queries = parse_search_query_plan(&raw, &self.previous_queries)
                .map_err(|error| error.at(ResearchStage::Planning))?;
            for query in &queries {
                self.trace_writer
                    .append(&TraceEvent::SearchQuery {
                        round,
                        query: query.query.clone(),
                        gap: query.gap.clone(),
                    })
                    .map_err(|error| error.at(ResearchStage::Trace))?;
            }
            self.previous_queries
                .extend(queries.iter().map(|query| query.query.clone()));

            let mut new_results = Vec::new();
            let mut round_urls = HashSet::new();
            for query in queries {
                let results = self
                    .execution_backend
                    .search_web(&query.query)
                    .await
                    .map_err(|error| error.at(ResearchStage::Search))?;
                for result in results {
                    self.trace_writer
                        .append(&TraceEvent::SearchResult {
                            round,
                            query: query.query.clone(),
                            search_result_id: result.search_result_id.clone(),
                            title: result.title.clone(),
                            url: result.url.clone(),
                            snippet: result.snippet.clone(),
                            rank: result.rank,
                        })
                        .map_err(|error| error.at(ResearchStage::Trace))?;
                    match normalize_url_for_deduplication(&result.url) {
                        Ok(url) => {
                            if !self.captured_page_urls.contains(&url) && round_urls.insert(url) {
                                new_results.push(result);
                            }
                        }
                        Err(error) => self
                            .trace_writer
                            .append(&TraceEvent::ArchiveSkip {
                                search_result_id: result.search_result_id,
                                reason: error.to_string(),
                                error_class: error.error_class(),
                            })
                            .map_err(|error| error.at(ResearchStage::Trace))?,
                    }
                }
            }

            if new_results.is_empty() {
                self.progress.stop_reason = Some(ResearchRunStopReason::NoNewUrls);
            }
            for result in new_results {
                if self.captured_snapshots.len() >= self.policy.max_snapshots as usize {
                    self.progress.stop_reason = Some(ResearchRunStopReason::SnapshotLimit);
                    break;
                }
                match self
                    .execution_backend
                    .capture_web_snapshot(&result.url)
                    .await
                {
                    Ok(snapshot) => {
                        let final_url = normalize_url_for_deduplication(&snapshot.crawl.final_url)
                            .map_err(|error| error.at(ResearchStage::Archive))?;
                        if self.captured_page_urls.contains(&final_url)
                            || self.captured_snapshot_refs.contains(&snapshot.snapshot_ref)
                        {
                            self.trace_writer
                                .append(&TraceEvent::ArchiveSkip {
                                    search_result_id: result.search_result_id,
                                    reason: "duplicate final URL or snapshot".into(),
                                    error_class: crate::ErrorClass::External,
                                })
                                .map_err(|error| error.at(ResearchStage::Trace))?;
                            continue;
                        }
                        self.snapshot_writer
                            .save(&snapshot)
                            .map_err(|error| error.at(ResearchStage::Archive))?;
                        self.trace_writer
                            .append(&TraceEvent::Archive {
                                snapshot_ref: snapshot.snapshot_ref.clone(),
                                content_hash: snapshot.content_hash.clone(),
                                final_url: snapshot.crawl.final_url.clone(),
                                char_len: snapshot.body.chars().count(),
                            })
                            .map_err(|error| error.at(ResearchStage::Trace))?;
                        self.captured_page_urls.insert(final_url);
                        self.captured_snapshot_refs
                            .insert(snapshot.snapshot_ref.clone());
                        self.captured_snapshots.push(snapshot);
                        self.progress.archived_snapshots = self.captured_snapshots.len();
                    }
                    Err(error) => self
                        .trace_writer
                        .append(&TraceEvent::ArchiveSkip {
                            search_result_id: result.search_result_id,
                            reason: error.to_string(),
                            error_class: error.error_class(),
                        })
                        .map_err(|error| error.at(ResearchStage::Trace))?,
                }
            }
            self.trace_writer
                .append(&TraceEvent::RoundCompleted {
                    round,
                    previous_queries: self.previous_queries.clone(),
                    archived_snapshot_refs: self
                        .captured_snapshots
                        .iter()
                        .map(|snapshot| snapshot.snapshot_ref.clone())
                        .collect(),
                })
                .map_err(|error| error.at(ResearchStage::Trace))?;
            if self.progress.stop_reason.is_some() {
                break;
            }
        }
        self.progress
            .stop_reason
            .get_or_insert(ResearchRunStopReason::CompletedRounds);
        Ok(&self.progress)
    }

    pub async fn synthesize_composed_answer(
        &mut self,
        reader: SnapshotReader,
    ) -> Result<ComposedResearchAnswer> {
        if self.captured_snapshots.is_empty() {
            return Err(ResearchError::NoUsableSource.at(ResearchStage::Selection));
        }

        let excerpts: Vec<_> = self
            .captured_snapshots
            .iter()
            .map(build_snapshot_navigation_excerpt)
            .collect();
        for excerpt in &excerpts {
            self.trace_writer
                .append(&TraceEvent::SnapshotNavigationExcerpt {
                    snapshot_ref: excerpt.snapshot_ref.clone(),
                    content_hash: excerpt.content_hash.clone(),
                    title: excerpt.title.clone(),
                    excerpt: excerpt.excerpt.clone(),
                })
                .map_err(|error| error.at(ResearchStage::Trace))?;
        }

        let run_snapshots: HashSet<_> = excerpts
            .iter()
            .map(|excerpt| excerpt.snapshot_ref.clone())
            .collect();
        let input_snapshot_refs = excerpts
            .iter()
            .map(|excerpt| excerpt.snapshot_ref.clone())
            .collect();
        let result = self
            .execution_backend
            .select_evidence_snapshots(&self.brief, &self.conversation_context, &excerpts)
            .await;
        let raw = self.record_model_call_result(
            "select",
            self.progress.round,
            input_snapshot_refs,
            result,
            ResearchStage::Selection,
        )?;
        let selected = parse_evidence_selection(&raw, &run_snapshots)
            .map_err(|error| error.at(ResearchStage::Selection))?;
        if selected.is_empty() {
            return Err(ResearchError::NoUsableSource.at(ResearchStage::Selection));
        }
        self.trace_writer
            .append(&TraceEvent::SnapshotSelection {
                selected: selected.clone(),
            })
            .map_err(|error| error.at(ResearchStage::Trace))?;

        let mut evidence = Vec::with_capacity(selected.len());
        for selection in &selected {
            let snapshot = reader
                .get(&selection.snapshot_ref)
                .map_err(|error| error.at(ResearchStage::Selection))?
                .ok_or_else(|| {
                    ResearchError::InvalidSnapshot(format!(
                        "selected snapshot missing from store: {}",
                        selection.snapshot_ref.as_str()
                    ))
                    .at(ResearchStage::Selection)
                })?;
            let expected = excerpts
                .iter()
                .find(|excerpt| excerpt.snapshot_ref == selection.snapshot_ref)
                .expect("selection was validated against excerpts");
            if snapshot.content_hash != expected.content_hash {
                return Err(ResearchError::HashMismatch {
                    reference: snapshot.snapshot_ref.0.clone(),
                    expected: expected.content_hash.clone(),
                    actual: snapshot.content_hash,
                }
                .at(ResearchStage::Selection));
            }
            evidence.push(snapshot);
        }

        if estimate_snapshot_input_tokens(&evidence) >= self.policy.input_budget as usize {
            return invalid_model_output("selected snapshot content reaches input budget")
                .map_err(|error| error.at(ResearchStage::Selection));
        }
        let supplied: HashSet<_> = evidence
            .iter()
            .map(|snapshot| snapshot.snapshot_ref.clone())
            .collect();
        drop(reader);
        let knowledge_draft = self.ensure_model_knowledge_draft().await?;
        let input_snapshot_refs = evidence
            .iter()
            .map(|snapshot| snapshot.snapshot_ref.clone())
            .collect();
        let result = self
            .execution_backend
            .synthesize_composed_answer(
                &self.brief,
                &self.conversation_context,
                &evidence,
                &knowledge_draft,
                self.answer_style,
            )
            .await;
        let raw = self.record_model_call_result(
            "synthesize",
            self.progress.round,
            input_snapshot_refs,
            result,
            ResearchStage::Synthesis,
        )?;
        let answer = parse_composed_research_answer(&raw, &supplied)
            .map_err(|error| error.at(ResearchStage::Synthesis))?;
        for claim in &answer.claims {
            self.trace_writer
                .append(&TraceEvent::ResearchClaim {
                    text: claim.text.clone(),
                    origin: claim.origin,
                    snapshot_refs: claim.snapshot_refs.clone(),
                    rationale: claim.rationale.clone(),
                })
                .map_err(|error| error.at(ResearchStage::Trace))?;
        }
        self.trace_writer
            .append(&TraceEvent::ComposedResearchAnswer {
                answer: answer.answer.clone(),
                claims: answer.claims.clone(),
                comparison: answer.comparison.clone(),
            })
            .map_err(|error| error.at(ResearchStage::Trace))?;
        Ok(answer)
    }

    async fn ensure_model_knowledge_draft(&mut self) -> Result<ModelKnowledgeDraft> {
        if let Some(draft) = &self.model_knowledge_draft {
            return Ok(draft.clone());
        }
        let result = self
            .execution_backend
            .generate_model_knowledge_draft(&self.brief, &self.conversation_context)
            .await;
        let raw = self.record_model_call_result(
            "knowledge_draft",
            0,
            Vec::new(),
            result,
            ResearchStage::Synthesis,
        )?;
        let draft = parse_model_knowledge_draft(&raw)?;
        self.trace_writer
            .append(&TraceEvent::KnowledgeDraft {
                draft: draft.clone(),
            })
            .map_err(|error| error.at(ResearchStage::Trace))?;
        self.model_knowledge_draft = Some(draft.clone());
        Ok(draft)
    }

    fn record_model_call_result(
        &mut self,
        operation: &str,
        round: u32,
        input_snapshot_refs: Vec<SnapshotRef>,
        result: Result<String>,
        stage: ResearchStage,
    ) -> Result<String> {
        match result {
            Ok(output) => {
                self.trace_writer
                    .append(&TraceEvent::ModelCall {
                        operation: operation.to_owned(),
                        round,
                        input_snapshot_refs,
                        output_chars: Some(output.chars().count()),
                        error_class: None,
                    })
                    .map_err(|error| error.at(ResearchStage::Trace))?;
                Ok(output)
            }
            Err(error) => {
                let event = TraceEvent::ModelCall {
                    operation: operation.to_owned(),
                    round,
                    input_snapshot_refs,
                    output_chars: None,
                    error_class: Some(error.error_class()),
                };
                let original = error.at(stage);
                match self.trace_writer.append(&event) {
                    Ok(()) => Err(original),
                    Err(trace) => Err(ResearchError::FailureTrace {
                        original: Box::new(original),
                        trace: Box::new(trace.at(ResearchStage::Trace)),
                    }),
                }
            }
        }
    }

    #[must_use]
    pub fn captured_snapshots(&self) -> &[Snapshot] {
        &self.captured_snapshots
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQueryOutput {
    queries: Vec<SearchQueryJson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQueryJson {
    query: String,
    gap: String,
}

pub fn parse_search_query_plan(raw: &str, previous_queries: &[String]) -> Result<Vec<SearchQuery>> {
    let output: SearchQueryOutput = parse_generate_model_json(raw)?;
    if output.queries.len() != QUERIES_PER_ROUND {
        return invalid_model_output("query output must contain exactly 3 queries");
    }
    let mut seen: HashSet<String> = previous_queries
        .iter()
        .map(|query| query.trim().to_lowercase())
        .collect();
    for query in &output.queries {
        let normalized = query.query.trim().to_lowercase();
        if normalized.is_empty()
            || validate_decision_rationale(&query.gap).is_err()
            || query.query.split_whitespace().count() > 12
            || query.query.chars().count() > MAX_QUERY_CHARS
            || !seen.insert(normalized)
        {
            return invalid_model_output("queries must be non-empty, bounded, and unique");
        }
    }
    Ok(output
        .queries
        .into_iter()
        .map(|query| SearchQuery {
            query: query.query,
            gap: query.gap,
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionOutput {
    selected: Vec<SelectionJson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionJson {
    snapshot_ref: SnapshotRef,
    reason: String,
}

pub fn parse_evidence_selection(
    raw: &str,
    run_snapshots: &HashSet<SnapshotRef>,
) -> Result<Vec<SourceSelection>> {
    let output: SelectionOutput = parse_generate_model_json(raw)?;
    if output.selected.len() > MAX_READ_SNAPSHOTS {
        return invalid_model_output("too many selected snapshots");
    }
    let mut seen = HashSet::new();
    if output.selected.iter().any(|selection| {
        selection.snapshot_ref.as_str().trim().is_empty()
            || validate_decision_rationale(&selection.reason).is_err()
            || !run_snapshots.contains(&selection.snapshot_ref)
            || !seen.insert(selection.snapshot_ref.clone())
    }) {
        return invalid_model_output(
            "selected snapshots must be non-empty, unique, and belong to this run",
        );
    }
    Ok(output
        .selected
        .into_iter()
        .map(|selection| SourceSelection {
            snapshot_ref: selection.snapshot_ref,
            reason: selection.reason,
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelKnowledgeDraftJson {
    answer: String,
    claims: Vec<String>,
    uncertainty: String,
    basis_summary: String,
}

pub fn parse_model_knowledge_draft(raw: &str) -> Result<ModelKnowledgeDraft> {
    let draft: ModelKnowledgeDraftJson = parse_generate_model_json(raw)?;
    if draft.answer.trim().is_empty()
        || draft.claims.is_empty()
        || draft.claims.iter().any(|claim| claim.trim().is_empty())
        || draft.uncertainty.trim().is_empty()
        || validate_decision_rationale(&draft.basis_summary).is_err()
    {
        return invalid_model_output(
            "knowledge draft must contain an answer, claims, uncertainty, and basis summary",
        );
    }
    Ok(ModelKnowledgeDraft {
        answer: draft.answer,
        claims: draft.claims,
        uncertainty: draft.uncertainty,
        basis_summary: draft.basis_summary,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposedResearchAnswerJson {
    answer: String,
    claims: Vec<ComposedResearchClaimJson>,
    comparison: ResearchAnswerComparison,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposedResearchClaimJson {
    text: String,
    origin: ResearchClaimOrigin,
    #[serde(default)]
    snapshot_refs: Vec<SnapshotRef>,
    rationale: String,
}

pub fn parse_composed_research_answer(
    raw: &str,
    supplied: &HashSet<SnapshotRef>,
) -> Result<ComposedResearchAnswer> {
    let output: ComposedResearchAnswerJson = parse_generate_model_json(raw)?;
    if output.answer.trim().is_empty()
        || output.claims.is_empty()
        || validate_decision_rationale(&output.comparison.synthesis_rationale).is_err()
        || output.claims.iter().any(|claim| {
            claim.text.trim().is_empty()
                || validate_decision_rationale(&claim.rationale).is_err()
                || match claim.origin {
                    ResearchClaimOrigin::ModelKnowledge => !claim.snapshot_refs.is_empty(),
                    ResearchClaimOrigin::WebEvidence => {
                        claim.snapshot_refs.is_empty()
                            || claim
                                .snapshot_refs
                                .iter()
                                .any(|reference| !supplied.contains(reference))
                    }
                }
        })
        || !output
            .claims
            .iter()
            .any(|claim| claim.origin == ResearchClaimOrigin::ModelKnowledge)
        || !output
            .claims
            .iter()
            .any(|claim| claim.origin == ResearchClaimOrigin::WebEvidence)
    {
        return invalid_model_output(
            "answer must compare model knowledge and web evidence with valid claim provenance",
        );
    }

    Ok(ComposedResearchAnswer {
        answer: output.answer,
        claims: output
            .claims
            .into_iter()
            .map(|claim| ComposedResearchClaim {
                text: claim.text,
                origin: claim.origin,
                snapshot_refs: claim.snapshot_refs,
                rationale: claim.rationale,
            })
            .collect(),
        comparison: output.comparison,
    })
}

#[must_use]
pub fn build_snapshot_navigation_excerpt(snapshot: &Snapshot) -> SnapshotNavigationExcerpt {
    let first_paragraph = snapshot
        .body
        .split("\n\n")
        .find(|part| !part.trim().is_empty())
        .unwrap_or_default()
        .trim();
    SnapshotNavigationExcerpt {
        snapshot_ref: snapshot.snapshot_ref.clone(),
        content_hash: snapshot.content_hash.clone(),
        title: snapshot.title.clone(),
        excerpt: format!(
            "{}\n{}\n{}",
            snapshot.title, first_paragraph, snapshot.crawl.final_url
        ),
    }
}

fn normalize_url_for_deduplication(raw: &str) -> Result<String> {
    let mut url = Url::parse(raw).map_err(|error| ResearchError::Search {
        message: format!("invalid result URL: {error}"),
    })?;
    url.set_fragment(None);
    Ok(url.to_string())
}

fn estimate_snapshot_input_tokens(snapshots: &[Snapshot]) -> usize {
    snapshots
        .iter()
        .map(|snapshot| snapshot.body.chars().count().div_ceil(4))
        .sum()
}

fn parse_generate_model_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|error| ResearchError::ModelOutput {
        message: format!("invalid JSON content: {error}"),
    })
}

fn invalid_model_output<T>(message: &str) -> Result<T> {
    Err(ResearchError::ModelOutput {
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{self, Write},
        path::PathBuf,
        time::SystemTime,
    };

    use chrono::Utc;

    use super::*;
    use crate::{
        CrawlMeta, ErrorClass, RunHeader, TRACE_SCHEMA_VERSION, TracePolicy,
        research_trace::LegacyRunHeader,
    };

    #[derive(Default)]
    struct FixtureBackend {
        plan_calls: u32,
        search_calls: u32,
        crawl_calls: u32,
        selected_ref: Option<SnapshotRef>,
        synthesize_calls: u32,
        plan_error: bool,
        duplicate_urls: bool,
        crawl_failures_remaining: u32,
        converged_final_url: bool,
        planned_briefs: Vec<FrozenResearchBrief>,
        planned_histories: Vec<Vec<CompletedTurnContext>>,
        selected_brief: Option<FrozenResearchBrief>,
        selected_history: Option<Vec<CompletedTurnContext>>,
        synthesized_brief: Option<FrozenResearchBrief>,
        synthesized_history: Option<Vec<CompletedTurnContext>>,
    }

    impl ResearchExecutionBackend for FixtureBackend {
        fn generate_model_knowledge_draft(
            &mut self,
            _brief: &FrozenResearchBrief,
            _conversation_context: &[CompletedTurnContext],
        ) -> impl Future<Output = Result<String>> {
            std::future::ready(Ok(
                r#"{"answer":"fixture knowledge answer","claims":["fixture knowledge claim"],"uncertainty":"fixture uncertainty","basis_summary":"fixture knowledge basis"}"#.into(),
            ))
        }

        fn generate_search_queries(
            &mut self,
            brief: &FrozenResearchBrief,
            conversation_context: &[CompletedTurnContext],
            _snapshots: &[Snapshot],
            _previous_queries: &[String],
        ) -> impl Future<Output = Result<String>> {
            self.plan_calls += 1;
            self.planned_briefs.push(brief.clone());
            self.planned_histories.push(conversation_context.to_vec());
            let round = self.plan_calls;
            std::future::ready(if self.plan_error {
                Err(ResearchError::ModelCall {
                    message: "fixture planning failure".into(),
                })
            } else {
                Ok(format!(
                    r#"{{"queries":[{{"query":"q{round}-0","gap":"fixture evidence gap"}},{{"query":"q{round}-1","gap":"fixture evidence gap"}},{{"query":"q{round}-2","gap":"fixture evidence gap"}}]}}"#
                ))
            })
        }

        fn search_web(&mut self, query: &str) -> impl Future<Output = Result<Vec<SearchResult>>> {
            self.search_calls += 1;
            std::future::ready(Ok(vec![
                SearchResult::new(
                    query,
                    query.into(),
                    "first".into(),
                    if self.duplicate_urls {
                        "https://example.com/duplicate#first".into()
                    } else {
                        format!("https://example.com/{query}#first")
                    },
                    1,
                ),
                SearchResult::new(
                    query,
                    query.into(),
                    "duplicate".into(),
                    if self.duplicate_urls {
                        "https://example.com/duplicate#duplicate".into()
                    } else {
                        format!("https://example.com/{query}#duplicate")
                    },
                    2,
                ),
            ]))
        }

        fn capture_web_snapshot(&mut self, url: &str) -> impl Future<Output = Result<Snapshot>> {
            self.crawl_calls += 1;
            let result = if self.crawl_failures_remaining > 0 {
                self.crawl_failures_remaining -= 1;
                Err(ResearchError::Fetch {
                    url: url.into(),
                    reason: "fixture transient failure".into(),
                })
            } else if url.contains("/q1-0#") && !self.duplicate_urls {
                Err(ResearchError::Fetch {
                    url: url.into(),
                    reason: "fixture skip".into(),
                })
            } else {
                let final_url = if self.converged_final_url {
                    "https://example.com/canonical".into()
                } else {
                    url.into()
                };
                Ok(Snapshot::new(
                    url.into(),
                    url.into(),
                    format!("body for {url}"),
                    CrawlMeta::basic(final_url, 200, Utc::now()),
                ))
            };
            std::future::ready(result)
        }

        fn select_evidence_snapshots(
            &mut self,
            brief: &FrozenResearchBrief,
            conversation_context: &[CompletedTurnContext],
            _excerpts: &[SnapshotNavigationExcerpt],
        ) -> impl Future<Output = Result<String>> {
            self.selected_brief = Some(brief.clone());
            self.selected_history = Some(conversation_context.to_vec());
            std::future::ready(self.selected_ref.as_ref().map_or_else(
                || {
                    Err(ResearchError::ModelCall {
                        message: "unused in execute_exploration fixture".into(),
                    })
                },
                |reference| {
                    Ok(format!(
                        r#"{{"selected":[{{"snapshot_ref":"{}","reason":"fixture selection reason"}}]}}"#,
                        reference.as_str()
                    ))
                },
            ))
        }

        fn synthesize_composed_answer(
            &mut self,
            brief: &FrozenResearchBrief,
            conversation_context: &[CompletedTurnContext],
            _snapshots: &[Snapshot],
            _knowledge_draft: &ModelKnowledgeDraft,
            _answer_style: ResearchAnswerStyle,
        ) -> impl Future<Output = Result<String>> {
            self.synthesize_calls += 1;
            self.synthesized_brief = Some(brief.clone());
            self.synthesized_history = Some(conversation_context.to_vec());
            std::future::ready(self.selected_ref.as_ref().map_or_else(
                || {
                    Err(ResearchError::ModelCall {
                        message: "unused in fixture".into(),
                    })
                },
                |reference| {
                    Ok(format!(
                        r#"{{"answer":"2024 年诺贝尔物理学奖授予 John Hopfield 与 Geoffrey Hinton。","claims":[{{"text":"模型知识草稿认为二人与机器学习相关。","origin":"model_knowledge","snapshot_refs":[],"rationale":"fixture keeps the model knowledge claim"}},{{"text":"二人因机器学习基础性发现与发明获奖。","origin":"web_evidence","snapshot_refs":["{}"],"rationale":"fixture snapshot directly supports the award claim"}}],"comparison":{{"agreements":["二者均提及机器学习"],"differences":[],"synthesis_rationale":"fixture uses requested weights"}}}}"#,
                        reference.as_str()
                    ))
                },
            ))
        }
    }

    struct FailAfterHeader {
        header_written: bool,
    }

    impl Write for FailAfterHeader {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.header_written {
                return Err(io::Error::other("fixture trace failure"));
            }
            if buffer.contains(&b'\n') {
                self.header_written = true;
            }
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct TempDb(PathBuf);

    impl TempDb {
        fn new(tag: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "traceable-search-orchestration-{tag}-{}-{nonce}.sqlite",
                std::process::id()
            )))
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn frozen_brief() -> FrozenResearchBrief {
        let brief = crate::ResearchBrief {
            schema_version: crate::RESEARCH_BRIEF_SCHEMA_VERSION,
            original_question: "question 原文".into(),
            research_question: "focused research question".into(),
            desired_output: Some("concise answer".into()),
            scope: crate::ResearchScope::default(),
            source_constraints: vec!["primary sources".into()],
            accepted_assumptions: vec!["fixture assumption".into()],
        };
        let hash = brief.content_hash().unwrap();
        FrozenResearchBrief::new(
            brief,
            "question 原文",
            "clarification-fixture".into(),
            &hash,
            Utc::now(),
        )
        .unwrap()
    }

    fn research_run_with_rounds(
        db: &TempDb,
        rounds: u32,
    ) -> ResearchRunExecutor<FixtureBackend, Vec<u8>> {
        let brief = frozen_brief();
        let policy = TracePolicy {
            rounds,
            input_budget: MAX_STRONG_INPUT_TOKENS as u32,
            max_snapshots: MAX_SNAPSHOTS as u32,
        };
        let trace = TraceWriter::new(
            Vec::new(),
            RunHeader {
                run_id: "fixture".into(),
                clarification_id: brief.clarification_id().into(),
                session_id: None,
                turn: None,
                brief: brief.clone(),
                started_at: Utc::now(),
                policy: policy.clone(),
                answer_style: ResearchAnswerStyle::WebFirst,
            },
        )
        .unwrap();
        ResearchRunExecutor::new(
            brief,
            policy,
            ResearchAnswerStyle::WebFirst,
            FixtureBackend::default(),
            SnapshotWriter::open(&db.0).unwrap(),
            trace,
        )
    }

    fn research_run(db: &TempDb) -> ResearchRunExecutor<FixtureBackend, Vec<u8>> {
        research_run_with_rounds(db, DEFAULT_EXPLORE_ROUNDS)
    }

    #[tokio::test]
    async fn resume_restores_committed_state_and_starts_at_the_next_round() {
        let db = TempDb::new("resume");
        let mut original = research_run_with_rounds(&db, 1);
        original.execute_exploration().await.unwrap();
        let replay = RunReplay {
            completed_round: original.progress.round,
            previous_queries: original.previous_queries.clone(),
            archived_snapshot_refs: original
                .captured_snapshots
                .iter()
                .map(|snapshot| snapshot.snapshot_ref.clone())
                .collect(),
            model_knowledge_draft: None,
        };
        let restored_count = replay.archived_snapshot_refs.len();
        drop(original);

        let brief = frozen_brief();
        let trace = TraceWriter::new(
            Vec::new(),
            RunHeader {
                run_id: "resumed-fixture".into(),
                clarification_id: brief.clarification_id().into(),
                session_id: None,
                turn: None,
                brief: brief.clone(),
                started_at: Utc::now(),
                policy: TracePolicy {
                    rounds: DEFAULT_EXPLORE_ROUNDS,
                    input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                    max_snapshots: MAX_SNAPSHOTS as u32,
                },
                answer_style: ResearchAnswerStyle::WebFirst,
            },
        )
        .unwrap();
        let reader = SnapshotReader::open(&db.0).unwrap();
        let execution_backend = FixtureBackend {
            plan_calls: 1,
            ..FixtureBackend::default()
        };
        let mut resumed = ResearchRunExecutor::resume(
            brief,
            TracePolicy {
                rounds: DEFAULT_EXPLORE_ROUNDS,
                input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                max_snapshots: MAX_SNAPSHOTS as u32,
            },
            ResearchAnswerStyle::WebFirst,
            execution_backend,
            SnapshotWriter::open(&db.0).unwrap(),
            trace,
            replay,
            &reader,
            Vec::new(),
        )
        .unwrap();

        assert_eq!(resumed.progress.round, 1);
        assert_eq!(resumed.captured_snapshots.len(), restored_count);
        assert_eq!(resumed.captured_page_urls.len(), restored_count);
        resumed.execute_exploration().await.unwrap();
        assert_eq!(resumed.progress.round, DEFAULT_EXPLORE_ROUNDS);
        assert_eq!(resumed.execution_backend.plan_calls, DEFAULT_EXPLORE_ROUNDS);
    }

    #[tokio::test]
    async fn run_records_external_selection_failure_as_the_only_terminal_event() {
        let db = TempDb::new("run-selection-failure");
        let mut research_run = research_run(&db);

        let error = research_run.execute(&db.0).await.unwrap_err();

        assert_eq!(error.stage(), Some(ResearchStage::Selection));
        assert_eq!(error.error_class(), ErrorClass::External);
        let trace = String::from_utf8(research_run.trace_writer.into_inner()).unwrap();
        let events = trace
            .lines()
            .map(|line| serde_json::from_str::<TraceEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, TraceEvent::RunFailed { .. }))
                .count(),
            1
        );
        assert!(matches!(
            events.last(),
            Some(TraceEvent::RunFailed {
                error_class: ErrorClass::External,
                stage: ResearchStage::Selection,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn run_records_internal_snapshot_setup_failure() {
        let db = TempDb::new("run-setup-writer");
        let missing = TempDb::new("run-setup-missing");
        let mut research_run = research_run(&db);

        let error = research_run.execute(&missing.0).await.unwrap_err();

        assert_eq!(error.stage(), Some(ResearchStage::Setup));
        assert_eq!(error.error_class(), ErrorClass::Internal);
        let trace = String::from_utf8(research_run.trace_writer.into_inner()).unwrap();
        let last = serde_json::from_str::<TraceEvent>(trace.lines().last().unwrap()).unwrap();
        assert!(matches!(
            last,
            TraceEvent::RunFailed {
                error_class: ErrorClass::Internal,
                stage: ResearchStage::Setup,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn run_preserves_original_and_trace_failures() {
        let db = TempDb::new("run-trace-failure");
        let trace = TraceWriter::new_legacy(
            FailAfterHeader {
                header_written: false,
            },
            LegacyRunHeader {
                run_id: "trace-failure".into(),
                question: "question".into(),
                started_at: Utc::now(),
                policy: TracePolicy {
                    rounds: DEFAULT_EXPLORE_ROUNDS,
                    input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                    max_snapshots: MAX_SNAPSHOTS as u32,
                },
            },
        )
        .unwrap();
        let execution_backend = FixtureBackend {
            plan_error: true,
            ..FixtureBackend::default()
        };
        let mut research_run = ResearchRunExecutor::new(
            frozen_brief(),
            TracePolicy {
                rounds: DEFAULT_EXPLORE_ROUNDS,
                input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                max_snapshots: MAX_SNAPSHOTS as u32,
            },
            ResearchAnswerStyle::WebFirst,
            execution_backend,
            SnapshotWriter::open(&db.0).unwrap(),
            trace,
        );

        let error = research_run.execute(&db.0).await.unwrap_err();

        match error {
            ResearchError::FailureTrace { original, trace } => {
                assert_eq!(original.stage(), Some(ResearchStage::Planning));
                assert_eq!(original.error_class(), ErrorClass::External);
                assert_eq!(trace.stage(), Some(ResearchStage::Trace));
                assert_eq!(trace.error_class(), ErrorClass::Internal);
            }
            other => panic!("expected both failures, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_exploration_runs_three_rounds_deduplicates_and_traces_archive_skip() {
        let db = TempDb::new("execute_exploration");
        let mut research_run = research_run(&db);

        let progress = research_run.execute_exploration().await.unwrap().clone();

        assert_eq!(progress.round, DEFAULT_EXPLORE_ROUNDS);
        assert_eq!(
            progress.stop_reason,
            Some(ResearchRunStopReason::CompletedRounds)
        );
        assert_eq!(
            research_run.execution_backend.plan_calls,
            DEFAULT_EXPLORE_ROUNDS
        );
        assert_eq!(
            research_run.execution_backend.search_calls,
            DEFAULT_EXPLORE_ROUNDS * QUERIES_PER_ROUND as u32
        );
        assert_eq!(
            research_run.execution_backend.crawl_calls,
            DEFAULT_EXPLORE_ROUNDS * 3
        );
        assert_eq!(research_run.captured_snapshots.len(), 8);
        let trace = String::from_utf8(research_run.trace_writer.into_inner()).unwrap();
        assert_eq!(trace.matches("\"type\":\"archive_skip\"").count(), 1);
        let result: serde_json::Value = trace
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .find(|event: &serde_json::Value| event["type"] == "search_result")
            .unwrap();
        assert_eq!(result["round"], 1);
        assert_eq!(result["query"], "q1-0");
    }

    #[test]
    fn execute_exploration_accepts_max_rounds() {
        let db = TempDb::new("max-rounds");
        let research_run = research_run_with_rounds(&db, MAX_EXPLORE_ROUNDS);

        assert_eq!(research_run.policy.rounds, MAX_EXPLORE_ROUNDS);
    }

    #[tokio::test]
    async fn execute_exploration_stops_when_every_search_repeats_seen_urls() {
        let db = TempDb::new("duplicate-search-results");
        let mut research_run = research_run(&db);
        research_run.execution_backend.duplicate_urls = true;

        let progress = research_run.execute_exploration().await.unwrap().clone();

        assert_eq!(progress.round, 2);
        assert_eq!(progress.stop_reason, Some(ResearchRunStopReason::NoNewUrls));
        assert_eq!(research_run.execution_backend.plan_calls, 2);
        assert_eq!(
            research_run.execution_backend.search_calls,
            2 * QUERIES_PER_ROUND as u32
        );
        assert_eq!(research_run.execution_backend.crawl_calls, 1);
        assert_eq!(research_run.captured_snapshots.len(), 1);
    }

    #[tokio::test]
    async fn transient_crawl_failure_is_retried_in_a_later_round() {
        let db = TempDb::new("crawl-retry");
        let mut research_run = research_run(&db);
        research_run.execution_backend.duplicate_urls = true;
        research_run.execution_backend.crawl_failures_remaining = 1;

        let progress = research_run.execute_exploration().await.unwrap().clone();

        assert_eq!(progress.round, 3);
        assert_eq!(progress.stop_reason, Some(ResearchRunStopReason::NoNewUrls));
        assert_eq!(research_run.execution_backend.crawl_calls, 2);
        assert_eq!(research_run.captured_snapshots.len(), 1);
    }

    #[tokio::test]
    async fn redirects_converging_on_one_page_archive_one_snapshot() {
        let db = TempDb::new("redirect-convergence");
        let mut research_run = research_run(&db);
        research_run.execution_backend.converged_final_url = true;

        research_run.execute_exploration().await.unwrap();

        assert_eq!(research_run.captured_snapshots.len(), 1);
        assert_eq!(research_run.captured_page_urls.len(), 1);
        assert_eq!(research_run.captured_snapshot_refs.len(), 1);
        assert_eq!(research_run.progress.archived_snapshots, 1);
    }

    #[tokio::test]
    async fn execute_exploration_stops_before_model_call_when_input_budget_is_exceeded() {
        let db = TempDb::new("budget");
        let mut research_run = research_run(&db);
        research_run.captured_snapshots.push(Snapshot::new(
            "https://example.com/large".into(),
            "large".into(),
            "x".repeat(MAX_STRONG_INPUT_TOKENS * 4),
            CrawlMeta::basic("https://example.com/large".into(), 200, Utc::now()),
        ));

        let progress = research_run.execute_exploration().await.unwrap().clone();

        assert_eq!(
            progress.stop_reason,
            Some(ResearchRunStopReason::InputBudget)
        );
        assert_eq!(research_run.execution_backend.plan_calls, 0);
    }

    #[tokio::test]
    async fn execute_exploration_stops_before_external_calls_at_snapshot_limit() {
        let db = TempDb::new("snapshot-limit");
        let mut research_run = research_run(&db);
        let snapshot = Snapshot::new(
            "https://example.com/captured_snapshots".into(),
            "captured_snapshots".into(),
            "body".into(),
            CrawlMeta::basic(
                "https://example.com/captured_snapshots".into(),
                200,
                Utc::now(),
            ),
        );
        research_run.captured_snapshots = vec![snapshot; MAX_SNAPSHOTS];

        let progress = research_run.execute_exploration().await.unwrap().clone();

        assert_eq!(
            progress.stop_reason,
            Some(ResearchRunStopReason::SnapshotLimit)
        );
        assert_eq!(research_run.execution_backend.plan_calls, 0);
        assert_eq!(research_run.execution_backend.search_calls, 0);
        assert_eq!(research_run.execution_backend.crawl_calls, 0);
    }

    #[tokio::test]
    async fn synthesize_stops_before_model_call_at_input_budget() {
        let db = TempDb::new("synthesize-budget");
        let mut research_run = research_run(&db);
        let snapshot = Snapshot::new(
            "https://example.com/large".into(),
            "large".into(),
            "x".repeat(MAX_STRONG_INPUT_TOKENS * 4),
            CrawlMeta::basic("https://example.com/large".into(), 200, Utc::now()),
        );
        research_run.snapshot_writer.save(&snapshot).unwrap();
        research_run.execution_backend.selected_ref = Some(snapshot.snapshot_ref.clone());
        research_run.captured_snapshots.push(snapshot);
        let reader = SnapshotReader::open(&db.0).unwrap();

        let error = research_run
            .synthesize_composed_answer(reader)
            .await
            .unwrap_err();
        assert_eq!(error.stage(), Some(ResearchStage::Selection));
        assert!(matches!(
            error,
            ResearchError::Staged { source, .. }
                if matches!(*source, ResearchError::ModelOutput { .. })
        ));
        assert_eq!(research_run.execution_backend.synthesize_calls, 0);
    }

    #[tokio::test]
    async fn nobel_fixture_runs_end_to_end_and_replays_audit_trace() {
        let db = TempDb::new("nobel-e2e");
        let mut research_run = research_run(&db);
        let expected_history = vec![CompletedTurnContext {
            turn: 1,
            user_question: "Who won previously?".into(),
            answer: "A grounded prior answer.".into(),
        }];
        research_run.conversation_context = expected_history.clone();

        research_run.execute_exploration().await.unwrap();
        let selected = research_run.captured_snapshots[0].snapshot_ref.clone();
        research_run.execution_backend.selected_ref = Some(selected.clone());
        let answer = research_run
            .synthesize_composed_answer(SnapshotReader::open(&db.0).unwrap())
            .await
            .unwrap();

        assert!(answer.answer.contains("John Hopfield"));
        assert!(answer.answer.contains("Geoffrey Hinton"));
        assert_eq!(answer.claims[1].snapshot_refs, vec![selected]);

        let expected_brief = research_run.brief.clone();
        assert!(
            research_run
                .execution_backend
                .planned_briefs
                .iter()
                .all(|brief| brief == &expected_brief)
        );
        assert_eq!(
            research_run.execution_backend.selected_brief.as_ref(),
            Some(&expected_brief)
        );
        assert_eq!(
            research_run.execution_backend.synthesized_brief.as_ref(),
            Some(&expected_brief)
        );
        assert!(
            research_run
                .execution_backend
                .planned_histories
                .iter()
                .all(|conversation_context| conversation_context == &expected_history)
        );
        assert_eq!(
            research_run.execution_backend.selected_history.as_ref(),
            Some(&expected_history)
        );
        assert_eq!(
            research_run.execution_backend.synthesized_history.as_ref(),
            Some(&expected_history)
        );

        let trace = String::from_utf8(research_run.trace_writer.into_inner()).unwrap();
        let events: Vec<TraceEvent> = trace
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut captured_snapshots = HashSet::new();
        for event in &events {
            match event {
                TraceEvent::Archive { snapshot_ref, .. } => {
                    captured_snapshots.insert(snapshot_ref.clone());
                }
                TraceEvent::SnapshotNavigationExcerpt { snapshot_ref, .. } => {
                    assert!(captured_snapshots.contains(snapshot_ref));
                }
                TraceEvent::SnapshotSelection { selected } => {
                    assert!(
                        selected
                            .iter()
                            .all(|source| captured_snapshots.contains(&source.snapshot_ref))
                    );
                }
                TraceEvent::ResearchClaim { snapshot_refs, .. } => {
                    assert!(
                        snapshot_refs
                            .iter()
                            .all(|reference| captured_snapshots.contains(reference))
                    );
                }
                TraceEvent::ComposedResearchAnswer { claims, .. } => {
                    assert!(
                        claims
                            .iter()
                            .flat_map(|claim| &claim.snapshot_refs)
                            .all(|reference| captured_snapshots.contains(reference))
                    );
                }
                _ => {}
            }
        }
        match events.first() {
            Some(TraceEvent::RunHeader {
                schema_version,
                clarification_id: Some(clarification_id),
                brief: Some(header_brief),
                ..
            }) => {
                assert_eq!(*schema_version, TRACE_SCHEMA_VERSION);
                assert_eq!(clarification_id, expected_brief.clarification_id());
                assert_eq!(header_brief.as_ref(), &expected_brief);
                assert_eq!(header_brief.content_hash(), expected_brief.content_hash());
            }
            event => panic!("expected current run header, got {event:?}"),
        }
        assert!(matches!(
            events.last(),
            Some(TraceEvent::ComposedResearchAnswer { .. })
        ));
    }

    #[test]
    fn prompt_shaped_two_field_selection_is_accepted() {
        let reference = SnapshotRef("snapshot:web/own".into());
        let run_snapshots = HashSet::from([reference.clone()]);
        let raw = format!(
            r#"{{"selected":[{{"snapshot_ref":"{}","reason":"direct evidence"}}]}}"#,
            reference.as_str()
        );

        let selected = parse_evidence_selection(&raw, &run_snapshots).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].snapshot_ref, reference);
        assert_eq!(selected[0].reason, "direct evidence");
    }

    #[test]
    fn legacy_three_field_selection_is_rejected() {
        let reference = SnapshotRef("snapshot:web/own".into());
        let run_snapshots = HashSet::from([reference.clone()]);
        let raw = format!(
            r#"{{"selected":[{{"snapshot_ref":"{}","relevance":"high","reason":"x"}}]}}"#,
            reference.as_str()
        );
        assert!(matches!(
            parse_evidence_selection(&raw, &run_snapshots),
            Err(ResearchError::ModelOutput { .. })
        ));
    }

    #[test]
    fn duplicate_and_empty_selection_refs_are_rejected() {
        let reference = SnapshotRef("snapshot:web/own".into());
        let duplicate = format!(
            r#"{{"selected":[{{"snapshot_ref":"{0}","reason":"x"}},{{"snapshot_ref":"{0}","reason":"y"}}]}}"#,
            reference.as_str()
        );
        assert!(matches!(
            parse_evidence_selection(&duplicate, &HashSet::from([reference])),
            Err(ResearchError::ModelOutput { .. })
        ));

        let empty = SnapshotRef(String::new());
        assert!(matches!(
            parse_evidence_selection(
                r#"{"selected":[{"snapshot_ref":"","reason":"x"}]}"#,
                &HashSet::from([empty])
            ),
            Err(ResearchError::ModelOutput { .. })
        ));
    }

    #[test]
    fn valid_single_claim_is_accepted() {
        let own = SnapshotRef("snapshot:web/own".into());
        let supplied = HashSet::from([own.clone()]);
        let raw = format!(
            r#"{{"answer":"supported","claims":[{{"text":"model","origin":"model_knowledge","rationale":"model knowledge remains useful"}},{{"text":"supported","origin":"web_evidence","snapshot_refs":["{}"],"rationale":"selected source supports this claim"}}],"comparison":{{"agreements":[],"differences":[],"synthesis_rationale":"uses both sources"}}}}"#,
            own.as_str()
        );
        assert!(parse_composed_research_answer(&raw, &supplied).is_ok());
    }

    #[test]
    fn empty_claims_are_rejected() {
        let supplied = HashSet::new();
        assert!(matches!(
            parse_composed_research_answer(r#"{"answer":"unsupported","claims":[]}"#, &supplied),
            Err(ResearchError::ModelOutput { .. })
        ));
    }

    #[test]
    fn final_claim_rationale_is_required() {
        let own = SnapshotRef("snapshot:web/own".into());
        let raw = format!(
            r#"{{"answer":"supported","claims":[{{"text":"model","origin":"model_knowledge"}},{{"text":"supported","origin":"web_evidence","snapshot_refs":["{}"]}}],"comparison":{{"agreements":[],"differences":[],"synthesis_rationale":"uses both sources"}}}}"#,
            own.as_str()
        );
        assert!(matches!(
            parse_composed_research_answer(&raw, &HashSet::from([own])),
            Err(ResearchError::ModelOutput { .. })
        ));
    }

    #[test]
    fn invalid_model_output_cannot_escape_run_or_citation_set() {
        let own = SnapshotRef("snapshot:web/own".into());
        let foreign = SnapshotRef("snapshot:web/foreign".into());
        let run_snapshots = HashSet::from([own.clone()]);
        let selection = format!(
            r#"{{"selected":[{{"snapshot_ref":"{}","reason":"x"}}]}}"#,
            foreign.as_str()
        );
        assert!(matches!(
            parse_evidence_selection(&selection, &run_snapshots),
            Err(ResearchError::ModelOutput { .. })
        ));

        let answer = format!(
            r#"{{"answer":"x","claims":[{{"text":"model","origin":"model_knowledge","rationale":"model knowledge remains useful"}},{{"text":"x","origin":"web_evidence","snapshot_refs":["{}"],"rationale":"foreign source should be rejected"}}],"comparison":{{"agreements":[],"differences":[],"synthesis_rationale":"uses both sources"}}}}"#,
            foreign.as_str()
        );
        assert!(matches!(
            parse_composed_research_answer(&answer, &HashSet::from([own])),
            Err(ResearchError::ModelOutput { .. })
        ));
    }
}

// ponytail: one sequential module only; split strategies when a second policy exists.

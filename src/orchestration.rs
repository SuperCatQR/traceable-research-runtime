//! Fixed-round Explore orchestration and pure strong-model output validation.

use std::{collections::HashSet, future::Future, io::Write, path::Path};

use serde::Deserialize;
use url::Url;

use crate::{
    Answer, Excerpt, PipelineStage, Query, Result, SearchError, SearchResult, Snapshot,
    SnapshotReader, SnapshotRef, SnapshotWriter, SourceSelection, TraceEvent, TraceWriter,
};

pub const EXPLORE_ROUNDS: u32 = 3;
pub const QUERIES_PER_ROUND: usize = 3;
pub const MAX_STRONG_INPUT_TOKENS: usize = 800_000;
pub const MAX_SNAPSHOTS: usize = 300;
pub const MAX_READ_SNAPSHOTS: usize = 100;
const MAX_QUERY_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    CompletedRounds,
    InputBudget,
    SnapshotLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResearchState {
    pub round: u32,
    pub estimated_input_tokens: usize,
    pub archived_snapshots: usize,
    pub stop_reason: Option<StopReason>,
}

/// Test seam for the three external effects. Implementations remain sequential.
pub trait ResearchBackend {
    fn plan(
        &mut self,
        question: &str,
        archived: &[Snapshot],
        previous_queries: &[String],
    ) -> impl Future<Output = Result<String>>;

    fn search(&mut self, query: &str) -> impl Future<Output = Result<Vec<SearchResult>>>;

    fn crawl(&mut self, url: &str) -> impl Future<Output = Result<Snapshot>>;

    fn select(
        &mut self,
        question: &str,
        excerpts: &[Excerpt],
    ) -> impl Future<Output = Result<String>>;

    fn synthesize(
        &mut self,
        question: &str,
        snapshots: &[Snapshot],
    ) -> impl Future<Output = Result<String>>;
}

pub struct ResearchSession<B, W: Write> {
    question: String,
    backend: B,
    snapshots: SnapshotWriter,
    trace: TraceWriter<W>,
    state: ResearchState,
    archived: Vec<Snapshot>,
    previous_queries: Vec<String>,
    seen_urls: HashSet<String>,
}

impl<B: ResearchBackend, W: Write> ResearchSession<B, W> {
    #[must_use]
    pub fn new(
        question: impl Into<String>,
        backend: B,
        snapshots: SnapshotWriter,
        trace: TraceWriter<W>,
    ) -> Self {
        Self {
            question: question.into(),
            backend,
            snapshots,
            trace,
            state: ResearchState::default(),
            archived: Vec::new(),
            previous_queries: Vec::new(),
            seen_urls: HashSet::new(),
        }
    }

    /// Runs the complete pipeline and records exactly one terminal failure event.
    pub async fn run(&mut self, snapshot_path: impl AsRef<Path>) -> Result<Answer> {
        let result = async {
            self.explore()
                .await
                .map_err(|error| error.at(PipelineStage::Planning))?;
            let reader = SnapshotReader::open(snapshot_path)
                .map_err(|error| error.at(PipelineStage::Setup))?;
            self.synthesize_answer(reader)
                .await
                .map_err(|error| error.at(PipelineStage::Synthesis))
        }
        .await;

        match result {
            Ok(answer) => Ok(answer),
            Err(error) => {
                let failure = TraceEvent::RunFailed {
                    error_class: error.error_class(),
                    stage: error.stage().unwrap_or(PipelineStage::Setup),
                    message: error.to_string(),
                };
                match self.trace.append(&failure) {
                    Ok(()) => Err(error),
                    Err(trace) => Err(SearchError::FailureTrace {
                        original: Box::new(error),
                        trace: Box::new(trace.at(PipelineStage::Trace)),
                    }),
                }
            }
        }
    }

    pub async fn explore(&mut self) -> Result<&ResearchState> {
        for round in 1..=EXPLORE_ROUNDS {
            self.state.round = round;
            self.state.estimated_input_tokens = estimate_snapshot_tokens(&self.archived);
            if self.state.estimated_input_tokens >= MAX_STRONG_INPUT_TOKENS {
                self.state.stop_reason = Some(StopReason::InputBudget);
                break;
            }

            let raw = self
                .backend
                .plan(&self.question, &self.archived, &self.previous_queries)
                .await
                .map_err(|error| error.at(PipelineStage::Planning))?;
            let queries = plan_queries(&raw, &self.previous_queries)
                .map_err(|error| error.at(PipelineStage::Planning))?;
            for query in &queries {
                self.trace
                    .append(&TraceEvent::Query {
                        round,
                        query: query.query.clone(),
                        gap: query.gap.clone(),
                    })
                    .map_err(|error| error.at(PipelineStage::Trace))?;
            }
            self.previous_queries
                .extend(queries.iter().map(|query| query.query.clone()));

            let mut new_results = Vec::new();
            for query in queries {
                let results = self
                    .backend
                    .search(&query.query)
                    .await
                    .map_err(|error| error.at(PipelineStage::Search))?;
                for result in results {
                    self.trace
                        .append(&TraceEvent::SearchResult {
                            round,
                            query: query.query.clone(),
                            search_result_id: result.search_result_id.clone(),
                            title: result.title.clone(),
                            url: result.url.clone(),
                            snippet: result.snippet.clone(),
                            rank: result.rank,
                        })
                        .map_err(|error| error.at(PipelineStage::Trace))?;
                    match normalized_url(&result.url) {
                        Ok(url) => {
                            if self.seen_urls.insert(url) {
                                new_results.push(result);
                            }
                        }
                        Err(error) => self
                            .trace
                            .append(&TraceEvent::ArchiveSkip {
                                search_result_id: result.search_result_id,
                                reason: error.to_string(),
                                error_class: error.error_class(),
                            })
                            .map_err(|error| error.at(PipelineStage::Trace))?,
                    }
                }
            }

            for result in new_results {
                if self.archived.len() >= MAX_SNAPSHOTS {
                    self.state.stop_reason = Some(StopReason::SnapshotLimit);
                    break;
                }
                match self.backend.crawl(&result.url).await {
                    Ok(snapshot) => {
                        self.snapshots
                            .save(&snapshot)
                            .map_err(|error| error.at(PipelineStage::Archive))?;
                        self.trace
                            .append(&TraceEvent::Archive {
                                snapshot_ref: snapshot.snapshot_ref.clone(),
                                content_hash: snapshot.content_hash.clone(),
                                final_url: snapshot.crawl.final_url.clone(),
                                char_len: snapshot.body.chars().count(),
                            })
                            .map_err(|error| error.at(PipelineStage::Trace))?;
                        self.archived.push(snapshot);
                        self.state.archived_snapshots = self.archived.len();
                    }
                    Err(error) => self
                        .trace
                        .append(&TraceEvent::ArchiveSkip {
                            search_result_id: result.search_result_id,
                            reason: error.to_string(),
                            error_class: error.error_class(),
                        })
                        .map_err(|error| error.at(PipelineStage::Trace))?,
                }
            }
            if self.state.stop_reason.is_some() {
                break;
            }
        }
        self.state
            .stop_reason
            .get_or_insert(StopReason::CompletedRounds);
        Ok(&self.state)
    }

    pub async fn synthesize_answer(&mut self, reader: SnapshotReader) -> Result<Answer> {
        if self.archived.is_empty() {
            return Err(SearchError::NoUsableSource.at(PipelineStage::Selection));
        }

        let excerpts: Vec<_> = self.archived.iter().map(make_excerpt).collect();
        for excerpt in &excerpts {
            self.trace
                .append(&TraceEvent::Excerpt {
                    snapshot_ref: excerpt.snapshot_ref.clone(),
                    content_hash: excerpt.content_hash.clone(),
                    title: excerpt.title.clone(),
                    excerpt: excerpt.excerpt.clone(),
                })
                .map_err(|error| error.at(PipelineStage::Trace))?;
        }

        let run_snapshots: HashSet<_> = excerpts
            .iter()
            .map(|excerpt| excerpt.snapshot_ref.clone())
            .collect();
        let raw = self
            .backend
            .select(&self.question, &excerpts)
            .await
            .map_err(|error| error.at(PipelineStage::Selection))?;
        let selected = select_sources(&raw, &run_snapshots)
            .map_err(|error| error.at(PipelineStage::Selection))?;
        if selected.is_empty() {
            return Err(SearchError::NoUsableSource.at(PipelineStage::Selection));
        }
        self.trace
            .append(&TraceEvent::SnapshotSelection {
                selected: selected.clone(),
            })
            .map_err(|error| error.at(PipelineStage::Trace))?;

        let mut evidence = Vec::with_capacity(selected.len());
        for selection in &selected {
            let snapshot = reader
                .get(&selection.snapshot_ref)
                .map_err(|error| error.at(PipelineStage::Selection))?
                .ok_or_else(|| {
                    SearchError::InvalidSnapshot(format!(
                        "selected snapshot missing from store: {}",
                        selection.snapshot_ref.as_str()
                    ))
                    .at(PipelineStage::Selection)
                })?;
            let expected = excerpts
                .iter()
                .find(|excerpt| excerpt.snapshot_ref == selection.snapshot_ref)
                .expect("selection was validated against excerpts");
            if snapshot.content_hash != expected.content_hash {
                return Err(SearchError::HashMismatch {
                    reference: snapshot.snapshot_ref.0.clone(),
                    expected: expected.content_hash.clone(),
                    actual: snapshot.content_hash,
                }
                .at(PipelineStage::Selection));
            }
            evidence.push(snapshot);
        }

        if estimate_snapshot_tokens(&evidence) >= MAX_STRONG_INPUT_TOKENS {
            return model_output("selected snapshot content reaches input budget")
                .map_err(|error| error.at(PipelineStage::Selection));
        }
        let supplied: HashSet<_> = evidence
            .iter()
            .map(|snapshot| snapshot.snapshot_ref.clone())
            .collect();
        drop(reader);
        let raw = self
            .backend
            .synthesize(&self.question, &evidence)
            .await
            .map_err(|error| error.at(PipelineStage::Synthesis))?;
        let answer = synthesize_answer(&raw, &supplied)
            .map_err(|error| error.at(PipelineStage::Synthesis))?;
        for claim in &answer.claims {
            self.trace
                .append(&TraceEvent::Claim {
                    text: claim.text.clone(),
                    snapshot_refs: claim.snapshot_refs.clone(),
                })
                .map_err(|error| error.at(PipelineStage::Trace))?;
        }
        self.trace
            .append(&TraceEvent::Answer {
                answer: answer.answer.clone(),
                claims: answer.claims.clone(),
            })
            .map_err(|error| error.at(PipelineStage::Trace))?;
        Ok(answer)
    }

    #[must_use]
    pub fn archived(&self) -> &[Snapshot] {
        &self.archived
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryOutput {
    queries: Vec<QueryJson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryJson {
    query: String,
    gap: String,
}

pub fn plan_queries(raw: &str, previous_queries: &[String]) -> Result<Vec<Query>> {
    let output: QueryOutput = parse_model_json(raw)?;
    if output.queries.len() != QUERIES_PER_ROUND {
        return model_output("query output must contain exactly 3 queries");
    }
    let mut seen: HashSet<String> = previous_queries
        .iter()
        .map(|query| query.trim().to_lowercase())
        .collect();
    for query in &output.queries {
        let normalized = query.query.trim().to_lowercase();
        if normalized.is_empty()
            || query.gap.trim().is_empty()
            || query.query.split_whitespace().count() > 12
            || query.query.chars().count() > MAX_QUERY_CHARS
            || !seen.insert(normalized)
        {
            return model_output("queries must be non-empty, bounded, and unique");
        }
    }
    Ok(output
        .queries
        .into_iter()
        .map(|query| Query {
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

pub fn select_sources(
    raw: &str,
    run_snapshots: &HashSet<SnapshotRef>,
) -> Result<Vec<SourceSelection>> {
    let output: SelectionOutput = parse_model_json(raw)?;
    if output.selected.len() > MAX_READ_SNAPSHOTS {
        return model_output("too many selected snapshots");
    }
    let mut seen = HashSet::new();
    if output.selected.iter().any(|selection| {
        selection.snapshot_ref.as_str().trim().is_empty()
            || selection.reason.trim().is_empty()
            || !run_snapshots.contains(&selection.snapshot_ref)
            || !seen.insert(selection.snapshot_ref.clone())
    }) {
        return model_output(
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
struct AnswerJson {
    answer: String,
    claims: Vec<ClaimJson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimJson {
    text: String,
    snapshot_refs: Vec<SnapshotRef>,
}

pub fn synthesize_answer(raw: &str, supplied: &HashSet<SnapshotRef>) -> Result<Answer> {
    let output: AnswerJson = parse_model_json(raw)?;
    if output.answer.trim().is_empty()
        || output.claims.is_empty()
        || output.claims.iter().any(|claim| {
            claim.text.trim().is_empty()
                || claim.snapshot_refs.is_empty()
                || claim
                    .snapshot_refs
                    .iter()
                    .any(|reference| !supplied.contains(reference))
        })
    {
        return model_output(
            "answer, claims, claim text, and references must be non-empty and cite supplied snapshots",
        );
    }

    Ok(Answer {
        answer: output.answer,
        claims: output
            .claims
            .into_iter()
            .map(|claim| crate::Claim {
                text: claim.text,
                snapshot_refs: claim.snapshot_refs,
            })
            .collect(),
    })
}

#[must_use]
pub fn make_excerpt(snapshot: &Snapshot) -> Excerpt {
    let first_paragraph = snapshot
        .body
        .split("\n\n")
        .find(|part| !part.trim().is_empty())
        .unwrap_or_default()
        .trim();
    Excerpt {
        snapshot_ref: snapshot.snapshot_ref.clone(),
        content_hash: snapshot.content_hash.clone(),
        title: snapshot.title.clone(),
        excerpt: format!(
            "{}\n{}\n{}",
            snapshot.title, first_paragraph, snapshot.crawl.final_url
        ),
    }
}

fn normalized_url(raw: &str) -> Result<String> {
    let mut url = Url::parse(raw).map_err(|error| SearchError::Search {
        message: format!("invalid result URL: {error}"),
    })?;
    url.set_fragment(None);
    Ok(url.to_string())
}

fn estimate_snapshot_tokens(snapshots: &[Snapshot]) -> usize {
    snapshots
        .iter()
        .map(|snapshot| snapshot.body.chars().count().div_ceil(4))
        .sum()
}

fn parse_model_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|error| SearchError::ModelOutput {
        message: format!("invalid JSON content: {error}"),
    })
}

fn model_output<T>(message: &str) -> Result<T> {
    Err(SearchError::ModelOutput {
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
    use crate::{CrawlMeta, ErrorClass, RunHeader, TracePolicy};

    #[derive(Default)]
    struct FixtureBackend {
        plan_calls: u32,
        search_calls: u32,
        crawl_calls: u32,
        selected_ref: Option<SnapshotRef>,
        synthesize_calls: u32,
        plan_error: bool,
        duplicate_urls: bool,
    }

    impl ResearchBackend for FixtureBackend {
        fn plan(
            &mut self,
            _question: &str,
            _snapshots: &[Snapshot],
            _previous_queries: &[String],
        ) -> impl Future<Output = Result<String>> {
            self.plan_calls += 1;
            let round = self.plan_calls;
            std::future::ready(if self.plan_error {
                Err(SearchError::ModelCall {
                    message: "fixture planning failure".into(),
                })
            } else {
                Ok(format!(
                    r#"{{"queries":[{{"query":"q{round}-0","gap":"g"}},{{"query":"q{round}-1","gap":"g"}},{{"query":"q{round}-2","gap":"g"}}]}}"#
                ))
            })
        }

        fn search(&mut self, query: &str) -> impl Future<Output = Result<Vec<SearchResult>>> {
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

        fn crawl(&mut self, url: &str) -> impl Future<Output = Result<Snapshot>> {
            self.crawl_calls += 1;
            let result = if url.contains("/q1-0#") {
                Err(SearchError::Fetch {
                    url: url.into(),
                    reason: "fixture skip".into(),
                })
            } else {
                Ok(Snapshot::new(
                    url.into(),
                    url.into(),
                    format!("body for {url}"),
                    CrawlMeta {
                        final_url: url.into(),
                        http_status: 200,
                        fetched_at: Utc::now(),
                    },
                ))
            };
            std::future::ready(result)
        }

        fn select(
            &mut self,
            _question: &str,
            _excerpts: &[Excerpt],
        ) -> impl Future<Output = Result<String>> {
            std::future::ready(self.selected_ref.as_ref().map_or_else(
                || {
                    Err(SearchError::ModelCall {
                        message: "unused in explore fixture".into(),
                    })
                },
                |reference| {
                    Ok(format!(
                        r#"{{"selected":[{{"snapshot_ref":"{}","reason":"x"}}]}}"#,
                        reference.as_str()
                    ))
                },
            ))
        }

        fn synthesize(
            &mut self,
            _question: &str,
            _snapshots: &[Snapshot],
        ) -> impl Future<Output = Result<String>> {
            self.synthesize_calls += 1;
            std::future::ready(self.selected_ref.as_ref().map_or_else(
                || {
                    Err(SearchError::ModelCall {
                        message: "unused in fixture".into(),
                    })
                },
                |reference| {
                    Ok(format!(
                        r#"{{"answer":"2024 年诺贝尔物理学奖授予 John Hopfield 与 Geoffrey Hinton。","claims":[{{"text":"二人因机器学习基础性发现与发明获奖。","snapshot_refs":["{}"]}}]}}"#,
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

    fn session(db: &TempDb) -> ResearchSession<FixtureBackend, Vec<u8>> {
        let trace = TraceWriter::new(
            Vec::new(),
            RunHeader {
                run_id: "fixture".into(),
                question: "question".into(),
                started_at: Utc::now(),
                policy: TracePolicy {
                    rounds: EXPLORE_ROUNDS,
                    input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                    max_snapshots: MAX_SNAPSHOTS as u32,
                },
            },
        )
        .unwrap();
        ResearchSession::new(
            "question",
            FixtureBackend::default(),
            SnapshotWriter::open(&db.0).unwrap(),
            trace,
        )
    }

    #[tokio::test]
    async fn run_records_external_selection_failure_as_the_only_terminal_event() {
        let db = TempDb::new("run-selection-failure");
        let mut session = session(&db);

        let error = session.run(&db.0).await.unwrap_err();

        assert_eq!(error.stage(), Some(PipelineStage::Selection));
        assert_eq!(error.error_class(), ErrorClass::External);
        let trace = String::from_utf8(session.trace.into_inner()).unwrap();
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
                stage: PipelineStage::Selection,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn run_records_internal_snapshot_setup_failure() {
        let db = TempDb::new("run-setup-writer");
        let missing = TempDb::new("run-setup-missing");
        let mut session = session(&db);

        let error = session.run(&missing.0).await.unwrap_err();

        assert_eq!(error.stage(), Some(PipelineStage::Setup));
        assert_eq!(error.error_class(), ErrorClass::Internal);
        let trace = String::from_utf8(session.trace.into_inner()).unwrap();
        let last = serde_json::from_str::<TraceEvent>(trace.lines().last().unwrap()).unwrap();
        assert!(matches!(
            last,
            TraceEvent::RunFailed {
                error_class: ErrorClass::Internal,
                stage: PipelineStage::Setup,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn run_preserves_original_and_trace_failures() {
        let db = TempDb::new("run-trace-failure");
        let trace = TraceWriter::new(
            FailAfterHeader {
                header_written: false,
            },
            RunHeader {
                run_id: "trace-failure".into(),
                question: "question".into(),
                started_at: Utc::now(),
                policy: TracePolicy {
                    rounds: EXPLORE_ROUNDS,
                    input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                    max_snapshots: MAX_SNAPSHOTS as u32,
                },
            },
        )
        .unwrap();
        let backend = FixtureBackend {
            plan_error: true,
            ..FixtureBackend::default()
        };
        let mut session = ResearchSession::new(
            "question",
            backend,
            SnapshotWriter::open(&db.0).unwrap(),
            trace,
        );

        let error = session.run(&db.0).await.unwrap_err();

        match error {
            SearchError::FailureTrace { original, trace } => {
                assert_eq!(original.stage(), Some(PipelineStage::Planning));
                assert_eq!(original.error_class(), ErrorClass::External);
                assert_eq!(trace.stage(), Some(PipelineStage::Trace));
                assert_eq!(trace.error_class(), ErrorClass::Internal);
            }
            other => panic!("expected both failures, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn explore_runs_three_rounds_deduplicates_and_traces_archive_skip() {
        let db = TempDb::new("explore");
        let mut session = session(&db);

        let state = session.explore().await.unwrap().clone();

        assert_eq!(state.round, EXPLORE_ROUNDS);
        assert_eq!(state.stop_reason, Some(StopReason::CompletedRounds));
        assert_eq!(session.backend.plan_calls, EXPLORE_ROUNDS);
        assert_eq!(session.backend.search_calls, EXPLORE_ROUNDS * 3);
        assert_eq!(session.backend.crawl_calls, EXPLORE_ROUNDS * 3);
        assert_eq!(session.archived.len(), 8);
        let trace = String::from_utf8(session.trace.into_inner()).unwrap();
        assert_eq!(trace.matches("\"type\":\"archive_skip\"").count(), 1);
        let result: serde_json::Value = trace
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .find(|event: &serde_json::Value| event["type"] == "search_result")
            .unwrap();
        assert_eq!(result["round"], 1);
        assert_eq!(result["query"], "q1-0");
    }

    #[tokio::test]
    async fn explore_runs_all_rounds_when_every_search_repeats_the_same_url() {
        let db = TempDb::new("duplicate-search-results");
        let mut session = session(&db);
        session.backend.duplicate_urls = true;

        let state = session.explore().await.unwrap().clone();

        assert_eq!(state.round, EXPLORE_ROUNDS);
        assert_eq!(state.stop_reason, Some(StopReason::CompletedRounds));
        assert_eq!(session.backend.plan_calls, EXPLORE_ROUNDS);
        assert_eq!(session.backend.search_calls, EXPLORE_ROUNDS * 3);
        assert_eq!(session.backend.crawl_calls, 1);
        assert_eq!(session.archived.len(), 1);
    }

    #[tokio::test]
    async fn explore_stops_before_model_call_when_input_budget_is_exceeded() {
        let db = TempDb::new("budget");
        let mut session = session(&db);
        session.archived.push(Snapshot::new(
            "https://example.com/large".into(),
            "large".into(),
            "x".repeat(MAX_STRONG_INPUT_TOKENS * 4),
            CrawlMeta {
                final_url: "https://example.com/large".into(),
                http_status: 200,
                fetched_at: Utc::now(),
            },
        ));

        let state = session.explore().await.unwrap().clone();

        assert_eq!(state.stop_reason, Some(StopReason::InputBudget));
        assert_eq!(session.backend.plan_calls, 0);
    }

    #[tokio::test]
    async fn synthesize_stops_before_model_call_at_input_budget() {
        let db = TempDb::new("synthesize-budget");
        let mut session = session(&db);
        let snapshot = Snapshot::new(
            "https://example.com/large".into(),
            "large".into(),
            "x".repeat(MAX_STRONG_INPUT_TOKENS * 4),
            CrawlMeta {
                final_url: "https://example.com/large".into(),
                http_status: 200,
                fetched_at: Utc::now(),
            },
        );
        session.snapshots.save(&snapshot).unwrap();
        session.backend.selected_ref = Some(snapshot.snapshot_ref.clone());
        session.archived.push(snapshot);
        let reader = SnapshotReader::open(&db.0).unwrap();

        let error = session.synthesize_answer(reader).await.unwrap_err();
        assert_eq!(error.stage(), Some(PipelineStage::Selection));
        assert!(matches!(
            error,
            SearchError::Staged { source, .. }
                if matches!(*source, SearchError::ModelOutput { .. })
        ));
        assert_eq!(session.backend.synthesize_calls, 0);
    }

    #[tokio::test]
    async fn nobel_fixture_runs_end_to_end_and_replays_audit_trace() {
        let db = TempDb::new("nobel-e2e");
        let mut session = session(&db);
        session.question = "2024 年诺贝尔物理学奖颁给了谁？获奖理由是什么？".into();

        session.explore().await.unwrap();
        let selected = session.archived[0].snapshot_ref.clone();
        session.backend.selected_ref = Some(selected.clone());
        let answer = session
            .synthesize_answer(SnapshotReader::open(&db.0).unwrap())
            .await
            .unwrap();

        assert!(answer.answer.contains("John Hopfield"));
        assert!(answer.answer.contains("Geoffrey Hinton"));
        assert_eq!(answer.claims[0].snapshot_refs, vec![selected]);

        let trace = String::from_utf8(session.trace.into_inner()).unwrap();
        let events: Vec<TraceEvent> = trace
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut archived = HashSet::new();
        for event in &events {
            match event {
                TraceEvent::Archive { snapshot_ref, .. } => {
                    archived.insert(snapshot_ref.clone());
                }
                TraceEvent::Excerpt { snapshot_ref, .. } => {
                    assert!(archived.contains(snapshot_ref));
                }
                TraceEvent::SnapshotSelection { selected } => {
                    assert!(
                        selected
                            .iter()
                            .all(|source| archived.contains(&source.snapshot_ref))
                    );
                }
                TraceEvent::Claim { snapshot_refs, .. } => {
                    assert!(
                        snapshot_refs
                            .iter()
                            .all(|reference| archived.contains(reference))
                    );
                }
                TraceEvent::Answer { claims, .. } => {
                    assert!(
                        claims
                            .iter()
                            .flat_map(|claim| &claim.snapshot_refs)
                            .all(|reference| archived.contains(reference))
                    );
                }
                _ => {}
            }
        }
        assert!(matches!(events.first(), Some(TraceEvent::RunHeader { .. })));
        assert!(matches!(events.last(), Some(TraceEvent::Answer { .. })));
    }

    #[test]
    fn prompt_shaped_two_field_selection_is_accepted() {
        let reference = SnapshotRef("snapshot:web/own".into());
        let run_snapshots = HashSet::from([reference.clone()]);
        let raw = format!(
            r#"{{"selected":[{{"snapshot_ref":"{}","reason":"direct evidence"}}]}}"#,
            reference.as_str()
        );

        let selected = select_sources(&raw, &run_snapshots).unwrap();
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
            select_sources(&raw, &run_snapshots),
            Err(SearchError::ModelOutput { .. })
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
            select_sources(&duplicate, &HashSet::from([reference])),
            Err(SearchError::ModelOutput { .. })
        ));

        let empty = SnapshotRef(String::new());
        assert!(matches!(
            select_sources(
                r#"{"selected":[{"snapshot_ref":"","reason":"x"}]}"#,
                &HashSet::from([empty])
            ),
            Err(SearchError::ModelOutput { .. })
        ));
    }

    #[test]
    fn valid_single_claim_is_accepted() {
        let own = SnapshotRef("snapshot:web/own".into());
        let supplied = HashSet::from([own.clone()]);
        let raw = format!(
            r#"{{"answer":"supported","claims":[{{"text":"supported","snapshot_refs":["{}"]}}]}}"#,
            own.as_str()
        );
        assert!(synthesize_answer(&raw, &supplied).is_ok());
    }

    #[test]
    fn empty_claims_are_rejected() {
        let supplied = HashSet::new();
        assert!(matches!(
            synthesize_answer(r#"{"answer":"unsupported","claims":[]}"#, &supplied),
            Err(SearchError::ModelOutput { .. })
        ));
    }

    #[test]
    fn model_output_cannot_escape_run_or_citation_set() {
        let own = SnapshotRef("snapshot:web/own".into());
        let foreign = SnapshotRef("snapshot:web/foreign".into());
        let run_snapshots = HashSet::from([own.clone()]);
        let selection = format!(
            r#"{{"selected":[{{"snapshot_ref":"{}","reason":"x"}}]}}"#,
            foreign.as_str()
        );
        assert!(matches!(
            select_sources(&selection, &run_snapshots),
            Err(SearchError::ModelOutput { .. })
        ));

        let answer = format!(
            r#"{{"answer":"x","claims":[{{"text":"x","snapshot_refs":["{}"]}}]}}"#,
            foreign.as_str()
        );
        assert!(matches!(
            synthesize_answer(&answer, &HashSet::from([own])),
            Err(SearchError::ModelOutput { .. })
        ));
    }
}

// ponytail: one sequential module only; split strategies when a second policy exists.

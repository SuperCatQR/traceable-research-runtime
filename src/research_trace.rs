//! Append-only, one-file-per-run JSONL audit trace.

use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ComposedResearchClaim, ErrorClass, ExplorationStopReason, FrozenResearchBrief,
    ModelKnowledgeDraft, ResearchAnswerComparison, ResearchAnswerStyle, ResearchError,
    ResearchStage, Result, SearchEngine, SearchEngineAttemptOutcome, SearchEngineUnavailability,
    SnapshotRef, validate_decision_rationale,
};

pub const TRACE_SCHEMA_VERSION: u32 = 7;

impl Default for TracePolicy {
    fn default() -> Self {
        Self {
            rounds: 3,
            input_budget: 1_000_000,
            max_snapshots: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TracePolicy {
    pub rounds: u32,
    pub input_budget: u32,
    pub max_snapshots: u32,
}

pub fn validate_trace_policy(policy: &TracePolicy) -> std::result::Result<(), String> {
    use crate::research_run::{
        MAX_EXPLORE_ROUNDS, MAX_SNAPSHOTS, MAX_STRONG_INPUT_TOKENS, MIN_EXPLORE_ROUNDS,
    };

    if !(MIN_EXPLORE_ROUNDS..=MAX_EXPLORE_ROUNDS).contains(&policy.rounds) {
        return Err(format!(
            "policy rounds must be between {MIN_EXPLORE_ROUNDS} and {MAX_EXPLORE_ROUNDS}"
        ));
    }
    if policy.input_budget == 0 || policy.input_budget as usize > MAX_STRONG_INPUT_TOKENS {
        return Err(format!(
            "policy input_budget must be between 1 and {MAX_STRONG_INPUT_TOKENS}"
        ));
    }
    if policy.max_snapshots == 0 || policy.max_snapshots as usize > MAX_SNAPSHOTS {
        return Err(format!(
            "policy max_snapshots must be between 1 and {MAX_SNAPSHOTS}"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHeader {
    pub run_id: String,
    pub clarification_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub brief: FrozenResearchBrief,
    pub started_at: DateTime<Utc>,
    pub policy: TracePolicy,
    #[serde(default)]
    pub answer_style: ResearchAnswerStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSelection {
    pub snapshot_ref: SnapshotRef,
    pub reason: String,
}

/// Current v7 trace event payload. The writer persists it inside a sequenced envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceEvent {
    RunHeader {
        schema_version: u32,
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        clarification_id: String,
        brief: Box<FrozenResearchBrief>,
        started_at: DateTime<Utc>,
        policy: TracePolicy,
        #[serde(default)]
        answer_style: ResearchAnswerStyle,
    },
    ModelCall {
        operation: String,
        round: u32,
        input_snapshot_refs: Vec<SnapshotRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_chars: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_class: Option<ErrorClass>,
    },
    KnowledgeDraft {
        draft: ModelKnowledgeDraft,
    },
    #[serde(rename = "query")]
    SearchQuery {
        round: u32,
        query: String,
        gap: String,
    },
    SearchAttemptCompleted {
        round: u32,
        query: String,
        engine: SearchEngine,
        outcome: SearchEngineAttemptOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http_status: Option<u16>,
    },
    SearchFallbackActivated {
        round: u32,
        query: String,
        from_engine: SearchEngine,
        to_engine: SearchEngine,
        reason: SearchEngineUnavailability,
    },
    SearchResult {
        round: u32,
        query: String,
        search_engine: SearchEngine,
        search_result_id: String,
        title: String,
        url: String,
        snippet: String,
        rank: u32,
    },
    Archive {
        snapshot_ref: SnapshotRef,
        content_hash: String,
        final_url: String,
        char_len: usize,
    },
    ArchiveSkip {
        search_result_id: String,
        reason: String,
        error_class: ErrorClass,
    },
    #[serde(rename = "excerpt")]
    SnapshotNavigationExcerpt {
        snapshot_ref: SnapshotRef,
        content_hash: String,
        title: String,
        excerpt: String,
    },
    SnapshotSelection {
        selected: Vec<SourceSelection>,
    },
    #[serde(rename = "claim")]
    ResearchClaim {
        text: String,
        #[serde(default)]
        origin: crate::ResearchClaimOrigin,
        #[serde(default)]
        snapshot_refs: Vec<SnapshotRef>,
        #[serde(default)]
        rationale: String,
    },
    #[serde(rename = "answer")]
    ComposedResearchAnswer {
        answer: String,
        claims: Vec<ComposedResearchClaim>,
        #[serde(default)]
        comparison: ResearchAnswerComparison,
    },
    RoundCompleted {
        round: u32,
        previous_queries: Vec<String>,
        archived_snapshot_refs: Vec<SnapshotRef>,
    },
    ExplorationStopped {
        completed_round: u32,
        reason: ExplorationStopReason,
    },
    RunFailed {
        error_class: ErrorClass,
        stage: ResearchStage,
        message: String,
    },
}

/// One persisted v7 trace line. Sequence is the replay order; occurred_at is
/// review metadata and is kept nondecreasing by the writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEventEnvelope {
    pub sequence: u64,
    pub occurred_at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: TraceEvent,
}

#[derive(Serialize)]
struct BorrowedTraceEventEnvelope<'a> {
    sequence: u64,
    occurred_at: DateTime<Utc>,
    #[serde(flatten)]
    event: &'a TraceEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunReplay {
    pub completed_round: u32,
    pub previous_queries: Vec<String>,
    pub archived_snapshot_refs: Vec<SnapshotRef>,
    pub model_knowledge_draft: Option<ModelKnowledgeDraft>,
    pub exploration_stop_reason: Option<ExplorationStopReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayedTrace {
    pub header: RunHeader,
    pub events: Vec<TraceEventEnvelope>,
    pub run_replay: RunReplay,
}

#[derive(Default)]
struct ReplayedSearchFlow {
    google_outcome: Option<SearchEngineAttemptOutcome>,
    fallback_reason: Option<SearchEngineUnavailability>,
    bing_outcome: Option<SearchEngineAttemptOutcome>,
    observed_result_count: u32,
}

impl ReplayedTrace {
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.events.last().is_some_and(|envelope| {
            matches!(
                envelope.event,
                TraceEvent::ComposedResearchAnswer { .. } | TraceEvent::RunFailed { .. }
            )
        })
    }
}

/// Owns the only write direction: construction writes the mandatory first line,
/// then `append` only adds subsequent events.
pub struct TraceWriter<W: Write> {
    sink: W,
    schema_version: u32,
    next_sequence: u64,
    last_occurred_at: Option<DateTime<Utc>>,
}

impl<W: Write> TraceWriter<W> {
    pub fn new(sink: W, header: RunHeader) -> Result<Self> {
        let mut writer = Self {
            sink,
            schema_version: TRACE_SCHEMA_VERSION,
            next_sequence: 1,
            last_occurred_at: None,
        };
        writer.write_event(&TraceEvent::RunHeader {
            schema_version: TRACE_SCHEMA_VERSION,
            run_id: header.run_id,
            session_id: header.session_id,
            turn: header.turn,
            clarification_id: header.clarification_id,
            brief: Box::new(header.brief),
            started_at: header.started_at,
            policy: header.policy,
            answer_style: header.answer_style,
        })?;
        Ok(writer)
    }

    pub fn append(&mut self, event: &TraceEvent) -> Result<()> {
        if matches!(event, TraceEvent::RunHeader { .. }) {
            return Err(ResearchError::Trace(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "run_header is only valid as the first trace line",
            )));
        }
        validate_trace_event_for_schema(self.schema_version, event)?;
        self.write_event(event)
    }

    pub fn into_inner(self) -> W {
        self.sink
    }

    fn write_event(&mut self, event: &TraceEvent) -> Result<()> {
        let candidate_time = match event {
            TraceEvent::RunHeader { started_at, .. } => *started_at,
            _ => Utc::now(),
        };
        let occurred_at = self
            .last_occurred_at
            .map_or(candidate_time, |last| last.max(candidate_time));
        let envelope = BorrowedTraceEventEnvelope {
            sequence: self.next_sequence,
            occurred_at,
            event,
        };
        serde_json::to_writer(&mut self.sink, &envelope).map_err(std::io::Error::other)?;
        self.sink.write_all(b"\n")?;
        self.sink.flush()?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| invalid_trace("trace sequence overflow"))?;
        self.last_occurred_at = Some(occurred_at);
        Ok(())
    }
}

impl TraceWriter<BufWriter<File>> {
    /// Creates `trace_dir/<run_id>.jsonl` without overwriting an existing run.
    pub fn create(trace_dir: impl AsRef<Path>, header: RunHeader) -> Result<Self> {
        validate_run_id(&header.run_id)?;
        ensure_v7_trace_directory(trace_dir.as_ref())?;
        let path = trace_dir.as_ref().join(format!("{}.jsonl", header.run_id));
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)?;
        Self::new(BufWriter::new(file), header)
    }

    /// Reopens only a matching, non-terminal v7 trace and returns its last committed round.
    pub(crate) fn resume(
        trace_dir: impl AsRef<Path>,
        header: &RunHeader,
    ) -> Result<(Self, RunReplay)> {
        validate_run_id(&header.run_id)?;
        require_v7_trace_directory(trace_dir.as_ref())?;
        let path = trace_dir.as_ref().join(format!("{}.jsonl", header.run_id));
        let (replay, schema_version, next_sequence, last_occurred_at) = replay_run(&path, header)?;
        let file = OpenOptions::new().append(true).open(path)?;
        Ok((
            Self {
                sink: BufWriter::new(file),
                schema_version,
                next_sequence,
                last_occurred_at: Some(last_occurred_at),
            },
            replay,
        ))
    }
}

fn trace_schema_marker_path(trace_dir: &Path) -> std::path::PathBuf {
    trace_dir.join(format!(".trace-schema-v{TRACE_SCHEMA_VERSION}"))
}

fn ensure_v7_trace_directory(trace_dir: &Path) -> Result<()> {
    fs::create_dir_all(trace_dir)?;
    let marker = trace_schema_marker_path(trace_dir);
    if marker.is_file() {
        return Ok(());
    }
    for entry in fs::read_dir(trace_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".trace-schema-v") {
            return Err(invalid_trace("trace directory schema marker mismatch"));
        }
        if entry.path().extension() == Some(OsStr::new("jsonl")) {
            return Err(invalid_trace("v7 writer refuses unmarked trace data"));
        }
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && marker.is_file() => {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn require_v7_trace_directory(trace_dir: &Path) -> Result<()> {
    if trace_schema_marker_path(trace_dir).is_file() {
        Ok(())
    } else {
        Err(invalid_trace("trace directory is not marked for v7"))
    }
}

fn replay_run(path: &Path, expected: &RunHeader) -> Result<(RunReplay, u32, u64, DateTime<Utc>)> {
    let replayed = replay_trace(path)?;
    if &replayed.header != expected {
        return Err(invalid_trace(
            "existing trace header does not match frozen run",
        ));
    }
    if replayed.is_terminal() {
        return Err(invalid_trace("trace is already terminal"));
    }
    let last = replayed
        .events
        .last()
        .ok_or_else(|| invalid_trace("missing run_header"))?;
    let next_sequence = last
        .sequence
        .checked_add(1)
        .ok_or_else(|| invalid_trace("trace sequence overflow"))?;
    Ok((
        replayed.run_replay,
        TRACE_SCHEMA_VERSION,
        next_sequence,
        last.occurred_at,
    ))
}

pub fn replay_trace(path: impl AsRef<Path>) -> Result<ReplayedTrace> {
    let contents = fs::read_to_string(path)?;
    if !contents.ends_with('\n') {
        return Err(invalid_trace("truncated trace event"));
    }
    let first_line = contents
        .lines()
        .next()
        .ok_or_else(|| invalid_trace("missing run_header"))?;
    let first_envelope: TraceEventEnvelope = serde_json::from_str(first_line)
        .map_err(|error| invalid_trace(format!("line 1: {error}")))?;
    let header = project_v7_run_header(&first_envelope.event)?;
    let mut events = Vec::new();
    let mut run_replay = RunReplay::default();
    let mut last_sequence = 0_u64;
    let mut last_occurred_at = None;
    let mut terminal_seen = false;
    let mut exploration_stopped_seen = false;
    let mut search_flows = HashMap::<(u32, String), ReplayedSearchFlow>::new();
    let mut declared_queries = Vec::<(u32, String)>::new();
    for (index, line) in contents.lines().enumerate() {
        if line.is_empty() {
            return Err(invalid_trace(format!(
                "empty trace event at line {}",
                index + 1
            )));
        }
        let envelope: TraceEventEnvelope = serde_json::from_str(line)
            .map_err(|error| invalid_trace(format!("line {}: {error}", index + 1)))?;
        if envelope.sequence != last_sequence + 1 {
            return Err(invalid_trace("trace sequence is not contiguous"));
        }
        if last_occurred_at.is_some_and(|last| envelope.occurred_at < last) {
            return Err(invalid_trace("trace occurred_at moved backwards"));
        }
        if terminal_seen {
            return Err(invalid_trace(
                "trace contains an event after its terminal event",
            ));
        }
        validate_trace_event_for_schema(TRACE_SCHEMA_VERSION, &envelope.event)?;
        match &envelope.event {
            TraceEvent::RunHeader { .. } if index == 0 => {}
            TraceEvent::RunHeader { .. } => return Err(invalid_trace("duplicate run_header")),
            TraceEvent::SearchQuery { round, query, .. } => {
                if exploration_stopped_seen
                    || *round == 0
                    || *round > header.policy.rounds
                    || *round != run_replay.completed_round + 1
                    || declared_queries
                        .iter()
                        .any(|(_, declared_query)| declared_query == query)
                    || search_flows
                        .insert((*round, query.clone()), ReplayedSearchFlow::default())
                        .is_some()
                {
                    return Err(invalid_trace("invalid or duplicate search query"));
                }
                declared_queries.push((*round, query.clone()));
            }
            TraceEvent::SearchAttemptCompleted {
                round,
                query,
                engine,
                outcome,
                ..
            } => {
                if exploration_stopped_seen {
                    return Err(invalid_trace("search attempt follows exploration_stopped"));
                }
                if matches!(
                    outcome,
                    SearchEngineAttemptOutcome::Completed { valid_result_count }
                        if *valid_result_count > 10
                ) {
                    return Err(invalid_trace("search attempt exceeds the result limit"));
                }
                let flow = search_flows
                    .get_mut(&(*round, query.clone()))
                    .ok_or_else(|| invalid_trace("search attempt has no matching query"))?;
                match engine {
                    SearchEngine::Google
                        if flow.google_outcome.is_none()
                            && flow.fallback_reason.is_none()
                            && flow.bing_outcome.is_none() =>
                    {
                        flow.google_outcome = Some(outcome.clone());
                    }
                    SearchEngine::Bing
                        if flow.bing_outcome.is_none() && flow.fallback_reason.is_some() =>
                    {
                        flow.bing_outcome = Some(outcome.clone());
                    }
                    _ => return Err(invalid_trace("invalid search engine attempt order")),
                }
            }
            TraceEvent::SearchFallbackActivated {
                round,
                query,
                from_engine,
                to_engine,
                reason,
            } => {
                if exploration_stopped_seen {
                    return Err(invalid_trace("search fallback follows exploration_stopped"));
                }
                let flow = search_flows
                    .get_mut(&(*round, query.clone()))
                    .ok_or_else(|| invalid_trace("search fallback has no matching query"))?;
                if *from_engine != SearchEngine::Google
                    || *to_engine != SearchEngine::Bing
                    || flow.fallback_reason.is_some()
                    || flow.bing_outcome.is_some()
                    || !matches!(
                        flow.google_outcome,
                        Some(SearchEngineAttemptOutcome::Unavailable { reason: unavailable })
                            if unavailable == *reason
                    )
                {
                    return Err(invalid_trace("invalid search fallback transition"));
                }
                flow.fallback_reason = Some(*reason);
            }
            TraceEvent::SearchResult {
                round,
                query,
                search_engine,
                search_result_id,
                url,
                rank,
                ..
            } => {
                if exploration_stopped_seen {
                    return Err(invalid_trace("search result follows exploration_stopped"));
                }
                let flow = search_flows
                    .get_mut(&(*round, query.clone()))
                    .ok_or_else(|| invalid_trace("search result has no matching query"))?;
                let selected_outcome = match search_engine {
                    SearchEngine::Google if flow.fallback_reason.is_none() => {
                        flow.google_outcome.as_ref()
                    }
                    SearchEngine::Bing if flow.fallback_reason.is_some() => {
                        flow.bing_outcome.as_ref()
                    }
                    _ => None,
                };
                let Some(SearchEngineAttemptOutcome::Completed { valid_result_count }) =
                    selected_outcome
                else {
                    return Err(invalid_trace(
                        "search result does not follow a completed matching attempt",
                    ));
                };
                if flow.observed_result_count >= *valid_result_count
                    || *rank != flow.observed_result_count + 1
                {
                    return Err(invalid_trace("search result count or rank is invalid"));
                }
                if *search_result_id != crate::search_result_id(query, url) {
                    return Err(invalid_trace("search result identity is invalid"));
                }
                flow.observed_result_count += 1;
            }
            TraceEvent::RoundCompleted {
                round,
                previous_queries,
                archived_snapshot_refs,
            } => {
                if exploration_stopped_seen {
                    return Err(invalid_trace("round_completed follows exploration_stopped"));
                }
                if *round != run_replay.completed_round + 1 || *round > header.policy.rounds {
                    return Err(invalid_trace("invalid round_completed sequence"));
                }
                validate_completed_round_search_flows(&search_flows, *round)?;
                let round_query_count = declared_queries
                    .iter()
                    .filter(|(query_round, _)| query_round == round)
                    .count();
                let declared_query_texts = declared_queries
                    .iter()
                    .map(|(_, query)| query.as_str())
                    .collect::<Vec<_>>();
                if round_query_count != crate::research_run::QUERIES_PER_ROUND
                    || previous_queries
                        .iter()
                        .map(String::as_str)
                        .ne(declared_query_texts)
                {
                    return Err(invalid_trace(
                        "round_completed does not match its declared search queries",
                    ));
                }
                run_replay.completed_round = *round;
                run_replay.previous_queries.clone_from(previous_queries);
                run_replay
                    .archived_snapshot_refs
                    .clone_from(archived_snapshot_refs);
            }
            TraceEvent::KnowledgeDraft { draft } => {
                if run_replay
                    .model_knowledge_draft
                    .replace(draft.clone())
                    .is_some()
                {
                    return Err(invalid_trace("trace contains multiple knowledge drafts"));
                }
            }
            TraceEvent::ExplorationStopped {
                completed_round,
                reason,
            } => {
                let reason_matches_progress = match reason {
                    ExplorationStopReason::CompletedRounds => {
                        *completed_round == header.policy.rounds
                    }
                    ExplorationStopReason::NoNewUrls => *completed_round > 0,
                    ExplorationStopReason::InputBudget | ExplorationStopReason::SnapshotLimit => {
                        true
                    }
                };
                if exploration_stopped_seen
                    || *completed_round != run_replay.completed_round
                    || !reason_matches_progress
                {
                    return Err(invalid_trace("invalid exploration_stopped event"));
                }
                exploration_stopped_seen = true;
                run_replay.exploration_stop_reason = Some(*reason);
            }
            TraceEvent::ComposedResearchAnswer { .. } => {
                if !exploration_stopped_seen || run_replay.model_knowledge_draft.is_none() {
                    return Err(invalid_trace(
                        "answer requires a knowledge draft and exploration stop reason",
                    ));
                }
                terminal_seen = true;
            }
            TraceEvent::RunFailed { .. } => {
                terminal_seen = true;
            }
            _ => {}
        }
        last_sequence = envelope.sequence;
        last_occurred_at = Some(envelope.occurred_at);
        events.push(envelope);
    }
    if events.is_empty() || !matches!(events[0].event, TraceEvent::RunHeader { .. }) {
        return Err(invalid_trace("first trace event is not run_header"));
    }
    Ok(ReplayedTrace {
        header,
        events,
        run_replay,
    })
}

fn validate_completed_round_search_flows(
    search_flows: &HashMap<(u32, String), ReplayedSearchFlow>,
    completed_round: u32,
) -> Result<()> {
    for ((round, _), flow) in search_flows {
        if *round != completed_round {
            continue;
        }
        let selected_outcome = flow
            .bing_outcome
            .as_ref()
            .or(flow.google_outcome.as_ref())
            .ok_or_else(|| invalid_trace("completed round contains an unattempted query"))?;
        let SearchEngineAttemptOutcome::Completed { valid_result_count } = selected_outcome else {
            return Err(invalid_trace(
                "completed round contains an unsuccessful search query",
            ));
        };
        if *valid_result_count != flow.observed_result_count {
            return Err(invalid_trace(
                "completed search result count differs from its attempt",
            ));
        }
    }
    Ok(())
}

fn project_v7_run_header(event: &TraceEvent) -> Result<RunHeader> {
    let TraceEvent::RunHeader {
        schema_version,
        run_id,
        session_id,
        turn,
        clarification_id,
        brief,
        started_at,
        policy,
        answer_style,
    } = event
    else {
        return Err(invalid_trace("first trace event is not run_header"));
    };
    if *schema_version != TRACE_SCHEMA_VERSION {
        return Err(invalid_trace(format!(
            "unsupported trace schema version {schema_version}"
        )));
    }
    validate_run_id(run_id)?;
    validate_trace_policy(policy).map_err(invalid_trace)?;
    if clarification_id != brief.clarification_id()
        || session_id.is_some() != turn.is_some()
        || matches!(turn, Some(0))
    {
        return Err(invalid_trace("invalid v7 run_header fields"));
    }
    Ok(RunHeader {
        run_id: run_id.clone(),
        clarification_id: clarification_id.clone(),
        session_id: session_id.clone(),
        turn: *turn,
        brief: brief.as_ref().clone(),
        started_at: *started_at,
        policy: policy.clone(),
        answer_style: *answer_style,
    })
}

pub fn validate_trace_event_for_schema(schema_version: u32, event: &TraceEvent) -> Result<()> {
    if schema_version != TRACE_SCHEMA_VERSION {
        return Ok(());
    }

    let validate = |rationale: &str| validate_decision_rationale(rationale).map_err(invalid_trace);
    match event {
        TraceEvent::KnowledgeDraft { draft } => validate(&draft.basis_summary),
        TraceEvent::SearchQuery { gap, .. } => validate(gap),
        TraceEvent::SnapshotSelection { selected } => {
            for selection in selected {
                validate(&selection.reason)?;
            }
            Ok(())
        }
        TraceEvent::ResearchClaim { rationale, .. } => validate(rationale),
        TraceEvent::ComposedResearchAnswer {
            claims, comparison, ..
        } => {
            validate(&comparison.synthesis_rationale)?;
            for claim in claims {
                validate(&claim.rationale)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn invalid_trace(message: impl Into<String>) -> ResearchError {
    ResearchError::Trace(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

fn validate_run_id(run_id: &str) -> Result<()> {
    let path = Path::new(run_id);
    if run_id.is_empty()
        || run_id == "."
        || run_id == ".."
        || path.file_name() != Some(OsStr::new(run_id))
    {
        return Err(ResearchError::Trace(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "run_id must be one non-empty file-name component",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use chrono::TimeZone;
    use serde_json::Value;

    use super::*;
    use crate::{RESEARCH_BRIEF_SCHEMA_VERSION, ResearchBrief, ResearchScope};

    fn header(run_id: &str) -> RunHeader {
        let brief = ResearchBrief {
            schema_version: RESEARCH_BRIEF_SCHEMA_VERSION,
            original_question: "original question".into(),
            research_question: "focused question".into(),
            desired_output: None,
            scope: ResearchScope::default(),
            source_constraints: Vec::new(),
            accepted_assumptions: Vec::new(),
        };
        let content_hash = brief.content_hash().unwrap();
        let frozen_at = Utc.with_ymd_and_hms(2026, 7, 11, 9, 59, 0).unwrap();
        let brief = FrozenResearchBrief::new(
            brief,
            "original question",
            "clarification-test".into(),
            &content_hash,
            frozen_at,
        )
        .unwrap();
        RunHeader {
            run_id: run_id.into(),
            clarification_id: "clarification-test".into(),
            session_id: None,
            turn: None,
            brief,
            started_at: Utc.with_ymd_and_hms(2026, 7, 11, 10, 0, 0).unwrap(),
            policy: TracePolicy {
                rounds: 3,
                input_budget: 800_000,
                max_snapshots: 300,
            },
            answer_style: ResearchAnswerStyle::WebFirst,
        }
    }

    fn replay_error_for_events(tag: &str, events: &[TraceEvent]) -> ResearchError {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "traceable-search-{tag}-{}-{nonce}.jsonl",
            std::process::id()
        ));
        let mut writer = TraceWriter::new(Vec::new(), header(tag)).unwrap();
        for event in events {
            writer.append(event).unwrap();
        }
        fs::write(&path, writer.into_inner()).unwrap();
        let error = replay_trace(&path).unwrap_err();
        fs::remove_file(path).unwrap();
        error
    }

    #[test]
    fn header_is_first_and_events_are_jsonl() {
        let mut writer = TraceWriter::new(Vec::new(), header("r-test")).unwrap();
        writer
            .append(&TraceEvent::SearchQuery {
                round: 1,
                query: "rust sqlite".into(),
                gap: "need primary source".into(),
            })
            .unwrap();
        writer
            .append(&TraceEvent::ArchiveSkip {
                search_result_id: "sr-1".into(),
                reason: "body_not_in_dom".into(),
                error_class: ErrorClass::External,
            })
            .unwrap();

        let bytes = writer.into_inner();
        let lines: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "run_header");
        assert_eq!(lines[0]["schema_version"], TRACE_SCHEMA_VERSION);
        assert_eq!(lines[0]["sequence"], 1);
        assert_eq!(lines[1]["sequence"], 2);
        assert_eq!(lines[2]["sequence"], 3);
        let occurred_at = lines
            .iter()
            .map(|line| {
                DateTime::parse_from_rfc3339(line["occurred_at"].as_str().unwrap())
                    .unwrap()
                    .with_timezone(&Utc)
            })
            .collect::<Vec<_>>();
        assert!(occurred_at.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(
            lines[0]["brief"]["brief"]["original_question"],
            "original question"
        );
        assert!(lines[0]["brief"]["frozen_at"].is_string());
        assert!(lines[0]["brief"].get("confirmed_at").is_none());
        assert_eq!(lines[1]["type"], "query");
        assert_eq!(lines[2]["type"], "archive_skip");
        assert_eq!(lines[2]["error_class"], "external");
    }

    #[test]
    fn replay_trace_returns_only_validated_v7_envelopes() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "traceable-search-replay-v7-{}-{nonce}",
            std::process::id()
        ));
        let expected_header = header("r-replay-v7");
        let mut writer = TraceWriter::create(&dir, expected_header.clone()).unwrap();
        writer
            .append(&TraceEvent::SearchQuery {
                round: 1,
                query: "rust audit trace".into(),
                gap: "Need a primary source describing the audit contract.".into(),
            })
            .unwrap();
        drop(writer);

        let replayed = replay_trace(dir.join("r-replay-v7.jsonl")).unwrap();

        assert_eq!(replayed.header, expected_header);
        assert_eq!(replayed.events.len(), 2);
        assert_eq!(replayed.events[0].sequence, 1);
        assert_eq!(replayed.events[1].sequence, 2);
        assert!(matches!(
            replayed.events[1].event,
            TraceEvent::SearchQuery { .. }
        ));
        assert_eq!(replayed.run_replay, RunReplay::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_fallback_after_a_contract_rejected_google_attempt() {
        let error = replay_error_for_events(
            "invalid-fallback",
            &[
                TraceEvent::SearchQuery {
                    round: 1,
                    query: "query".into(),
                    gap: "Need evidence from a public primary source.".into(),
                },
                TraceEvent::SearchAttemptCompleted {
                    round: 1,
                    query: "query".into(),
                    engine: SearchEngine::Google,
                    outcome: SearchEngineAttemptOutcome::ContractRejected {
                        reason: crate::SearchBoundaryContractFailure::InvalidResponse,
                    },
                    http_status: Some(200),
                },
                TraceEvent::SearchFallbackActivated {
                    round: 1,
                    query: "query".into(),
                    from_engine: SearchEngine::Google,
                    to_engine: SearchEngine::Bing,
                    reason: SearchEngineUnavailability::EngineUnresponsive,
                },
            ],
        );

        assert!(
            error
                .to_string()
                .contains("invalid search fallback transition")
        );
    }

    #[test]
    fn replay_rejects_completed_round_with_missing_search_results() {
        let error = replay_error_for_events(
            "missing-search-result",
            &[
                TraceEvent::SearchQuery {
                    round: 1,
                    query: "query-1".into(),
                    gap: "Need evidence from a public primary source.".into(),
                },
                TraceEvent::SearchQuery {
                    round: 1,
                    query: "query-2".into(),
                    gap: "Need a second independent public source.".into(),
                },
                TraceEvent::SearchQuery {
                    round: 1,
                    query: "query-3".into(),
                    gap: "Need a third independent public source.".into(),
                },
                TraceEvent::SearchAttemptCompleted {
                    round: 1,
                    query: "query-1".into(),
                    engine: SearchEngine::Google,
                    outcome: SearchEngineAttemptOutcome::Completed {
                        valid_result_count: 1,
                    },
                    http_status: Some(200),
                },
                TraceEvent::SearchAttemptCompleted {
                    round: 1,
                    query: "query-2".into(),
                    engine: SearchEngine::Google,
                    outcome: SearchEngineAttemptOutcome::Completed {
                        valid_result_count: 0,
                    },
                    http_status: Some(200),
                },
                TraceEvent::SearchAttemptCompleted {
                    round: 1,
                    query: "query-3".into(),
                    engine: SearchEngine::Google,
                    outcome: SearchEngineAttemptOutcome::Completed {
                        valid_result_count: 0,
                    },
                    http_status: Some(200),
                },
                TraceEvent::RoundCompleted {
                    round: 1,
                    previous_queries: vec!["query-1".into(), "query-2".into(), "query-3".into()],
                    archived_snapshot_refs: Vec::new(),
                },
            ],
        );

        assert!(
            error
                .to_string()
                .contains("completed search result count differs")
        );
    }

    #[test]
    fn snapshot_selection_trace_uses_exactly_two_fields() {
        let event = TraceEvent::SnapshotSelection {
            selected: vec![SourceSelection {
                snapshot_ref: SnapshotRef("snapshot:web/own".into()),
                reason: "direct evidence".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "type": "snapshot_selection",
                "selected": [{
                    "snapshot_ref": "snapshot:web/own",
                    "reason": "direct evidence"
                }]
            })
        );
    }

    #[test]
    fn v1_trace_is_rejected_by_the_v7_replay_boundary() {
        let mut bytes = TraceWriter::new(Vec::new(), header("obsolete-v1"))
            .unwrap()
            .into_inner();
        let trace = String::from_utf8(bytes.clone()).unwrap().replacen(
            &format!("\"schema_version\":{TRACE_SCHEMA_VERSION}"),
            "\"schema_version\":1",
            1,
        );
        bytes = trace.into_bytes();
        let path = std::env::temp_dir().join(format!(
            "traceable-search-obsolete-v1-{}.jsonl",
            std::process::id()
        ));
        fs::write(&path, bytes).unwrap();

        let error = replay_trace(&path).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported trace schema version 1")
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn replay_rejects_invalid_persisted_policy() {
        let mut invalid_header = header("invalid-policy");
        invalid_header.policy = TracePolicy {
            rounds: 0,
            input_budget: 0,
            max_snapshots: 0,
        };
        let fixture = TraceWriter::new(Vec::new(), invalid_header)
            .unwrap()
            .into_inner();
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "traceable-search-invalid-policy-{}-{nonce}.jsonl",
            std::process::id()
        ));
        fs::write(&path, fixture).unwrap();
        let error = replay_trace(&path).unwrap_err();
        assert!(error.to_string().contains("policy rounds"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_run_has_a_structured_terminal_event_contract() {
        let raw = r#"{"type":"run_failed","stage":"synthesis","error_class":"external","message":"invalid model output"}"#;
        assert!(serde_json::from_str::<TraceEvent>(raw).is_ok());
    }

    #[test]
    fn successful_run_has_no_failed_terminal_event() {
        let mut writer = TraceWriter::new(Vec::new(), header("r-success")).unwrap();
        writer
            .append(&TraceEvent::ComposedResearchAnswer {
                answer: "grounded".into(),
                claims: Vec::new(),
                comparison: ResearchAnswerComparison {
                    synthesis_rationale: "The final answer uses the requested evidence weighting."
                        .into(),
                    ..ResearchAnswerComparison::default()
                },
            })
            .unwrap();
        let trace = String::from_utf8(writer.into_inner()).unwrap();
        assert!(!trace.contains("\"type\":\"run_failed\""));
    }

    #[test]
    fn current_trace_writer_requires_each_decision_rationale() {
        let mut writer = TraceWriter::new(Vec::new(), header("r-rationale-contract")).unwrap();
        assert!(
            writer
                .append(&TraceEvent::KnowledgeDraft {
                    draft: ModelKnowledgeDraft {
                        answer: "knowledge".into(),
                        claims: vec!["claim".into()],
                        uncertainty: "uncertain".into(),
                        basis_summary: String::new(),
                    },
                })
                .is_err()
        );
        assert!(
            writer
                .append(&TraceEvent::SearchQuery {
                    round: 1,
                    query: "query".into(),
                    gap: String::new(),
                })
                .is_err()
        );
        assert!(
            writer
                .append(&TraceEvent::SnapshotSelection {
                    selected: vec![SourceSelection {
                        snapshot_ref: SnapshotRef("snapshot:web/rationale".into()),
                        reason: String::new(),
                    }],
                })
                .is_err()
        );
        assert!(
            writer
                .append(&TraceEvent::ResearchClaim {
                    text: "claim".into(),
                    origin: crate::ResearchClaimOrigin::ModelKnowledge,
                    snapshot_refs: Vec::new(),
                    rationale: String::new(),
                })
                .is_err()
        );
        assert!(
            writer
                .append(&TraceEvent::ComposedResearchAnswer {
                    answer: "answer".into(),
                    claims: vec![ComposedResearchClaim {
                        text: "claim".into(),
                        origin: crate::ResearchClaimOrigin::ModelKnowledge,
                        snapshot_refs: Vec::new(),
                        rationale: "The draft remains useful context for this answer.".into(),
                    }],
                    comparison: ResearchAnswerComparison::default(),
                })
                .is_err()
        );
    }

    #[test]
    fn obsolete_v5_and_v6_trace_schemas_are_rejected() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "traceable-search-v5-obsolete-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        for schema_version in [5, 6] {
            let path = dir.join(format!("r-v{schema_version}-obsolete.jsonl"));
            let trace = String::from_utf8(
                TraceWriter::new(Vec::new(), header(&format!("r-v{schema_version}-obsolete")))
                    .unwrap()
                    .into_inner(),
            )
            .unwrap()
            .replacen(
                &format!("\"schema_version\":{TRACE_SCHEMA_VERSION}"),
                &format!("\"schema_version\":{schema_version}"),
                1,
            );
            fs::write(&path, trace).unwrap();
            let error = replay_trace(&path).unwrap_err();
            assert!(error.to_string().contains(&format!(
                "unsupported trace schema version {schema_version}"
            )));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disk_writer_is_create_once_and_rejects_path_traversal() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "traceable-search-trace-{}-{nonce}",
            std::process::id()
        ));

        let writer = TraceWriter::create(&dir, header("r-test")).unwrap();
        drop(writer);
        assert!(TraceWriter::create(&dir, header("r-test")).is_err());
        assert!(TraceWriter::create(&dir, header("../escape")).is_err());
        let text = fs::read_to_string(dir.join("r-test.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["type"],
            "run_header"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v7_writer_rejects_an_unmarked_directory_with_existing_trace_data() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "traceable-search-old-trace-dir-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("old-v6-run.jsonl"),
            "{\"type\":\"run_header\",\"schema_version\":6}\n",
        )
        .unwrap();

        let error = match TraceWriter::create(&dir, header("new-v7-run")) {
            Ok(_) => panic!("v7 writer accepted unmarked trace data"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unmarked trace data"));
        assert!(!dir.join("new-v7-run.jsonl").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resume_projects_only_the_last_completed_round() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "traceable-search-resume-{}-{nonce}",
            std::process::id()
        ));
        let header = header("r-resume");
        let snapshot_ref = SnapshotRef("snapshot:web/resume".into());
        let mut writer = TraceWriter::create(&dir, header.clone()).unwrap();
        let committed_queries = [
            "committed query 1",
            "committed query 2",
            "committed query 3",
        ];
        for query in committed_queries {
            writer
                .append(&TraceEvent::SearchQuery {
                    round: 1,
                    query: query.into(),
                    gap: "committed evidence gap".into(),
                })
                .unwrap();
        }
        for query in committed_queries {
            writer
                .append(&TraceEvent::SearchAttemptCompleted {
                    round: 1,
                    query: query.into(),
                    engine: SearchEngine::Google,
                    outcome: SearchEngineAttemptOutcome::Completed {
                        valid_result_count: 0,
                    },
                    http_status: Some(200),
                })
                .unwrap();
        }
        writer
            .append(&TraceEvent::RoundCompleted {
                round: 1,
                previous_queries: committed_queries.map(str::to_owned).to_vec(),
                archived_snapshot_refs: vec![snapshot_ref.clone()],
            })
            .unwrap();
        writer
            .append(&TraceEvent::SearchQuery {
                round: 2,
                query: "uncommitted query".into(),
                gap: "uncommitted gap".into(),
            })
            .unwrap();
        drop(writer);

        let (mut writer, replay) = TraceWriter::resume(&dir, &header).unwrap();
        writer
            .append(&TraceEvent::ArchiveSkip {
                search_result_id: "after-resume".into(),
                reason: "resume sequence probe".into(),
                error_class: ErrorClass::External,
            })
            .unwrap();
        drop(writer);
        assert_eq!(replay.completed_round, 1);
        assert_eq!(replay.previous_queries, committed_queries);
        assert_eq!(replay.archived_snapshot_refs, [snapshot_ref]);
        let envelopes = fs::read_to_string(dir.join("r-resume.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<TraceEventEnvelope>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            envelopes
                .iter()
                .map(|envelope| envelope.sequence)
                .collect::<Vec<_>>(),
            (1..=10).collect::<Vec<_>>()
        );
        assert!(
            envelopes
                .windows(2)
                .all(|pair| pair[0].occurred_at <= pair[1].occurred_at)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resume_rejects_terminal_and_truncated_traces() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "traceable-search-resume-invalid-{}-{nonce}",
            std::process::id()
        ));

        let terminal = header("r-terminal");
        let mut writer = TraceWriter::create(&dir, terminal.clone()).unwrap();
        writer
            .append(&TraceEvent::ComposedResearchAnswer {
                answer: "done".into(),
                claims: Vec::new(),
                comparison: ResearchAnswerComparison {
                    synthesis_rationale: "The final answer uses the requested evidence weighting."
                        .into(),
                    ..ResearchAnswerComparison::default()
                },
            })
            .unwrap();
        drop(writer);
        assert!(TraceWriter::resume(&dir, &terminal).is_err());

        let truncated = header("r-truncated");
        let writer = TraceWriter::create(&dir, truncated.clone()).unwrap();
        drop(writer);
        let path = dir.join("r-truncated.jsonl");
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str(r#"{"type":"query""#);
        fs::write(path, text).unwrap();
        assert!(TraceWriter::resume(&dir, &truncated).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn second_header_is_rejected() {
        let mut writer = TraceWriter::new(Vec::new(), header("r-test")).unwrap();
        let unused = header("unused");
        let duplicate = TraceEvent::RunHeader {
            schema_version: TRACE_SCHEMA_VERSION,
            run_id: "r-other".into(),
            session_id: None,
            turn: None,
            clarification_id: unused.clarification_id,
            brief: Box::new(unused.brief),
            started_at: Utc::now(),
            policy: unused.policy,
            answer_style: unused.answer_style,
        };
        assert!(writer.append(&duplicate).is_err());
    }
}

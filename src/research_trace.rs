//! Append-only, one-file-per-run JSONL audit trace.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ComposedResearchClaim, ErrorClass, FrozenResearchBrief, ModelKnowledgeDraft,
    RationaleAuditStatus, ResearchAnswerComparison, ResearchAnswerStyle, ResearchError,
    ResearchStage, Result, SnapshotRef, validate_decision_rationale,
};

pub const TRACE_SCHEMA_VERSION: u32 = 6;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayedRunHeader {
    Legacy {
        schema_version: u32,
        run_id: String,
        question: String,
        started_at: DateTime<Utc>,
        policy: TracePolicy,
    },
    V6(Box<RunHeader>),
}

impl ReplayedRunHeader {
    #[must_use]
    pub const fn rationale_audit_status(&self) -> RationaleAuditStatus {
        match self {
            Self::V6(_) => RationaleAuditStatus::RequiredAndValidated,
            Self::Legacy { .. } => RationaleAuditStatus::LegacyUnverified,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct LegacyRunHeader {
    pub run_id: String,
    pub question: String,
    pub started_at: DateTime<Utc>,
    pub policy: TracePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSelection {
    pub snapshot_ref: SnapshotRef,
    pub reason: String,
}

/// Current v6 trace contract. New fields and variants require explicit schema validation.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clarification_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief: Option<Box<FrozenResearchBrief>>,
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
    SearchResult {
        round: u32,
        query: String,
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
    RunFailed {
        error_class: ErrorClass,
        stage: ResearchStage,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunReplay {
    pub completed_round: u32,
    pub previous_queries: Vec<String>,
    pub archived_snapshot_refs: Vec<SnapshotRef>,
    pub model_knowledge_draft: Option<ModelKnowledgeDraft>,
}

/// Owns the only write direction: construction writes the mandatory first line,
/// then `append` only adds subsequent events.
pub struct TraceWriter<W: Write> {
    sink: W,
    schema_version: u32,
}

impl<W: Write> TraceWriter<W> {
    pub fn new(sink: W, header: RunHeader) -> Result<Self> {
        let mut writer = Self {
            sink,
            schema_version: TRACE_SCHEMA_VERSION,
        };
        writer.write_event(&TraceEvent::RunHeader {
            schema_version: TRACE_SCHEMA_VERSION,
            run_id: header.run_id,
            session_id: header.session_id,
            turn: header.turn,
            question: None,
            clarification_id: Some(header.clarification_id),
            brief: Some(Box::new(header.brief)),
            started_at: header.started_at,
            policy: header.policy,
            answer_style: header.answer_style,
        })?;
        Ok(writer)
    }

    // ponytail: test-only bridge for generating v1/v2 fixtures to verify legacy replay.
    #[cfg(test)]
    pub(crate) fn new_legacy(sink: W, header: LegacyRunHeader) -> Result<Self> {
        let mut writer = Self {
            sink,
            schema_version: 2,
        };
        writer.write_event(&TraceEvent::RunHeader {
            schema_version: 2,
            run_id: header.run_id,
            session_id: None,
            turn: None,
            question: Some(header.question),
            clarification_id: None,
            brief: None,
            started_at: header.started_at,
            policy: header.policy,
            answer_style: ResearchAnswerStyle::WebFirst,
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
        serde_json::to_writer(&mut self.sink, event).map_err(std::io::Error::other)?;
        self.sink.write_all(b"\n")?;
        self.sink.flush()?;
        Ok(())
    }
}

impl TraceWriter<BufWriter<File>> {
    /// Creates `trace_dir/<run_id>.jsonl` without overwriting an existing run.
    pub fn create(trace_dir: impl AsRef<Path>, header: RunHeader) -> Result<Self> {
        validate_run_id(&header.run_id)?;
        fs::create_dir_all(trace_dir.as_ref())?;
        let path = trace_dir.as_ref().join(format!("{}.jsonl", header.run_id));
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)?;
        Self::new(BufWriter::new(file), header)
    }

    /// Reopens only a matching, non-terminal v6 trace and returns its last committed round.
    pub(crate) fn resume(
        trace_dir: impl AsRef<Path>,
        header: &RunHeader,
    ) -> Result<(Self, RunReplay)> {
        validate_run_id(&header.run_id)?;
        let path = trace_dir.as_ref().join(format!("{}.jsonl", header.run_id));
        let (replay, schema_version) = replay_run(&path, header)?;
        let file = OpenOptions::new().append(true).open(path)?;
        Ok((
            Self {
                sink: BufWriter::new(file),
                schema_version,
            },
            replay,
        ))
    }
}

fn replay_run(path: &Path, expected: &RunHeader) -> Result<(RunReplay, u32)> {
    let schema_version = match replay_run_header(path)? {
        ReplayedRunHeader::V6(existing) if existing.as_ref() == expected => TRACE_SCHEMA_VERSION,
        _ => {
            return Err(invalid_trace(
                "existing trace header does not match frozen run",
            ));
        }
    };

    let contents = fs::read_to_string(path)?;
    if !contents.ends_with('\n') {
        return Err(invalid_trace("truncated trace event"));
    }
    let mut replay = RunReplay::default();
    for (index, line) in contents.lines().enumerate().skip(1) {
        if line.is_empty() {
            return Err(invalid_trace(format!(
                "empty trace event at line {}",
                index + 1
            )));
        }
        let event: TraceEvent = serde_json::from_str(line)
            .map_err(|error| invalid_trace(format!("line {}: {error}", index + 1)))?;
        validate_trace_event_for_schema(schema_version, &event)?;
        match event {
            TraceEvent::RoundCompleted {
                round,
                previous_queries,
                archived_snapshot_refs,
            } => {
                if round != replay.completed_round + 1 || round > expected.policy.rounds {
                    return Err(invalid_trace("invalid round_completed sequence"));
                }
                replay = RunReplay {
                    completed_round: round,
                    previous_queries,
                    archived_snapshot_refs,
                    model_knowledge_draft: replay.model_knowledge_draft,
                };
            }
            TraceEvent::KnowledgeDraft { draft } => {
                if replay.model_knowledge_draft.replace(draft).is_some() {
                    return Err(invalid_trace("trace contains multiple knowledge drafts"));
                }
            }
            TraceEvent::RunHeader { .. } => {
                return Err(invalid_trace("duplicate run_header"));
            }
            TraceEvent::ComposedResearchAnswer { .. } | TraceEvent::RunFailed { .. } => {
                return Err(invalid_trace("trace is already terminal"));
            }
            _ => {}
        }
    }
    Ok((replay, schema_version))
}

pub fn replay_run_header(path: impl AsRef<Path>) -> Result<ReplayedRunHeader> {
    let mut first_line = String::new();
    let bytes = BufReader::new(File::open(path)?).read_line(&mut first_line)?;
    if bytes == 0 || !first_line.ends_with('\n') {
        return Err(invalid_trace("missing or truncated run_header"));
    }
    let raw_header: serde_json::Value = serde_json::from_str(first_line.trim_end())
        .map_err(|error| invalid_trace(error.to_string()))?;
    let declared_schema_version = raw_header
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| (value <= u64::from(u32::MAX)).then_some(value as u32))
        .ok_or_else(|| invalid_trace("run_header schema_version must be a u32"))?;
    if !matches!(declared_schema_version, 1 | 2 | TRACE_SCHEMA_VERSION) {
        return Err(invalid_trace(format!(
            "unsupported trace schema version {declared_schema_version}"
        )));
    }
    let event: TraceEvent =
        serde_json::from_value(raw_header).map_err(|error| invalid_trace(error.to_string()))?;
    let TraceEvent::RunHeader {
        schema_version,
        run_id,
        session_id,
        turn,
        question,
        clarification_id,
        brief,
        started_at,
        policy,
        answer_style,
    } = event
    else {
        return Err(invalid_trace("first trace event is not run_header"));
    };
    if schema_version != declared_schema_version {
        return Err(invalid_trace(
            "run_header schema_version changed while decoding",
        ));
    }
    validate_run_id(&run_id)?;
    validate_trace_policy(&policy).map_err(invalid_trace)?;
    match schema_version {
        1 | 2 => match (session_id, turn, question, clarification_id, brief) {
            (None, None, Some(question), None, None) => Ok(ReplayedRunHeader::Legacy {
                schema_version,
                run_id,
                question,
                started_at,
                policy,
            }),
            _ => Err(invalid_trace("invalid legacy run_header fields")),
        },
        TRACE_SCHEMA_VERSION => match (session_id, turn, question, clarification_id, brief) {
            (session_id, turn, None, Some(clarification_id), Some(brief))
                if clarification_id == brief.clarification_id()
                    && session_id.is_some() == turn.is_some()
                    && !matches!(turn, Some(0)) =>
            {
                let header = Box::new(RunHeader {
                    run_id,
                    clarification_id,
                    session_id,
                    turn,
                    brief: *brief,
                    started_at,
                    policy,
                    answer_style,
                });
                Ok(ReplayedRunHeader::V6(header))
            }
            _ => Err(invalid_trace("invalid current run_header fields")),
        },
        version => Err(invalid_trace(format!(
            "unsupported trace schema version {version}"
        ))),
    }
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
    fn v1_jsonl_events_remain_readable() {
        let fixture = concat!(
            r#"{"type":"run_header","schema_version":1,"run_id":"legacy","question":"q","started_at":"2026-07-11T10:00:00Z","policy":{"rounds":3,"input_budget":800000,"max_snapshots":300}}"#,
            "\n",
            r#"{"type":"snapshot_selection","selected":[{"snapshot_ref":"snapshot:web/legacy","reason":"evidence","relevance":"high"}]}"#,
        );

        let events = fixture
            .lines()
            .map(serde_json::from_str::<TraceEvent>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            TraceEvent::RunHeader {
                schema_version: 1,
                ..
            }
        ));
        assert!(matches!(&events[1], TraceEvent::SnapshotSelection { .. }));
    }

    #[test]
    fn replay_rejects_invalid_persisted_policy() {
        let fixture = concat!(
            r#"{"type":"run_header","schema_version":1,"run_id":"legacy","question":"q","started_at":"2026-07-11T10:00:00Z","policy":{"rounds":0,"input_budget":0,"max_snapshots":0}}"#,
            "\n",
        );
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "traceable-search-invalid-policy-{}-{nonce}.jsonl",
            std::process::id()
        ));
        fs::write(&path, fixture).unwrap();
        let error = replay_run_header(&path).unwrap_err();
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
    fn obsolete_v5_trace_schema_is_rejected_before_decoding_its_brief() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "traceable-search-v5-obsolete-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r-v5-obsolete.jsonl");
        fs::write(&path, "{\"type\":\"run_header\",\"schema_version\":5}\n").unwrap();
        let error = replay_run_header(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported trace schema version 5")
        );
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
        writer
            .append(&TraceEvent::RoundCompleted {
                round: 1,
                previous_queries: vec!["committed query".into()],
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

        let (writer, replay) = TraceWriter::resume(&dir, &header).unwrap();
        drop(writer);
        assert_eq!(replay.completed_round, 1);
        assert_eq!(replay.previous_queries, ["committed query"]);
        assert_eq!(replay.archived_snapshot_refs, [snapshot_ref]);
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
            question: None,
            clarification_id: Some(unused.clarification_id),
            brief: Some(Box::new(unused.brief)),
            started_at: Utc::now(),
            policy: unused.policy,
            answer_style: unused.answer_style,
        };
        assert!(writer.append(&duplicate).is_err());
    }
}

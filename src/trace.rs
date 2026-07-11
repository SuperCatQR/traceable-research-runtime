//! Append-only, one-file-per-run JSONL audit trace.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Claim, ErrorClass, PipelineStage, Result, SearchError, SnapshotRef};

pub const TRACE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TracePolicy {
    pub rounds: u32,
    pub input_budget: u32,
    pub max_snapshots: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHeader {
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

/// Stable v2 trace contract. New fields/variants may be appended; old ones stay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceEvent {
    RunHeader {
        schema_version: u32,
        run_id: String,
        question: String,
        started_at: DateTime<Utc>,
        policy: TracePolicy,
    },
    Query {
        round: u32,
        query: String,
        gap: String,
    },
    SearchResult {
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
    Excerpt {
        snapshot_ref: SnapshotRef,
        content_hash: String,
        title: String,
        excerpt: String,
    },
    SnapshotSelection {
        selected: Vec<SourceSelection>,
    },
    Claim {
        text: String,
        snapshot_refs: Vec<SnapshotRef>,
    },
    Answer {
        answer: String,
        claims: Vec<Claim>,
    },
    RunFailed {
        error_class: ErrorClass,
        stage: PipelineStage,
        message: String,
    },
}

/// Owns the only write direction: construction writes the mandatory first line,
/// then `append` only adds subsequent events.
pub struct TraceWriter<W: Write> {
    sink: W,
}

impl<W: Write> TraceWriter<W> {
    pub fn new(sink: W, header: RunHeader) -> Result<Self> {
        let mut writer = Self { sink };
        writer.write_event(&TraceEvent::RunHeader {
            schema_version: TRACE_SCHEMA_VERSION,
            run_id: header.run_id,
            question: header.question,
            started_at: header.started_at,
            policy: header.policy,
        })?;
        Ok(writer)
    }

    pub fn append(&mut self, event: &TraceEvent) -> Result<()> {
        if matches!(event, TraceEvent::RunHeader { .. }) {
            return Err(SearchError::Trace(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "run_header is only valid as the first trace line",
            )));
        }
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
}

fn validate_run_id(run_id: &str) -> Result<()> {
    let path = Path::new(run_id);
    if run_id.is_empty()
        || run_id == "."
        || run_id == ".."
        || path.file_name() != Some(OsStr::new(run_id))
    {
        return Err(SearchError::Trace(std::io::Error::new(
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

    fn header(run_id: &str) -> RunHeader {
        RunHeader {
            run_id: run_id.into(),
            question: "original question".into(),
            started_at: Utc.with_ymd_and_hms(2026, 7, 11, 10, 0, 0).unwrap(),
            policy: TracePolicy {
                rounds: 3,
                input_budget: 800_000,
                max_snapshots: 300,
            },
        }
    }

    #[test]
    fn header_is_first_and_events_are_jsonl() {
        let mut writer = TraceWriter::new(Vec::new(), header("r-test")).unwrap();
        writer
            .append(&TraceEvent::Query {
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
        assert_eq!(lines[0]["question"], "original question");
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
    fn failed_run_has_a_structured_terminal_event_contract() {
        let raw = r#"{"type":"run_failed","stage":"synthesis","error_class":"external","message":"invalid model output"}"#;
        assert!(serde_json::from_str::<TraceEvent>(raw).is_ok());
    }

    #[test]
    fn successful_run_has_no_failed_terminal_event() {
        let mut writer = TraceWriter::new(Vec::new(), header("r-success")).unwrap();
        writer
            .append(&TraceEvent::Answer {
                answer: "grounded".into(),
                claims: Vec::new(),
            })
            .unwrap();
        let trace = String::from_utf8(writer.into_inner()).unwrap();
        assert!(!trace.contains("\"type\":\"run_failed\""));
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
    fn second_header_is_rejected() {
        let mut writer = TraceWriter::new(Vec::new(), header("r-test")).unwrap();
        let duplicate = TraceEvent::RunHeader {
            schema_version: TRACE_SCHEMA_VERSION,
            run_id: "r-other".into(),
            question: "other".into(),
            started_at: Utc::now(),
            policy: header("unused").policy,
        };
        assert!(writer.append(&duplicate).is_err());
    }
}

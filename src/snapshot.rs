//! Content-addressed snapshot persistence with capability-separated handles.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::Type};

use crate::{
    CrawlMeta, Result, SearchError, Snapshot, SnapshotRef, content_hash, snapshot_id, snapshot_ref,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS snapshots (
    snapshot_id   TEXT PRIMARY KEY NOT NULL,
    snapshot_ref  TEXT NOT NULL UNIQUE,
    requested_url TEXT NOT NULL,
    title         TEXT NOT NULL,
    body          TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    final_url     TEXT NOT NULL,
    http_status   INTEGER NOT NULL CHECK (http_status BETWEEN 100 AND 599),
    fetched_at    TEXT NOT NULL,
    crawl_meta_json TEXT,
    CHECK (snapshot_ref = 'snapshot:web/' || snapshot_id),
    CHECK (content_hash GLOB 'sha256:*')
) STRICT;
"#;

/// Write-only capability for the global snapshot database.
pub struct SnapshotWriter {
    conn: Connection,
}

impl SnapshotWriter {
    /// Opens (or creates) the database and installs its schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        if !has_crawl_meta_column(&conn)? {
            conn.execute_batch("ALTER TABLE snapshots ADD COLUMN crawl_meta_json TEXT;")?;
        }
        Ok(Self { conn })
    }

    /// Archives one immutable snapshot. Re-saving the same identity is a no-op.
    pub fn save(&mut self, snapshot: &Snapshot) -> Result<()> {
        validate_snapshot(snapshot)?;
        let crawl_meta_json = serde_json::to_string(&snapshot.crawl)
            .map_err(|error| SearchError::InvalidSnapshot(format!("crawl metadata: {error}")))?;

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO snapshots (
                snapshot_id, snapshot_ref, requested_url, title, body,
                content_hash, final_url, http_status, fetched_at, crawl_meta_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(snapshot_id) DO NOTHING",
            params![
                snapshot.snapshot_id,
                snapshot.snapshot_ref.as_str(),
                snapshot.requested_url,
                snapshot.title,
                snapshot.body,
                snapshot.content_hash,
                snapshot.crawl.final_url,
                i64::from(snapshot.crawl.http_status),
                snapshot.crawl.fetched_at.to_rfc3339(),
                crawl_meta_json,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}

/// Read-only capability used by research sessions.
pub struct SnapshotReader {
    conn: Connection,
    has_crawl_meta: bool,
}

impl SnapshotReader {
    /// Opens an existing snapshot database without write/create permission.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let has_crawl_meta = has_crawl_meta_column(&conn)?;
        Ok(Self {
            conn,
            has_crawl_meta,
        })
    }

    /// Reads a snapshot by trace reference and re-verifies its content address.
    pub fn get(&self, reference: &SnapshotRef) -> Result<Option<Snapshot>> {
        let metadata_column = if self.has_crawl_meta {
            "crawl_meta_json"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT snapshot_id, snapshot_ref, requested_url, title, body,
                    content_hash, final_url, http_status, fetched_at, {metadata_column}
             FROM snapshots WHERE snapshot_ref = ?1"
        );
        let snapshot = self
            .conn
            .query_row(&sql, [reference.as_str()], row_to_snapshot)
            .optional()?;

        if let Some(snapshot) = &snapshot {
            validate_snapshot(snapshot)?;
        }
        Ok(snapshot)
    }
}

fn has_crawl_meta_column(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('snapshots') WHERE name = 'crawl_meta_json'
        )",
        [],
        |row| row.get(0),
    )
}

fn row_to_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<Snapshot> {
    let status: i64 = row.get(7)?;
    let http_status =
        u16::try_from(status).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, status))?;
    let fetched_at_raw: String = row.get(8)?;
    let fetched_at = DateTime::parse_from_rfc3339(&fetched_at_raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(8, Type::Text, Box::new(error))
        })?;
    let final_url: String = row.get(6)?;
    let crawl_meta_json: Option<String> = row.get(9)?;
    let mut crawl = match crawl_meta_json {
        Some(value) => serde_json::from_str::<CrawlMeta>(&value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(error))
        })?,
        None => CrawlMeta::basic(final_url.clone(), http_status, fetched_at),
    };
    crawl.final_url = final_url;
    crawl.http_status = http_status;
    crawl.fetched_at = fetched_at;

    Ok(Snapshot {
        snapshot_id: row.get(0)?,
        snapshot_ref: SnapshotRef(row.get(1)?),
        requested_url: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        content_hash: row.get(5)?,
        crawl,
    })
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    let actual_hash = content_hash(&snapshot.body);
    if actual_hash != snapshot.content_hash {
        return Err(SearchError::HashMismatch {
            reference: snapshot.snapshot_ref.to_string(),
            expected: snapshot.content_hash.clone(),
            actual: actual_hash,
        });
    }

    let expected_id = snapshot_id(&snapshot.crawl.final_url, &snapshot.content_hash);
    if snapshot.snapshot_id != expected_id {
        return Err(SearchError::InvalidSnapshot(format!(
            "snapshot_id: expected {expected_id}, got {}",
            snapshot.snapshot_id
        )));
    }

    let expected_ref = snapshot_ref(&snapshot.snapshot_id);
    if snapshot.snapshot_ref.as_str() != expected_ref {
        return Err(SearchError::InvalidSnapshot(format!(
            "snapshot_ref: expected {expected_ref}, got {}",
            snapshot.snapshot_ref
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::SystemTime};

    use chrono::TimeZone;
    use rusqlite::Connection;

    use super::*;
    use crate::ErrorClass;

    struct TempDb(PathBuf);

    impl TempDb {
        fn new(tag: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "traceable-search-{tag}-{}-{nonce}.sqlite",
                std::process::id()
            )))
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn sample() -> Snapshot {
        let mut crawl = CrawlMeta::basic(
            "https://example.com/final".into(),
            200,
            Utc.with_ymd_and_hms(2026, 7, 11, 10, 0, 0).unwrap(),
        );
        crawl.metadata = serde_json::json!({"title": "Example", "language": "en"});
        crawl.raw_markdown_bytes = 42;
        crawl.fit_markdown_bytes = 21;
        crawl.body_kind = Some(crate::CrawlBodyKind::RawMarkdown);
        crawl.truncated = true;
        Snapshot::new(
            "https://example.com/original".into(),
            "Example".into(),
            "archived body".into(),
            crawl,
        )
    }

    #[test]
    fn snapshot_round_trip_is_idempotent() {
        let db = TempDb::new("roundtrip");
        let snapshot = sample();
        let mut writer = SnapshotWriter::open(&db.0).unwrap();
        writer.save(&snapshot).unwrap();
        writer.save(&snapshot).unwrap();
        drop(writer);

        let reader = SnapshotReader::open(&db.0).unwrap();
        assert_eq!(reader.get(&snapshot.snapshot_ref).unwrap(), Some(snapshot));
        assert_eq!(
            reader
                .get(&SnapshotRef("snapshot:web/missing".into()))
                .unwrap(),
            None
        );
    }

    #[test]
    fn reader_detects_body_tampering() {
        let db = TempDb::new("tamper");
        let snapshot = sample();
        let mut writer = SnapshotWriter::open(&db.0).unwrap();
        writer.save(&snapshot).unwrap();
        drop(writer);

        let conn = Connection::open(&db.0).unwrap();
        conn.execute(
            "UPDATE snapshots SET body = ?1 WHERE snapshot_ref = ?2",
            params!["tampered", snapshot.snapshot_ref.as_str()],
        )
        .unwrap();
        drop(conn);

        let reader = SnapshotReader::open(&db.0).unwrap();
        let error = reader.get(&snapshot.snapshot_ref).unwrap_err();
        assert!(matches!(error, SearchError::HashMismatch { .. }));
        assert_eq!(error.error_class(), ErrorClass::Internal);
    }

    #[test]
    fn legacy_schema_is_readable_and_migrated_on_write_open() {
        let db = TempDb::new("legacy");
        let snapshot = sample();
        let conn = Connection::open(&db.0).unwrap();
        conn.execute_batch(&SCHEMA.replace("    crawl_meta_json TEXT,\n", ""))
            .unwrap();
        conn.execute(
            "INSERT INTO snapshots (
                snapshot_id, snapshot_ref, requested_url, title, body,
                content_hash, final_url, http_status, fetched_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                snapshot.snapshot_id,
                snapshot.snapshot_ref.as_str(),
                snapshot.requested_url,
                snapshot.title,
                snapshot.body,
                snapshot.content_hash,
                snapshot.crawl.final_url,
                i64::from(snapshot.crawl.http_status),
                snapshot.crawl.fetched_at.to_rfc3339(),
            ],
        )
        .unwrap();
        drop(conn);

        let reader = SnapshotReader::open(&db.0).unwrap();
        let legacy = reader.get(&snapshot.snapshot_ref).unwrap().unwrap();
        assert_eq!(legacy.crawl.final_url, snapshot.crawl.final_url);
        assert_eq!(legacy.crawl.metadata, serde_json::Value::Null);
        assert_eq!(legacy.crawl.body_kind, None);
        drop(reader);

        drop(SnapshotWriter::open(&db.0).unwrap());
        let conn = Connection::open(&db.0).unwrap();
        assert!(has_crawl_meta_column(&conn).unwrap());
    }

    #[test]
    fn writer_rejects_mutated_identity() {
        let db = TempDb::new("identity");
        let mut snapshot = sample();
        snapshot.snapshot_id = "not-content-addressed".into();
        let mut writer = SnapshotWriter::open(&db.0).unwrap();
        assert!(matches!(
            writer.save(&snapshot).unwrap_err(),
            SearchError::InvalidSnapshot(_)
        ));
    }
}

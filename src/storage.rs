//! SQLite persistence and the blocking storage boundary.
//!
//! Storage owns the connection and migration lifecycle. Callers submit short
//! transactions through this module; no transaction or connection guard should
//! be held across an async suspension point. Event payloads remain opaque to
//! this module, while sequence allocation and command idempotency are kept
//! together in one transaction.

use std::{
    fmt::{self, Debug, Formatter},
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use crate::{
    error::{Result, RuntimeError, RuntimeStage},
    identity::{CommandId, SubjectId},
};

const CURRENT_SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MIGRATION_0001: &str = include_str!("../migrations/0001_runtime.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_markdown_corpus.sql");

/// The two append-only event streams currently owned by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventStream {
    /// Conversation and clarification lifecycle events.
    Lifecycle,
    /// Prepared execution and research events.
    Execution,
}

impl EventStream {
    const fn table_name(self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle_events",
            Self::Execution => "execution_events",
        }
    }

    const fn stage(self) -> RuntimeStage {
        match self {
            Self::Lifecycle => RuntimeStage::Lifecycle,
            Self::Execution => RuntimeStage::Trace,
        }
    }
}

/// An event ready to be appended to one stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewEvent {
    pub(crate) scope: String,
    pub(crate) owner_subject_id: SubjectId,
    pub(crate) command_id: CommandId,
    pub(crate) event_schema_version: u32,
    pub(crate) event_type: String,
    pub(crate) recorded_at: DateTime<Utc>,
    pub(crate) payload_json: String,
}

/// A command identity and its deterministic result, without its event range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewCommandCommit {
    pub(crate) scope: String,
    pub(crate) command_id: CommandId,
    pub(crate) request_hash: String,
    pub(crate) result_json: String,
    pub(crate) committed_at: DateTime<Utc>,
}

/// The sequence interval occupied by one command's event batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SequenceRange {
    first: i64,
    last: i64,
}

/// A command commit as recovered from SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCommitRecord {
    pub(crate) scope: String,
    pub(crate) command_id: CommandId,
    pub(crate) request_hash: String,
    pub(crate) result_json: String,
    pub(crate) first_sequence: i64,
    pub(crate) last_sequence: i64,
    pub(crate) committed_at: DateTime<Utc>,
}

/// Result of an idempotent command append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCommitResult {
    pub(crate) record: CommandCommitRecord,
    /// True when the command was already committed and no event was added.
    pub(crate) reused: bool,
}

/// An event row after storage-level decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredEvent {
    pub(crate) stream: EventStream,
    pub(crate) scope: String,
    pub(crate) sequence: i64,
    pub(crate) owner_subject_id: SubjectId,
    pub(crate) command_id: CommandId,
    pub(crate) command_event_index: i64,
    pub(crate) event_schema_version: u32,
    pub(crate) event_type: String,
    pub(crate) recorded_at: DateTime<Utc>,
    pub(crate) payload_json: String,
}

/// Shared SQLite storage handle.
///
/// The connection is guarded by a synchronous mutex because all lock and SQL
/// work happens inside a short synchronous operation or a spawn_blocking task.
/// The handle itself is cheap to clone and does not expose the guard.
#[derive(Clone)]
pub(crate) struct Storage {
    connection: Arc<Mutex<Connection>>,
}

impl Debug for Storage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Storage").finish_non_exhaustive()
    }
}

impl Storage {
    /// Opens a file-backed database, configures SQLite, and applies migrations.
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        ensure_parent_directory(path)?;
        let mut connection = Connection::open(path)?;
        configure_connection(&connection)?;
        migrate(&mut connection)?;
        Ok(Self { connection: Arc::new(Mutex::new(connection)) })
    }

    /// Opens an isolated in-memory database, primarily for unit tests.
    pub(crate) fn open_in_memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure_connection(&connection)?;
        migrate(&mut connection)?;
        Ok(Self { connection: Arc::new(Mutex::new(connection)) })
    }

    /// Runs a synchronous operation while holding the connection briefly.
    ///
    /// This is intended for code already running on a blocking thread. Async
    /// callers should use Storage::run_blocking instead.
    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut connection = self.lock_connection()?;
        operation(&mut connection)
    }

    /// Executes a closure in an immediate SQLite transaction.
    pub(crate) fn transact<T, F>(&self, operation: F) -> Result<T>
    where
        F: for<'tx> FnOnce(&mut StorageTransaction<'tx>) -> Result<T>,
    {
        self.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut transaction = StorageTransaction { transaction };
            let value = operation(&mut transaction)?;
            transaction.transaction.commit()?;
            Ok(value)
        })
    }

    /// Moves a storage operation to Tokio's blocking pool.
    ///
    /// The closure must own all captured values. This keeps a rusqlite
    /// connection and its synchronous mutex out of the async executor and
    /// makes it impossible for a connection guard to cross await.
    pub(crate) async fn run_blocking<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Storage) -> Result<T> + Send + 'static,
    {
        let storage = self.clone();
        spawn_blocking_storage(move || operation(&storage)).await
    }

    /// Returns the currently applied migration version.
    #[cfg(test)]
    pub(crate) fn schema_version(&self) -> Result<i64> {
        self.with_connection(|connection| {
            connection
                .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| {
                    row.get(0)
                })
                .map_err(Into::into)
        })
    }

    /// Reads one complete, ordered event stream.
    pub(crate) fn read_events(&self, stream: EventStream, scope: &str) -> Result<Vec<StoredEvent>> {
        validate_scope(scope)?;
        self.with_connection(|connection| read_events(connection, stream, scope))
    }

    /// Reads a previously committed command, if one exists for the scope.
    #[cfg(test)]
    pub(crate) fn read_command_commit(
        &self,
        scope: &str,
        command_id: &CommandId,
    ) -> Result<Option<CommandCommitRecord>> {
        validate_scope(scope)?;
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT scope, command_id, request_hash, result_json,
                        first_sequence, last_sequence, committed_at
                 FROM command_commits
                 WHERE scope = ?1 AND command_id = ?2",
            )?;
            statement
                .query_row(params![scope, command_id.as_str()], row_to_command_commit)
                .optional()
                .map_err(Into::into)
        })
    }

    fn lock_connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| RuntimeError::Storage {
            message: "storage connection lock poisoned".to_owned(),
        })
    }
}

/// Runs a synchronous storage closure on Tokio's blocking pool.
pub(crate) async fn spawn_blocking_storage<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| RuntimeError::Storage { message: "storage worker terminated".to_owned() })?
}

/// A short-lived immediate transaction over the storage schema.
pub(crate) struct StorageTransaction<'conn> {
    transaction: Transaction<'conn>,
}

impl StorageTransaction<'_> {
    /// Reads one complete event stream inside the current immediate transaction.
    pub(crate) fn read_events(&self, stream: EventStream, scope: &str) -> Result<Vec<StoredEvent>> {
        validate_scope(scope)?;
        read_events(&self.transaction, stream, scope)
    }

    /// Reads an idempotency record before state validation.
    pub(crate) fn read_command_commit(
        &self,
        scope: &str,
        command_id: &CommandId,
    ) -> Result<Option<CommandCommitRecord>> {
        validate_scope(scope)?;
        self.find_command(scope, command_id)
    }

    /// Appends an event batch and allocates contiguous stream-local sequences.
    fn append_events(&mut self, stream: EventStream, events: &[NewEvent]) -> Result<SequenceRange> {
        if events.is_empty() {
            return Err(RuntimeError::validation(stream.stage(), "event batch must not be empty"));
        }
        let scope = events[0].scope.as_str();
        validate_scope(scope)?;
        for event in events {
            validate_event(event)?;
            if event.scope != scope {
                return Err(RuntimeError::validation(
                    stream.stage(),
                    "one event batch cannot span multiple scopes",
                ));
            }
        }

        let table = stream.table_name();
        let query = format!("SELECT COALESCE(MAX(sequence), 0) FROM {table} WHERE scope = ?1");
        let current: i64 = self.transaction.query_row(&query, [scope], |row| row.get(0))?;
        let first = current.checked_add(1).ok_or_else(|| RuntimeError::Storage {
            message: "event sequence overflow".to_owned(),
        })?;
        let insert = format!(
            "INSERT INTO {table} (
                scope, sequence, owner_subject_id, command_id,
                command_event_index, event_schema_version, event_type,
                recorded_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        );
        for (index, event) in events.iter().enumerate() {
            let offset = i64::try_from(index).map_err(|_| RuntimeError::Storage {
                message: "event batch is too large".to_owned(),
            })?;
            let sequence = first.checked_add(offset).ok_or_else(|| RuntimeError::Storage {
                message: "event sequence overflow".to_owned(),
            })?;
            self.transaction.execute(
                &insert,
                params![
                    event.scope.as_str(),
                    sequence,
                    event.owner_subject_id.as_str(),
                    event.command_id.as_str(),
                    offset,
                    i64::from(event.event_schema_version),
                    event.event_type.as_str(),
                    format_timestamp(event.recorded_at),
                    event.payload_json.as_str(),
                ],
            )?;
        }
        let last = first
            .checked_add(i64::try_from(events.len() - 1).map_err(|_| RuntimeError::Storage {
                message: "event batch is too large".to_owned(),
            })?)
            .ok_or_else(|| RuntimeError::Storage {
                message: "event sequence overflow".to_owned(),
            })?;
        Ok(SequenceRange { first, last })
    }

    /// Appends an event batch and its command result atomically.
    ///
    /// If the command already exists with the same request hash, no event is
    /// appended and the first result is returned. Reusing a command ID with a
    /// different request hash is a conflict.
    pub(crate) fn append_events_with_command(
        &mut self,
        stream: EventStream,
        command: &NewCommandCommit,
        events: &[NewEvent],
    ) -> Result<CommandCommitResult> {
        validate_command(command)?;
        if events.is_empty() {
            return Err(RuntimeError::validation(
                stream.stage(),
                "a committed command must produce at least one event",
            ));
        }
        if events
            .iter()
            .any(|event| event.scope != command.scope || event.command_id != command.command_id)
        {
            return Err(RuntimeError::validation(
                stream.stage(),
                "command and event identities do not match",
            ));
        }

        if let Some(existing) = self.find_command(&command.scope, &command.command_id)? {
            if existing.request_hash == command.request_hash {
                return Ok(CommandCommitResult { record: existing, reused: true });
            }
            return Err(RuntimeError::Conflict {
                stage: stream.stage(),
                message: "command ID was already committed with a different request".to_owned(),
            });
        }

        let range = self.append_events(stream, events)?;
        let committed_at = format_timestamp(command.committed_at);
        self.transaction.execute(
            "INSERT INTO command_commits (
                scope, command_id, request_hash, result_json,
                first_sequence, last_sequence, committed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                command.scope.as_str(),
                command.command_id.as_str(),
                command.request_hash.as_str(),
                command.result_json.as_str(),
                range.first,
                range.last,
                committed_at,
            ],
        )?;
        Ok(CommandCommitResult {
            record: CommandCommitRecord {
                scope: command.scope.clone(),
                command_id: command.command_id.clone(),
                request_hash: command.request_hash.clone(),
                result_json: command.result_json.clone(),
                first_sequence: range.first,
                last_sequence: range.last,
                committed_at: command.committed_at,
            },
            reused: false,
        })
    }

    fn find_command(
        &self,
        scope: &str,
        command_id: &CommandId,
    ) -> Result<Option<CommandCommitRecord>> {
        let mut statement = self.transaction.prepare(
            "SELECT scope, command_id, request_hash, result_json,
                    first_sequence, last_sequence, committed_at
             FROM command_commits
             WHERE scope = ?1 AND command_id = ?2",
        )?;
        statement
            .query_row(params![scope, command_id.as_str()], row_to_command_commit)
            .optional()
            .map_err(Into::into)
    }
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;",
    )?;
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL CHECK (version >= 1),
             applied_at INTEGER NOT NULL
         ) STRICT;",
    )?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (migration_count, current): (i64, i64) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if !(0..=CURRENT_SCHEMA_VERSION).contains(&current) {
        return Err(RuntimeError::Storage {
            message: format!(
                "unsupported storage schema version {current}; supported through {CURRENT_SCHEMA_VERSION}"
            ),
        });
    }
    if migration_count != current {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Storage,
            message: "storage migration history is not contiguous".to_owned(),
        });
    }
    for (version, migration) in [(1_i64, MIGRATION_0001), (2_i64, MIGRATION_0002)]
        .into_iter()
        .filter(|(version, _)| *version > current)
    {
        transaction.execute_batch(migration)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            params![version, Utc::now().timestamp()],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn ensure_parent_directory(path: &Path) -> Result<()> {
    if path == Path::new(":memory:") || path.to_string_lossy().starts_with("file:") {
        return Ok(());
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn validate_event(event: &NewEvent) -> Result<()> {
    validate_scope(&event.scope)?;
    validate_identifier(&event.event_type, "event_type", 128)?;
    if event.event_schema_version == 0 {
        return Err(RuntimeError::validation(
            RuntimeStage::Trace,
            "event schema version must be positive",
        ));
    }
    validate_json(&event.payload_json, "event payload")
}

fn validate_command(command: &NewCommandCommit) -> Result<()> {
    validate_scope(&command.scope)?;
    validate_identifier(&command.request_hash, "request_hash", 128)?;
    validate_json(&command.result_json, "command result")
}

fn validate_scope(scope: &str) -> Result<()> {
    validate_identifier(scope, "scope", 256)
}

fn validate_identifier(value: &str, field: &str, max_bytes: usize) -> Result<()> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(RuntimeError::validation(
            RuntimeStage::Storage,
            format!("{field} must be 1..={max_bytes} bytes"),
        ));
    }
    Ok(())
}

fn validate_json(value: &str, field: &str) -> Result<()> {
    if value.len() > MAX_JSON_BYTES {
        return Err(RuntimeError::validation(
            RuntimeStage::Storage,
            format!("{field} exceeds {MAX_JSON_BYTES} bytes"),
        ));
    }
    serde_json::from_str::<Value>(value).map_err(|error| {
        RuntimeError::validation(
            RuntimeStage::Storage,
            format!("{field} is not valid JSON: {error}"),
        )
    })?;
    Ok(())
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).map(|timestamp| timestamp.with_timezone(&Utc)).map_err(
        |error| RuntimeError::CorruptState {
            stage: RuntimeStage::Storage,
            message: format!("invalid persisted timestamp: {error}"),
        },
    )
}

fn read_events(
    connection: &Connection,
    stream: EventStream,
    scope: &str,
) -> Result<Vec<StoredEvent>> {
    let table = stream.table_name();
    let query = format!(
        "SELECT scope, sequence, owner_subject_id, command_id,
                command_event_index, event_schema_version, event_type,
                recorded_at, payload_json
         FROM {table}
         WHERE scope = ?1
         ORDER BY sequence ASC"
    );
    let mut statement = connection.prepare(&query)?;
    let mut rows = statement.query([scope])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let schema_version: i64 = row.get(5)?;
        let schema_version =
            u32::try_from(schema_version).map_err(|_| RuntimeError::CorruptState {
                stage: RuntimeStage::Storage,
                message: "persisted event schema version is out of range".to_owned(),
            })?;
        let owner_subject_id_raw: String = row.get(2)?;
        let command_id_raw: String = row.get(3)?;
        let recorded_at_raw: String = row.get(7)?;
        events.push(StoredEvent {
            stream,
            scope: row.get(0)?,
            sequence: row.get(1)?,
            owner_subject_id: parse_subject_id(owner_subject_id_raw)?,
            command_id: parse_command_id(command_id_raw)?,
            command_event_index: row.get(4)?,
            event_schema_version: schema_version,
            event_type: row.get(6)?,
            recorded_at: parse_timestamp(&recorded_at_raw)?,
            payload_json: row.get(8)?,
        });
    }
    for (index, event) in events.iter().enumerate() {
        let expected = i64::try_from(index + 1).map_err(|_| RuntimeError::Storage {
            message: "event stream is too large".to_owned(),
        })?;
        if event.sequence != expected {
            return Err(RuntimeError::CorruptState {
                stage: stream.stage(),
                message: "event sequence is not contiguous".to_owned(),
            });
        }
    }
    Ok(events)
}

fn row_to_command_commit(row: &rusqlite::Row<'_>) -> rusqlite::Result<CommandCommitRecord> {
    let command_id_raw: String = row.get(1)?;
    let command_id = parse_command_id(command_id_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let committed_at_raw: String = row.get(6)?;
    let committed_at = parse_timestamp(&committed_at_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(CommandCommitRecord {
        scope: row.get(0)?,
        command_id,
        request_hash: row.get(2)?,
        result_json: row.get(3)?,
        first_sequence: row.get(4)?,
        last_sequence: row.get(5)?,
        committed_at,
    })
}

fn parse_subject_id(value: String) -> Result<SubjectId> {
    SubjectId::from_value(value).map_err(|_| RuntimeError::CorruptState {
        stage: RuntimeStage::Storage,
        message: "persisted owner subject ID is invalid".to_owned(),
    })
}

fn parse_command_id(value: String) -> Result<CommandId> {
    CommandId::from_value(value).map_err(|_| RuntimeError::CorruptState {
        stage: RuntimeStage::Storage,
        message: "persisted command ID is invalid".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::tempdir;

    fn event(command_id: &str, index: usize) -> NewEvent {
        NewEvent {
            scope: "conversation:test".to_owned(),
            owner_subject_id: SubjectId::from_value("subject-test").unwrap(),
            command_id: CommandId::from_value(command_id).unwrap(),
            event_schema_version: 1,
            event_type: format!("event_{index}"),
            recorded_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            payload_json: format!(r#"{{"index":{index}}}"#),
        }
    }

    fn command(command_id: &str, hash: &str) -> NewCommandCommit {
        NewCommandCommit {
            scope: "conversation:test".to_owned(),
            command_id: CommandId::from_value(command_id).unwrap(),
            request_hash: hash.to_owned(),
            result_json: r#"{"ok":true}"#.to_owned(),
            committed_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    #[test]
    fn file_open_configures_schema_and_is_idempotent() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite");
        let storage = Storage::open(&path).unwrap();
        assert_eq!(storage.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        storage
            .with_connection(|connection| {
                let foreign_keys: i64 =
                    connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
                let journal_mode: String =
                    connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
                let synchronous: i64 =
                    connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
                let busy_timeout: i64 =
                    connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
                assert_eq!(foreign_keys, 1);
                assert_eq!(journal_mode, "wal");
                assert_eq!(synchronous, 2);
                assert_eq!(busy_timeout, 5_000);
                Ok(())
            })
            .unwrap();
        drop(storage);
        let reopened = Storage::open(&path).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn command_append_is_idempotent_and_conflicting_reuse_is_rejected() {
        let storage = Storage::open_in_memory().unwrap();
        let first = storage
            .transact(|transaction| {
                transaction.append_events_with_command(
                    EventStream::Lifecycle,
                    &command("command-1", "hash-a"),
                    &[event("command-1", 0), event("command-1", 1)],
                )
            })
            .unwrap();
        assert!(!first.reused);
        assert_eq!(first.record.first_sequence, 1);
        assert_eq!(first.record.last_sequence, 2);

        let reused = storage
            .transact(|transaction| {
                transaction.append_events_with_command(
                    EventStream::Lifecycle,
                    &command("command-1", "hash-a"),
                    &[event("command-1", 0), event("command-1", 1)],
                )
            })
            .unwrap();
        assert!(reused.reused);
        assert_eq!(reused.record.result_json, first.record.result_json);
        assert_eq!(
            storage.read_events(EventStream::Lifecycle, "conversation:test").unwrap().len(),
            2
        );
        let persisted = storage
            .read_command_commit("conversation:test", &CommandId::from_value("command-1").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(persisted.first_sequence, 1);
        assert_eq!(persisted.last_sequence, 2);

        let error = storage
            .transact(|transaction| {
                transaction.append_events_with_command(
                    EventStream::Lifecycle,
                    &command("command-1", "hash-b"),
                    &[event("command-1", 0)],
                )
            })
            .unwrap_err();
        assert!(matches!(error, RuntimeError::Conflict { .. }));
    }

    #[test]
    fn failed_transaction_rolls_back_the_entire_event_batch() {
        let storage = Storage::open_in_memory().unwrap();
        let result: Result<()> = storage.transact(|transaction| {
            transaction.append_events_with_command(
                EventStream::Lifecycle,
                &command("command-rollback", "hash-rollback"),
                &[event("command-rollback", 0), event("command-rollback", 1)],
            )?;
            Err(RuntimeError::Internal { message: "injected failure".to_owned() })
        });
        assert!(result.is_err());
        assert!(
            storage.read_events(EventStream::Lifecycle, "conversation:test").unwrap().is_empty()
        );
        assert!(
            storage
                .read_command_commit(
                    "conversation:test",
                    &CommandId::from_value("command-rollback").unwrap(),
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn append_only_triggers_reject_mutation() {
        let storage = Storage::open_in_memory().unwrap();
        storage
            .transact(|transaction| {
                transaction.append_events_with_command(
                    EventStream::Execution,
                    &command("command-1", "hash-a"),
                    &[event("command-1", 0)],
                )
            })
            .unwrap();
        let update = storage.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE execution_events SET event_type = ?1
                     WHERE scope = ?2 AND sequence = 1",
                    params!["changed", "conversation:test"],
                )
                .map(|_| ())
                .map_err(Into::into)
        });
        assert!(update.is_err());
        let delete = storage.with_connection(|connection| {
            connection
                .execute(
                    "DELETE FROM execution_events WHERE scope = ?1 AND sequence = 1",
                    ["conversation:test"],
                )
                .map(|_| ())
                .map_err(Into::into)
        });
        assert!(delete.is_err());
    }

    #[test]
    fn future_schema_is_rejected() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("future.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY NOT NULL,
                     applied_at INTEGER NOT NULL
                 ) STRICT;
                 INSERT INTO schema_migrations(version, applied_at) VALUES (99, 0);",
            )
            .unwrap();
        drop(connection);
        let error = Storage::open(&path).unwrap_err();
        assert!(matches!(error, RuntimeError::Storage { .. }));
    }

    #[tokio::test]
    async fn blocking_boundary_executes_storage_work_off_executor() {
        let storage = Storage::open_in_memory().unwrap();
        let version = storage.run_blocking(|storage| storage.schema_version()).await.unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }
}

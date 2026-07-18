-- Runtime schema migration 0001.
--
-- The event tables are append-only facts.  Mutable projections and corpus
-- tables are added by later migrations; this migration deliberately keeps the
-- storage seam independent of any business module.

CREATE TABLE lifecycle_events (
    scope               TEXT NOT NULL CHECK (length(scope) BETWEEN 1 AND 256),
    sequence            INTEGER NOT NULL CHECK (sequence >= 1),
    owner_subject_id    TEXT NOT NULL CHECK (length(owner_subject_id) BETWEEN 1 AND 128),
    command_id          TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    command_event_index INTEGER NOT NULL CHECK (command_event_index >= 0),
    event_schema_version INTEGER NOT NULL CHECK (event_schema_version >= 1),
    event_type          TEXT NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    recorded_at         TEXT NOT NULL CHECK (length(recorded_at) BETWEEN 1 AND 64),
    payload_json        TEXT NOT NULL
                        CHECK (
                            json_valid(payload_json)
                            AND length(CAST(payload_json AS BLOB)) <= 16777216
                        ),
    PRIMARY KEY (scope, sequence),
    UNIQUE (scope, command_id, command_event_index)
) STRICT;

CREATE INDEX lifecycle_events_scope_order
    ON lifecycle_events(scope, sequence);

CREATE TABLE execution_events (
    scope               TEXT NOT NULL CHECK (length(scope) BETWEEN 1 AND 256),
    sequence            INTEGER NOT NULL CHECK (sequence >= 1),
    owner_subject_id    TEXT NOT NULL CHECK (length(owner_subject_id) BETWEEN 1 AND 128),
    command_id          TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    command_event_index INTEGER NOT NULL CHECK (command_event_index >= 0),
    event_schema_version INTEGER NOT NULL CHECK (event_schema_version >= 1),
    event_type          TEXT NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    recorded_at         TEXT NOT NULL CHECK (length(recorded_at) BETWEEN 1 AND 64),
    payload_json        TEXT NOT NULL
                        CHECK (
                            json_valid(payload_json)
                            AND length(CAST(payload_json AS BLOB)) <= 16777216
                        ),
    PRIMARY KEY (scope, sequence),
    UNIQUE (scope, command_id, command_event_index)
) STRICT;

CREATE INDEX execution_events_scope_order
    ON execution_events(scope, sequence);

-- A command is committed together with the event sequence range it produced.
-- `scope` identifies one logical event stream (for example a conversation or
-- one Markdown research execution), so sequence numbers are stream-local.
CREATE TABLE command_commits (
    scope          TEXT NOT NULL CHECK (length(scope) BETWEEN 1 AND 256),
    command_id     TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    request_hash   TEXT NOT NULL CHECK (length(request_hash) BETWEEN 1 AND 128),
    result_json    TEXT NOT NULL
                   CHECK (
                       json_valid(result_json)
                       AND length(CAST(result_json AS BLOB)) <= 16777216
                   ),
    first_sequence INTEGER NOT NULL CHECK (first_sequence >= 1),
    last_sequence  INTEGER NOT NULL CHECK (last_sequence >= first_sequence),
    committed_at   TEXT NOT NULL CHECK (length(committed_at) BETWEEN 1 AND 64),
    PRIMARY KEY (scope, command_id)
) STRICT;

CREATE INDEX command_commits_scope_sequence
    ON command_commits(scope, first_sequence, last_sequence);

-- Application code has no update/delete path, and the database reinforces the
-- append-only contract for callers that accidentally use a raw connection.
CREATE TRIGGER lifecycle_events_append_only_update
BEFORE UPDATE ON lifecycle_events
BEGIN
    SELECT RAISE(ABORT, 'lifecycle_events are append-only');
END;

CREATE TRIGGER lifecycle_events_append_only_delete
BEFORE DELETE ON lifecycle_events
BEGIN
    SELECT RAISE(ABORT, 'lifecycle_events are append-only');
END;

CREATE TRIGGER execution_events_append_only_update
BEFORE UPDATE ON execution_events
BEGIN
    SELECT RAISE(ABORT, 'execution_events are append-only');
END;

CREATE TRIGGER execution_events_append_only_delete
BEFORE DELETE ON execution_events
BEGIN
    SELECT RAISE(ABORT, 'execution_events are append-only');
END;

CREATE TRIGGER command_commits_append_only_update
BEFORE UPDATE ON command_commits
BEGIN
    SELECT RAISE(ABORT, 'command_commits are append-only');
END;

CREATE TRIGGER command_commits_append_only_delete
BEFORE DELETE ON command_commits
BEGIN
    SELECT RAISE(ABORT, 'command_commits are append-only');
END;

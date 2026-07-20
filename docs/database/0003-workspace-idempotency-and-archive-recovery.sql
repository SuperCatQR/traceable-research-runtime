-- Workspace HTTP idempotency records and archive recovery.
-- SQLite 3.37+ (STRICT tables)

BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS idempotency_records (
    user_id         TEXT NOT NULL REFERENCES user_accounts(user_id) ON DELETE CASCADE,
    method          TEXT NOT NULL,
    resource_scope  TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash    TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('in_progress', 'completed')),
    status_code     INTEGER,
    response_json   TEXT CHECK (response_json IS NULL OR json_valid(response_json)),
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    PRIMARY KEY (user_id, method, resource_scope, idempotency_key)
) STRICT;

CREATE INDEX IF NOT EXISTS idempotency_records_expiry
    ON idempotency_records(expires_at);

INSERT INTO schema_migrations(version, applied_at)
VALUES (3, unixepoch());

COMMIT;

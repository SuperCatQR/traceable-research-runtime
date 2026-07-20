-- Durable operation identity, serialization locks, and fail-closed legacy claims.
-- SQLite 3.37+ (STRICT tables)

BEGIN IMMEDIATE;

CREATE TABLE idempotency_records_next (
    user_id             TEXT NOT NULL REFERENCES user_accounts(user_id) ON DELETE CASCADE,
    method              TEXT NOT NULL,
    resource_scope      TEXT NOT NULL,
    idempotency_key     TEXT NOT NULL,
    request_hash        TEXT NOT NULL,
    operation_id        TEXT NOT NULL CHECK (length(operation_id) = 32),
    operation_created_at INTEGER NOT NULL,
    claim_token         TEXT NOT NULL CHECK (length(claim_token) = 32),
    claimed_at          INTEGER NOT NULL,
    serialization_key   TEXT,
    status              TEXT NOT NULL CHECK (status IN ('in_progress', 'completed', 'blocked')),
    status_code         INTEGER,
    response_json       TEXT CHECK (response_json IS NULL OR json_valid(response_json)),
    -- created_at is retained as a compatibility alias for the v3-v6 Catalog API.
    created_at          INTEGER NOT NULL,
    expires_at          INTEGER NOT NULL,
    PRIMARY KEY (user_id, method, resource_scope, idempotency_key)
) STRICT;

INSERT INTO idempotency_records_next (
    user_id, method, resource_scope, idempotency_key, request_hash,
    operation_id, operation_created_at, claim_token, claimed_at,
    serialization_key, status, status_code, response_json, created_at, expires_at
)
SELECT user_id, method, resource_scope, idempotency_key, request_hash,
       lower(hex(randomblob(16))), created_at, claim_token, created_at,
       CASE WHEN status = 'in_progress'
            THEN 'legacy:' || user_id || ':' || method || ':' || resource_scope
            ELSE NULL END,
       CASE WHEN status = 'in_progress' THEN 'blocked' ELSE status END,
       status_code, response_json, created_at, expires_at
FROM idempotency_records;

DROP TABLE idempotency_records;
ALTER TABLE idempotency_records_next RENAME TO idempotency_records;

CREATE INDEX idempotency_records_expiry
    ON idempotency_records(expires_at);

CREATE INDEX idempotency_records_operation
    ON idempotency_records(operation_id);

CREATE UNIQUE INDEX idempotency_records_operation_unique
    ON idempotency_records(operation_id);

CREATE UNIQUE INDEX idempotency_records_serialization_active
    ON idempotency_records(user_id, serialization_key)
    WHERE serialization_key IS NOT NULL
      AND status IN ('in_progress', 'blocked');

INSERT INTO schema_migrations(version, applied_at)
VALUES (7, unixepoch());

COMMIT;

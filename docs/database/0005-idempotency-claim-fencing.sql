-- Fence idempotency completion and release with a per-claim token.
-- SQLite 3.37+ (STRICT tables)

BEGIN IMMEDIATE;

CREATE TABLE idempotency_records_next (
    user_id         TEXT NOT NULL REFERENCES user_accounts(user_id) ON DELETE CASCADE,
    method          TEXT NOT NULL,
    resource_scope  TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash    TEXT NOT NULL,
    claim_token     TEXT NOT NULL CHECK (length(claim_token) = 32),
    status          TEXT NOT NULL CHECK (status IN ('in_progress', 'completed')),
    status_code     INTEGER,
    response_json   TEXT CHECK (response_json IS NULL OR json_valid(response_json)),
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    PRIMARY KEY (user_id, method, resource_scope, idempotency_key)
) STRICT;

INSERT INTO idempotency_records_next (
    user_id, method, resource_scope, idempotency_key, request_hash, claim_token,
    status, status_code, response_json, created_at, expires_at
)
SELECT user_id, method, resource_scope, idempotency_key, request_hash,
       lower(hex(randomblob(16))), status, status_code, response_json,
       created_at, expires_at
FROM idempotency_records;

DROP TABLE idempotency_records;
ALTER TABLE idempotency_records_next RENAME TO idempotency_records;

CREATE INDEX idempotency_records_expiry
    ON idempotency_records(expires_at);

INSERT INTO schema_migrations(version, applied_at)
VALUES (5, unixepoch());

COMMIT;

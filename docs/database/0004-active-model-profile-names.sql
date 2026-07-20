-- Allow archived Model Profile display names to be reused.
-- SQLite 3.37+ (STRICT tables)

PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

CREATE TABLE model_profiles_next (
    profile_id          TEXT PRIMARY KEY NOT NULL,
    user_id             TEXT NOT NULL REFERENCES user_accounts(user_id) ON DELETE CASCADE,
    display_name        TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 80),
    api_base_url        TEXT NOT NULL CHECK (length(api_base_url) BETWEEN 8 AND 2048),
    model_id            TEXT NOT NULL CHECK (length(model_id) BETWEEN 1 AND 200),
    api_key_ciphertext  BLOB NOT NULL,
    api_key_nonce       BLOB NOT NULL CHECK (length(api_key_nonce) = 12),
    revision            INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    is_default          INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    verified_at         INTEGER,
    archived_at         INTEGER,
    CHECK (length(profile_id) BETWEEN 32 AND 36)
) STRICT;

INSERT INTO model_profiles_next (
    profile_id, user_id, display_name, api_base_url, model_id,
    api_key_ciphertext, api_key_nonce, revision, is_default,
    created_at, updated_at, verified_at, archived_at
)
SELECT profile_id, user_id, display_name, api_base_url, model_id,
       api_key_ciphertext, api_key_nonce, revision, is_default,
       created_at, updated_at, verified_at, archived_at
FROM model_profiles;

DROP TABLE model_profiles;
ALTER TABLE model_profiles_next RENAME TO model_profiles;

CREATE INDEX model_profiles_user_active
    ON model_profiles(user_id, archived_at, updated_at DESC);

CREATE UNIQUE INDEX model_profiles_one_active_default
    ON model_profiles(user_id)
    WHERE is_default = 1 AND archived_at IS NULL;

CREATE UNIQUE INDEX model_profiles_one_active_name
    ON model_profiles(user_id, display_name)
    WHERE archived_at IS NULL;

INSERT INTO schema_migrations(version, applied_at)
VALUES (4, unixepoch());

COMMIT;
PRAGMA foreign_keys = ON;

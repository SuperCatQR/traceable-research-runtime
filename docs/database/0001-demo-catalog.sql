-- Demo catalogue migration 0001
-- SQLite 3.37+ (STRICT tables)

-- UP
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version     INTEGER PRIMARY KEY NOT NULL,
    applied_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE user_accounts (
    user_id         TEXT PRIMARY KEY NOT NULL,
    normalized_email TEXT NOT NULL UNIQUE,
    display_name    TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 80),
    password_hash   TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    CHECK (length(user_id) BETWEEN 32 AND 36),
    CHECK (length(normalized_email) BETWEEN 3 AND 320)
) STRICT;

CREATE TABLE login_sessions (
    token_hash      BLOB PRIMARY KEY NOT NULL CHECK (length(token_hash) = 32),
    user_id         TEXT NOT NULL REFERENCES user_accounts(user_id) ON DELETE CASCADE,
    created_at      INTEGER NOT NULL,
    last_seen_at    INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    revoked_at      INTEGER,
    CHECK (expires_at > created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at)
) STRICT;

CREATE INDEX login_sessions_user_expiry
    ON login_sessions(user_id, expires_at DESC);

CREATE TABLE model_profiles (
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
    CHECK (length(profile_id) BETWEEN 32 AND 36),
    UNIQUE (user_id, display_name)
) STRICT;

CREATE INDEX model_profiles_user_active
    ON model_profiles(user_id, archived_at, updated_at DESC);

CREATE UNIQUE INDEX model_profiles_one_active_default
    ON model_profiles(user_id)
    WHERE is_default = 1 AND archived_at IS NULL;

CREATE TABLE research_conversations (
    conversation_id         TEXT PRIMARY KEY NOT NULL,
    user_id                 TEXT NOT NULL REFERENCES user_accounts(user_id) ON DELETE CASCADE,
    core_conversation_id    TEXT NOT NULL UNIQUE,
    title                   TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    model_profile_id        TEXT NOT NULL REFERENCES model_profiles(profile_id) ON DELETE RESTRICT,
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL,
    archived_at             INTEGER,
    CHECK (length(conversation_id) BETWEEN 32 AND 36)
) STRICT;

CREATE INDEX research_conversations_user_activity
    ON research_conversations(user_id, archived_at, updated_at DESC);

CREATE INDEX research_conversations_model_profile
    ON research_conversations(model_profile_id);

CREATE TABLE research_turns (
    turn_id                 TEXT PRIMARY KEY NOT NULL,
    conversation_id         TEXT NOT NULL REFERENCES research_conversations(conversation_id) ON DELETE CASCADE,
    turn_number             INTEGER NOT NULL CHECK (turn_number >= 1),
    clarification_id        TEXT NOT NULL UNIQUE,
    run_id                  TEXT,
    user_question           TEXT NOT NULL CHECK (length(user_question) BETWEEN 1 AND 4000),
    status                  TEXT NOT NULL CHECK (status IN ('clarifying', 'ready', 'running', 'completed', 'failed', 'cancelled')),
    model_profile_id        TEXT NOT NULL REFERENCES model_profiles(profile_id) ON DELETE RESTRICT,
    model_profile_revision  INTEGER NOT NULL CHECK (model_profile_revision >= 1),
    model_api_base_url      TEXT NOT NULL,
    model_id                TEXT NOT NULL,
    answer_json             TEXT CHECK (answer_json IS NULL OR json_valid(answer_json)),
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL,
    completed_at            INTEGER,
    UNIQUE (conversation_id, turn_number)
) STRICT;

CREATE INDEX research_turns_conversation_order
    ON research_turns(conversation_id, turn_number);

CREATE INDEX research_turns_active_profile_revision
    ON research_turns(model_profile_id, model_profile_revision, status);

INSERT INTO schema_migrations(version, applied_at)
VALUES (1, unixepoch());

-- DOWN
-- Run only after the application no longer reads catalogue version 1.
-- BEGIN IMMEDIATE;
-- DROP TABLE research_turns;
-- DROP TABLE research_conversations;
-- DROP TABLE model_profiles;
-- DROP TABLE login_sessions;
-- DROP TABLE user_accounts;
-- DELETE FROM schema_migrations WHERE version = 1;
-- DROP TABLE schema_migrations;
-- COMMIT;

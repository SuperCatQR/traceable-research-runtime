-- Demo catalogue migration 0002
-- Freeze the requested answer-composition style for every Research Turn.

-- UP
BEGIN IMMEDIATE;

ALTER TABLE research_turns
    ADD COLUMN answer_style TEXT NOT NULL DEFAULT 'web_first'
    CHECK (answer_style IN ('web_first', 'knowledge_first'));

INSERT INTO schema_migrations(version, applied_at)
VALUES (2, unixepoch());

COMMIT;

-- DOWN
-- SQLite cannot drop a column without rebuilding the table. Keep this migration additive.

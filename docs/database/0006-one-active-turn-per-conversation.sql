-- Enforce at most one nonterminal Research Turn per Conversation.
-- SQLite 3.37+ (STRICT tables)

BEGIN IMMEDIATE;

CREATE UNIQUE INDEX research_turns_one_active_per_conversation
    ON research_turns(conversation_id)
    WHERE status IN ('clarifying', 'ready', 'running');

INSERT INTO schema_migrations(version, applied_at)
VALUES (6, unixepoch());

COMMIT;

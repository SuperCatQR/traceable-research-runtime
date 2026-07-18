-- Immutable, content-addressed Markdown Corpus storage.

CREATE TABLE markdown_source_document_versions (
    owner_subject_id TEXT NOT NULL CHECK (length(owner_subject_id) BETWEEN 1 AND 128),
    markdown_source_document_id TEXT NOT NULL CHECK (length(markdown_source_document_id) BETWEEN 1 AND 128),
    markdown_source_document_version_id TEXT NOT NULL CHECK (length(markdown_source_document_version_id) BETWEEN 1 AND 128),
    markdown_source_document_version_content_hash TEXT NOT NULL CHECK (length(markdown_source_document_version_content_hash) = 71),
    version_json TEXT NOT NULL
                 CHECK (
                     json_valid(version_json)
                     AND length(CAST(version_json AS BLOB)) <= 33554432
                 ),
    PRIMARY KEY (owner_subject_id, markdown_source_document_version_id),
    UNIQUE (
        owner_subject_id,
        markdown_source_document_id,
        markdown_source_document_version_content_hash
    )
) STRICT;

CREATE TABLE markdown_corpus_snapshots (
    owner_subject_id TEXT NOT NULL CHECK (length(owner_subject_id) BETWEEN 1 AND 128),
    markdown_corpus_snapshot_id TEXT NOT NULL CHECK (length(markdown_corpus_snapshot_id) BETWEEN 1 AND 128),
    markdown_corpus_snapshot_hash TEXT NOT NULL CHECK (length(markdown_corpus_snapshot_hash) = 71),
    root_markdown_corpus_navigation_node_id TEXT NOT NULL CHECK (length(root_markdown_corpus_navigation_node_id) BETWEEN 1 AND 128),
    markdown_corpus_snapshot_published_at TEXT NOT NULL CHECK (length(markdown_corpus_snapshot_published_at) BETWEEN 1 AND 64),
    snapshot_json TEXT NOT NULL
                  CHECK (
                      json_valid(snapshot_json)
                      AND length(CAST(snapshot_json AS BLOB)) <= 629145600
                  ),
    PRIMARY KEY (owner_subject_id, markdown_corpus_snapshot_id),
    UNIQUE (owner_subject_id, markdown_corpus_snapshot_hash)
) STRICT;

CREATE TABLE markdown_corpus_snapshot_document_versions (
    owner_subject_id TEXT NOT NULL,
    markdown_corpus_snapshot_id TEXT NOT NULL,
    markdown_source_document_id TEXT NOT NULL,
    markdown_source_document_version_id TEXT NOT NULL,
    PRIMARY KEY (
        owner_subject_id,
        markdown_corpus_snapshot_id,
        markdown_source_document_id
    ),
    FOREIGN KEY (owner_subject_id, markdown_corpus_snapshot_id)
        REFERENCES markdown_corpus_snapshots(owner_subject_id, markdown_corpus_snapshot_id),
    FOREIGN KEY (owner_subject_id, markdown_source_document_version_id)
        REFERENCES markdown_source_document_versions(owner_subject_id, markdown_source_document_version_id)
) STRICT;

CREATE TRIGGER markdown_source_document_versions_immutable_update
BEFORE UPDATE ON markdown_source_document_versions
BEGIN
    SELECT RAISE(ABORT, 'markdown_source_document_versions are immutable');
END;

CREATE TRIGGER markdown_source_document_versions_immutable_delete
BEFORE DELETE ON markdown_source_document_versions
BEGIN
    SELECT RAISE(ABORT, 'markdown_source_document_versions are immutable');
END;

CREATE TRIGGER markdown_corpus_snapshots_immutable_update
BEFORE UPDATE ON markdown_corpus_snapshots
BEGIN
    SELECT RAISE(ABORT, 'markdown_corpus_snapshots are immutable');
END;

CREATE TRIGGER markdown_corpus_snapshots_immutable_delete
BEFORE DELETE ON markdown_corpus_snapshots
BEGIN
    SELECT RAISE(ABORT, 'markdown_corpus_snapshots are immutable');
END;

CREATE TRIGGER markdown_corpus_snapshot_document_versions_immutable_update
BEFORE UPDATE ON markdown_corpus_snapshot_document_versions
BEGIN
    SELECT RAISE(ABORT, 'markdown_corpus_snapshot_document_versions are immutable');
END;

CREATE TRIGGER markdown_corpus_snapshot_document_versions_immutable_delete
BEFORE DELETE ON markdown_corpus_snapshot_document_versions
BEGIN
    SELECT RAISE(ABORT, 'markdown_corpus_snapshot_document_versions are immutable');
END;

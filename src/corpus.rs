//! Versioned Markdown corpus publication and snapshot-bound reading.

use crate::domain::{canonical_content_hash, canonical_json_bytes, sha256_content_hash};
use crate::error::{Result, RuntimeError, RuntimeStage};
use crate::identity::{
    MarkdownCorpusNavigationNodeId, MarkdownCorpusSnapshotId, MarkdownSourceDocumentId,
    MarkdownSourceDocumentVersionId, MarkdownSourceSegmentId, PrincipalCapability,
    ResearchPrincipal, SubjectId,
};
use crate::storage::Storage;
use chrono::{DateTime, Utc};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

/// Markdown source schema version.
pub const MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION: u32 = 1;
/// Parser and offset schema version.
pub const MARKDOWN_PARSER_SCHEMA_VERSION: u32 = 1;
/// Canonicalization algorithm version.
pub const MARKDOWN_CANONICALIZATION_SCHEMA_VERSION: u32 = 1;
/// Navigation schema version.
pub const MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION: u32 = 1;
/// Snapshot hash preimage schema version.
pub const MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION: u32 = 1;
/// Maximum bytes accepted for one source file.
pub const MAX_MARKDOWN_SOURCE_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum bytes accepted for one published corpus.
pub const MAX_MARKDOWN_CORPUS_BYTES: usize = 512 * 1024 * 1024;
/// Maximum source documents in one publication.
pub const MAX_MARKDOWN_SOURCE_DOCUMENTS: usize = 10_000;
/// Maximum navigation nodes in one publication.
pub const MAX_NAVIGATION_NODES: usize = 50_000;
/// Maximum child edges or linked documents per node.
pub const MAX_NAVIGATION_LINKS_PER_NODE: usize = 1_000;
/// Maximum total navigation edges.
pub const MAX_NAVIGATION_EDGES: usize = 1_000_000;
/// Maximum bytes in one segment.
pub const MAX_MARKDOWN_SOURCE_SEGMENT_BYTES: usize = 256 * 1024;
/// Maximum title bytes.
pub const MAX_MARKDOWN_DOCUMENT_TITLE_BYTES: usize = 512;
/// Maximum abstract bytes.
pub const MAX_MARKDOWN_DOCUMENT_ABSTRACT_BYTES: usize = 4 * 1024;

/// A publication input whose bytes have not yet been canonicalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownSourceDocumentInput {
    /// Path relative to the caller's corpus root.
    pub relative_path: String,
    /// Raw UTF-8 bytes.
    pub markdown_source_bytes: Vec<u8>,
}

/// A navigation node supplied by the caller; navigation generation is outside this Module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationNodeInput {
    /// Stable navigation ID.
    pub markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Display label.
    pub markdown_corpus_navigation_node_label: String,
    /// Short display summary.
    pub markdown_corpus_navigation_node_summary: String,
    /// Direct child node IDs.
    pub child_markdown_corpus_navigation_node_ids: Vec<MarkdownCorpusNavigationNodeId>,
    /// Source documents linked to this node.
    pub linked_markdown_source_document_ids: Vec<MarkdownSourceDocumentId>,
}

/// The complete immutable publication input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishMarkdownCorpusSnapshotInput {
    /// Source documents.
    pub markdown_source_documents: Vec<MarkdownSourceDocumentInput>,
    /// Navigation nodes.
    pub markdown_corpus_navigation_nodes: Vec<MarkdownCorpusNavigationNodeInput>,
    /// The one root node.
    pub root_markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
}

/// Persistent Versioned Markdown Corpus Module.
#[derive(Debug, Clone)]
pub struct VersionedMarkdownCorpus {
    storage: Storage,
}

impl VersionedMarkdownCorpus {
    /// Opens a file-backed corpus store and applies storage migrations.
    #[allow(dead_code)]
    pub(crate) fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { storage: Storage::open(database_path)? })
    }

    /// Creates the Module from the Runtime's shared storage handle.
    pub(crate) fn from_storage(storage: Storage) -> Self {
        Self { storage }
    }

    /// Validates and atomically publishes an immutable Markdown Corpus Snapshot.
    pub async fn publish_markdown_corpus_snapshot(
        &self,
        principal: &ResearchPrincipal,
        input: PublishMarkdownCorpusSnapshotInput,
        published_at: DateTime<Utc>,
    ) -> Result<MarkdownCorpusSnapshot> {
        principal.require(PrincipalCapability::PublishMarkdownCorpusSnapshot)?;
        let snapshot =
            build_markdown_corpus_snapshot(principal.subject_id.clone(), input, published_at)?;
        let persisted = snapshot.clone();
        self.storage
            .run_blocking(move |storage| persist_markdown_corpus_snapshot(storage, &persisted))
            .await?;
        Ok(snapshot)
    }

    /// Opens and revalidates one snapshot owned by the principal.
    pub async fn open_markdown_corpus_snapshot(
        &self,
        principal: &ResearchPrincipal,
        snapshot_id: &MarkdownCorpusSnapshotId,
    ) -> Result<MarkdownCorpusSnapshot> {
        let owner_subject_id = principal.subject_id.clone();
        let snapshot_id = snapshot_id.clone();
        self.storage
            .run_blocking(move |storage| {
                load_markdown_corpus_snapshot(storage, &owner_subject_id, &snapshot_id)
            })
            .await
    }
}

/// A parsed, immutable version of one Markdown source document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownSourceDocumentVersion {
    /// Stable document identity from front matter.
    pub markdown_source_document_id: MarkdownSourceDocumentId,
    /// Content-addressed version identity.
    pub markdown_source_document_version_id: MarkdownSourceDocumentVersionId,
    /// Normalized relative path (not exposed to models).
    pub relative_path: String,
    /// Unique level-one heading text.
    pub markdown_source_document_title: String,
    /// First ordinary paragraph after the heading.
    pub markdown_source_document_abstract: String,
    /// Canonical full source, including front matter and metadata.
    pub canonical_markdown_source: String,
    /// Canonical Markdown body used for evidence offsets.
    pub canonical_markdown_document_body: String,
    /// Hash of the canonical source and parser versions.
    pub markdown_source_document_version_content_hash: String,
    /// Mechanical body segments.
    pub markdown_source_segments: Vec<MarkdownSourceSegment>,
    /// Source schema version.
    pub markdown_source_document_schema_version: u32,
    /// Parser schema version.
    pub markdown_parser_schema_version: u32,
    /// Canonicalization schema version.
    pub markdown_canonicalization_schema_version: u32,
}

/// A mechanical, contiguous region of a canonical Markdown body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownSourceSegment {
    /// Version-local segment ID.
    pub markdown_source_segment_id: MarkdownSourceSegmentId,
    /// Most recent heading, if any.
    pub markdown_source_segment_section_heading: Option<String>,
    /// Inclusive canonical body byte offset.
    pub markdown_source_segment_start_byte_offset_in_document: u64,
    /// Exclusive canonical body byte offset.
    pub markdown_source_segment_end_byte_offset_in_document: u64,
    /// Hash of the exact segment bytes.
    pub markdown_source_segment_hash: String,
    /// Exact segment text, retained for a snapshot-bound read.
    pub canonical_markdown_source_segment_text: String,
}

impl MarkdownSourceSegment {
    /// Returns the segment byte range.
    #[must_use]
    pub const fn byte_range(&self) -> std::ops::Range<usize> {
        self.markdown_source_segment_start_byte_offset_in_document as usize
            ..self.markdown_source_segment_end_byte_offset_in_document as usize
    }
}

/// A validated navigation node inside an immutable snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusNavigationNode {
    /// Stable node ID.
    pub markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Label.
    pub markdown_corpus_navigation_node_label: String,
    /// Summary.
    pub markdown_corpus_navigation_node_summary: String,
    /// Direct child IDs.
    pub child_markdown_corpus_navigation_node_ids: Vec<MarkdownCorpusNavigationNodeId>,
    /// Linked source document IDs.
    pub linked_markdown_source_document_ids: Vec<MarkdownSourceDocumentId>,
}

/// An immutable Markdown Corpus Snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCorpusSnapshot {
    /// Owning principal.
    pub owner_subject_id: SubjectId,
    /// Content-addressed snapshot ID.
    pub markdown_corpus_snapshot_id: MarkdownCorpusSnapshotId,
    /// Hash of all source/navigation inputs and schema versions.
    pub markdown_corpus_snapshot_hash: String,
    /// One navigation root.
    pub root_markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId,
    /// Sorted document versions.
    pub markdown_source_document_versions: Vec<MarkdownSourceDocumentVersion>,
    /// Sorted navigation nodes.
    pub markdown_corpus_navigation_nodes: Vec<MarkdownCorpusNavigationNode>,
    /// Publication time.
    pub markdown_corpus_snapshot_published_at: DateTime<Utc>,
    /// Navigation schema version.
    pub markdown_corpus_navigation_schema_version: u32,
    /// Snapshot hash schema version.
    pub markdown_corpus_snapshot_hash_schema_version: u32,
}

impl MarkdownCorpusSnapshot {
    /// Opens a reader bound to this exact snapshot.
    #[must_use]
    pub fn reader(&self) -> MarkdownCorpusSnapshotReader<'_> {
        MarkdownCorpusSnapshotReader { snapshot: self }
    }
}

/// A read-only view that cannot cross snapshot boundaries.
pub struct MarkdownCorpusSnapshotReader<'a> {
    snapshot: &'a MarkdownCorpusSnapshot,
}

impl<'a> MarkdownCorpusSnapshotReader<'a> {
    /// Returns the complete direct child node candidate set.
    pub fn list_direct_child_markdown_corpus_navigation_nodes(
        &self,
        parent_id: &MarkdownCorpusNavigationNodeId,
    ) -> Result<Vec<&'a MarkdownCorpusNavigationNode>> {
        let parent = self.node(parent_id)?;
        parent.child_markdown_corpus_navigation_node_ids.iter().map(|id| self.node(id)).collect()
    }

    /// Returns title/abstract candidates linked to a node.
    pub fn list_branch_document_abstracts(
        &self,
        node_id: &MarkdownCorpusNavigationNodeId,
    ) -> Result<Vec<MarkdownSourceDocumentAbstractCandidate<'a>>> {
        let node = self.node(node_id)?;
        node.linked_markdown_source_document_ids
            .iter()
            .map(|document_id| {
                let document = self.document(document_id)?;
                Ok(MarkdownSourceDocumentAbstractCandidate {
                    markdown_source_document_id: &document.markdown_source_document_id,
                    markdown_source_document_title: &document.markdown_source_document_title,
                    markdown_source_document_abstract: &document.markdown_source_document_abstract,
                })
            })
            .collect()
    }

    /// Returns all segment metadata for one document.
    #[allow(dead_code)]
    pub(crate) fn list_markdown_source_segments(
        &self,
        document_id: &MarkdownSourceDocumentId,
    ) -> Result<&'a [MarkdownSourceSegment]> {
        Ok(&self.document(document_id)?.markdown_source_segments)
    }

    /// Reads one exact segment and rechecks its hash against canonical body bytes.
    pub fn read_authorized_markdown_source_segment(
        &self,
        document_id: &MarkdownSourceDocumentId,
        segment_id: &MarkdownSourceSegmentId,
    ) -> Result<AuthorizedMarkdownSourceSegment<'a>> {
        let document = self.document(document_id)?;
        let segment = document
            .markdown_source_segments
            .iter()
            .find(|segment| &segment.markdown_source_segment_id == segment_id)
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus })?;
        let bytes = document.canonical_markdown_document_body.as_bytes();
        let range = segment.byte_range();
        let actual = bytes.get(range.clone()).ok_or_else(|| RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "segment offset is outside canonical body".to_owned(),
        })?;
        if sha256_content_hash(actual) != segment.markdown_source_segment_hash {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Corpus,
                message: "segment hash does not match canonical body".to_owned(),
            });
        }
        let text = std::str::from_utf8(actual).map_err(|_| RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "segment is not valid UTF-8".to_owned(),
        })?;
        Ok(AuthorizedMarkdownSourceSegment {
            markdown_source_document_id: &document.markdown_source_document_id,
            markdown_source_segment_id: &segment.markdown_source_segment_id,
            markdown_source_segment_hash: &segment.markdown_source_segment_hash,
            markdown_source_segment_start_byte_offset_in_document: segment
                .markdown_source_segment_start_byte_offset_in_document,
            canonical_markdown_source_segment_text: text,
        })
    }

    /// Returns the immutable snapshot ID.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn markdown_corpus_snapshot_id(&self) -> &MarkdownCorpusSnapshotId {
        &self.snapshot.markdown_corpus_snapshot_id
    }

    fn node(
        &self,
        id: &MarkdownCorpusNavigationNodeId,
    ) -> Result<&'a MarkdownCorpusNavigationNode> {
        self.snapshot
            .markdown_corpus_navigation_nodes
            .iter()
            .find(|node| &node.markdown_corpus_navigation_node_id == id)
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus })
    }

    fn document(&self, id: &MarkdownSourceDocumentId) -> Result<&'a MarkdownSourceDocumentVersion> {
        self.snapshot
            .markdown_source_document_versions
            .iter()
            .find(|document| &document.markdown_source_document_id == id)
            .ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus })
    }
}

/// A title/abstract candidate exposed to a branch report task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownSourceDocumentAbstractCandidate<'a> {
    /// Stable document ID.
    pub markdown_source_document_id: &'a MarkdownSourceDocumentId,
    /// Title.
    pub markdown_source_document_title: &'a str,
    /// Abstract.
    pub markdown_source_document_abstract: &'a str,
}

/// The one authorized segment payload exposed to a review/extraction task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorizedMarkdownSourceSegment<'a> {
    /// Source document ID.
    pub markdown_source_document_id: &'a MarkdownSourceDocumentId,
    /// Segment ID.
    pub markdown_source_segment_id: &'a MarkdownSourceSegmentId,
    /// Segment content hash.
    pub markdown_source_segment_hash: &'a str,
    /// Absolute body start offset.
    pub markdown_source_segment_start_byte_offset_in_document: u64,
    /// Exact authorized text.
    pub canonical_markdown_source_segment_text: &'a str,
}

/// Parses and canonicalizes one Markdown source document.
pub fn parse_markdown_source_document(
    input: &MarkdownSourceDocumentInput,
) -> Result<MarkdownSourceDocumentVersion> {
    let relative_path = normalize_relative_path(&input.relative_path)?;
    if input.markdown_source_bytes.len() > MAX_MARKDOWN_SOURCE_FILE_BYTES {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "Markdown source file exceeds the 16 MiB cap",
        ));
    }
    let raw = std::str::from_utf8(&input.markdown_source_bytes).map_err(|_| {
        RuntimeError::validation(RuntimeStage::Corpus, "Markdown source is not valid UTF-8")
    })?;
    if raw.starts_with('\u{feff}') || raw.contains('\0') {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "Markdown source contains a BOM or NUL",
        ));
    }
    let mut canonical = raw.replace("\r\n", "\n");
    if canonical.contains('\r') {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "Markdown source contains an isolated carriage return",
        ));
    }
    while canonical.ends_with('\n') {
        canonical.pop();
    }
    canonical.push('\n');
    let (document_id, markdown_body_start) = parse_front_matter(&canonical)?;
    let markdown = &canonical[markdown_body_start..];
    let parsed = parse_metadata_blocks(markdown)?;
    let body = remove_leading_blank_lines(&markdown[parsed.abstract_end..]);
    if body.trim().is_empty() {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "Markdown source has no canonical body after abstract",
        ));
    }
    let canonical_body = ensure_one_trailing_lf(body);
    let segments = split_markdown_body(&canonical_body, &document_id)?;
    let version_hash_input = DocumentVersionHashInput {
        canonical_markdown_source: &canonical,
        markdown_source_document_id: &document_id,
        markdown_source_document_schema_version: MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION,
        markdown_parser_schema_version: MARKDOWN_PARSER_SCHEMA_VERSION,
        markdown_canonicalization_schema_version: MARKDOWN_CANONICALIZATION_SCHEMA_VERSION,
    };
    let version_hash = canonical_content_hash(&version_hash_input)?;
    let version_id = MarkdownSourceDocumentVersionId::from_value(format!(
        "markdown-source-document-version-{}",
        version_hash.trim_start_matches("sha256:").get(..32).unwrap_or("short")
    ))?;
    Ok(MarkdownSourceDocumentVersion {
        markdown_source_document_id: document_id,
        markdown_source_document_version_id: version_id,
        relative_path,
        markdown_source_document_title: parsed.title,
        markdown_source_document_abstract: parsed.abstract_text,
        canonical_markdown_source: canonical,
        canonical_markdown_document_body: canonical_body,
        markdown_source_document_version_content_hash: version_hash,
        markdown_source_segments: segments,
        markdown_source_document_schema_version: MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION,
        markdown_parser_schema_version: MARKDOWN_PARSER_SCHEMA_VERSION,
        markdown_canonicalization_schema_version: MARKDOWN_CANONICALIZATION_SCHEMA_VERSION,
    })
}

/// Builds an immutable snapshot after validating all documents and navigation.
pub fn build_markdown_corpus_snapshot(
    owner_subject_id: SubjectId,
    input: PublishMarkdownCorpusSnapshotInput,
    published_at: DateTime<Utc>,
) -> Result<MarkdownCorpusSnapshot> {
    if input.markdown_source_documents.is_empty()
        || input.markdown_source_documents.len() > MAX_MARKDOWN_SOURCE_DOCUMENTS
    {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "publication must contain 1..=10000 Markdown source documents",
        ));
    }
    let mut total_bytes = 0usize;
    let mut relative_paths = BTreeSet::new();
    for document in &input.markdown_source_documents {
        total_bytes =
            total_bytes.checked_add(document.markdown_source_bytes.len()).ok_or_else(|| {
                RuntimeError::validation(
                    RuntimeStage::Corpus,
                    "publication exceeds the total Markdown corpus byte cap",
                )
            })?;
        if total_bytes > MAX_MARKDOWN_CORPUS_BYTES {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "publication exceeds the total Markdown corpus byte cap",
            ));
        }
        let normalized_path = normalize_relative_path(&document.relative_path)?;
        if !relative_paths.insert(normalized_path) {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "duplicate normalized Markdown source relative path",
            ));
        }
    }
    let mut versions = Vec::with_capacity(input.markdown_source_documents.len());
    let mut document_ids = BTreeSet::new();
    for document in &input.markdown_source_documents {
        let version = parse_markdown_source_document(document)?;
        if !document_ids.insert(version.markdown_source_document_id.as_str().to_owned()) {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "duplicate markdown_source_document_id",
            ));
        }
        versions.push(version);
    }
    versions.sort_by(|left, right| {
        left.markdown_source_document_id.as_str().cmp(right.markdown_source_document_id.as_str())
    });
    let nodes = validate_navigation(
        input.markdown_corpus_navigation_nodes,
        &document_ids,
        &input.root_markdown_corpus_navigation_node_id,
    )?;
    let hash_input = SnapshotHashInput {
        root_markdown_corpus_navigation_node_id: &input.root_markdown_corpus_navigation_node_id,
        markdown_source_document_versions: &versions,
        markdown_corpus_navigation_nodes: &nodes,
        markdown_source_document_schema_version: MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION,
        markdown_parser_schema_version: MARKDOWN_PARSER_SCHEMA_VERSION,
        markdown_canonicalization_schema_version: MARKDOWN_CANONICALIZATION_SCHEMA_VERSION,
        markdown_corpus_navigation_schema_version: MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION,
        markdown_corpus_snapshot_hash_schema_version: MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION,
    };
    let snapshot_hash = canonical_content_hash(&hash_input)?;
    let snapshot_id = MarkdownCorpusSnapshotId::from_value(format!(
        "markdown-corpus-snapshot-{}",
        snapshot_hash.trim_start_matches("sha256:").get(..32).unwrap_or("short")
    ))?;
    Ok(MarkdownCorpusSnapshot {
        owner_subject_id,
        markdown_corpus_snapshot_id: snapshot_id,
        markdown_corpus_snapshot_hash: snapshot_hash,
        root_markdown_corpus_navigation_node_id: input.root_markdown_corpus_navigation_node_id,
        markdown_source_document_versions: versions,
        markdown_corpus_navigation_nodes: nodes,
        markdown_corpus_snapshot_published_at: published_at,
        markdown_corpus_navigation_schema_version: MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION,
        markdown_corpus_snapshot_hash_schema_version: MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION,
    })
}

#[derive(Debug, Serialize)]
struct DocumentVersionHashInput<'a> {
    canonical_markdown_source: &'a str,
    markdown_source_document_id: &'a MarkdownSourceDocumentId,
    markdown_source_document_schema_version: u32,
    markdown_parser_schema_version: u32,
    markdown_canonicalization_schema_version: u32,
}

#[derive(Debug, Serialize)]
struct SnapshotHashInput<'a> {
    root_markdown_corpus_navigation_node_id: &'a MarkdownCorpusNavigationNodeId,
    markdown_source_document_versions: &'a [MarkdownSourceDocumentVersion],
    markdown_corpus_navigation_nodes: &'a [MarkdownCorpusNavigationNode],
    markdown_source_document_schema_version: u32,
    markdown_parser_schema_version: u32,
    markdown_canonicalization_schema_version: u32,
    markdown_corpus_navigation_schema_version: u32,
    markdown_corpus_snapshot_hash_schema_version: u32,
}

fn normalize_relative_path(path: &str) -> Result<String> {
    if path.is_empty() || path.contains('\\') || path.contains('\0') {
        return Err(RuntimeError::validation(RuntimeStage::Corpus, "relative path is invalid"));
    }
    if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "absolute or drive-qualified paths are forbidden",
        ));
    }
    let mut normalized = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "relative path contains an empty or traversal segment",
            ));
        }
        normalized.push(part);
    }
    Ok(normalized.join("/"))
}

fn parse_front_matter(canonical: &str) -> Result<(MarkdownSourceDocumentId, usize)> {
    if !canonical.starts_with("---\n") {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "Markdown source must begin with YAML front matter",
        ));
    }
    let rest = &canonical[4..];
    let mut delimiter_offset = None;
    let mut offset = 4;
    for line in rest.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        if content == "---" {
            delimiter_offset = Some(offset + line.len());
            break;
        }
        offset += line.len();
    }
    let end = delimiter_offset.ok_or_else(|| {
        RuntimeError::validation(RuntimeStage::Corpus, "front matter closing delimiter is missing")
    })?;
    let yaml = &canonical[4..end - 4];
    let key_count = yaml
        .lines()
        .filter(|line| line.trim_start().starts_with("markdown_source_document_id:"))
        .count();
    if key_count != 1
        || yaml
            .lines()
            .filter(|line| !line.trim().is_empty())
            .any(|line| !line.trim_start().starts_with("markdown_source_document_id:"))
    {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "front matter must contain exactly one allowed field",
        ));
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FrontMatter {
        markdown_source_document_id: String,
    }
    let front_matter: FrontMatter = serde_yaml_ng::from_str(yaml).map_err(|error| {
        RuntimeError::validation(RuntimeStage::Corpus, format!("invalid front matter: {error}"))
    })?;
    let id = MarkdownSourceDocumentId::from_value(front_matter.markdown_source_document_id)?;
    Ok((id, end))
}

struct ParsedMetadata {
    title: String,
    abstract_text: String,
    abstract_end: usize,
}

fn parse_metadata_blocks(markdown: &str) -> Result<ParsedMetadata> {
    let parser = Parser::new_ext(markdown, pulldown_cmark::Options::empty()).into_offset_iter();
    let mut heading_count = 0;
    let mut title = None;
    let mut abstract_text = None;
    let mut abstract_end = None;
    let mut active: Option<ActiveBlock> = None;
    let mut saw_h1_end = false;
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level: HeadingLevel::H1, .. }) => {
                heading_count += 1;
                if heading_count > 1 {
                    return Err(RuntimeError::validation(
                        RuntimeStage::Corpus,
                        "Markdown source must contain exactly one level-one heading",
                    ));
                }
                active = Some(ActiveBlock::new(range.start));
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => {
                let block = active.take().ok_or_else(|| {
                    RuntimeError::validation(RuntimeStage::Corpus, "malformed level-one heading")
                })?;
                let normalized = normalize_metadata_text(&block.text, "title")?;
                title = Some(normalized);
                saw_h1_end = true;
            }
            Event::Start(Tag::Paragraph) if saw_h1_end && abstract_text.is_none() => {
                active = Some(ActiveBlock::new(range.start));
            }
            Event::End(TagEnd::Paragraph) if active.is_some() && abstract_text.is_none() => {
                let block = active.take().ok_or_else(|| {
                    RuntimeError::validation(RuntimeStage::Corpus, "malformed abstract paragraph")
                })?;
                let normalized = normalize_metadata_text(&block.text, "abstract")?;
                abstract_text = Some(normalized);
                abstract_end = Some(range.end);
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(block) = &mut active {
                    block.text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(block) = &mut active {
                    block.text.push(' ');
                }
            }
            Event::Html(_)
            | Event::InlineHtml(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
                if active.is_some() =>
            {
                return Err(RuntimeError::validation(
                    RuntimeStage::Corpus,
                    "raw HTML or math is not allowed in title/abstract",
                ));
            }
            _ => {}
        }
    }
    if heading_count != 1 || !saw_h1_end {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "Markdown source must contain one level-one heading",
        ));
    }
    Ok(ParsedMetadata {
        title: title
            .ok_or_else(|| RuntimeError::validation(RuntimeStage::Corpus, "title missing"))?,
        abstract_text: abstract_text
            .ok_or_else(|| RuntimeError::validation(RuntimeStage::Corpus, "abstract missing"))?,
        abstract_end: abstract_end.ok_or_else(|| {
            RuntimeError::validation(RuntimeStage::Corpus, "abstract offset missing")
        })?,
    })
}

struct ActiveBlock {
    #[allow(dead_code)]
    start: usize,
    text: String,
}

impl ActiveBlock {
    fn new(start: usize) -> Self {
        Self { start, text: String::new() }
    }
}

fn normalize_metadata_text(text: &str, name: &str) -> Result<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            format!("{name} must not be empty"),
        ));
    }
    let max = if name == "title" {
        MAX_MARKDOWN_DOCUMENT_TITLE_BYTES
    } else {
        MAX_MARKDOWN_DOCUMENT_ABSTRACT_BYTES
    };
    if normalized.len() > max {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            format!("{name} exceeds {max} bytes"),
        ));
    }
    Ok(normalized)
}

fn remove_leading_blank_lines(text: &str) -> &str {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            offset += line.len();
        } else {
            break;
        }
    }
    &text[offset..]
}

fn ensure_one_trailing_lf(text: &str) -> String {
    let mut text = text.to_owned();
    while text.ends_with('\n') {
        text.pop();
    }
    text.push('\n');
    text
}

fn split_markdown_body(
    body: &str,
    document_id: &MarkdownSourceDocumentId,
) -> Result<Vec<MarkdownSourceSegment>> {
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut block_start = None;
    let mut block_end = 0;
    let mut in_fence = false;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let without_lf = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = without_lf.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            in_fence = !in_fence;
        }
        let blank = without_lf.trim().is_empty();
        if !blank || in_fence {
            if block_start.is_none() {
                block_start = Some(offset);
            }
            block_end = offset + line.len();
        } else if let Some(start) = block_start.take() {
            blocks.push((start, trim_block_end(body, start, block_end)));
        }
        offset += line.len();
    }
    if let Some(start) = block_start {
        blocks.push((start, trim_block_end(body, start, block_end)));
    }
    let mut segments = Vec::with_capacity(blocks.len());
    let mut current_heading: Option<String> = None;
    for (index, (start, end)) in blocks.into_iter().enumerate() {
        if end <= start {
            continue;
        }
        let bytes = body.as_bytes().get(start..end).ok_or_else(|| RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "segment boundaries are outside canonical body".to_owned(),
        })?;
        if bytes.len() > MAX_MARKDOWN_SOURCE_SEGMENT_BYTES {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "Markdown Source Segment exceeds the 256 KiB cap",
            ));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "canonical body segment is not UTF-8".to_owned(),
        })?;
        let first_line = text.lines().next().unwrap_or_default().trim_start();
        let section_heading = if let Some(heading) = parse_section_heading(first_line) {
            current_heading = Some(heading.clone());
            Some(heading)
        } else {
            current_heading.clone()
        };
        let segment_hash = sha256_content_hash(bytes);
        let segment_id = MarkdownSourceSegmentId::from_value(format!(
            "markdown-source-segment-{}-{}",
            document_id.as_str(),
            index + 1
        ))?;
        segments.push(MarkdownSourceSegment {
            markdown_source_segment_id: segment_id,
            markdown_source_segment_section_heading: section_heading,
            markdown_source_segment_start_byte_offset_in_document: start as u64,
            markdown_source_segment_end_byte_offset_in_document: end as u64,
            markdown_source_segment_hash: segment_hash,
            canonical_markdown_source_segment_text: text.to_owned(),
        });
    }
    if segments.is_empty() {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "canonical body has no readable segments",
        ));
    }
    Ok(segments)
}

fn trim_block_end(body: &str, start: usize, mut end: usize) -> usize {
    while end > start && body.as_bytes().get(end - 1) == Some(&b'\n') {
        end -= 1;
    }
    end
}

fn parse_section_heading(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut level = 0;
    while level < bytes.len() && bytes[level] == b'#' {
        level += 1;
    }
    if (2..=6).contains(&level) && bytes.get(level) == Some(&b' ') {
        let heading = line[level + 1..].trim();
        if !heading.is_empty() {
            return Some(heading.to_owned());
        }
    }
    None
}

fn validate_navigation(
    inputs: Vec<MarkdownCorpusNavigationNodeInput>,
    document_ids: &BTreeSet<String>,
    root_id: &MarkdownCorpusNavigationNodeId,
) -> Result<Vec<MarkdownCorpusNavigationNode>> {
    if inputs.is_empty() || inputs.len() > MAX_NAVIGATION_NODES {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "navigation must contain 1..=50000 nodes",
        ));
    }
    let mut nodes = BTreeMap::new();
    let mut edge_count = 0usize;
    for input in inputs {
        if input.markdown_corpus_navigation_node_label.len() > 512
            || input.markdown_corpus_navigation_node_summary.len() > 4 * 1024
            || input.child_markdown_corpus_navigation_node_ids.len() > MAX_NAVIGATION_LINKS_PER_NODE
            || input.linked_markdown_source_document_ids.len() > MAX_NAVIGATION_LINKS_PER_NODE
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "navigation node exceeds a size limit",
            ));
        }
        if input.markdown_corpus_navigation_node_label.trim().is_empty()
            || input.markdown_corpus_navigation_node_summary.trim().is_empty()
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "navigation label and summary must not be empty",
            ));
        }
        let mut children = input.child_markdown_corpus_navigation_node_ids;
        let mut linked_documents = input.linked_markdown_source_document_ids;
        if has_duplicate_ids(&children) || has_duplicate_ids(&linked_documents) {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "navigation edges must not contain duplicates",
            ));
        }
        edge_count =
            edge_count.saturating_add(children.len()).saturating_add(linked_documents.len());
        if edge_count > MAX_NAVIGATION_EDGES {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "navigation edge count exceeds the publication cap",
            ));
        }
        children.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        linked_documents.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let node = MarkdownCorpusNavigationNode {
            markdown_corpus_navigation_node_id: input.markdown_corpus_navigation_node_id,
            markdown_corpus_navigation_node_label: input.markdown_corpus_navigation_node_label,
            markdown_corpus_navigation_node_summary: input.markdown_corpus_navigation_node_summary,
            child_markdown_corpus_navigation_node_ids: children,
            linked_markdown_source_document_ids: linked_documents,
        };
        if nodes.insert(node.markdown_corpus_navigation_node_id.clone(), node).is_some() {
            return Err(RuntimeError::validation(
                RuntimeStage::Corpus,
                "duplicate navigation node ID",
            ));
        }
    }
    if !nodes.contains_key(root_id) {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "navigation root does not exist",
        ));
    }
    let mut indegree: BTreeMap<MarkdownCorpusNavigationNodeId, usize> =
        nodes.keys().cloned().map(|id| (id, 0)).collect();
    for node in nodes.values() {
        for child in &node.child_markdown_corpus_navigation_node_ids {
            if !nodes.contains_key(child) {
                return Err(RuntimeError::validation(
                    RuntimeStage::Corpus,
                    "navigation child does not exist",
                ));
            }
            *indegree.get_mut(child).ok_or_else(|| RuntimeError::Internal {
                message: "navigation indegree map is incomplete".to_owned(),
            })? += 1;
        }
        for document_id in &node.linked_markdown_source_document_ids {
            if !document_ids.contains(document_id.as_str()) {
                return Err(RuntimeError::validation(
                    RuntimeStage::Corpus,
                    "navigation links an unknown source document",
                ));
            }
        }
    }
    if indegree.get(root_id).copied().unwrap_or_default() != 0 {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "navigation root must have zero incoming edges",
        ));
    }
    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::from([root_id.clone()]);
    while let Some(id) = queue.pop_front() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        for child in &nodes[&id].child_markdown_corpus_navigation_node_ids {
            queue.push_back(child.clone());
        }
    }
    if reachable.len() != nodes.len() {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "navigation contains an unreachable orphan node",
        ));
    }
    // Kahn's algorithm rejects cycles without recursive stack growth.
    let mut pending = indegree;
    let mut ready: VecDeque<_> =
        pending.iter().filter_map(|(id, degree)| (*degree == 0).then_some(id.clone())).collect();
    let mut processed = 0;
    while let Some(id) = ready.pop_front() {
        processed += 1;
        for child in &nodes[&id].child_markdown_corpus_navigation_node_ids {
            let degree = pending.get_mut(child).ok_or_else(|| RuntimeError::Internal {
                message: "navigation cycle map is incomplete".to_owned(),
            })?;
            *degree -= 1;
            if *degree == 0 {
                ready.push_back(child.clone());
            }
        }
    }
    if processed != nodes.len() {
        return Err(RuntimeError::validation(
            RuntimeStage::Corpus,
            "navigation graph contains a cycle",
        ));
    }
    Ok(nodes.into_values().collect())
}

fn has_duplicate_ids<T: AsRef<str>>(values: &[T]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().any(|value| !seen.insert(value.as_ref().to_owned()))
}

fn persist_markdown_corpus_snapshot(
    storage: &Storage,
    snapshot: &MarkdownCorpusSnapshot,
) -> Result<()> {
    validate_snapshot_integrity(snapshot)?;
    let snapshot_json =
        String::from_utf8(canonical_json_bytes(snapshot)?).map_err(|_| RuntimeError::Internal {
            message: "canonical snapshot serialization was not UTF-8".to_owned(),
        })?;
    storage.with_connection(|connection| {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for version in &snapshot.markdown_source_document_versions {
            let version_json = String::from_utf8(canonical_json_bytes(version)?).map_err(|_| {
                RuntimeError::Internal {
                    message: "canonical document version serialization was not UTF-8".to_owned(),
                }
            })?;
            let existing: Option<(String, String)> = transaction
                .query_row(
                    "SELECT markdown_source_document_version_content_hash, version_json
                     FROM markdown_source_document_versions
                     WHERE owner_subject_id = ?1
                       AND markdown_source_document_version_id = ?2",
                    params![
                        snapshot.owner_subject_id.as_str(),
                        version.markdown_source_document_version_id.as_str(),
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((content_hash, existing_json)) = existing {
                if content_hash != version.markdown_source_document_version_content_hash
                    || existing_json != version_json
                {
                    return Err(RuntimeError::Conflict {
                        stage: RuntimeStage::Corpus,
                        message: "document version ID conflicts with different canonical content"
                            .to_owned(),
                    });
                }
            } else {
                transaction.execute(
                    "INSERT INTO markdown_source_document_versions (
                        owner_subject_id, markdown_source_document_id,
                        markdown_source_document_version_id,
                        markdown_source_document_version_content_hash, version_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        snapshot.owner_subject_id.as_str(),
                        version.markdown_source_document_id.as_str(),
                        version.markdown_source_document_version_id.as_str(),
                        version.markdown_source_document_version_content_hash.as_str(),
                        version_json,
                    ],
                )?;
            }
        }
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT markdown_corpus_snapshot_hash, snapshot_json
                 FROM markdown_corpus_snapshots
                 WHERE owner_subject_id = ?1 AND markdown_corpus_snapshot_id = ?2",
                params![
                    snapshot.owner_subject_id.as_str(),
                    snapshot.markdown_corpus_snapshot_id.as_str(),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((content_hash, existing_json)) = existing {
            if content_hash != snapshot.markdown_corpus_snapshot_hash
                || existing_json != snapshot_json
            {
                return Err(RuntimeError::Conflict {
                    stage: RuntimeStage::Corpus,
                    message: "snapshot ID conflicts with different canonical content".to_owned(),
                });
            }
            transaction.commit()?;
            return Ok(());
        }
        transaction.execute(
            "INSERT INTO markdown_corpus_snapshots (
                owner_subject_id, markdown_corpus_snapshot_id,
                markdown_corpus_snapshot_hash, root_markdown_corpus_navigation_node_id,
                markdown_corpus_snapshot_published_at, snapshot_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                snapshot.owner_subject_id.as_str(),
                snapshot.markdown_corpus_snapshot_id.as_str(),
                snapshot.markdown_corpus_snapshot_hash.as_str(),
                snapshot.root_markdown_corpus_navigation_node_id.as_str(),
                snapshot.markdown_corpus_snapshot_published_at.to_rfc3339(),
                snapshot_json,
            ],
        )?;
        for version in &snapshot.markdown_source_document_versions {
            transaction.execute(
                "INSERT INTO markdown_corpus_snapshot_document_versions (
                    owner_subject_id, markdown_corpus_snapshot_id,
                    markdown_source_document_id, markdown_source_document_version_id
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    snapshot.owner_subject_id.as_str(),
                    snapshot.markdown_corpus_snapshot_id.as_str(),
                    version.markdown_source_document_id.as_str(),
                    version.markdown_source_document_version_id.as_str(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    })
}

fn load_markdown_corpus_snapshot(
    storage: &Storage,
    owner_subject_id: &SubjectId,
    snapshot_id: &MarkdownCorpusSnapshotId,
) -> Result<MarkdownCorpusSnapshot> {
    let stored = storage.with_connection(|connection| {
        connection
            .query_row(
                "SELECT markdown_corpus_snapshot_hash, snapshot_json
                 FROM markdown_corpus_snapshots
                 WHERE owner_subject_id = ?1 AND markdown_corpus_snapshot_id = ?2",
                params![owner_subject_id.as_str(), snapshot_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(Into::into)
    })?;
    let (stored_snapshot_hash, snapshot_json) =
        stored.ok_or(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Corpus })?;
    let snapshot: MarkdownCorpusSnapshot =
        serde_json::from_str(&snapshot_json).map_err(|error| RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: format!("stored Markdown Corpus Snapshot is invalid JSON: {error}"),
        })?;
    if &snapshot.owner_subject_id != owner_subject_id
        || &snapshot.markdown_corpus_snapshot_id != snapshot_id
        || snapshot.markdown_corpus_snapshot_hash != stored_snapshot_hash
    {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "stored Markdown Corpus Snapshot ownership, identity, or hash mismatch"
                .to_owned(),
        });
    }
    validate_snapshot_integrity(&snapshot)?;
    Ok(snapshot)
}

fn validate_snapshot_integrity(snapshot: &MarkdownCorpusSnapshot) -> Result<()> {
    if snapshot.markdown_corpus_navigation_schema_version
        != MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION
        || snapshot.markdown_corpus_snapshot_hash_schema_version
            != MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION
    {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "unsupported Markdown Corpus Snapshot schema version".to_owned(),
        });
    }
    let mut document_ids = BTreeSet::new();
    for version in &snapshot.markdown_source_document_versions {
        validate_document_version_integrity(version)?;
        if !document_ids.insert(version.markdown_source_document_id.as_str().to_owned()) {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Corpus,
                message: "stored snapshot contains duplicate document IDs".to_owned(),
            });
        }
    }
    let navigation_inputs: Vec<_> = snapshot
        .markdown_corpus_navigation_nodes
        .iter()
        .map(|node| MarkdownCorpusNavigationNodeInput {
            markdown_corpus_navigation_node_id: node.markdown_corpus_navigation_node_id.clone(),
            markdown_corpus_navigation_node_label: node
                .markdown_corpus_navigation_node_label
                .clone(),
            markdown_corpus_navigation_node_summary: node
                .markdown_corpus_navigation_node_summary
                .clone(),
            child_markdown_corpus_navigation_node_ids: node
                .child_markdown_corpus_navigation_node_ids
                .clone(),
            linked_markdown_source_document_ids: node.linked_markdown_source_document_ids.clone(),
        })
        .collect();
    let validated_nodes = validate_navigation(
        navigation_inputs,
        &document_ids,
        &snapshot.root_markdown_corpus_navigation_node_id,
    )?;
    let hash_input = SnapshotHashInput {
        root_markdown_corpus_navigation_node_id: &snapshot.root_markdown_corpus_navigation_node_id,
        markdown_source_document_versions: &snapshot.markdown_source_document_versions,
        markdown_corpus_navigation_nodes: &validated_nodes,
        markdown_source_document_schema_version: MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION,
        markdown_parser_schema_version: MARKDOWN_PARSER_SCHEMA_VERSION,
        markdown_canonicalization_schema_version: MARKDOWN_CANONICALIZATION_SCHEMA_VERSION,
        markdown_corpus_navigation_schema_version: MARKDOWN_CORPUS_NAVIGATION_SCHEMA_VERSION,
        markdown_corpus_snapshot_hash_schema_version: MARKDOWN_CORPUS_SNAPSHOT_HASH_SCHEMA_VERSION,
    };
    let expected = canonical_content_hash(&hash_input)?;
    if expected != snapshot.markdown_corpus_snapshot_hash {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "Markdown Corpus Snapshot hash mismatch".to_owned(),
        });
    }
    Ok(())
}

fn validate_document_version_integrity(version: &MarkdownSourceDocumentVersion) -> Result<()> {
    if version.markdown_source_document_schema_version != MARKDOWN_SOURCE_DOCUMENT_SCHEMA_VERSION
        || version.markdown_parser_schema_version != MARKDOWN_PARSER_SCHEMA_VERSION
        || version.markdown_canonicalization_schema_version
            != MARKDOWN_CANONICALIZATION_SCHEMA_VERSION
    {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "unsupported Markdown Source Document Version schema".to_owned(),
        });
    }
    let hash_input = DocumentVersionHashInput {
        canonical_markdown_source: &version.canonical_markdown_source,
        markdown_source_document_id: &version.markdown_source_document_id,
        markdown_source_document_schema_version: version.markdown_source_document_schema_version,
        markdown_parser_schema_version: version.markdown_parser_schema_version,
        markdown_canonicalization_schema_version: version.markdown_canonicalization_schema_version,
    };
    if canonical_content_hash(&hash_input)? != version.markdown_source_document_version_content_hash
    {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "Markdown Source Document Version hash mismatch".to_owned(),
        });
    }
    let body = version.canonical_markdown_document_body.as_bytes();
    let mut segment_ids = BTreeSet::new();
    let mut previous_end = 0usize;
    for segment in &version.markdown_source_segments {
        if !segment_ids.insert(segment.markdown_source_segment_id.as_str().to_owned()) {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Corpus,
                message: "stored Markdown Source Segment IDs are duplicated".to_owned(),
            });
        }
        let start = usize::try_from(segment.markdown_source_segment_start_byte_offset_in_document)
            .map_err(|_| RuntimeError::CorruptState {
                stage: RuntimeStage::Corpus,
                message: "Markdown Source Segment start offset is out of range".to_owned(),
            })?;
        let end = usize::try_from(segment.markdown_source_segment_end_byte_offset_in_document)
            .map_err(|_| RuntimeError::CorruptState {
                stage: RuntimeStage::Corpus,
                message: "Markdown Source Segment end offset is out of range".to_owned(),
            })?;
        if start >= end
            || start < previous_end
            || end > body.len()
            || !version.canonical_markdown_document_body.is_char_boundary(start)
            || !version.canonical_markdown_document_body.is_char_boundary(end)
            || end - start > MAX_MARKDOWN_SOURCE_SEGMENT_BYTES
        {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Corpus,
                message: "Markdown Source Segment offset or ordering is invalid".to_owned(),
            });
        }
        previous_end = end;
        let bytes = body.get(start..end).ok_or_else(|| RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "Markdown Source Segment offset is invalid".to_owned(),
        })?;
        if sha256_content_hash(bytes) != segment.markdown_source_segment_hash
            || bytes != segment.canonical_markdown_source_segment_text.as_bytes()
        {
            return Err(RuntimeError::CorruptState {
                stage: RuntimeStage::Corpus,
                message: "Markdown Source Segment hash or text mismatch".to_owned(),
            });
        }
    }
    if version.markdown_source_segments.is_empty() {
        return Err(RuntimeError::CorruptState {
            stage: RuntimeStage::Corpus,
            message: "stored Markdown Source Document Version has no segments".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(path: &str, id: &str, body: &str) -> MarkdownSourceDocumentInput {
        MarkdownSourceDocumentInput {
            relative_path: path.to_owned(),
            markdown_source_bytes: format!(
                "---\nmarkdown_source_document_id: {id}\n---\n\n# 标题 {id}\n\n摘要 {id}\n\n{body}"
            )
            .into_bytes(),
        }
    }

    fn navigation(document_id: &str) -> MarkdownCorpusNavigationNodeInput {
        MarkdownCorpusNavigationNodeInput {
            markdown_corpus_navigation_node_id: MarkdownCorpusNavigationNodeId::from_value("root")
                .unwrap(),
            markdown_corpus_navigation_node_label: "Root".to_owned(),
            markdown_corpus_navigation_node_summary: "Summary".to_owned(),
            child_markdown_corpus_navigation_node_ids: Vec::new(),
            linked_markdown_source_document_ids: vec![
                MarkdownSourceDocumentId::from_value(document_id).unwrap(),
            ],
        }
    }

    #[test]
    fn parses_front_matter_metadata_and_multibyte_offsets() {
        let parsed = parse_markdown_source_document(&document(
            "a/doc.md",
            "doc-1",
            "## 研究\n\n上海规则与工资\n\n```md\n## not heading\n\n内容\n```\n",
        ))
        .unwrap();
        assert_eq!(parsed.markdown_source_document_title, "标题 doc-1");
        assert_eq!(parsed.markdown_source_segments.len(), 3);
        let segment = &parsed.markdown_source_segments[2];
        let bytes = parsed.canonical_markdown_document_body.as_bytes();
        assert_eq!(
            &bytes[segment.byte_range()],
            segment.canonical_markdown_source_segment_text.as_bytes()
        );
        assert!(
            segment.markdown_source_segment_end_byte_offset_in_document
                > segment.markdown_source_segment_start_byte_offset_in_document
        );
    }

    #[test]
    fn rejects_invalid_paths_encoding_and_duplicate_front_matter() {
        let mut invalid = document("../doc.md", "doc-1", "body");
        assert!(parse_markdown_source_document(&invalid).is_err());
        invalid.relative_path = "doc.md".to_owned();
        invalid.markdown_source_bytes = b"---\nmarkdown_source_document_id: one\nmarkdown_source_document_id: two\n---\n# t\n\na\n\nb\n".to_vec();
        assert!(parse_markdown_source_document(&invalid).is_err());
        invalid.markdown_source_bytes = vec![0xff, 0xfe];
        assert!(parse_markdown_source_document(&invalid).is_err());
    }

    #[test]
    fn validates_snapshot_navigation_dag_and_hash() {
        let input =
            PublishMarkdownCorpusSnapshotInput {
                markdown_source_documents: vec![document("a.md", "doc-1", "body")],
                markdown_corpus_navigation_nodes: vec![navigation("doc-1")],
                root_markdown_corpus_navigation_node_id:
                    MarkdownCorpusNavigationNodeId::from_value("root").unwrap(),
            };
        let snapshot = build_markdown_corpus_snapshot(
            SubjectId::from_value("subject-1").unwrap(),
            input,
            Utc::now(),
        )
        .unwrap();
        assert!(snapshot.markdown_corpus_snapshot_hash.starts_with("sha256:"));
        let reader = snapshot.reader();
        let docs = reader
            .list_branch_document_abstracts(
                &MarkdownCorpusNavigationNodeId::from_value("root").unwrap(),
            )
            .unwrap();
        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn rejects_navigation_cycle_and_orphan() {
        let a = MarkdownCorpusNavigationNodeId::from_value("a").unwrap();
        let b = MarkdownCorpusNavigationNodeId::from_value("b").unwrap();
        let input =
            PublishMarkdownCorpusSnapshotInput {
                markdown_source_documents: vec![document("a.md", "doc-1", "body")],
                markdown_corpus_navigation_nodes: vec![
                    MarkdownCorpusNavigationNodeInput {
                        markdown_corpus_navigation_node_id: a.clone(),
                        markdown_corpus_navigation_node_label: "A".to_owned(),
                        markdown_corpus_navigation_node_summary: "A".to_owned(),
                        child_markdown_corpus_navigation_node_ids: vec![b.clone()],
                        linked_markdown_source_document_ids: Vec::new(),
                    },
                    MarkdownCorpusNavigationNodeInput {
                        markdown_corpus_navigation_node_id: b.clone(),
                        markdown_corpus_navigation_node_label: "B".to_owned(),
                        markdown_corpus_navigation_node_summary: "B".to_owned(),
                        child_markdown_corpus_navigation_node_ids: vec![a],
                        linked_markdown_source_document_ids: Vec::new(),
                    },
                ],
                root_markdown_corpus_navigation_node_id:
                    MarkdownCorpusNavigationNodeId::from_value("a").unwrap(),
            };
        assert!(
            build_markdown_corpus_snapshot(
                SubjectId::from_value("subject-1").unwrap(),
                input,
                Utc::now(),
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn persists_reopens_and_authorizes_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("runtime.sqlite");
        let principal = ResearchPrincipal::new(
            SubjectId::from_value("subject-1").unwrap(),
            [PrincipalCapability::PublishMarkdownCorpusSnapshot],
        );
        let corpus = VersionedMarkdownCorpus::open(&database).unwrap();
        let snapshot = corpus
            .publish_markdown_corpus_snapshot(
                &principal,
                PublishMarkdownCorpusSnapshotInput {
                    markdown_source_documents: vec![document("a.md", "doc-1", "body")],
                    markdown_corpus_navigation_nodes: vec![navigation("doc-1")],
                    root_markdown_corpus_navigation_node_id:
                        MarkdownCorpusNavigationNodeId::from_value("root").unwrap(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        drop(corpus);

        let reopened = VersionedMarkdownCorpus::open(&database).unwrap();
        let loaded = reopened
            .open_markdown_corpus_snapshot(&principal, &snapshot.markdown_corpus_snapshot_id)
            .await
            .unwrap();
        assert_eq!(loaded.markdown_corpus_snapshot_hash, snapshot.markdown_corpus_snapshot_hash);

        let other = ResearchPrincipal::new(
            SubjectId::from_value("subject-2").unwrap(),
            [PrincipalCapability::PublishMarkdownCorpusSnapshot],
        );
        assert!(matches!(
            reopened
                .open_markdown_corpus_snapshot(&other, &snapshot.markdown_corpus_snapshot_id)
                .await,
            Err(RuntimeError::ObjectNotAvailable { .. })
        ));
    }
}

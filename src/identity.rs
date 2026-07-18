//! Opaque identifiers, principals, clocks and ID generation.

use crate::error::{Result, RuntimeError, RuntimeStage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use uuid::Uuid;

const MAX_IDENTIFIER_BYTES: usize = 128;

/// A validated opaque identifier. The marker prevents accidental mixing of
/// identifiers belonging to different domain objects.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueId<K>(String, #[serde(skip)] PhantomData<K>);

impl<K> OpaqueId<K> {
    /// Creates an identifier after applying the runtime's path-safe grammar.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
            return Err(RuntimeError::validation(
                RuntimeStage::Setup,
                "identifier must be 1..=128 bytes",
            ));
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return Err(RuntimeError::validation(
                RuntimeStage::Setup,
                "identifier contains an unsupported character",
            ));
        }
        Ok(Self(value, PhantomData))
    }

    /// Returns the opaque string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the string representation.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl<K> Display for OpaqueId<K> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<K> AsRef<str> for OpaqueId<K> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

macro_rules! define_id {
    ($name:ident, $marker:ident, $prefix:literal) => {
        #[doc(hidden)]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        pub struct $marker;

        /// A typed opaque identifier.
        pub type $name = OpaqueId<$marker>;

        impl OpaqueId<$marker> {
            /// Creates an identifier from an existing value.
            pub fn from_value(value: impl Into<String>) -> Result<Self> {
                Self::new(value)
            }

            /// Generates a new runtime-owned identifier.
            pub fn generate() -> Self {
                let uuid = Uuid::now_v7();
                Self(format!(concat!($prefix, "-{}"), uuid), PhantomData)
            }
        }
    };
}

define_id!(SubjectId, SubjectIdKind, "subject");
define_id!(CommandId, CommandIdKind, "command");
define_id!(DocumentResearchConversationId, DocumentResearchConversationIdKind, "conversation");
define_id!(DocumentResearchRequestId, DocumentResearchRequestIdKind, "request");
define_id!(MarkdownCorpusSnapshotId, MarkdownCorpusSnapshotIdKind, "markdown-corpus-snapshot");
define_id!(
    MarkdownCorpusNavigationCandidateSetId,
    MarkdownCorpusNavigationCandidateSetIdKind,
    "markdown-corpus-navigation-candidate-set"
);
define_id!(
    MarkdownCorpusNavigationNodeId,
    MarkdownCorpusNavigationNodeIdKind,
    "markdown-corpus-navigation-node"
);
define_id!(MarkdownSourceDocumentId, MarkdownSourceDocumentIdKind, "markdown-source-document");
define_id!(
    MarkdownSourceDocumentVersionId,
    MarkdownSourceDocumentVersionIdKind,
    "markdown-source-document-version"
);
define_id!(MarkdownSourceSegmentId, MarkdownSourceSegmentIdKind, "markdown-source-segment");
define_id!(
    ResearchDocumentReadRequestId,
    ResearchDocumentReadRequestIdKind,
    "research-document-read-request"
);
define_id!(
    VerbatimSourceEvidenceExtractionRequestId,
    VerbatimSourceEvidenceExtractionRequestIdKind,
    "verbatim-source-evidence-extraction-request"
);
define_id!(
    MarkdownResearchExecutionId,
    MarkdownResearchExecutionIdKind,
    "markdown-research-execution"
);
define_id!(
    DocumentResearchBranchTaskId,
    DocumentResearchBranchTaskIdKind,
    "document-research-branch-task"
);
define_id!(
    MarkdownResearchModelTaskId,
    MarkdownResearchModelTaskIdKind,
    "markdown-research-model-task"
);
define_id!(VerbatimSourceEvidenceId, VerbatimSourceEvidenceIdKind, "verbatim-source-evidence");
define_id!(
    EvidenceLinkedResearchClaimId,
    EvidenceLinkedResearchClaimIdKind,
    "evidence-linked-research-claim"
);
define_id!(ResearchCoverageGapId, ResearchCoverageGapIdKind, "research-coverage-gap");
define_id!(PublicSourceCitationId, PublicSourceCitationIdKind, "public-source-citation");

/// Capabilities supplied by the host authentication layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalCapability {
    /// Allows publication of Markdown Corpus snapshots.
    PublishMarkdownCorpusSnapshot,
    /// Allows creation and execution of research requests.
    ExecuteMarkdownResearch,
}

/// The authenticated subject on whose behalf a command runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchPrincipal {
    /// Stable subject identifier supplied by the host.
    pub subject_id: SubjectId,
    /// Capabilities granted by the host.
    pub capabilities: Vec<PrincipalCapability>,
}

impl ResearchPrincipal {
    /// Creates a principal with the requested capabilities.
    pub fn new(
        subject_id: SubjectId,
        capabilities: impl IntoIterator<Item = PrincipalCapability>,
    ) -> Self {
        let mut capabilities: Vec<_> = capabilities.into_iter().collect();
        capabilities.sort_by_key(|capability| *capability as u8);
        capabilities.dedup();
        Self { subject_id, capabilities }
    }

    /// Returns whether this principal has a capability.
    #[must_use]
    pub fn can(&self, capability: PrincipalCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// Requires a capability without revealing authorization details.
    pub fn require(&self, capability: PrincipalCapability) -> Result<()> {
        if self.can(capability) {
            Ok(())
        } else {
            Err(RuntimeError::ObjectNotAvailable { stage: RuntimeStage::Setup })
        }
    }
}

/// Injectable wall clock used for deterministic event tests.
#[allow(dead_code)]
pub trait Clock: Send + Sync {
    /// Returns the current UTC timestamp.
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock backed by `Utc::now`.
#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)]
pub struct SystemClock;

#[allow(dead_code)]
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Injectable opaque ID generator.
#[allow(dead_code)]
pub trait IdGenerator: Send + Sync {
    /// Generates an ID with the given domain prefix.
    fn generate(&self, prefix: &str) -> String;
}

/// Production ID generator backed by UUID v7.
#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)]
pub struct UuidV7IdGenerator;

#[allow(dead_code)]
impl IdGenerator for UuidV7IdGenerator {
    fn generate(&self, prefix: &str) -> String {
        format!("{prefix}-{}", Uuid::now_v7())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    #[test]
    fn identifiers_reject_paths_and_accept_domain_values() {
        let accepted = SubjectId::from_value("subject-1").unwrap();
        assert_eq!(accepted.as_str(), "subject-1");
        assert!(SubjectId::from_value("../secret").is_err());
    }

    #[test]
    fn generated_ids_have_stable_prefixes() {
        assert!(
            MarkdownCorpusSnapshotId::generate().as_str().starts_with("markdown-corpus-snapshot-")
        );
    }

    #[test]
    fn principal_capabilities_are_deduplicated() {
        let principal = ResearchPrincipal::new(
            SubjectId::from_value("subject-1").unwrap(),
            [
                PrincipalCapability::ExecuteMarkdownResearch,
                PrincipalCapability::ExecuteMarkdownResearch,
            ],
        );
        assert_eq!(principal.capabilities.len(), 1);
        assert!(principal.can(PrincipalCapability::ExecuteMarkdownResearch));
    }

    #[test]
    fn clock_seam_is_callable() {
        let timestamp =
            DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(FixedClock(timestamp).now(), timestamp);
    }
}

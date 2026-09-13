//! Stable update failures that never echo fetched manifest bytes.

use thiserror::Error;

use crate::{ArtifactDigest, ReleaseVersion, SignatureSubject, SignatureVerificationFault};

/// Why an installed or staged artifact did not match what was pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactMismatch {
    /// Staged bytes do not hash to the digest the manifest pinned.
    ///
    /// The substitution case: something served different bytes under a
    /// version and platform that is already trusted.
    Digest {
        /// Digest the manifest requires.
        pinned: ArtifactDigest,
        /// Digest recomputed over the exact staged bytes.
        staged: ArtifactDigest,
    },
    /// A retained version's bytes no longer hash to what was recorded.
    Retained {
        /// Version whose retained copy failed re-verification.
        version: ReleaseVersion,
        /// Digest recorded when the version was installed.
        recorded: ArtifactDigest,
        /// Digest recomputed from the retained copy.
        found: ArtifactDigest,
    },
}

impl std::fmt::Display for ArtifactMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Digest { pinned, staged } => write!(
                formatter,
                "staged artifact {staged} does not match pinned {pinned}"
            ),
            Self::Retained {
                version,
                recorded,
                found,
            } => write!(
                formatter,
                "retained version `{version}` is {found}, recorded as {recorded}"
            ),
        }
    }
}

/// Failures from manifest admission and the install/rollback transitions.
///
/// A release manifest is remote data. No variant carries a file name, a
/// signature, or any other value read out of the fetched document: a malformed
/// row is reported by its index plus a compile-time field path and reason.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UpdateError {
    /// One named field violated a stable rule.
    #[error("release field `{field}` is invalid: {reason}")]
    InvalidField {
        /// Compile-time field path; never attacker-controlled text.
        field: &'static str,
        /// Compile-time safe explanation.
        reason: &'static str,
    },
    /// One artifact row was invalid, which rejects the whole manifest.
    #[error("release manifest artifact {artifact} field `{field}` is invalid: {reason}")]
    InvalidArtifact {
        /// Zero-based row position; derived, never fetched text.
        artifact: usize,
        /// Compile-time field path; never attacker-controlled text.
        field: &'static str,
        /// Compile-time safe explanation.
        reason: &'static str,
    },
    /// Manifest bytes exceeded the fixed admission budget.
    #[error("release manifest exceeds the {limit}-byte admission limit")]
    ManifestTooLarge {
        /// Exact byte budget.
        limit: usize,
    },
    /// Detached signature bundle was empty or exceeded its fixed budget.
    #[error("release signature bundle is empty or exceeds the {limit}-byte admission limit")]
    SignatureBundleSize {
        /// Exact bundle byte budget.
        limit: usize,
    },
    /// A detached bundle was not the exact bundle pinned by the signed manifest.
    #[error("release signature bundle digest {found} does not match pinned {pinned}")]
    SignatureBundleDigestMismatch {
        /// Bundle digest pinned inside the authenticated manifest.
        pinned: ArtifactDigest,
        /// Digest recomputed over the supplied bundle.
        found: ArtifactDigest,
    },
    /// Manifest or artifact signature verification failed without raw output.
    #[error("release {subject} signature verification is {fault}")]
    SignatureVerification {
        /// Exact subject class.
        subject: SignatureSubject,
        /// Closed body-free verifier result.
        fault: SignatureVerificationFault,
    },
    /// Manifest bytes did not hash to the digest the caller pinned.
    ///
    /// Raised before the document is decoded, so an unpinned manifest is never
    /// parsed at all.
    #[error("release manifest digest {found} does not match pinned {pinned}")]
    ManifestDigestMismatch {
        /// Digest the caller pinned.
        pinned: ArtifactDigest,
        /// Digest recomputed over the exact fetched bytes.
        found: ArtifactDigest,
    },
    /// Manifest bytes are not UTF-8, or not the expected JSON shape.
    #[error("release manifest is malformed or contains unknown fields")]
    InvalidDocument,
    /// The document uses an unsupported manifest schema.
    #[error("unsupported release manifest schema {found}; supported schema is {supported}")]
    UnsupportedManifestSchema {
        /// Version found in the document.
        found: u32,
        /// Exact schema understood by this implementation.
        supported: u32,
    },
    /// The manifest offers no artifact for this platform.
    #[error("release `{version}` offers no artifact for platform `{platform}`")]
    NoArtifactForPlatform {
        /// Release version the manifest describes.
        version: ReleaseVersion,
        /// Platform token that found no row.
        platform: String,
    },
    /// Artifact bytes exceeded the fixed install budget.
    #[error("release artifact exceeds the {limit}-byte admission limit")]
    ArtifactTooLarge {
        /// Exact artifact byte budget.
        limit: usize,
    },
    /// Staged or retained bytes did not match their pin.
    #[error("{0}")]
    Mismatch(Box<ArtifactMismatch>),
    /// The install root is not a directory this crate wrote.
    #[error("install root is not a valid heycode installation")]
    InvalidRoot,
    /// The durable install record is malformed.
    #[error("install record is malformed")]
    CorruptRecord,
    /// This version is already the installed current version.
    #[error("release `{version}` is already installed")]
    AlreadyCurrent {
        /// The already-current version.
        version: ReleaseVersion,
    },
    /// An existing installation received no policy approval bound to its current version.
    #[error("release `{candidate}` is not approved to replace current `{current}`")]
    UpdatePolicyRequired {
        /// Durable current version the approval must name.
        current: ReleaseVersion,
        /// Candidate release version.
        candidate: ReleaseVersion,
    },
    /// A fixed filesystem operation failed. Raw paths and OS text are omitted.
    #[error("install filesystem operation `{operation}` failed")]
    Io {
        /// Compile-time operation label.
        operation: &'static str,
    },
}

pub(crate) const fn invalid_field(field: &'static str, reason: &'static str) -> UpdateError {
    UpdateError::InvalidField { field, reason }
}

pub(crate) const fn invalid_artifact(
    artifact: usize,
    field: &'static str,
    reason: &'static str,
) -> UpdateError {
    UpdateError::InvalidArtifact {
        artifact,
        field,
        reason,
    }
}

pub(crate) fn mismatch(mismatch: ArtifactMismatch) -> UpdateError {
    UpdateError::Mismatch(Box::new(mismatch))
}

pub(crate) const fn io(operation: &'static str) -> UpdateError {
    UpdateError::Io { operation }
}

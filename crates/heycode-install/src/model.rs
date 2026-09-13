//! Validated release-artifact vocabulary.

use std::cmp::Ordering;
use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::UpdateError;
use crate::error::invalid_field;

/// SHA-256 identity of one exact byte sequence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ArtifactDigest(String);

impl ArtifactDigest {
    /// Validate an algorithm-qualified lowercase digest.
    ///
    /// # Errors
    /// Anything but `sha256:` followed by exactly 64 lowercase hexadecimal
    /// digits is rejected.
    pub fn parse(value: &str) -> Result<Self, UpdateError> {
        let valid = value.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
        if !valid {
            return Err(invalid_field(
                "digest",
                "must be sha256 followed by 64 lowercase hexadecimal digits",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Digest of exact bytes.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let mut rendered = String::with_capacity(71);
        rendered.push_str("sha256:");
        for byte in hasher.finalize() {
            use std::fmt::Write as _;
            let _ = write!(rendered, "{byte:02x}");
        }
        Self(rendered)
    }

    /// Algorithm-qualified lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A released heycode version, as an identity.
///
/// Q15 gives this identity strict Semantic-Versioning precedence. Install and
/// rollback still use recorded machine history; only release-channel policy
/// uses the ordering to decide automatic movement.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ReleaseVersion(String);

impl ReleaseVersion {
    /// Validate a release version.
    ///
    /// # Errors
    /// Requires `MAJOR.MINOR.PATCH` with no leading zeroes, optionally
    /// followed by `-` and a bounded alphanumeric pre-release, within 64
    /// bytes. The text must also be a portable single path component, because
    /// it names a retained directory.
    pub fn parse(value: impl Into<String>) -> Result<Self, UpdateError> {
        let value = value.into();
        if !valid_release_version(&value) {
            return Err(invalid_field(
                "version",
                "must be major.minor.patch with an optional bounded pre-release",
            ));
        }
        Ok(Self(value))
    }

    /// Stable version text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this version has a prerelease component.
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        self.0.contains('-')
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Ord for ReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_release_versions(self.as_str(), other.as_str())
    }
}

impl PartialOrd for ReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn valid_release_version(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    let (core, prerelease) = match value.split_once('-') {
        Some((core, rest)) => (core, Some(rest)),
        None => (value, None),
    };
    let mut parts = core.split('.');
    let numeric = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0'))
    };
    if !matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(major), Some(minor), Some(patch), None)
            if numeric(major) && numeric(minor) && numeric(patch)
    ) {
        return false;
    }
    prerelease.is_none_or(|rest| {
        !rest.is_empty()
            && rest.len() <= 32
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            && !rest.split('.').any(|identifier| {
                identifier.is_empty()
                    || (identifier.bytes().all(|byte| byte.is_ascii_digit())
                        && identifier.len() > 1
                        && identifier.starts_with('0'))
            })
    })
}

fn compare_release_versions(left: &str, right: &str) -> Ordering {
    let (left_core, left_pre) = left
        .split_once('-')
        .map_or((left, None), |(core, pre)| (core, Some(pre)));
    let (right_core, right_pre) = right
        .split_once('-')
        .map_or((right, None), |(core, pre)| (core, Some(pre)));
    for (left_part, right_part) in left_core.split('.').zip(right_core.split('.')) {
        let ordering = compare_numeric_identifier(left_part, right_part);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    match (left_pre, right_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => compare_prerelease(left, right),
    }
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => {
                let left_numeric = left.bytes().all(|byte| byte.is_ascii_digit());
                let right_numeric = right.bytes().all(|byte| byte.is_ascii_digit());
                let ordering = match (left_numeric, right_numeric) {
                    (true, true) => compare_numeric_identifier(left, right),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => left.cmp(right),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

fn compare_numeric_identifier(left: &str, right: &str) -> Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

/// The host an artifact is built for, as an opaque matched token.
///
/// This crate does not interpret operating-system or architecture semantics —
/// it selects the row whose token equals the host's. Keeping it opaque avoids
/// a third copy of a platform enum in this workspace and keeps the crate
/// honest about what it actually decides.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ReleasePlatform(String);

impl ReleasePlatform {
    /// Validate a platform token such as `macos-aarch64`.
    ///
    /// # Errors
    /// Must be lowercase alphanumeric segments joined by `-`, within 64 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, UpdateError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && !value.starts_with('-')
            && !value.ends_with('-')
            && !value.contains("--")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(invalid_field(
                "platform",
                "must be lowercase alphanumeric segments joined by hyphens",
            ));
        }
        Ok(Self(value))
    }

    /// The token for the host this binary is running on.
    ///
    /// # Errors
    /// A host whose `OS`/`ARCH` constants do not form a valid token.
    pub fn host() -> Result<Self, UpdateError> {
        Self::new(format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    }

    /// Stable platform token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReleasePlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Signature-bundle format required by a release manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureScheme {
    /// GitHub artifact-attestation Sigstore bundle with SLSA provenance.
    GithubSigstoreBundleV1,
}

/// The exact detached attestation bundle required for one artifact.
///
/// The manifest is itself verified before this record is trusted. The bundle
/// digest prevents a package mirror from pairing the artifact with a different
/// bundle before the configured verifier checks the signer identity and exact
/// subject bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactAttestation {
    pub(crate) scheme: SignatureScheme,
    pub(crate) bundle_digest: ArtifactDigest,
}

impl ArtifactAttestation {
    /// Required signature-bundle format.
    #[must_use]
    pub const fn scheme(&self) -> SignatureScheme {
        self.scheme
    }

    /// Digest of the exact detached bundle.
    #[must_use]
    pub const fn bundle_digest(&self) -> &ArtifactDigest {
        &self.bundle_digest
    }
}

/// One platform's artifact within a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseArtifact {
    pub(crate) platform: ReleasePlatform,
    pub(crate) digest: ArtifactDigest,
    pub(crate) attestation: ArtifactAttestation,
}

impl ReleaseArtifact {
    /// Host token this artifact is built for.
    #[must_use]
    pub const fn platform(&self) -> &ReleasePlatform {
        &self.platform
    }

    /// The pinned digest of the exact artifact bytes.
    #[must_use]
    pub const fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }

    /// Required detached attestation bundle.
    #[must_use]
    pub const fn attestation(&self) -> &ArtifactAttestation {
        &self.attestation
    }
}

/// One admitted release manifest generation.
///
/// Whole-generation admission: a single malformed artifact row rejects the
/// entire document, so a publisher cannot suppress a platform by corrupting
/// its row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseManifest {
    pub(crate) schema_version: u32,
    pub(crate) version: ReleaseVersion,
    pub(crate) config_schema_version: u32,
    pub(crate) plugin_api_version: u32,
    pub(crate) digest: ArtifactDigest,
    pub(crate) artifacts: Vec<ReleaseArtifact>,
}

impl ReleaseManifest {
    /// Exact manifest schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Release version this manifest describes.
    #[must_use]
    pub const fn version(&self) -> &ReleaseVersion {
        &self.version
    }

    /// Highest configuration schema this release's binary understands.
    ///
    /// Recorded at install time so a later rollback can decide, without
    /// running the older binary, whether it could read the configuration on
    /// disk. This is the fact B03's `ConfigVersionState` is compared against.
    #[must_use]
    pub const fn config_schema_version(&self) -> u32 {
        self.config_schema_version
    }

    /// Exact external plugin host API exposed by this release.
    #[must_use]
    pub const fn plugin_api_version(&self) -> u32 {
        self.plugin_api_version
    }

    /// Verified digest of the exact bytes this manifest was parsed from.
    #[must_use]
    pub const fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }

    /// Offered artifacts, ordered by platform token.
    #[must_use]
    pub fn artifacts(&self) -> &[ReleaseArtifact] {
        &self.artifacts
    }

    /// The single artifact for this exact platform.
    ///
    /// # Errors
    /// [`UpdateError::NoArtifactForPlatform`] when the release does not build
    /// for this host. There is no nearest-match behaviour.
    pub fn artifact_for(
        &self,
        platform: &ReleasePlatform,
    ) -> Result<&ReleaseArtifact, UpdateError> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.platform() == platform)
            .ok_or_else(|| UpdateError::NoArtifactForPlatform {
                version: self.version.clone(),
                platform: platform.as_str().to_owned(),
            })
    }
}

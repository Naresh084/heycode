//! Public content-addressed cache receipts and inspection snapshots.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{PackageCacheError, PluginId, PluginManifest, PluginVersion};

/// SHA-256 identity of one canonical package tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageContentHash(String);

impl PackageContentHash {
    pub(crate) fn from_digest(digest: [u8; 32]) -> Self {
        let mut rendered = String::with_capacity(71);
        rendered.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(rendered, "{byte:02x}");
        }
        Self(rendered)
    }

    /// Parse one canonical package-tree content address.
    ///
    /// # Errors
    /// Anything but `sha256:` followed by 64 lowercase hexadecimal digits is
    /// rejected as an invalid cache identity.
    pub fn parse(value: &str) -> Result<Self, PackageCacheError> {
        let valid = value.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
        if !valid {
            return Err(crate::cache_error::corrupt(
                crate::CacheCorruption::InvalidReference,
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Algorithm-qualified lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn hex(&self) -> &str {
        self.0.strip_prefix("sha256:").unwrap_or_default()
    }
}

impl std::fmt::Display for PackageContentHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether this call published a new id/version reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallDisposition {
    /// New immutable reference committed.
    Installed,
    /// The exact id/version/content was already committed and verified.
    AlreadyPresent,
}

/// Verified installed package returned to a local Consumer.
#[derive(Clone)]
pub struct InstalledPlugin {
    pub(crate) manifest: PluginManifest,
    pub(crate) content_hash: PackageContentHash,
    pub(crate) package_root: PathBuf,
    pub(crate) disposition: InstallDisposition,
}

impl InstalledPlugin {
    /// Validated installed manifest.
    #[must_use]
    pub const fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Canonical tree content address.
    #[must_use]
    pub const fn content_hash(&self) -> &PackageContentHash {
        &self.content_hash
    }

    /// Exact immutable package root for later activation Consumers.
    #[must_use]
    pub fn package_root(&self) -> &Path {
        &self.package_root
    }

    /// Whether this call published or reused the version reference.
    #[must_use]
    pub const fn disposition(&self) -> InstallDisposition {
        self.disposition
    }
}

/// Path-free deterministic row for cache inspection/UI projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachedPluginSummary {
    /// Validated plugin identity.
    pub id: PluginId,
    /// Exact semantic package version.
    pub version: PluginVersion,
    /// Verified canonical package tree hash.
    pub content_hash: PackageContentHash,
    /// Number of manifest contribution declarations.
    pub contribution_count: usize,
    /// Explicit manifest default-enable request.
    pub default_enabled: bool,
}

/// Schema-v1 deterministic cache inspection projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginCacheSnapshot {
    /// Stable snapshot schema.
    pub schema_version: u32,
    /// Rows sorted by plugin id then SemVer precedence and exact text.
    pub packages: Vec<CachedPluginSummary>,
}

/// Exact bounded cleanup outcome for abandoned transient state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CacheCleanupReport {
    /// Complete abandoned staging directories removed.
    pub staging_directories_removed: usize,
    /// Inactive abandoned lock files removed after an OS-lock probe.
    pub lock_files_removed: usize,
    /// Complete content objects with no committed reference removed.
    pub orphan_objects_removed: usize,
}

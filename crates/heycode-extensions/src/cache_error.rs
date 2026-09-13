//! Stable package-cache failures without source bytes or host paths.

use thiserror::Error;

use crate::{ManifestError, PluginId, PluginVersion};

/// Unsafe condition found while reading an untrusted package directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageSourceIssue {
    /// Root or entry is a symbolic link.
    SymbolicLink,
    /// A regular file has another hard-link name.
    HardLink,
    /// An entry is neither a directory nor a regular file.
    UnsupportedFileType,
    /// Entry name cannot be represented by the portable path vocabulary.
    NonPortablePath,
    /// Two entry names collide under portable case-insensitive lookup.
    CaseCollision,
    /// Source metadata changed while the package was read.
    ChangedDuringRead,
    /// Package directory is nested inside the cache or contains the cache.
    CacheOverlap,
}

impl std::fmt::Display for PackageSourceIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SymbolicLink => "symbolic_link",
            Self::HardLink => "hard_link",
            Self::UnsupportedFileType => "unsupported_file_type",
            Self::NonPortablePath => "non_portable_path",
            Self::CaseCollision => "case_collision",
            Self::ChangedDuringRead => "changed_during_read",
            Self::CacheOverlap => "cache_overlap",
        })
    }
}

/// Closed package size/count budget exceeded during admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageLimit {
    /// Too many filesystem entries.
    Entries,
    /// One file exceeded its individual byte cap.
    FileBytes,
    /// Aggregate file bytes exceeded the package cap.
    TotalBytes,
    /// Directory nesting exceeded the portable depth cap.
    Depth,
}

impl std::fmt::Display for PackageLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Entries => "entries",
            Self::FileBytes => "file_bytes",
            Self::TotalBytes => "total_bytes",
            Self::Depth => "depth",
        })
    }
}

/// Structural corruption classes found in the owner-only cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheCorruption {
    /// An internal path is a symlink or has the wrong file type.
    UnsafeFileType,
    /// An internal file has multiple hard-link names.
    HardLink,
    /// An internal file or directory is not owner-only.
    Permissions,
    /// A reference document is malformed.
    InvalidReference,
    /// A committed reference points to no object.
    MissingObject,
    /// Recomputed package content differs from its address.
    ContentHashMismatch,
    /// Object manifest id/version differs from its reference.
    ManifestIdentityMismatch,
    /// The reference tree contains an entry outside schema v1.
    UnexpectedEntry,
}

impl std::fmt::Display for CacheCorruption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnsafeFileType => "unsafe_file_type",
            Self::HardLink => "hard_link",
            Self::Permissions => "permissions",
            Self::InvalidReference => "invalid_reference",
            Self::MissingObject => "missing_object",
            Self::ContentHashMismatch => "content_hash_mismatch",
            Self::ManifestIdentityMismatch => "manifest_identity_mismatch",
            Self::UnexpectedEntry => "unexpected_entry",
        })
    }
}

/// Failures from the content-addressed plugin package cache.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PackageCacheError {
    /// Cache path is relative, missing a safe parent, or otherwise invalid.
    #[error("plugin cache root is invalid")]
    InvalidRoot,
    /// Durable cache layout is newer or older than this implementation.
    #[error("unsupported plugin cache schema {found}; supported schema is {supported}")]
    UnsupportedCacheSchema {
        /// Version found in the owner-only marker.
        found: u32,
        /// Exact layout understood by this implementation.
        supported: u32,
    },
    /// This host cannot enforce the required owner-only cache boundary.
    #[error("plugin cache owner-only security is unsupported on this host")]
    UnsupportedSecurity,
    /// Cache structure or permissions violated a durable invariant.
    #[error("plugin cache is corrupt: {reason}")]
    CorruptCache {
        /// Stable corruption class.
        reason: CacheCorruption,
    },
    /// Source directory contained an unsafe filesystem shape.
    #[error("plugin package source is unsafe: {issue}")]
    UnsafeSource {
        /// Stable source issue without a host path.
        issue: PackageSourceIssue,
    },
    /// Package admission exceeded a fixed resource budget.
    #[error("plugin package exceeds the `{limit}` limit")]
    PackageLimit {
        /// Exceeded budget class.
        limit: PackageLimit,
    },
    /// Required manifest path is absent or not a regular file.
    #[error("plugin package is missing `.heycode-plugin/plugin.toml`")]
    MissingManifest,
    /// Manifest bytes are not UTF-8.
    #[error("plugin package manifest is not UTF-8")]
    ManifestNotUtf8,
    /// A declared contribution/configuration path is absent from the package.
    #[error("plugin package is missing a path declared by its manifest")]
    MissingDeclaredPath,
    /// Strict PL01 manifest validation failed.
    #[error("plugin package manifest is invalid: {0}")]
    Manifest(#[from] ManifestError),
    /// The id/version already has different immutable content.
    #[error("plugin `{id}` version `{version}` is already bound to different content")]
    VersionConflict {
        /// Validated plugin id.
        id: PluginId,
        /// Validated package version.
        version: PluginVersion,
    },
    /// Another process retained the exact bounded install lock.
    #[error("plugin cache install is busy")]
    Busy,
    /// A fixed filesystem operation failed. Raw paths and OS text are omitted.
    #[error("plugin cache filesystem operation `{operation}` failed")]
    Io {
        /// Compile-time operation label.
        operation: &'static str,
    },
}

pub(crate) const fn io(operation: &'static str) -> PackageCacheError {
    PackageCacheError::Io { operation }
}

pub(crate) const fn corrupt(reason: CacheCorruption) -> PackageCacheError {
    PackageCacheError::CorruptCache { reason }
}

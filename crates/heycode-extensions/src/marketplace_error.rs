//! Stable marketplace failures that never echo fetched catalog bytes.

use std::fmt;

use thiserror::Error;

use crate::{CatalogDigest, PackageContentHash, PluginId, PluginVersion};

/// Exactly which property of a pin an installed package violated.
///
/// One closed enum rather than three loose error variants: a Consumer that
/// wants to treat substitution as a single security event matches one arm, and
/// one that wants to distinguish the classes matches all three exhaustively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Substitution {
    /// The installed package carries a different identity than the pin.
    Identity {
        /// Identity the caller pinned.
        requested: PluginId,
        /// Identity the installed package actually declares.
        installed: PluginId,
    },
    /// The installed package carries a different version than the pin.
    ///
    /// A pinned version never silently upgrades to another offered version.
    Version {
        /// Pinned plugin id.
        id: PluginId,
        /// Version the caller pinned.
        requested: PluginVersion,
        /// Version the installed package actually declares.
        installed: PluginVersion,
    },
    /// The installed bytes do not hash to the pinned content digest.
    ///
    /// This is the substitution attack: a source served different bytes under
    /// an identity and version that is already trusted.
    Content {
        /// Pinned plugin id.
        id: PluginId,
        /// Pinned package version.
        version: PluginVersion,
        /// Digest the pin requires.
        pinned: PackageContentHash,
        /// Digest recomputed from the installed package tree.
        installed: PackageContentHash,
    },
}

impl fmt::Display for Substitution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity {
                requested,
                installed,
            } => write!(
                formatter,
                "pinned plugin `{requested}` installed as `{installed}`"
            ),
            Self::Version {
                id,
                requested,
                installed,
            } => write!(
                formatter,
                "pinned `{id}` version `{requested}` installed as version `{installed}`"
            ),
            Self::Content {
                id,
                version,
                pinned,
                installed,
            } => write!(
                formatter,
                "pinned `{id}` version `{version}` content {installed} does not match pinned {pinned}"
            ),
        }
    }
}

/// Failures from marketplace source configuration, catalog admission, and
/// package provenance establishment.
///
/// A marketplace catalog is remote data authored by whoever runs the
/// marketplace. No variant carries a locator, a signature, or any other value
/// read out of the fetched document: a malformed row is reported by its index
/// plus a compile-time field path and reason. The identities and digests that
/// do appear are validated newtypes whose character sets exclude control
/// characters by construction.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MarketplaceError {
    /// One named configuration or catalog field violated a stable rule.
    #[error("marketplace field `{field}` is invalid: {reason}")]
    InvalidField {
        /// Compile-time field path; never attacker-controlled text.
        field: &'static str,
        /// Compile-time safe explanation.
        reason: &'static str,
    },
    /// Catalog bytes exceeded the fixed admission budget.
    #[error("marketplace catalog exceeds the {limit}-byte admission limit")]
    CatalogTooLarge {
        /// Exact byte budget.
        limit: usize,
    },
    /// Catalog bytes did not hash to the digest the marketplace source pinned.
    ///
    /// Raised before the document is decoded, so an unpinned catalog is never
    /// parsed at all.
    #[error("marketplace catalog digest {found} does not match pinned {pinned}")]
    CatalogDigestMismatch {
        /// Digest recorded in the marketplace source configuration.
        pinned: CatalogDigest,
        /// Digest recomputed over the exact fetched bytes.
        found: CatalogDigest,
    },
    /// Catalog bytes are not UTF-8.
    #[error("marketplace catalog is not UTF-8")]
    CatalogNotUtf8,
    /// TOML syntax, shape, types, or unknown fields were invalid.
    #[error("marketplace catalog TOML is malformed or contains unknown fields")]
    InvalidDocument,
    /// The document uses an unsupported catalog schema.
    #[error("unsupported marketplace catalog schema {found}; supported schema is {supported}")]
    UnsupportedCatalogSchema {
        /// Version found in the document.
        found: u32,
        /// Exact schema understood by this implementation.
        supported: u32,
    },
    /// The catalog declared more package rows than the fixed budget allows.
    #[error("marketplace catalog exceeds the {limit}-entry admission limit")]
    TooManyEntries {
        /// Exact row budget.
        limit: usize,
    },
    /// One package row was invalid, which rejects the whole generation.
    #[error("marketplace catalog entry {entry} field `{field}` is invalid: {reason}")]
    InvalidEntry {
        /// Zero-based row position; derived, never fetched text.
        entry: usize,
        /// Compile-time field path; never attacker-controlled text.
        field: &'static str,
        /// Compile-time safe explanation.
        reason: &'static str,
    },
    /// The catalog offers no row for the exact requested id and version.
    ///
    /// A pin that the catalog cannot satisfy fails. It never resolves to a
    /// neighbouring version.
    #[error("marketplace offers no `{id}` version `{version}`")]
    NotOffered {
        /// Requested plugin id.
        id: PluginId,
        /// Requested package version.
        version: PluginVersion,
    },
    /// An installed package did not match the pin it was admitted against.
    ///
    /// Boxed because the substitution facts are larger than every other
    /// failure here, and a `Result` is cheaper to move when the rare error
    /// carries the weight.
    #[error("{0}")]
    Substituted(Box<Substitution>),
}

pub(crate) const fn invalid_field(field: &'static str, reason: &'static str) -> MarketplaceError {
    MarketplaceError::InvalidField { field, reason }
}

pub(crate) const fn invalid_entry(
    entry: usize,
    field: &'static str,
    reason: &'static str,
) -> MarketplaceError {
    MarketplaceError::InvalidEntry {
        entry,
        field,
        reason,
    }
}

pub(crate) fn substituted(substitution: Substitution) -> MarketplaceError {
    MarketplaceError::Substituted(Box::new(substitution))
}

//! Validated marketplace source, catalog and signature vocabulary.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::marketplace_error::invalid_field;
use crate::model::valid_kebab;
use crate::{
    MarketplaceError, PackageContentHash, PluginId, PluginSignature, PluginSourceKind,
    PluginVersion,
};

/// SHA-256 identity of one exact catalog document.
///
/// Deliberately a different type from [`PackageContentHash`]. That type
/// addresses a canonical package *tree* under its own domain separator; this
/// one addresses the exact bytes of one fetched document. Sharing a type would
/// let a caller pass one where the other is required, and the two can never be
/// equal for the same package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CatalogDigest(String);

impl CatalogDigest {
    /// Validate an algorithm-qualified lowercase digest.
    ///
    /// # Errors
    /// Anything but `sha256:` followed by exactly 64 lowercase hexadecimal
    /// digits is rejected.
    pub fn parse(value: &str) -> Result<Self, MarketplaceError> {
        let valid = value.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
        if !valid {
            return Err(invalid_field(
                "catalog_digest",
                "must be sha256 followed by 64 lowercase hexadecimal digits",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Digest of exact document bytes.
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

impl fmt::Display for CatalogDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable marketplace identity.
///
/// Equal to the namespace segment of every plugin id the marketplace may vend,
/// so a hostile catalog cannot shadow another marketplace's packages.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MarketplaceId(String);

impl MarketplaceId {
    /// Validate a marketplace identity.
    ///
    /// # Errors
    /// The identity must be one lowercase kebab-case segment within the stable
    /// length budget.
    pub fn new(value: impl Into<String>) -> Result<Self, MarketplaceError> {
        let value = value.into();
        if !valid_kebab(&value) {
            return Err(invalid_field(
                "marketplace",
                "must be one lowercase kebab-case segment",
            ));
        }
        Ok(Self(value))
    }

    /// Stable string identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MarketplaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Where a marketplace catalog document is fetched from.
///
/// Distinct from [`PluginSourceKind`], which says where one package comes
/// from. A catalog served over HTTPS routinely vends packages hosted
/// elsewhere, so the two are pinned independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceSourceKind {
    /// Catalog read from a portable relative path beneath a host-chosen root.
    ///
    /// Absolute paths are refused for the same reason PL01 refuses one in a
    /// package source: a locator that cannot be absolute cannot escape the
    /// root the host picked for it (GOTCHAS #103).
    LocalDirectory,
    /// Catalog read from a Git repository.
    Git,
    /// Catalog fetched over HTTPS.
    Https,
    /// Catalog read from a supported package registry.
    Registry,
}

impl MarketplaceSourceKind {
    /// Stable configuration identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalDirectory => "local_directory",
            Self::Git => "git",
            Self::Https => "https",
            Self::Registry => "registry",
        }
    }

    /// Whether this catalog arrives from outside the operator's own machine.
    ///
    /// A remote catalog may not vend [`PluginSourceKind::Local`] packages: a
    /// document authored elsewhere must not be able to point an installer at
    /// the operator's filesystem.
    #[must_use]
    pub const fn is_remote(self) -> bool {
        match self {
            Self::LocalDirectory => false,
            Self::Git | Self::Https | Self::Registry => true,
        }
    }

    pub(crate) const fn locator_kind(self) -> PluginSourceKind {
        match self {
            Self::LocalDirectory => PluginSourceKind::Local,
            Self::Git => PluginSourceKind::Git,
            Self::Https => PluginSourceKind::Https,
            Self::Registry => PluginSourceKind::Registry,
        }
    }
}

impl fmt::Display for MarketplaceSourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One configured marketplace, pinned to an exact catalog document.
///
/// The pin is mandatory. A marketplace whose catalog may change under the
/// operator cannot support any of this row's guarantees, because every package
/// digest it vends could change with it. Following a moving catalog safely
/// requires verifying a publisher signature over it, which this crate cannot
/// do — see [`SignatureState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MarketplaceSource {
    id: MarketplaceId,
    kind: MarketplaceSourceKind,
    locator: String,
    catalog_digest: CatalogDigest,
}

impl MarketplaceSource {
    /// Validate one pinned marketplace source.
    ///
    /// # Errors
    /// A locator that is not a public credential-free URL, registry path, or
    /// portable relative path for its kind, or a malformed catalog digest.
    pub fn new(
        id: MarketplaceId,
        kind: MarketplaceSourceKind,
        locator: impl Into<String>,
        catalog_digest: &str,
    ) -> Result<Self, MarketplaceError> {
        let locator = locator.into();
        crate::validator::validate_source_locator(kind.locator_kind(), &locator).map_err(|_| {
            invalid_field(
                "source.locator",
                "must be a bounded public locator without credentials, query strings, or traversal",
            )
        })?;
        Ok(Self {
            id,
            kind,
            locator,
            catalog_digest: CatalogDigest::parse(catalog_digest)?,
        })
    }

    /// Configured marketplace identity.
    #[must_use]
    pub const fn id(&self) -> &MarketplaceId {
        &self.id
    }

    /// Where the catalog document is fetched from.
    #[must_use]
    pub const fn kind(&self) -> MarketplaceSourceKind {
        self.kind
    }

    /// Validated catalog locator.
    #[must_use]
    pub fn locator(&self) -> &str {
        &self.locator
    }

    /// Exact catalog document this source is pinned to.
    #[must_use]
    pub const fn catalog_digest(&self) -> &CatalogDigest {
        &self.catalog_digest
    }
}

/// What this crate can honestly say about a detached package signature.
///
/// There is deliberately no `Valid` variant. This crate performs **no**
/// cryptographic verification and holds no trusted publisher key material, so
/// the strongest true statement it can make is that a syntactically canonical
/// Ed25519 signature was declared and recorded. Reporting anything stronger
/// would be provenance theatre: a reader would take a verified claim from a
/// boundary that never checked one.
///
/// Recording the signature is still worth doing — a later row that acquires a
/// key store can verify exactly these bytes — but until then, callers must not
/// treat [`SignatureState::Present`] as evidence of authorship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SignatureState {
    /// The catalog declared no signature for this package.
    Absent,
    /// A canonical signature was declared and recorded, and **not** verified.
    Present(PluginSignature),
}

/// One pinned package row from a validated marketplace catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogEntry {
    id: PluginId,
    version: PluginVersion,
    source_kind: PluginSourceKind,
    locator: String,
    content: PackageContentHash,
    signature: SignatureState,
}

impl CatalogEntry {
    pub(crate) const fn new(
        id: PluginId,
        version: PluginVersion,
        source_kind: PluginSourceKind,
        locator: String,
        content: PackageContentHash,
        signature: SignatureState,
    ) -> Self {
        Self {
            id,
            version,
            source_kind,
            locator,
            content,
            signature,
        }
    }

    /// Namespaced plugin identity this row offers.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// Exact offered package version.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        &self.version
    }

    /// Where the package itself is fetched from.
    #[must_use]
    pub const fn source_kind(&self) -> PluginSourceKind {
        self.source_kind
    }

    /// Validated package locator.
    #[must_use]
    pub fn locator(&self) -> &str {
        &self.locator
    }

    /// The pinned canonical package tree digest.
    ///
    /// This is heycode's own content address for the installed tree, which is
    /// what [`crate::PluginInstallCache`] computes and what this crate can
    /// therefore verify. It is not a digest of an upstream archive; the
    /// manifest's `source.checksum` addresses that artifact and no code in
    /// this crate can check it, because nothing here fetches one.
    #[must_use]
    pub const fn content(&self) -> &PackageContentHash {
        &self.content
    }

    /// Recorded, unverified signature state.
    #[must_use]
    pub const fn signature(&self) -> &SignatureState {
        &self.signature
    }
}

/// One validated marketplace catalog generation.
///
/// Whole-generation admission: a single malformed row rejects the entire
/// document, matching every other catalog boundary in this workspace. A
/// partially admitted catalog would let a hostile marketplace suppress rows by
/// corrupting them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MarketplaceCatalog {
    pub(crate) schema_version: u32,
    pub(crate) marketplace: MarketplaceId,
    pub(crate) digest: CatalogDigest,
    pub(crate) entries: Vec<CatalogEntry>,
}

impl MarketplaceCatalog {
    /// Exact catalog schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Identity of the marketplace that authored this generation.
    #[must_use]
    pub const fn marketplace(&self) -> &MarketplaceId {
        &self.marketplace
    }

    /// Verified digest of the exact bytes this generation was parsed from.
    #[must_use]
    pub const fn digest(&self) -> &CatalogDigest {
        &self.digest
    }

    /// Offered rows, ordered by id then version precedence.
    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// The single row offering this exact id and version, if any.
    ///
    /// There is no nearest-match behaviour: a pin the catalog does not offer
    /// resolves to nothing rather than to a neighbouring version.
    #[must_use]
    pub fn pin(&self, id: &PluginId, version: &PluginVersion) -> Option<&CatalogEntry> {
        self.entries
            .iter()
            .find(|entry| entry.id() == id && entry.version() == version)
    }
}

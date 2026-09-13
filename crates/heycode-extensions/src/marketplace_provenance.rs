//! Established package provenance and the substitution gate that produces it.

use serde::Serialize;

use crate::marketplace_error::substituted;
use crate::{
    InstalledPlugin, MarketplaceCatalog, MarketplaceError, MarketplaceId, PackageContentHash,
    PluginId, PluginSourceKind, PluginVersion, SignatureState, Substitution,
};

/// Where a package actually came from, as far as heycode can establish.
///
/// Establishing an origin requires evidence the *host* held: a catalog pin the
/// operator configured, or a path the operator named. The package's own
/// `[source]` block is a claim by the package about itself and is never
/// evidence — a hostile package would simply claim whatever it needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageOrigin {
    /// Admitted against one pinned row of this marketplace's catalog.
    Marketplace {
        /// The marketplace whose catalog pinned this package.
        marketplace: MarketplaceId,
    },
    /// Installed from a path the operator named at the moment of installing.
    OperatorPath,
    /// No origin evidence exists.
    ///
    /// Unknown is a terminal state. There is no constructor anywhere in this
    /// crate that turns it into either of the established variants, because
    /// the only thing that could justify the promotion — evidence — is by
    /// definition absent.
    Unknown,
}

/// What heycode can report about one installed package's origin.
///
/// The verified content digest is recorded alongside the origin so the record
/// can be re-checked later. A digest compared once at install time and never
/// again is not substitution resistance: it says the bytes were right once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageProvenance {
    origin: PackageOrigin,
    id: PluginId,
    version: PluginVersion,
    content: PackageContentHash,
    signature: SignatureState,
    claimed_source_kind: PluginSourceKind,
}

impl PackageProvenance {
    /// Provenance for a package installed from a path the operator named.
    ///
    /// The operator's own action is the evidence. There is no publisher pin,
    /// so the recorded digest is the one the cache computed, and re-checking
    /// detects the bytes changing under this identity.
    #[must_use]
    pub fn operator_path(installed: &InstalledPlugin) -> Self {
        Self::record(
            PackageOrigin::OperatorPath,
            installed,
            SignatureState::Absent,
        )
    }

    /// Provenance for a package whose origin cannot be established.
    ///
    /// Use this whenever no host-held evidence exists. It is deliberately the
    /// honest answer rather than a plausible one: a package that claims a
    /// marketplace origin in its own manifest still lands here.
    #[must_use]
    pub fn unknown(installed: &InstalledPlugin) -> Self {
        Self::record(PackageOrigin::Unknown, installed, SignatureState::Absent)
    }

    fn record(
        origin: PackageOrigin,
        installed: &InstalledPlugin,
        signature: SignatureState,
    ) -> Self {
        Self {
            origin,
            id: installed.manifest().id().clone(),
            version: installed.manifest().version().clone(),
            content: installed.content_hash().clone(),
            signature,
            claimed_source_kind: installed.manifest().source().kind,
        }
    }

    /// Established origin, or [`PackageOrigin::Unknown`].
    #[must_use]
    pub const fn origin(&self) -> &PackageOrigin {
        &self.origin
    }

    /// Whether any host-held evidence established this origin.
    #[must_use]
    pub const fn is_established(&self) -> bool {
        match self.origin {
            PackageOrigin::Marketplace { .. } | PackageOrigin::OperatorPath => true,
            PackageOrigin::Unknown => false,
        }
    }

    /// Plugin identity these facts are about.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// Package version these facts are about.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        &self.version
    }

    /// The canonical package tree digest verified when this record was made.
    #[must_use]
    pub const fn content(&self) -> &PackageContentHash {
        &self.content
    }

    /// Recorded, unverified signature state carried over from the catalog row.
    #[must_use]
    pub const fn signature(&self) -> &SignatureState {
        &self.signature
    }

    /// What the package's own manifest asserts about where it came from.
    ///
    /// Reportable so a surface can show a claim next to the established fact,
    /// which is how an operator sees that a package claiming a marketplace
    /// origin actually arrived with none. Never evidence for [`Self::origin`].
    #[must_use]
    pub const fn claimed_source_kind(&self) -> PluginSourceKind {
        self.claimed_source_kind
    }

    /// Re-check a recorded provenance against a freshly resolved install.
    ///
    /// The cache's own resolution already proves the object matches the
    /// reference the cache committed. That is self-consistency: an attacker
    /// who can write the cache writes both halves. This compares against the
    /// digest recorded when the origin was established, which the cache does
    /// not own.
    ///
    /// # Errors
    /// [`MarketplaceError::Substituted`] with the exact [`Substitution`] class.
    pub fn reverify(&self, installed: &InstalledPlugin) -> Result<(), MarketplaceError> {
        check_installed(&self.id, &self.version, &self.content, installed)
    }
}

impl MarketplaceCatalog {
    /// Admit an installed package against the pin the caller asked for.
    ///
    /// The requested id and version are the authority, never the installed
    /// package's own declarations. Reading the identity off what arrived and
    /// then looking *that* up in the catalog would admit any version the
    /// marketplace happens to offer, which is a silent upgrade wearing a
    /// verification's clothes.
    ///
    /// # Errors
    /// [`MarketplaceError::NotOffered`] when the catalog has no row for this
    /// exact id and version, or [`MarketplaceError::Substituted`] when the
    /// installed package's identity, version, or bytes differ from the pin.
    pub fn admit(
        &self,
        id: &PluginId,
        version: &PluginVersion,
        installed: &InstalledPlugin,
    ) -> Result<PackageProvenance, MarketplaceError> {
        let entry = self
            .pin(id, version)
            .ok_or_else(|| MarketplaceError::NotOffered {
                id: id.clone(),
                version: version.clone(),
            })?;
        check_installed(id, version, entry.content(), installed)?;
        Ok(PackageProvenance::record(
            PackageOrigin::Marketplace {
                marketplace: self.marketplace().clone(),
            },
            installed,
            entry.signature().clone(),
        ))
    }
}

/// The one comparison behind both admission and re-verification.
///
/// Kept single so a change to the rule cannot hold in one direction and lapse
/// in the other.
fn check_installed(
    id: &PluginId,
    version: &PluginVersion,
    content: &PackageContentHash,
    installed: &InstalledPlugin,
) -> Result<(), MarketplaceError> {
    if installed.manifest().id() != id {
        return Err(substituted(Substitution::Identity {
            requested: id.clone(),
            installed: installed.manifest().id().clone(),
        }));
    }
    if installed.manifest().version() != version {
        return Err(substituted(Substitution::Version {
            id: id.clone(),
            requested: version.clone(),
            installed: installed.manifest().version().clone(),
        }));
    }
    if installed.content_hash() != content {
        return Err(substituted(Substitution::Content {
            id: id.clone(),
            version: version.clone(),
            pinned: content.clone(),
            installed: installed.content_hash().clone(),
        }));
    }
    Ok(())
}

//! PL08 managed marketplace and plugin policy.
//!
//! The policy is a pure, immutable administrator snapshot. Each rule pins one
//! exact marketplace source, catalog generation, package source, plugin
//! version, canonical tree digest, update channel, host platform, signature
//! posture and capability ceiling. Evaluation always produces a verdict for
//! all eight axes; only eight affirmative [`ManagedPolicyVerdict::Allowed`]
//! values authorize an operation. In particular, an unverified signature or
//! upstream artifact checksum remains `Unknown` and can never authorize an
//! install or activation.
//!
//! [`crate::PluginInstallCache::install_managed_marketplace`] freezes and
//! validates source bytes, evaluates PL05 provenance plus this policy, and only
//! then enters PL02's lock/object/reference commit. Activation resolves and
//! rehashes the cache again, re-applies PL05 and the current policy, freezes the
//! declarative documents, and only then returns a core plugin. Root settings,
//! managed-profile parsing and product composition remain Consumers of this
//! boundary; this module performs no discovery, persistence, network fetch or
//! signature verification.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use heycode_core::Plugin;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::cache_source::PreparedPackage;
use crate::lifecycle::{LifecycleError, PluginLifecycleAdmission, PluginOperation};
use crate::{
    CatalogEntry, ContributionKind, DeclarativeContributionHost, DeclarativePackage,
    DeclarativePluginError, InstalledPlugin, MarketplaceCatalog, MarketplaceError,
    MarketplaceSource, PackageCacheError, PackageContentHash, PackageProvenance, PlatformTarget,
    PluginId, PluginInstallCache, PluginManifest, PluginPermission, PluginResolutionError,
    PluginVersion, ResolvedPluginGraph, SignatureState, UpdateChannel,
    declarative_activation_plugin,
};

/// Every independent PL08 admission decision, in evaluation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedPolicyAxis {
    /// Exact marketplace and package source pins.
    Source,
    /// Manifest-requested update channel.
    Channel,
    /// Exact marketplace namespace acting as the catalog publisher identity.
    Publisher,
    /// Exact requested/catalog/manifest semantic version.
    Version,
    /// Pinned catalog and package-tree digests, plus optional artifact proof.
    Digest,
    /// Explicit signature requirement and available verification evidence.
    Signature,
    /// Exact administrator-approved host target.
    Platform,
    /// Requested permission and contribution-kind ceiling.
    Capability,
}

impl ManagedPolicyAxis {
    /// Complete closed decision set, in stable fail-closed order.
    pub const ALL: [Self; 8] = [
        Self::Source,
        Self::Channel,
        Self::Publisher,
        Self::Version,
        Self::Digest,
        Self::Signature,
        Self::Platform,
        Self::Capability,
    ];

    /// Stable policy identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Channel => "channel",
            Self::Publisher => "publisher",
            Self::Version => "version",
            Self::Digest => "digest",
            Self::Signature => "signature",
            Self::Platform => "platform",
            Self::Capability => "capability",
        }
    }
}

impl fmt::Display for ManagedPolicyAxis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One policy axis outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedPolicyVerdict {
    /// Evidence exists and satisfies the exact administrator rule.
    Allowed,
    /// Evidence exists but violates or lacks the configured allowance.
    Denied,
    /// The current boundary cannot establish the fact the rule requires.
    Unknown,
}

impl ManagedPolicyVerdict {
    const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

impl fmt::Display for ManagedPolicyVerdict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        })
    }
}

/// Explicit administrator posture for PL05's upstream checksum limitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedChecksumRequirement {
    /// Require the verified catalog-document and canonical package-tree pins.
    CatalogAndPackage,
    /// Additionally require verification of the manifest's upstream artifact
    /// checksum.
    ///
    /// No current fetch owner supplies that proof, so a syntactically present
    /// checksum evaluates to `Unknown`, never `Allowed`.
    VerifiedUpstreamArtifact,
}

/// Explicit administrator posture for PL05's unverified signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSignatureRequirement {
    /// Signature authorship is not required; digest pins remain mandatory.
    NotRequired,
    /// Require a canonical declaration pinned by the exact catalog bytes.
    ///
    /// This proves only that a declaration was pinned, not authorship.
    Declared,
    /// Require publisher-authenticated signature verification.
    ///
    /// `SignatureState::Present` is explicitly unverified, so it evaluates to
    /// `Unknown` until a later cryptographic verifier supplies stronger
    /// evidence.
    Verified,
}

/// Exact contribution and permission ceiling for one managed package rule.
#[derive(Clone, PartialEq, Eq)]
pub struct ManagedCapabilityPolicy {
    permissions: BTreeSet<PluginPermission>,
    contributions: BTreeSet<ContributionKind>,
}

impl ManagedCapabilityPolicy {
    /// Construct explicit permission and contribution allowlists.
    ///
    /// Empty lists deny every requested permission or contribution kind. A
    /// duplicate fails rather than being silently normalized, keeping an
    /// administrator document deterministic.
    ///
    /// # Errors
    /// Duplicate permission or contribution values.
    pub fn new(
        permissions: impl IntoIterator<Item = PluginPermission>,
        contributions: impl IntoIterator<Item = ContributionKind>,
    ) -> Result<Self, ManagedPolicyConfigurationError> {
        let mut permission_set = BTreeSet::new();
        for permission in permissions {
            if !permission_set.insert(permission) {
                return Err(ManagedPolicyConfigurationError::DuplicatePermission);
            }
        }
        let mut contribution_set = BTreeSet::new();
        for contribution in contributions {
            if !contribution_set.insert(contribution) {
                return Err(ManagedPolicyConfigurationError::DuplicateContribution);
            }
        }
        Ok(Self {
            permissions: permission_set,
            contributions: contribution_set,
        })
    }

    /// Explicit permitted manifest permissions.
    #[must_use]
    pub const fn permissions(&self) -> &BTreeSet<PluginPermission> {
        &self.permissions
    }

    /// Explicit permitted contribution kinds.
    #[must_use]
    pub const fn contributions(&self) -> &BTreeSet<ContributionKind> {
        &self.contributions
    }

    fn admits(&self, manifest: &PluginManifest) -> bool {
        manifest
            .permissions()
            .iter()
            .all(|permission| self.permissions.contains(permission))
            && manifest
                .contributions()
                .iter()
                .all(|contribution| self.contributions.contains(&contribution.kind()))
    }
}

impl fmt::Debug for ManagedCapabilityPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedCapabilityPolicy")
            .field("permissions", &self.permissions)
            .field("contributions", &self.contributions)
            .finish()
    }
}

/// One exact administrator-approved package rule.
#[derive(Clone)]
pub struct ManagedPluginPolicyRule {
    source: MarketplaceSource,
    entry: CatalogEntry,
    channel: UpdateChannel,
    platform: PlatformTarget,
    checksum: ManagedChecksumRequirement,
    signature: ManagedSignatureRequirement,
    capabilities: ManagedCapabilityPolicy,
}

impl ManagedPluginPolicyRule {
    /// Pin one exact row from one already pinned PL05 catalog generation.
    ///
    /// The rule copies the configured marketplace locator/digest and the
    /// package row's locator/tree digest/signature declaration. A later
    /// catalog or mirror cannot satisfy the rule merely by vending the same id.
    ///
    /// # Errors
    /// A catalog not admitted from `source`, or a missing exact id/version row.
    #[allow(clippy::too_many_arguments)]
    pub fn from_catalog(
        source: &MarketplaceSource,
        catalog: &MarketplaceCatalog,
        id: &PluginId,
        version: &PluginVersion,
        channel: UpdateChannel,
        platform: PlatformTarget,
        checksum: ManagedChecksumRequirement,
        signature: ManagedSignatureRequirement,
        capabilities: ManagedCapabilityPolicy,
    ) -> Result<Self, ManagedPolicyConfigurationError> {
        if catalog.marketplace() != source.id() || catalog.digest() != source.catalog_digest() {
            return Err(ManagedPolicyConfigurationError::CatalogSourceMismatch);
        }
        let entry = catalog
            .pin(id, version)
            .cloned()
            .ok_or(ManagedPolicyConfigurationError::RuleNotOffered)?;
        Ok(Self {
            source: source.clone(),
            entry,
            channel,
            platform,
            checksum,
            signature,
            capabilities,
        })
    }

    /// Exact plugin identity this rule admits.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        self.entry.id()
    }

    /// Exact plugin version this rule admits.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        self.entry.version()
    }

    /// Required update channel.
    #[must_use]
    pub const fn channel(&self) -> UpdateChannel {
        self.channel
    }

    /// Required host target.
    #[must_use]
    pub const fn platform(&self) -> PlatformTarget {
        self.platform
    }

    /// Required digest evidence.
    #[must_use]
    pub const fn checksum_requirement(&self) -> ManagedChecksumRequirement {
        self.checksum
    }

    /// Required signature evidence.
    #[must_use]
    pub const fn signature_requirement(&self) -> ManagedSignatureRequirement {
        self.signature
    }

    /// Permission and contribution ceiling.
    #[must_use]
    pub const fn capabilities(&self) -> &ManagedCapabilityPolicy {
        &self.capabilities
    }
}

impl fmt::Debug for ManagedPluginPolicyRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedPluginPolicyRule")
            .field("id", self.entry.id())
            .field("version", self.entry.version())
            .field("marketplace", self.source.id())
            .field("marketplace_source_kind", &self.source.kind())
            .field("package_source_kind", &self.entry.source_kind())
            .field("channel", &self.channel)
            .field("platform", &self.platform)
            .field("checksum", &self.checksum)
            .field("signature", &self.signature)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

/// Immutable exact-rule managed policy snapshot.
#[derive(Clone)]
pub struct ManagedPluginPolicy {
    rules: BTreeMap<(PluginId, String), ManagedPluginPolicyRule>,
}

impl ManagedPluginPolicy {
    /// Construct one deterministic managed policy generation.
    ///
    /// An empty policy is valid and denies every package. There is no implicit
    /// wildcard or fallback rule.
    ///
    /// # Errors
    /// More than one rule names the same exact id/version.
    pub fn new(
        rules: impl IntoIterator<Item = ManagedPluginPolicyRule>,
    ) -> Result<Self, ManagedPolicyConfigurationError> {
        let mut rows = BTreeMap::new();
        for rule in rules {
            let key = (rule.id().clone(), rule.version().as_str().to_owned());
            if rows.insert(key, rule).is_some() {
                return Err(ManagedPolicyConfigurationError::DuplicateRule);
            }
        }
        Ok(Self { rules: rows })
    }

    /// Number of exact id/version rules in this generation.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether no package can be admitted by this generation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Evaluate PL05 evidence and the complete manifest against all axes.
    ///
    /// This method performs no I/O or mutation and always returns every axis.
    /// A rule miss is eight explicit denials, never an implicit default.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_marketplace(
        &self,
        source: &MarketplaceSource,
        catalog: &MarketplaceCatalog,
        id: &PluginId,
        version: &PluginVersion,
        manifest: &PluginManifest,
        content: &PackageContentHash,
        host: PlatformTarget,
    ) -> ManagedPolicyEvaluation {
        let key = (id.clone(), version.as_str().to_owned());
        let Some(rule) = self.rules.get(&key) else {
            return ManagedPolicyEvaluation::all(ManagedPolicyVerdict::Denied);
        };
        let current = catalog.pin(id, version);

        let source_verdict = match current {
            None => ManagedPolicyVerdict::Unknown,
            Some(entry)
                if &rule.source == source
                    && catalog.marketplace() == source.id()
                    && catalog.digest() == source.catalog_digest()
                    && entry.source_kind() == rule.entry.source_kind()
                    && entry.locator() == rule.entry.locator() =>
            {
                ManagedPolicyVerdict::Allowed
            }
            Some(_) => ManagedPolicyVerdict::Denied,
        };

        let channel = if manifest.source().update_channel == rule.channel {
            ManagedPolicyVerdict::Allowed
        } else {
            ManagedPolicyVerdict::Denied
        };

        let namespace = id.as_str().split('/').next();
        let publisher = if source.id() == rule.source.id()
            && catalog.marketplace() == source.id()
            && namespace == Some(source.id().as_str())
        {
            ManagedPolicyVerdict::Allowed
        } else {
            ManagedPolicyVerdict::Denied
        };

        let version_verdict = match current {
            Some(entry)
                if entry.id() == rule.entry.id()
                    && entry.version() == rule.entry.version()
                    && manifest.id() == id
                    && manifest.version() == version =>
            {
                ManagedPolicyVerdict::Allowed
            }
            Some(_) | None => ManagedPolicyVerdict::Denied,
        };

        let digest_base = match current {
            None => ManagedPolicyVerdict::Unknown,
            Some(entry)
                if catalog.digest() == source.catalog_digest()
                    && source.catalog_digest() == rule.source.catalog_digest()
                    && entry.content() == rule.entry.content()
                    && content == rule.entry.content() =>
            {
                ManagedPolicyVerdict::Allowed
            }
            Some(_) => ManagedPolicyVerdict::Denied,
        };
        let digest = match (digest_base, rule.checksum) {
            (ManagedPolicyVerdict::Allowed, ManagedChecksumRequirement::CatalogAndPackage) => {
                ManagedPolicyVerdict::Allowed
            }
            (
                ManagedPolicyVerdict::Allowed,
                ManagedChecksumRequirement::VerifiedUpstreamArtifact,
            ) if manifest.source().checksum.is_some() => ManagedPolicyVerdict::Unknown,
            (
                ManagedPolicyVerdict::Allowed,
                ManagedChecksumRequirement::VerifiedUpstreamArtifact,
            ) => ManagedPolicyVerdict::Denied,
            (verdict, _) => verdict,
        };

        let signature = match current {
            None => ManagedPolicyVerdict::Unknown,
            Some(entry) if entry.signature() != rule.entry.signature() => {
                ManagedPolicyVerdict::Denied
            }
            Some(entry) => match (rule.signature, entry.signature()) {
                (ManagedSignatureRequirement::NotRequired, _) => ManagedPolicyVerdict::Allowed,
                (ManagedSignatureRequirement::Declared, SignatureState::Present(_)) => {
                    ManagedPolicyVerdict::Allowed
                }
                (ManagedSignatureRequirement::Declared, SignatureState::Absent) => {
                    ManagedPolicyVerdict::Denied
                }
                (ManagedSignatureRequirement::Verified, SignatureState::Present(_)) => {
                    ManagedPolicyVerdict::Unknown
                }
                (ManagedSignatureRequirement::Verified, SignatureState::Absent) => {
                    ManagedPolicyVerdict::Denied
                }
            },
        };

        let platform = if host == rule.platform && manifest.platforms().contains(&host) {
            ManagedPolicyVerdict::Allowed
        } else {
            ManagedPolicyVerdict::Denied
        };
        let capability = if rule.capabilities.admits(manifest) {
            ManagedPolicyVerdict::Allowed
        } else {
            ManagedPolicyVerdict::Denied
        };

        ManagedPolicyEvaluation {
            source: source_verdict,
            channel,
            publisher,
            version: version_verdict,
            digest,
            signature,
            platform,
            capability,
        }
    }
}

impl fmt::Debug for ManagedPluginPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedPluginPolicy")
            .field("rule_count", &self.rules.len())
            .finish()
    }
}

/// SHA-256 identity of one exact PL08 source/catalog/host/policy generation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ManagedPluginAdmissionGenerationId(String);

impl ManagedPluginAdmissionGenerationId {
    /// Parse one canonical generation fingerprint.
    ///
    /// # Errors
    /// Anything but `sha256:` followed by 64 lowercase hexadecimal digits.
    pub fn parse(value: impl Into<String>) -> Result<Self, ManagedPolicyConfigurationError> {
        let value = value.into();
        let valid = value.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
        if !valid {
            return Err(ManagedPolicyConfigurationError::InvalidGenerationId);
        }
        Ok(Self(value))
    }

    /// Canonical algorithm-qualified fingerprint.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ManagedPluginAdmissionGenerationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedPluginAdmissionGenerationId([SHA256])")
    }
}

/// Immutable PL08 evidence generation shared by lifecycle and code activation.
#[derive(Clone)]
pub struct ManagedPluginAdmissionGeneration {
    id: ManagedPluginAdmissionGenerationId,
    marketplace_source: MarketplaceSource,
    catalog: MarketplaceCatalog,
    host: PlatformTarget,
    policy: ManagedPluginPolicy,
}

impl ManagedPluginAdmissionGeneration {
    /// Bind and fingerprint one exact current PL08 generation.
    ///
    /// # Errors
    /// The catalog must be the exact generation pinned by its source.
    pub fn new(
        marketplace_source: MarketplaceSource,
        catalog: MarketplaceCatalog,
        host: PlatformTarget,
        policy: ManagedPluginPolicy,
    ) -> Result<Self, ManagedPolicyConfigurationError> {
        if catalog.marketplace() != marketplace_source.id()
            || catalog.digest() != marketplace_source.catalog_digest()
            || policy.rules.values().any(|rule| {
                rule.source != marketplace_source
                    || catalog.pin(rule.id(), rule.version()) != Some(&rule.entry)
            })
        {
            return Err(ManagedPolicyConfigurationError::CatalogSourceMismatch);
        }
        let id = fingerprint_generation(&marketplace_source, &catalog, host, &policy);
        Ok(Self {
            id,
            marketplace_source,
            catalog,
            host,
            policy,
        })
    }

    /// Exact generation fingerprint for stale-authority rejection.
    #[must_use]
    pub const fn id(&self) -> &ManagedPluginAdmissionGenerationId {
        &self.id
    }

    /// Re-resolve, rehash, re-prove provenance and re-evaluate all PL08 axes.
    ///
    /// # Errors
    /// Cache, provenance, policy or immutable-document admission failure.
    pub fn prepare_managed_declarative(
        &self,
        cache: &PluginInstallCache,
        id: &PluginId,
        version: &PluginVersion,
    ) -> Result<ManagedDeclarativePackage, ManagedPluginError> {
        cache.prepare_managed_declarative(
            &self.marketplace_source,
            &self.catalog,
            id,
            version,
            self.host,
            &self.policy,
        )
    }
}

impl fmt::Debug for ManagedPluginAdmissionGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedPluginAdmissionGeneration")
            .field("id", &self.id)
            .field("marketplace", self.marketplace_source.id())
            .field("host", &self.host)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

fn fingerprint_generation(
    source: &MarketplaceSource,
    catalog: &MarketplaceCatalog,
    host: PlatformTarget,
    policy: &ManagedPluginPolicy,
) -> ManagedPluginAdmissionGenerationId {
    let mut hasher = Sha256::new();
    hash_part(&mut hasher, b"dshx-managed-plugin-admission-v1");
    hash_part(&mut hasher, source.id().as_str().as_bytes());
    hash_part(&mut hasher, source.kind().as_str().as_bytes());
    hash_part(&mut hasher, source.locator().as_bytes());
    hash_part(&mut hasher, source.catalog_digest().as_str().as_bytes());
    hash_part(&mut hasher, &catalog.schema_version().to_be_bytes());
    hash_part(&mut hasher, catalog.marketplace().as_str().as_bytes());
    hash_part(&mut hasher, catalog.digest().as_str().as_bytes());
    hash_part(&mut hasher, host.os().as_str().as_bytes());
    hash_part(&mut hasher, host.architecture().as_str().as_bytes());
    for rule in policy.rules.values() {
        hash_part(&mut hasher, rule.id().as_str().as_bytes());
        hash_part(&mut hasher, rule.version().as_str().as_bytes());
        hash_part(
            &mut hasher,
            match rule.channel {
                UpdateChannel::Stable => b"stable",
                UpdateChannel::Preview => b"preview",
                UpdateChannel::Pinned => b"pinned",
            },
        );
        hash_part(&mut hasher, rule.platform.os().as_str().as_bytes());
        hash_part(
            &mut hasher,
            rule.platform.architecture().as_str().as_bytes(),
        );
        hash_part(
            &mut hasher,
            match rule.checksum {
                ManagedChecksumRequirement::CatalogAndPackage => b"catalog_and_package",
                ManagedChecksumRequirement::VerifiedUpstreamArtifact => {
                    b"verified_upstream_artifact"
                }
            },
        );
        hash_part(
            &mut hasher,
            match rule.signature {
                ManagedSignatureRequirement::NotRequired => b"not_required",
                ManagedSignatureRequirement::Declared => b"declared",
                ManagedSignatureRequirement::Verified => b"verified",
            },
        );
        for permission in &rule.capabilities.permissions {
            hash_part(&mut hasher, permission.as_str().as_bytes());
        }
        hash_part(&mut hasher, b"contributions");
        for contribution in &rule.capabilities.contributions {
            hash_part(&mut hasher, contribution.as_str().as_bytes());
        }
    }
    let mut rendered = String::with_capacity(71);
    rendered.push_str("sha256:");
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        let _ = write!(rendered, "{byte:02x}");
    }
    ManagedPluginAdmissionGenerationId(rendered)
}

fn hash_part(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// Complete safe eight-axis evaluation report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ManagedPolicyEvaluation {
    source: ManagedPolicyVerdict,
    channel: ManagedPolicyVerdict,
    publisher: ManagedPolicyVerdict,
    version: ManagedPolicyVerdict,
    digest: ManagedPolicyVerdict,
    signature: ManagedPolicyVerdict,
    platform: ManagedPolicyVerdict,
    capability: ManagedPolicyVerdict,
}

impl ManagedPolicyEvaluation {
    const fn all(verdict: ManagedPolicyVerdict) -> Self {
        Self {
            source: verdict,
            channel: verdict,
            publisher: verdict,
            version: verdict,
            digest: verdict,
            signature: verdict,
            platform: verdict,
            capability: verdict,
        }
    }

    /// Verdict for one exact decision axis.
    #[must_use]
    pub const fn verdict(self, axis: ManagedPolicyAxis) -> ManagedPolicyVerdict {
        match axis {
            ManagedPolicyAxis::Source => self.source,
            ManagedPolicyAxis::Channel => self.channel,
            ManagedPolicyAxis::Publisher => self.publisher,
            ManagedPolicyAxis::Version => self.version,
            ManagedPolicyAxis::Digest => self.digest,
            ManagedPolicyAxis::Signature => self.signature,
            ManagedPolicyAxis::Platform => self.platform,
            ManagedPolicyAxis::Capability => self.capability,
        }
    }

    /// Whether every axis is affirmatively allowed.
    #[must_use]
    pub fn is_allowed(self) -> bool {
        ManagedPolicyAxis::ALL
            .into_iter()
            .all(|axis| self.verdict(axis).is_allowed())
    }

    /// Convert an all-allowed evaluation into operation authority.
    ///
    /// # Errors
    /// The first denied or unknown axis in [`ManagedPolicyAxis::ALL`] order.
    pub fn ensure_allowed(self) -> Result<(), ManagedPolicyRejection> {
        for axis in ManagedPolicyAxis::ALL {
            let verdict = self.verdict(axis);
            if !verdict.is_allowed() {
                return Err(ManagedPolicyRejection { axis, verdict });
            }
        }
        Ok(())
    }
}

/// Body-free stable policy refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedPolicyRejection {
    axis: ManagedPolicyAxis,
    verdict: ManagedPolicyVerdict,
}

impl ManagedPolicyRejection {
    /// Axis that prevented the operation.
    #[must_use]
    pub const fn axis(&self) -> ManagedPolicyAxis {
        self.axis
    }

    /// Denied or Unknown evidence state.
    #[must_use]
    pub const fn verdict(&self) -> ManagedPolicyVerdict {
        self.verdict
    }
}

impl fmt::Display for ManagedPolicyRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.verdict {
            ManagedPolicyVerdict::Denied => {
                write!(formatter, "managed plugin policy denied {}", self.axis)
            }
            ManagedPolicyVerdict::Unknown => write!(
                formatter,
                "managed plugin policy has unknown {} evidence",
                self.axis
            ),
            ManagedPolicyVerdict::Allowed => {
                formatter.write_str("managed plugin policy rejection is invalid")
            }
        }
    }
}

impl std::error::Error for ManagedPolicyRejection {}

/// Stable invalid administrator-policy construction failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ManagedPolicyConfigurationError {
    /// The catalog was not the exact generation pinned by its source.
    #[error("managed plugin rule catalog does not match its marketplace source")]
    CatalogSourceMismatch,
    /// The exact package id/version is absent from the pinned generation.
    #[error("managed plugin rule package is not offered by its pinned catalog")]
    RuleNotOffered,
    /// Two rules target the same exact package id/version.
    #[error("managed plugin policy contains a duplicate package rule")]
    DuplicateRule,
    /// A permission allowlist contains the same value twice.
    #[error("managed plugin capability policy contains a duplicate permission")]
    DuplicatePermission,
    /// A contribution allowlist contains the same value twice.
    #[error("managed plugin capability policy contains a duplicate contribution kind")]
    DuplicateContribution,
    /// A PL08 generation fingerprint was not canonical lowercase SHA-256.
    #[error("managed plugin admission generation id is invalid")]
    InvalidGenerationId,
}

/// Managed install/activation failures with no catalog body, locator,
/// signature bytes, package document or host path.
#[derive(Debug, Error)]
pub enum ManagedPluginError {
    /// PL07 generation was unresolved or the package drifted from it.
    #[error(transparent)]
    Resolution(Box<PluginResolutionError>),
    /// PL02 source/cache validation or immutable publication failed.
    #[error(transparent)]
    Cache(#[from] PackageCacheError),
    /// PL05 exact catalog/package provenance failed.
    #[error(transparent)]
    Marketplace(#[from] MarketplaceError),
    /// Administrator policy denied or could not prove one axis.
    #[error(transparent)]
    Policy(#[from] ManagedPolicyRejection),
    /// Bounded immutable declarative loading or generation admission failed.
    #[error(transparent)]
    Declarative(#[from] DeclarativePluginError),
}

impl From<PluginResolutionError> for ManagedPluginError {
    fn from(error: PluginResolutionError) -> Self {
        Self::Resolution(Box::new(error))
    }
}

/// Receipt for a package admitted and committed through the managed path.
pub struct ManagedInstalledPlugin {
    installed: InstalledPlugin,
    provenance: PackageProvenance,
    evaluation: ManagedPolicyEvaluation,
}

impl ManagedInstalledPlugin {
    /// Exact PL02 installed package receipt.
    #[must_use]
    pub const fn installed(&self) -> &InstalledPlugin {
        &self.installed
    }

    /// PL05 marketplace provenance established before cache mutation.
    #[must_use]
    pub const fn provenance(&self) -> &PackageProvenance {
        &self.provenance
    }

    /// Complete PL08 pre-mutation decision report.
    #[must_use]
    pub const fn evaluation(&self) -> &ManagedPolicyEvaluation {
        &self.evaluation
    }
}

impl fmt::Debug for ManagedInstalledPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedInstalledPlugin")
            .field("id", self.installed.manifest().id())
            .field("version", self.installed.manifest().version())
            .field("disposition", &self.installed.disposition())
            .field("evaluation", &self.evaluation)
            .finish()
    }
}

/// Opaque package that passed current PL05 and PL08 activation admission.
pub struct ManagedDeclarativePackage {
    package: DeclarativePackage,
    provenance: PackageProvenance,
    evaluation: ManagedPolicyEvaluation,
}

/// Lifecycle admission backed by one exact current managed policy generation.
pub struct ManagedLifecycleAdmission {
    cache: Arc<PluginInstallCache>,
    generation: ManagedPluginAdmissionGeneration,
}

impl ManagedLifecycleAdmission {
    /// Bind current cache, pinned marketplace generation, host and policy.
    ///
    /// # Errors
    /// The catalog must be the exact generation pinned by its source.
    pub fn new(
        cache: Arc<PluginInstallCache>,
        marketplace_source: MarketplaceSource,
        catalog: MarketplaceCatalog,
        host: PlatformTarget,
        policy: ManagedPluginPolicy,
    ) -> Result<Self, ManagedPolicyConfigurationError> {
        let generation =
            ManagedPluginAdmissionGeneration::new(marketplace_source, catalog, host, policy)?;
        Ok(Self { cache, generation })
    }

    /// Bind lifecycle admission to a generation also used by code activation.
    #[must_use]
    pub const fn from_generation(
        cache: Arc<PluginInstallCache>,
        generation: ManagedPluginAdmissionGeneration,
    ) -> Self {
        Self { cache, generation }
    }
}

impl fmt::Debug for ManagedLifecycleAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedLifecycleAdmission")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl PluginLifecycleAdmission for ManagedLifecycleAdmission {
    fn authorize(
        &self,
        operation: PluginOperation,
        id: &PluginId,
        version: &PluginVersion,
    ) -> Result<(), LifecycleError> {
        if matches!(
            operation,
            PluginOperation::Disable | PluginOperation::Remove
        ) {
            return Ok(());
        }
        self.generation
            .prepare_managed_declarative(&self.cache, id, version)
            .map(|_| ())
            .map_err(|_| LifecycleError::ManagedPolicyRejected {
                operation,
                id: id.as_str().to_owned(),
            })
    }
}

impl ManagedDeclarativePackage {
    /// Exact manifest admitted for activation.
    #[must_use]
    pub fn manifest(&self) -> &PluginManifest {
        self.package.manifest()
    }

    /// PL05 provenance re-established from a freshly rehashed cache object.
    #[must_use]
    pub const fn provenance(&self) -> &PackageProvenance {
        &self.provenance
    }

    /// Complete PL08 pre-activation decision report.
    #[must_use]
    pub const fn evaluation(&self) -> &ManagedPolicyEvaluation {
        &self.evaluation
    }
}

impl fmt::Debug for ManagedDeclarativePackage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedDeclarativePackage")
            .field("id", self.package.manifest().id())
            .field("version", self.package.manifest().version())
            .field("evaluation", &self.evaluation)
            .finish()
    }
}

impl PluginInstallCache {
    /// Install one fetched directory through PL05 plus immutable PL08 policy.
    ///
    /// Source bytes are fully validated and frozen first. Marketplace
    /// provenance and every policy axis are evaluated against that frozen
    /// manifest/tree hash before the first cache lock, content object or
    /// id/version reference is created. The exact frozen bytes then enter the
    /// unchanged PL02 no-clobber commit.
    ///
    /// # Errors
    /// Source/cache/PL05 failures, or the first denied/unknown policy axis.
    /// Every failure before commit leaves the target cache generation
    /// unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn install_managed_marketplace(
        &self,
        package_source: impl AsRef<Path>,
        marketplace_source: &MarketplaceSource,
        catalog: &MarketplaceCatalog,
        id: &PluginId,
        version: &PluginVersion,
        host: PlatformTarget,
        policy: &ManagedPluginPolicy,
    ) -> Result<ManagedInstalledPlugin, ManagedPluginError> {
        let prepared = self.prepare_directory(package_source.as_ref())?;
        self.commit_managed_prepared(
            prepared,
            marketplace_source,
            catalog,
            id,
            version,
            host,
            policy,
        )
    }

    /// Install only when PL07 and PL08 admit the same frozen package bytes.
    ///
    /// The exact source snapshot must equal one row of `graph` before managed
    /// policy/provenance checks and before PL02 publication. Dependency,
    /// conflict, platform or generation-drift refusal therefore leaves the
    /// cache unchanged.
    ///
    /// # Errors
    /// PL07 generation mismatch, PL08/PL05 refusal, or PL02 cache failure.
    #[allow(clippy::too_many_arguments)]
    pub fn install_resolved_managed_marketplace(
        &self,
        graph: &ResolvedPluginGraph,
        package_source: impl AsRef<Path>,
        marketplace_source: &MarketplaceSource,
        catalog: &MarketplaceCatalog,
        id: &PluginId,
        version: &PluginVersion,
        host: PlatformTarget,
        policy: &ManagedPluginPolicy,
    ) -> Result<ManagedInstalledPlugin, ManagedPluginError> {
        let prepared = self.prepare_directory(package_source.as_ref())?;
        graph.verify_manifest(&prepared.manifest)?;
        self.commit_managed_prepared(
            prepared,
            marketplace_source,
            catalog,
            id,
            version,
            host,
            policy,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_managed_prepared(
        &self,
        prepared: PreparedPackage,
        marketplace_source: &MarketplaceSource,
        catalog: &MarketplaceCatalog,
        id: &PluginId,
        version: &PluginVersion,
        host: PlatformTarget,
        policy: &ManagedPluginPolicy,
    ) -> Result<ManagedInstalledPlugin, ManagedPluginError> {
        let evaluation = policy.evaluate_marketplace(
            marketplace_source,
            catalog,
            id,
            version,
            &prepared.manifest,
            &prepared.content_hash,
            host,
        );
        evaluation.ensure_allowed()?;
        let candidate = candidate_receipt(&prepared);
        let provenance = catalog.admit(id, version, &candidate)?;
        let installed = self.commit_prepared(prepared)?;
        Ok(ManagedInstalledPlugin {
            installed,
            provenance,
            evaluation,
        })
    }

    /// Resolve, rehash and policy-admit one installed declarative package.
    ///
    /// PL02 resolution and PL05 provenance re-verification happen before the
    /// current policy is evaluated. Only an all-Allowed result permits bounded
    /// document loading, and only the opaque returned type can enter
    /// [`managed_declarative_activation_plugin`]. No host registry callback is
    /// made by this method.
    ///
    /// # Errors
    /// Cache/provenance/policy/document failure before activation.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_managed_declarative(
        &self,
        marketplace_source: &MarketplaceSource,
        catalog: &MarketplaceCatalog,
        id: &PluginId,
        version: &PluginVersion,
        host: PlatformTarget,
        policy: &ManagedPluginPolicy,
    ) -> Result<ManagedDeclarativePackage, ManagedPluginError> {
        let prepared = self.resolve_prepared(id, version)?;
        let evaluation = policy.evaluate_marketplace(
            marketplace_source,
            catalog,
            id,
            version,
            &prepared.manifest,
            &prepared.content_hash,
            host,
        );
        evaluation.ensure_allowed()?;
        let candidate = candidate_receipt(&prepared);
        let provenance = catalog.admit(id, version, &candidate)?;
        let package = DeclarativePackage::from_prepared(&prepared)?;
        Ok(ManagedDeclarativePackage {
            package,
            provenance,
            evaluation,
        })
    }
}

/// Build PL03's aggregate plugin only from current-policy-admitted packages.
///
/// Package and contribution collision checks remain the existing PL03/K09
/// transaction. This wrapper adds no alternate activation path: it merely
/// makes an unreviewed [`DeclarativePackage`] unrepresentable at the managed
/// constructor boundary.
///
/// # Errors
/// Duplicate active packages or contribution claims.
pub fn managed_declarative_activation_plugin(
    packages: Vec<ManagedDeclarativePackage>,
    host: Arc<dyn DeclarativeContributionHost>,
) -> Result<Box<dyn Plugin>, ManagedPluginError> {
    declarative_activation_plugin(
        packages
            .into_iter()
            .map(|package| package.package)
            .collect(),
        host,
    )
    .map_err(ManagedPluginError::from)
}

/// Construct managed PL03 activation in PL07 dependency order.
///
/// Every input already passed current PL05/PL08 admission. This final join
/// requires the complete exact PL07 generation and orders it before delegating
/// to the existing transactional activation bridge.
///
/// # Errors
/// Missing, duplicate, unexpected or drifted graph packages, or PL03
/// generation collisions.
pub fn resolved_managed_declarative_activation_plugin(
    graph: &ResolvedPluginGraph,
    packages: Vec<ManagedDeclarativePackage>,
    host: Arc<dyn DeclarativeContributionHost>,
) -> Result<Box<dyn Plugin>, ManagedPluginError> {
    let ordered = graph.order_values(packages, |package| package.manifest())?;
    managed_declarative_activation_plugin(ordered, host)
}

fn candidate_receipt(prepared: &PreparedPackage) -> InstalledPlugin {
    InstalledPlugin {
        manifest: prepared.manifest.clone(),
        content_hash: prepared.content_hash.clone(),
        package_root: std::path::PathBuf::new(),
        disposition: crate::InstallDisposition::AlreadyPresent,
    }
}

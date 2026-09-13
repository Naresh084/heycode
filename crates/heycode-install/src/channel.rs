//! Q15 release-channel and external plugin host-API admission.

use std::collections::BTreeSet;

use crate::error::invalid_field;
use crate::{
    AttestedReleaseManifest, ReleasePlatform, ReleaseSignatureVerifier, ReleaseVersion,
    UpdateError, VerifiedReleaseArtifact,
};

/// Update stream selected by the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseChannel {
    /// Automatic updates accept only newer non-prerelease versions.
    Stable,
    /// Automatic updates accept newer stable or prerelease versions.
    Preview,
    /// Only one exact version is eligible; it may intentionally be older than
    /// the current release.
    Pinned(ReleaseVersion),
}

/// Inclusive plugin host-API range declared by one enabled external plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginApiCompatibility {
    plugin: String,
    minimum: u32,
    maximum: u32,
}

impl PluginApiCompatibility {
    /// Validate one enabled plugin compatibility row.
    ///
    /// # Errors
    /// Invalid plugin ids, zero versions, or an inverted range are refused.
    pub fn new(plugin: &str, minimum: u32, maximum: u32) -> Result<Self, UpdateError> {
        if !valid_plugin_id(plugin) {
            return Err(invalid_field(
                "plugins.id",
                "must be one bounded namespaced plugin id",
            ));
        }
        if minimum == 0 || maximum == 0 || minimum > maximum {
            return Err(invalid_field(
                "plugins.api",
                "must be a positive inclusive range",
            ));
        }
        Ok(Self {
            plugin: plugin.to_owned(),
            minimum,
            maximum,
        })
    }

    /// Safe namespaced plugin id.
    #[must_use]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// Inclusive minimum compatible API.
    #[must_use]
    pub const fn minimum(&self) -> u32 {
        self.minimum
    }

    /// Inclusive maximum compatible API.
    #[must_use]
    pub const fn maximum(&self) -> u32 {
        self.maximum
    }
}

/// Deterministic snapshot of every enabled external plugin's API range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCompatibilitySet {
    plugins: Vec<PluginApiCompatibility>,
}

impl PluginCompatibilitySet {
    /// Empty set for an installation with no enabled external plugins.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Validate, sort, and freeze enabled plugin compatibility rows.
    ///
    /// # Errors
    /// Duplicate plugin ids are refused rather than resolved by input order.
    pub fn new(mut plugins: Vec<PluginApiCompatibility>) -> Result<Self, UpdateError> {
        plugins.sort_by(|left, right| left.plugin.cmp(&right.plugin));
        let mut seen = BTreeSet::new();
        if plugins
            .iter()
            .any(|plugin| !seen.insert(plugin.plugin.clone()))
        {
            return Err(invalid_field(
                "plugins",
                "contains a duplicate enabled plugin id",
            ));
        }
        Ok(Self { plugins })
    }

    /// Sorted compatibility rows.
    #[must_use]
    pub fn plugins(&self) -> &[PluginApiCompatibility] {
        &self.plugins
    }

    pub(crate) fn first_incompatible(&self, host_api: u32) -> Option<&PluginApiCompatibility> {
        self.plugins
            .iter()
            .find(|plugin| host_api < plugin.minimum || host_api > plugin.maximum)
    }
}

/// Why channel/API policy refused a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReleasePolicyRefusal {
    /// Stable policy never selects a prerelease.
    PreviewNotAllowed {
        /// Refused candidate.
        candidate: ReleaseVersion,
    },
    /// Automatic stable/preview policy never moves backward.
    NotNewer {
        /// Current installed version.
        current: ReleaseVersion,
        /// Candidate version.
        candidate: ReleaseVersion,
    },
    /// Candidate did not equal the selected pin.
    PinMismatch {
        /// Exact selected version.
        pinned: ReleaseVersion,
        /// Candidate version.
        candidate: ReleaseVersion,
    },
    /// An enabled plugin cannot run under the candidate host API.
    PluginApiIncompatible {
        /// Safe plugin id.
        plugin: String,
        /// Host API exposed by the candidate release.
        candidate_api: u32,
        /// Plugin's inclusive minimum.
        minimum: u32,
        /// Plugin's inclusive maximum.
        maximum: u32,
    },
}

/// Channel and plugin-API policy for one installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePolicy {
    channel: ReleaseChannel,
    plugins: PluginCompatibilitySet,
}

impl ReleasePolicy {
    /// Freeze one channel and enabled-plugin generation.
    #[must_use]
    pub const fn new(channel: ReleaseChannel, plugins: PluginCompatibilitySet) -> Self {
        Self { channel, plugins }
    }

    /// Evaluate policy for an installation with no current version.
    ///
    /// Fresh installation has no ordering comparison, but stable/prerelease,
    /// exact pin and enabled-plugin API policy still apply. Signature and
    /// artifact verification remain separate and mandatory.
    ///
    /// # Errors
    /// The selected channel or an enabled plugin refuses the candidate.
    pub fn evaluate_fresh(
        &self,
        candidate: &AttestedReleaseManifest,
    ) -> Result<(), ReleasePolicyRefusal> {
        let version = candidate.manifest().version();
        match &self.channel {
            ReleaseChannel::Stable if version.is_prerelease() => {
                return Err(ReleasePolicyRefusal::PreviewNotAllowed {
                    candidate: version.clone(),
                });
            }
            ReleaseChannel::Pinned(pinned) if version != pinned => {
                return Err(ReleasePolicyRefusal::PinMismatch {
                    pinned: pinned.clone(),
                    candidate: version.clone(),
                });
            }
            ReleaseChannel::Stable | ReleaseChannel::Preview | ReleaseChannel::Pinned(_) => {}
        }
        let candidate_api = candidate.manifest().plugin_api_version();
        if let Some(plugin) = self.plugins.first_incompatible(candidate_api) {
            return Err(ReleasePolicyRefusal::PluginApiIncompatible {
                plugin: plugin.plugin.clone(),
                candidate_api,
                minimum: plugin.minimum,
                maximum: plugin.maximum,
            });
        }
        Ok(())
    }

    /// Evaluate one already-authenticated release manifest.
    ///
    /// The returned approval borrows the exact manifest and is bound to
    /// `current`; artifact verification mints a non-forgeable update token that
    /// [`crate::InstallRoot`] independently compares with its durable state.
    #[must_use]
    pub fn evaluate<'a>(
        &'a self,
        current: &ReleaseVersion,
        candidate: &'a AttestedReleaseManifest,
    ) -> ReleasePolicyVerdict<'a> {
        let version = candidate.manifest().version();
        match &self.channel {
            ReleaseChannel::Stable if version.is_prerelease() => {
                return ReleasePolicyVerdict::Refused(ReleasePolicyRefusal::PreviewNotAllowed {
                    candidate: version.clone(),
                });
            }
            ReleaseChannel::Pinned(pinned) if version != pinned => {
                return ReleasePolicyVerdict::Refused(ReleasePolicyRefusal::PinMismatch {
                    pinned: pinned.clone(),
                    candidate: version.clone(),
                });
            }
            ReleaseChannel::Stable | ReleaseChannel::Preview if version == current => {
                return ReleasePolicyVerdict::Current;
            }
            ReleaseChannel::Stable | ReleaseChannel::Preview if version < current => {
                return ReleasePolicyVerdict::Refused(ReleasePolicyRefusal::NotNewer {
                    current: current.clone(),
                    candidate: version.clone(),
                });
            }
            ReleaseChannel::Pinned(_) if version == current => {
                return ReleasePolicyVerdict::Current;
            }
            _ => {}
        }
        let candidate_api = candidate.manifest().plugin_api_version();
        if let Some(plugin) = self.plugins.first_incompatible(candidate_api) {
            return ReleasePolicyVerdict::Refused(ReleasePolicyRefusal::PluginApiIncompatible {
                plugin: plugin.plugin.clone(),
                candidate_api,
                minimum: plugin.minimum,
                maximum: plugin.maximum,
            });
        }
        ReleasePolicyVerdict::Approved(ApprovedRelease {
            manifest: candidate,
            from: current.clone(),
        })
    }
}

/// Policy result for one release candidate.
pub enum ReleasePolicyVerdict<'a> {
    /// The candidate may be verified and installed from the observed current version.
    Approved(ApprovedRelease<'a>),
    /// Candidate is already current.
    Current,
    /// Candidate is not eligible.
    Refused(ReleasePolicyRefusal),
}

impl ReleasePolicyVerdict<'_> {
    /// Borrow the refusal when this verdict refused.
    #[must_use]
    pub const fn refusal(&self) -> Option<&ReleasePolicyRefusal> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            Self::Approved(_) | Self::Current => None,
        }
    }
}

/// Non-forgeable approval bound to one authenticated manifest and current version.
pub struct ApprovedRelease<'a> {
    manifest: &'a AttestedReleaseManifest,
    from: ReleaseVersion,
}

impl ApprovedRelease<'_> {
    /// Verify and bind one platform artifact to this update approval.
    ///
    /// # Errors
    /// Any artifact checksum, signature-bundle, or signer failure.
    pub fn verify_artifact(
        self,
        platform: &ReleasePlatform,
        bytes: Vec<u8>,
        bundle: &[u8],
        verifier: &dyn ReleaseSignatureVerifier,
    ) -> Result<VerifiedReleaseArtifact, UpdateError> {
        let mut artifact = self
            .manifest
            .verify_artifact(platform, bytes, bundle, verifier)?;
        artifact.update_approval = Some(UpdateApproval { from: self.from });
        Ok(artifact)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateApproval {
    pub(crate) from: ReleaseVersion,
}

fn valid_plugin_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 || value.trim() != value {
        return false;
    }
    let mut parts = value.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(namespace), Some(name), None)
            if valid_plugin_component(namespace) && valid_plugin_component(name)
    )
}

fn valid_plugin_component(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

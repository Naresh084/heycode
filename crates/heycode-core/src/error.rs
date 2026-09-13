//! Errors raised by the plugin system.

use thiserror::Error;

/// Failure modes of plugin composition and context access.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Two plugins claimed the same service key.
    #[error(
        "service `{key}` already provided by plugin `{existing}` (second claimant: `{claimant}`)"
    )]
    DuplicateService {
        /// The contested service key.
        key: String,
        /// Plugin that registered the key first.
        existing: String,
        /// Plugin that attempted the second registration.
        claimant: String,
    },

    /// A service key was requested that no plugin provided.
    #[error("missing service `{0}`")]
    MissingService(String),

    /// `compose` saw two plugins with the same name.
    #[error("duplicate plugin name `{0}`")]
    DuplicatePlugin(String),

    /// Transitional `name()` and descriptor id disagreed.
    #[error("plugin `{plugin}` reports descriptor id `{descriptor}`; the identities must match")]
    DescriptorIdMismatch {
        /// Legacy plugin name used by profiles today.
        plugin: String,
        /// Stable id declared by its descriptor.
        descriptor: String,
    },

    /// A plugin's declared injects were not satisfied at its apply time.
    #[error("plugin `{plugin}` requires missing services: {missing:?}")]
    UnsatisfiedInject {
        /// Plugin whose requirements failed.
        plugin: String,
        /// Service keys absent from the context.
        missing: Vec<String>,
    },

    /// Two plugins claimed one exact registry row.
    #[error(
        "{kind} contribution `{name}` already belongs to plugin `{existing}` (second claimant: `{claimant}`)"
    )]
    DuplicateContribution {
        /// Exact registry namespace.
        kind: String,
        /// Contested row name.
        name: String,
        /// First owner.
        existing: String,
        /// Second owner.
        claimant: String,
    },

    /// A contribution name was unsafe for diagnostics/lookup.
    #[error("plugin `{plugin}` declared invalid {kind} contribution `{name}`")]
    InvalidContribution {
        /// Declaring plugin.
        plugin: String,
        /// Exact registry namespace.
        kind: String,
        /// Rejected name.
        name: String,
    },

    /// A plugin declared an exact row outside its descriptor family.
    #[error(
        "plugin `{plugin}` declared exact contribution kind `{kind}` without descriptor family `{family}`"
    )]
    ContributionFamilyMismatch {
        /// Declaring plugin.
        plugin: String,
        /// Exact row kind.
        kind: String,
        /// Required broad family.
        family: String,
    },

    /// Exact contribution registration happened outside plugin apply.
    #[error("exact contribution registration requires an active plugin apply")]
    ContributionOutsideApply,

    /// Shared inventory state could not be read or updated.
    #[error("plugin contribution inventory is unavailable")]
    InventoryUnavailable,

    /// A failed activation left state its rollback could not remove.
    ///
    /// Core verifies every activation transaction instead of trusting it; this
    /// is the verification refusing to call a partial activation clean. The
    /// original failure is preserved in `cause`.
    #[error(
        "plugin `{plugin}` failed and its activation did not fully roll back ({residue}); original failure: {cause}"
    )]
    BrokenActivation {
        /// Plugin whose activation left residue.
        plugin: String,
        /// What survived the rollback.
        residue: String,
        /// The failure that triggered the rollback.
        cause: String,
    },

    /// A plugin-internal invariant failed during apply; wraps the message.
    #[error("plugin error: {0}")]
    Plugin(String),
}

impl CoreError {
    /// Wrap a plain message into [`CoreError::Plugin`].
    #[must_use]
    pub fn other(message: impl Into<String>) -> Self {
        Self::Plugin(message.into())
    }
}

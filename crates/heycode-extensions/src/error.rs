//! Stable, value-redacted manifest validation failures.

use thiserror::Error;

use crate::{ApiVersion, Architecture, ContributionKind, OperatingSystem, PluginPermission};

/// Errors returned while parsing or validating an external plugin manifest.
///
/// Free-form TOML values and parser diagnostics are deliberately absent so a
/// malformed manifest cannot reflect credential material into logs or UI.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestError {
    /// The input exceeded the stable v1 byte budget.
    #[error("plugin manifest exceeds the supported byte limit")]
    TooLarge,
    /// TOML syntax, shape, types, or unknown fields were invalid.
    #[error("plugin manifest TOML is malformed or contains unknown fields")]
    InvalidDocument,
    /// The document uses an unsupported manifest schema.
    #[error("unsupported plugin manifest schema {found}; supported schema is {supported}")]
    UnsupportedSchema {
        /// Version found in the document.
        found: u32,
        /// Exact schema understood by this validator.
        supported: u32,
    },
    /// One named field violated a stable validation rule.
    #[error("plugin manifest field `{field}` is invalid: {reason}")]
    InvalidField {
        /// Compile-time field path; never attacker-controlled text.
        field: &'static str,
        /// Compile-time safe explanation.
        reason: &'static str,
    },
    /// A set-like field contained a duplicate value.
    #[error("plugin manifest field `{field}` contains duplicate entries")]
    DuplicateField {
        /// Compile-time field path; never attacker-controlled text.
        field: &'static str,
    },
    /// The plugin cannot run against the selected heycode API version.
    #[error("plugin API range {minimum}..={maximum} does not include host API {host}")]
    IncompatibleApi {
        /// Current host API.
        host: ApiVersion,
        /// Plugin minimum API.
        minimum: ApiVersion,
        /// Plugin maximum API.
        maximum: ApiVersion,
    },
    /// The plugin does not list the selected host target.
    #[error("plugin does not support host platform {os}-{architecture}")]
    UnsupportedPlatform {
        /// Host operating system.
        os: OperatingSystem,
        /// Host architecture.
        architecture: Architecture,
    },
    /// A relationship listed the plugin itself or listed one id as both a
    /// dependency and conflict.
    #[error("plugin dependency/conflict relationships are inconsistent")]
    DependencyConflict,
    /// A contribution or authentication declaration needs an undeclared grant.
    #[error("plugin declaration `{required_by}` requires permission `{permission}`")]
    MissingPermission {
        /// Missing permission.
        permission: PluginPermission,
        /// Stable declaration class requiring it.
        required_by: &'static str,
    },
    /// A namespaced or override contribution conflicts with another claimant.
    #[error("{kind} contribution `{name}` collides with another contribution")]
    ContributionCollision {
        /// Exact contribution registry.
        kind: ContributionKind,
        /// Validated public contribution name.
        name: String,
    },
    /// An override was not explicitly admitted by the target registry.
    #[error("{kind} contribution override `{name}` is not allowed by the host registry")]
    OverrideNotAllowed {
        /// Exact contribution registry.
        kind: ContributionKind,
        /// Validated requested public name.
        name: String,
    },
    /// One atomic manifest batch contained the same package identity twice.
    #[error("plugin manifest batch contains a duplicate plugin id")]
    DuplicatePluginId,
}

pub(crate) const fn invalid(field: &'static str, reason: &'static str) -> ManifestError {
    ManifestError::InvalidField { field, reason }
}

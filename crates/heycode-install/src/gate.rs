//! The B03 gate: whether the configuration on disk permits a rollback.

use heycode_config::ConfigVersionState;

use crate::ReleaseVersion;

/// Why a rollback was refused.
///
/// One vocabulary shared by the pure configuration rule and the transition
/// that performs the swap, so the two can never describe the same refusal
/// differently.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RollbackRefusal {
    /// No prior version is recorded to return to.
    NothingToRollBackTo,
    /// The recorded prior version is no longer retained on disk.
    ///
    /// Loud rather than silent, exactly as PL06 refuses a plugin rollback to a
    /// version the cache has pruned: a dangling target that "succeeds" leaves
    /// an installation pointing at nothing.
    TargetNotRetained {
        /// The version the record names but the disk does not hold.
        version: ReleaseVersion,
    },
    /// The configuration was migrated by a binary newer than the target.
    ///
    /// The target would classify this document [`ConfigVersionState::Newer`]
    /// and refuse it as `ConfigError::NewerSchema` at its next start. Refusing
    /// before the swap converts a rollback that appears to succeed and then
    /// breaks into one actionable failure.
    ConfigurationTooNew {
        /// Schema version the configuration document declares.
        document: u32,
        /// Highest schema version the rollback target understands.
        target_supports: u32,
    },
    /// An enabled external plugin cannot run under the rollback target's host API.
    PluginApiIncompatible {
        /// Safe plugin id.
        plugin: String,
        /// Host API exposed by the rollback target.
        target_api: u32,
        /// Plugin's inclusive minimum.
        minimum: u32,
        /// Plugin's inclusive maximum.
        maximum: u32,
    },
}

impl std::fmt::Display for RollbackRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToRollBackTo => {
                formatter.write_str("no prior version is recorded to roll back to")
            }
            Self::TargetNotRetained { version } => {
                write!(formatter, "prior version `{version}` is no longer retained")
            }
            Self::ConfigurationTooNew {
                document,
                target_supports,
            } => write!(
                formatter,
                "configuration schema {document} is newer than the target understands \
                 ({target_supports})"
            ),
            Self::PluginApiIncompatible {
                plugin,
                target_api,
                minimum,
                maximum,
            } => write!(
                formatter,
                "plugin `{plugin}` requires host API {minimum}..={maximum}; target provides {target_api}"
            ),
        }
    }
}

/// Whether a rollback may proceed, and why not when it may not.
///
/// A verdict rather than a `Result`: a refused rollback is an expected outcome
/// an operator needs rendered, not an exceptional one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RollbackVerdict {
    /// Nothing in the observed state prevents the rollback.
    Permitted,
    /// The rollback must not proceed.
    Refused(RollbackRefusal),
}

/// Decide whether a configuration document permits rolling back to a binary
/// that understands schema `target_supports`.
///
/// The comparison is against the **target**, never against the binary doing
/// the classifying. [`ConfigVersionState`] is relative to whoever produced it,
/// so a document the running binary calls `Newer` may still be one the target
/// reads perfectly well.
///
/// [`ConfigVersionState::Unversioned`] permits the rollback: a document with
/// no marker predates the marker, so every binary that has ever shipped reads
/// it identically. Refusing there would block rollback for precisely the
/// oldest installations that most need one.
#[must_use]
pub fn rollback_config_verdict(
    document: ConfigVersionState,
    target_supports: u32,
) -> RollbackVerdict {
    let declared = match document {
        ConfigVersionState::Unversioned => return RollbackVerdict::Permitted,
        ConfigVersionState::Older(version)
        | ConfigVersionState::Current(version)
        | ConfigVersionState::Newer(version) => version,
    };
    if declared > target_supports {
        return RollbackVerdict::Refused(RollbackRefusal::ConfigurationTooNew {
            document: declared,
            target_supports,
        });
    }
    RollbackVerdict::Permitted
}

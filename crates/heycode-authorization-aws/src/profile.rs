//! Effective AWS profile name and where it came from.

use serde::Serialize;

use crate::AwsAuthError;

/// Environment variable that names the effective profile.
pub const AWS_PROFILE_VAR: &str = "AWS_PROFILE";
/// Profile the AWS SDKs use when nothing names one.
pub const AWS_DEFAULT_PROFILE: &str = "default";

/// Validated AWS profile name.
///
/// A profile name is a local alias for a block of configuration. It is safe to
/// report: it names *where* a credential is configured, never what it is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AwsProfileName(String);

impl AwsProfileName {
    /// Validate a profile name.
    ///
    /// # Errors
    /// Empty, over-64-byte, non-printable-ASCII, section-bracket-bearing and
    /// edge-whitespace names return [`AwsAuthError::InvalidProfileName`].
    pub fn new(value: impl Into<String>) -> Result<Self, AwsAuthError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (1..=64).contains(&bytes.len())
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
            && !value.contains(['[', ']'])
            && value.trim() == value;
        if valid {
            Ok(Self(value))
        } else {
            Err(AwsAuthError::InvalidProfileName)
        }
    }

    /// The SDK default profile.
    #[must_use]
    pub fn default_profile() -> Self {
        Self(AWS_DEFAULT_PROFILE.to_owned())
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which profile is in effect, and why.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum AwsProfileResolution {
    /// `AWS_PROFILE` named this profile.
    Environment {
        /// Effective profile.
        profile: AwsProfileName,
    },
    /// Nothing named a profile, so the SDK default applies.
    Default {
        /// Effective profile.
        profile: AwsProfileName,
    },
    /// `AWS_PROFILE` is set to a value that is not a usable profile name.
    ///
    /// This is not "the default profile applies": the SDKs would fail on the
    /// same value, and silently substituting `default` would report a profile
    /// that no request will use.
    Malformed,
}

impl AwsProfileResolution {
    /// Resolve the effective profile from the host environment.
    #[must_use]
    pub fn resolve(host: &dyn crate::AwsHost) -> Self {
        match host.var(AWS_PROFILE_VAR) {
            Some(value) => AwsProfileName::new(value)
                .map_or(Self::Malformed, |profile| Self::Environment { profile }),
            None => Self::Default {
                profile: AwsProfileName::default_profile(),
            },
        }
    }

    /// The effective profile, or `None` when configuration named an unusable
    /// one.
    #[must_use]
    pub const fn profile(&self) -> Option<&AwsProfileName> {
        match self {
            Self::Environment { profile } | Self::Default { profile } => Some(profile),
            Self::Malformed => None,
        }
    }
}

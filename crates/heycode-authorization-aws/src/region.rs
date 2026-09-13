//! Effective AWS region and where it came from.

use serde::Serialize;

use crate::ini::{self, SharedFile};
use crate::{AwsAuthError, AwsFileRead, AwsProfileName, AwsProfileResolution};

/// Environment variable the AWS SDKs read first for the region.
pub const AWS_REGION_VAR: &str = "AWS_REGION";
/// Older environment variable, read only when `AWS_REGION` is absent.
pub const AWS_DEFAULT_REGION_VAR: &str = "AWS_DEFAULT_REGION";

/// Validated AWS region id.
///
/// The id becomes a hostname label (`bedrock.{region}.amazonaws.com`), so the
/// character set is closed here rather than at each call site.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AwsRegion(String);

impl AwsRegion {
    /// Validate a region id.
    ///
    /// # Errors
    /// Ids outside 2..=64 bytes of `[a-z0-9-]` that start with a letter and
    /// end alphanumerically return [`AwsAuthError::InvalidRegion`].
    pub fn new(value: impl Into<String>) -> Result<Self, AwsAuthError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (2..=64).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        if valid {
            Ok(Self(value))
        } else {
            Err(AwsAuthError::InvalidRegion { value })
        }
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a region setting was found. Names the location, never a value.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum AwsRegionOrigin {
    /// An explicit coordinate persisted with the selected heycode connection.
    Connection,
    /// A process environment variable.
    Environment {
        /// Variable name.
        variable: &'static str,
    },
    /// The `region` setting of one profile in the shared config file.
    SharedConfigFile {
        /// Profile whose section carried the setting.
        profile: AwsProfileName,
    },
}

/// Which region is in effect, and why.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AwsRegionResolution {
    /// A well-formed region was found.
    Resolved {
        /// Effective region.
        region: AwsRegion,
        /// Where it came from.
        origin: AwsRegionOrigin,
    },
    /// A region was configured at this origin but is not a usable region id.
    ///
    /// The rejected text is deliberately not echoed; the origin is what a
    /// reader needs in order to repair it.
    Malformed {
        /// Where the unusable setting lives.
        origin: AwsRegionOrigin,
    },
    /// Nothing named a region.
    Unresolved,
    /// The shared config file exists but could not be read, so whether a
    /// region is configured is unknown. This is not "no region": the answer
    /// was not obtained, and treating it as absent would report a fact nobody
    /// established.
    Undetermined,
}

impl AwsRegionResolution {
    /// Resolve the effective region in documented precedence: `AWS_REGION`,
    /// then `AWS_DEFAULT_REGION`, then the effective profile's `region`.
    ///
    /// There is no built-in fallback region. Guessing one would turn "you have
    /// not configured AWS" into a request against somebody else's account
    /// boundary.
    #[must_use]
    pub fn resolve(host: &dyn crate::AwsHost, profile: &AwsProfileResolution) -> Self {
        for variable in [AWS_REGION_VAR, AWS_DEFAULT_REGION_VAR] {
            if let Some(value) = host.var(variable) {
                return Self::from_value(&value, AwsRegionOrigin::Environment { variable });
            }
        }
        let Some(profile) = profile.profile() else {
            return Self::Unresolved;
        };
        let text = match ini::read(host, SharedFile::Config) {
            AwsFileRead::Found(text) => text,
            AwsFileRead::Absent => return Self::Unresolved,
            AwsFileRead::Unreadable => return Self::Undetermined,
        };
        match ini::section_value(&text, profile.as_str(), SharedFile::Config, "region") {
            Some(value) => Self::from_value(
                &value,
                AwsRegionOrigin::SharedConfigFile {
                    profile: profile.clone(),
                },
            ),
            None => Self::Unresolved,
        }
    }

    /// The effective region, or `None` when none resolved.
    #[must_use]
    pub const fn region(&self) -> Option<&AwsRegion> {
        match self {
            Self::Resolved { region, .. } => Some(region),
            Self::Malformed { .. } | Self::Unresolved | Self::Undetermined => None,
        }
    }

    fn from_value(value: &str, origin: AwsRegionOrigin) -> Self {
        match AwsRegion::new(value) {
            Ok(region) => Self::Resolved { region, origin },
            Err(_) => Self::Malformed { origin },
        }
    }
}

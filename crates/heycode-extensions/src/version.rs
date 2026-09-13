//! Validated plugin and host API versions.

use std::cmp::Ordering;
use std::fmt;

use serde::Serialize;

use crate::ManifestError;
use crate::error::invalid;

/// Positive integer version of the stable heycode plugin host API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ApiVersion(u32);

impl ApiVersion {
    /// Validate a host API version.
    ///
    /// # Errors
    /// Version zero is reserved and rejected.
    pub const fn new(value: u32) -> Result<Self, ManifestError> {
        if value == 0 {
            Err(invalid("api_version", "must be greater than zero"))
        } else {
            Ok(Self(value))
        }
    }

    /// Numeric protocol version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PrereleaseIdentifier {
    Numeric(u64),
    Text(String),
}

impl Ord for PrereleaseIdentifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Numeric(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for PrereleaseIdentifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Strict Semantic Versioning 2.0 package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PluginVersion {
    raw: String,
    #[serde(skip)]
    major: u64,
    #[serde(skip)]
    minor: u64,
    #[serde(skip)]
    patch: u64,
    #[serde(skip)]
    prerelease: Option<Vec<PrereleaseIdentifier>>,
}

impl PluginVersion {
    /// Parse a strict SemVer 2.0 string.
    ///
    /// # Errors
    /// Missing core fields, numeric overflow, leading zeroes, empty
    /// identifiers, and invalid identifier characters are rejected.
    pub fn parse(value: impl Into<String>) -> Result<Self, ManifestError> {
        let raw = value.into();
        let parsed = parse_semver(&raw)
            .ok_or_else(|| invalid("version", "must be a valid Semantic Versioning 2.0 value"))?;
        Ok(Self {
            raw,
            major: parsed.0,
            minor: parsed.1,
            patch: parsed.2,
            prerelease: parsed.3,
        })
    }

    /// Original validated version text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Compare SemVer precedence while deliberately ignoring build metadata.
    #[must_use]
    pub fn precedence_cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| match (&self.prerelease, &other.prerelease) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            })
    }
}

impl fmt::Display for PluginVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

type ParsedSemver = (u64, u64, u64, Option<Vec<PrereleaseIdentifier>>);

fn parse_semver(raw: &str) -> Option<ParsedSemver> {
    if raw.is_empty() || raw.trim() != raw || raw.len() > 128 || raw.chars().any(char::is_control) {
        return None;
    }
    let (without_build, build) = match raw.split_once('+') {
        Some((left, right)) if !right.contains('+') && valid_identifiers(right, false) => {
            (left, Some(right))
        }
        Some(_) => return None,
        None => (raw, None),
    };
    let _ = build;
    let (core, prerelease_raw) = match without_build.split_once('-') {
        Some((left, right)) => (left, Some(right)),
        None => (without_build, None),
    };
    let mut core_parts = core.split('.');
    let major = parse_core_number(core_parts.next()?)?;
    let minor = parse_core_number(core_parts.next()?)?;
    let patch = parse_core_number(core_parts.next()?)?;
    if core_parts.next().is_some() {
        return None;
    }
    let prerelease = match prerelease_raw {
        Some(value) => Some(parse_prerelease(value)?),
        None => None,
    };
    Some((major, minor, patch, prerelease))
}

fn parse_core_number(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn valid_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    if value.is_empty() {
        return false;
    }
    value.split('.').all(|identifier| {
        !identifier.is_empty()
            && identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !(reject_numeric_leading_zero
                && identifier.len() > 1
                && identifier.bytes().all(|byte| byte.is_ascii_digit())
                && identifier.starts_with('0'))
    })
}

fn parse_prerelease(value: &str) -> Option<Vec<PrereleaseIdentifier>> {
    if !valid_identifiers(value, true) {
        return None;
    }
    value
        .split('.')
        .map(|identifier| {
            if identifier.bytes().all(|byte| byte.is_ascii_digit()) {
                identifier.parse().ok().map(PrereleaseIdentifier::Numeric)
            } else {
                Some(PrereleaseIdentifier::Text(identifier.to_owned()))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::PluginVersion;

    #[test]
    fn semver_precedence_ignores_build_and_orders_prerelease() {
        let alpha = PluginVersion::parse("1.0.0-alpha.1").unwrap();
        let beta = PluginVersion::parse("1.0.0-beta").unwrap();
        let release = PluginVersion::parse("1.0.0+build.7").unwrap();
        let release_other = PluginVersion::parse("1.0.0+build.9").unwrap();
        assert_eq!(alpha.precedence_cmp(&beta), std::cmp::Ordering::Less);
        assert_eq!(beta.precedence_cmp(&release), std::cmp::Ordering::Less);
        assert_ne!(release, release_other);
        assert_eq!(
            release.precedence_cmp(&release_other),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn semver_rejects_invalid_core_and_identifiers() {
        for value in [
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-",
            "1.2.3-alpha..one",
            "1.2.3-01",
            "1.2.3+",
            "1.2.3+a?",
        ] {
            assert!(PluginVersion::parse(value).is_err(), "accepted {value}");
        }
    }
}

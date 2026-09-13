//! Strict Claude Code version detection and compatibility policy.

use std::fmt::{Display, Formatter};

use crate::ClaudeRuntimeConfigError;

/// Parsed stable Claude Code CLI version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaudeCliVersion {
    major: u16,
    minor: u16,
    patch: u16,
}

impl ClaudeCliVersion {
    /// Construct an exact three-component version.
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Major component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Minor component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Patch component.
    #[must_use]
    pub const fn patch(self) -> u16 {
        self.patch
    }
}

impl Display for ClaudeCliVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Closed compatibility interval for the installed Claude Code CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeVersionPolicy {
    minimum: ClaudeCliVersion,
    maximum_exclusive: ClaudeCliVersion,
}

impl ClaudeVersionPolicy {
    /// Validate a half-open compatible version interval.
    ///
    /// # Errors
    /// Empty or reversed intervals are rejected.
    pub fn new(
        minimum: ClaudeCliVersion,
        maximum_exclusive: ClaudeCliVersion,
    ) -> Result<Self, ClaudeRuntimeConfigError> {
        if minimum >= maximum_exclusive {
            return Err(ClaudeRuntimeConfigError::InvalidVersionPolicy);
        }
        Ok(Self {
            minimum,
            maximum_exclusive,
        })
    }

    /// Policy validated against the R07 evidence baseline.
    ///
    /// Claude Code 2.1.241 is the first locally certified release for this
    /// process contract. A new major requires an explicit compatibility pass.
    #[must_use]
    pub const fn supported() -> Self {
        Self {
            minimum: ClaudeCliVersion::new(2, 1, 241),
            maximum_exclusive: ClaudeCliVersion::new(3, 0, 0),
        }
    }

    /// Whether `version` lies inside the half-open interval.
    #[must_use]
    pub const fn accepts(self, version: ClaudeCliVersion) -> bool {
        let at_least_minimum = version.major > self.minimum.major
            || (version.major == self.minimum.major
                && (version.minor > self.minimum.minor
                    || (version.minor == self.minimum.minor
                        && version.patch >= self.minimum.patch)));
        let before_maximum = version.major < self.maximum_exclusive.major
            || (version.major == self.maximum_exclusive.major
                && (version.minor < self.maximum_exclusive.minor
                    || (version.minor == self.maximum_exclusive.minor
                        && version.patch < self.maximum_exclusive.patch)));
        at_least_minimum && before_maximum
    }

    /// Inclusive lower bound.
    #[must_use]
    pub const fn minimum(self) -> ClaudeCliVersion {
        self.minimum
    }

    /// Exclusive upper bound.
    #[must_use]
    pub const fn maximum_exclusive(self) -> ClaudeCliVersion {
        self.maximum_exclusive
    }
}

pub(crate) fn parse_version_output(bytes: &[u8]) -> Option<ClaudeCliVersion> {
    if bytes.len() > 128 {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?.trim();
    let numeric = text.strip_suffix(" (Claude Code)")?;
    let mut parts = numeric.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    let patch = parse_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(ClaudeCliVersion::new(major, minor, patch))
}

fn parse_component(value: &str) -> Option<u16> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_the_product_suffix_and_exact_three_part_version() {
        assert_eq!(
            parse_version_output(b"2.1.243 (Claude Code)\n"),
            Some(ClaudeCliVersion::new(2, 1, 243))
        );
        for invalid in [
            b"2.1.243".as_slice(),
            b"2.1 (Claude Code)",
            b"2.1.2.3 (Claude Code)",
            b"2.x.243 (Claude Code)",
            b"2.1.243 (Other)",
        ] {
            assert_eq!(parse_version_output(invalid), None);
        }
    }

    #[test]
    fn arbitrary_policy_rejects_empty_intervals() {
        let version = ClaudeCliVersion::new(2, 1, 243);
        assert_eq!(
            ClaudeVersionPolicy::new(version, version),
            Err(ClaudeRuntimeConfigError::InvalidVersionPolicy)
        );
    }
}

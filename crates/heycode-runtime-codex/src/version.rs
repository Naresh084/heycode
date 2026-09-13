//! Exact Codex CLI version parsing and compatibility pin.

use std::fmt::{Display, Formatter};

use crate::{CodexAppServerError, CodexAppServerErrorCode};

/// Codex CLI release whose generated app-server schema R03 implements.
pub const SUPPORTED_CODEX_CLI_VERSION: &str = "0.153.2";

/// Parsed numeric Codex CLI release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodexCliVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl CodexCliVersion {
    pub(crate) fn parse_output(output: &[u8]) -> Result<Self, CodexAppServerError> {
        let output = std::str::from_utf8(output)
            .map_err(|_| CodexAppServerError::new(CodexAppServerErrorCode::UnsupportedVersion))?;
        let line = output
            .strip_suffix('\n')
            .unwrap_or(output)
            .strip_suffix('\r')
            .unwrap_or_else(|| output.strip_suffix('\n').unwrap_or(output));
        let mut fields = line.split_ascii_whitespace();
        if fields.next() != Some("codex-cli") {
            return Err(CodexAppServerError::new(
                CodexAppServerErrorCode::UnsupportedVersion,
            ));
        }
        let version = fields
            .next()
            .ok_or_else(|| CodexAppServerError::new(CodexAppServerErrorCode::UnsupportedVersion))?;
        if fields.next().is_some() {
            return Err(CodexAppServerError::new(
                CodexAppServerErrorCode::UnsupportedVersion,
            ));
        }
        Self::parse(version)
    }

    pub(crate) fn parse(value: &str) -> Result<Self, CodexAppServerError> {
        let mut components = value.split('.');
        let major = parse_component(components.next())?;
        let minor = parse_component(components.next())?;
        let patch = parse_component(components.next())?;
        if components.next().is_some() {
            return unsupported();
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    pub(crate) fn ensure_supported(self) -> Result<Self, CodexAppServerError> {
        if self.to_string() == SUPPORTED_CODEX_CLI_VERSION {
            Ok(self)
        } else {
            unsupported()
        }
    }

    pub(crate) fn appears_in_user_agent(self, user_agent: &str) -> bool {
        let expected = self.to_string();
        user_agent.split_ascii_whitespace().any(|component| {
            component
                .rsplit_once('/')
                .is_some_and(|(_, version)| version == expected)
        })
    }

    /// Major release component.
    #[must_use]
    pub const fn major(self) -> u64 {
        self.major
    }

    /// Minor release component.
    #[must_use]
    pub const fn minor(self) -> u64 {
        self.minor
    }

    /// Patch release component.
    #[must_use]
    pub const fn patch(self) -> u64 {
        self.patch
    }
}

impl Display for CodexCliVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_component(value: Option<&str>) -> Result<u64, CodexAppServerError> {
    let value = value
        .ok_or_else(|| CodexAppServerError::new(CodexAppServerErrorCode::UnsupportedVersion))?;
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return unsupported();
    }
    value
        .parse()
        .map_err(|_| CodexAppServerError::new(CodexAppServerErrorCode::UnsupportedVersion))
}

fn unsupported<T>() -> Result<T, CodexAppServerError> {
    Err(CodexAppServerError::new(
        CodexAppServerErrorCode::UnsupportedVersion,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn reviewed_installed_release_is_supported_and_unreviewed_release_is_rejected() {
        assert!(
            CodexCliVersion::parse("0.153.2")
                .unwrap()
                .ensure_supported()
                .is_ok()
        );
        assert!(
            CodexCliVersion::parse("0.153.3")
                .unwrap()
                .ensure_supported()
                .is_err()
        );
    }

    #[test]
    fn exact_version_output_is_strict_and_body_free() {
        let version = CodexCliVersion::parse_output(b"codex-cli 0.153.2\n").unwrap();
        assert_eq!(version.to_string(), SUPPORTED_CODEX_CLI_VERSION);
        assert_eq!(
            CodexCliVersion::parse_output(b"codex-cli 0.153.2\r\n")
                .unwrap()
                .to_string(),
            SUPPORTED_CODEX_CLI_VERSION
        );
        for invalid in [
            b"codex 0.153.2".as_slice(),
            b"codex-cli 0.153.2 extra",
            b"codex-cli 00.153.2",
            b"codex-cli 0.146",
            b"codex-cli 0.153.2\nextra",
        ] {
            let error = CodexCliVersion::parse_output(invalid).unwrap_err();
            assert_eq!(error.code(), CodexAppServerErrorCode::UnsupportedVersion);
            assert!(!error.to_string().contains("extra"));
        }
    }
}

//! Validated OpenCode process and catalog configuration.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt::{Debug, Formatter};
use std::path::{Path, PathBuf};

use heycode_runtime::RuntimeContractError;

const ALLOWED_ENVIRONMENT_NAMES: &[&str] = &[
    "APPDATA",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOCALAPPDATA",
    "LOGNAME",
    "PROGRAMDATA",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USER",
    "USERPROFILE",
    "WINDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "__CF_USER_TEXT_ENCODING",
];

/// Capture the minimum non-credential environment used by installed OpenCode.
///
/// Home/XDG paths permit OpenCode to use its own account/config stores. API
/// keys, token variables, proxy URLs, interpreter search paths, plugin controls
/// and arbitrary process variables are excluded by construction. Rows are
/// name-sorted.
#[must_use]
pub fn opencode_environment_snapshot() -> Vec<(OsString, OsString)> {
    opencode_environment_from(std::env::vars_os())
}

fn opencode_environment_from<I>(values: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut retained = BTreeMap::new();
    for (name, value) in values {
        let Some(text) = name.to_str() else {
            continue;
        };
        let normalized = text.to_ascii_uppercase();
        if ALLOWED_ENVIRONMENT_NAMES
            .iter()
            .any(|allowed| allowed.to_ascii_uppercase() == normalized)
        {
            retained.insert(normalized, (name, value));
        }
    }
    retained.into_values().collect()
}

/// Operation-time OpenCode discovery and explicit child-environment policy.
#[derive(Clone)]
pub struct OpenCodeRuntimeConfig {
    program: OsString,
    catalog_workspace: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

impl OpenCodeRuntimeConfig {
    /// Construct the default `opencode` discovery configuration.
    ///
    /// # Errors
    /// The catalog workspace must be an absolute, NUL-free path.
    pub fn new(catalog_workspace: impl Into<PathBuf>) -> Result<Self, RuntimeContractError> {
        let catalog_workspace = catalog_workspace.into();
        if !catalog_workspace.is_absolute() || contains_nul(catalog_workspace.as_os_str()) {
            return Err(invalid(
                "OpenCode catalog workspace",
                "an absolute NUL-free path",
            ));
        }
        Ok(Self {
            program: OsString::from("opencode"),
            catalog_workspace,
            environment: opencode_environment_snapshot(),
        })
    }

    /// Replace the executable name or path resolved by the subprocess Provider.
    ///
    /// # Errors
    /// Empty, NUL-bearing, or over-4096-byte values are rejected.
    pub fn with_program(
        mut self,
        program: impl Into<OsString>,
    ) -> Result<Self, RuntimeContractError> {
        let program = program.into();
        if program.is_empty() || contains_nul(&program) || os_len(&program) > 4096 {
            return Err(invalid("OpenCode program", "1..=4096 NUL-free bytes"));
        }
        self.program = program;
        Ok(self)
    }

    /// Replace the complete child environment used by version and ACP processes.
    ///
    /// No ambient variable is inherited by this boundary.
    ///
    /// # Errors
    /// More than 128 entries, duplicate/empty/`=`-bearing names, NULs, or a
    /// combined environment larger than 1 MiB are rejected.
    pub fn with_environment(
        mut self,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<Self, RuntimeContractError> {
        let mut names = BTreeSet::new();
        let invalid_shape = environment.len() > 128
            || environment.iter().any(|(name, value)| {
                name.is_empty()
                    || contains_nul(name)
                    || contains_nul(value)
                    || name.to_string_lossy().contains('=')
                    || !names.insert(name.clone())
            });
        let total = environment
            .iter()
            .map(|(name, value)| os_len(name).saturating_add(os_len(value)))
            .sum::<usize>();
        if invalid_shape || total > 1024 * 1024 {
            return Err(invalid(
                "OpenCode environment",
                "at most 128 unique explicit entries and 1 MiB",
            ));
        }
        self.environment = environment;
        Ok(self)
    }

    pub(crate) fn program(&self) -> &OsStr {
        &self.program
    }

    pub(crate) fn catalog_workspace(&self) -> &Path {
        &self.catalog_workspace
    }

    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }
}

impl Debug for OpenCodeRuntimeConfig {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenCodeRuntimeConfig")
            .field("program", &"<redacted>")
            .field("catalog_workspace", &"<redacted>")
            .field("environment_count", &self.environment.len())
            .finish()
    }
}

fn contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

fn os_len(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

fn invalid(field: &'static str, requirement: &'static str) -> RuntimeContractError {
    RuntimeContractError::InvalidField { field, requirement }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn installed_environment_is_sorted_and_credential_blind() {
        let environment = opencode_environment_from([
            (OsString::from("PATH"), OsString::from("/safe/bin")),
            (OsString::from("HOME"), OsString::from("/safe/home")),
            (
                OsString::from("XDG_DATA_HOME"),
                OsString::from("/safe/data"),
            ),
            (
                OsString::from("OPENROUTER_API_KEY"),
                OsString::from("private-key-canary"),
            ),
            (
                OsString::from("HTTPS_PROXY"),
                OsString::from("https://private-proxy-canary"),
            ),
            (
                OsString::from("OPENCODE_PLUGIN"),
                OsString::from("private-plugin-canary"),
            ),
        ]);
        let names = environment
            .iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["HOME", "XDG_DATA_HOME"]);
        let debug = format!("{environment:?}");
        assert!(!debug.contains("private-key-canary"));
        assert!(!debug.contains("private-proxy-canary"));
        assert!(!debug.contains("private-plugin-canary"));
    }
}

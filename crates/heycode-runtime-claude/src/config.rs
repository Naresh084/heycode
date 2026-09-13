//! Exact program, environment, workspace and version configuration.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::{ClaudeRuntimeConfigError, ClaudeVersionPolicy};

const MAX_PROGRAM_BYTES: usize = 4096;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_ENVIRONMENT_BYTES: usize = 1024 * 1024;

const ALLOWED_ENVIRONMENT_NAMES: &[&str] = &[
    "APPDATA",
    "CLAUDE_CONFIG_DIR",
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
    "__CF_USER_TEXT_ENCODING",
];

/// Caller configuration before the subprocess provider resolves the executable.
#[derive(Clone)]
pub struct ClaudeRuntimeConfig {
    program: OsString,
    cwd: PathBuf,
    pub(crate) discovery_workspace:
        Option<std::sync::Arc<heycode_runtime::RuntimeDiscoveryWorkspace>>,
    environment: Vec<(OsString, OsString)>,
    version_policy: ClaudeVersionPolicy,
}

impl ClaudeRuntimeConfig {
    /// Create the installed-runtime configuration for one absolute working directory.
    ///
    /// The default process environment is an explicit non-credential allowlist.
    /// API keys, OAuth-token variables, proxy URLs, hooks and inherited provider
    /// selectors are excluded by construction.
    ///
    /// # Errors
    /// Relative working directories are rejected.
    pub fn new(cwd: impl Into<PathBuf>) -> Result<Self, ClaudeRuntimeConfigError> {
        let cwd = cwd.into();
        validate_cwd(&cwd)?;
        Ok(Self {
            program: OsString::from("claude"),
            cwd,
            discovery_workspace: None,
            environment: claude_environment_snapshot(),
            version_policy: ClaudeVersionPolicy::supported(),
        })
    }

    /// Override the executable name or absolute path resolved by subprocess.
    ///
    /// # Errors
    /// Empty, NUL-bearing or oversized values are rejected.
    pub fn with_program(
        mut self,
        program: impl Into<OsString>,
    ) -> Result<Self, ClaudeRuntimeConfigError> {
        let program = program.into();
        validate_program(&program)?;
        self.program = program;
        Ok(self)
    }

    /// Replace the complete explicit subprocess environment.
    ///
    /// This is intended for trusted embedding and deterministic fixtures. The
    /// normal constructor uses [`claude_environment_snapshot`].
    ///
    /// # Errors
    /// Invalid, duplicate, NUL-bearing or over-limit entries are rejected.
    pub fn with_environment<I, K, V>(mut self, values: I) -> Result<Self, ClaudeRuntimeConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let values = values
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<Vec<_>>();
        validate_environment(&values)?;
        self.environment = values;
        Ok(self)
    }

    /// Replace the accepted CLI version interval.
    #[must_use]
    pub const fn with_version_policy(mut self, policy: ClaudeVersionPolicy) -> Self {
        self.version_policy = policy;
        self
    }

    pub(crate) fn program(&self) -> &OsStr {
        &self.program
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    pub(crate) const fn version_policy(&self) -> ClaudeVersionPolicy {
        self.version_policy
    }
}

impl std::fmt::Debug for ClaudeRuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClaudeRuntimeConfig")
            .field("program", &"<redacted>")
            .field("cwd", &"<redacted>")
            .field("environment_count", &self.environment.len())
            .field("version_policy", &self.version_policy)
            .finish()
    }
}

/// Capture the minimum non-credential environment needed by the native CLI.
///
/// The result is name-sorted and includes `NO_COLOR=1`, the official
/// `CLAUDE_CODE_SKIP_PROMPT_HISTORY=1` no-persistence backstop, and a constructed
/// system-only `PATH` so Claude's shell can resolve standard operating-system
/// commands. It does not inherit the ambient `PATH`, `ANTHROPIC_API_KEY`,
/// `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, proxy URLs, hook controls,
/// or arbitrary `CLAUDE_CODE_*` variables. Subscription credentials remain owned
/// and read by Claude Code through its official store.
#[must_use]
pub fn claude_environment_snapshot() -> Vec<(OsString, OsString)> {
    let mut retained = BTreeMap::new();
    for (name, value) in std::env::vars_os() {
        let Some(text) = name.to_str() else {
            continue;
        };
        let normalized = normalize_environment_name(text);
        if ALLOWED_ENVIRONMENT_NAMES
            .iter()
            .any(|allowed| normalize_environment_name(allowed) == normalized)
        {
            retained.insert(normalized, (name, value));
        }
    }
    retained.insert(
        normalize_environment_name("CLAUDE_CODE_SKIP_PROMPT_HISTORY"),
        (
            OsString::from("CLAUDE_CODE_SKIP_PROMPT_HISTORY"),
            OsString::from("1"),
        ),
    );
    retained.insert(
        normalize_environment_name("NO_COLOR"),
        (OsString::from("NO_COLOR"), OsString::from("1")),
    );
    retained.insert(
        normalize_environment_name("PATH"),
        (OsString::from("PATH"), trusted_system_path()),
    );
    retained.into_values().collect()
}

#[cfg(not(windows))]
fn trusted_system_path() -> OsString {
    OsString::from("/usr/bin:/bin:/usr/sbin:/sbin")
}

#[cfg(windows)]
fn trusted_system_path() -> OsString {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .unwrap_or_else(|| OsString::from(r"C:\Windows"));
    let root = PathBuf::from(root);
    std::env::join_paths([
        root.join("System32"),
        root.clone(),
        root.join("System32").join("Wbem"),
        root.join("System32").join("WindowsPowerShell").join("v1.0"),
    ])
    .unwrap_or_else(|_| OsString::from(r"C:\Windows\System32;C:\Windows"))
}

fn validate_cwd(cwd: &Path) -> Result<(), ClaudeRuntimeConfigError> {
    if cwd.is_absolute() && !contains_nul(cwd.as_os_str()) {
        Ok(())
    } else {
        Err(ClaudeRuntimeConfigError::InvalidWorkingDirectory)
    }
}

fn validate_program(program: &OsStr) -> Result<(), ClaudeRuntimeConfigError> {
    if program.is_empty()
        || contains_nul(program)
        || program.as_encoded_bytes().len() > MAX_PROGRAM_BYTES
    {
        Err(ClaudeRuntimeConfigError::InvalidProgram)
    } else {
        Ok(())
    }
}

fn validate_environment(values: &[(OsString, OsString)]) -> Result<(), ClaudeRuntimeConfigError> {
    if values.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(ClaudeRuntimeConfigError::InvalidEnvironment);
    }
    let mut names = BTreeMap::new();
    let mut bytes = 0usize;
    for (name, value) in values {
        let Some(name_text) = name.to_str() else {
            return Err(ClaudeRuntimeConfigError::InvalidEnvironment);
        };
        if name_text.is_empty()
            || name_text.contains('=')
            || contains_nul(name)
            || contains_nul(value)
            || names
                .insert(normalize_environment_name(name_text), ())
                .is_some()
        {
            return Err(ClaudeRuntimeConfigError::InvalidEnvironment);
        }
        bytes = bytes
            .saturating_add(name.as_encoded_bytes().len())
            .saturating_add(value.as_encoded_bytes().len());
    }
    if bytes > MAX_ENVIRONMENT_BYTES {
        Err(ClaudeRuntimeConfigError::InvalidEnvironment)
    } else {
        Ok(())
    }
}

fn contains_nul(value: &OsStr) -> bool {
    value.as_encoded_bytes().contains(&0)
}

fn normalize_environment_name(value: &str) -> String {
    if cfg!(windows) {
        value.to_ascii_uppercase()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn default_environment_is_sorted_and_excludes_all_credential_names() {
        let environment = claude_environment_snapshot();
        let names = environment
            .iter()
            .map(|(name, _)| name.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let mut sorted = names.clone();
        sorted.sort_by_key(|name| normalize_environment_name(name));
        assert_eq!(names, sorted);
        assert!(names.iter().any(|name| name == "NO_COLOR"));
        assert!(
            names
                .iter()
                .any(|name| name == "CLAUDE_CODE_SKIP_PROMPT_HISTORY")
        );
        let path = environment
            .iter()
            .find(|(name, _)| name.to_string_lossy().eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .expect("constructed PATH");
        assert_eq!(path, trusted_system_path().to_string_lossy());
        let ambient_path = std::env::var_os("PATH");
        if ambient_path.as_deref() != Some(OsStr::new(&path)) {
            assert_ne!(Some(OsString::from(&path)), ambient_path);
        }
        for name in names {
            let upper = name.to_ascii_uppercase();
            assert!(!upper.contains("KEY"), "{name}");
            assert!(!upper.contains("TOKEN"), "{name}");
            assert!(!upper.contains("SECRET"), "{name}");
            assert!(!upper.contains("PASSWORD"), "{name}");
        }
    }

    #[test]
    fn debug_redacts_program_cwd_and_environment_values() {
        let config = ClaudeRuntimeConfig::new(std::env::current_dir().unwrap())
            .unwrap()
            .with_program("private-program-canary")
            .unwrap()
            .with_environment([("VISIBLE", "private-environment-canary")])
            .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("private-program-canary"));
        assert!(!debug.contains("private-environment-canary"));
        assert!(!debug.contains(std::env::current_dir().unwrap().to_string_lossy().as_ref()));
    }
}

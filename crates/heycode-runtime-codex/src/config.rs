//! Explicit request-to-launch resolution for Codex app-server.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use heycode_exec::{ProcessSpec, SubprocessService};

use crate::launch::{BoundLaunch, LaunchIdentity};
use crate::{CodexAppServerError, CodexAppServerErrorCode};

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const VERSION_OUTPUT_LIMIT: usize = 4 * 1024;
pub(crate) const WIRE_LINE_LIMIT: usize = 1024 * 1024;

const ALLOWED_ENVIRONMENT_NAMES: &[&str] = &[
    "APPDATA",
    "CODEX_HOME",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOCALAPPDATA",
    "LOGNAME",
    "PATH",
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
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "__CF_USER_TEXT_ENCODING",
];

/// Capture the minimum non-credential environment required by an installed
/// Codex CLI and its official subscription store.
///
/// The result is name-sorted. `PATH` is retained for packaged
/// `/usr/bin/env node` launchers, then canonicalized and safety-checked during
/// resolution. `CODEX_HOME` and platform home variables let the CLI locate its
/// official authentication state. API keys, OAuth token variables, proxy
/// URLs, hooks, and arbitrary runtime controls are excluded by construction.
#[must_use]
pub fn codex_environment_snapshot() -> Vec<(OsString, OsString)> {
    codex_environment_from(std::env::vars_os())
}

fn codex_environment_from<I>(values: I) -> Vec<(OsString, OsString)>
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

/// Safe client identity sent during the mandatory initialize handshake.
#[derive(Clone, PartialEq, Eq)]
pub struct CodexClientInfo {
    name: String,
    title: String,
    version: String,
}

impl CodexClientInfo {
    /// Validate exact client identity fields.
    ///
    /// # Errors
    /// Names must be lowercase identifier text; title/version must be bounded,
    /// trimmed and control-free.
    pub fn new(
        name: impl Into<String>,
        title: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, CodexAppServerError> {
        let name = name.into();
        let title = title.into();
        let version = version.into();
        let valid_name = !name.is_empty()
            && name.len() <= 64
            && name.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || (matches!(byte, b'_' | b'-') && index > 0)
            })
            && name
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric);
        if !valid_name || !valid_one_line(&title, 128) || !valid_one_line(&version, 64) {
            return Err(CodexAppServerError::new(
                CodexAppServerErrorCode::InvalidConfig,
            ));
        }
        Ok(Self {
            name,
            title,
            version,
        })
    }

    /// Stable compliance/client identifier.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Human client title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Client implementation version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl std::fmt::Debug for CodexClientInfo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexClientInfo")
            .field("name", &self.name)
            .field("title_bytes", &self.title.len())
            .field("version", &self.version)
            .finish()
    }
}

/// Unresolved Codex executable, workspace, environment and client identity.
///
/// The environment is complete: the subprocess Provider clears inheritance
/// and only these explicit pairs reach both version and app-server processes.
#[derive(Clone)]
pub struct CodexAppServerConfig {
    program: OsString,
    cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
    client_info: CodexClientInfo,
    discovery_workspace: Option<std::sync::Arc<heycode_runtime::RuntimeDiscoveryWorkspace>>,
    pub(crate) outer_sandbox_mode: heycode_exec::SandboxMode,
}

impl CodexAppServerConfig {
    /// Capture caller intent without resolving the executable or process spec.
    ///
    /// # Errors
    /// Empty executable names and relative/NUL-bearing workspaces fail.
    /// Environment validation happens once in `resolve`, owned by the active
    /// subprocess Provider.
    pub fn new<I, K, V>(
        program: impl AsRef<OsStr>,
        cwd: impl AsRef<Path>,
        environment: I,
        client_info: CodexClientInfo,
    ) -> Result<Self, CodexAppServerError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let program = program.as_ref().to_os_string();
        let cwd = cwd.as_ref().to_path_buf();
        if program.is_empty()
            || os_contains_nul(&program)
            || !cwd.is_absolute()
            || path_contains_nul(&cwd)
        {
            return Err(CodexAppServerError::new(
                CodexAppServerErrorCode::InvalidConfig,
            ));
        }
        let environment = environment
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<Vec<_>>();
        let validation_program = cwd.join(".heycode-codex-config-validation");
        ProcessSpec::new(validation_program, &cwd)
            .and_then(|spec| spec.with_environment(environment.iter().cloned()))
            .map_err(|_| CodexAppServerError::new(CodexAppServerErrorCode::InvalidConfig))?;
        Ok(Self {
            program,
            cwd,
            environment,
            client_info,
            discovery_workspace: None,
            outer_sandbox_mode: heycode_exec::SandboxMode::Off,
        })
    }

    /// Record the host's effective process sandbox for startup diagnostics.
    /// This never changes subprocess policy or grants filesystem access. A
    /// restrictive mode can work when the official Codex state is writable;
    /// compatibility is determined by the actual initialization attempt.
    #[must_use]
    pub const fn with_outer_sandbox_mode(mut self, mode: heycode_exec::SandboxMode) -> Self {
        self.outer_sandbox_mode = mode;
        self
    }

    pub(crate) fn with_discovery_workspace(
        mut self,
        workspace: std::sync::Arc<heycode_runtime::RuntimeDiscoveryWorkspace>,
    ) -> Self {
        self.discovery_workspace = Some(workspace);
        self
    }

    pub(crate) fn resolve(
        &self,
        subprocess: &SubprocessService,
    ) -> Result<ResolvedCodexAppServer, CodexAppServerError> {
        let launch = BoundLaunch::resolve(&self.program, &self.cwd, &self.environment, subprocess)?;
        let probe_cwd = self
            .discovery_workspace
            .as_ref()
            .map_or(self.cwd.as_path(), |owner| owner.path());
        let version = ProcessSpec::new(launch.program(), probe_cwd)
            .and_then(|spec| spec.with_args(launch.args_with(&["--version"])))
            .and_then(|spec| spec.with_environment(launch.environment().iter().cloned()))
            .and_then(|spec| spec.with_timeout(Some(VERSION_TIMEOUT)))
            .and_then(|spec| spec.with_output_limit_bytes(VERSION_OUTPUT_LIMIT))
            .map_err(CodexAppServerError::from)?;
        let server = ProcessSpec::new(launch.program(), probe_cwd)
            .and_then(|spec| {
                spec.with_args(launch.args_with(&["app-server", "--stdio", "--strict-config"]))
            })
            .and_then(|spec| spec.with_environment(launch.environment().iter().cloned()))
            .map(ProcessSpec::with_interactive_stdio)
            .map_err(CodexAppServerError::from)?;
        Ok(ResolvedCodexAppServer {
            version,
            server,
            identity: launch.identity(),
            client_info: self.client_info.clone(),
        })
    }
}

impl std::fmt::Debug for CodexAppServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexAppServerConfig")
            .field("program", &"<redacted>")
            .field("cwd", &"<redacted>")
            .field("environment_count", &self.environment.len())
            .field("client_info", &self.client_info)
            .finish()
    }
}

pub(crate) struct ResolvedCodexAppServer {
    pub(crate) version: ProcessSpec,
    pub(crate) server: ProcessSpec,
    pub(crate) identity: LaunchIdentity,
    pub(crate) client_info: CodexClientInfo,
}

fn valid_one_line(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[cfg(unix)]
fn path_contains_nul(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().contains(&0)
}

#[cfg(unix)]
fn os_contains_nul(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().contains(&0)
}

#[cfg(windows)]
fn path_contains_nul(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    path.as_os_str().encode_wide().any(|unit| unit == 0)
}

#[cfg(windows)]
fn os_contains_nul(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().any(|unit| unit == 0)
}

#[cfg(not(any(unix, windows)))]
fn path_contains_nul(path: &Path) -> bool {
    path.to_string_lossy().contains('\0')
}

#[cfg(not(any(unix, windows)))]
fn os_contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::codex_environment_from;

    #[test]
    fn installed_environment_is_minimal_and_credential_blind() {
        let environment = codex_environment_from([
            (OsString::from("PATH"), OsString::from("/safe/bin")),
            (OsString::from("HOME"), OsString::from("/safe/home")),
            (OsString::from("CODEX_HOME"), OsString::from("/safe/codex")),
            (OsString::from("LANG"), OsString::from("en_US.UTF-8")),
            (
                OsString::from("OPENAI_API_KEY"),
                OsString::from("credential-canary"),
            ),
            (
                OsString::from("ANTHROPIC_API_KEY"),
                OsString::from("credential-canary"),
            ),
            (
                OsString::from("HTTPS_PROXY"),
                OsString::from("https://private-canary"),
            ),
            (
                OsString::from("NODE_OPTIONS"),
                OsString::from("--require private-canary"),
            ),
        ]);
        let names = environment
            .iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["CODEX_HOME", "HOME", "LANG", "PATH"]);
        let debug = format!("{environment:?}");
        assert!(!debug.contains("credential-canary"));
        assert!(!debug.contains("private-canary"));
    }
}

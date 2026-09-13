//! Validated subprocess requests and outcomes.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{ProcessError, ProcessErrorCode};

const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const MAX_SPEC_COMPONENTS: usize = 1024;
const MAX_SPEC_BYTES: usize = 1024 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Behavior when one captured stream reaches its byte ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputOverflowPolicy {
    /// Fail the operation without returning a partial body.
    Error,
    /// Retain a bounded tail and mark the result truncated.
    Tail,
}

/// Opaque identity of one locally managed process.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(String);

impl ProcessId {
    pub(crate) fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Stable opaque text for diagnostics and registries.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProcessId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How the host contains a spawned process tree.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainmentMechanism {
    /// Windows Job Object.
    JobObject,
    /// Linux cgroup v2.
    CgroupV2,
    /// POSIX process group; descendants can deliberately escape with `setsid`.
    PosixProcessGroup,
    /// FreeBSD process reaper.
    ProcessReaper,
    /// A newer backend not understood by this build.
    Other(String),
}

impl ContainmentMechanism {
    /// Stable machine identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::JobObject => "job_object",
            Self::CgroupV2 => "cgroup_v2",
            Self::PosixProcessGroup => "process_group",
            Self::ProcessReaper => "process_reaper",
            Self::Other(name) => name,
        }
    }
}

/// Spawn-free containment facts published by a subprocess provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubprocessContainment {
    mechanism: ContainmentMechanism,
    tree_kill_on_drop: bool,
    resists_session_escape: bool,
}

impl SubprocessContainment {
    pub(crate) fn from_processkit() -> Self {
        let name = processkit::host_containment().mechanism().name();
        let mechanism = match name {
            "job_object" => ContainmentMechanism::JobObject,
            "cgroup_v2" => ContainmentMechanism::CgroupV2,
            "process_group" => ContainmentMechanism::PosixProcessGroup,
            "process_reaper" => ContainmentMechanism::ProcessReaper,
            other => ContainmentMechanism::Other(other.to_owned()),
        };
        let resists_session_escape = !matches!(
            mechanism,
            ContainmentMechanism::PosixProcessGroup | ContainmentMechanism::Other(_)
        );
        Self {
            mechanism,
            tree_kill_on_drop: true,
            resists_session_escape,
        }
    }

    /// Predicted host containment mechanism.
    #[must_use]
    pub fn mechanism(&self) -> &ContainmentMechanism {
        &self.mechanism
    }

    /// Whether dropping an owned live handle kills its contained tree.
    #[must_use]
    pub const fn tree_kill_on_drop(&self) -> bool {
        self.tree_kill_on_drop
    }

    /// Whether descendants cannot escape containment by starting a new session.
    #[must_use]
    pub const fn resists_session_escape(&self) -> bool {
        self.resists_session_escape
    }
}

/// Exact, fully resolved subprocess launch specification.
///
/// `Debug` exposes only counts and limits because argv and environment values
/// can contain credentials or user data.
#[derive(Clone)]
pub struct ProcessSpec {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
    timeout: Option<Duration>,
    output_limit_bytes: usize,
    output_overflow_policy: OutputOverflowPolicy,
    interactive_stdio: bool,
}

impl std::fmt::Debug for ProcessSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessSpec")
            .field("program", &"<redacted>")
            .field("cwd", &"<redacted>")
            .field("argument_count", &self.args.len())
            .field("environment_count", &self.environment.len())
            .field("timeout", &self.timeout)
            .field("output_limit_bytes", &self.output_limit_bytes)
            .field("output_overflow_policy", &self.output_overflow_policy)
            .field("interactive_stdio", &self.interactive_stdio)
            .finish()
    }
}

impl ProcessSpec {
    /// Start an exact launch specification with no arguments or environment.
    ///
    /// # Errors
    /// The executable and working directory must be absolute, NUL-free paths.
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Result<Self, ProcessError> {
        let program = program.into();
        let cwd = cwd.into();
        validate_absolute_path(&program)?;
        validate_absolute_path(&cwd)?;
        Ok(Self {
            program,
            args: Vec::new(),
            cwd,
            environment: Vec::new(),
            timeout: None,
            output_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
            output_overflow_policy: OutputOverflowPolicy::Error,
            interactive_stdio: false,
        })
    }

    /// Replace the exact argv tail.
    ///
    /// # Errors
    /// More than 1024 components, a NUL, or a total specification larger than
    /// 1 MiB is rejected.
    pub fn with_args<I, S>(mut self, args: I) -> Result<Self, ProcessError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        if args.len() > MAX_SPEC_COMPONENTS || args.iter().any(|arg| contains_nul(arg)) {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        self.args = args;
        self.validate_total_size()?;
        Ok(self)
    }

    /// Replace the complete child environment.
    ///
    /// The local provider always clears inherited environment first; only these
    /// exact pairs reach the child.
    ///
    /// # Errors
    /// Empty, `=`-bearing, NUL-bearing, duplicate, oversized, or excessive
    /// environment entries are rejected.
    pub fn with_environment<I, K, V>(mut self, values: I) -> Result<Self, ProcessError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let environment: Vec<(OsString, OsString)> = values
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        let mut names = BTreeSet::new();
        let invalid = environment.len() > MAX_SPEC_COMPONENTS
            || environment.iter().any(|(key, value)| {
                key.is_empty()
                    || contains_nul(key)
                    || contains_equals(key)
                    || contains_nul(value)
                    || !names.insert(normalize_environment_name(key))
            });
        if invalid {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        self.environment = environment;
        self.validate_total_size()?;
        Ok(self)
    }

    /// Set an optional process deadline.
    ///
    /// # Errors
    /// A configured timeout must be greater than zero and no more than 24 hours.
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Result<Self, ProcessError> {
        validate_timeout(timeout)?;
        self.timeout = timeout;
        Ok(self)
    }

    /// Set the fail-loud capture ceiling applied independently to stdout and
    /// stderr.
    ///
    /// # Errors
    /// The limit must be between one byte and 64 MiB.
    pub fn with_output_limit_bytes(mut self, bytes: usize) -> Result<Self, ProcessError> {
        validate_output_limit(bytes)?;
        self.output_limit_bytes = bytes;
        Ok(self)
    }

    /// Select fail-loud or bounded-tail behavior at the capture ceiling.
    #[must_use]
    pub const fn with_output_overflow_policy(mut self, policy: OutputOverflowPolicy) -> Self {
        self.output_overflow_policy = policy;
        self
    }

    /// Request an owned stdin writer and line-oriented stdout stream at spawn.
    #[must_use]
    pub const fn with_interactive_stdio(mut self) -> Self {
        self.interactive_stdio = true;
        self
    }

    /// Exact executable path.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Exact argv tail, excluding the executable.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// Exact absolute working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Complete explicit environment; no inherited values are implied.
    #[must_use]
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    /// Optional process deadline.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Per-stream fail-loud output ceiling.
    #[must_use]
    pub const fn output_limit_bytes(&self) -> usize {
        self.output_limit_bytes
    }

    /// Capture behavior when the per-stream byte ceiling is reached.
    #[must_use]
    pub const fn output_overflow_policy(&self) -> OutputOverflowPolicy {
        self.output_overflow_policy
    }

    /// Whether this spec is for [`crate::SubprocessService::spawn_interactive`].
    #[must_use]
    pub const fn interactive_stdio(&self) -> bool {
        self.interactive_stdio
    }

    /// Exact program followed by argv.
    #[must_use]
    pub fn launch_argv(&self) -> Vec<OsString> {
        let mut argv = Vec::with_capacity(self.args.len() + 1);
        argv.push(self.program.as_os_str().to_os_string());
        argv.extend(self.args.iter().cloned());
        argv
    }

    /// Convert exact launch argv to the string-only sandbox-provider boundary.
    ///
    /// # Errors
    /// Non-UTF-8 components cannot cross a string-only backend.
    pub fn launch_argv_strings(&self) -> Result<Vec<String>, ProcessError> {
        self.launch_argv()
            .into_iter()
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| ProcessError::new(ProcessErrorCode::InvalidSpec))
            })
            .collect()
    }

    /// Replace only program/argv after a policy transform, preserving all
    /// other resolved process facts.
    ///
    /// # Errors
    /// Empty/invalid/relative wrapper argv is rejected.
    pub fn with_launch_argv<I, S>(self, argv: I) -> Result<Self, ProcessError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
        if argv.is_empty() {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        let rest = argv.split_off(1);
        let program = PathBuf::from(argv.remove(0));
        ProcessSpec::new(program, self.cwd)?
            .with_args(rest)?
            .with_environment(self.environment)?
            .with_timeout(self.timeout)?
            .with_output_limit_bytes(self.output_limit_bytes)
            .map(|spec| {
                let spec = spec.with_output_overflow_policy(self.output_overflow_policy);
                if self.interactive_stdio {
                    spec.with_interactive_stdio()
                } else {
                    spec
                }
            })
    }

    fn validate_total_size(&self) -> Result<(), ProcessError> {
        let total = os_len(self.program.as_os_str())
            .saturating_add(os_len(self.cwd.as_os_str()))
            .saturating_add(self.args.iter().map(|arg| os_len(arg)).sum::<usize>())
            .saturating_add(
                self.environment
                    .iter()
                    .map(|(key, value)| os_len(key).saturating_add(os_len(value)))
                    .sum::<usize>(),
            );
        if total > MAX_SPEC_BYTES {
            Err(ProcessError::new(ProcessErrorCode::InvalidSpec))
        } else {
            Ok(())
        }
    }
}

/// Terminal disposition of a settled subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProcessExit {
    /// The child exited with this platform code.
    Exited {
        /// Raw exit code.
        code: i32,
    },
    /// The child was terminated by a Unix signal.
    Signalled {
        /// Signal number when reported by the host.
        signal: Option<i32>,
    },
    /// The configured wall-clock deadline elapsed.
    TimedOut,
    /// A configured output-inactivity deadline elapsed.
    InactivityTimedOut,
}

impl ProcessExit {
    /// Whether the process exited normally with code zero.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Exited { code: 0 })
    }
}

/// Bounded captured output plus terminal disposition.
pub struct ProcessOutput {
    exit: ProcessExit,
    stdout: Vec<u8>,
    stderr: String,
    duration: Duration,
    truncated: bool,
}

impl ProcessOutput {
    pub(crate) const fn new(
        exit: ProcessExit,
        stdout: Vec<u8>,
        stderr: String,
        duration: Duration,
        truncated: bool,
    ) -> Self {
        Self {
            exit,
            stdout,
            stderr,
            duration,
            truncated,
        }
    }

    /// Terminal disposition; non-zero and timeout remain results.
    #[must_use]
    pub const fn exit(&self) -> &ProcessExit {
        &self.exit
    }

    /// Exact captured stdout bytes.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Captured stderr decoded by the local provider as UTF-8 with replacement.
    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// Spawn-to-settlement wall-clock duration.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Whether either captured stream discarded an older prefix at its bound.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

impl std::fmt::Debug for ProcessOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessOutput")
            .field("exit", &self.exit)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("duration", &self.duration)
            .field("truncated", &self.truncated)
            .finish()
    }
}

pub(crate) fn map_outcome(outcome: processkit::Outcome) -> Result<ProcessExit, ProcessError> {
    match outcome.name() {
        "exited" => outcome
            .code()
            .map(|code| ProcessExit::Exited { code })
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::Io)),
        "signalled" => Ok(ProcessExit::Signalled {
            signal: outcome.signal(),
        }),
        "timed_out" => Ok(ProcessExit::TimedOut),
        "inactivity_timed_out" => Ok(ProcessExit::InactivityTimedOut),
        _ => Err(ProcessError::new(ProcessErrorCode::Unsupported)),
    }
}

pub(crate) fn validate_absolute_path(path: &Path) -> Result<(), ProcessError> {
    if path.is_absolute() && !contains_nul(path.as_os_str()) {
        Ok(())
    } else {
        Err(ProcessError::new(ProcessErrorCode::InvalidSpec))
    }
}

pub(crate) fn validate_timeout(timeout: Option<Duration>) -> Result<(), ProcessError> {
    if timeout.is_some_and(|value| value.is_zero() || value > MAX_TIMEOUT) {
        Err(ProcessError::new(ProcessErrorCode::InvalidSpec))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_output_limit(bytes: usize) -> Result<(), ProcessError> {
    if bytes == 0 || bytes > MAX_OUTPUT_LIMIT_BYTES {
        Err(ProcessError::new(ProcessErrorCode::InvalidSpec))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn os_units(value: &OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes()
}

#[cfg(unix)]
fn contains_nul(value: &OsStr) -> bool {
    os_units(value).contains(&0)
}

#[cfg(unix)]
fn contains_equals(value: &OsStr) -> bool {
    os_units(value).contains(&b'=')
}

#[cfg(unix)]
fn os_len(value: &OsStr) -> usize {
    os_units(value).len()
}

#[cfg(windows)]
fn wide_units(value: &OsStr) -> impl Iterator<Item = u16> + '_ {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide()
}

#[cfg(windows)]
fn contains_nul(value: &OsStr) -> bool {
    wide_units(value).any(|unit| unit == 0)
}

#[cfg(windows)]
fn contains_equals(value: &OsStr) -> bool {
    wide_units(value).any(|unit| unit == u16::from(b'='))
}

#[cfg(windows)]
fn os_len(value: &OsStr) -> usize {
    wide_units(value).count().saturating_mul(2)
}

#[cfg(not(any(unix, windows)))]
fn contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

#[cfg(not(any(unix, windows)))]
fn contains_equals(value: &OsStr) -> bool {
    value.to_string_lossy().contains('=')
}

#[cfg(not(any(unix, windows)))]
fn os_len(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[cfg(windows)]
fn normalize_environment_name(value: &OsStr) -> OsString {
    OsString::from(value.to_string_lossy().to_ascii_uppercase())
}

#[cfg(not(windows))]
fn normalize_environment_name(value: &OsStr) -> OsString {
    value.to_os_string()
}

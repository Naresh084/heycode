//! Explicit shell request resolution and subprocess-backed execution.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::model::{validate_absolute_path, validate_output_limit, validate_timeout};
use crate::{
    OutputOverflowPolicy, ProcessError, ProcessErrorCode, ProcessOutput, ProcessSpec,
    SERVICE_SHELL, SERVICE_SUBPROCESS, SubprocessService,
};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

/// Stable shell implementation identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShellId(String);

impl ShellId {
    fn new(value: impl Into<String>) -> Result<Self, ProcessError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || (byte == b'-' && index > 0)
            })
            && !value.ends_with('-')
            && !value.contains("--");
        if valid {
            Ok(Self(value))
        } else {
            Err(ProcessError::new(ProcessErrorCode::InvalidSpec))
        }
    }

    /// Registry/diagnostic identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ShellId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Caller intent before provider-owned default resolution.
#[derive(Clone)]
pub struct ShellRequest {
    command: String,
    no_timeout: bool,
    cwd: Option<PathBuf>,
    timeout: Option<Duration>,
    output_limit_bytes: Option<usize>,
}

impl ShellRequest {
    /// Validate one exact shell program string without choosing defaults.
    ///
    /// # Errors
    /// Blank, NUL-bearing, or over-1-MiB commands are rejected.
    pub fn new(command: impl Into<String>) -> Result<Self, ProcessError> {
        let command = command.into();
        if command.trim().is_empty()
            || command.len() > MAX_COMMAND_BYTES
            || command.as_bytes().contains(&0)
        {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        Ok(Self {
            command,
            no_timeout: false,
            cwd: None,
            timeout: None,
            output_limit_bytes: None,
        })
    }

    /// Override the provider's default cwd.
    ///
    /// # Errors
    /// The override must be an absolute NUL-free path.
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Result<Self, ProcessError> {
        let cwd = cwd.into();
        validate_absolute_path(&cwd)?;
        self.cwd = Some(cwd);
        Ok(self)
    }

    /// Supply a current workspace directory only when the caller omitted it.
    /// Explicit directory overrides remain authoritative.
    ///
    /// # Errors
    /// A supplied default must be an absolute NUL-free path when it is used.
    pub fn with_default_cwd(mut self, cwd: impl Into<PathBuf>) -> Result<Self, ProcessError> {
        if self.cwd.is_none() {
            let cwd = cwd.into();
            validate_absolute_path(&cwd)?;
            self.cwd = Some(cwd);
        }
        Ok(self)
    }

    /// Override the provider's default deadline.
    ///
    /// # Errors
    /// Zero or greater-than-24-hour deadlines are rejected.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, ProcessError> {
        validate_timeout(Some(timeout))?;
        self.no_timeout = false;
        self.timeout = Some(timeout);
        Ok(self)
    }

    /// Run without an automatic deadline, including the provider default.
    #[must_use]
    pub fn without_timeout(mut self) -> Self {
        self.no_timeout = true;
        self.timeout = None;
        self
    }

    /// Exact command supplied by the caller.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Override the provider's default per-stream capture bound.
    ///
    /// # Errors
    /// The same 1 B..64 MiB boundary as [`ProcessSpec`] is enforced.
    pub fn with_output_limit_bytes(mut self, bytes: usize) -> Result<Self, ProcessError> {
        validate_output_limit(bytes)?;
        self.output_limit_bytes = Some(bytes);
        Ok(self)
    }
}

impl std::fmt::Debug for ShellRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellRequest")
            .field("command_bytes", &self.command.len())
            .field("cwd_override", &self.cwd.is_some())
            .field("timeout", &self.timeout)
            .field("output_limit_bytes", &self.output_limit_bytes)
            .finish()
    }
}

/// Fully materialized shell launch. Execution performs no defaulting.
#[derive(Clone)]
pub struct ShellSpec {
    shell_id: ShellId,
    process: ProcessSpec,
    timeout: Option<Duration>,
}

impl ShellSpec {
    /// Resolved shell implementation id.
    #[must_use]
    pub const fn shell_id(&self) -> &ShellId {
        &self.shell_id
    }

    /// Resolved absolute working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        self.process.cwd()
    }

    /// Resolved deadline; None means execution has no automatic time limit.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Resolved per-stream output bound.
    #[must_use]
    pub const fn output_limit_bytes(&self) -> usize {
        self.process.output_limit_bytes()
    }

    /// Complete explicit child environment.
    #[must_use]
    pub fn environment(&self) -> &[(OsString, OsString)] {
        self.process.environment()
    }

    /// Exact resolved program followed by argv.
    #[must_use]
    pub fn launch_argv(&self) -> Vec<OsString> {
        let mut argv = Vec::with_capacity(self.process.args().len() + 1);
        argv.push(self.process.program().as_os_str().to_os_string());
        argv.extend(self.process.args().iter().cloned());
        argv
    }

    /// Convert the exact launch to UTF-8 strings for an argv-transforming
    /// confinement provider.
    ///
    /// # Errors
    /// A non-UTF-8 path/argument cannot cross a string-only wrapper boundary.
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

    /// Replace only the resolved launch argv after an explicit wrapper
    /// transform, preserving cwd/environment/timeout/output facts.
    ///
    /// # Errors
    /// Empty, relative-program, NUL-bearing, or oversized argv is rejected by
    /// the underlying [`ProcessSpec`] boundary.
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
        let process = ProcessSpec::new(program, self.process.cwd().to_path_buf())?
            .with_args(rest)?
            .with_environment(self.process.environment().iter().cloned())?
            .with_timeout(self.process.timeout())?
            .with_output_limit_bytes(self.process.output_limit_bytes())?
            .with_output_overflow_policy(self.process.output_overflow_policy());
        Ok(Self {
            shell_id: self.shell_id,
            process,
            timeout: self.timeout,
        })
    }

    /// Consume the resolved shell wrapper and return its exact process spec.
    ///
    /// This is the bridge used by persistent-terminal/background Consumers:
    /// every shell default has already materialized, and the Consumer may add
    /// only an explicit stdio mode before handing the spec to another exec
    /// service. No command, cwd, environment, timeout, or output fact is
    /// re-resolved here.
    #[must_use]
    pub fn into_process(self) -> ProcessSpec {
        self.process
    }
}

impl std::fmt::Debug for ShellSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellSpec")
            .field("shell_id", &self.shell_id)
            .field("process", &self.process)
            .finish()
    }
}

/// Resolved local shell defaults captured at plugin construction.
#[derive(Clone)]
pub struct LocalShellConfig {
    shell_id: ShellId,
    program: PathBuf,
    command_prefix: Vec<OsString>,
    default_cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
    default_timeout: Duration,
    output_limit_bytes: usize,
}

impl LocalShellConfig {
    /// Resolve this platform's shell executable and capture the safe default
    /// environment exactly once.
    ///
    /// # Errors
    /// Missing platform shell, invalid cwd/deadline, or invalid environment
    /// state fails before plugin composition.
    pub fn platform(
        default_cwd: impl Into<PathBuf>,
        default_timeout: Duration,
    ) -> Result<Self, ProcessError> {
        let default_cwd = default_cwd.into();
        validate_absolute_path(&default_cwd)?;
        validate_timeout(Some(default_timeout))?;
        let (shell_id, program, command_prefix) = platform_shell()?;
        let environment = safe_environment_snapshot();
        let validation = ProcessSpec::new(program.clone(), default_cwd.clone())?
            .with_args(command_prefix.clone())?
            .with_environment(environment.iter().cloned())?
            .with_timeout(Some(default_timeout))?
            .with_output_limit_bytes(DEFAULT_OUTPUT_LIMIT_BYTES)?
            .with_output_overflow_policy(OutputOverflowPolicy::Tail);
        drop(validation);
        Ok(Self {
            shell_id,
            program,
            command_prefix,
            default_cwd,
            environment,
            default_timeout,
            output_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
        })
    }
}

impl std::fmt::Debug for LocalShellConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalShellConfig")
            .field("shell_id", &self.shell_id)
            .field("program", &"<redacted>")
            .field("command_prefix_count", &self.command_prefix.len())
            .field("default_cwd", &"<redacted>")
            .field("environment_count", &self.environment.len())
            .field("default_timeout", &self.default_timeout)
            .field("output_limit_bytes", &self.output_limit_bytes)
            .finish()
    }
}

/// Replaceable shell resolver/executor implementation.
#[async_trait]
pub trait ShellBackend: Send + Sync {
    /// Resolved process authority for launching a PTY with this shell's confinement.
    fn subprocess(&self) -> Option<SubprocessService> {
        None
    }

    /// Materialize every default into a complete spec.
    ///
    /// # Errors
    /// Invalid request/provider state fails before execution.
    fn resolve(&self, request: ShellRequest) -> Result<ShellSpec, ProcessError>;

    /// Execute one already-resolved spec without changing it.
    ///
    /// # Errors
    /// Subprocess launch, cancellation, capture, or teardown failures.
    async fn execute(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError>;
    /// Execute with live bytes, failing closed if streaming is unsupported.
    async fn execute_streaming(
        &self,
        _spec: ShellSpec,
        _cancellation: CancellationToken,
        _sink: Arc<dyn crate::ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        Err(ProcessError::new(crate::ProcessErrorCode::Unsupported))
    }
}

/// Typed shell request-resolution and execution service.
#[derive(Clone)]
pub struct ShellService {
    backend: Arc<dyn ShellBackend>,
}

impl ShellService {
    /// Bind one backend.
    #[must_use]
    pub fn new(backend: Arc<dyn ShellBackend>) -> Self {
        Self { backend }
    }

    /// Construct a standalone local shell for embedding and tests.
    /// Shipped worlds compose `subprocess-local` plus `shell-local` so context
    /// shutdown owns process cancellation.
    #[must_use]
    pub fn local(config: LocalShellConfig) -> Self {
        Self::new(Arc::new(LocalShellBackend {
            config,
            subprocess: SubprocessService::local(),
        }))
    }

    /// Preserve this resolver's exact defaults while executing inside a host-owned scope.
    #[must_use]
    pub fn with_executor(&self, subprocess: SubprocessService) -> Self {
        Self::new(Arc::new(ScopedShellBackend {
            resolver: self.clone(),
            subprocess,
        }))
    }

    /// Process authority shared with this shell, when the backend supports PTYs.
    #[must_use]
    pub fn subprocess(&self) -> Option<SubprocessService> {
        self.backend.subprocess()
    }

    /// Materialize every provider default into a complete spec.
    ///
    /// # Errors
    /// Invalid request/provider state fails before execution.
    pub fn resolve(&self, request: ShellRequest) -> Result<ShellSpec, ProcessError> {
        self.backend.resolve(request)
    }

    /// Execute one already-resolved spec without changing it.
    ///
    /// # Errors
    /// Subprocess launch, cancellation, capture, or teardown failures.
    pub async fn execute(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.backend.execute(spec, cancellation).await
    }
    /// Execute with live output using the same resolved provider and process ownership.
    ///
    /// # Errors
    /// Unsupported streaming or ordinary execution failure.
    pub async fn execute_streaming(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
        sink: Arc<dyn crate::ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        self.backend
            .execute_streaming(spec, cancellation, sink)
            .await
    }
}

struct LocalShellBackend {
    config: LocalShellConfig,
    subprocess: SubprocessService,
}

#[async_trait]
impl ShellBackend for LocalShellBackend {
    fn subprocess(&self) -> Option<SubprocessService> {
        Some(self.subprocess.clone())
    }

    fn resolve(&self, request: ShellRequest) -> Result<ShellSpec, ProcessError> {
        let cwd = request
            .cwd
            .unwrap_or_else(|| self.config.default_cwd.clone());
        let timeout = if request.no_timeout {
            None
        } else {
            Some(request.timeout.unwrap_or(self.config.default_timeout))
        };
        let output_limit = request
            .output_limit_bytes
            .unwrap_or(self.config.output_limit_bytes);
        let mut args = self.config.command_prefix.clone();
        args.push(OsString::from(request.command));
        let process = ProcessSpec::new(self.config.program.clone(), cwd)?
            .with_args(args)?
            .with_environment(self.config.environment.iter().cloned())?
            .with_timeout(timeout)?
            .with_output_limit_bytes(output_limit)?
            .with_output_overflow_policy(OutputOverflowPolicy::Tail);
        Ok(ShellSpec {
            shell_id: self.config.shell_id.clone(),
            process,
            timeout,
        })
    }

    async fn execute(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.subprocess
            .output(spec.into_process(), cancellation)
            .await
    }
    async fn execute_streaming(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
        sink: Arc<dyn crate::ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        self.subprocess
            .output_streaming(spec.into_process(), cancellation, sink)
            .await
    }
}

/// Publish the local resolved shell Consumer over `subprocess`.
#[must_use]
pub fn local_shell_plugin(config: LocalShellConfig) -> Box<dyn heycode_core::Plugin> {
    struct LocalShellPlugin(LocalShellConfig);

    impl heycode_core::Plugin for LocalShellPlugin {
        fn name(&self) -> &'static str {
            "shell-local"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SUBPROCESS]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SHELL]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let subprocess = context
                .get::<SubprocessService>(SERVICE_SUBPROCESS)
                .ok_or_else(|| {
                    heycode_core::CoreError::other("subprocess service type mismatch")
                })?;
            context.provide(
                SERVICE_SHELL,
                self.name(),
                ShellService::new(Arc::new(LocalShellBackend {
                    config: self.0.clone(),
                    subprocess: (*subprocess).clone(),
                })),
            )
        }
    }

    Box::new(LocalShellPlugin(config))
}

/// Publish subprocess and shell together for hand-built embedded compositions.
/// The production factory uses the split plugins so profile dependencies and
/// ownership remain independently inspectable.
#[must_use]
pub fn local_execution_plugin(config: LocalShellConfig) -> Box<dyn heycode_core::Plugin> {
    struct LocalExecutionPlugin(LocalShellConfig);

    impl heycode_core::Plugin for LocalExecutionPlugin {
        fn name(&self) -> &'static str {
            "execution-local"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_SANDBOX, SERVICE_SUBPROCESS, SERVICE_SHELL]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let sandbox = crate::SandboxService::new(
                crate::SandboxMode::Off,
                self.0.default_cwd.clone(),
                None,
            )
            .map_err(|_| heycode_core::CoreError::other("execution workspace root is invalid"))?;
            provide_local_execution(context, self.name(), &self.0, sandbox)
        }
    }

    Box::new(LocalExecutionPlugin(config))
}

/// Publish subprocess and shell with one explicitly resolved sandbox service.
#[must_use]
pub fn local_execution_plugin_with_sandbox(
    config: LocalShellConfig,
    sandbox: crate::SandboxService,
) -> Box<dyn heycode_core::Plugin> {
    struct LocalExecutionPlugin(LocalShellConfig, crate::SandboxService);

    impl heycode_core::Plugin for LocalExecutionPlugin {
        fn name(&self) -> &'static str {
            "execution-local"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_SANDBOX, SERVICE_SUBPROCESS, SERVICE_SHELL]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            provide_local_execution(context, self.name(), &self.0, self.1.clone())
        }
    }

    Box::new(LocalExecutionPlugin(config, sandbox))
}

fn provide_local_execution(
    context: &mut heycode_core::Context,
    owner: &'static str,
    config: &LocalShellConfig,
    sandbox: crate::SandboxService,
) -> heycode_core::CoreResult<()> {
    let backend = crate::local::backend(sandbox.clone());
    let shutdown = backend.shutdown_token();
    context.effect(move || shutdown.cancel());
    let subprocess = SubprocessService::new(backend);
    context.provide(crate::SERVICE_SANDBOX, owner, sandbox)?;
    context.provide(SERVICE_SUBPROCESS, owner, subprocess.clone())?;
    context.provide(
        SERVICE_SHELL,
        owner,
        ShellService::new(Arc::new(LocalShellBackend {
            config: config.clone(),
            subprocess,
        })),
    )
}

fn looks_like_credential(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "PASSWORD", "SECRET", "TOKEN"]
        .iter()
        .any(|needle| upper.contains(needle))
}

/// Snapshot the current process environment after removing names that look
/// credential-bearing. Non-Unicode names are excluded because they cannot be
/// classified safely.
#[must_use]
pub fn safe_environment_snapshot() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|name| !looks_like_credential(name))
        })
        .collect()
}

#[cfg(unix)]
fn platform_shell() -> Result<(ShellId, PathBuf, Vec<OsString>), ProcessError> {
    let program = processkit::which("bash").map_err(ProcessError::from)?;
    Ok((ShellId::new("bash")?, program, vec![OsString::from("-c")]))
}

#[cfg(windows)]
fn platform_shell() -> Result<(ShellId, PathBuf, Vec<OsString>), ProcessError> {
    let from_comspec = std::env::var_os("ComSpec").map(PathBuf::from);
    let program = from_comspec
        .filter(|path| path.is_absolute() && path.is_file())
        .or_else(|| {
            std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .map(|root| root.join("System32").join("cmd.exe"))
                .filter(|path| path.is_file())
        })
        .ok_or_else(|| ProcessError::new(ProcessErrorCode::NotFound))?;
    Ok((
        ShellId::new("cmd")?,
        program,
        vec![
            OsString::from("/D"),
            OsString::from("/S"),
            OsString::from("/C"),
        ],
    ))
}

#[cfg(not(any(unix, windows)))]
fn platform_shell() -> Result<(ShellId, PathBuf, Vec<OsString>), ProcessError> {
    let program = processkit::which("sh").map_err(ProcessError::from)?;
    Ok((ShellId::new("sh")?, program, vec![OsString::from("-c")]))
}

struct ScopedShellBackend {
    resolver: ShellService,
    subprocess: SubprocessService,
}
#[async_trait]
impl ShellBackend for ScopedShellBackend {
    fn subprocess(&self) -> Option<SubprocessService> {
        Some(self.subprocess.clone())
    }

    fn resolve(&self, request: ShellRequest) -> Result<ShellSpec, ProcessError> {
        self.resolver.resolve(request)
    }
    async fn execute(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.subprocess
            .output(spec.into_process(), cancellation)
            .await
    }

    async fn execute_streaming(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
        sink: Arc<dyn crate::ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        self.subprocess
            .output_streaming(spec.into_process(), cancellation, sink)
            .await
    }
}

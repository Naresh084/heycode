//! Exact Claude Code CLI requests, parsers and lifecycle ownership.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use heycode_exec::{
    OutputOverflowPolicy, ProcessError, ProcessErrorCode, ProcessExit, ProcessOutput, ProcessSpec,
    SubprocessService,
};
use heycode_runtime::{
    AccountState, AccountStatus, RuntimeConfiguration, RuntimeError, RuntimeErrorCode,
};
use tokio_util::sync::CancellationToken;

use crate::executable::ResolvedExecutable;
use crate::lifecycle::Lifecycle;
use crate::version::parse_version_output;
use crate::{ClaudeCliVersion, ClaudeRuntimeConfig, ClaudeVersionPolicy};

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const VERSION_OUTPUT_LIMIT: usize = 4096;
const ACCOUNT_OUTPUT_LIMIT: usize = 16 * 1024;
const HANDSHAKE_OUTPUT_LIMIT: usize = 256 * 1024;
const HANDSHAKE_TOKEN: &str = "R07_CLAUDE_HANDSHAKE_OK";
const HANDSHAKE_SYSTEM_PROMPT: &str =
    "Return only the exact requested handshake token. Do not inspect files or use tools.";
const HANDSHAKE_PROMPT: &str = "Reply exactly R07_CLAUDE_HANDSHAKE_OK.";
const EMPTY_MCP_CONFIG: &str = r#"{"mcpServers":{}}"#;

struct ProcessConfig {
    program: OsString,
    cwd: PathBuf,
    discovery_workspace: Option<Arc<heycode_runtime::RuntimeDiscoveryWorkspace>>,
    environment: Vec<(OsString, OsString)>,
    version_policy: ClaudeVersionPolicy,
}

impl std::fmt::Debug for ProcessConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessConfig")
            .field("program", &"<redacted>")
            .field("cwd", &"<redacted>")
            .field("environment_count", &self.environment.len())
            .field("version_policy", &self.version_policy)
            .finish()
    }
}

/// Successful fixed no-persistence Claude Code query handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeHandshakeReceipt {
    version: ClaudeCliVersion,
    no_session_persistence: bool,
}

impl ClaudeHandshakeReceipt {
    /// Compatible installed CLI version used for the query.
    #[must_use]
    pub const fn version(self) -> ClaudeCliVersion {
        self.version
    }

    /// Whether the exact invocation selected Claude Code's no-persistence mode.
    #[must_use]
    pub const fn no_session_persistence(self) -> bool {
        self.no_session_persistence
    }
}

/// Installed Claude Code process client shared by the runtime Provider.
#[derive(Clone)]
pub struct ClaudeCliClient {
    subprocess: SubprocessService,
    config: Arc<ProcessConfig>,
    lifecycle: Arc<Lifecycle>,
}

impl ClaudeCliClient {
    /// Bind validated caller intent to the replaceable subprocess Provider.
    /// Executable resolution remains operation-time so installation, removal
    /// and upgrades become visible without making optional runtime registration
    /// fail product composition.
    #[must_use]
    pub fn new(subprocess: SubprocessService, config: ClaudeRuntimeConfig) -> Self {
        Self {
            subprocess,
            config: Arc::new(ProcessConfig {
                program: config.program().to_os_string(),
                cwd: config.cwd().to_path_buf(),
                discovery_workspace: config.discovery_workspace.clone(),
                environment: config.environment().to_vec(),
                version_policy: config.version_policy(),
            }),
            lifecycle: Lifecycle::new(),
        }
    }

    /// Detect and validate the installed Claude Code version.
    ///
    /// # Errors
    /// Cancellation, process failure, malformed identity, or incompatible
    /// versions return a fixed body-free runtime error.
    pub async fn version(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ClaudeCliVersion, RuntimeError> {
        let _operation = self.lifecycle.begin(&cancellation)?;
        let executable = self.resolve_executable()?;
        self.version_with(&executable, cancellation).await
    }

    async fn version_with(
        &self,
        executable: &ResolvedExecutable,
        cancellation: CancellationToken,
    ) -> Result<ClaudeCliVersion, RuntimeError> {
        let output = self
            .run(
                executable,
                self.spec(
                    executable,
                    [OsString::from("--version")],
                    VERSION_TIMEOUT,
                    VERSION_OUTPUT_LIMIT,
                )?,
                cancellation,
            )
            .await?;
        require_success(&output)?;
        let Some(version) = parse_version_output(output.stdout()) else {
            return Err(protocol_error());
        };
        if !self.config.version_policy.accepts(version) {
            return Err(safe_error(
                RuntimeErrorCode::Unavailable,
                "Claude Code version is incompatible with this runtime plugin",
            ));
        }
        Ok(version)
    }

    /// Inspect official Claude Code auth status without reading credential files.
    ///
    /// # Errors
    /// Cancellation, incompatible CLI, process failure, or malformed JSON
    /// returns a fixed body-free runtime error.
    pub async fn account(
        &self,
        cancellation: CancellationToken,
    ) -> Result<AccountState, RuntimeError> {
        let _operation = self.lifecycle.begin(&cancellation)?;
        let executable = self.resolve_executable()?;
        self.version_with(&executable, cancellation.clone()).await?;
        self.account_after_version(&executable, cancellation).await
    }

    async fn account_after_version(
        &self,
        executable: &ResolvedExecutable,
        cancellation: CancellationToken,
    ) -> Result<AccountState, RuntimeError> {
        let output = self
            .run(
                executable,
                self.spec(
                    executable,
                    [
                        OsString::from("auth"),
                        OsString::from("status"),
                        OsString::from("--json"),
                    ],
                    ACCOUNT_TIMEOUT,
                    ACCOUNT_OUTPUT_LIMIT,
                )?,
                cancellation,
            )
            .await?;
        parse_account_output(&output)
    }

    /// Run a fixed, tool-free stream-JSON query using no-session persistence.
    ///
    /// This is a transport compatibility probe, not a general user-input API.
    /// R08 adds durable delegated-session dispatch before arbitrary text can
    /// cross this process boundary.
    ///
    /// # Errors
    /// Cancellation, incompatible CLI, nonzero exit, malformed stream output,
    /// tool activity, or a mismatched result fails with a body-free error.
    pub async fn handshake(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ClaudeHandshakeReceipt, RuntimeError> {
        let _operation = self.lifecycle.begin(&cancellation)?;
        let executable = self.resolve_executable()?;
        let version = self.version_with(&executable, cancellation.clone()).await?;
        let account = self
            .account_after_version(&executable, cancellation.clone())
            .await?;
        if account.status() != AccountStatus::Connected {
            return Err(safe_error(
                RuntimeErrorCode::Unauthorized,
                "Claude Code account is not authenticated",
            ));
        }
        let output = self
            .run(
                &executable,
                self.spec(
                    &executable,
                    handshake_args(),
                    HANDSHAKE_TIMEOUT,
                    HANDSHAKE_OUTPUT_LIMIT,
                )?,
                cancellation,
            )
            .await?;
        require_success(&output)?;
        parse_handshake_output(output.stdout())?;
        Ok(ClaudeHandshakeReceipt {
            version,
            no_session_persistence: true,
        })
    }

    /// Launch one long-lived stream-json session process.
    ///
    /// The executable is re-resolved and re-verified at operation time, and the
    /// version is validated before any session argv reaches the process, so a
    /// replacement or an incompatible upgrade is visible per operation rather
    /// than per registration.
    ///
    /// # Errors
    /// Cancellation, resolution/identity failure, incompatible version or
    /// spawn failure returns a fixed body-free error.
    pub(crate) async fn launch_session(
        &self,
        identity: crate::session::SessionIdentity<'_>,
        configuration: &RuntimeConfiguration,
        workspace: &std::path::Path,
        cancellation: CancellationToken,
    ) -> Result<(heycode_exec::InteractiveProcess, CancellationToken), RuntimeError> {
        let _operation = self.lifecycle.begin(&cancellation)?;
        let executable = self.resolve_executable()?;
        self.version_with(&executable, cancellation.clone()).await?;
        executable.verify()?;
        // A session's cwd is the child's own working directory: the CLI has no
        // top-level `--cwd` flag.
        let spec = ProcessSpec::new(executable.path(), workspace)
            .and_then(|spec| spec.with_args(crate::session::session_args(identity, configuration)))
            .and_then(|spec| spec.with_environment(self.config.environment.iter().cloned()))
            .map(ProcessSpec::with_interactive_stdio)
            .map_err(|_| invalid_request_error())?;
        // One session-lifetime child token, not a per-operation one.
        let lifecycle = self.lifecycle.shutdown_token().child_token();
        let spawn = self.subprocess.spawn_interactive(spec, lifecycle.clone());
        tokio::pin!(spawn);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                lifecycle.cancel();
                if let Ok(process) = spawn.await {
                    let (process, input, _lines) = process.into_parts();
                    let _settled = input.finish().await;
                    let _reaped = process.cancel().await;
                }
                Err(RuntimeError::cancelled())
            }
            result = &mut spawn => result
                .map(|process| (process, lifecycle))
                .map_err(map_process_error),
        }
    }

    /// Quiescent, idempotent process-client close.
    ///
    /// Active operations are cancelled through the subprocess tree owner and
    /// close waits for every operation lease to settle.
    ///
    /// # Errors
    /// A cancelled close still waits for quiescence, then returns Cancelled.
    pub async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        self.lifecycle.close(cancellation).await
    }

    pub(crate) fn force_close(&self) {
        self.lifecycle.force_close();
    }

    pub(crate) fn discovery_workspace(&self) -> &std::path::Path {
        self.config
            .discovery_workspace
            .as_ref()
            .map_or(self.config.cwd.as_path(), |owner| owner.path())
    }

    fn resolve_executable(&self) -> Result<ResolvedExecutable, RuntimeError> {
        let path = self
            .subprocess
            .resolve_program(&self.config.program)
            .map_err(map_process_error)?;
        ResolvedExecutable::resolve(path)
    }

    fn spec<I>(
        &self,
        executable: &ResolvedExecutable,
        args: I,
        timeout: Duration,
        limit: usize,
    ) -> Result<ProcessSpec, RuntimeError>
    where
        I: IntoIterator<Item = OsString>,
    {
        ProcessSpec::new(
            executable.path(),
            self.config
                .discovery_workspace
                .as_ref()
                .map_or(self.config.cwd.as_path(), |owner| owner.path()),
        )
        .and_then(|spec| spec.with_args(args))
        .and_then(|spec| spec.with_environment(self.config.environment.iter().cloned()))
        .and_then(|spec| spec.with_timeout(Some(timeout)))
        .and_then(|spec| spec.with_output_limit_bytes(limit))
        .map(|spec| spec.with_output_overflow_policy(OutputOverflowPolicy::Error))
        .map_err(|_| invalid_request_error())
    }

    async fn run(
        &self,
        executable: &ResolvedExecutable,
        spec: ProcessSpec,
        caller: CancellationToken,
    ) -> Result<ProcessOutput, RuntimeError> {
        self.lifecycle.check(&caller)?;
        executable.verify()?;
        let shutdown = self.lifecycle.shutdown_token();
        let process_cancellation = shutdown.child_token();
        let output = self.subprocess.output(spec, process_cancellation.clone());
        tokio::pin!(output);
        let (result, interruption) = tokio::select! {
            biased;
            result = &mut output => (result, None),
            () = caller.cancelled() => {
                process_cancellation.cancel();
                (output.await, Some(Interruption::Caller))
            }
            () = shutdown.cancelled() => {
                process_cancellation.cancel();
                (output.await, Some(Interruption::Shutdown))
            }
        };
        match interruption {
            Some(interruption) => settle_interrupted(result, interruption),
            None => result.map_err(map_process_error),
        }
    }
}

impl std::fmt::Debug for ClaudeCliClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClaudeCliClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
enum Interruption {
    Caller,
    Shutdown,
}

fn settle_interrupted(
    result: Result<ProcessOutput, ProcessError>,
    interruption: Interruption,
) -> Result<ProcessOutput, RuntimeError> {
    match result {
        Ok(_) => Err(interruption.error()),
        Err(error) if error.code() == ProcessErrorCode::Cancelled => Err(interruption.error()),
        Err(error) => Err(map_process_error(error)),
    }
}

impl Interruption {
    fn error(self) -> RuntimeError {
        match self {
            Self::Caller => RuntimeError::cancelled(),
            Self::Shutdown => RuntimeError::closed(),
        }
    }
}

fn handshake_args() -> Vec<OsString> {
    [
        "--safe-mode",
        "--strict-mcp-config",
        "--mcp-config",
        EMPTY_MCP_CONFIG,
        "--tools",
        "",
        "--disable-slash-commands",
        "--no-chrome",
        "--permission-mode",
        "dontAsk",
        "--no-session-persistence",
        "--output-format",
        "stream-json",
        "--verbose",
        "--prompt-suggestions",
        "false",
        "--system-prompt",
        HANDSHAKE_SYSTEM_PROMPT,
        "--print",
        HANDSHAKE_PROMPT,
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn require_success(output: &ProcessOutput) -> Result<(), RuntimeError> {
    if output.exit().is_success() && !output.truncated() {
        Ok(())
    } else {
        Err(unavailable_error())
    }
}

fn parse_account_output(output: &ProcessOutput) -> Result<AccountState, RuntimeError> {
    if !matches!(output.exit(), ProcessExit::Exited { .. }) {
        return Err(unavailable_error());
    }
    parse_account(
        output.exit().is_success(),
        output.truncated(),
        output.stdout(),
    )
}

fn parse_account(
    exit_success: bool,
    truncated: bool,
    bytes: &[u8],
) -> Result<AccountState, RuntimeError> {
    if truncated {
        return Err(protocol_error());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| protocol_error())?;
    let logged_in = value
        .as_object()
        .and_then(|object| object.get("loggedIn"))
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(protocol_error)?;
    if logged_in {
        if !exit_success {
            return Err(protocol_error());
        }
        AccountState::connected(None).map_err(|_| protocol_error())
    } else {
        Ok(AccountState::without_label(AccountStatus::Disconnected))
    }
}

fn parse_handshake_output(bytes: &[u8]) -> Result<(), RuntimeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| protocol_error())?;
    let mut initialized = false;
    let mut assistant_seen = false;
    let mut settled = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if settled {
            return Err(protocol_error());
        }
        let value: serde_json::Value = serde_json::from_str(line).map_err(|_| protocol_error())?;
        let object = value.as_object().ok_or_else(protocol_error)?;
        let event_type = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(protocol_error)?;
        if contains_disallowed_tool_type(&value) {
            return Err(protocol_error());
        }
        let is_init = event_type == "system"
            && object.get("subtype").and_then(serde_json::Value::as_str) == Some("init");
        if !initialized && !is_init {
            return Err(protocol_error());
        }
        match event_type {
            "system" if is_init => {
                if initialized {
                    return Err(protocol_error());
                }
                initialized = true;
            }
            "rate_limit_event" => {}
            "assistant" => {
                if assistant_seen {
                    return Err(protocol_error());
                }
                validate_handshake_assistant(object)?;
                assistant_seen = true;
            }
            "result" => {
                let valid = initialized
                    && assistant_seen
                    && object.get("subtype").and_then(serde_json::Value::as_str) == Some("success")
                    && object.get("is_error").and_then(serde_json::Value::as_bool) == Some(false)
                    && object.get("result").and_then(serde_json::Value::as_str)
                        == Some(HANDSHAKE_TOKEN)
                    && object
                        .get("session_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some();
                if !valid {
                    return Err(protocol_error());
                }
                settled = true;
            }
            _ => return Err(protocol_error()),
        }
    }
    if initialized && assistant_seen && settled {
        Ok(())
    } else {
        Err(protocol_error())
    }
}

fn validate_handshake_assistant(
    event: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), RuntimeError> {
    let message = event
        .get("message")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(protocol_error)?;
    if message.get("type").and_then(serde_json::Value::as_str) != Some("message")
        || message.get("role").and_then(serde_json::Value::as_str) != Some("assistant")
    {
        return Err(protocol_error());
    }
    let content = message
        .get("content")
        .and_then(serde_json::Value::as_array)
        .filter(|content| !content.is_empty())
        .ok_or_else(protocol_error)?;
    let mut text = String::new();
    for block in content {
        let block = block.as_object().ok_or_else(protocol_error)?;
        if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
            return Err(protocol_error());
        }
        text.push_str(
            block
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(protocol_error)?,
        );
    }
    if text == HANDSHAKE_TOKEN {
        Ok(())
    } else {
        Err(protocol_error())
    }
}

fn contains_disallowed_tool_type(value: &serde_json::Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::Array(values) => pending.extend(values),
            serde_json::Value::Object(values) => {
                if values
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| {
                        matches!(kind, "tool_use" | "tool_result" | "server_tool_use")
                    })
                {
                    return true;
                }
                pending.extend(values.values());
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    false
}

fn map_process_error(error: ProcessError) -> RuntimeError {
    match error.code() {
        ProcessErrorCode::Cancelled => RuntimeError::cancelled(),
        ProcessErrorCode::OutputLimit => protocol_error(),
        ProcessErrorCode::InvalidSpec => invalid_request_error(),
        ProcessErrorCode::Teardown => safe_error(
            RuntimeErrorCode::Internal,
            "Claude Code process teardown could not be confirmed",
        ),
        ProcessErrorCode::NotFound
        | ProcessErrorCode::PermissionDenied
        | ProcessErrorCode::Spawn
        | ProcessErrorCode::ServiceStopped
        | ProcessErrorCode::Unsupported
        | ProcessErrorCode::Sandbox
        | ProcessErrorCode::Io => unavailable_error(),
        _ => unavailable_error(),
    }
}

fn safe_error(code: RuntimeErrorCode, message: &'static str) -> RuntimeError {
    match RuntimeError::try_new(code, message) {
        Ok(error) => error,
        Err(_) => RuntimeError::internal("invalid static Claude runtime error"),
    }
}

fn unavailable_error() -> RuntimeError {
    safe_error(
        RuntimeErrorCode::Unavailable,
        "Claude Code runtime process is unavailable",
    )
}

fn protocol_error() -> RuntimeError {
    safe_error(
        RuntimeErrorCode::Protocol,
        "Claude Code process protocol failed",
    )
}

fn invalid_request_error() -> RuntimeError {
    safe_error(
        RuntimeErrorCode::InvalidRequest,
        "Claude Code process request is invalid",
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The malformed-output CLASSIFICATION, proven deterministically.
    ///
    /// The integration test cannot own this: it drives a real process, so under
    /// load the handshake times out and the class becomes `Unavailable` — a
    /// true statement about a slow machine, not about parsing. Classification
    /// belongs to a test that no scheduler can perturb.
    #[test]
    fn malformed_handshake_output_is_a_protocol_error_and_keeps_the_body_out() {
        const CANARY: &str = "private-provider-body-canary";
        let error = parse_handshake_output(CANARY.as_bytes())
            .expect_err("non-JSON output is a protocol failure");
        assert_eq!(error.code(), RuntimeErrorCode::Protocol);
        assert!(!error.to_string().contains(CANARY));
        assert!(!format!("{error:?}").contains(CANARY));

        // A well-formed stream that never reaches a result is equally a
        // protocol failure, not a silent success.
        let truncated = br#"{"type":"system","subtype":"init"}"#;
        assert_eq!(
            parse_handshake_output(truncated).unwrap_err().code(),
            RuntimeErrorCode::Protocol
        );
    }

    #[test]
    fn account_parser_ignores_private_account_fields_and_accepts_disconnected_exit() {
        let account = parse_account(
            true,
            false,
            br#"{"loggedIn":true,"email":"private-canary","orgId":"private-org"}"#,
        )
        .unwrap();
        assert_eq!(account.status(), AccountStatus::Connected);
        assert_eq!(account.label(), None);
        assert!(!format!("{account:?}").contains("private-canary"));

        assert_eq!(
            parse_account(
                false,
                false,
                br#"{"loggedIn":false,"detail":"private-canary"}"#
            )
            .unwrap()
            .status(),
            AccountStatus::Disconnected
        );
    }

    #[test]
    fn handshake_parser_requires_init_exact_result_and_no_tool_activity() {
        let valid = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"rate_limit_event\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"R07_CLAUDE_HANDSHAKE_OK\"}]}}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"R07_CLAUDE_HANDSHAKE_OK\",\"session_id\":\"private\"}\n"
        );
        assert_eq!(parse_handshake_output(valid.as_bytes()), Ok(()));

        let tool = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\"}]}}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"R07_CLAUDE_HANDSHAKE_OK\",\"session_id\":\"private\"}\n"
        );
        assert_eq!(
            parse_handshake_output(tool.as_bytes()).unwrap_err().code(),
            RuntimeErrorCode::Protocol
        );

        let unknown_event = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"future_event\",\"private\":\"canary\"}\n"
        );
        assert_eq!(
            parse_handshake_output(unknown_event.as_bytes())
                .unwrap_err()
                .code(),
            RuntimeErrorCode::Protocol
        );

        let unknown_content = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"private\"}]}}\n"
        );
        assert_eq!(
            parse_handshake_output(unknown_content.as_bytes())
                .unwrap_err()
                .code(),
            RuntimeErrorCode::Protocol
        );
    }

    #[test]
    fn malformed_provider_bodies_never_enter_errors() {
        let canary = "private-provider-body-canary";
        let error = parse_handshake_output(canary.as_bytes()).unwrap_err();
        assert!(!error.to_string().contains(canary));
        assert!(!format!("{error:?}").contains(canary));
    }
}

//! Fixed body-free Codex app-server failures.

/// Stable failure classification for the Codex app-server Provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAppServerErrorCode {
    /// Configuration or caller input failed validation.
    InvalidConfig,
    /// The configured executable could not be resolved or started.
    Unavailable,
    /// The installed CLI does not exactly match the pinned wire schema.
    UnsupportedVersion,
    /// One process operation failed after launch.
    Process,
    /// Initialization failed while a restrictive host sandbox was active.
    SandboxStartup,
    /// Caller or Provider lifecycle cancellation settled the operation.
    Cancelled,
    /// JSONL/JSON-RPC framing, shape, correlation or lifecycle failed.
    Protocol,
    /// The server returned a classified JSON-RPC error.
    Remote,
    /// The app-server reported bounded-queue overload.
    Overloaded,
    /// A request conflicts with current connection state.
    Conflict,
    /// The connection completed close of its reported containment group.
    Closed,
}

impl CodexAppServerErrorCode {
    /// Stable machine identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfig => "invalid_config",
            Self::Unavailable => "unavailable",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Process => "process",
            Self::SandboxStartup => "sandbox_startup",
            Self::Cancelled => "cancelled",
            Self::Protocol => "protocol",
            Self::Remote => "remote",
            Self::Overloaded => "overloaded",
            Self::Conflict => "conflict",
            Self::Closed => "closed",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::InvalidConfig => "Codex app-server configuration is invalid",
            Self::Unavailable => "Codex CLI is unavailable",
            Self::UnsupportedVersion => "Codex CLI version is unsupported",
            Self::Process => "Codex app-server process failed",
            Self::SandboxStartup => {
                "Codex app-server failed to initialize under the heycode sandbox; check that its official state directory is writable, or select --sandbox off or another runtime"
            }
            Self::Cancelled => "Codex app-server operation was cancelled and settled",
            Self::Protocol => "Codex app-server protocol failed",
            Self::Remote => "Codex app-server request failed",
            Self::Overloaded => "Codex app-server is overloaded",
            Self::Conflict => "Codex app-server state conflicts with the request",
            Self::Closed => "Codex app-server connection is closed",
        }
    }
}

impl std::fmt::Display for CodexAppServerErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Fixed diagnostic retaining no executable path, argv, environment, output,
/// provider body, account data or raw protocol text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CodexAppServerError {
    code: CodexAppServerErrorCode,
    message: &'static str,
}

impl CodexAppServerError {
    pub(crate) const fn new(code: CodexAppServerErrorCode) -> Self {
        Self {
            code,
            message: code.message(),
        }
    }

    /// Stable failure classification.
    #[must_use]
    pub const fn code(&self) -> CodexAppServerErrorCode {
        self.code
    }
}

impl From<heycode_exec::ProcessError> for CodexAppServerError {
    fn from(error: heycode_exec::ProcessError) -> Self {
        use heycode_exec::ProcessErrorCode;

        let code = match error.code() {
            ProcessErrorCode::Cancelled | ProcessErrorCode::ServiceStopped => {
                CodexAppServerErrorCode::Cancelled
            }
            ProcessErrorCode::NotFound
            | ProcessErrorCode::PermissionDenied
            | ProcessErrorCode::Spawn => CodexAppServerErrorCode::Unavailable,
            ProcessErrorCode::InvalidSpec => CodexAppServerErrorCode::InvalidConfig,
            ProcessErrorCode::OutputLimit
            | ProcessErrorCode::Teardown
            | ProcessErrorCode::Unsupported
            | ProcessErrorCode::Sandbox
            | ProcessErrorCode::Io => CodexAppServerErrorCode::Process,
            _ => CodexAppServerErrorCode::Process,
        };
        Self::new(code)
    }
}

//! Secret-safe subprocess failures.

/// Stable subprocess failure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProcessErrorCode {
    /// A caller supplied an invalid process boundary value.
    InvalidSpec,
    /// The exact executable path was not found.
    NotFound,
    /// The operating system denied process launch or I/O.
    PermissionDenied,
    /// The executable could not be started.
    Spawn,
    /// The caller or owning plugin cancelled the process.
    Cancelled,
    /// The owning subprocess provider has shut down.
    ServiceStopped,
    /// Captured output exceeded the explicit bound.
    OutputLimit,
    /// Terminal process-tree teardown could not be confirmed.
    Teardown,
    /// The host cannot provide a requested process primitive.
    Unsupported,
    /// The effective process sandbox could not confine the launch.
    Sandbox,
    /// No terminal session with that identity is registered for the owner.
    UnknownTerminal,
    /// The terminal session's child has already ended its terminal.
    TerminalExited,
    /// The terminal registry is at its explicit session bound.
    TerminalCapacity,
    /// The process runtime reported an uncategorized I/O failure.
    Io,
}

impl ProcessErrorCode {
    /// Stable machine identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid_spec",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::Spawn => "spawn",
            Self::Cancelled => "cancelled",
            Self::ServiceStopped => "service_stopped",
            Self::OutputLimit => "output_limit",
            Self::Teardown => "teardown",
            Self::Unsupported => "unsupported",
            Self::Sandbox => "sandbox",
            Self::UnknownTerminal => "unknown_terminal",
            Self::TerminalExited => "terminal_exited",
            Self::TerminalCapacity => "terminal_capacity",
            Self::Io => "io",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid subprocess specification",
            Self::NotFound => "subprocess executable was not found",
            Self::PermissionDenied => "subprocess operation was denied",
            Self::Spawn => "subprocess could not be started",
            Self::Cancelled => "subprocess was cancelled and settled",
            Self::ServiceStopped => "subprocess service has stopped",
            Self::OutputLimit => "subprocess output exceeded its configured limit",
            Self::Teardown => "subprocess tree teardown could not be confirmed",
            Self::Unsupported => "subprocess operation is unsupported on this host",
            Self::Sandbox => "subprocess sandbox policy could not be applied",
            Self::UnknownTerminal => "terminal session is not registered for this owner",
            Self::TerminalExited => "terminal session has already ended",
            Self::TerminalCapacity => "terminal registry is at its session limit",
            Self::Io => "subprocess I/O failed",
        }
    }
}

impl std::fmt::Display for ProcessErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A fixed, secret-safe subprocess error.
///
/// It deliberately retains no argv, environment value, captured output, path,
/// or operating-system error text.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ProcessError {
    code: ProcessErrorCode,
    message: &'static str,
}

impl ProcessError {
    /// Construct the fixed public message for `code`.
    #[must_use]
    pub const fn new(code: ProcessErrorCode) -> Self {
        Self {
            code,
            message: code.message(),
        }
    }

    /// Stable failure classification.
    #[must_use]
    pub const fn code(&self) -> ProcessErrorCode {
        self.code
    }
}

impl From<processkit::Error> for ProcessError {
    fn from(error: processkit::Error) -> Self {
        use processkit::ErrorKind;

        let code = if error.output_overflow().is_some() {
            ProcessErrorCode::OutputLimit
        } else {
            match error.kind() {
                ErrorKind::NotFound => ProcessErrorCode::NotFound,
                ErrorKind::PermissionDenied => ProcessErrorCode::PermissionDenied,
                ErrorKind::Spawn => ProcessErrorCode::Spawn,
                ErrorKind::Cancelled => ProcessErrorCode::Cancelled,
                ErrorKind::Teardown => ProcessErrorCode::Teardown,
                ErrorKind::Unsupported => ProcessErrorCode::Unsupported,
                ErrorKind::Timeout
                | ErrorKind::Exit
                | ErrorKind::Signalled
                | ErrorKind::Predicate
                | ErrorKind::Other => ProcessErrorCode::Io,
                _ => ProcessErrorCode::Io,
            }
        };
        Self::new(code)
    }
}

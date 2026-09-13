//! Fixed configuration failures for the Claude Code process boundary.

/// Invalid local Claude runtime configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClaudeRuntimeConfigError {
    /// Working directory is not one absolute path.
    #[error("Claude Code runtime working directory is invalid")]
    InvalidWorkingDirectory,
    /// Program name/path is empty, oversized, or NUL-bearing.
    #[error("Claude Code runtime executable configuration is invalid")]
    InvalidProgram,
    /// Explicit process environment violates the subprocess boundary.
    #[error("Claude Code runtime environment configuration is invalid")]
    InvalidEnvironment,
    /// Supported version interval is empty or reversed.
    #[error("Claude Code runtime version policy is invalid")]
    InvalidVersionPolicy,
    /// Constant delegated-runtime descriptor failed validation.
    #[error("Claude Code runtime descriptor is invalid")]
    InvalidDescriptor,
}

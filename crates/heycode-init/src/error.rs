//! Stable initializer failures.

/// Preview or apply failure.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// Workspace root is absent, unreadable or not a directory.
    #[error("workspace root must be an accessible directory")]
    InvalidWorkspace,
    /// AGENTS.md is a symlink or another non-regular filesystem object.
    #[error("AGENTS.md must be a regular non-symlink file")]
    UnsafeTarget,
    /// Existing instructions exceed the bounded reader.
    #[error("AGENTS.md exceeds the {max_bytes}-byte initializer limit")]
    TooLarge {
        /// Maximum accepted file size.
        max_bytes: usize,
    },
    /// Existing instructions are not UTF-8 Markdown.
    #[error("AGENTS.md must contain valid UTF-8")]
    InvalidUtf8,
    /// Managed markers are missing, repeated or reversed.
    #[error("AGENTS.md has malformed heycode managed-section markers")]
    MalformedManagedSection,
    /// The workspace or file changed after preview.
    #[error("the /init preview is stale; run /init again")]
    StalePreview,
    /// Token syntax is not the closed lowercase-hex format.
    #[error("invalid /init preview token; run /init again")]
    InvalidToken,
    /// Command arguments do not match the preview/apply grammar.
    #[error("usage: /init [preview] | /init apply <token>")]
    Usage,
    /// Process-local initializer mutex was poisoned.
    #[error("AGENTS.md initializer is unavailable")]
    WriterUnavailable,
    /// One bounded filesystem operation failed.
    #[error("failed to {operation} AGENTS.md")]
    Io {
        /// Stable operation label; never a path or file content.
        operation: &'static str,
        /// Original source retained for downcasting/debugging.
        #[source]
        source: std::io::Error,
    },
}

impl InitError {
    pub(crate) fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io { operation, source }
    }
}

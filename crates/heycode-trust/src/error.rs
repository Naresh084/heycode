//! Fixed, path-free workspace-trust failures.

/// Workspace identity, persistence, lifecycle, or CAS failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceTrustError {
    /// The workspace root is absent, non-directory, non-UTF-8, or cannot be canonicalized.
    #[error("workspace root is invalid or unavailable")]
    InvalidWorkspace,
    /// The trust store has an unsupported schema, unsafe type, or malformed content.
    #[error("workspace trust store is invalid")]
    InvalidStore,
    /// An I/O or writer boundary could not durably load or commit the store.
    #[error("workspace trust store is unavailable")]
    StoreUnavailable,
    /// The caller based its mutation on an obsolete live snapshot.
    #[error("workspace trust changed (expected revision {expected}, actual {actual})")]
    Conflict {
        /// Caller-observed revision.
        expected: u64,
        /// Current authoritative revision.
        actual: u64,
    },
    /// Another store owner committed after this service loaded its generation.
    #[error("workspace trust store changed; reload before retrying")]
    StoreChanged,
    /// A monotonic live/store revision could not advance.
    #[error("workspace trust revision is exhausted")]
    RevisionExhausted,
    /// Unknown cannot be written as an affirmative session/persistent choice.
    #[error("unknown is not a trust decision; use reset")]
    InvalidDecision,
    /// Headless and ACP startup cannot prompt for an unknown workspace.
    #[error("workspace trust is required for non-interactive {frontend}")]
    NonInteractiveTrustRequired {
        /// Frontend that refused to prompt.
        frontend: crate::TrustFrontend,
    },
    /// The owning plugin/context was already shut down.
    #[error("workspace trust service is unavailable")]
    ServiceUnavailable,
    /// This platform has no audited owner-only persistent-store backend.
    #[error("persistent workspace trust is unsupported on this platform")]
    UnsupportedSecurity,
}

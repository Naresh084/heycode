//! Authorization registry, flow, cancellation, and commit failures.

/// Authorization failures safe to surface in product UI.
#[derive(Debug, thiserror::Error)]
pub enum AuthorizationError {
    /// Flow id was malformed.
    #[error("authorization flow id `{value}` must match [a-z][a-z0-9-]*")]
    InvalidFlowId {
        /// Rejected id.
        value: String,
    },
    /// Flow id already exists.
    #[error("authorization flow `{flow}` is already registered")]
    DuplicateFlow {
        /// Duplicate id.
        flow: String,
    },
    /// Requested flow is not registered.
    #[error("authorization flow `{flow}` is not registered")]
    UnknownFlow {
        /// Missing id.
        flow: String,
    },
    /// Registry mutex was poisoned.
    #[error("authorization registry is unavailable after a previous panic")]
    RegistryUnavailable,
    /// Cancellation won before credential commit.
    #[error("authorization cancelled")]
    Cancelled,
    /// Flow failed with safe actionable text.
    #[error("authorization flow `{flow}` failed ({code}): {message}")]
    Flow {
        /// Flow id.
        flow: String,
        /// Stable safe failure code.
        code: String,
        /// Redacted detail.
        message: String,
    },
    /// Credential service refused commit/readback.
    #[error("authorization credential commit failed: {message}")]
    CredentialCommit {
        /// Redacted credential error.
        message: String,
    },
    /// Readback did not prove the provider remains authoritative.
    #[error(
        "authorization wrote provider `{written}` but authoritative readback is `{authoritative}`"
    )]
    CommitNotAuthoritative {
        /// Provider that accepted the write.
        written: String,
        /// Provider visible at readback.
        authoritative: String,
    },
}

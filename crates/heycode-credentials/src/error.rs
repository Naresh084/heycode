//! Credential registry and provider failures.

use crate::CredentialSource;

/// Failures at credential reference/provider boundaries.
#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    /// Reference contained unsupported bytes or was empty.
    #[error("invalid credential reference `{value}`")]
    InvalidReference {
        /// Rejected reference.
        value: String,
    },
    /// Kebab-case id was malformed.
    #[error("invalid {what} `{value}`; expected [a-z][a-z0-9-]*")]
    InvalidId {
        /// Id category.
        what: &'static str,
        /// Rejected value.
        value: String,
    },
    /// Provider id already exists.
    #[error("credential provider `{provider}` is already registered")]
    DuplicateProvider {
        /// Duplicate id.
        provider: String,
    },
    /// Registry mutex was poisoned.
    #[error("credential registry is unavailable after a previous panic")]
    RegistryUnavailable,
    /// Provider boundary failed. Its free-form body is discarded because a
    /// provider may accidentally include secret material.
    #[error("credential provider `{provider}` failed")]
    Provider {
        /// Provider id.
        provider: String,
    },
    /// Provider's inspection and resolution disagreed.
    #[error("credential provider `{provider}` reported configured but returned no value")]
    InconsistentProvider {
        /// Provider id.
        provider: String,
    },
    /// Active configured provider shadows fallbacks but cannot be written.
    #[error(
        "credential reference is shadowed by read-only provider `{provider}` ({credential_source:?})"
    )]
    ShadowedReadOnly {
        /// Active provider id.
        provider: String,
        /// Active safe provenance.
        credential_source: Option<CredentialSource>,
    },
    /// No provider is configured or willing to accept a write.
    #[error("no writable credential provider is available for this reference")]
    NoWritableProvider,
}

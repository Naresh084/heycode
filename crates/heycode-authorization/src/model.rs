//! Safe flow descriptors, requests, grants, and receipts.

use heycode_credentials::{
    CredentialDescriptor, CredentialProviderId, CredentialQuery, CredentialSecret,
    CredentialValidation,
};
use tokio_util::sync::CancellationToken;

use crate::AuthorizationError;

/// Stable lowercase kebab-case authorization flow id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorizationFlowId(String);

impl AuthorizationFlowId {
    /// Validate a flow id.
    ///
    /// # Errors
    /// Malformed ids return [`AuthorizationError::InvalidFlowId`].
    pub fn new(value: impl Into<String>) -> Result<Self, AuthorizationError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        if !valid {
            return Err(AuthorizationError::InvalidFlowId { value });
        }
        Ok(Self(value))
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Non-zero caller correlation for one interactive authorization operation.
///
/// The id is safe metadata used only to route masked prompt notifications; it
/// never identifies or contains credential material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorizationOperationId(u64);

impl AuthorizationOperationId {
    /// Construct a validated non-zero operation id.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Stable numeric representation for local protocol correlation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Human interaction mechanism owned by a flow.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationMethod {
    /// Masked API key/token entry.
    ApiKey,
    /// Browser redirect OAuth.
    OAuth,
    /// Device-code flow.
    DeviceCode,
    /// Command/native runtime login.
    Command,
    /// Ambient cloud/local identity check.
    Ambient,
}

/// Safe catalog row for one flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationDescriptor {
    /// Stable flow id.
    pub id: AuthorizationFlowId,
    /// Human label.
    pub label: String,
    /// Interaction mechanism.
    pub method: AuthorizationMethod,
    /// Whether the flow needs a human dialog.
    pub interactive: bool,
    /// Non-secret credential query this flow owns.
    pub query: CredentialQuery,
}

/// One authorization invocation.
#[derive(Debug, Clone)]
pub struct AuthorizationRequest {
    /// Credential reference/kind being authorized.
    pub query: CredentialQuery,
    /// Optional interactive-front-end correlation.
    pub operation: Option<AuthorizationOperationId>,
    /// Single lifecycle token owned by the caller.
    pub cancellation: CancellationToken,
}

/// Secret grant returned by a flow before registry-owned commit.
pub struct AuthorizationGrant {
    secret: CredentialSecret,
    validation: CredentialValidation,
}

impl AuthorizationGrant {
    /// Create a grant. It carries no “success” flag; commit decides success.
    #[must_use]
    pub fn new(secret: CredentialSecret) -> Self {
        Self {
            secret,
            validation: CredentialValidation::Unknown,
        }
    }

    /// Grant proven valid by a provider-specific live check.
    #[must_use]
    pub fn validated(secret: CredentialSecret, checked_at_ms: u64) -> Self {
        Self {
            secret,
            validation: CredentialValidation::Valid { checked_at_ms },
        }
    }

    pub(crate) fn into_parts(self) -> (CredentialSecret, CredentialValidation) {
        (self.secret, self.validation)
    }
}

/// Safe typed flow failure returned before credential commit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct AuthorizationFlowFailure {
    /// Stable machine code (`unauthorized`, `host`, `model`, `network`, …).
    pub code: String,
    /// Safe actionable message with no provider body or secret.
    pub message: String,
}

impl AuthorizationFlowFailure {
    /// Construct a safe flow failure.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Safe proof returned only after write and authoritative readback.
#[derive(Debug)]
pub struct AuthorizationReceipt {
    /// Flow that produced the grant.
    pub flow: AuthorizationFlowId,
    /// Provider that durably accepted the write.
    pub committed_by: CredentialProviderId,
    /// Safe authoritative descriptor read after commit.
    pub credential: CredentialDescriptor,
}

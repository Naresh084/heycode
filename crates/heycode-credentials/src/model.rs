//! Safe credential identity and descriptor vocabulary.

use serde::Serialize;

use crate::CredentialsError;

/// Stable non-secret credential reference stored in settings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CredentialReference(String);

impl CredentialReference {
    /// Validate a reference identifier.
    ///
    /// # Errors
    /// Empty values, whitespace/control bytes, and punctuation outside
    /// `_-. :/` are rejected.
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialsError> {
        let value = value.into();
        let valid = value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && value.as_bytes().iter().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.' | b':' | b'/')
            });
        if !valid {
            return Err(CredentialsError::InvalidReference { value });
        }
        Ok(Self(value))
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Credential semantic kind (`api-key`, `oauth-token`, cloud chain, …).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CredentialKind(String);

impl CredentialKind {
    /// Validate a lowercase kebab-case kind.
    ///
    /// # Errors
    /// Malformed ids return [`CredentialsError::InvalidId`].
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialsError> {
        valid_kebab("credential kind", value.into()).map(Self)
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable id of one credential provider implementation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CredentialProviderId(String);

impl CredentialProviderId {
    /// Validate a lowercase kebab-case provider id.
    ///
    /// # Errors
    /// Malformed ids return [`CredentialsError::InvalidId`].
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialsError> {
        valid_kebab("credential provider id", value.into()).map(Self)
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Safe provenance of an active credential record.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// Explicit process environment.
    Environment,
    /// Historical OS-store provenance retained for metadata compatibility.
    /// heycode ships no provider that reads or writes an OS secret store.
    Keychain,
    /// Owner-only heycode home credential file.
    File,
    /// Command-produced secret.
    Command,
    /// Provider-native ambient cloud/local runtime chain.
    AmbientRuntime,
    /// Official delegated subscription runtime.
    SubscriptionRuntime,
}

/// Safe validation state; no server response body or secret is stored.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CredentialValidation {
    /// No live validation has completed.
    Unknown,
    /// Provider validation succeeded at this wall-clock instant.
    Valid {
        /// Unix epoch milliseconds.
        checked_at_ms: u64,
    },
    /// Provider validation failed with a stable safe reason code.
    Invalid {
        /// Unix epoch milliseconds.
        checked_at_ms: u64,
        /// Safe machine code, never raw provider text.
        reason: String,
    },
    /// Cached validation expired and must refresh before a health claim.
    Stale {
        /// Original validation instant.
        checked_at_ms: u64,
    },
}

/// One lookup request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct CredentialQuery {
    /// Non-secret reference.
    pub reference: CredentialReference,
    /// Expected semantic kind.
    pub kind: CredentialKind,
}

impl CredentialQuery {
    /// Construct a lookup request.
    #[must_use]
    pub fn new(reference: CredentialReference, kind: CredentialKind) -> Self {
        Self { reference, kind }
    }
}

/// Provider inspection result before secret resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialProviderState {
    /// Whether this provider currently holds the reference.
    pub(crate) configured: bool,
    /// Safe provenance when configured.
    pub(crate) source: Option<CredentialSource>,
    /// Whether this active provider can be written.
    pub(crate) writable: bool,
    /// Safe live-validation state.
    pub(crate) validation: CredentialValidation,
}

impl CredentialProviderState {
    /// Configured provider state.
    #[must_use]
    pub fn configured(source: CredentialSource, writable: bool) -> Self {
        Self {
            configured: true,
            source: Some(source),
            writable,
            validation: CredentialValidation::Unknown,
        }
    }

    /// Provider does not hold this reference but may accept future writes.
    #[must_use]
    pub fn unconfigured(writable: bool) -> Self {
        Self {
            configured: false,
            source: None,
            writable,
            validation: CredentialValidation::Unknown,
        }
    }

    /// Attach a safe validation result.
    #[must_use]
    pub fn with_validation(mut self, validation: CredentialValidation) -> Self {
        self.validation = validation;
        self
    }
}

/// Wire-safe credential status. It has no value/secret field by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialDescriptor {
    /// Non-secret reference.
    pub reference: CredentialReference,
    /// Semantic kind.
    pub kind: CredentialKind,
    /// Whether an active provider holds it.
    pub configured: bool,
    /// Active safe provenance.
    pub source: Option<CredentialSource>,
    /// Active provider id, or preferred writable provider while unconfigured.
    pub provider: Option<CredentialProviderId>,
    /// Whether a write may target the active/preferred provider.
    pub writable: bool,
    /// Safe validation state.
    pub validation: CredentialValidation,
}

impl CredentialDescriptor {
    /// Descriptor when no provider is configured or writable.
    #[must_use]
    pub fn unconfigured(query: CredentialQuery) -> Self {
        Self {
            reference: query.reference,
            kind: query.kind,
            configured: false,
            source: None,
            provider: None,
            writable: false,
            validation: CredentialValidation::Unknown,
        }
    }
}

fn valid_kebab(what: &'static str, value: String) -> Result<String, CredentialsError> {
    let bytes = value.as_bytes();
    let valid = bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
    if valid {
        Ok(value)
    } else {
        Err(CredentialsError::InvalidId { what, value })
    }
}

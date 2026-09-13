//! Explicitly exposed, zeroized secret value.

use secrecy::{ExposeSecret as _, SecretString};

/// Secret returned by a credential provider.
///
/// It is not serializable or displayable and zeroizes on drop through
/// [`secrecy::SecretString`]. Access is deliberately explicit.
pub struct CredentialSecret(SecretString);

impl CredentialSecret {
    /// Wrap a provider-returned secret.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// Explicitly expose the secret at the operation boundary that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for CredentialSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialSecret([REDACTED])")
    }
}

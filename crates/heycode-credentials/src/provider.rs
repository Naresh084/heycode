//! Credential provider contract.

use crate::{CredentialProviderId, CredentialProviderState, CredentialQuery, CredentialSecret};

/// One source capable of inspecting and resolving credential references.
pub trait CredentialProvider: Send + Sync {
    /// Stable provider id.
    fn id(&self) -> &CredentialProviderId;
    /// Lower number wins. Environment uses 0.
    fn precedence(&self) -> u16;
    /// Inspect safe configured/writable/provenance state without resolving.
    ///
    /// # Errors
    /// Return redacted actionable text only.
    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String>;
    /// Resolve the secret if configured.
    ///
    /// # Errors
    /// Return redacted actionable text only.
    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String>;
    /// Persist a new secret when inspection reported `writable = true`.
    ///
    /// # Errors
    /// Read-only providers keep the default failure. Writable providers return
    /// redacted actionable text only.
    fn write(&self, _query: &CredentialQuery, _secret: &CredentialSecret) -> Result<(), String> {
        Err("provider is read-only".to_owned())
    }
    /// Delete a configured secret when inspection reported writable.
    ///
    /// # Errors
    /// Read-only providers keep the default failure. Writable providers return
    /// redacted actionable text only.
    fn delete(&self, _query: &CredentialQuery) -> Result<(), String> {
        Err("provider is read-only".to_owned())
    }
}

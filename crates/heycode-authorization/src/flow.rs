//! Authorization flow contract.

use async_trait::async_trait;

use crate::{
    AuthorizationDescriptor, AuthorizationFlowFailure, AuthorizationGrant, AuthorizationRequest,
};

/// One interactive or ambient authorization implementation.
#[async_trait]
pub trait AuthorizationFlow: Send + Sync {
    /// Safe catalog metadata.
    fn descriptor(&self) -> AuthorizationDescriptor;
    /// Validate the authoritative saved credential without prompting or writing.
    ///
    /// # Errors
    /// Flows without a live credential probe fail closed.
    async fn validate_existing(
        &self,
        _secret: &heycode_credentials::CredentialSecret,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<(), AuthorizationFlowFailure> {
        Err(AuthorizationFlowFailure::new(
            "validation-unavailable",
            "This connection cannot validate a saved credential",
        ))
    }

    /// Produce a secret grant, honoring the supplied cancellation token.
    ///
    /// # Errors
    /// Return redacted actionable text only. The registry owns persistence.
    async fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationFlowFailure>;
}

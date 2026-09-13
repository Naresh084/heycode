//! Route-scoped credential resolution performed at the operation that needs
//! the secret.
//!
//! [`CredentialsService::resolve`] answers `Option`, which forces every
//! caller to invent its own "absent" failure; the ones in this workspace threw
//! the reason away. [`CredentialsService::resolve_route`] gives that answer a
//! type whose text names the requested route and nothing else.

use crate::{CredentialQuery, CredentialSecret, CredentialsService};

/// Failure to resolve one exact credential route.
///
/// Every variant names the requested reference and nothing else. No variant
/// can name another route, report whether another route holds a credential,
/// or carry secret material: a caller that asked for `A` learns only about
/// `A`. That is a security property — "route A has no key, but B does" is a
/// fact about the user's other accounts, and an error is the wrong place to
/// publish it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialResolutionError {
    /// No active provider holds this reference.
    #[error("no credential is configured for `{reference}`")]
    Missing {
        /// Requested non-secret reference.
        reference: String,
    },
    /// A provider holds this reference but resolution failed.
    #[error("credential `{reference}` is unavailable: {message}")]
    Unavailable {
        /// Requested non-secret reference.
        reference: String,
        /// Redacted registry/provider detail.
        message: String,
    },
    /// The consulted resolver is bound to a different route than the one the
    /// operation asked for. The other route is deliberately not named.
    #[error("no credential resolver is bound to `{reference}`")]
    RouteMismatch {
        /// Requested non-secret reference.
        reference: String,
    },
}

impl CredentialResolutionError {
    /// The requested non-secret reference. This is the only route any variant
    /// knows about.
    #[must_use]
    pub fn reference(&self) -> &str {
        match self {
            Self::Missing { reference }
            | Self::Unavailable { reference, .. }
            | Self::RouteMismatch { reference } => reference,
        }
    }
}

impl CredentialsService {
    /// Resolve one exact route for the operation that needs it now.
    ///
    /// Every call performs a full precedence walk for the caller's exact
    /// query. No secret is retained between calls, so a key rotated in the
    /// backing store reaches the next request without recomposition.
    ///
    /// Resolution is scoped to one route: the query is passed to providers
    /// unchanged and no second reference is ever attempted. A route with no
    /// credential therefore fails instead of quietly succeeding with another
    /// route's secret.
    ///
    /// # Errors
    /// [`CredentialResolutionError::Missing`] when no active provider holds
    /// the reference, [`CredentialResolutionError::Unavailable`] when the
    /// registry or a provider failed. Both name only `query.reference`.
    pub fn resolve_route(
        &self,
        query: &CredentialQuery,
    ) -> Result<CredentialSecret, CredentialResolutionError> {
        let reference = query.reference.as_str().to_owned();
        match self.resolve(query) {
            Ok(Some(secret)) => Ok(secret),
            Ok(None) => Err(CredentialResolutionError::Missing { reference }),
            Err(error) => Err(CredentialResolutionError::Unavailable {
                reference,
                message: error.to_string(),
            }),
        }
    }
}

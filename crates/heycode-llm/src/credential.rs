//! Per-operation credential resolution for authenticated routes.
//!
//! An authenticated route holds a **reference**, not a value. Adapters keep a
//! [`RouteCredential`] and call [`RouteCredential::acquire`] once per
//! operation, so nothing here retains a secret across operations: a key
//! rotated in the environment, keychain, credential file or credential
//! command reaches the next request without recomposing the world.
//!
//! Resolution is bound to exactly one route. A [`CredentialResolver`] answers
//! for its own route or fails; it is never consulted for another route's
//! reference, and a mismatch is refused before the resolver is called. A
//! request that cannot be authenticated as the route it claims must fail —
//! sending it authenticated as a *different* route would be an incident, not
//! a convenience.

use std::sync::Arc;

use heycode_credentials::{
    CredentialQuery, CredentialResolutionError, CredentialSecret, CredentialsService,
};

use crate::inference::{AdapterOwnedAuth, AuthenticationBinding, CredentialHandle};

/// Resolves the secret of exactly one credential route, at the operation that
/// needs it.
pub trait CredentialResolver: Send + Sync {
    /// The exact route this resolver is permanently bound to.
    fn route(&self) -> &CredentialHandle;

    /// Resolve the secret for `route`, now.
    ///
    /// Implementations must answer for their own route or fail. Returning
    /// another route's secret is a cross-route fallback: the caller would send
    /// a request it believes is authenticated as `route` under someone else's
    /// credential.
    ///
    /// # Errors
    /// Absent, unreadable or foreign-route credentials fail with text naming
    /// only the requested reference.
    fn resolve(
        &self,
        route: &CredentialHandle,
    ) -> Result<CredentialSecret, CredentialResolutionError>;
}

/// One credential resolved for the lifetime of exactly one operation.
///
/// This is the whole cache: a secret lives from the acquisition that starts an
/// operation until that operation's value is dropped, and is shared by that
/// operation's retry attempts. Nothing outside the operation can reach it, so
/// the invalidation rule needs no clock — the next operation resolves again.
pub struct OperationCredential(Arc<CredentialSecret>);

impl OperationCredential {
    /// Expose the secret at the transport boundary that must send it.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

impl std::fmt::Debug for OperationCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OperationCredential([REDACTED])")
    }
}

#[derive(Clone)]
enum Binding {
    /// An owner-supplied literal bound for this adapter's lifetime.
    Fixed(Arc<CredentialSecret>),
    /// A non-secret reference resolved once per operation.
    PerOperation {
        route: CredentialHandle,
        resolver: Arc<dyn CredentialResolver>,
    },
}

/// The credential one authenticated route sends.
///
/// [`RouteCredential::fixed`] preserves the historical behaviour of a key
/// captured at composition; [`RouteCredential::registry`] and
/// [`RouteCredential::per_operation`] resolve per operation instead. The two
/// are distinguishable without exposing anything:
/// [`RouteCredential::binding`] reports `AdapterOwned` for the first and
/// `Credential(reference)` for the second, so a durable request snapshot
/// records which one actually authenticated the call.
#[derive(Clone)]
pub struct RouteCredential(Binding);

impl RouteCredential {
    /// A literal secret bound for the adapter's lifetime.
    ///
    /// A key captured this way is never re-read: rotation reaches this route
    /// only when the adapter is rebuilt. Prefer [`Self::registry`].
    #[must_use]
    pub fn fixed(secret: impl Into<String>) -> Self {
        Self(Binding::Fixed(Arc::new(CredentialSecret::new(
            secret.into(),
        ))))
    }

    /// Resolve one exact route through `resolver` on every operation.
    ///
    /// `route` is the reference this route authenticates as. A resolver bound
    /// to a different reference is refused at [`Self::acquire`] and never
    /// consulted.
    #[must_use]
    pub fn per_operation(route: CredentialHandle, resolver: Arc<dyn CredentialResolver>) -> Self {
        Self(Binding::PerOperation { route, resolver })
    }

    /// Resolve one exact route through the composed credential registry on
    /// every operation.
    #[must_use]
    pub fn registry(credentials: CredentialsService, query: CredentialQuery) -> Self {
        let route = CredentialHandle::from_reference(&query.reference);
        Self::per_operation(
            route.clone(),
            Arc::new(RegistryCredential {
                route,
                query,
                credentials,
            }),
        )
    }

    /// Secret-free authentication binding this credential contributes to a
    /// resolved call.
    #[must_use]
    pub fn binding(&self) -> AuthenticationBinding {
        match &self.0 {
            Binding::Fixed(_) => AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
            Binding::PerOperation { route, .. } => AuthenticationBinding::Credential(route.clone()),
        }
    }

    /// The resolver behind a per-operation route, for callers that drive one
    /// resolution explicitly rather than through an adapter dispatch.
    #[must_use]
    pub fn resolver(&self) -> Option<Arc<dyn CredentialResolver>> {
        match &self.0 {
            Binding::Fixed(_) => None,
            Binding::PerOperation { resolver, .. } => Some(resolver.clone()),
        }
    }

    /// The non-secret route reference when this credential resolves per
    /// operation.
    #[must_use]
    pub fn route(&self) -> Option<&CredentialHandle> {
        match &self.0 {
            Binding::Fixed(_) => None,
            Binding::PerOperation { route, .. } => Some(route),
        }
    }

    /// Obtain the secret for exactly one operation.
    ///
    /// Call this once per operation, before the first attempt: every attempt
    /// of that operation then reuses the same value, so a retry storm cannot
    /// turn into a keychain-prompt storm. The next operation calls again and
    /// therefore observes any rotation.
    ///
    /// # Errors
    /// A per-operation route with no reachable credential, an unreadable
    /// store, or a resolver bound to another route.
    pub fn acquire(&self) -> Result<OperationCredential, CredentialResolutionError> {
        match &self.0 {
            Binding::Fixed(secret) => Ok(OperationCredential(secret.clone())),
            Binding::PerOperation { route, resolver } => {
                if resolver.route() != route {
                    return Err(CredentialResolutionError::RouteMismatch {
                        reference: route.as_str().to_owned(),
                    });
                }
                resolver
                    .resolve(route)
                    .map(|secret| OperationCredential(Arc::new(secret)))
            }
        }
    }

    /// Whether a fixed secret is blank. A per-operation route reports `false`:
    /// its presence is an operation-time fact, not a construction-time one.
    pub(crate) fn fixed_is_blank(&self) -> bool {
        match &self.0 {
            Binding::Fixed(secret) => secret.expose().trim().is_empty(),
            Binding::PerOperation { .. } => false,
        }
    }

    /// A construction-time stand-in for header-shape validation. It is never
    /// sent: a per-operation route has no secret until an operation asks.
    pub(crate) fn probe_value(&self) -> &str {
        match &self.0 {
            Binding::Fixed(secret) => secret.expose(),
            Binding::PerOperation { .. } => "unresolved",
        }
    }
}

impl std::fmt::Debug for RouteCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Binding::Fixed(_) => formatter.write_str("RouteCredential::Fixed([REDACTED])"),
            Binding::PerOperation { route, .. } => write!(
                formatter,
                "RouteCredential::PerOperation({})",
                route.as_str()
            ),
        }
    }
}

/// Resolves one exact route through the composed credential registry.
struct RegistryCredential {
    route: CredentialHandle,
    query: CredentialQuery,
    credentials: CredentialsService,
}

impl CredentialResolver for RegistryCredential {
    fn route(&self) -> &CredentialHandle {
        &self.route
    }

    fn resolve(
        &self,
        route: &CredentialHandle,
    ) -> Result<CredentialSecret, CredentialResolutionError> {
        // The registry walk itself is route-scoped, but this resolver holds a
        // secret-bearing query: answering a call for a foreign route with it
        // is exactly the fallback this row forbids.
        if route != &self.route {
            return Err(CredentialResolutionError::RouteMismatch {
                reference: route.as_str().to_owned(),
            });
        }
        self.credentials.resolve_route(&self.query)
    }
}

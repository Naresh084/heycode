//! Effect-owned flow registry and commit coordinator.

use std::sync::{Arc, Mutex, Weak};

use heycode_core::Context;
use heycode_credentials::CredentialsService;
use tokio_util::sync::CancellationToken;

use crate::{
    AuthorizationDescriptor, AuthorizationError, AuthorizationFlow, AuthorizationFlowId,
    AuthorizationOperationId, AuthorizationReceipt, AuthorizationRequest,
};

const VALIDATION_TTL_MS: u64 = 15 * 60 * 1_000;

struct FlowEntry {
    flow: Arc<dyn AuthorizationFlow>,
    token: Arc<()>,
}

struct AuthorizationInner {
    flows: Mutex<Vec<FlowEntry>>,
    credentials: Arc<CredentialsService>,
}

/// Shared authorization flow registry.
#[derive(Clone)]
pub struct AuthorizationService {
    inner: Arc<AuthorizationInner>,
}

impl AuthorizationService {
    /// Check the currently authoritative credential using its provider-owned flow.
    /// This does not write credentials or reuse a catalog as proof of authentication.
    ///
    /// # Errors
    /// Missing credentials, rejected keys, unavailable probes or cancellation.
    pub async fn validate_existing(
        &self,
        id: &AuthorizationFlowId,
        cancellation: CancellationToken,
    ) -> Result<(), AuthorizationError> {
        if cancellation.is_cancelled() {
            return Err(AuthorizationError::Cancelled);
        }
        let flow = self.flow(id)?;
        let failure = |code: &str, message: &str| AuthorizationError::Flow {
            flow: id.as_str().to_owned(),
            code: code.into(),
            message: message.into(),
        };
        let secret = self
            .inner
            .credentials
            .resolve(&flow.descriptor().query)
            .map_err(|_| failure("credential", "The saved credential could not be read"))?
            .ok_or_else(|| failure("unauthorized", "No credential is configured"))?;
        flow.validate_existing(&secret, cancellation.clone())
            .await
            .map_err(|error| failure(&error.code, &error.message))?;
        if cancellation.is_cancelled() {
            return Err(AuthorizationError::Cancelled);
        }
        Ok(())
    }

    /// Authorize an operation-scoped flow without installing a persistent registry row.
    ///
    /// # Errors
    /// Cancellation, validation or authoritative credential commit failure.
    pub async fn authorize_once(
        &self,
        flow: &dyn AuthorizationFlow,
        operation: Option<AuthorizationOperationId>,
        cancellation: CancellationToken,
    ) -> Result<AuthorizationReceipt, AuthorizationError> {
        self.run_flow(flow, flow.descriptor().query, operation, cancellation)
            .await
    }
    /// Build over the authoritative credential service.
    #[must_use]
    pub fn new(credentials: Arc<CredentialsService>) -> Self {
        Self {
            inner: Arc::new(AuthorizationInner {
                flows: Mutex::new(Vec::new()),
                credentials,
            }),
        }
    }

    /// Register one unique flow as a context effect.
    ///
    /// # Errors
    /// Duplicate ids or poisoned registry fail before publication.
    pub fn register(
        &self,
        context: &Context,
        flow: Arc<dyn AuthorizationFlow>,
    ) -> Result<(), AuthorizationError> {
        let descriptor = flow.descriptor();
        let mut flows = self
            .inner
            .flows
            .lock()
            .map_err(|_| AuthorizationError::RegistryUnavailable)?;
        if flows
            .iter()
            .any(|entry| entry.flow.descriptor().id == descriptor.id)
        {
            return Err(AuthorizationError::DuplicateFlow {
                flow: descriptor.id.as_str().to_owned(),
            });
        }
        let token = Arc::new(());
        flows.push(FlowEntry {
            flow,
            token: token.clone(),
        });
        flows.sort_by_key(|entry| entry.flow.descriptor().id);
        drop(flows);
        let registration = FlowRegistration {
            inner: Arc::downgrade(&self.inner),
            id: descriptor.id,
            token,
            active: true,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Safe flow catalog in id order.
    ///
    /// # Errors
    /// Poisoned registry fails loud.
    pub fn descriptors(&self) -> Result<Vec<AuthorizationDescriptor>, AuthorizationError> {
        let flows = self
            .inner
            .flows
            .lock()
            .map_err(|_| AuthorizationError::RegistryUnavailable)?;
        Ok(flows.iter().map(|entry| entry.flow.descriptor()).collect())
    }

    /// Inspect safe credential state before deciding whether sign-in is needed.
    /// This never resolves or exposes the credential value.
    ///
    /// # Errors
    /// Credential provider inspection or registry failure.
    pub fn credential_state(
        &self,
        query: &heycode_credentials::CredentialQuery,
    ) -> Result<heycode_credentials::CredentialDescriptor, heycode_credentials::CredentialsError>
    {
        self.inner.credentials.describe(query)
    }

    /// Run one flow and return proof only after committed authoritative readback.
    ///
    /// # Errors
    /// Unknown flow, pre/post-flow cancellation, flow failure, credential
    /// commit/readback failure, or precedence change.
    pub async fn authorize(
        &self,
        id: &AuthorizationFlowId,
        query: heycode_credentials::CredentialQuery,
        cancellation: CancellationToken,
    ) -> Result<AuthorizationReceipt, AuthorizationError> {
        self.authorize_correlated(id, query, None, cancellation)
            .await
    }

    /// Run one flow with a safe caller correlation for interactive prompt
    /// routing and return proof only after committed authoritative readback.
    ///
    /// # Errors
    /// The same registry, cancellation, flow, commit, and readback failures as
    /// [`Self::authorize`].
    pub async fn authorize_correlated(
        &self,
        id: &AuthorizationFlowId,
        query: heycode_credentials::CredentialQuery,
        operation: Option<AuthorizationOperationId>,
        cancellation: CancellationToken,
    ) -> Result<AuthorizationReceipt, AuthorizationError> {
        if cancellation.is_cancelled() {
            return Err(AuthorizationError::Cancelled);
        }
        let flow = self.flow(id)?;
        self.run_flow(flow.as_ref(), query, operation, cancellation)
            .await
    }

    async fn run_flow(
        &self,
        flow: &dyn AuthorizationFlow,
        query: heycode_credentials::CredentialQuery,
        operation: Option<AuthorizationOperationId>,
        cancellation: CancellationToken,
    ) -> Result<AuthorizationReceipt, AuthorizationError> {
        if cancellation.is_cancelled() {
            return Err(AuthorizationError::Cancelled);
        }
        let id = flow.descriptor().id;
        let grant = flow
            .authorize(AuthorizationRequest {
                query: query.clone(),
                operation,
                cancellation: cancellation.clone(),
            })
            .await
            .map_err(|failure| AuthorizationError::Flow {
                flow: id.as_str().to_owned(),
                code: failure.code,
                message: failure.message,
            })?;
        if cancellation.is_cancelled() {
            return Err(AuthorizationError::Cancelled);
        }
        let (secret, validation) = grant.into_parts();
        let committed_by = self
            .inner
            .credentials
            .write(&query, &secret)
            .map_err(|error| AuthorizationError::CredentialCommit {
                message: error.to_string(),
            })?;
        self.inner
            .credentials
            .record_validation(
                &query,
                &committed_by,
                validation,
                &secret,
                VALIDATION_TTL_MS,
            )
            .map_err(|error| AuthorizationError::CredentialCommit {
                message: error.to_string(),
            })?;
        let credential = self.inner.credentials.describe(&query).map_err(|error| {
            AuthorizationError::CredentialCommit {
                message: error.to_string(),
            }
        })?;
        let authoritative = credential
            .provider
            .as_ref()
            .map_or("none", |provider| provider.as_str());
        if !credential.configured || credential.provider.as_ref() != Some(&committed_by) {
            return Err(AuthorizationError::CommitNotAuthoritative {
                written: committed_by.as_str().to_owned(),
                authoritative: authoritative.to_owned(),
            });
        }
        Ok(AuthorizationReceipt {
            flow: id.clone(),
            committed_by,
            credential,
        })
    }

    fn flow(
        &self,
        id: &AuthorizationFlowId,
    ) -> Result<Arc<dyn AuthorizationFlow>, AuthorizationError> {
        let flows = self
            .inner
            .flows
            .lock()
            .map_err(|_| AuthorizationError::RegistryUnavailable)?;
        flows
            .iter()
            .find(|entry| entry.flow.descriptor().id == *id)
            .map(|entry| entry.flow.clone())
            .ok_or_else(|| AuthorizationError::UnknownFlow {
                flow: id.as_str().to_owned(),
            })
    }
}

struct FlowRegistration {
    inner: Weak<AuthorizationInner>,
    id: AuthorizationFlowId,
    token: Arc<()>,
    active: bool,
}

impl FlowRegistration {
    fn remove(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut flows) = inner.flows.lock() else {
            return;
        };
        flows.retain(|entry| {
            entry.flow.descriptor().id != self.id || !Arc::ptr_eq(&entry.token, &self.token)
        });
    }
}

impl Drop for FlowRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}

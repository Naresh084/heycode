//! Flow uniqueness, cancellation, and committed-write receipt contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_authorization::{
    AuthorizationDescriptor, AuthorizationError, AuthorizationFlow, AuthorizationFlowFailure,
    AuthorizationFlowId, AuthorizationGrant, AuthorizationMethod, AuthorizationRequest,
    AuthorizationService, SERVICE_AUTHORIZATION, authorization_plugin,
};
use heycode_core::compose;
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
    SERVICE_CREDENTIALS, credentials_plugin,
};
use heycode_settings::{SettingsDocuments, settings_plugin};
use tokio_util::sync::CancellationToken;

struct WritableProvider {
    id: CredentialProviderId,
    writes: Arc<AtomicUsize>,
    value: std::sync::Mutex<Option<String>>,
}

impl CredentialProvider for WritableProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }
    fn precedence(&self) -> u16 {
        10
    }
    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(if self.value.lock().unwrap().is_some() {
            CredentialProviderState::configured(CredentialSource::Keychain, true)
        } else {
            CredentialProviderState::unconfigured(true)
        })
    }
    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(self
            .value
            .lock()
            .unwrap()
            .clone()
            .map(CredentialSecret::new))
    }
    fn write(&self, _query: &CredentialQuery, secret: &CredentialSecret) -> Result<(), String> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        *self.value.lock().unwrap() = Some(secret.expose().to_owned());
        Ok(())
    }
}

struct GrantFlow {
    id: AuthorizationFlowId,
    calls: Arc<AtomicUsize>,
    cancel_before_return: bool,
}

#[async_trait]
impl AuthorizationFlow for GrantFlow {
    fn descriptor(&self) -> AuthorizationDescriptor {
        AuthorizationDescriptor {
            id: self.id.clone(),
            label: "Test API key".to_owned(),
            method: AuthorizationMethod::ApiKey,
            interactive: true,
            query: query(),
        }
    }

    async fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationFlowFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.cancel_before_return {
            request.cancellation.cancel();
        }
        Ok(AuthorizationGrant::new(CredentialSecret::new(
            "authorized-secret",
        )))
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("OPENROUTER_API_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn world() -> (
    heycode_core::Context,
    Arc<AuthorizationService>,
    Arc<AtomicUsize>,
) {
    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        authorization_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    let credentials = context
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    credentials
        .register(
            &context,
            Arc::new(WritableProvider {
                id: CredentialProviderId::new("test-writer").unwrap(),
                writes: writes.clone(),
                value: std::sync::Mutex::new(None),
            }),
        )
        .unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    (context, authorization, writes)
}

#[tokio::test]
async fn successful_flow_returns_receipt_only_after_committed_readback() {
    let (mut context, service, writes) = world();
    let calls = Arc::new(AtomicUsize::new(0));
    let id = AuthorizationFlowId::new("openrouter-api-key").unwrap();
    service
        .register(
            &context,
            Arc::new(GrantFlow {
                id: id.clone(),
                calls: calls.clone(),
                cancel_before_return: false,
            }),
        )
        .unwrap();
    let receipt = service
        .authorize(&id, query(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(receipt.credential.configured);
    assert_eq!(receipt.committed_by.as_str(), "test-writer");
    let json = format!("{receipt:?}");
    assert!(!json.contains("authorized-secret"));
    context.shutdown();
}

#[tokio::test]
async fn one_operation_flow_commits_without_installing_a_registry_row() {
    let (_context, service, writes) = world();
    let calls = Arc::new(AtomicUsize::new(0));
    let flow = GrantFlow {
        id: AuthorizationFlowId::new("endpoint-key").unwrap(),
        calls: calls.clone(),
        cancel_before_return: false,
    };
    let receipt = service
        .authorize_once(&flow, None, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(receipt.flow.as_str(), "endpoint-key");
    assert!(receipt.credential.configured);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(service.descriptors().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_before_or_after_flow_prevents_commit() {
    let (mut context, service, writes) = world();
    let calls = Arc::new(AtomicUsize::new(0));
    let id = AuthorizationFlowId::new("cancel-api-key").unwrap();
    service
        .register(
            &context,
            Arc::new(GrantFlow {
                id: id.clone(),
                calls: calls.clone(),
                cancel_before_return: true,
            }),
        )
        .unwrap();

    let pre_cancelled = CancellationToken::new();
    pre_cancelled.cancel();
    assert!(matches!(
        service.authorize(&id, query(), pre_cancelled).await,
        Err(AuthorizationError::Cancelled)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    assert!(matches!(
        service
            .authorize(&id, query(), CancellationToken::new())
            .await,
        Err(AuthorizationError::Cancelled)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    context.shutdown();
}

#[test]
fn duplicate_flow_key_fails_and_shutdown_unregisters() {
    let (mut context, service, _writes) = world();
    let id = AuthorizationFlowId::new("duplicate-flow").unwrap();
    let flow = || {
        Arc::new(GrantFlow {
            id: id.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
            cancel_before_return: false,
        }) as Arc<dyn AuthorizationFlow>
    };
    service.register(&context, flow()).unwrap();
    assert!(matches!(
        service.register(&context, flow()),
        Err(AuthorizationError::DuplicateFlow { .. })
    ));
    context.shutdown();
    assert!(service.descriptors().unwrap().is_empty());
}

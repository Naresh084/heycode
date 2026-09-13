//! Masking, commit gating, and provider classification contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_authorization::{
    AuthorizationError, AuthorizationFlowId, AuthorizationService, SERVICE_AUTHORIZATION,
    authorization_plugin,
};
use heycode_authorization_api_key::{
    ApiKeyAuthorizationFlow, ApiKeyFlowConfig, ApiKeyValidationFailure, ApiKeyValidator,
    HttpApiKeyValidator, InteractiveSecretPrompt, SecretPrompt, SecretPromptNotification,
    SecretPromptRequest,
};
use heycode_core::compose;
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialValidation,
    CredentialsService, SERVICE_CREDENTIALS, credentials_plugin,
};
use heycode_settings::{SettingsDocuments, settings_plugin};
use tokio_util::sync::CancellationToken;

struct Prompt {
    masked: Arc<AtomicBool>,
    secret: &'static str,
}

#[async_trait]
impl SecretPrompt for Prompt {
    async fn prompt(
        &self,
        request: SecretPromptRequest,
        _cancellation: CancellationToken,
    ) -> Result<CredentialSecret, String> {
        self.masked.store(request.masked, Ordering::SeqCst);
        Ok(CredentialSecret::new(self.secret))
    }
}

struct Validator(Result<(), ApiKeyValidationFailure>);

#[async_trait]
impl ApiKeyValidator for Validator {
    async fn validate(
        &self,
        _secret: &CredentialSecret,
        _cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        self.0
    }
}

struct Writer {
    id: CredentialProviderId,
    writes: Arc<AtomicUsize>,
    configured: AtomicBool,
    saved: std::sync::Mutex<Option<String>>,
}

impl CredentialProvider for Writer {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }
    fn precedence(&self) -> u16 {
        10
    }
    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(if self.configured.load(Ordering::SeqCst) {
            CredentialProviderState::configured(CredentialSource::Keychain, true)
        } else {
            CredentialProviderState::unconfigured(true)
        })
    }
    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(self
            .saved
            .lock()
            .unwrap()
            .as_ref()
            .map(CredentialSecret::new))
    }
    fn write(&self, _query: &CredentialQuery, secret: &CredentialSecret) -> Result<(), String> {
        *self.saved.lock().unwrap() = Some(secret.expose().to_owned());
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.configured.store(true, Ordering::SeqCst);
        Ok(())
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("OPENROUTER_API_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn stack() -> (
    heycode_core::Context,
    Arc<AuthorizationService>,
    Arc<AtomicUsize>,
) {
    let context = compose(&[
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        authorization_plugin(),
    ])
    .unwrap();
    let credentials = context
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    credentials
        .register(
            &context,
            Arc::new(Writer {
                id: CredentialProviderId::new("test-keychain").unwrap(),
                writes: writes.clone(),
                configured: AtomicBool::new(false),
                saved: std::sync::Mutex::new(None),
            }),
        )
        .unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    (context, authorization, writes)
}

fn flow(
    prompt: Arc<dyn SecretPrompt>,
    validator: Arc<dyn ApiKeyValidator>,
) -> ApiKeyAuthorizationFlow {
    ApiKeyAuthorizationFlow::new(
        ApiKeyFlowConfig {
            id: AuthorizationFlowId::new("openrouter-api-key").unwrap(),
            label: "OpenRouter API key".to_owned(),
            query: query(),
            prompt: "Paste your OpenRouter API key".to_owned(),
        },
        prompt,
        validator,
    )
}

#[tokio::test]
async fn masked_validated_input_commits_and_records_valid_evidence() {
    let (mut context, authorization, writes) = stack();
    let masked = Arc::new(AtomicBool::new(false));
    let api_flow = flow(
        Arc::new(Prompt {
            masked: masked.clone(),
            secret: "sk-or-valid",
        }),
        Arc::new(Validator(Ok(()))),
    );
    let id = api_flow.id().clone();
    authorization
        .register(&context, Arc::new(api_flow))
        .unwrap();
    let receipt = authorization
        .authorize(&id, query(), CancellationToken::new())
        .await
        .unwrap();
    assert!(masked.load(Ordering::SeqCst));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(matches!(
        receipt.credential.validation,
        CredentialValidation::Valid { .. }
    ));
    context.shutdown();
}

#[tokio::test]
async fn validation_failure_code_surfaces_and_never_commits() {
    for failure in [
        ApiKeyValidationFailure::Unauthorized,
        ApiKeyValidationFailure::Host,
        ApiKeyValidationFailure::Model,
        ApiKeyValidationFailure::Network,
    ] {
        let (mut context, authorization, writes) = stack();
        let api_flow = flow(
            Arc::new(Prompt {
                masked: Arc::new(AtomicBool::new(false)),
                secret: "bad",
            }),
            Arc::new(Validator(Err(failure))),
        );
        let id = api_flow.id().clone();
        authorization
            .register(&context, Arc::new(api_flow))
            .unwrap();
        let error = authorization
            .authorize(&id, query(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AuthorizationError::Flow { ref code, .. } if code == failure.code()
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        context.shutdown();
    }
}

async fn one_response(status: u16, body: &'static str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 4096];
        let _ = socket.read(&mut request).await.unwrap();
        let reason = if status == 200 { "OK" } else { "Error" };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn http_validator_classifies_status_and_model_absence() {
    for (status, expected) in [
        (401, ApiKeyValidationFailure::Unauthorized),
        (404, ApiKeyValidationFailure::Host),
        (500, ApiKeyValidationFailure::Network),
    ] {
        let url = one_response(status, "{}").await;
        let validator = HttpApiKeyValidator::new(format!("{url}/key"), None, None).unwrap();
        assert_eq!(
            validator
                .validate(&CredentialSecret::new("probe"), CancellationToken::new())
                .await
                .unwrap_err(),
            expected
        );
    }

    let url = one_response(200, r#"{"data":[{"id":"another/model"}]}"#).await;
    let validator = HttpApiKeyValidator::new(
        format!("{url}/models"),
        None,
        Some("required/model".to_owned()),
    )
    .unwrap();
    assert_eq!(
        validator
            .validate(&CredentialSecret::new("probe"), CancellationToken::new())
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Model
    );
}

#[tokio::test]
async fn live_openrouter_invalid_key_is_unauthorized_when_enabled() {
    if std::env::var_os("HEYCODE_E2E").is_none() {
        return;
    }
    let validator = HttpApiKeyValidator::openrouter(None).unwrap();
    assert_eq!(
        validator
            .validate(
                &CredentialSecret::new("sk-or-invalid-heycode-validation-probe"),
                CancellationToken::new(),
            )
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Unauthorized
    );
}

#[tokio::test]
async fn interactive_prompt_emits_only_safe_metadata_and_returns_answered_secret() {
    let bus = heycode_core::EventBus::default();
    let prompt = InteractiveSecretPrompt::new(bus.clone());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    bus.on::<SecretPromptNotification>(move |event| {
        let _ = sender.send(event.clone());
    });
    let prompt_task = {
        let prompt = prompt.clone();
        tokio::spawn(async move {
            prompt
                .prompt(
                    SecretPromptRequest {
                        error: None,
                        prompt: "Paste key".to_owned(),
                        query: query(),
                        operation: None,
                        masked: true,
                    },
                    CancellationToken::new(),
                )
                .await
        })
    };
    let requested = receiver.recv().await.unwrap();
    let id = match requested {
        SecretPromptNotification::Requested { id, masked, .. } => {
            assert!(masked);
            id
        }
        other => panic!("unexpected event: {other:?}"),
    };
    let debug = format!("{requested:?}");
    assert!(!debug.contains("secret"));
    assert!(prompt.answer(id, CredentialSecret::new("broker-secret")));
    assert_eq!(
        prompt_task.await.unwrap().unwrap().expose(),
        "broker-secret"
    );
}

#[tokio::test]
async fn dropping_prompt_future_removes_pending_answer_authority() {
    let bus = heycode_core::EventBus::default();
    let prompt = InteractiveSecretPrompt::new(bus.clone());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    bus.on::<SecretPromptNotification>(move |event| {
        let _ = sender.send(event.clone());
    });
    let task = {
        let prompt = prompt.clone();
        tokio::spawn(async move {
            prompt
                .prompt(
                    SecretPromptRequest {
                        error: None,
                        prompt: "Paste key".to_owned(),
                        query: query(),
                        operation: None,
                        masked: true,
                    },
                    CancellationToken::new(),
                )
                .await
        })
    };
    let id = match receiver.recv().await.unwrap() {
        SecretPromptNotification::Requested { id, .. } => id,
        event => panic!("unexpected event: {event:?}"),
    };
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(matches!(
        receiver.recv().await.unwrap(),
        SecretPromptNotification::Resolved {
            id: resolved,
            answered: false,
        } if resolved == id
    ));
    assert!(!prompt.answer(id, CredentialSecret::new("too-late")));
}

#[test]
fn configured_base_urls_replace_the_official_hosts_for_every_probe() {
    let deepseek =
        HttpApiKeyValidator::deepseek_at("http://127.0.0.1:9/", Some("m".into())).unwrap();
    assert_eq!(deepseek.endpoints(), ("http://127.0.0.1:9/models", None));
    let openrouter =
        HttpApiKeyValidator::openrouter_at("http://127.0.0.1:9/api/v1", Some("m".into())).unwrap();
    assert_eq!(
        openrouter.endpoints(),
        (
            "http://127.0.0.1:9/api/v1/key",
            Some("http://127.0.0.1:9/api/v1/models")
        )
    );
    let official = HttpApiKeyValidator::openrouter(None).unwrap();
    assert_eq!(
        official.endpoints(),
        ("https://openrouter.ai/api/v1/key", None)
    );
    assert_eq!(
        HttpApiKeyValidator::deepseek(None).unwrap().endpoints().0,
        "https://api.deepseek.com/models"
    );
}

struct CorrectKeyOnly;
#[async_trait]
impl ApiKeyValidator for CorrectKeyOnly {
    async fn validate(
        &self,
        secret: &CredentialSecret,
        _: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        if secret.expose() == "fixture-valid-key" {
            Ok(())
        } else {
            Err(ApiKeyValidationFailure::Unauthorized)
        }
    }
}

#[tokio::test]
async fn rejected_interactive_key_reprompts_before_any_commit_and_corrected_key_is_validated() {
    let (mut context, authorization, writes) = stack();
    let broker = Arc::new(InteractiveSecretPrompt::new(
        heycode_core::EventBus::default(),
    ));
    let mut events = broker.subscribe();
    let api_flow = flow(broker.clone(), Arc::new(CorrectKeyOnly));
    let id = api_flow.id().clone();
    authorization
        .register(&context, Arc::new(api_flow))
        .unwrap();
    let task = tokio::spawn({
        let authorization = authorization.clone();
        let id = id.clone();
        async move {
            authorization
                .authorize(&id, query(), CancellationToken::new())
                .await
        }
    });
    let first = match events.recv().await.unwrap() {
        SecretPromptNotification::Requested { id, error, .. } => {
            assert!(error.is_none());
            id
        }
        _ => panic!("expected input"),
    };
    assert!(broker.answer(first, CredentialSecret::new("fixture-invalid-key")));
    assert!(matches!(
        events.recv().await.unwrap(),
        SecretPromptNotification::Resolved { answered: true, .. }
    ));
    let second = match events.recv().await.unwrap() {
        SecretPromptNotification::Requested { id, error, .. } => {
            let error = error.unwrap();
            assert!(error.contains("invalid"));
            assert!(!error.contains("fixture-invalid-key"));
            id
        }
        _ => panic!("expected corrected input"),
    };
    assert_ne!(first, second);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert!(!authorization.credential_state(&query()).unwrap().configured);
    assert!(broker.answer(second, CredentialSecret::new("fixture-valid-key")));
    task.await.unwrap().unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    authorization
        .validate_existing(&id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "saved-key validation is read-only"
    );
    context.shutdown();
}

#[tokio::test]
async fn cancelled_invalid_key_retry_never_commits() {
    let (mut context, authorization, writes) = stack();
    let broker = Arc::new(InteractiveSecretPrompt::new(
        heycode_core::EventBus::default(),
    ));
    let mut events = broker.subscribe();
    let api_flow = flow(broker.clone(), Arc::new(CorrectKeyOnly));
    let id = api_flow.id().clone();
    authorization
        .register(&context, Arc::new(api_flow))
        .unwrap();
    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let token = cancellation.clone();
        async move { authorization.authorize(&id, query(), token).await }
    });
    let first = match events.recv().await.unwrap() {
        SecretPromptNotification::Requested { id, .. } => id,
        _ => panic!("expected input"),
    };
    broker.answer(first, CredentialSecret::new("fixture-invalid-key"));
    events.recv().await.unwrap();
    assert!(matches!(
        events.recv().await.unwrap(),
        SecretPromptNotification::Requested { error: Some(_), .. }
    ));
    cancellation.cancel();
    assert!(task.await.unwrap().is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    context.shutdown();
}

#[tokio::test]
async fn revoked_saved_key_fails_live_validation_without_replacing_it() {
    let (mut context, authorization, writes) = stack();
    let api_flow = flow(
        Arc::new(Prompt {
            masked: Arc::new(AtomicBool::new(false)),
            secret: "fixture-valid-key",
        }),
        Arc::new(CorrectKeyOnly),
    );
    let id = api_flow.id().clone();
    authorization
        .register(&context, Arc::new(api_flow))
        .unwrap();
    authorization
        .authorize(&id, query(), CancellationToken::new())
        .await
        .unwrap();
    // The same authoritative credential is now rejected by the provider.
    let rejected = flow(
        Arc::new(Prompt {
            masked: Arc::new(AtomicBool::new(false)),
            secret: "never-used",
        }),
        Arc::new(Validator(Err(ApiKeyValidationFailure::Unauthorized))),
    );
    let other = AuthorizationService::new(
        context
            .get::<CredentialsService>(SERVICE_CREDENTIALS)
            .unwrap(),
    );
    other.register(&context, Arc::new(rejected)).unwrap();
    let error = other
        .validate_existing(&id, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, AuthorizationError::Flow { code, .. } if code == "unauthorized"));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(other.credential_state(&query()).unwrap().configured);
    context.shutdown();
}

#[tokio::test]
async fn successful_http_status_requires_the_provider_response_shape() {
    for body in [
        "{}",
        "null",
        r#"{"error":{"code":401,"message":"fixture-secret"}}"#,
    ] {
        let url = one_response(200, body).await;
        let validator = HttpApiKeyValidator::openrouter_at(&url, None).unwrap();
        assert_eq!(
            validator
                .validate(
                    &CredentialSecret::new("fixture-secret"),
                    CancellationToken::new()
                )
                .await,
            Err(ApiKeyValidationFailure::Host)
        );
    }
    let url = one_response(200, r#"{"data":{"label":"fixture"}}"#).await;
    HttpApiKeyValidator::openrouter_at(&url, None)
        .unwrap()
        .validate(
            &CredentialSecret::new("fixture-secret"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    for body in [r#"{"data":[]}"#, r#"{"models":[]}"#, "[]"] {
        let url = one_response(200, body).await;
        HttpApiKeyValidator::new(format!("{url}/models"), None, None)
            .unwrap()
            .validate(
                &CredentialSecret::new("fixture-secret"),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
}

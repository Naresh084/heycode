//! POA01 provider-owned OpenAI profile, authorization flow and validator.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_authorization::{AuthorizationError, AuthorizationService, SERVICE_AUTHORIZATION};
use heycode_authorization_api_key::{
    ApiKeyValidationFailure, ApiKeyValidator, SecretPrompt, SecretPromptRequest,
};
use heycode_core::{Context, CoreError, Plugin, ProviderProtocol, compose};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialSecret, CredentialsService,
    SERVICE_CREDENTIALS,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream, TransportError,
};
use heycode_provider_openai::{
    OPENAI_API_KEY_REFERENCE, OPENAI_FLOW_ID, OPENAI_GPT_5_6_SOL, OpenAiApiKeyValidator,
    OpenAiPluginConfig, openai_plugin, openai_profile, openai_prompt_cache_settings_namespace,
};
use heycode_settings::{SERVICE_SETTINGS, SettingsDocuments, SettingsService, settings_plugin};
use tokio_util::sync::CancellationToken;

const TEST_SECRET: &str = "sk-proj-test-not-a-real-key";

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

struct StaticPrompt;

#[async_trait]
impl SecretPrompt for StaticPrompt {
    async fn prompt(
        &self,
        request: SecretPromptRequest,
        _cancellation: CancellationToken,
    ) -> Result<CredentialSecret, String> {
        assert!(request.masked);
        Ok(CredentialSecret::new("invalid-openai-test-secret"))
    }
}

struct RejectingValidator;

#[async_trait]
impl ApiKeyValidator for RejectingValidator {
    async fn validate(
        &self,
        _secret: &CredentialSecret,
        _cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        Err(ApiKeyValidationFailure::Unauthorized)
    }
}

type RecordedRequest = (String, Vec<(String, String)>);
type RecordedRequests = Arc<Mutex<Vec<RecordedRequest>>>;

enum Reply {
    Response(HttpResponse),
    Failure(TransportError),
}

struct ScriptedTransport {
    replies: Mutex<Vec<Reply>>,
    requests: RecordedRequests,
}

impl HttpTransport for ScriptedTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.requests.lock().unwrap().push((
            request.url().to_owned(),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        ));
        let mut replies = self.replies.lock().unwrap();
        assert!(!replies.is_empty(), "unexpected extra validation request");
        match replies.remove(0) {
            Reply::Response(response) => Box::pin(async move { Ok(response) }),
            Reply::Failure(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("CUSTOM_OPENAI_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn ok(body: serde_json::Value) -> Reply {
    Reply::Response(HttpResponse {
        headers: BTreeMap::new(),
        status: 200,
        content_type: Some("application/json".to_owned()),
        body: body.to_string().into_bytes(),
    })
}

fn status(status: u16) -> Reply {
    Reply::Response(HttpResponse {
        headers: BTreeMap::new(),
        status,
        content_type: Some("application/json".to_owned()),
        body: serde_json::json!({
            "error": {
                "message": "must-not-leak",
                "type": "invalid_request_error",
                "param": serde_json::Value::Null,
                "code": "invalid_api_key"
            }
        })
        .to_string()
        .into_bytes(),
    })
}

fn model_row() -> serde_json::Value {
    serde_json::json!({
        "id": OPENAI_GPT_5_6_SOL,
        "object": "model",
        "created": 1_752_019_200_u64,
        "owned_by": "system",
        "shutdown_date": serde_json::Value::Null
    })
}

fn list_body() -> serde_json::Value {
    serde_json::json!({ "object": "list", "data": [model_row()] })
}

fn scripted_validator(
    required_model: Option<&str>,
    replies: Vec<Reply>,
) -> (OpenAiApiKeyValidator, RecordedRequests) {
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(ScriptedTransport {
        replies: Mutex::new(replies),
        requests: requests.clone(),
    }));
    let validator = OpenAiApiKeyValidator::new(http, required_model.map(str::to_owned)).unwrap();
    (validator, requests)
}

#[test]
fn profile_owns_current_identity_default_and_credential_reference() {
    let profile = openai_profile();
    assert_eq!(profile.registry_name, "openai");
    assert_eq!(profile.descriptor.id, "openai");
    assert_eq!(profile.descriptor.display_name, "OpenAI");
    // Both protocol families are published by the official API surface, and
    // POA01 rides the Responses adapter P03 already implements.
    assert_eq!(
        profile.descriptor.protocols,
        vec![
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions
        ]
    );
    assert_eq!(profile.default_model, OPENAI_GPT_5_6_SOL);
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(OPENAI_API_KEY_REFERENCE)
    );
}

#[tokio::test]
async fn plugin_owns_flow_rejects_invalid_key_before_commit_and_disposes() {
    let config = OpenAiPluginConfig::new(
        query(),
        Arc::new(StaticPrompt),
        Arc::new(RejectingValidator),
    );
    let provider_plugin = openai_plugin(config);
    assert_eq!(
        provider_plugin.inject(),
        &[SERVICE_AUTHORIZATION, SERVICE_SETTINGS]
    );
    assert_eq!(
        provider_plugin.descriptor().contributions,
        &[
            heycode_core::PluginContributionKind::Provider,
            heycode_core::PluginContributionKind::Service,
        ]
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(CredentialsPlugin),
        settings_plugin(SettingsDocuments::new()),
        heycode_authorization::authorization_plugin(),
        provider_plugin,
    ];
    let mut context = compose(&plugins).unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    let descriptors = authorization.descriptors().unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].id.as_str(), OPENAI_FLOW_ID);
    assert_eq!(descriptors[0].query, query());
    let settings = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let cache_namespace = openai_prompt_cache_settings_namespace().unwrap();
    let cache_snapshot = settings.get(&cache_namespace).unwrap().unwrap();
    assert_eq!(cache_snapshot.resolved()["enabled"], false);

    let error = authorization
        .authorize(
            &descriptors[0].id,
            descriptors[0].query.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AuthorizationError::Flow { ref code, .. } if code == "unauthorized"
    ));

    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "provider-openai"
            && row.kind == heycode_core::ContributionKind::AuthorizationFlow
            && row.name == OPENAI_FLOW_ID
    }));
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "provider-openai"
            && row.kind == heycode_core::ContributionKind::SettingsNamespace
            && row.name == cache_namespace.as_str()
    }));

    context.shutdown();
    assert!(authorization.descriptors().unwrap().is_empty());
    assert!(settings.get(&cache_namespace).unwrap().is_none());
}

#[tokio::test]
async fn validation_sends_a_bearer_header_to_the_unparameterized_models_endpoint() {
    let (validator, requests) = scripted_validator(None, vec![ok(list_body())]);
    validator
        .validate(
            &CredentialSecret::new(TEST_SECRET),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    // The documented endpoint takes no query parameters, and the documented
    // auth form is a bearer header — not a provider-specific key header.
    assert_eq!(
        *requests.lock().unwrap(),
        [(
            "https://api.openai.com/v1/models".to_owned(),
            vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("authorization".to_owned(), format!("Bearer {TEST_SECRET}")),
            ]
        )]
    );
}

#[tokio::test]
async fn model_entitlement_is_a_separate_check_from_key_acceptance() {
    // A 200 on the list proves the key; only the exact model lookup proves the
    // configured id exists for it (GOTCHAS #38).
    let (validator, requests) = scripted_validator(
        Some(OPENAI_GPT_5_6_SOL),
        vec![ok(list_body()), ok(model_row())],
    );
    validator
        .validate(
            &CredentialSecret::new(TEST_SECRET),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let urls: Vec<String> = requests
        .lock()
        .unwrap()
        .iter()
        .map(|(url, _)| url.clone())
        .collect();
    assert_eq!(
        urls,
        [
            "https://api.openai.com/v1/models".to_owned(),
            format!("https://api.openai.com/v1/models/{OPENAI_GPT_5_6_SOL}"),
        ]
    );

    let (validator, _) = scripted_validator(
        Some("gpt-does-not-exist"),
        vec![ok(list_body()), status(404)],
    );
    assert_eq!(
        validator
            .validate(
                &CredentialSecret::new(TEST_SECRET),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Model
    );
}

#[tokio::test]
async fn an_unsafe_configured_model_id_never_reaches_a_url() {
    // A configured id is untrusted text until it is proven URL-safe; a query
    // or path injection must fail at construction, before any request.
    for rejected in ["", "gpt-5?x=1", "gpt 5", "../models", "gpt/5"] {
        let http = HttpService::new(Arc::new(ScriptedTransport {
            replies: Mutex::new(Vec::new()),
            requests: Arc::new(Mutex::new(Vec::new())),
        }));
        assert!(
            OpenAiApiKeyValidator::new(http, Some(rejected.to_owned())).is_err(),
            "`{rejected}` must be refused"
        );
    }
}

#[tokio::test]
async fn validation_failures_use_the_closed_safe_taxonomy() {
    for (code, expected) in [
        (401, ApiKeyValidationFailure::Unauthorized),
        (403, ApiKeyValidationFailure::Unauthorized),
        (404, ApiKeyValidationFailure::Host),
        (429, ApiKeyValidationFailure::Network),
        (500, ApiKeyValidationFailure::Network),
        (503, ApiKeyValidationFailure::Network),
        (418, ApiKeyValidationFailure::Host),
    ] {
        let (validator, _) = scripted_validator(None, vec![status(code)]);
        assert_eq!(
            validator
                .validate(
                    &CredentialSecret::new(TEST_SECRET),
                    CancellationToken::new()
                )
                .await
                .unwrap_err(),
            expected,
            "HTTP {code}"
        );
    }

    let (validator, _) = scripted_validator(
        None,
        vec![Reply::Failure(TransportError::Network {
            message: "must-not-leak".to_owned(),
        })],
    );
    assert_eq!(
        validator
            .validate(
                &CredentialSecret::new(TEST_SECRET),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Network
    );

    // A non-JSON success body is a wrong endpoint, not an accepted key.
    let (validator, _) = scripted_validator(
        None,
        vec![Reply::Response(HttpResponse {
            headers: BTreeMap::new(),
            status: 200,
            content_type: Some("text/html".to_owned()),
            body: b"<html>must-not-leak</html>".to_vec(),
        })],
    );
    assert_eq!(
        validator
            .validate(
                &CredentialSecret::new(TEST_SECRET),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Host
    );
}

#[tokio::test]
async fn cancellation_stops_validation_before_any_request() {
    let (validator, requests) = scripted_validator(None, vec![ok(list_body())]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        validator
            .validate(&CredentialSecret::new(TEST_SECRET), cancellation)
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Cancelled
    );
    assert!(requests.lock().unwrap().is_empty());
}

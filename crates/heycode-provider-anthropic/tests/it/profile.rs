//! PAN01 provider-owned Anthropic profile, authorization flow and validator.

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
use heycode_provider_anthropic::{
    ANTHROPIC_API_KEY_REFERENCE, ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_FLOW_ID, ANTHROPIC_VERSION,
    AnthropicApiKeyValidator, AnthropicPluginConfig, anthropic_plugin, anthropic_profile,
};
use tokio_util::sync::CancellationToken;

const TEST_SECRET: &str = "sk-ant-test-not-a-real-key";

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
        Ok(CredentialSecret::new("invalid-anthropic-test-secret"))
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
        CredentialReference::new("CUSTOM_ANTHROPIC_KEY").unwrap(),
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
            "type": "error",
            "error": {"type": "authentication_error", "message": "must-not-leak"}
        })
        .to_string()
        .into_bytes(),
    })
}

fn model_row() -> serde_json::Value {
    serde_json::json!({
        "id": ANTHROPIC_CLAUDE_OPUS_5,
        "type": "model",
        "display_name": "Claude Opus 5",
        "created_at": "2026-07-24T00:00:00Z",
        "max_input_tokens": 1_000_000_u64,
        "max_tokens": 128_000_u64,
        "capabilities": null
    })
}

fn list_body() -> serde_json::Value {
    serde_json::json!({
        "data": [model_row()],
        "first_id": ANTHROPIC_CLAUDE_OPUS_5,
        "last_id": ANTHROPIC_CLAUDE_OPUS_5,
        "has_more": false
    })
}

fn scripted_validator(
    required_model: Option<&str>,
    replies: Vec<Reply>,
) -> (AnthropicApiKeyValidator, RecordedRequests) {
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(ScriptedTransport {
        replies: Mutex::new(replies),
        requests: requests.clone(),
    }));
    let validator = AnthropicApiKeyValidator::new(http, required_model.map(str::to_owned)).unwrap();
    (validator, requests)
}

#[test]
fn profile_owns_current_identity_default_and_credential_reference() {
    let profile = anthropic_profile();
    assert_eq!(profile.registry_name, "anthropic");
    assert_eq!(profile.descriptor.id, "anthropic");
    assert_eq!(profile.descriptor.display_name, "Anthropic");
    assert_eq!(
        profile.descriptor.protocols,
        vec![ProviderProtocol::AnthropicMessages]
    );
    assert_eq!(profile.default_model, ANTHROPIC_CLAUDE_OPUS_5);
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(ANTHROPIC_API_KEY_REFERENCE)
    );
}

#[tokio::test]
async fn plugin_owns_flow_rejects_invalid_key_before_commit_and_disposes() {
    let config = AnthropicPluginConfig::new(
        query(),
        Arc::new(StaticPrompt),
        Arc::new(RejectingValidator),
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        Box::new(CredentialsPlugin),
        heycode_authorization::authorization_plugin(),
        anthropic_plugin(config),
    ];
    let mut context = compose(&plugins).unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    let descriptors = authorization.descriptors().unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].id.as_str(), ANTHROPIC_FLOW_ID);
    assert_eq!(descriptors[0].query, query());

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
        row.plugin == "provider-anthropic"
            && row.kind == heycode_core::ContributionKind::AuthorizationFlow
            && row.name == ANTHROPIC_FLOW_ID
    }));

    context.shutdown();
    assert!(authorization.descriptors().unwrap().is_empty());
}

#[tokio::test]
async fn validation_sends_the_native_key_and_version_headers_never_a_bearer() {
    let (validator, requests) = scripted_validator(None, vec![ok(list_body())]);
    validator
        .validate(
            &CredentialSecret::new(TEST_SECRET),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        *requests.lock().unwrap(),
        [(
            "https://api.anthropic.com/v1/models?limit=1".to_owned(),
            vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("x-api-key".to_owned(), TEST_SECRET.to_owned()),
                ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
            ]
        )]
    );
}

#[tokio::test]
async fn model_entitlement_is_a_separate_check_from_key_acceptance() {
    // A 200 on the list proves the key; only the exact model lookup proves the
    // configured id exists for it (GOTCHAS #38).
    let (validator, requests) = scripted_validator(
        Some(ANTHROPIC_CLAUDE_OPUS_5),
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
            "https://api.anthropic.com/v1/models?limit=1".to_owned(),
            format!("https://api.anthropic.com/v1/models/{ANTHROPIC_CLAUDE_OPUS_5}"),
        ]
    );

    let (validator, _) = scripted_validator(
        Some("claude-does-not-exist"),
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
async fn validation_failures_use_the_closed_safe_taxonomy() {
    for (code, expected) in [
        (401, ApiKeyValidationFailure::Unauthorized),
        (403, ApiKeyValidationFailure::Unauthorized),
        (404, ApiKeyValidationFailure::Host),
        (429, ApiKeyValidationFailure::Network),
        (500, ApiKeyValidationFailure::Network),
        (529, ApiKeyValidationFailure::Network),
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

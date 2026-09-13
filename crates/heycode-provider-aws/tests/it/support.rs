//! Injected transport, credential and host fakes shared by the PAWS03 cases.
//!
//! Nothing here reaches the network: every case drives one scripted
//! [`HttpTransport`] whose recorded requests are the only proof of what would
//! have gone to AWS.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_authorization_aws::{AWS_BEDROCK_API_KEY_REFERENCE, AwsAuthService, MapAwsHost};
use heycode_core::{Context, CoreError, Plugin, ServiceKey};
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
    SERVICE_CREDENTIALS,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SERVICE_HTTP, SseEventStream, TransportError,
};
use heycode_provider_aws::{
    BedrockApiKeyAuthorizer, BedrockCatalog, BedrockFoundationModel, BedrockRequestAuthorizer,
    MantleCatalog,
};
use tokio_util::sync::CancellationToken;

/// Deliberately shaped like a real Bedrock API key without being one.
pub(crate) const TEST_SECRET: &str = "ABSKtest-not-a-real-bedrock-api-key";
pub(crate) const ROTATED_TEST_SECRET: &str = "ABSKrotated-not-a-real-bedrock-api-key";
pub(crate) const TEST_REGION: &str = "us-east-1";
pub(crate) const EXPECTED_URL: &str = "https://bedrock.us-east-1.amazonaws.com/foundation-models";

/// One recorded request: URL, method and every header name/value in order.
pub(crate) type RecordedRequest = (String, String, Vec<(String, String)>);
pub(crate) type RecordedRequests = Arc<Mutex<Vec<RecordedRequest>>>;

enum Scripted {
    Response(HttpResponse),
    Failure(TransportError),
}

struct RecordingTransport {
    scripted: Mutex<Vec<Scripted>>,
    requests: RecordedRequests,
}

impl HttpTransport for RecordingTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.requests.lock().unwrap().push((
            request.url().to_owned(),
            format!("{:?}", request.method()),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        ));
        let mut scripted = self.scripted.lock().unwrap();
        assert!(!scripted.is_empty(), "unexpected extra discovery request");
        match scripted.remove(0) {
            Scripted::Response(response) => Box::pin(async move { Ok(response) }),
            Scripted::Failure(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

pub(crate) fn http(responses: Vec<HttpResponse>) -> (HttpService, RecordedRequests) {
    scripted_http(responses.into_iter().map(Scripted::Response).collect())
}

pub(crate) fn failing_http(error: TransportError) -> (HttpService, RecordedRequests) {
    scripted_http(vec![Scripted::Failure(error)])
}

fn scripted_http(scripted: Vec<Scripted>) -> (HttpService, RecordedRequests) {
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    let service = HttpService::new(Arc::new(RecordingTransport {
        scripted: Mutex::new(scripted),
        requests: requests.clone(),
    }));
    (service, requests)
}

/// A JSON response with an explicit status.
pub(crate) fn response(status: u16, body: &serde_json::Value) -> HttpResponse {
    raw_response(
        status,
        Some("application/json"),
        body.to_string().into_bytes(),
    )
}

pub(crate) fn raw_response(status: u16, content_type: Option<&str>, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        headers: BTreeMap::new(),
        status,
        content_type: content_type.map(str::to_owned),
        body,
    }
}

/// Credential behavior a case wants to observe.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Secret {
    /// The configured key resolves.
    Present,
    /// Nothing is configured.
    Absent,
    /// The store is configured but cannot answer.
    Broken,
}

struct SecretProvider {
    id: CredentialProviderId,
    secret: Secret,
}

/// Mutable secret source used to prove operation-time credential resolution.
#[derive(Clone)]
pub(crate) struct RotatingSecret {
    value: Arc<Mutex<Option<String>>>,
}

impl RotatingSecret {
    pub(crate) fn set(&self, value: Option<&str>) {
        *self.value.lock().unwrap() = value.map(str::to_owned);
    }
}

struct RotatingSecretProvider {
    id: CredentialProviderId,
    value: Arc<Mutex<Option<String>>>,
}

struct ProcessSecretProvider {
    id: CredentialProviderId,
}

impl CredentialProvider for ProcessSecretProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(
            if std::env::var_os(query.reference.as_str())
                .filter(|value| !value.is_empty())
                .is_some()
            {
                CredentialProviderState::configured(CredentialSource::Environment, false)
            } else {
                CredentialProviderState::unconfigured(false)
            },
        )
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        std::env::var(query.reference.as_str())
            .map(CredentialSecret::new)
            .map(Some)
            .or_else(|error| match error {
                std::env::VarError::NotPresent => Ok(None),
                std::env::VarError::NotUnicode(_) => {
                    Err("environment value is not valid Unicode".to_owned())
                }
            })
    }
}

impl CredentialProvider for RotatingSecretProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(if self.value.lock().unwrap().is_some() {
            CredentialProviderState::configured(CredentialSource::Environment, false)
        } else {
            CredentialProviderState::unconfigured(false)
        })
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(self
            .value
            .lock()
            .unwrap()
            .as_ref()
            .map(|value| CredentialSecret::new(value.clone())))
    }
}

impl CredentialProvider for SecretProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(match self.secret {
            Secret::Absent => CredentialProviderState::unconfigured(false),
            Secret::Present | Secret::Broken => {
                CredentialProviderState::configured(CredentialSource::Environment, false)
            }
        })
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        match self.secret {
            Secret::Present => Ok(Some(CredentialSecret::new(TEST_SECRET))),
            Secret::Absent => Ok(None),
            Secret::Broken => Err("credential store is unavailable".to_owned()),
        }
    }
}

pub(crate) fn query() -> CredentialQuery {
    query_for(AWS_BEDROCK_API_KEY_REFERENCE)
}

pub(crate) fn query_for(reference: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(reference).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

pub(crate) fn credentials(secret: Secret) -> (Context, Arc<CredentialsService>) {
    let context = Context::new();
    let service = Arc::new(CredentialsService::new());
    service
        .register(
            &context,
            Arc::new(SecretProvider {
                id: CredentialProviderId::new("test-secret").unwrap(),
                secret,
            }),
        )
        .unwrap();
    (context, service)
}

pub(crate) fn rotating_credentials(
    initial: Option<&str>,
) -> (Context, Arc<CredentialsService>, RotatingSecret) {
    let context = Context::new();
    let service = Arc::new(CredentialsService::new());
    let secret = RotatingSecret {
        value: Arc::new(Mutex::new(initial.map(str::to_owned))),
    };
    service
        .register(
            &context,
            Arc::new(RotatingSecretProvider {
                id: CredentialProviderId::new("rotating-test-secret").unwrap(),
                value: secret.value.clone(),
            }),
        )
        .unwrap();
    (context, service, secret)
}

pub(crate) fn process_credentials() -> (Context, Arc<CredentialsService>) {
    let context = Context::new();
    let service = Arc::new(CredentialsService::new());
    service
        .register(
            &context,
            Arc::new(ProcessSecretProvider {
                id: CredentialProviderId::new("process-test-secret").unwrap(),
            }),
        )
        .unwrap();
    (context, service)
}

/// A catalog wired to the scripted transport and the API-key authorizer.
pub(crate) fn catalog(
    secret: Secret,
    responses: Vec<HttpResponse>,
) -> (Context, BedrockCatalog, RecordedRequests) {
    let (http, requests) = http(responses);
    let (context, catalog) = catalog_over(secret, http);
    (context, catalog, requests)
}

/// A catalog over an already-built transport. The returned [`Context`] owns
/// the credential-provider effect and must outlive the catalog.
pub(crate) fn catalog_over(secret: Secret, http: HttpService) -> (Context, BedrockCatalog) {
    let (context, credentials) = credentials(secret);
    let authorizer = Arc::new(BedrockApiKeyAuthorizer::new(credentials, query()));
    (context, build_catalog(http, authorizer))
}

pub(crate) fn build_catalog(
    http: HttpService,
    authorizer: Arc<dyn BedrockRequestAuthorizer>,
) -> BedrockCatalog {
    BedrockCatalog::new(
        http,
        authorizer,
        &heycode_authorization_aws::AwsRegion::new(TEST_REGION).unwrap(),
    )
}

/// One `FoundationModelSummary` with every documented member published.
pub(crate) fn summary(model_id: &str) -> serde_json::Value {
    serde_json::json!({
        "modelArn": format!("arn:aws:bedrock:{TEST_REGION}::foundation-model/{model_id}"),
        "modelId": model_id,
        "modelName": "Claude Sonnet 4",
        "providerName": "Anthropic",
        "inputModalities": ["TEXT", "IMAGE"],
        "outputModalities": ["TEXT"],
        "responseStreamingSupported": true,
        "customizationsSupported": ["FINE_TUNING"],
        "inferenceTypesSupported": ["ON_DEMAND"],
        "modelLifecycle": { "status": "ACTIVE" }
    })
}

pub(crate) fn body(summaries: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "modelSummaries": summaries })
}

/// Publish the scripted transport under the shared HTTP key.
struct HttpPlugin(HttpService);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), self.0.clone())
    }
}

pub(crate) fn http_plugin(http: HttpService) -> Box<dyn Plugin> {
    Box::new(HttpPlugin(http))
}

/// Publish a credentials service holding the requested secret behavior.
pub(crate) struct CredentialsPlugin(pub(crate) Secret);

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())?;
        let service = context
            .get::<CredentialsService>(SERVICE_CREDENTIALS)
            .ok_or_else(|| CoreError::other("credentials service type mismatch"))?;
        service
            .register(
                context,
                Arc::new(SecretProvider {
                    id: CredentialProviderId::new("test-secret")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    secret: self.0,
                }),
            )
            .map_err(|error| CoreError::other(error.to_string()))
    }
}

/// Publish the PAWS01 status service over a deterministic host view.
pub(crate) struct AwsAuthPlugin(pub(crate) MapAwsHost);

impl Plugin for AwsAuthPlugin {
    fn name(&self) -> &'static str {
        "test-aws-auth"
    }

    fn inject(&self) -> &'static [ServiceKey] {
        &[SERVICE_CREDENTIALS, SERVICE_HTTP]
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[heycode_authorization_aws::SERVICE_AWS_AUTH]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        let credentials = context
            .get::<CredentialsService>(SERVICE_CREDENTIALS)
            .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
        let http = context
            .get::<HttpService>(SERVICE_HTTP)
            .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
        let service = AwsAuthService::new(
            http.as_ref().clone(),
            credentials,
            Arc::new(self.0.clone()),
            query(),
        );
        context.provide(
            heycode_authorization_aws::SERVICE_AWS_AUTH,
            self.name(),
            service,
        )
    }
}

// ---------------------------------------------------------------------------
// PAWS02 Amazon Bedrock Mantle helpers
// ---------------------------------------------------------------------------

/// The documented regional Mantle model-listing URL for [`TEST_REGION`].
pub(crate) const MANTLE_URL: &str = "https://bedrock-mantle.us-east-1.api.aws/v1/models";

/// One OpenAI-shaped model entry.
///
/// Every field AWS documents as unreliable on this endpoint is deliberately
/// populated with a recognizable value, so a case can prove none of it reaches
/// a descriptor.
pub(crate) fn mantle_entry(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "object": "model",
        "created": 1_700_000_000_u64,
        "owned_by": "unreliable-owner-must-not-surface",
        "display_name": "Unreliable Display Name",
        "context_length": 999_999_u64
    })
}

pub(crate) fn mantle_body(entries: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "object": "list", "data": entries })
}

/// A Mantle catalog wired to the scripted transport and the API-key authorizer.
pub(crate) fn mantle_catalog(
    secret: Secret,
    responses: Vec<HttpResponse>,
) -> (Context, MantleCatalog, RecordedRequests) {
    let (http, requests) = http(responses);
    let (context, catalog) = mantle_catalog_over(secret, http);
    (context, catalog, requests)
}

/// A Mantle catalog over an already-built transport. The returned [`Context`]
/// owns the credential-provider effect and must outlive the catalog.
pub(crate) fn mantle_catalog_over(secret: Secret, http: HttpService) -> (Context, MantleCatalog) {
    let (context, credentials) = credentials(secret);
    let authorizer = Arc::new(BedrockApiKeyAuthorizer::new(credentials, query()));
    (
        context,
        MantleCatalog::new(
            http,
            authorizer,
            &heycode_authorization_aws::AwsRegion::new(TEST_REGION).unwrap(),
        ),
    )
}

/// Normalize one `FoundationModelSummary` through the real discovery path.
///
/// Going through `BedrockCatalog::discover` rather than constructing a
/// `BedrockFoundationModel` directly keeps the fixture honest: the row a case
/// reasons about is one the production normalizer actually produced.
pub(crate) async fn normalize_one(row: serde_json::Value) -> BedrockFoundationModel {
    let (context, source, _) = catalog(Secret::Present, vec![response(200, &body(vec![row]))]);
    let rows = source
        .discover(CancellationToken::new())
        .await
        .expect("the fixture row must normalize");
    drop(context);
    rows.into_iter().next().expect("exactly one row")
}

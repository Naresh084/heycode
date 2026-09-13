//! POA01 authenticated OpenAI model-list discovery and normalization.
//!
//! Fixtures mirror the official `GET /v1/models` shape published in
//! `openai/openai-openapi` (`ListModelsResponse` + `Model`) and on
//! <https://developers.openai.com/api/reference/resources/models/methods/list>:
//! an `object: "list"` envelope wrapping a `data` array of rows carrying
//! `id`, `object`, `created`, `owned_by` and a nullable `shutdown_date`.
//! The endpoint accepts no query parameters and publishes no cursor, so one
//! refresh is exactly one request.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
    SERVICE_CREDENTIALS,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SERVICE_HTTP, SseEventStream,
};
use heycode_llm::{
    CapabilitySupport, CatalogError, CatalogFailureKind, CatalogRefreshMode, CatalogRegistry,
    ModelCapabilities, ModelCatalog, ModelDescriptor, ModelLifecycleStatus, SERVICE_MODELS,
};
use heycode_provider_openai::{
    OPENAI_API_KEY_REFERENCE, OPENAI_GPT_5_6_SOL, OpenAiCatalog, OpenAiCatalogConfig,
    openai_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

const TEST_SECRET: &str = "sk-proj-test-not-a-real-key";
/// The documented endpoint takes no query parameters and no cursor.
const MODELS_URL: &str = "https://api.openai.com/v1/models";

/// One recorded request: URL plus every header name/value in send order.
pub(crate) type RecordedRequest = (String, Vec<(String, String)>);
pub(crate) type RecordedRequests = Arc<Mutex<Vec<RecordedRequest>>>;

struct RecordingTransport {
    responses: Mutex<Vec<HttpResponse>>,
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
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        ));
        let mut responses = self.responses.lock().unwrap();
        assert!(!responses.is_empty(), "unexpected extra catalog request");
        let response = responses.remove(0);
        Box::pin(async move { Ok(response) })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct SecretProvider {
    id: CredentialProviderId,
    secret: Option<String>,
}

impl CredentialProvider for SecretProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(if self.secret.is_some() {
            CredentialProviderState::configured(CredentialSource::Environment, false)
        } else {
            CredentialProviderState::unconfigured(false)
        })
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(self.secret.as_ref().map(CredentialSecret::new))
    }
}

pub(crate) fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(OPENAI_API_KEY_REFERENCE).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

pub(crate) fn credentials(secret: Option<&str>) -> (Context, Arc<CredentialsService>) {
    let context = Context::new();
    let credentials = Arc::new(CredentialsService::new());
    credentials
        .register(
            &context,
            Arc::new(SecretProvider {
                id: CredentialProviderId::new("test-secret").unwrap(),
                secret: secret.map(str::to_owned),
            }),
        )
        .unwrap();
    (context, credentials)
}

pub(crate) fn http(responses: Vec<HttpResponse>) -> (HttpService, RecordedRequests) {
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    let service = HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(responses),
        requests: requests.clone(),
    }));
    (service, requests)
}

fn make_catalog(
    secret: Option<&str>,
    responses: Vec<HttpResponse>,
) -> (Context, OpenAiCatalog, RecordedRequests) {
    let (context, service) = credentials(secret);
    let (http, requests) = http(responses);
    let catalog = OpenAiCatalog::new(http, service, query()).unwrap();
    (context, catalog, requests)
}

pub(crate) fn response(status: u16, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        headers: BTreeMap::new(),
        status,
        content_type: Some("application/json".to_owned()),
        body: body.to_string().into_bytes(),
    }
}

/// A flagship row with no announced shutdown, verbatim in documented shape.
fn sol_row() -> serde_json::Value {
    serde_json::json!({
        "id": OPENAI_GPT_5_6_SOL,
        "object": "model",
        "created": 1_752_019_200_u64,
        "owned_by": "system",
        "shutdown_date": serde_json::Value::Null
    })
}

/// `shutdown_date` is the only availability evidence this endpoint publishes.
/// The documented example value is used verbatim.
fn retiring_row() -> serde_json::Value {
    serde_json::json!({
        "id": "gpt-4o",
        "object": "model",
        "created": 1_715_367_049_u64,
        "owned_by": "system",
        "shutdown_date": "2026-10-23"
    })
}

/// `shutdown_date` is documented as optional, not merely nullable: an absent
/// key is the same "not announced" state as an explicit null.
fn org_owned_row() -> serde_json::Value {
    serde_json::json!({
        "id": "ft:gpt-4.1-2025-04-14:acme::9abcdefg",
        "object": "model",
        "created": 1_745_000_000_u64,
        "owned_by": "organization-owner"
    })
}

fn list(rows: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "object": "list", "data": rows })
}

fn fixture() -> Vec<HttpResponse> {
    vec![response(
        200,
        list(vec![sol_row(), retiring_row(), org_owned_row()]),
    )]
}

fn find<'a>(models: &'a [ModelDescriptor], id: &str) -> &'a ModelDescriptor {
    models
        .iter()
        .find(|model| model.id == id)
        .unwrap_or_else(|| panic!("catalog is missing `{id}`"))
}

#[tokio::test]
async fn one_unpaginated_bearer_request_publishes_every_entitled_model() {
    let (_context, catalog, requests) = make_catalog(Some(TEST_SECRET), fixture());
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 3);

    // The list endpoint documents no query parameter and no cursor, so a
    // refresh is exactly one request to the bare path. Inventing a `limit`
    // or an `after` would be sending a parameter the API never published.
    assert_eq!(
        *requests.lock().unwrap(),
        [(
            MODELS_URL.to_owned(),
            vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("authorization".to_owned(), format!("Bearer {TEST_SECRET}")),
            ]
        )]
    );

    let sol = find(&models, OPENAI_GPT_5_6_SOL);
    // The endpoint publishes no display name, so identity is the id itself.
    assert_eq!(sol.display_name, OPENAI_GPT_5_6_SOL);
    assert_eq!(sol.created_at_ms, Some(1_752_019_200_000));
    // It publishes no alias, window, output cap, price or performance field.
    assert!(sol.aliases.is_empty());
    assert_eq!(sol.context_window, None);
    assert_eq!(sol.max_output_tokens, None);
    assert!(sol.pricing.is_unknown());
    assert!(sol.performance.is_unknown());
    // The list endpoint itself has no capability fields. Three exact maintained
    // model facts are joined from current first-party documentation because
    // their production Consumers require affirmative model evidence.
    assert_eq!(
        sol.capabilities.native_compaction,
        CapabilitySupport::Supported
    );
    assert_eq!(sol.capabilities.prompt_cache, CapabilitySupport::Supported);
    assert_eq!(sol.capabilities.native_web, CapabilitySupport::Supported);
    assert_eq!(
        sol.capabilities,
        ModelCapabilities {
            native_compaction: CapabilitySupport::Supported,
            prompt_cache: CapabilitySupport::Supported,
            native_web: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        }
    );
    // A null `shutdown_date` is "not announced", never a retirement claim.
    assert_eq!(sol.lifecycle.status, ModelLifecycleStatus::Unknown);
    assert_eq!(sol.lifecycle.retirement_at_ms, None);
    assert!(sol.lifecycle.replacement_ids.is_empty());

    // An org-owned fine-tune with the key omitted entirely is the same state.
    let tuned = find(&models, "ft:gpt-4.1-2025-04-14:acme::9abcdefg");
    assert_eq!(tuned.lifecycle.status, ModelLifecycleStatus::Unknown);
    assert_eq!(tuned.lifecycle.retirement_at_ms, None);
    assert_eq!(tuned.capabilities, ModelCapabilities::unknown());
}

#[tokio::test]
async fn an_announced_shutdown_becomes_deprecated_at_an_exact_retirement_instant() {
    let (_context, catalog, _) = make_catalog(Some(TEST_SECRET), fixture());
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    let retiring = find(&models, "gpt-4o");

    // An announced shutdown is still selectable but must be planned away, and
    // the deadline is the start of the announced day in UTC — the earliest
    // instant that date can begin, so resolution never dispatches past it
    // (CAT03 resolves the phase at an explicit instant).
    assert_eq!(retiring.lifecycle.status, ModelLifecycleStatus::Deprecated);
    assert_eq!(retiring.lifecycle.retirement_at_ms, Some(1_792_713_600_000));
    // The endpoint publishes no replacement recommendation.
    assert!(retiring.lifecycle.replacement_ids.is_empty());
}

#[tokio::test]
async fn an_account_without_the_provider_default_still_publishes_its_entitled_models() {
    // The list is account-scoped: it is exactly what this key may use. A key
    // with no access to the provider default is a real account, not a
    // malformed response, so its generation must publish rather than fail.
    let (_context, catalog, _) = make_catalog(
        Some(TEST_SECRET),
        vec![response(200, list(vec![retiring_row(), org_owned_row()]))],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|model| model.id != OPENAI_GPT_5_6_SOL));
}

#[tokio::test]
async fn a_malformed_row_or_envelope_rejects_the_whole_generation() {
    let cases: Vec<(&str, Vec<HttpResponse>)> = vec![
        (
            "envelope object is not a list",
            vec![response(
                200,
                serde_json::json!({ "object": "model", "data": [sol_row()] }),
            )],
        ),
        (
            "row object is not a model",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "not-a-model", "object": "deployment",
                        "created": 1_700_000_000_u64, "owned_by": "system"
                    }),
                ]),
            )],
        ),
        (
            "blank id",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "", "object": "model",
                        "created": 1_700_000_000_u64, "owned_by": "system"
                    }),
                ]),
            )],
        ),
        (
            "id outside the unreserved URL charset",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "gpt-5?limit=1", "object": "model",
                        "created": 1_700_000_000_u64, "owned_by": "system"
                    }),
                ]),
            )],
        ),
        (
            "missing created",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "gpt-no-created", "object": "model", "owned_by": "system"
                    }),
                ]),
            )],
        ),
        (
            "missing owned_by",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "gpt-no-owner", "object": "model",
                        "created": 1_700_000_000_u64
                    }),
                ]),
            )],
        ),
        (
            "unparseable shutdown date",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "gpt-soon", "object": "model",
                        "created": 1_700_000_000_u64, "owned_by": "system",
                        "shutdown_date": "soon"
                    }),
                ]),
            )],
        ),
        (
            "impossible shutdown date",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "gpt-impossible", "object": "model",
                        "created": 1_700_000_000_u64, "owned_by": "system",
                        "shutdown_date": "2026-13-45"
                    }),
                ]),
            )],
        ),
        (
            "shutdown date carrying a time is not the documented date format",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "gpt-datetime", "object": "model",
                        "created": 1_700_000_000_u64, "owned_by": "system",
                        "shutdown_date": "2026-10-23T00:00:00Z"
                    }),
                ]),
            )],
        ),
        (
            "shutdown date before the epoch",
            vec![response(
                200,
                list(vec![
                    sol_row(),
                    serde_json::json!({
                        "id": "gpt-ancient", "object": "model",
                        "created": 1_700_000_000_u64, "owned_by": "system",
                        "shutdown_date": "1969-07-20"
                    }),
                ]),
            )],
        ),
        (
            "duplicate id",
            vec![response(200, list(vec![sol_row(), sol_row()]))],
        ),
        ("empty generation", vec![response(200, list(Vec::new()))]),
        (
            "response is not JSON",
            vec![HttpResponse {
                headers: BTreeMap::new(),
                status: 200,
                content_type: Some("text/html".to_owned()),
                body: b"<html>must-not-leak</html>".to_vec(),
            }],
        ),
        (
            // A JSON-shaped body under a non-JSON content type is an
            // interceptor or a wrong endpoint, not the model list. Parsing it
            // anyway would publish whatever that host chose to return.
            "documented shape under a non-JSON content type",
            vec![HttpResponse {
                headers: BTreeMap::new(),
                status: 200,
                content_type: Some("text/html".to_owned()),
                body: list(vec![sol_row()]).to_string().into_bytes(),
            }],
        ),
    ];

    for (label, responses) in cases {
        let (_context, catalog, _) = make_catalog(Some(TEST_SECRET), responses);
        let error = catalog
            .fetch(CancellationToken::new())
            .await
            .expect_err(label);
        assert_eq!(
            error.kind(),
            CatalogFailureKind::InvalidResponse,
            "{label} must reject the generation"
        );
        assert!(!error.message().contains("must-not-leak"), "{label}");
    }
}

#[tokio::test]
async fn status_and_transport_failures_classify_without_exposing_bodies() {
    for (status, kind) in [
        (401, CatalogFailureKind::Unauthorized),
        (403, CatalogFailureKind::Unauthorized),
        (429, CatalogFailureKind::Unavailable),
        (500, CatalogFailureKind::Unavailable),
        (503, CatalogFailureKind::Unavailable),
        (404, CatalogFailureKind::InvalidResponse),
    ] {
        let (_context, catalog, _) = make_catalog(
            Some(TEST_SECRET),
            vec![response(
                status,
                serde_json::json!({
                    "error": {
                        "message": "must-not-leak",
                        "type": "invalid_request_error",
                        "param": serde_json::Value::Null,
                        "code": "invalid_api_key"
                    }
                }),
            )],
        );
        let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind(), kind, "HTTP {status}");
        assert!(!error.message().contains("must-not-leak"), "HTTP {status}");
        assert!(
            !format!("{error:?}").contains("must-not-leak"),
            "HTTP {status}"
        );
    }
}

#[tokio::test]
async fn an_oversized_generation_is_refused_before_it_is_normalized() {
    // The endpoint is unpaginated, so the only bound on a generation is the
    // row cap. A response larger than it is a hostile or broken host, not a
    // catalog, and must fail rather than be normalized row by row.
    let rows: Vec<serde_json::Value> = (0..4097)
        .map(|index| {
            serde_json::json!({
                "id": format!("gpt-bulk-{index}"),
                "object": "model",
                "created": 1_700_000_000_u64,
                "owned_by": "system"
            })
        })
        .collect();
    let (_context, catalog, _) = make_catalog(Some(TEST_SECRET), vec![response(200, list(rows))]);
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn a_custom_base_url_reaches_the_wire_and_an_invalid_one_never_publishes() {
    // A compatible proxy must receive the same documented path, and a base URL
    // that cannot form an HTTP request must fail before the source is
    // registered rather than at the first refresh.
    let (context, credentials) = credentials(Some(TEST_SECRET));
    let (service, requests) = http(vec![response(200, list(vec![sol_row()]))]);
    let catalog = OpenAiCatalog::with_base_url(
        service,
        credentials.clone(),
        query(),
        "https://gateway.example.internal/openai/",
    )
    .unwrap();
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(
        requests.lock().unwrap()[0].0,
        "https://gateway.example.internal/openai/v1/models"
    );

    let (bad_http, _) = http(Vec::new());
    assert!(
        OpenAiCatalog::with_base_url(bad_http, credentials, query(), "not-a-url").is_err(),
        "an unusable base URL must fail before publication"
    );
    drop(context);
}

#[tokio::test]
async fn a_missing_credential_is_unauthorized_before_any_request() {
    let (_context, catalog, requests) = make_catalog(None, fixture());
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unauthorized);
    assert!(!error.message().contains(TEST_SECRET));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_never_masquerades_as_a_generation() {
    let (_context, catalog, requests) = make_catalog(Some(TEST_SECRET), fixture());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = catalog.fetch(cancellation).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Cancelled);
    assert!(requests.lock().unwrap().is_empty());
}

struct HttpPlugin(HttpService);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), self.0.clone())
    }
}

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        let service = CredentialsService::new();
        context.provide(SERVICE_CREDENTIALS, self.name(), service)?;
        let service = context
            .get::<CredentialsService>(SERVICE_CREDENTIALS)
            .ok_or_else(|| CoreError::other("credentials service type mismatch"))?;
        service
            .register(
                context,
                Arc::new(SecretProvider {
                    id: CredentialProviderId::new("test-secret")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    secret: Some(TEST_SECRET.to_owned()),
                }),
            )
            .map_err(|error| CoreError::other(error.to_string()))
    }
}

#[tokio::test]
async fn catalog_plugin_registers_and_disposes_the_openai_source() {
    let (http, _requests) = http(fixture());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(http)),
        Box::new(CredentialsPlugin),
        openai_catalog_plugin(OpenAiCatalogConfig::official(query())),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            "openai",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        view.snapshot
            .models
            .iter()
            .any(|model| model.id == OPENAI_GPT_5_6_SOL)
    );
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "catalog-openai"
                && row.kind == heycode_core::ContributionKind::ModelCatalog
                && row.name == "openai")
    );

    context.shutdown();
    assert!(matches!(
        models
            .refresh(
                "openai",
                CatalogRefreshMode::Force,
                CancellationToken::new()
            )
            .await,
        Err(CatalogError::UnknownCatalog { .. })
    ));
}

#[tokio::test]
async fn live_official_catalog_lists_account_models_when_enabled() {
    // No OpenAI credential exists on the build host, so this stays skipped
    // until one is supplied. It is the only path that proves the live headers
    // and the unpaginated envelope against the real service.
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let Ok(secret) = std::env::var("OPENAI_API_KEY") else {
        return;
    };
    let (context, credentials) = credentials(Some(&secret));
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let catalog =
        OpenAiCatalog::new(HttpService::new(Arc::new(transport)), credentials, query()).unwrap();
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert!(!models.is_empty());
    // The list endpoint publishes no capability, window or price field.
    for model in &models {
        assert_eq!(model.capabilities.tools, CapabilitySupport::Unknown);
        assert_eq!(model.context_window, None);
        assert!(model.pricing.is_unknown());
    }
    drop(context);
}

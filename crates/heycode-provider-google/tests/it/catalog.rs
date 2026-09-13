//! PGCP02 authenticated Gemini model discovery and capability normalization.
//!
//! Fixtures mirror the `ListModelsResponse` and `Model` shapes published in the
//! Gemini Developer API discovery document
//! (<https://generativelanguage.googleapis.com/$discovery/rest?version=v1>):
//! a `models` array of rows carrying `name`, `baseModelId`, `version`,
//! `displayName`, `description`, `inputTokenLimit`, `outputTokenLimit`,
//! `supportedGenerationMethods`, `thinking`, `temperature`, `maxTemperature`,
//! `topP` and `topK`, plus an optional `nextPageToken`. Only the *shape* is
//! pinned here; the numeric values are illustrative, because no numeric limit
//! is verifiable without calling the live service.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_credentials::{CredentialProviderId, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpResponse, HttpService, SERVICE_HTTP};
use heycode_llm::{
    CapabilitySupport, CatalogError, CatalogFailureKind, CatalogRefreshMode, CatalogRegistry,
    ModelCapabilities, ModelCatalog, ModelDescriptor, ModelLifecycleStatus, SERVICE_MODELS,
};
use heycode_provider_google::{
    GOOGLE_GEMINI_3_7_FLASH, GeminiCatalog, GoogleCatalogConfig, google_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    RecordedRequests, SecretProvider, TEST_SECRET, credentials, http, query, response,
};

/// The documented list path with the documented maximum page size.
const FIRST_PAGE_URL: &str = "https://generativelanguage.googleapis.com/v1/models?pageSize=1000";

fn make_catalog(
    secret: Option<&str>,
    responses: Vec<HttpResponse>,
) -> (Context, GeminiCatalog, RecordedRequests) {
    let (context, service) = credentials(secret);
    let (http, requests) = http(responses);
    let catalog = GeminiCatalog::api_key(http, service, query()).unwrap();
    (context, catalog, requests)
}

/// A thinking-capable flagship row in the documented shape.
fn flash_row() -> serde_json::Value {
    serde_json::json!({
        "name": format!("models/{GOOGLE_GEMINI_3_7_FLASH}"),
        "baseModelId": GOOGLE_GEMINI_3_7_FLASH,
        "version": "3.7",
        "displayName": "Gemini 3.7 Flash",
        "description": "Our latest and most capable Flash model.",
        "inputTokenLimit": 1_048_576,
        "outputTokenLimit": 65_536,
        "supportedGenerationMethods": ["generateContent", "countTokens"],
        "thinking": true,
        "temperature": 1.0,
        "maxTemperature": 2.0,
        "topP": 0.95,
        "topK": 64
    })
}

/// A row that explicitly denies thinking.
fn lite_row() -> serde_json::Value {
    serde_json::json!({
        "name": "models/gemini-3.5-flash-lite",
        "baseModelId": "gemini-3.5-flash-lite",
        "version": "3.5",
        "displayName": "Gemini 3.5 Flash-Lite",
        "inputTokenLimit": 1_048_576,
        "outputTokenLimit": 65_536,
        "supportedGenerationMethods": ["generateContent"],
        "thinking": false
    })
}

/// A row that publishes no capability or limit field at all.
fn bare_row() -> serde_json::Value {
    serde_json::json!({
        "name": "models/gemini-bare-preview",
        "baseModelId": "gemini-bare-preview",
        "version": "1.0",
        "supportedGenerationMethods": ["generateContent"]
    })
}

/// A real model this credential holds that no heycode inference request can call.
fn embedding_row() -> serde_json::Value {
    serde_json::json!({
        "name": "models/gemini-embedding-001",
        "baseModelId": "gemini-embedding-001",
        "version": "001",
        "displayName": "Gemini Embedding 001",
        "inputTokenLimit": 2048,
        "outputTokenLimit": 1,
        "supportedGenerationMethods": ["embedContent"]
    })
}

/// `supportedGenerationMethods` is optional, so a row may omit it entirely.
fn methodless_row() -> serde_json::Value {
    serde_json::json!({
        "name": "models/gemini-unstated",
        "baseModelId": "gemini-unstated",
        "version": "1.0",
        "displayName": "Gemini Unstated"
    })
}

fn page(rows: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "models": rows })
}

fn fixture() -> Vec<HttpResponse> {
    vec![response(
        200,
        page(vec![flash_row(), lite_row(), bare_row(), embedding_row()]),
    )]
}

fn find<'a>(models: &'a [ModelDescriptor], id: &str) -> &'a ModelDescriptor {
    models
        .iter()
        .find(|model| model.id == id)
        .unwrap_or_else(|| panic!("catalog is missing `{id}`"))
}

#[tokio::test]
async fn one_api_key_request_publishes_the_documented_fields_of_an_accessible_model() {
    let (_context, catalog, requests) = make_catalog(Some(TEST_SECRET), fixture());
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();

    // The API key travels in the documented header and never in the URL, and
    // the API-key mode names no quota project.
    assert_eq!(
        *requests.lock().unwrap(),
        [(
            FIRST_PAGE_URL.to_owned(),
            vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("x-goog-api-key".to_owned(), TEST_SECRET.to_owned()),
            ]
        )]
    );

    let flash = find(&models, GOOGLE_GEMINI_3_7_FLASH);
    // Identity is the `models/` resource-name suffix; `displayName` is display
    // metadata and never the request-facing id.
    assert_eq!(flash.display_name, "Gemini 3.7 Flash");
    assert_eq!(flash.context_window, Some(1_048_576));
    assert_eq!(flash.max_output_tokens, Some(65_536));
    // `baseModelId` names a family several rows share, so it is not published
    // as an alias that could collide with another row's id.
    assert!(flash.aliases.is_empty());
    // The endpoint publishes no lifecycle and no price field.
    assert_eq!(flash.lifecycle.status, ModelLifecycleStatus::Unknown);
    assert_eq!(flash.lifecycle.retirement_at_ms, None);
    assert!(flash.lifecycle.replacement_ids.is_empty());
    assert!(flash.pricing.is_unknown());
    assert!(flash.performance.is_unknown());
}

#[tokio::test]
async fn thinking_is_the_only_capability_with_evidence_and_unknown_never_becomes_supported() {
    let (_context, catalog, _) = make_catalog(Some(TEST_SECRET), fixture());
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();

    // `thinking: true` is explicit support for reasoning; every other heycode
    // capability has no field in the `Model` resource and stays Unknown.
    let flash = find(&models, GOOGLE_GEMINI_3_7_FLASH);
    assert_eq!(flash.capabilities.reasoning, CapabilitySupport::Supported);
    assert_eq!(
        ModelCapabilities {
            reasoning: CapabilitySupport::Unknown,
            ..flash.capabilities.clone()
        },
        ModelCapabilities::unknown()
    );

    // `thinking: false` is explicit non-support, which is not the same answer.
    let lite = find(&models, "gemini-3.5-flash-lite");
    assert_eq!(lite.capabilities.reasoning, CapabilitySupport::Unsupported);

    // An absent `thinking` field is Unknown: neither claim is made, and no
    // display name means identity falls back to the id rather than being
    // invented.
    let bare = find(&models, "gemini-bare-preview");
    assert_eq!(bare.capabilities, ModelCapabilities::unknown());
    assert_eq!(bare.display_name, "gemini-bare-preview");
    assert_eq!(bare.context_window, None);
    assert_eq!(bare.max_output_tokens, None);
}

#[tokio::test]
async fn a_model_that_cannot_serve_generate_content_is_not_published_as_available() {
    let (_context, catalog, _) = make_catalog(
        Some(TEST_SECRET),
        vec![response(
            200,
            page(vec![
                flash_row(),
                embedding_row(),
                methodless_row(),
                lite_row(),
            ]),
        )],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();

    // The embedding model is a real row this credential holds, and the
    // method-less row is well formed — neither is malformed, so neither
    // rejects the generation. Both are simply not callable by a heycode
    // inference request, so neither is offered as an available model.
    assert_eq!(
        models.iter().map(|model| &model.id).collect::<Vec<_>>(),
        vec!["gemini-3.5-flash-lite", GOOGLE_GEMINI_3_7_FLASH]
    );
}

#[tokio::test]
async fn no_lifecycle_is_inferred_from_a_model_id_that_reads_as_preview() {
    // The documented model list marks preview variants in prose, and their ids
    // usually say so too. The API publishes no lifecycle field, so reading one
    // out of the id would be a heuristic wearing the clothes of evidence.
    let (_context, catalog, _) = make_catalog(Some(TEST_SECRET), fixture());
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    let preview_named = find(&models, "gemini-bare-preview");
    assert_eq!(
        preview_named.lifecycle.status,
        ModelLifecycleStatus::Unknown
    );
    assert_eq!(preview_named.lifecycle.retirement_at_ms, None);
}

#[tokio::test]
async fn a_generation_with_no_callable_model_is_refused_instead_of_published_empty() {
    let (_context, catalog, _) = make_catalog(
        Some(TEST_SECRET),
        vec![response(200, page(vec![embedding_row(), methodless_row()]))],
    );
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn an_account_without_the_provider_default_still_publishes_its_accessible_models() {
    // The list is credential-scoped: it is exactly what this key may call. A
    // key with no access to the provider default is a real account, not a
    // malformed response, so its generation must publish rather than fail.
    let (_context, catalog, _) = make_catalog(
        Some(TEST_SECRET),
        vec![response(200, page(vec![lite_row()]))],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert!(
        models
            .iter()
            .all(|model| model.id != GOOGLE_GEMINI_3_7_FLASH)
    );
}

#[tokio::test]
async fn a_malformed_row_rejects_the_whole_generation() {
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "resource name is not a model",
            serde_json::json!({
                "name": "tunedModels/my-tuning", "baseModelId": "gemini-3.7-flash",
                "version": "3.7", "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "blank id after the documented prefix",
            serde_json::json!({
                "name": "models/", "baseModelId": "gemini-3.7-flash",
                "version": "3.7", "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "id outside the unreserved URL charset",
            serde_json::json!({
                "name": "models/gemini?pageSize=1", "baseModelId": "gemini",
                "version": "3.7", "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "id carrying the method separator",
            serde_json::json!({
                "name": "models/gemini-3.7-flash:generateContent",
                "baseModelId": "gemini-3.7-flash", "version": "3.7",
                "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "missing baseModelId",
            serde_json::json!({
                "name": "models/gemini-no-base", "version": "3.7",
                "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "missing version",
            serde_json::json!({
                "name": "models/gemini-no-version", "baseModelId": "gemini-no-version",
                "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "blank version",
            serde_json::json!({
                "name": "models/gemini-blank-version", "baseModelId": "gemini-blank-version",
                "version": "   ", "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "display name carrying a control character",
            serde_json::json!({
                "name": "models/gemini-control", "baseModelId": "gemini-control",
                "version": "3.7", "displayName": "Gemini\u{7}Flash",
                "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "zero input token limit",
            serde_json::json!({
                "name": "models/gemini-zero-in", "baseModelId": "gemini-zero-in",
                "version": "3.7", "inputTokenLimit": 0,
                "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            "negative output token limit",
            serde_json::json!({
                "name": "models/gemini-negative-out", "baseModelId": "gemini-negative-out",
                "version": "3.7", "outputTokenLimit": -1,
                "supportedGenerationMethods": ["generateContent"]
            }),
        ),
        (
            // A row that is not callable is still validated before it is
            // dropped: a malformed body is a malformed generation.
            "malformed row that does not advertise generateContent",
            serde_json::json!({
                "name": "models/gemini-zero-embed", "baseModelId": "gemini-zero-embed",
                "version": "001", "inputTokenLimit": 0,
                "supportedGenerationMethods": ["embedContent"]
            }),
        ),
    ];

    for (label, row) in cases {
        let (_context, catalog, _) = make_catalog(
            Some(TEST_SECRET),
            vec![response(200, page(vec![flash_row(), row]))],
        );
        let error = catalog
            .fetch(CancellationToken::new())
            .await
            .expect_err(label);
        assert_eq!(
            error.kind(),
            CatalogFailureKind::InvalidResponse,
            "{label} must reject the generation"
        );
    }
}

#[tokio::test]
async fn a_malformed_envelope_rejects_the_whole_generation_without_leaking_the_body() {
    let cases: Vec<(&str, Vec<HttpResponse>)> = vec![
        (
            "models is not an array",
            vec![response(
                200,
                serde_json::json!({ "models": "must-not-leak" }),
            )],
        ),
        ("empty generation", vec![response(200, page(Vec::new()))]),
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
                body: page(vec![flash_row()]).to_string().into_bytes(),
            }],
        ),
        (
            "empty page token promises a page it cannot address",
            vec![response(
                200,
                serde_json::json!({ "models": [flash_row()], "nextPageToken": "" }),
            )],
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
        assert!(!format!("{error:?}").contains("must-not-leak"), "{label}");
    }
}

#[tokio::test]
async fn a_duplicate_model_id_across_pages_rejects_the_generation() {
    let (_context, catalog, _) = make_catalog(
        Some(TEST_SECRET),
        vec![
            response(
                200,
                serde_json::json!({ "models": [flash_row()], "nextPageToken": "next" }),
            ),
            response(200, page(vec![flash_row()])),
        ],
    );
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn pagination_follows_the_documented_token_and_percent_encodes_it() {
    let (_context, catalog, requests) = make_catalog(
        Some(TEST_SECRET),
        vec![
            response(
                200,
                serde_json::json!({
                    "models": [flash_row()],
                    // Page tokens are opaque server strings and may carry
                    // bytes that are meaningful inside a query string.
                    "nextPageToken": "a+b/c=d&pageSize=1"
                }),
            ),
            response(200, page(vec![lite_row()])),
        ],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 2);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, FIRST_PAGE_URL);
    assert_eq!(
        requests[1].0,
        format!("{FIRST_PAGE_URL}&pageToken=a%2Bb%2Fc%3Dd%26pageSize%3D1")
    );
}

#[tokio::test]
async fn a_never_ending_cursor_fails_instead_of_paging_forever() {
    // Every page carries a distinct row, so the budget is the only thing that
    // can stop the walk.
    let responses = (0..17)
        .map(|index| {
            response(
                200,
                serde_json::json!({
                    "models": [{
                        "name": format!("models/gemini-page-{index}"),
                        "baseModelId": format!("gemini-page-{index}"),
                        "version": "1.0",
                        "supportedGenerationMethods": ["generateContent"]
                    }],
                    "nextPageToken": "always"
                }),
            )
        })
        .collect();
    let (_context, catalog, requests) = make_catalog(Some(TEST_SECRET), responses);
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert_eq!(requests.lock().unwrap().len(), 16);
}

/// One bulk row that would publish normally.
fn bulk_row(index: usize, method: &str) -> serde_json::Value {
    serde_json::json!({
        "name": format!("models/gemini-bulk-{index}"),
        "baseModelId": format!("gemini-bulk-{index}"),
        "version": "1.0",
        "supportedGenerationMethods": [method]
    })
}

#[tokio::test]
async fn an_oversized_payload_is_refused_before_it_is_normalized() {
    // Every row here would publish, so without the cap this is a successful
    // 4097-row generation rather than any other failure.
    let rows: Vec<serde_json::Value> = (0..4097)
        .map(|index| bulk_row(index, "generateContent"))
        .collect();
    let (_context, catalog, _) = make_catalog(Some(TEST_SECRET), vec![response(200, page(rows))]);
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    // Named exactly, because "too large" and "nothing callable" are both
    // invalid responses and a test that cannot tell them apart proves neither.
    assert!(
        error.message().contains("too large"),
        "unexpected failure: {}",
        error.message()
    );
}

#[tokio::test]
async fn the_row_cap_counts_every_row_the_host_sent_not_the_rows_that_survive_filtering() {
    // 4096 uncallable rows plus one callable one: the published generation
    // would be a single model, so a cap applied after filtering would let this
    // payload through.
    let mut rows: Vec<serde_json::Value> = (0..4096)
        .map(|index| bulk_row(index, "embedContent"))
        .collect();
    rows.push(flash_row());
    let (_context, catalog, _) = make_catalog(Some(TEST_SECRET), vec![response(200, page(rows))]);
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(
        error.message().contains("too large"),
        "unexpected failure: {}",
        error.message()
    );
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
                        "code": status,
                        "message": "must-not-leak",
                        "status": "PERMISSION_DENIED"
                    }
                }),
            )],
        );
        let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind(), kind, "HTTP {status}");
        assert!(!error.message().contains("must-not-leak"), "HTTP {status}");
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("must-not-leak"), "HTTP {status}");
        // A failure carries neither the provider body nor the credential that
        // produced it.
        assert!(!rendered.contains(TEST_SECRET), "HTTP {status}");
    }
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

#[tokio::test]
async fn a_custom_base_url_reaches_the_wire_and_an_invalid_one_never_publishes() {
    let (context, credentials) = credentials(Some(TEST_SECRET));
    let (service, requests) = http(vec![response(200, page(vec![flash_row()]))]);
    let catalog = GeminiCatalog::api_key(service, credentials.clone(), query())
        .unwrap()
        .with_base_url("https://gateway.example.internal/gemini/")
        .unwrap();
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(
        requests.lock().unwrap()[0].0,
        "https://gateway.example.internal/gemini/v1/models?pageSize=1000"
    );

    let (bad_http, _) = http(Vec::new());
    assert!(
        GeminiCatalog::api_key(bad_http, credentials, query())
            .unwrap()
            .with_base_url("not-a-url")
            .is_err(),
        "an unusable base URL must fail before publication"
    );
    drop(context);
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
                    secret: Some(TEST_SECRET.to_owned()),
                }),
            )
            .map_err(|error| CoreError::other(error.to_string()))
    }
}

#[tokio::test]
async fn catalog_plugin_registers_and_disposes_the_google_source() {
    let (http, _requests) = http(fixture());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(http)),
        Box::new(CredentialsPlugin),
        google_catalog_plugin(GoogleCatalogConfig::api_key(query())),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            "google",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        view.snapshot
            .models
            .iter()
            .any(|model| model.id == GOOGLE_GEMINI_3_7_FLASH)
    );
    // The contribution is declared before apply and audited against the live
    // registry, so a registration nobody declared fails composition evidence.
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "catalog-google"
                && row.kind == heycode_core::ContributionKind::ModelCatalog
                && row.name == "google")
    );

    context.shutdown();
    assert!(matches!(
        models
            .refresh(
                "google",
                CatalogRefreshMode::Force,
                CancellationToken::new()
            )
            .await,
        Err(CatalogError::UnknownCatalog { .. })
    ));
}

#[tokio::test]
async fn live_official_catalog_lists_accessible_models_when_enabled() {
    // No Google credential exists on the build host, so this stays skipped
    // until one is supplied. It is the only path that proves the live header,
    // the documented page parameters and the envelope against the real
    // service.
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let Ok(secret) = std::env::var("GEMINI_API_KEY") else {
        return;
    };
    let (context, credentials) = credentials(Some(&secret));
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let catalog =
        GeminiCatalog::api_key(HttpService::new(Arc::new(transport)), credentials, query())
            .unwrap();
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert!(!models.is_empty());
    for model in &models {
        // The list endpoint publishes no tool, price or lifecycle field.
        assert_eq!(model.capabilities.tools, CapabilitySupport::Unknown);
        assert!(model.pricing.is_unknown());
        assert_eq!(model.lifecycle.status, ModelLifecycleStatus::Unknown);
    }
    drop(context);
}

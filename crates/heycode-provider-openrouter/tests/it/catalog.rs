//! POR02 current OpenRouter catalog and GLM-5.3-Flash evidence.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SERVICE_HTTP, SseEventStream,
};
use heycode_llm::testing::{
    CatalogConformanceFixture, ConformanceFixtureMetadata, ConformanceSourceKind,
};
use heycode_llm::{
    CapabilitySupport, CatalogError, CatalogFailureKind, CatalogRefreshMode, CatalogRegistry,
    ModelCatalog, ModelLifecycleStatus, SERVICE_MODELS,
};
use heycode_provider_openrouter::{
    OPENROUTER_GLM_5_3_FLASH, OpenRouterCatalog, OpenRouterCatalogConfig, openrouter_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

type RecordedRequests = Arc<Mutex<Vec<(String, Vec<String>)>>>;

const FIXTURE_CAPTURED_AT_MS: u64 = 1_788_048_000_000;
const FIXTURE_SOURCE_VERSION: &str = "openrouter-api-v1";
const MODELS_SOURCE: &str = "https://openrouter.ai/api/v1/models";
const GLM_SOURCE: &str = "https://openrouter.ai/api/v1/model/z-ai/glm-5.3-flash";

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
                .map(|header| header.name().to_owned())
                .collect(),
        ));
        let response = self.responses.lock().unwrap().remove(0);
        Box::pin(async move { Ok(response) })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn response(status: u16, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        headers: std::collections::BTreeMap::new(),
        status,
        content_type: Some("application/json".to_owned()),
        body: body.to_string().into_bytes(),
    }
}

fn glm_row() -> serde_json::Value {
    serde_json::json!({
        "id": OPENROUTER_GLM_5_3_FLASH,
        "canonical_slug": "z-ai/glm-5.3-flash-20260826",
        "name": "Z.ai: GLM 5.3 Flash",
        "created": 1787752741_u64,
        "description": "current fixture",
        "context_length": 1_310_720_u64,
        "architecture": {
            "input_modalities": ["text", "image", "video"],
            "output_modalities": ["text"],
            "tokenizer": "Other",
            "instruct_type": null
        },
        "pricing": {
            "prompt": "0.000000075",
            "completion": "0.00000025",
            "input_cache_read": "0.000000015"
        },
        "top_provider": {
            "context_length": 1_048_576_u64,
            "max_completion_tokens": 131_072_u64,
            "is_moderated": false
        },
        "supported_parameters": [
            "include_reasoning", "max_tokens", "reasoning", "reasoning_effort",
            "response_format", "structured_outputs", "temperature", "tool_choice", "tools"
        ],
        "default_parameters": {"temperature": 1, "top_p": 0.95},
        "expiration_date": "2098-12-31",
        "reasoning": {
            "mandatory": true,
            "default_enabled": true,
            "supported_efforts": ["max", "high", "low"],
            "default_effort": "max"
        }
    })
}

fn text_row() -> serde_json::Value {
    serde_json::json!({
        "id": "example/text-only",
        "canonical_slug": "example/text-only-20260801",
        "name": "Example Text Only ",
        "created": 1786000000_u64,
        "description": "fixture\nwith provider formatting",
        "context_length": 8192,
        "architecture": {
            "input_modalities": ["text"],
            "output_modalities": ["text"],
            "tokenizer": "Other",
            "instruct_type": null
        },
        "pricing": {"prompt": "0.1", "completion": "0.2"},
        "top_provider": {
            "context_length": 8192,
            "max_completion_tokens": 4096,
            "is_moderated": false
        },
        "supported_parameters": [],
        "default_parameters": {},
        "expiration_date": null
    })
}

fn fixtures() -> Vec<HttpResponse> {
    fixtures_with_glm(glm_row())
}

fn fixtures_with_glm(glm: serde_json::Value) -> Vec<HttpResponse> {
    catalog_fixtures_with_glm(glm)
        .into_iter()
        .map(|fixture| response(200, fixture.into_payload()))
        .collect()
}

fn catalog_fixtures_with_glm(glm: serde_json::Value) -> Vec<CatalogConformanceFixture> {
    vec![
        catalog_fixture(
            "openrouter/models-current",
            MODELS_SOURCE,
            serde_json::json!({
                "data": [text_row(), glm.clone()],
                "total_count": 2,
                "links": {"next": null}
            }),
        ),
        catalog_fixture(
            "openrouter/glm-5.3-flash-current",
            GLM_SOURCE,
            serde_json::json!({"data": glm}),
        ),
    ]
}

fn catalog_fixture(
    name: &str,
    source: &str,
    payload: serde_json::Value,
) -> CatalogConformanceFixture {
    let metadata = ConformanceFixtureMetadata::new(
        "openrouter",
        ConformanceSourceKind::RedactedCapture,
        source,
        FIXTURE_SOURCE_VERSION,
        FIXTURE_CAPTURED_AT_MS,
    )
    .unwrap();
    let fixture = CatalogConformanceFixture::new(name, metadata, payload).unwrap();
    CatalogConformanceFixture::from_json(&fixture.to_json().unwrap()).unwrap()
}

#[test]
fn current_catalog_fixtures_name_their_primary_sources_version_and_capture() {
    let fixtures = catalog_fixtures_with_glm(glm_row());
    assert_eq!(fixtures.len(), 2);
    assert_eq!(fixtures[0].metadata().source(), MODELS_SOURCE);
    assert_eq!(fixtures[1].metadata().source(), GLM_SOURCE);
    for fixture in fixtures {
        assert_eq!(fixture.metadata().provider(), "openrouter");
        assert_eq!(
            fixture.metadata().source_kind(),
            ConformanceSourceKind::RedactedCapture
        );
        assert_eq!(fixture.metadata().source_version(), FIXTURE_SOURCE_VERSION);
        assert_eq!(fixture.metadata().captured_at_ms(), FIXTURE_CAPTURED_AT_MS);
        assert!(fixture.payload()["data"].is_array() || fixture.payload()["data"].is_object());
    }
}

fn make_catalog(responses: Vec<HttpResponse>) -> (OpenRouterCatalog, RecordedRequests) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(responses),
        requests: requests.clone(),
    }));
    (OpenRouterCatalog::new(http).unwrap(), requests)
}

#[tokio::test]
async fn full_and_single_model_shapes_normalize_current_glm_evidence() {
    let (catalog, requests) = make_catalog(fixtures());
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 2);

    let glm = models
        .iter()
        .find(|model| model.id == OPENROUTER_GLM_5_3_FLASH)
        .unwrap();
    assert_eq!(glm.display_name, "Z.ai: GLM 5.3 Flash");
    assert!(glm.created_at_ms.is_some());
    assert_eq!(glm.context_window, Some(1_048_576));
    assert_eq!(glm.max_output_tokens, Some(131_072));
    assert_eq!(glm.lifecycle.status, ModelLifecycleStatus::Deprecated);
    assert!(glm.lifecycle.retirement_at_ms.is_some());
    assert_eq!(glm.capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(glm.capabilities.reasoning, CapabilitySupport::Supported);
    assert_eq!(glm.capabilities.image_input, CapabilitySupport::Supported);
    assert_eq!(
        glm.capabilities.document_input,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        glm.capabilities.structured_output,
        CapabilitySupport::Supported
    );
    assert_eq!(glm.capabilities.prompt_cache, CapabilitySupport::Supported);
    assert_eq!(glm.capabilities.native_web, CapabilitySupport::Supported);

    let text = models
        .iter()
        .find(|model| model.id == "example/text-only")
        .unwrap();
    assert_eq!(text.capabilities.tools, CapabilitySupport::Unsupported);
    assert_eq!(text.capabilities.reasoning, CapabilitySupport::Unsupported);
    assert_eq!(text.capabilities.native_web, CapabilitySupport::Supported);
    assert_eq!(
        text.capabilities.image_input,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        text.capabilities.structured_output,
        CapabilitySupport::Unsupported
    );

    assert_eq!(
        *requests.lock().unwrap(),
        [
            (
                "https://openrouter.ai/api/v1/models".to_owned(),
                vec!["accept".to_owned()]
            ),
            (
                "https://openrouter.ai/api/v1/model/z-ai/glm-5.3-flash".to_owned(),
                vec!["accept".to_owned()]
            ),
        ]
    );
    // CAT06: published per-token prices normalize exactly, in the currency and
    // unit they were published in, and an unpublished component stays absent
    // rather than becoming a zero a consumer could read as free.
    let pricing = &glm.pricing;
    assert_eq!(pricing.currency(), Some(heycode_llm::PriceCurrency::Usd));
    assert_eq!(pricing.unit(), Some(heycode_llm::TokenPriceUnit::PerToken));
    let provenance = pricing.provenance().expect("priced rows retain provenance");
    assert_eq!(provenance.source(), "openrouter:models-api");
    assert!(provenance.captured_at_ms() > 0);
    for (component, expected) in [
        // pico-units are 10^-12 USD, so 0.000000075 USD/token is 75_000.
        (heycode_llm::PriceComponent::Input, 75_000_u64),
        (heycode_llm::PriceComponent::Output, 250_000),
        (heycode_llm::PriceComponent::CachedInput, 15_000),
    ] {
        assert_eq!(
            pricing.price(component).map(|price| price.pico_units()),
            Some(expected),
            "{component:?} must normalize exactly"
        );
    }
    for absent in [
        heycode_llm::PriceComponent::CacheWrite,
        heycode_llm::PriceComponent::Reasoning,
    ] {
        assert!(
            pricing.price(absent).is_none(),
            "{absent:?} was not published and must stay unknown, not free"
        );
    }
    // The catalog publishes no latency or throughput evidence.
    assert!(glm.performance.is_unknown());
}

#[tokio::test]
async fn exact_glm_reasoning_and_tool_contract_drift_rejects_the_generation() {
    let mut changed_efforts = glm_row();
    changed_efforts["reasoning"]["supported_efforts"] = serde_json::json!(["max", "medium", "low"]);

    let mut changed_default = glm_row();
    changed_default["reasoning"]["default_effort"] = serde_json::json!("high");

    let mut optional_reasoning = glm_row();
    optional_reasoning["reasoning"]["mandatory"] = serde_json::json!(false);

    let mut disabled_by_default = glm_row();
    disabled_by_default["reasoning"]["default_enabled"] = serde_json::json!(false);

    let mut missing_tool_choice = glm_row();
    missing_tool_choice["supported_parameters"]
        .as_array_mut()
        .expect("fixture parameters are an array")
        .retain(|parameter| parameter.as_str() != Some("tool_choice"));

    let mut missing_tools = glm_row();
    missing_tools["supported_parameters"]
        .as_array_mut()
        .expect("fixture parameters are an array")
        .retain(|parameter| parameter.as_str() != Some("tools"));

    let mut missing_reasoning_parameter = glm_row();
    missing_reasoning_parameter["supported_parameters"]
        .as_array_mut()
        .expect("fixture parameters are an array")
        .retain(|parameter| parameter.as_str() != Some("reasoning"));

    for row in [
        changed_efforts,
        changed_default,
        optional_reasoning,
        disabled_by_default,
        missing_tool_choice,
        missing_tools,
        missing_reasoning_parameter,
    ] {
        let (catalog, _) = make_catalog(fixtures_with_glm(row));
        let error = catalog
            .fetch(CancellationToken::new())
            .await
            .expect_err("drift from the strict GLM route must reject the generation");
        assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    }
}

/// A non-GLM reasoning row whose published vocabulary is its own.
fn graded_row() -> serde_json::Value {
    serde_json::json!({
        "id": "vendor/graded-reasoner",
        "canonical_slug": "vendor/graded-reasoner-20260801",
        "name": "Vendor Graded Reasoner",
        "created": 1786100000_u64,
        "description": "fixture",
        "context_length": 200_000_u64,
        "architecture": {
            "input_modalities": ["text"],
            "output_modalities": ["text"],
            "tokenizer": "Other",
            "instruct_type": null
        },
        "pricing": {"prompt": "0.1", "completion": "0.2"},
        "top_provider": {
            "context_length": 200_000_u64,
            "max_completion_tokens": 64_000_u64,
            "is_moderated": false
        },
        "supported_parameters": ["reasoning", "max_tokens"],
        "expiration_date": null,
        "reasoning": {
            "mandatory": false,
            "default_enabled": true,
            "supported_efforts": ["low", "medium", "high"],
            "default_effort": "medium"
        }
    })
}

/// A reasoning-capable row that names no effort vocabulary at all.
fn unpublished_effort_row() -> serde_json::Value {
    let mut row = graded_row();
    row["id"] = serde_json::json!("vendor/silent-reasoner");
    row["canonical_slug"] = serde_json::json!("vendor/silent-reasoner-20260801");
    row["name"] = serde_json::json!("Vendor Silent Reasoner");
    row["reasoning"] = serde_json::json!({"mandatory": false, "default_enabled": true});
    row
}

fn fixtures_with_rows(rows: Vec<serde_json::Value>) -> Vec<HttpResponse> {
    let mut data = vec![text_row(), glm_row()];
    data.extend(rows);
    vec![
        response(
            200,
            serde_json::json!({
                "data": data.clone(),
                "total_count": data.len(),
                "links": {"next": null}
            }),
        ),
        response(200, serde_json::json!({"data": glm_row()})),
    ]
}

#[tokio::test]
async fn each_row_retains_the_exact_reasoning_vocabulary_it_published() {
    let (catalog, _) = make_catalog(fixtures_with_rows(vec![
        graded_row(),
        unpublished_effort_row(),
    ]));
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    let row = |id: &str| {
        models
            .iter()
            .find(|model| model.id == id)
            .unwrap_or_else(|| panic!("catalog is missing `{id}`"))
            .clone()
    };

    // Published order, default and flags survive normalization untouched.
    let graded = row("vendor/graded-reasoner");
    let reasoning = graded.reasoning.as_ref().expect("published reasoning");
    assert_eq!(reasoning.efforts(), ["low", "medium", "high"]);
    assert_eq!(reasoning.default_effort(), Some("medium"));
    assert_eq!(reasoning.default_enabled(), Some(true));
    assert_eq!(reasoning.mandatory(), Some(false));
    assert_eq!(graded.capabilities.reasoning, CapabilitySupport::Supported);

    // The strict route keeps exactly the values it publishes.
    let glm = row(OPENROUTER_GLM_5_3_FLASH);
    let strict = glm.reasoning.as_ref().expect("GLM publishes reasoning");
    assert_eq!(strict.efforts(), ["max", "high", "low"]);
    assert_eq!(strict.default_effort(), Some("max"));

    // A reasoning row without a vocabulary stays reasoning-capable with an
    // unknown vocabulary; nothing invents one for it.
    let silent = row("vendor/silent-reasoner");
    let unnamed = silent
        .reasoning
        .as_ref()
        .expect("published reasoning state");
    assert!(!unnamed.names_efforts());
    assert_eq!(unnamed.default_effort(), None);
    assert_eq!(silent.capabilities.reasoning, CapabilitySupport::Supported);

    // A row with no reasoning block at all retains nothing.
    assert!(row("example/text-only").reasoning.is_none());
}

#[tokio::test]
async fn contradictory_published_reasoning_metadata_rejects_the_generation() {
    let mut default_outside_vocabulary = graded_row();
    default_outside_vocabulary["reasoning"]["default_effort"] = serde_json::json!("xhigh");

    let mut default_without_vocabulary = graded_row();
    default_without_vocabulary["reasoning"] =
        serde_json::json!({"default_effort": "medium", "default_enabled": true});

    let mut duplicate_efforts = graded_row();
    duplicate_efforts["reasoning"]["supported_efforts"] = serde_json::json!(["low", "low", "high"]);

    let mut padded_effort = graded_row();
    padded_effort["reasoning"]["supported_efforts"] = serde_json::json!(["low", " medium", "high"]);

    for row in [
        default_outside_vocabulary,
        default_without_vocabulary,
        duplicate_efforts,
        padded_effort,
    ] {
        let (catalog, _) = make_catalog(fixtures_with_rows(vec![row]));
        let error = catalog
            .fetch(CancellationToken::new())
            .await
            .expect_err("invalid reasoning metadata must reject the generation");
        assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    }
}

#[tokio::test]
async fn malformed_or_partial_generation_and_statuses_fail_without_rows() {
    let (catalog, _) = make_catalog(vec![response(
        200,
        serde_json::json!({
            "data": [glm_row()],
            "total_count": 2,
            "links": {"next": "/api/v1/models?offset=1&limit=1"}
        }),
    )]);
    assert_eq!(
        catalog
            .fetch(CancellationToken::new())
            .await
            .unwrap_err()
            .kind(),
        CatalogFailureKind::InvalidResponse
    );

    for (status, kind) in [
        (401, CatalogFailureKind::Unauthorized),
        (429, CatalogFailureKind::Unavailable),
        (500, CatalogFailureKind::Unavailable),
    ] {
        let (catalog, _) = make_catalog(vec![response(
            status,
            serde_json::json!({"private":"must-not-leak"}),
        )]);
        let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(!error.message().contains("must-not-leak"));
    }
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

#[tokio::test]
async fn catalog_plugin_registers_and_disposes_the_openrouter_source() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(fixtures()),
        requests,
    }));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(http)),
        openrouter_catalog_plugin(OpenRouterCatalogConfig::official()),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            "openrouter",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(view.snapshot.models.iter().any(|model| {
        model.id == OPENROUTER_GLM_5_3_FLASH
            && model.capabilities.tools == CapabilitySupport::Supported
    }));

    context.shutdown();
    assert!(matches!(
        models
            .refresh(
                "openrouter",
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await,
        Err(CatalogError::UnknownCatalog { .. })
    ));
}

#[tokio::test]
async fn live_official_catalog_matches_the_glm_fixture_when_enabled() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let catalog = OpenRouterCatalog::new(HttpService::new(Arc::new(transport))).unwrap();
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    let glm = models
        .iter()
        .find(|model| model.id == OPENROUTER_GLM_5_3_FLASH)
        .unwrap();
    assert_eq!(glm.display_name, "Z.ai: GLM 5.3 Flash");
    assert_eq!(glm.context_window, Some(1_048_576));
    assert_eq!(glm.max_output_tokens, Some(131_072));
    assert_eq!(glm.capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(glm.capabilities.reasoning, CapabilitySupport::Supported);
    assert_eq!(glm.capabilities.image_input, CapabilitySupport::Supported);
}

#[tokio::test]
async fn routing_prices_and_sub_pico_prices_do_not_hide_the_catalog() {
    for unsupported in ["-1", "0.0000000416666666666667"] {
        let mut row = text_row();
        row["pricing"]["prompt"] = unsupported.into();
        row["pricing"]["input_cache_write"] = unsupported.into();
        let (catalog, _) = make_catalog(vec![
            response(
                200,
                serde_json::json!({
                    "data": [row, glm_row()], "total_count": 2, "links": {"next": null}
                }),
            ),
            response(200, serde_json::json!({"data": glm_row()})),
        ]);
        let models = catalog.fetch(CancellationToken::new()).await.unwrap();
        assert_eq!(models.len(), 2);
        let text = models
            .iter()
            .find(|model| model.id == "example/text-only")
            .unwrap();
        assert!(
            text.pricing
                .price(heycode_llm::PriceComponent::Input)
                .is_none()
        );
        assert!(
            text.pricing
                .price(heycode_llm::PriceComponent::CacheWrite)
                .is_none()
        );
        assert!(
            text.pricing
                .price(heycode_llm::PriceComponent::Output)
                .is_some()
        );
    }
}

#[tokio::test]
async fn malformed_prices_still_fail_instead_of_becoming_free() {
    for malformed in ["-2", "NaN", "not-a-price"] {
        let mut row = glm_row();
        row["pricing"]["prompt"] = malformed.into();
        let (catalog, _) = make_catalog(fixtures_with_glm(row));
        assert_eq!(
            catalog
                .fetch(CancellationToken::new())
                .await
                .unwrap_err()
                .kind(),
            CatalogFailureKind::InvalidResponse
        );
    }
}

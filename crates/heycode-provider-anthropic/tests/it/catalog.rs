//! PAN01 authenticated Anthropic model-list discovery and normalization.
//!
//! Fixtures mirror the documented `GET /v1/models` shape
//! (<https://platform.claude.com/docs/en/api/models-list>): a `data` array of
//! `ModelInfo` rows plus `first_id`, `last_id` and `has_more` cursor fields.

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
    ModelCatalog, ModelDescriptor, ModelLifecycleStatus, SERVICE_MODELS,
};
use heycode_provider_anthropic::{
    ANTHROPIC_API_KEY_REFERENCE, ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_VERSION, AnthropicCatalog,
    AnthropicCatalogConfig, anthropic_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

const TEST_SECRET: &str = "sk-ant-test-not-a-real-key";
const FIRST_PAGE_URL: &str = "https://api.anthropic.com/v1/models?limit=1000";
const SECOND_PAGE_URL: &str =
    "https://api.anthropic.com/v1/models?limit=1000&after_id=claude-opus-5";

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
        CredentialReference::new(ANTHROPIC_API_KEY_REFERENCE).unwrap(),
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
) -> (Context, AnthropicCatalog, RecordedRequests) {
    let (context, service) = credentials(secret);
    let (http, requests) = http(responses);
    let catalog = AnthropicCatalog::new(http, service, query()).unwrap();
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

fn capability(supported: bool) -> serde_json::Value {
    serde_json::json!({ "supported": supported })
}

/// The documented capability tree for a current model, verbatim in shape.
fn full_capabilities() -> serde_json::Value {
    serde_json::json!({
        "batch": capability(true),
        "citations": capability(true),
        "code_execution": capability(true),
        "context_management": {
            "clear_thinking_20251015": capability(true),
            "clear_tool_uses_20250919": capability(true),
            "compact_20260112": capability(true),
            "supported": true
        },
        "effort": {
            "low": capability(true),
            "medium": capability(true),
            "high": capability(true),
            "max": capability(true),
            "xhigh": capability(true),
            "supported": true
        },
        "image_input": capability(true),
        "pdf_input": capability(true),
        "structured_outputs": capability(true),
        "thinking": {
            "supported": true,
            "types": { "adaptive": capability(true), "enabled": capability(true) }
        }
    })
}

fn opus_row() -> serde_json::Value {
    serde_json::json!({
        "id": ANTHROPIC_CLAUDE_OPUS_5,
        "type": "model",
        "display_name": "Claude Opus 5",
        "created_at": "2026-07-24T00:00:00Z",
        "max_input_tokens": 1_000_000_u64,
        "max_tokens": 128_000_u64,
        "capabilities": full_capabilities()
    })
}

/// Explicit negative evidence plus an absent `compact_20260112` strategy.
fn haiku_row() -> serde_json::Value {
    serde_json::json!({
        "id": "claude-haiku-4-5",
        "type": "model",
        "display_name": " Claude Haiku 4.5 ",
        "created_at": "2025-10-01T00:00:00Z",
        "max_input_tokens": 200_000_u64,
        "max_tokens": 64_000_u64,
        "capabilities": {
            "batch": capability(true),
            "citations": capability(true),
            "code_execution": capability(false),
            "context_management": { "supported": false },
            "effort": {
                "low": capability(false),
                "medium": capability(false),
                "high": capability(false),
                "max": capability(false),
                "supported": false
            },
            "image_input": capability(true),
            "pdf_input": capability(false),
            "structured_outputs": capability(true),
            "thinking": {
                "supported": false,
                "types": { "adaptive": capability(false), "enabled": capability(false) }
            }
        }
    })
}

/// `capabilities`, `max_input_tokens` and `max_tokens` are all documented as
/// nullable; absence is no evidence at all.
fn null_evidence_row() -> serde_json::Value {
    serde_json::json!({
        "id": "claude-legacy-unknown",
        "type": "model",
        "display_name": "Claude Legacy Unknown",
        "created_at": "1970-01-01T00:00:00Z",
        "max_input_tokens": null,
        "max_tokens": null,
        "capabilities": null
    })
}

fn page(rows: Vec<serde_json::Value>, has_more: bool) -> serde_json::Value {
    let first = rows
        .first()
        .and_then(|row| row["id"].as_str())
        .map(str::to_owned);
    let last = rows
        .last()
        .and_then(|row| row["id"].as_str())
        .map(str::to_owned);
    serde_json::json!({
        "data": rows,
        "first_id": first,
        "last_id": last,
        "has_more": has_more
    })
}

fn paged_fixture() -> Vec<HttpResponse> {
    vec![
        response(200, page(vec![opus_row()], true)),
        response(200, page(vec![haiku_row(), null_evidence_row()], false)),
    ]
}

fn find<'a>(models: &'a [ModelDescriptor], id: &str) -> &'a ModelDescriptor {
    models
        .iter()
        .find(|model| model.id == id)
        .unwrap_or_else(|| panic!("catalog is missing `{id}`"))
}

#[tokio::test]
async fn authenticated_pages_normalize_documented_capability_evidence() {
    let (_context, catalog, requests) = make_catalog(Some(TEST_SECRET), paged_fixture());
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 3);

    let opus = find(&models, ANTHROPIC_CLAUDE_OPUS_5);
    assert_eq!(opus.display_name, "Claude Opus 5");
    assert_eq!(opus.created_at_ms, Some(1_784_851_200_000));
    assert_eq!(opus.context_window, Some(1_000_000));
    assert_eq!(opus.max_output_tokens, Some(128_000));
    assert_eq!(opus.capabilities.reasoning, CapabilitySupport::Supported);
    assert_eq!(opus.capabilities.image_input, CapabilitySupport::Supported);
    assert_eq!(
        opus.capabilities.document_input,
        CapabilitySupport::Supported
    );
    assert_eq!(
        opus.capabilities.structured_output,
        CapabilitySupport::Supported
    );
    assert_eq!(
        opus.capabilities.native_compaction,
        CapabilitySupport::Supported
    );
    // Current primary docs state that prompt caching is supported on all
    // active Claude models. A successful account-scoped list row is therefore
    // exact provider-wide evidence even though the row has no cache field.
    assert_eq!(opus.capabilities.prompt_cache, CapabilitySupport::Supported);
    // Tool-calling and provider-web still have no equivalent evidence.
    for unknown in [opus.capabilities.tools, opus.capabilities.native_web] {
        assert_eq!(unknown, CapabilitySupport::Unknown);
    }
    // Neither lifecycle, price nor performance is published by this endpoint.
    assert_eq!(opus.lifecycle.status, ModelLifecycleStatus::Unknown);
    assert_eq!(opus.lifecycle.retirement_at_ms, None);
    assert!(opus.pricing.is_unknown());
    assert!(opus.performance.is_unknown());
    // The list endpoint publishes no alias field.
    assert!(opus.aliases.is_empty());

    let haiku = find(&models, "claude-haiku-4-5");
    assert_eq!(haiku.display_name, "Claude Haiku 4.5");
    assert_eq!(haiku.context_window, Some(200_000));
    assert_eq!(haiku.max_output_tokens, Some(64_000));
    assert_eq!(haiku.capabilities.reasoning, CapabilitySupport::Unsupported);
    assert_eq!(haiku.capabilities.image_input, CapabilitySupport::Supported);
    assert_eq!(
        haiku.capabilities.document_input,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        haiku.capabilities.structured_output,
        CapabilitySupport::Supported
    );
    // `context_management.supported: false` is explicit negative evidence even
    // though the strategy key itself is absent.
    assert_eq!(
        haiku.capabilities.native_compaction,
        CapabilitySupport::Unsupported
    );

    let legacy = find(&models, "claude-legacy-unknown");
    assert_eq!(legacy.context_window, None);
    assert_eq!(legacy.max_output_tokens, None);
    assert_eq!(
        legacy.capabilities.prompt_cache,
        CapabilitySupport::Supported
    );
    assert_eq!(legacy.capabilities.reasoning, CapabilitySupport::Unknown);

    let expected_headers: Vec<(String, String)> = vec![
        ("accept".to_owned(), "application/json".to_owned()),
        ("x-api-key".to_owned(), TEST_SECRET.to_owned()),
        ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
    ];
    assert_eq!(
        *requests.lock().unwrap(),
        [
            (FIRST_PAGE_URL.to_owned(), expected_headers.clone()),
            (SECOND_PAGE_URL.to_owned(), expected_headers),
        ]
    );
}

#[tokio::test]
async fn a_malformed_row_or_cursor_rejects_the_whole_generation() {
    let cases: Vec<(&str, Vec<HttpResponse>)> = vec![
        (
            "blank id",
            vec![response(
                200,
                page(
                    vec![
                        opus_row(),
                        serde_json::json!({
                            "id": "", "type": "model", "display_name": "Blank",
                            "created_at": "2026-01-01T00:00:00Z",
                            "max_input_tokens": 1000, "max_tokens": 100, "capabilities": null
                        }),
                    ],
                    false,
                ),
            )],
        ),
        (
            "wrong object type",
            vec![response(
                200,
                page(
                    vec![
                        opus_row(),
                        serde_json::json!({
                            "id": "claude-not-a-model", "type": "deployment",
                            "display_name": "Not A Model",
                            "created_at": "2026-01-01T00:00:00Z",
                            "max_input_tokens": 1000, "max_tokens": 100, "capabilities": null
                        }),
                    ],
                    false,
                ),
            )],
        ),
        (
            "control character in display name",
            vec![response(
                200,
                page(
                    vec![
                        opus_row(),
                        serde_json::json!({
                            "id": "claude-control", "type": "model",
                            "display_name": "Claude\u{7}Bell",
                            "created_at": "2026-01-01T00:00:00Z",
                            "max_input_tokens": 1000, "max_tokens": 100, "capabilities": null
                        }),
                    ],
                    false,
                ),
            )],
        ),
        (
            "non-RFC3339 release instant",
            vec![response(
                200,
                page(
                    vec![
                        opus_row(),
                        serde_json::json!({
                            "id": "claude-bad-date", "type": "model",
                            "display_name": "Bad Date", "created_at": "yesterday",
                            "max_input_tokens": 1000, "max_tokens": 100, "capabilities": null
                        }),
                    ],
                    false,
                ),
            )],
        ),
        (
            "zero context window",
            vec![response(
                200,
                page(
                    vec![
                        opus_row(),
                        serde_json::json!({
                            "id": "claude-zero", "type": "model", "display_name": "Zero",
                            "created_at": "2026-01-01T00:00:00Z",
                            "max_input_tokens": 0, "max_tokens": 100, "capabilities": null
                        }),
                    ],
                    false,
                ),
            )],
        ),
        (
            "duplicate id across pages",
            vec![
                response(200, page(vec![opus_row()], true)),
                response(200, page(vec![opus_row()], false)),
            ],
        ),
        (
            "empty generation",
            vec![response(200, page(Vec::new(), false))],
        ),
        (
            "provider default model absent",
            vec![response(200, page(vec![haiku_row()], false))],
        ),
        (
            "cursor disagrees with the last row",
            vec![
                response(
                    200,
                    serde_json::json!({
                        "data": [opus_row()],
                        "first_id": ANTHROPIC_CLAUDE_OPUS_5,
                        "last_id": "claude-somewhere-else",
                        "has_more": true
                    }),
                ),
                response(200, page(vec![haiku_row()], false)),
            ],
        ),
        (
            "another page promised without a cursor",
            vec![response(
                200,
                serde_json::json!({
                    "data": [opus_row()],
                    "first_id": ANTHROPIC_CLAUDE_OPUS_5,
                    "last_id": null,
                    "has_more": true
                }),
            )],
        ),
        (
            "response is not JSON",
            vec![HttpResponse {
                headers: BTreeMap::new(),
                status: 200,
                content_type: Some("text/html".to_owned()),
                body: b"<html>must-not-leak</html>".to_vec(),
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
async fn unbounded_pagination_is_refused_after_the_page_budget() {
    // A server that always promises another page must terminate as a bounded
    // invalid response instead of looping forever.
    let responses = (0..64)
        .map(|index| {
            response(
                200,
                serde_json::json!({
                    "data": [{
                        "id": format!("claude-page-{index}"),
                        "type": "model",
                        "display_name": format!("Claude Page {index}"),
                        "created_at": "2026-01-01T00:00:00Z",
                        "max_input_tokens": 1000,
                        "max_tokens": 100,
                        "capabilities": null
                    }],
                    "first_id": format!("claude-page-{index}"),
                    "last_id": format!("claude-page-{index}"),
                    "has_more": true
                }),
            )
        })
        .collect();
    let (_context, catalog, requests) = make_catalog(Some(TEST_SECRET), responses);
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    let sent = requests.lock().unwrap().len();
    assert!(
        sent > 1 && sent <= 32,
        "page budget must bound {sent} requests"
    );
}

#[tokio::test]
async fn status_and_transport_failures_classify_without_exposing_bodies() {
    for (status, kind) in [
        (401, CatalogFailureKind::Unauthorized),
        (403, CatalogFailureKind::Unauthorized),
        (429, CatalogFailureKind::Unavailable),
        (500, CatalogFailureKind::Unavailable),
        (529, CatalogFailureKind::Unavailable),
        (404, CatalogFailureKind::InvalidResponse),
    ] {
        let (_context, catalog, _) = make_catalog(
            Some(TEST_SECRET),
            vec![response(
                status,
                serde_json::json!({
                    "type": "error",
                    "error": {"type": "authentication_error", "message": "must-not-leak"}
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
async fn a_missing_credential_is_unauthorized_before_any_request() {
    let (_context, catalog, requests) = make_catalog(None, paged_fixture());
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unauthorized);
    assert!(!error.message().contains(TEST_SECRET));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_never_masquerades_as_a_generation() {
    let (_context, catalog, requests) = make_catalog(Some(TEST_SECRET), paged_fixture());
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
async fn catalog_plugin_registers_and_disposes_the_anthropic_source() {
    let (http, _requests) = http(paged_fixture());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(http)),
        Box::new(CredentialsPlugin),
        anthropic_catalog_plugin(AnthropicCatalogConfig::official(query())),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            "anthropic",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        view.snapshot
            .models
            .iter()
            .any(|model| model.id == ANTHROPIC_CLAUDE_OPUS_5)
    );
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "catalog-anthropic"
                && row.kind == heycode_core::ContributionKind::ModelCatalog
                && row.name == "anthropic")
    );

    context.shutdown();
    assert!(matches!(
        models
            .refresh(
                "anthropic",
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await,
        Err(CatalogError::UnknownCatalog { .. })
    ));
}

#[tokio::test]
async fn live_official_catalog_publishes_the_default_model_when_enabled() {
    // No Anthropic credential exists on the build host, so this stays skipped
    // until one is supplied. It is the only path that proves the live headers.
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let Ok(secret) = std::env::var("ANTHROPIC_API_KEY") else {
        return;
    };
    let (context, credentials) = credentials(Some(&secret));
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let catalog =
        AnthropicCatalog::new(HttpService::new(Arc::new(transport)), credentials, query()).unwrap();
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    let opus = find(&models, ANTHROPIC_CLAUDE_OPUS_5);
    assert!(opus.context_window.is_some_and(|window| window > 0));
    assert_eq!(opus.capabilities.tools, CapabilitySupport::Unknown);
    drop(context);
}

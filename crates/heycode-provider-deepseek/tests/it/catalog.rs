//! Current DeepSeek V4 discovery, lifecycle and error contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpSseRequest, HttpTransport,
    SseEventStream,
};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogRefreshMode, CatalogRegistry, ModelCatalog,
    ModelLifecycleStatus, ModelSelectionError,
};
use heycode_provider_deepseek::{
    DEEPSEEK_LEGACY_RETIREMENT_MS, DEEPSEEK_V4_FLASH, DEEPSEEK_V4_PRO, DeepSeekCatalog,
};
use tokio_util::sync::CancellationToken;

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

struct BufferedTransport {
    responses: Mutex<Vec<HttpResponse>>,
}

impl HttpTransport for BufferedTransport {
    fn send(
        &self,
        _request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        let response = self.responses.lock().unwrap().remove(0);
        Box::pin(async move { Ok(response) })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct RotatingSecretProvider {
    id: CredentialProviderId,
    secret: Arc<Mutex<String>>,
}

impl CredentialProvider for RotatingSecretProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(CredentialProviderState::configured(
            CredentialSource::Environment,
            false,
        ))
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(Some(CredentialSecret::new(
            self.secret.lock().unwrap().clone(),
        )))
    }
}

struct RecordingTransport {
    responses: Mutex<Vec<HttpResponse>>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
}

impl HttpTransport for RecordingTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        let authorization = request
            .headers()
            .iter()
            .find(|header| header.name() == "authorization")
            .map(|header| header.value().to_owned())
            .unwrap();
        self.requests
            .lock()
            .unwrap()
            .push((request.url().to_owned(), authorization));
        let response = self.responses.lock().unwrap().remove(0);
        Box::pin(async move { Ok(response) })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("DEEPSEEK_API_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn services(
    secret: Option<&str>,
    responses: Vec<HttpResponse>,
) -> (
    heycode_core::Context,
    Arc<CredentialsService>,
    heycode_http::HttpService,
) {
    let context = heycode_core::Context::new();
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
    let http = heycode_http::HttpService::new(Arc::new(BufferedTransport {
        responses: Mutex::new(responses),
    }));
    (context, credentials, http)
}

fn response(status: u16, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        headers: std::collections::BTreeMap::new(),
        status,
        content_type: Some("application/json".to_owned()),
        body: body.to_string().into_bytes(),
    }
}

fn current_fixture() -> HttpResponse {
    response(
        200,
        serde_json::json!({
            "object":"list",
            "data":[
                {"id":"deepseek-v4-flash","object":"model","owned_by":"deepseek"},
                {"id":"deepseek-v4-pro","object":"model","owned_by":"deepseek"}
            ]
        }),
    )
}

#[tokio::test]
async fn live_shape_normalizes_v4_limits_capabilities_and_legacy_tombstones() {
    let (_context, credentials, http) = services(Some("test-key"), vec![current_fixture()]);
    let catalog = DeepSeekCatalog::new(http, credentials, query()).unwrap();
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 4);

    for id in [DEEPSEEK_V4_FLASH, DEEPSEEK_V4_PRO] {
        let model = models.iter().find(|model| model.id == id).unwrap();
        assert_eq!(model.context_window, Some(1_048_576));
        assert_eq!(model.max_output_tokens, Some(384 * 1024));
        assert_eq!(model.lifecycle.status, ModelLifecycleStatus::Preview);
        assert_eq!(model.capabilities.tools, CapabilitySupport::Supported);
        assert_eq!(model.capabilities.reasoning, CapabilitySupport::Supported);
        assert_eq!(
            model.capabilities.prompt_cache,
            CapabilitySupport::Supported
        );
    }
    for id in ["deepseek-chat", "deepseek-reasoner"] {
        let model = models.iter().find(|model| model.id == id).unwrap();
        assert_eq!(model.lifecycle.status, ModelLifecycleStatus::Retired);
        assert_eq!(
            model.lifecycle.retirement_at_ms,
            Some(DEEPSEEK_LEGACY_RETIREMENT_MS)
        );
        assert_eq!(model.lifecycle.replacement_ids, [DEEPSEEK_V4_FLASH]);
    }
}

#[tokio::test]
async fn retired_legacy_selection_fails_with_v4_alternatives_and_flash_selects() {
    assert_eq!(
        heycode_llm::DeepSeekProvider::DEFAULT_MODEL,
        DEEPSEEK_V4_FLASH
    );
    let (context, credentials, http) = services(Some("test-key"), vec![current_fixture()]);
    let source: Arc<dyn ModelCatalog> =
        Arc::new(DeepSeekCatalog::new(http, credentials, query()).unwrap());
    let registry = CatalogRegistry::new(Duration::from_secs(300));
    registry.register(&context, source).unwrap();
    registry
        .refresh(
            "deepseek",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        registry.resolve_model("deepseek", "deepseek-chat", 1_800_000_000_000),
        Err(ModelSelectionError::RetiredModel { alternatives, .. })
            if alternatives.first().map(String::as_str) == Some(DEEPSEEK_V4_FLASH)
    ));
    assert_eq!(
        registry
            .resolve_model("deepseek", DEEPSEEK_V4_FLASH, 1_800_000_000_000)
            .unwrap()
            .descriptor
            .id,
        DEEPSEEK_V4_FLASH
    );
}

#[tokio::test]
async fn auth_and_server_statuses_are_classified_without_response_bodies() {
    for (status, kind) in [
        (401, CatalogFailureKind::Unauthorized),
        (403, CatalogFailureKind::Unauthorized),
        (429, CatalogFailureKind::Unavailable),
        (500, CatalogFailureKind::Unavailable),
    ] {
        let (_context, credentials, http) = services(
            Some("test-key"),
            vec![response(
                status,
                serde_json::json!({"secret":"must-not-leak"}),
            )],
        );
        let error = DeepSeekCatalog::new(http, credentials, query())
            .unwrap()
            .fetch(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(!error.message().contains("must-not-leak"));
    }

    let (_context, credentials, http) = services(None, Vec::new());
    let error = DeepSeekCatalog::new(http, credentials, query())
        .unwrap()
        .fetch(CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unauthorized);
}

#[tokio::test]
async fn malformed_catalog_fails_without_partial_rows() {
    let (_context, credentials, http) = services(
        Some("test-key"),
        vec![response(
            200,
            serde_json::json!({
                "object":"list",
                "data":[
                    {"id":"deepseek-v4-flash","object":"model","owned_by":"deepseek"},
                    {"id":"","object":"model","owned_by":"deepseek"}
                ]
            }),
        )],
    );
    let error = DeepSeekCatalog::new(http, credentials, query())
        .unwrap()
        .fetch(CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn each_refresh_resolves_the_current_credential_and_custom_base_url() {
    let context = heycode_core::Context::new();
    let secret = Arc::new(Mutex::new("first-key".to_owned()));
    let credentials = Arc::new(CredentialsService::new());
    credentials
        .register(
            &context,
            Arc::new(RotatingSecretProvider {
                id: CredentialProviderId::new("rotating-secret").unwrap(),
                secret: secret.clone(),
            }),
        )
        .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = heycode_http::HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(vec![current_fixture(), current_fixture()]),
        requests: requests.clone(),
    }));
    let catalog =
        DeepSeekCatalog::with_base_url(http, credentials, query(), "https://proxy.example/v1/")
            .unwrap();

    catalog.fetch(CancellationToken::new()).await.unwrap();
    *secret.lock().unwrap() = "second-key".to_owned();
    catalog.fetch(CancellationToken::new()).await.unwrap();

    assert_eq!(
        *requests.lock().unwrap(),
        [
            (
                "https://proxy.example/v1/models".to_owned(),
                "Bearer first-key".to_owned(),
            ),
            (
                "https://proxy.example/v1/models".to_owned(),
                "Bearer second-key".to_owned(),
            ),
        ]
    );
}

#[tokio::test]
async fn pre_cancelled_refresh_stops_before_credentials_or_http() {
    let (_context, credentials, http) = services(None, Vec::new());
    let catalog = DeepSeekCatalog::new(http, credentials, query()).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = catalog.fetch(cancellation).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Cancelled);
}

//! Production-composition evidence for the explicit OTLP provider.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use heycode_core::{Context, CoreResult, Plugin};
use heycode_credentials::{
    CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialQuery,
    CredentialSecret, CredentialSource,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream, TransportError,
};
use heycode_telemetry::{EgressKind, SERVICE_TELEMETRY, TelemetryEvent, TelemetryEventName};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct CapturedRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct RecordingHttp {
    requests: Mutex<Vec<CapturedRequest>>,
    responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
}

impl RecordingHttp {
    fn new(responses: impl IntoIterator<Item = Result<HttpResponse, TransportError>>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Default for RecordingHttp {
    fn default() -> Self {
        Self::new([])
    }
}

impl HttpTransport for RecordingHttp {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        let captured = CapturedRequest {
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().unwrap_or_default().to_vec(),
        };
        self.requests.lock().unwrap().push(captured);
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Ok(HttpResponse {
                    status: 200,
                    content_type: Some("application/json".to_owned()),
                    headers: Default::default(),
                    body: b"{}".to_vec(),
                })
            });
        Box::pin(async move { response })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct TestHttpPlugin(Arc<dyn HttpTransport>);

impl Plugin for TestHttpPlugin {
    fn name(&self) -> &'static str {
        "telemetry-otlp-test-http"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[heycode_http::SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> CoreResult<()> {
        context.provide(
            heycode_http::SERVICE_HTTP,
            self.name(),
            HttpService::new(self.0.clone()),
        )
    }
}

struct RotatingCredential {
    id: CredentialProviderId,
    secret: Mutex<String>,
}

impl RotatingCredential {
    fn new(secret: &str) -> Arc<Self> {
        Arc::new(Self {
            id: CredentialProviderId::new("telemetry-test-credentials").unwrap(),
            secret: Mutex::new(secret.to_owned()),
        })
    }

    fn rotate(&self, secret: &str) {
        *self.secret.lock().unwrap() = secret.to_owned();
    }
}

impl CredentialProvider for RotatingCredential {
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

struct TestCredentialPlugin(Arc<dyn CredentialProvider>);

impl Plugin for TestCredentialPlugin {
    fn name(&self) -> &'static str {
        "telemetry-otlp-test-credentials"
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        &[heycode_credentials::SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> CoreResult<()> {
        let service = context
            .get::<heycode_credentials::CredentialsService>(
                heycode_credentials::SERVICE_CREDENTIALS,
            )
            .ok_or_else(|| heycode_core::CoreError::other("credentials missing"))?;
        service
            .register(context, self.0.clone())
            .map_err(|_| heycode_core::CoreError::other("credential registration failed"))
    }
}

fn compose_product(
    documents: heycode_settings::SettingsDocuments,
    http: Arc<dyn HttpTransport>,
    credential: Option<Arc<dyn CredentialProvider>>,
) -> heycode_core::Context {
    let mut plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_settings::settings_plugin(documents),
        heycode_credentials::credentials_plugin(),
    ];
    if let Some(credential) = credential {
        plugins.push(Box::new(TestCredentialPlugin(credential)));
    }
    plugins.push(Box::new(TestHttpPlugin(http)));
    plugins.push(heycode_telemetry_otlp::telemetry_otlp_http_plugin());
    heycode_core::compose(&plugins).unwrap()
}

fn user_documents(section: serde_json::Value) -> heycode_settings::SettingsDocuments {
    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_user(
            heycode_telemetry_otlp::settings_namespace().unwrap(),
            section,
        )
        .unwrap();
    documents
}

fn success() -> HttpResponse {
    HttpResponse {
        status: 200,
        content_type: Some("application/json".to_owned()),
        headers: Default::default(),
        body: b"{}".to_vec(),
    }
}

#[test]
fn explicit_product_plugin_composes_and_exports_otlp_http_json() {
    let http = Arc::new(RecordingHttp::default());
    let mut context = compose_product(Default::default(), http.clone(), None);
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(SERVICE_TELEMETRY)
        .expect("the explicit product plugin must publish telemetry");
    assert_eq!(telemetry.egress(), EgressKind::Exporting);

    telemetry.record(&TelemetryEvent::new(TelemetryEventName::SessionStarted, 1));
    telemetry.flush();

    let requests = http.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "http://localhost:4318/v1/metrics");
    assert!(
        requests[0]
            .headers
            .contains(&("content-type".to_owned(), "application/json".to_owned()))
    );
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name == "user-agent" && value.contains("OTel-OTLP-Exporter-Rust"))
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap()["resourceMetrics"]
            .is_array()
    );

    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .get(&heycode_telemetry_otlp::settings_namespace().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.applies(),
        heycode_settings::SettingsApplies::Restart
    );
    assert_eq!(
        snapshot.wire_projection().unwrap().resolved()["endpoint"],
        heycode_telemetry_otlp::DEFAULT_METRICS_ENDPOINT
    );
    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == heycode_telemetry_otlp::PLUGIN_TELEMETRY_OTLP_HTTP
            && row.kind == heycode_core::ContributionKind::SettingsNamespace
            && row.name == "telemetry-otlp"
    }));

    drop(telemetry);
    context.shutdown();
}

#[test]
fn credential_reference_is_resolved_per_batch_and_never_enters_settings_or_payload() {
    const FIRST: &str = "sk-ant-api03-FIRST000000000000000";
    const SECOND: &str = "sk-ant-api03-SECOND00000000000000";
    let documents = user_documents(json!({
        "auth":{
            "header":"authorization",
            "credential_reference":"telemetry/collector",
            "credential_kind":"api-key",
            "scheme":"bearer"
        },
        "max_attempts":1
    }));
    let credential = RotatingCredential::new(FIRST);
    let http = Arc::new(RecordingHttp::default());
    let mut context = compose_product(
        documents,
        http.clone(),
        Some(credential.clone() as Arc<dyn CredentialProvider>),
    );
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(SERVICE_TELEMETRY)
        .unwrap();

    telemetry.record(&TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1));
    telemetry.flush();
    credential.rotate(SECOND);
    telemetry.record(&TelemetryEvent::new(TelemetryEventName::ToolInvoked, 2));
    telemetry.flush();

    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .headers
            .contains(&("authorization".to_owned(), format!("Bearer {FIRST}")))
    );
    assert!(
        requests[1]
            .headers
            .contains(&("authorization".to_owned(), format!("Bearer {SECOND}")))
    );
    for request in &requests {
        let body = String::from_utf8(request.body.clone()).unwrap();
        assert!(!body.contains(FIRST) && !body.contains(SECOND), "{body}");
    }
    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .get(&heycode_telemetry_otlp::settings_namespace().unwrap())
        .unwrap()
        .unwrap();
    let rendered = format!("{snapshot:?} {:?}", snapshot.wire_projection());
    assert!(!rendered.contains(FIRST) && !rendered.contains(SECOND));
    assert!(rendered.contains("telemetry/collector"));

    drop(telemetry);
    context.shutdown();
}

#[test]
fn missing_credential_refuses_before_http_without_fallback() {
    let documents = user_documents(json!({
        "auth":{
            "header":"authorization",
            "credential_reference":"telemetry/missing",
            "credential_kind":"api-key",
            "scheme":"raw"
        },
        "max_attempts":1
    }));
    let http = Arc::new(RecordingHttp::default());
    let mut context = compose_product(documents, http.clone(), None);
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(SERVICE_TELEMETRY)
        .unwrap();
    telemetry.record(&TelemetryEvent::new(TelemetryEventName::RequestFailed, 1));
    telemetry.flush();
    assert!(http.requests().is_empty());
    drop(telemetry);
    context.shutdown();
}

#[test]
fn retryable_otlp_status_retries_but_other_server_failures_do_not() {
    let mut retry_headers = std::collections::BTreeMap::new();
    retry_headers.insert("retry-after".to_owned(), "0".to_owned());
    let retrying = Arc::new(RecordingHttp::new([
        Ok(HttpResponse {
            status: 503,
            content_type: Some("application/json".to_owned()),
            headers: retry_headers,
            body: br#"{"message":"must-not-leak"}"#.to_vec(),
        }),
        Ok(success()),
    ]));
    let documents = user_documents(json!({
        "max_attempts":3,
        "initial_backoff_ms":1,
        "max_backoff_ms":2
    }));
    let mut context = compose_product(documents, retrying.clone(), None);
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(SERVICE_TELEMETRY)
        .unwrap();
    telemetry.record(&TelemetryEvent::new(TelemetryEventName::RequestFailed, 1));
    telemetry.flush();
    assert_eq!(retrying.requests().len(), 2);
    drop(telemetry);
    context.shutdown();

    let not_retrying = Arc::new(RecordingHttp::new([Ok(HttpResponse {
        status: 500,
        content_type: Some("application/json".to_owned()),
        headers: Default::default(),
        body: b"opaque server failure".to_vec(),
    })]));
    let documents = user_documents(json!({
        "max_attempts":3,
        "initial_backoff_ms":1,
        "max_backoff_ms":2
    }));
    let mut context = compose_product(documents, not_retrying.clone(), None);
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(SERVICE_TELEMETRY)
        .unwrap();
    telemetry.record(&TelemetryEvent::new(TelemetryEventName::RequestFailed, 1));
    telemetry.flush();
    assert_eq!(not_retrying.requests().len(), 1);
    drop(telemetry);
    context.shutdown();
}

struct PendingHttp {
    calls: AtomicU64,
    tokens: Mutex<Vec<CancellationToken>>,
}

impl PendingHttp {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicU64::new(0),
            tokens: Mutex::new(Vec::new()),
        })
    }
}

impl HttpTransport for PendingHttp {
    fn send(
        &self,
        _request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.tokens.lock().unwrap().push(cancellation);
        Box::pin(futures::future::pending())
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

#[test]
fn request_timeout_is_bounded_and_cancels_the_http_operation() {
    let documents = user_documents(json!({"timeout_ms":10,"max_attempts":1}));
    let http = PendingHttp::new();
    let mut context = compose_product(documents, http.clone(), None);
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(SERVICE_TELEMETRY)
        .unwrap();
    telemetry.record(&TelemetryEvent::new(TelemetryEventName::RequestFailed, 1));
    telemetry.flush();
    assert_eq!(http.calls.load(Ordering::Relaxed), 1);
    assert!(http.tokens.lock().unwrap()[0].is_cancelled());
    drop(telemetry);
    context.shutdown();
}

#[test]
fn explicit_otlp_and_local_off_cannot_layer() {
    let http = Arc::new(RecordingHttp::default());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_settings::settings_plugin(Default::default()),
        heycode_credentials::credentials_plugin(),
        Box::new(TestHttpPlugin(http)),
        heycode_telemetry::telemetry_plugin(),
        heycode_telemetry_otlp::telemetry_otlp_http_plugin(),
    ];
    match heycode_core::compose(&plugins) {
        Err(heycode_core::CoreError::DuplicateService { key, .. }) => {
            assert_eq!(key, SERVICE_TELEMETRY.as_str())
        }
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("local-off and explicit OTLP must be mutually exclusive"),
    }
}

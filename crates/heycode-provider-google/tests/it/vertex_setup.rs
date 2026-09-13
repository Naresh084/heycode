//! Vertex Gemini setup readiness without generating model content.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_authorization_gcp::GcpAuthService;
use heycode_authorization_gcp::testing::MapGcpEnvironment;
use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference};
use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpResponse, HttpService, HttpSseRequest,
    HttpTransport, SseEventStream,
};
use heycode_llm::{CatalogError, CatalogFailureKind, CatalogRegistry, ModelCatalog};
use heycode_provider_google::{
    GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE, GOOGLE_GEMINI_3_7_FLASH, GOOGLE_VERTEX_PROVIDER,
    VertexGeminiCatalog, vertex_google_connection_profile,
};
use tokio_util::sync::CancellationToken;

use super::support::{TEST_SECRET, credentials};

const ADC_PATH: &str = "/fixture/application_default_credentials.json";
const PROJECT: &str = "vertex-fixture";
const LOCATION: &str = "us-central1";
const EXPECTED_URL: &str = "https://us-central1-aiplatform.googleapis.com/v1beta1/projects/vertex-fixture/locations/us-central1/publishers/google/models/gemini-3.7-flash:fetchPublisherModelConfig";

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedRequest {
    method: HttpMethod,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

struct RecordingTransport {
    responses: Mutex<Vec<Result<HttpResponse, heycode_http::TransportError>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl HttpTransport for RecordingTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.requests.lock().unwrap().push(RecordedRequest {
            method: request.method(),
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().map(<[u8]>::to_vec),
        });
        let result = self.responses.lock().unwrap().remove(0);
        Box::pin(async move { result })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn oauth_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE).unwrap(),
        CredentialKind::new("oauth-token").unwrap(),
    )
}

fn configured_environment() -> MapGcpEnvironment {
    MapGcpEnvironment::new()
        .with_var("GOOGLE_APPLICATION_CREDENTIALS", ADC_PATH)
        .with_var("NO_GCE_CHECK", "true")
        .with_file(
            ADC_PATH,
            br#"{"type":"authorized_user","client_id":"fixture","client_secret":"secret","refresh_token":"token"}"#,
        )
}

fn absent_environment() -> MapGcpEnvironment {
    MapGcpEnvironment::new().with_var("HOME", "/home/fixture")
}

fn parameters(project: &str, location: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("location".to_owned(), location.to_owned()),
        ("project".to_owned(), project.to_owned()),
    ])
}

fn make_catalog(
    environment: MapGcpEnvironment,
    secret: Option<&str>,
    responses: Vec<Result<HttpResponse, heycode_http::TransportError>>,
) -> (
    heycode_core::Context,
    VertexGeminiCatalog,
    Arc<Mutex<Vec<RecordedRequest>>>,
) {
    let (context, credentials) = credentials(secret);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(responses),
        requests: requests.clone(),
    }));
    let gcp = Arc::new(GcpAuthService::new(Arc::new(environment), http.clone()));
    let catalog = VertexGeminiCatalog::oauth_token(http, credentials, gcp, oauth_query()).unwrap();
    (context, catalog, requests)
}

fn response(status: u16, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: body.to_string().into_bytes(),
    }
}

fn metadata_not_found() -> Result<HttpResponse, heycode_http::TransportError> {
    Ok(HttpResponse {
        status: 404,
        content_type: Some("text/plain".to_owned()),
        headers: BTreeMap::from([("metadata-flavor".to_owned(), "Google".to_owned())]),
        body: Vec::new(),
    })
}

#[test]
fn vertex_connection_metadata_owns_exact_coordinates_and_external_prerequisites() {
    let profile = vertex_google_connection_profile();
    assert_eq!(profile.registry_name, GOOGLE_VERTEX_PROVIDER);
    assert_eq!(profile.family, heycode_llm::ConnectionFamily::Cloud);
    assert_eq!(
        profile.default_model.as_deref(),
        Some(GOOGLE_GEMINI_3_7_FLASH)
    );
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE)
    );
    assert_eq!(
        profile
            .parameters
            .iter()
            .map(|field| field.id.as_str())
            .collect::<Vec<_>>(),
        ["project", "location"]
    );
    let help = profile.help.unwrap();
    for required in [
        "Application Default Credentials",
        "billing",
        "Vertex AI API",
        "Vertex AI User",
        "cloud-platform OAuth token",
        "does not mint",
    ] {
        assert!(help.contains(required), "missing prerequisite: {required}");
    }
}

#[tokio::test]
async fn successful_probe_is_one_bodyless_authenticated_get_and_returns_the_maintained_row() {
    let (_context, catalog, requests) = make_catalog(
        configured_environment(),
        Some(TEST_SECRET),
        vec![Ok(response(
            200,
            serde_json::json!({"publisherModel": "ready"}),
        ))],
    );

    let models = catalog
        .fetch_parameters(&parameters(PROJECT, LOCATION), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, GOOGLE_GEMINI_3_7_FLASH);
    assert_eq!(
        *requests.lock().unwrap(),
        [RecordedRequest {
            method: HttpMethod::Get,
            url: EXPECTED_URL.to_owned(),
            headers: vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("authorization".to_owned(), format!("Bearer {TEST_SECRET}")),
            ],
            body: None,
        }]
    );
}

#[tokio::test]
async fn global_location_uses_the_unprefixed_vertex_host() {
    let (_context, catalog, requests) = make_catalog(
        configured_environment(),
        Some(TEST_SECRET),
        vec![Ok(response(200, serde_json::json!({"ready": true})))],
    );
    catalog
        .fetch_parameters(&parameters(PROJECT, "global"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        requests.lock().unwrap()[0].url,
        format!(
            "https://aiplatform.googleapis.com/v1beta1/projects/{PROJECT}/locations/global/publishers/google/models/{GOOGLE_GEMINI_3_7_FLASH}:fetchPublisherModelConfig"
        )
    );
}

#[tokio::test]
async fn malformed_coordinates_fail_before_adc_credential_or_http_work() {
    for draft in [
        BTreeMap::new(),
        parameters("INVALID PROJECT", LOCATION),
        parameters(PROJECT, "us-central1-a"),
        BTreeMap::from([
            ("location".to_owned(), LOCATION.to_owned()),
            ("project".to_owned(), PROJECT.to_owned()),
            ("extra".to_owned(), "value".to_owned()),
        ]),
    ] {
        let (_context, catalog, requests) = make_catalog(absent_environment(), None, Vec::new());
        let failure = catalog
            .fetch_parameters(&draft, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
        assert!(requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn adc_and_operation_token_are_independent_required_prerequisites() {
    let (_context, no_adc, requests) = make_catalog(
        absent_environment(),
        Some(TEST_SECRET),
        vec![
            metadata_not_found(),
            metadata_not_found(),
            metadata_not_found(),
        ],
    );
    let no_adc = no_adc
        .fetch_parameters(&parameters(PROJECT, LOCATION), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(no_adc.kind(), CatalogFailureKind::Unauthorized);
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.url != EXPECTED_URL)
    );

    let unknown_adc = absent_environment().with_var("NO_GCE_CHECK", "true");
    let (_context, unknown_adc, requests) = make_catalog(
        unknown_adc,
        Some(TEST_SECRET),
        vec![Ok(response(200, serde_json::json!({"ready": true})))],
    );
    let unknown_adc = unknown_adc
        .fetch_parameters(&parameters(PROJECT, LOCATION), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(unknown_adc.kind(), CatalogFailureKind::Unavailable);
    assert!(requests.lock().unwrap().is_empty());

    let (_context, no_token, requests) = make_catalog(configured_environment(), None, Vec::new());
    let no_token = no_token
        .fetch_parameters(&parameters(PROJECT, LOCATION), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(no_token.kind(), CatalogFailureKind::Unauthorized);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn readiness_status_shape_network_and_cancellation_are_classified_without_bodies() {
    for (result, expected) in [
        (
            Ok(response(
                403,
                serde_json::json!({"secret": "do not render"}),
            )),
            CatalogFailureKind::Unauthorized,
        ),
        (
            Ok(response(
                503,
                serde_json::json!({"secret": "do not render"}),
            )),
            CatalogFailureKind::Unavailable,
        ),
        (
            Ok(response(200, serde_json::json!([]))),
            CatalogFailureKind::InvalidResponse,
        ),
        (
            Ok(HttpResponse {
                status: 200,
                content_type: Some("text/plain".to_owned()),
                headers: BTreeMap::new(),
                body: b"{}".to_vec(),
            }),
            CatalogFailureKind::InvalidResponse,
        ),
        (
            Err(heycode_http::TransportError::Timeout),
            CatalogFailureKind::Network,
        ),
    ] {
        let (_context, catalog, _) =
            make_catalog(configured_environment(), Some(TEST_SECRET), vec![result]);
        let failure = catalog
            .fetch_parameters(&parameters(PROJECT, LOCATION), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.kind(), expected);
        assert!(!failure.message().contains("do not render"));
        assert!(!failure.message().contains(TEST_SECRET));
    }

    let (_context, catalog, requests) =
        make_catalog(configured_environment(), Some(TEST_SECRET), Vec::new());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let failure = catalog
        .fetch_parameters(&parameters(PROJECT, LOCATION), cancelled)
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::Cancelled);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ordinary_fetch_is_credential_blind_and_parameter_probe_never_populates_registry_cache() {
    let (context, catalog, requests) = make_catalog(
        configured_environment(),
        Some(TEST_SECRET),
        vec![Ok(response(200, serde_json::json!({"ready": true})))],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert!(requests.lock().unwrap().is_empty());

    let registry = CatalogRegistry::new(std::time::Duration::from_secs(300));
    registry.register(&context, Arc::new(catalog)).unwrap();
    let draft = registry
        .probe_parameters(
            GOOGLE_VERTEX_PROVIDER,
            &parameters(PROJECT, LOCATION),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(draft.models.len(), 1);
    assert_eq!(requests.lock().unwrap().len(), 1);
    let error = registry.cached(GOOGLE_VERTEX_PROVIDER).unwrap_err();
    assert!(matches!(error, CatalogError::NoCachedCatalog { .. }));
    assert!(
        !registry
            .supports_parameter_credentials(GOOGLE_VERTEX_PROVIDER)
            .unwrap()
    );
}

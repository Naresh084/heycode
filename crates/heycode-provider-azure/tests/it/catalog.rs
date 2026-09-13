use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_core::Context;
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
};
use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpResponse, HttpService, HttpSseRequest,
    HttpTransport, SseEventStream,
};
use heycode_llm::{CapabilitySupport, CatalogFailureKind, ModelCatalog};
use heycode_provider_azure::{
    AZURE_OPENAI_API_KEY_REFERENCE, AZURE_OPENAI_PROVIDER, AzureOpenAiCatalog,
    AzureOpenAiCatalogConfig, azure_openai_connection_profile,
};
use tokio_util::sync::CancellationToken;

const SECRET: &str = "fixture-not-a-real-azure-key";

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

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(AZURE_OPENAI_API_KEY_REFERENCE).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn parameters(resource: &str, deployment: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("deployment".to_owned(), deployment.to_owned()),
        ("resource".to_owned(), resource.to_owned()),
    ])
}

fn response(status: u16, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: body.to_string().into_bytes(),
    }
}

fn model(id: &str) -> serde_json::Value {
    serde_json::json!({"id":id,"object":"model","created":1,"owned_by":"azure"})
}

fn make_catalog(
    secret: Option<&str>,
    responses: Vec<Result<HttpResponse, heycode_http::TransportError>>,
) -> (
    Context,
    AzureOpenAiCatalog,
    Arc<Mutex<Vec<RecordedRequest>>>,
) {
    let context = Context::new();
    let credentials = Arc::new(CredentialsService::new());
    credentials
        .register(
            &context,
            Arc::new(SecretProvider {
                id: CredentialProviderId::new("azure-test-secret").unwrap(),
                secret: secret.map(str::to_owned),
            }),
        )
        .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(responses),
        requests: requests.clone(),
    }));
    let catalog = AzureOpenAiCatalog::new(
        http,
        credentials,
        AzureOpenAiCatalogConfig::api_key(query()),
    )
    .unwrap();
    (context, catalog, requests)
}

#[test]
fn profile_requires_resource_deployment_and_an_api_key_reference() {
    let profile = azure_openai_connection_profile();
    assert_eq!(profile.registry_name, AZURE_OPENAI_PROVIDER);
    assert_eq!(profile.family, heycode_llm::ConnectionFamily::Cloud);
    assert_eq!(profile.default_model, None);
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(AZURE_OPENAI_API_KEY_REFERENCE)
    );
    assert_eq!(
        profile
            .parameters
            .iter()
            .map(|parameter| parameter.id.as_str())
            .collect::<Vec<_>>(),
        ["resource", "deployment"]
    );
    let help = profile.help.unwrap();
    for required in [
        "Responses API",
        "/openai/v1",
        "deployment name",
        "Azure API key",
    ] {
        assert!(help.contains(required), "{required}");
    }
    assert!(!help.contains("not yet"));
}

#[tokio::test]
async fn exact_deployment_probe_is_one_bodyless_api_key_get_and_keeps_capabilities_unknown() {
    let (_context, catalog, requests) =
        make_catalog(Some(SECRET), vec![Ok(response(200, model("prod-gpt")))]);
    let rows = catalog
        .fetch_parameters(
            &parameters("team-agent", "prod-gpt"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "prod-gpt");
    for support in [
        rows[0].capabilities.tools,
        rows[0].capabilities.reasoning,
        rows[0].capabilities.structured_output,
        rows[0].capabilities.native_web,
    ] {
        assert_eq!(support, CapabilitySupport::Unknown);
    }
    assert_eq!(
        *requests.lock().unwrap(),
        [RecordedRequest {
            method: HttpMethod::Get,
            url: "https://team-agent.openai.azure.com/openai/v1/models/prod-gpt".to_owned(),
            headers: vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("api-key".to_owned(), SECRET.to_owned()),
            ],
            body: None,
        }]
    );
}

#[tokio::test]
async fn masked_draft_key_can_probe_without_being_in_the_credential_registry() {
    let (_context, catalog, requests) = make_catalog(None, vec![Ok(response(200, model("prod")))]);
    assert!(catalog.supports_parameter_credentials());
    let secret = CredentialSecret::new(SECRET);
    let rows = catalog
        .fetch_parameters_with_credential(
            &parameters("team-agent", "prod"),
            Some(&secret),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(rows[0].id, "prod");
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn inactive_refresh_is_credential_blind_and_malformed_coordinates_fail_before_io() {
    let (_context, catalog, requests) = make_catalog(None, Vec::new());
    assert!(
        catalog
            .fetch(CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    for draft in [
        BTreeMap::new(),
        parameters("bad/name", "prod"),
        parameters("team-agent", "bad/name"),
        BTreeMap::from([
            ("resource".to_owned(), "team-agent".to_owned()),
            ("deployment".to_owned(), "prod".to_owned()),
            ("extra".to_owned(), "value".to_owned()),
        ]),
    ] {
        let failure = catalog
            .fetch_parameters(&draft, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
    }
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_key_status_shape_network_and_cancellation_are_classified_without_bodies() {
    let (_context, missing, requests) = make_catalog(None, Vec::new());
    let failure = missing
        .fetch_parameters(&parameters("team-agent", "prod"), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::Unauthorized);
    assert!(requests.lock().unwrap().is_empty());

    let cases = [
        (
            Ok(response(403, serde_json::json!({"secret":"hidden"}))),
            CatalogFailureKind::Unauthorized,
        ),
        (
            Ok(response(404, serde_json::json!({"secret":"hidden"}))),
            CatalogFailureKind::Unavailable,
        ),
        (
            Ok(response(503, serde_json::json!({"secret":"hidden"}))),
            CatalogFailureKind::Unavailable,
        ),
        (
            Ok(response(
                200,
                serde_json::json!({"id":"other","object":"model","created":1,"owned_by":"azure","secret":"hidden"}),
            )),
            CatalogFailureKind::InvalidResponse,
        ),
        (
            Err(heycode_http::TransportError::Timeout),
            CatalogFailureKind::Network,
        ),
    ];
    for (result, expected) in cases {
        let (_context, catalog, _requests) = make_catalog(Some(SECRET), vec![result]);
        let failure = catalog
            .fetch_parameters(&parameters("team-agent", "prod"), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.kind(), expected);
        assert!(!failure.message().contains("hidden"));
        assert!(!failure.message().contains(SECRET));
    }

    let (_context, cancelled, requests) = make_catalog(Some(SECRET), Vec::new());
    let token = CancellationToken::new();
    token.cancel();
    let failure = cancelled
        .fetch_parameters(&parameters("team-agent", "prod"), token)
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::Cancelled);
    assert!(requests.lock().unwrap().is_empty());
}

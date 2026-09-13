use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_credentials::CredentialSecret;
use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpResponse, HttpService, HttpSseRequest,
    HttpTransport, SseEventStream,
};
use heycode_llm::{CapabilitySupport, CatalogFailureKind, ModelCatalog};
use heycode_provider_openai_compatible::{
    CUSTOM_OPENAI_PROVIDER, CustomOpenAiCatalog, CustomOpenAiCatalogConfig, CustomOpenAiEndpoint,
    custom_openai_connection_profile,
};
use tokio_util::sync::CancellationToken;

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

fn response(status: u16, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status,
        content_type: Some("application/json; charset=utf-8".to_owned()),
        headers: BTreeMap::new(),
        body: body.to_string().into_bytes(),
    }
}

fn catalog(
    responses: Vec<Result<HttpResponse, heycode_http::TransportError>>,
) -> (CustomOpenAiCatalog, Arc<Mutex<Vec<RecordedRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(responses),
        requests: requests.clone(),
    }));
    (
        CustomOpenAiCatalog::new(http, None, CustomOpenAiCatalogConfig::discovery()).unwrap(),
        requests,
    )
}

#[test]
fn profile_is_explicit_chat_only_local_without_a_fabricated_default() {
    let profile = custom_openai_connection_profile();
    assert_eq!(profile.registry_name, CUSTOM_OPENAI_PROVIDER);
    assert_eq!(profile.family, heycode_llm::ConnectionFamily::Local);
    assert_eq!(profile.default_endpoint, None);
    assert_eq!(profile.default_model, None);
    assert_eq!(profile.credential_reference, None);
    assert!(profile.allows_explicit_model());
    assert_eq!(
        profile.descriptor.protocols,
        [heycode_core::ProviderProtocol::OpenAiChatCompletions]
    );
    let help = profile.help.unwrap();
    for expected in ["/v1", "GET /models", "POST /chat/completions", "optional"] {
        assert!(help.contains(expected), "{expected}");
    }
}

#[tokio::test]
async fn canonical_list_is_one_bodyless_get_and_keeps_every_capability_unknown() {
    let (catalog, requests) = catalog(vec![Ok(response(
        200,
        serde_json::json!({
            "object":"list",
            "data":[
                {"id":"local/a","object":"model","created":1,"owned_by":"fixture"},
                {"id":"local-b"}
            ]
        }),
    ))]);
    let rows = catalog
        .fetch_endpoint("http://localhost:8000/v1/", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["local/a", "local-b"]
    );
    for row in rows {
        for support in [
            row.capabilities.tools,
            row.capabilities.reasoning,
            row.capabilities.image_input,
            row.capabilities.structured_output,
            row.capabilities.native_web,
        ] {
            assert_eq!(support, CapabilitySupport::Unknown);
        }
    }
    assert_eq!(
        *requests.lock().unwrap(),
        [RecordedRequest {
            method: HttpMethod::Get,
            url: "http://localhost:8000/v1/models".to_owned(),
            headers: vec![("accept".to_owned(), "application/json".to_owned())],
            body: None,
        }]
    );
}

#[tokio::test]
async fn masked_optional_key_is_sent_only_as_bearer_authorization() {
    let (catalog, requests) = catalog(vec![Ok(response(
        200,
        serde_json::json!({"object":"list","data":[{"id":"secured"}]}),
    ))]);
    assert!(catalog.supports_endpoint_credentials());
    let secret = CredentialSecret::new("fixture-not-a-real-key");
    let rows = catalog
        .fetch_endpoint_with_credential(
            "https://server.example/v1",
            Some(&secret),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(rows[0].id, "secured");
    let headers = &requests.lock().unwrap()[0].headers;
    assert_eq!(
        headers,
        &[
            ("accept".to_owned(), "application/json".to_owned()),
            (
                "authorization".to_owned(),
                "Bearer fixture-not-a-real-key".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn invalid_urls_and_cancelled_drafts_fail_before_io() {
    let (catalog, requests) = catalog(Vec::new());
    for value in [
        "",
        " localhost:8000/v1",
        "file:///tmp/server",
        "http://user:pass@localhost/v1",
        "http://localhost/v1?key=value",
        "http://localhost/v1#fragment",
    ] {
        let error = catalog
            .fetch_endpoint(value, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse, "{value}");
    }
    let token = CancellationToken::new();
    token.cancel();
    let error = catalog
        .fetch_endpoint("http://localhost:8000/v1", token)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Cancelled);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn status_and_shape_failures_are_classified_without_response_content() {
    let cases = [
        (
            Ok(response(401, serde_json::json!({"secret":"hidden"}))),
            CatalogFailureKind::Unauthorized,
        ),
        (
            Ok(response(404, serde_json::json!({"secret":"hidden"}))),
            CatalogFailureKind::Unavailable,
        ),
        (
            Ok(response(
                200,
                serde_json::json!({"object":"list","data":[{"id":"same"},{"id":"same"}],"secret":"hidden"}),
            )),
            CatalogFailureKind::InvalidResponse,
        ),
        (
            Err(heycode_http::TransportError::Timeout),
            CatalogFailureKind::Network,
        ),
    ];
    for (result, expected) in cases {
        let (catalog, _requests) = catalog(vec![result]);
        let failure = catalog
            .fetch_endpoint("http://localhost:8000/v1", CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.kind(), expected);
        assert!(!failure.message().contains("hidden"));
    }
}

#[tokio::test]
async fn setup_safe_inactive_refresh_performs_no_io() {
    let (catalog, requests) = catalog(Vec::new());
    assert!(
        catalog
            .fetch(CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(requests.lock().unwrap().is_empty());
    assert_eq!(
        CustomOpenAiEndpoint::new("http://localhost:8000/v1/")
            .unwrap()
            .as_str(),
        "http://localhost:8000/v1"
    );
}

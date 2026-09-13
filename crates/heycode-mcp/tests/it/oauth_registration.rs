//! MCP05 client discovery and registration. The fixtures mirror the current
//! MCP 2026-07-28 registration priority: pre-registered, CIMD, then DCR.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpTransport, SseEventStream,
};
use heycode_mcp::oauth::{
    OAuthAuthorizationServerMetadata, OAuthClientMetadataDocument, OAuthClientRegistration,
    OAuthClientRegistrationMechanism, OAuthDiscovery, OAuthDynamicClientMetadata, OAuthFault,
    OAuthRedirectUri, resolve_client_registration,
};
use tokio_util::sync::CancellationToken;

struct ScriptedHttp {
    responses: Mutex<VecDeque<HttpResponse>>,
    seen: Mutex<Vec<SeenRequest>>,
}

#[derive(Debug, Clone)]
struct SeenRequest {
    url: String,
    body: Option<serde_json::Value>,
}

impl ScriptedHttp {
    fn new(responses: Vec<HttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn json(status: u16, body: serde_json::Value) -> HttpResponse {
        HttpResponse {
            status,
            content_type: Some("application/json".to_owned()),
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn seen(&self) -> Vec<SeenRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl HttpTransport for ScriptedHttp {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        let body = request
            .body()
            .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok());
        self.seen.lock().unwrap().push(SeenRequest {
            url: request.url().to_owned(),
            body,
        });
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted response");
        Box::pin(async move { Ok(response) })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn redirect() -> OAuthRedirectUri {
    OAuthRedirectUri::new("http://127.0.0.1:7777/callback").unwrap()
}

fn server_metadata(
    cimd: bool,
    registration_endpoint: Option<&str>,
) -> OAuthAuthorizationServerMetadata {
    let mut document = serde_json::json!({
        "issuer": "https://auth.example.test/tenant",
        "authorization_endpoint": "https://auth.example.test/tenant/authorize",
        "token_endpoint": "https://auth.example.test/tenant/token",
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "client_id_metadata_document_supported": cimd
    });
    if let Some(endpoint) = registration_endpoint {
        document["registration_endpoint"] = serde_json::json!(endpoint);
    }
    OAuthAuthorizationServerMetadata::parse("https://auth.example.test/tenant", &document).unwrap()
}

fn dcr_metadata() -> OAuthDynamicClientMetadata {
    OAuthDynamicClientMetadata::native("heycode", redirect()).unwrap()
}

fn cimd() -> OAuthClientMetadataDocument {
    OAuthClientMetadataDocument::parse(
        "https://client.example.test/oauth/client.json",
        &serde_json::json!({
            "client_id": "https://client.example.test/oauth/client.json",
            "client_name": "heycode",
            "redirect_uris": ["http://127.0.0.1:7777/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }),
    )
    .unwrap()
}

#[tokio::test]
async fn protected_resource_and_authorization_metadata_follow_the_normative_discovery_order() {
    let transport = ScriptedHttp::new(vec![
        ScriptedHttp::json(404, serde_json::json!({})),
        ScriptedHttp::json(
            200,
            serde_json::json!({
                "resource": "https://mcp.example.test/server/mcp",
                "authorization_servers": ["https://auth.example.test/tenant"]
            }),
        ),
        ScriptedHttp::json(404, serde_json::json!({})),
        ScriptedHttp::json(
            200,
            serde_json::json!({
                "issuer": "https://auth.example.test/tenant",
                "authorization_endpoint": "https://auth.example.test/tenant/authorize",
                "token_endpoint": "https://auth.example.test/tenant/token",
                "registration_endpoint": "https://auth.example.test/tenant/register",
                "code_challenge_methods_supported": ["S256"],
                "client_id_metadata_document_supported": true
            }),
        ),
    ]);

    let discovery = OAuthDiscovery::discover(
        &transport,
        "https://mcp.example.test/server/mcp",
        None,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(discovery.resource(), "https://mcp.example.test/server/mcp");
    assert_eq!(
        discovery.authorization_server().issuer(),
        "https://auth.example.test/tenant"
    );
    let urls = transport
        .seen()
        .into_iter()
        .map(|request| request.url)
        .collect::<Vec<_>>();
    assert_eq!(
        urls,
        [
            "https://mcp.example.test/.well-known/oauth-protected-resource/server/mcp",
            "https://mcp.example.test/.well-known/oauth-protected-resource",
            "https://auth.example.test/.well-known/oauth-authorization-server/tenant",
            "https://auth.example.test/.well-known/openid-configuration/tenant",
        ]
    );
}

#[tokio::test]
async fn a_challenge_metadata_url_is_used_without_falling_back_to_an_attacker_substitute() {
    let transport = ScriptedHttp::new(vec![ScriptedHttp::json(
        200,
        serde_json::json!({
            "resource": "https://mcp.example.test/mcp",
            "authorization_servers": [
                "https://auth-a.example.test",
                "https://auth-b.example.test"
            ]
        }),
    )]);

    let error = OAuthDiscovery::discover(
        &transport,
        "https://mcp.example.test/mcp",
        Some("https://mcp.example.test/custom/resource-metadata"),
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();

    assert_eq!(error, OAuthFault::AuthorizationServerSelectionRequired);
    assert_eq!(
        transport.seen()[0].url,
        "https://mcp.example.test/custom/resource-metadata"
    );
    assert_eq!(transport.seen().len(), 1);
}

#[test]
fn metadata_issuer_substitution_and_missing_pkce_are_refused() {
    let expected = "https://auth.example.test";
    for (document, fault) in [
        (
            serde_json::json!({
                "issuer": "https://attacker.example.test",
                "authorization_endpoint": "https://auth.example.test/authorize",
                "token_endpoint": "https://auth.example.test/token",
                "code_challenge_methods_supported": ["S256"]
            }),
            OAuthFault::IssuerMismatch,
        ),
        (
            serde_json::json!({
                "issuer": expected,
                "authorization_endpoint": "https://auth.example.test/authorize",
                "token_endpoint": "https://auth.example.test/token"
            }),
            OAuthFault::ChallengeMethodUnsupported,
        ),
    ] {
        assert_eq!(
            OAuthAuthorizationServerMetadata::parse(expected, &document).unwrap_err(),
            fault
        );
    }
}

#[tokio::test]
async fn pre_registered_client_information_has_priority_and_is_issuer_bound() {
    let metadata = server_metadata(true, Some("https://auth.example.test/tenant/register"));
    let pre_registered = OAuthClientRegistration::pre_registered_public(
        "https://auth.example.test/tenant",
        "pre-registered-client",
        redirect(),
    )
    .unwrap();
    let transport = ScriptedHttp::new(Vec::new());

    let selected = resolve_client_registration(
        &transport,
        &metadata,
        Some(pre_registered),
        Some(cimd()),
        &dcr_metadata(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        selected.mechanism(),
        OAuthClientRegistrationMechanism::PreRegistered
    );
    assert_eq!(selected.client_id(), "pre-registered-client");
    assert_eq!(transport.seen().len(), 0, "selection performs no DCR");

    let wrong_issuer = OAuthClientRegistration::pre_registered_public(
        "https://other.example.test",
        "substituted-client",
        redirect(),
    )
    .unwrap();
    let error = resolve_client_registration(
        &transport,
        &metadata,
        Some(wrong_issuer),
        Some(cimd()),
        &dcr_metadata(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error, OAuthFault::ClientIssuerMismatch);
}

#[tokio::test]
async fn cimd_has_priority_over_deprecated_dcr_and_binds_the_exact_callback() {
    let metadata = server_metadata(true, Some("https://auth.example.test/tenant/register"));
    let transport = ScriptedHttp::new(Vec::new());

    let selected = resolve_client_registration(
        &transport,
        &metadata,
        None,
        Some(cimd()),
        &dcr_metadata(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        selected.mechanism(),
        OAuthClientRegistrationMechanism::ClientIdMetadataDocument
    );
    assert_eq!(
        selected.client_id(),
        "https://client.example.test/oauth/client.json"
    );
    assert!(selected.issuer_binding().is_none(), "CIMD is portable");
    assert_eq!(transport.seen().len(), 0);

    let wrong_callback = OAuthDynamicClientMetadata::native(
        "heycode",
        OAuthRedirectUri::new("http://127.0.0.1:8888/callback").unwrap(),
    )
    .unwrap();
    let error = resolve_client_registration(
        &transport,
        &metadata,
        None,
        Some(cimd()),
        &wrong_callback,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error, OAuthFault::RedirectMismatch);
}

#[tokio::test]
async fn dcr_is_the_bounded_fallback_and_declares_a_native_public_client() {
    let metadata = server_metadata(false, Some("https://auth.example.test/tenant/register"));
    let transport = ScriptedHttp::new(vec![ScriptedHttp::json(
        201,
        serde_json::json!({
            "client_id": "issued-client",
            "redirect_uris": ["http://127.0.0.1:7777/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "application_type": "native"
        }),
    )]);

    let selected = resolve_client_registration(
        &transport,
        &metadata,
        None,
        None,
        &dcr_metadata(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        selected.mechanism(),
        OAuthClientRegistrationMechanism::Dynamic
    );
    assert_eq!(selected.client_id(), "issued-client");
    assert_eq!(
        selected.issuer_binding(),
        Some("https://auth.example.test/tenant")
    );
    let seen = transport.seen();
    assert_eq!(seen[0].url, "https://auth.example.test/tenant/register");
    let body = seen[0].body.as_ref().unwrap();
    assert_eq!(body["application_type"], "native");
    assert_eq!(body["token_endpoint_auth_method"], "none");
    assert_eq!(
        body["grant_types"],
        serde_json::json!(["authorization_code", "refresh_token"])
    );
    assert!(body.get("client_id").is_none());
    assert!(body.get("client_secret").is_none());
}

#[tokio::test]
async fn a_dcr_error_or_secret_substitution_is_body_free_and_unpublished() {
    for response in [
        ScriptedHttp::json(
            400,
            serde_json::json!({
                "error": "invalid_redirect_uri",
                "error_description": "sk-server-body-canary"
            }),
        ),
        ScriptedHttp::json(
            201,
            serde_json::json!({
                "client_id": "issued-client",
                "client_secret": "sk-injected-client-secret",
                "token_endpoint_auth_method": "none",
                "redirect_uris": ["http://127.0.0.1:7777/callback"],
                "application_type": "native"
            }),
        ),
    ] {
        let metadata = server_metadata(false, Some("https://auth.example.test/tenant/register"));
        let transport = ScriptedHttp::new(vec![response]);
        let error = resolve_client_registration(
            &transport,
            &metadata,
            None,
            None,
            &dcr_metadata(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            OAuthFault::ClientRegistrationRejected | OAuthFault::MalformedClientRegistration
        ));
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("sk-server-body-canary"), "{rendered}");
        assert!(
            !rendered.contains("sk-injected-client-secret"),
            "{rendered}"
        );
    }
}

#[test]
fn cimd_and_callback_uris_are_exact_and_transport_safe() {
    for uri in [
        "http://example.test/callback",
        "ftp://127.0.0.1/callback",
        "https://user:secret@example.test/callback",
        "https://example.test/callback#fragment",
    ] {
        assert_eq!(
            OAuthRedirectUri::new(uri).unwrap_err(),
            OAuthFault::InvalidRedirectUri,
            "accepted {uri}"
        );
    }

    for (url, client_id) in [
        (
            "http://client.example.test/client.json",
            "http://client.example.test/client.json",
        ),
        ("https://client.example.test", "https://client.example.test"),
        (
            "https://client.example.test/client.json",
            "https://other.example.test/client.json",
        ),
    ] {
        let error = OAuthClientMetadataDocument::parse(
            url,
            &serde_json::json!({
                "client_id": client_id,
                "client_name": "heycode",
                "redirect_uris": ["http://127.0.0.1:7777/callback"],
                "grant_types": ["authorization_code"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none"
            }),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            OAuthFault::InvalidClientMetadata | OAuthFault::ClientIdMismatch
        ));
    }
}

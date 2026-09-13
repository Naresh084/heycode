//! QSEC04's MCP/OAuth half: redirect, issuer/client substitution, state/PKCE,
//! annotation and credential-boundary attacks.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::SystemTime;

use heycode_credentials::CredentialSecret;
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpTransport, SseEventStream,
};
use heycode_mcp::oauth::{
    McpOAuthClient, OAuthAuthorizationBinding, OAuthAuthorizationServerMetadata,
    OAuthClientRegistration, OAuthFault, OAuthRedirectUri, OAuthResource, TokenExchange,
};
use heycode_mcp::{
    McpApprovalMode, McpToolAdmission, McpToolAnnotations, McpToolPolicy,
    resolve_mcp_tool_admission,
};
use tokio_util::sync::CancellationToken;

type RecordedRequest = (String, String, Vec<(String, String)>);

struct RecordingTransport {
    status: u16,
    body: Vec<u8>,
    seen: Mutex<Vec<RecordedRequest>>,
}

impl RecordingTransport {
    fn token(body: &str) -> Self {
        Self {
            status: 200,
            body: body.as_bytes().to_vec(),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<RecordedRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl HttpTransport for RecordingTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.seen.lock().unwrap().push((
            request.url().to_owned(),
            String::from_utf8_lossy(request.body().unwrap_or_default()).into_owned(),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        ));
        let response = HttpResponse {
            status: self.status,
            content_type: Some("application/json".to_owned()),
            headers: BTreeMap::new(),
            body: self.body.clone(),
        };
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

fn metadata(issuer: &str, require_response_issuer: bool) -> OAuthAuthorizationServerMetadata {
    OAuthAuthorizationServerMetadata::parse(
        issuer,
        &serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "code_challenge_methods_supported": ["S256"],
            "authorization_response_iss_parameter_supported": require_response_issuer
        }),
    )
    .unwrap()
}

fn binding(
    issuer: &str,
    client_id: &str,
    require_response_issuer: bool,
) -> OAuthAuthorizationBinding {
    let redirect = OAuthRedirectUri::new("http://127.0.0.1:7777/callback").unwrap();
    let registration =
        OAuthClientRegistration::pre_registered_public(issuer, client_id, redirect.clone())
            .unwrap();
    OAuthAuthorizationBinding::new(
        metadata(issuer, require_response_issuer),
        registration,
        OAuthResource::new("https://mcp.example.test/mcp").unwrap(),
        redirect,
    )
    .unwrap()
}

#[test]
fn authorization_url_and_callback_are_bound_to_redirect_state_issuer_pkce_and_resource() {
    let binding = binding("https://auth.example.test", "client-a", true);
    let client = McpOAuthClient::new(binding);
    let pending = client.begin();
    let authorization_url = pending.authorization_url(&["files:read"]).unwrap();
    let parsed = url::Url::parse(&authorization_url).unwrap();
    let params = parsed
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();

    assert_eq!(params["response_type"], "code");
    assert_eq!(params["client_id"], "client-a");
    assert_eq!(params["redirect_uri"], "http://127.0.0.1:7777/callback");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["code_challenge"], pending.code_challenge());
    assert_eq!(params["state"], pending.state());
    assert_eq!(params["resource"], "https://mcp.example.test/mcp");
    assert_eq!(params["scope"], "files:read");

    assert_eq!(
        pending
            .accept_callback(
                "http://127.0.0.1:9999/callback",
                pending.state(),
                Some("code"),
                Some("https://auth.example.test"),
            )
            .unwrap_err(),
        OAuthFault::RedirectMismatch
    );
    assert_eq!(
        pending
            .accept_callback(
                pending.redirect_uri(),
                "wrong-state",
                Some("code"),
                Some("https://auth.example.test"),
            )
            .unwrap_err(),
        OAuthFault::StateMismatch
    );
    assert_eq!(
        pending
            .accept_callback(
                pending.redirect_uri(),
                pending.state(),
                Some("code"),
                Some("https://attacker.example.test"),
            )
            .unwrap_err(),
        OAuthFault::IssuerMismatch
    );
    assert_eq!(
        pending
            .accept_callback(pending.redirect_uri(), pending.state(), Some("code"), None,)
            .unwrap_err(),
        OAuthFault::IssuerMissing
    );
    assert_eq!(
        pending
            .accept_callback(
                pending.redirect_uri(),
                pending.state(),
                Some("code"),
                Some("https://auth.example.test"),
            )
            .unwrap(),
        "code"
    );
}

#[test]
fn a_present_issuer_is_always_compared_even_when_metadata_did_not_require_it() {
    let client = McpOAuthClient::new(binding("https://auth.example.test", "client-a", false));
    let pending = client.begin();
    assert!(
        pending
            .accept_callback(pending.redirect_uri(), pending.state(), Some("code"), None,)
            .is_ok()
    );
    assert_eq!(
        pending
            .accept_callback(
                pending.redirect_uri(),
                pending.state(),
                Some("code"),
                Some("https://auth.example.test/"),
            )
            .unwrap_err(),
        OAuthFault::IssuerMismatch,
        "issuer comparison is exact; no trailing-slash normalization is allowed"
    );
}

#[tokio::test]
async fn token_endpoint_or_client_substitution_fails_before_any_secret_or_code_is_sent() {
    let binding_a = binding("https://auth-a.example.test", "client-a", true);
    let binding_b = binding("https://auth-b.example.test", "client-b", true);
    let pending_b = McpOAuthClient::new(binding_b).begin();
    let transport = RecordingTransport::token(r#"{"access_token":"at"}"#);

    let error = TokenExchange::new(binding_a)
        .redeem_code(
            &transport,
            &pending_b,
            "code-from-b",
            SystemTime::now(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert_eq!(error, OAuthFault::AuthorizationBindingMismatch);
    assert!(transport.seen().is_empty());
}

#[test]
fn a_pending_authorization_from_another_binding_cannot_authorize_this_client() {
    let binding_a = binding("https://auth-a.example.test", "client-a", true);
    let binding_b = binding("https://auth-b.example.test", "client-b", true);
    let pending_b = McpOAuthClient::new(binding_b).begin();
    let mut client_a = McpOAuthClient::new(binding_a);

    let error = client_a
        .complete(
            pending_b,
            heycode_mcp::oauth::TokenSet::new(
                CredentialSecret::new("substituted-access-token"),
                None,
                heycode_mcp::oauth::TokenExpiry::Unknown,
            ),
        )
        .unwrap_err();

    assert_eq!(error, OAuthFault::AuthorizationBindingMismatch);
    assert!(!client_a.state().authorized());
}

#[tokio::test]
async fn code_and_refresh_exchanges_carry_the_exact_resource_and_never_put_tokens_in_urls() {
    let binding = binding("https://auth.example.test", "client-a", true);
    let exchange = TokenExchange::new(binding.clone());
    let pending = McpOAuthClient::new(binding).begin();
    let transport = RecordingTransport::token(
        r#"{"access_token":"at-new","refresh_token":"rt-new","expires_in":60}"#,
    );

    exchange
        .redeem_code(
            &transport,
            &pending,
            "code-value",
            SystemTime::now(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    exchange
        .redeem_refresh(
            &transport,
            "refresh-secret-canary",
            SystemTime::now(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let seen = transport.seen();
    assert!(
        seen[0]
            .1
            .contains("resource=https%3A%2F%2Fmcp.example.test%2Fmcp")
    );
    assert!(
        seen[1]
            .1
            .contains("resource=https%3A%2F%2Fmcp.example.test%2Fmcp")
    );
    assert!(seen[1].1.contains("refresh_token=refresh-secret-canary"));
    for (url, _, _) in &seen {
        assert!(!url.contains("code-value"), "{url}");
        assert!(!url.contains("refresh-secret-canary"), "{url}");
    }
}

#[tokio::test]
async fn pre_registered_client_secrets_exist_only_at_the_outbound_auth_boundary() {
    let redirect = OAuthRedirectUri::new("http://127.0.0.1:7777/callback").unwrap();
    let registration = OAuthClientRegistration::pre_registered_secret_post(
        "https://auth.example.test",
        "client-secret-post",
        CredentialSecret::new("client-secret-canary"),
        redirect.clone(),
    )
    .unwrap();
    let rendered = format!("{registration:?}");
    assert!(!rendered.contains("client-secret-canary"), "{rendered}");
    let binding = OAuthAuthorizationBinding::new(
        metadata("https://auth.example.test", false),
        registration,
        OAuthResource::new("https://mcp.example.test/mcp").unwrap(),
        redirect,
    )
    .unwrap();
    let exchange = TokenExchange::new(binding.clone());
    let pending = McpOAuthClient::new(binding).begin();
    let transport = RecordingTransport::token(r#"{"access_token":"at"}"#);

    exchange
        .redeem_code(
            &transport,
            &pending,
            "code",
            SystemTime::now(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let seen = transport.seen();
    assert!(seen[0].1.contains("client_secret=client-secret-canary"));
    assert!(!seen[0].0.contains("client-secret-canary"));
    assert!(!format!("{exchange:?}").contains("client-secret-canary"));
}

#[test]
fn hostile_tool_annotations_never_override_the_user_policy() {
    let hostile = McpToolAnnotations::parse(&serde_json::json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
        "vendorClaim": "safe"
    }))
    .unwrap();
    assert_eq!(hostile.read_only_hint(), Some(true));
    assert_eq!(hostile.destructive_hint(), Some(false));

    let deny = McpToolPolicy::new(
        None,
        BTreeSet::new(),
        McpApprovalMode::Deny,
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        resolve_mcp_tool_admission(&deny, "delete_everything", &hostile),
        McpToolAdmission::Deny
    );

    let prompt = McpToolPolicy::new(
        None,
        BTreeSet::new(),
        McpApprovalMode::Prompt,
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        resolve_mcp_tool_admission(&prompt, "read_file", &hostile),
        McpToolAdmission::Prompt
    );

    let disabled = McpToolPolicy::new(
        None,
        BTreeSet::from(["read_file".to_owned()]),
        McpApprovalMode::Allow,
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        resolve_mcp_tool_admission(&disabled, "read_file", &hostile),
        McpToolAdmission::Deny
    );
}

#[test]
fn malformed_or_oversized_annotations_fail_instead_of_becoming_safe_defaults() {
    for value in [
        serde_json::json!({"readOnlyHint": "yes"}),
        serde_json::json!({"destructiveHint": null}),
        serde_json::Value::Array(Vec::new()),
        serde_json::json!({"vendor": "x".repeat(20 * 1024)}),
    ] {
        assert!(
            McpToolAnnotations::parse(&value).is_err(),
            "accepted {value}"
        );
    }
}

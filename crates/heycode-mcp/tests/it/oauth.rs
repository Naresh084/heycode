//! MCP04: the four authorization states, and the invariants that keep a code
//! or a token from being usable by anyone but this client.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, SystemTime};

use base64::Engine as _;
use heycode_credentials::CredentialSecret;
use heycode_mcp::oauth::{
    CodeChallengeMethod, Freshness, McpOAuthClient, OAuthAuthorizationBinding,
    OAuthAuthorizationServerMetadata, OAuthClientRegistration, OAuthFault, OAuthRedirectUri,
    OAuthResource, PendingAuthorization, TokenExpiry, TokenSet,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};

const ISSUER: &str = "https://auth.example.test";

fn binding() -> OAuthAuthorizationBinding {
    let redirect = OAuthRedirectUri::new("http://127.0.0.1:7777/callback").unwrap();
    let metadata = OAuthAuthorizationServerMetadata::parse(
        ISSUER,
        &serde_json::json!({
            "issuer": ISSUER,
            "authorization_endpoint": "https://auth.example.test/authorize",
            "token_endpoint": "https://auth.example.test/token",
            "code_challenge_methods_supported": ["S256"]
        }),
    )
    .unwrap();
    let registration = OAuthClientRegistration::pre_registered_public(
        ISSUER,
        "heycode-public-client",
        redirect.clone(),
    )
    .unwrap();
    OAuthAuthorizationBinding::new(
        metadata,
        registration,
        OAuthResource::new("https://mcp.example.test/mcp").unwrap(),
        redirect,
    )
    .unwrap()
}

fn client() -> McpOAuthClient {
    McpOAuthClient::new(binding())
}

fn accept_callback(
    pending: &PendingAuthorization,
    state: &str,
    code: Option<&str>,
) -> Result<String, OAuthFault> {
    pending.accept_callback(pending.redirect_uri(), state, code, None)
}

fn token_response(
    access: &str,
    refresh: Option<&str>,
    expires_in: Option<u64>,
) -> serde_json::Value {
    let mut body = json!({"access_token": access, "token_type": "Bearer"});
    if let Some(refresh) = refresh {
        body["refresh_token"] = json!(refresh);
    }
    if let Some(expires_in) = expires_in {
        body["expires_in"] = json!(expires_in);
    }
    body
}

/// State 1 of 4: a fresh authorization ends holding tokens.
#[test]
fn authorize_moves_an_unauthenticated_client_to_holding_tokens() {
    let mut client = client();
    assert!(!client.state().authorized());

    let pending = client.begin();
    let code = accept_callback(&pending, pending.state(), Some("auth-code")).unwrap();
    assert_eq!(code, "auth-code");

    let tokens = TokenSet::from_response(
        &token_response("at-1", Some("rt-1"), Some(3600)),
        SystemTime::now(),
    )
    .unwrap();
    client.complete(pending, tokens).unwrap();

    assert!(client.state().authorized());
    assert_eq!(client.state().tokens().unwrap().expose_access(), "at-1");
}

/// State 2 of 4: refresh replaces the access token in place.
#[test]
fn refresh_replaces_the_access_token_and_keeps_the_session() {
    let mut client = client();
    let pending = client.begin();
    client
        .complete(
            pending,
            TokenSet::from_response(
                &token_response("at-1", Some("rt-1"), Some(1)),
                SystemTime::now(),
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(client.expose_refresh_token().unwrap(), "rt-1");
    client
        .refreshed(
            TokenSet::from_response(
                &token_response("at-2", Some("rt-2"), Some(3600)),
                SystemTime::now(),
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(client.state().tokens().unwrap().expose_access(), "at-2");
    assert_eq!(client.expose_refresh_token().unwrap(), "rt-2");
}

/// RFC 6749 §6 lets a refresh response omit a new refresh token, and the old
/// one stays valid. Dropping it would silently downgrade the session to
/// one-shot — the next refresh would demand a browser for no reason.
#[test]
fn a_refresh_without_a_new_refresh_token_keeps_the_existing_one() {
    let mut client = client();
    let pending = client.begin();
    client
        .complete(
            pending,
            TokenSet::from_response(
                &token_response("at-1", Some("rt-1"), Some(1)),
                SystemTime::now(),
            )
            .unwrap(),
        )
        .unwrap();

    client
        .refreshed(
            TokenSet::from_response(&token_response("at-2", None, Some(3600)), SystemTime::now())
                .unwrap(),
        )
        .unwrap();

    assert_eq!(client.state().tokens().unwrap().expose_access(), "at-2");
    assert_eq!(
        client.expose_refresh_token().unwrap(),
        "rt-1",
        "the surviving refresh token must be the original"
    );
}

/// State 3 of 4: logout is total. Nothing held afterwards can authorize.
#[test]
fn logout_discards_every_token_and_refresh_then_fails() {
    let mut client = client();
    let pending = client.begin();
    client
        .complete(
            pending,
            TokenSet::from_response(
                &token_response("at-1", Some("rt-1"), Some(3600)),
                SystemTime::now(),
            )
            .unwrap(),
        )
        .unwrap();

    client.logout();

    assert!(!client.state().authorized());
    assert!(client.state().tokens().is_none());
    assert_eq!(
        client.expose_refresh_token().unwrap_err(),
        OAuthFault::NotAuthorized
    );
    assert_eq!(
        client
            .refreshed(
                TokenSet::from_response(&token_response("at-2", None, Some(60)), SystemTime::now())
                    .unwrap()
            )
            .unwrap_err(),
        OAuthFault::NotAuthorized,
        "a refresh after logout must not resurrect the session"
    );
    client.logout(); // idempotent
    assert!(!client.state().authorized());
}

/// State 4 of 4: after logout, a full authorization works again and issues a
/// *different* verifier and state.
#[test]
fn reauthorize_after_logout_starts_a_genuinely_new_authorization() {
    let mut client = client();
    let first = client.begin();
    client
        .complete(
            first,
            TokenSet::from_response(&token_response("at-1", None, Some(3600)), SystemTime::now())
                .unwrap(),
        )
        .unwrap();
    let first_state = {
        let pending = client.begin();
        pending.state().to_owned()
    };
    client.logout();

    let second = client.begin();
    assert_ne!(
        second.state(),
        first_state.as_str(),
        "a reauthorization must not reuse the CSRF state"
    );

    client
        .complete(
            second,
            TokenSet::from_response(&token_response("at-9", None, Some(3600)), SystemTime::now())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(client.state().tokens().unwrap().expose_access(), "at-9");
}

/// Every authorization gets its own verifier. Reuse would let a code
/// intercepted from one authorization be redeemed against another.
#[test]
fn each_authorization_generates_a_distinct_verifier_and_state() {
    let client = client();
    let a = client.begin();
    let b = client.begin();
    assert_ne!(a.expose_verifier(), b.expose_verifier());
    assert_ne!(a.state(), b.state());
    assert_ne!(a.code_challenge(), b.code_challenge());
    // RFC 7636 §4.1: 43-128 characters.
    assert!((43..=128).contains(&a.expose_verifier().len()));
}

/// The challenge must actually be S256 of the verifier — a client that sends a
/// challenge unrelated to its verifier fails the exchange, and one that sends
/// the verifier itself has silently implemented `plain`.
#[test]
fn the_challenge_is_the_sha256_of_the_verifier() {
    let client = client();
    let pending = client.begin();
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(pending.expose_verifier().as_bytes()));
    assert_eq!(pending.code_challenge(), expected);
    assert_ne!(pending.code_challenge(), pending.expose_verifier());
    assert_eq!(pending.method(), CodeChallengeMethod::S256);
    assert_eq!(pending.method().as_str(), "S256");
}

/// `plain` gives away the only thing PKCE buys. A server offering it alone is
/// refused rather than accommodated — and a server advertising nothing is
/// refused too, because unknown is not supported.
#[test]
fn a_server_without_s256_is_refused_rather_than_downgraded() {
    for advertised in [vec!["plain"], vec![], vec!["S384", "plain"]] {
        assert_eq!(
            CodeChallengeMethod::negotiate(&advertised).unwrap_err(),
            OAuthFault::ChallengeMethodUnsupported,
            "accepted {advertised:?}"
        );
    }
    assert_eq!(
        CodeChallengeMethod::negotiate(&["plain", "S256"]).unwrap(),
        CodeChallengeMethod::S256
    );
}

/// A forged callback is rejected on `state` before the code is even read, so a
/// planted code never reaches the exchange.
#[test]
fn a_callback_with_the_wrong_state_is_refused_before_the_code_is_used() {
    let client = client();
    let pending = client.begin();

    assert_eq!(
        accept_callback(&pending, "not-the-state", Some("planted-code")).unwrap_err(),
        OAuthFault::StateMismatch
    );
    assert_eq!(
        accept_callback(&pending, "", Some("planted-code")).unwrap_err(),
        OAuthFault::StateMismatch
    );
    // Ordering matters: a callback that is wrong on BOTH counts must report the
    // state mismatch, proving the state was checked before the code was read.
    assert_eq!(
        accept_callback(&pending, "not-the-state", None).unwrap_err(),
        OAuthFault::StateMismatch
    );
    // Right state, no code: a different, honest failure.
    assert_eq!(
        accept_callback(&pending, pending.state(), None).unwrap_err(),
        OAuthFault::MissingCode
    );
    assert_eq!(
        accept_callback(&pending, pending.state(), Some("")).unwrap_err(),
        OAuthFault::MissingCode
    );
}

/// An absent `expires_in` is unknown — not "never expires", not "expired".
/// Same rule the usage stack keeps: unknown is not zero.
#[test]
fn an_absent_expires_in_is_unknown_not_immortal_and_not_expired() {
    let now = SystemTime::now();
    let unknown = TokenSet::from_response(&token_response("at", None, None), now).unwrap();
    assert_eq!(unknown.expiry(), TokenExpiry::Unknown);
    assert_eq!(unknown.freshness(now), Freshness::Unknown);
    assert_eq!(
        unknown.freshness(now + Duration::from_secs(86_400)),
        Freshness::Unknown,
        "an unknown lifetime never becomes a known one by waiting"
    );

    let stated = TokenSet::from_response(&token_response("at", None, Some(60)), now).unwrap();
    assert_eq!(stated.freshness(now), Freshness::Fresh);
    assert_eq!(
        stated.freshness(now + Duration::from_secs(61)),
        Freshness::Expired
    );
}

#[test]
fn a_token_response_without_an_access_token_is_malformed() {
    let now = SystemTime::now();
    for body in [
        json!({"token_type": "Bearer"}),
        json!({"access_token": ""}),
        json!({"access_token": 42}),
    ] {
        assert_eq!(
            TokenSet::from_response(&body, now).unwrap_err(),
            OAuthFault::MalformedTokenResponse,
            "accepted {body}"
        );
    }
}

/// A session with no refresh token must say so, rather than fail vaguely — the
/// caller's next step is a browser, and it needs to know that.
#[test]
fn a_session_without_a_refresh_token_reports_that_specifically() {
    let mut client = client();
    let pending = client.begin();
    client
        .complete(
            pending,
            TokenSet::from_response(&token_response("at-1", None, Some(3600)), SystemTime::now())
                .unwrap(),
        )
        .unwrap();

    assert!(!client.state().tokens().unwrap().refreshable());
    assert_eq!(
        client.expose_refresh_token().unwrap_err(),
        OAuthFault::NotRefreshable
    );
    assert_eq!(
        client
            .refreshed(
                TokenSet::from_response(&token_response("at-2", None, Some(60)), SystemTime::now())
                    .unwrap()
            )
            .unwrap_err(),
        OAuthFault::NotRefreshable
    );
}

/// Nothing secret may reach a formatted string. A verifier in a log is the same
/// disclosure as a leaked authorization code.
#[test]
fn no_secret_reaches_debug_output() {
    let mut client = client();
    let pending = client.begin();
    let verifier = pending.expose_verifier().to_owned();
    let state = pending.state().to_owned();

    let rendered = format!("{pending:?}");
    assert!(!rendered.contains(&verifier), "{rendered}");
    assert!(!rendered.contains(&state), "{rendered}");
    assert!(rendered.contains("[REDACTED]"));

    client
        .complete(
            pending,
            TokenSet::from_response(
                &token_response("access-canary", Some("refresh-canary"), Some(60)),
                SystemTime::now(),
            )
            .unwrap(),
        )
        .unwrap();
    let rendered = format!("{client:?}");
    assert!(!rendered.contains("access-canary"), "{rendered}");
    assert!(!rendered.contains("refresh-canary"), "{rendered}");
}

/// A fault is a closed class and must never carry attacker-influenced text; an
/// OAuth error response and a callback URL are both attacker-influenced.
#[test]
fn faults_carry_no_values() {
    for fault in [
        OAuthFault::ChallengeMethodUnsupported,
        OAuthFault::StateMismatch,
        OAuthFault::MissingCode,
        OAuthFault::Denied,
        OAuthFault::MalformedTokenResponse,
        OAuthFault::NotRefreshable,
        OAuthFault::NotAuthorized,
        OAuthFault::Unreachable,
    ] {
        let rendered = fault.to_string();
        assert!(!rendered.is_empty());
        assert_eq!(
            std::mem::size_of_val(&fault),
            1,
            "a fault must stay a bare tag"
        );
    }
}

/// The redirect URI recorded on the authorization is the one the exchange must
/// send; RFC 6749 requires them to match exactly.
#[test]
fn the_pending_authorization_carries_the_exact_redirect_uri() {
    let client = client();
    let pending = client.begin();
    assert_eq!(pending.redirect_uri(), "http://127.0.0.1:7777/callback");
}

/// `CredentialSecret` is the storage type, so an OAuth token inherits the same
/// zeroizing, non-serializable handling as every other credential.
#[test]
fn tokens_are_held_as_credential_secrets() {
    let secret = CredentialSecret::new("value-canary");
    assert!(!format!("{secret:?}").contains("value-canary"));
    let tokens = TokenSet::new(secret, None, TokenExpiry::Unknown);
    assert_eq!(tokens.expose_access(), "value-canary");
}

mod records {
    //! MCP04 credential records: tokens that survive a restart, and a logout
    //! that provably removes them.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    use heycode_core::Context;
    use heycode_credentials::{
        CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialQuery,
        CredentialSecret, CredentialSource, CredentialsService,
    };
    use heycode_mcp::oauth::{OAuthRecords, TokenExpiry, TokenSet};

    /// A writable in-memory store standing in for the keychain.
    struct MemoryStore {
        id: CredentialProviderId,
        entries: Mutex<HashMap<String, String>>,
    }

    impl MemoryStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                id: CredentialProviderId::new("memory").unwrap(),
                entries: Mutex::new(HashMap::new()),
            })
        }
        fn key(query: &CredentialQuery) -> String {
            query.reference.as_str().to_owned()
        }
        fn len(&self) -> usize {
            self.entries.lock().unwrap().len()
        }
    }

    impl CredentialProvider for MemoryStore {
        fn id(&self) -> &CredentialProviderId {
            &self.id
        }
        fn precedence(&self) -> u16 {
            10
        }
        fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
            let held = self
                .entries
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .contains_key(&Self::key(query));
            Ok(if held {
                CredentialProviderState::configured(CredentialSource::Keychain, true)
            } else {
                CredentialProviderState::unconfigured(true)
            })
        }
        fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
            Ok(self
                .entries
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .get(&Self::key(query))
                .map(CredentialSecret::new))
        }
        fn write(&self, query: &CredentialQuery, secret: &CredentialSecret) -> Result<(), String> {
            self.entries
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .insert(Self::key(query), secret.expose().to_owned());
            Ok(())
        }
        fn delete(&self, query: &CredentialQuery) -> Result<(), String> {
            self.entries
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .remove(&Self::key(query));
            Ok(())
        }
    }

    fn records() -> (Arc<MemoryStore>, OAuthRecords, Context) {
        let store = MemoryStore::new();
        let service = Arc::new(CredentialsService::new());
        let context = Context::default();
        service
            .register(&context, store.clone() as Arc<dyn CredentialProvider>)
            .unwrap();
        let records = OAuthRecords::new(service, "example-server", &super::binding()).unwrap();
        (store, records, context)
    }

    #[test]
    fn a_stored_token_set_survives_and_reloads_intact() {
        let (_store, records, _context) = records();
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        records
            .store(&TokenSet::new(
                CredentialSecret::new("at-1"),
                Some(CredentialSecret::new("rt-1")),
                TokenExpiry::At(at),
            ))
            .unwrap();

        let loaded = records.load().unwrap().expect("a stored record reloads");
        assert_eq!(loaded.expose_access(), "at-1");
        assert!(loaded.refreshable());
        assert_eq!(loaded.expiry(), TokenExpiry::At(at));
    }

    /// An unknown expiry must survive the round trip as unknown. Persisting it
    /// as zero would make every reloaded token look expired.
    #[test]
    fn an_unknown_expiry_round_trips_as_unknown() {
        let (_store, records, _context) = records();
        records
            .store(&TokenSet::new(
                CredentialSecret::new("at-1"),
                None,
                TokenExpiry::Unknown,
            ))
            .unwrap();
        let loaded = records.load().unwrap().unwrap();
        assert_eq!(loaded.expiry(), TokenExpiry::Unknown);
        assert!(!loaded.refreshable());
    }

    /// Logout is one delete, and after it the store holds nothing at all — no
    /// orphaned refresh token the user believes they revoked.
    #[test]
    fn clearing_removes_every_trace_and_is_idempotent() {
        let (store, records, _context) = records();
        records
            .store(&TokenSet::new(
                CredentialSecret::new("at-1"),
                Some(CredentialSecret::new("rt-1")),
                TokenExpiry::Unknown,
            ))
            .unwrap();
        assert_eq!(store.len(), 1);

        records.clear().unwrap();
        assert_eq!(store.len(), 0, "logout must leave nothing behind");
        assert!(records.load().unwrap().is_none());
        records.clear().unwrap(); // idempotent: logging out twice is not an error
        assert!(records.load().unwrap().is_none());
    }

    /// A corrupt or older-format record sends the user through a fresh
    /// authorization rather than wedging the server behind an error.
    #[test]
    fn an_unparsable_record_reads_as_absent_rather_than_failing() {
        let (store, records, context) = records();
        let service = Arc::new(CredentialsService::new());
        service
            .register(&context, store.clone() as Arc<dyn CredentialProvider>)
            .unwrap();
        let query = CredentialQuery::new(
            heycode_credentials::CredentialReference::new("mcp-oauth:example-server").unwrap(),
            heycode_credentials::CredentialKind::new("mcp-oauth-tokens").unwrap(),
        );
        for corrupt in ["not json at all", "{}", r#"{"access_token":""}"#] {
            store
                .write(&query, &CredentialSecret::new(corrupt))
                .unwrap();
            assert!(
                records.load().unwrap().is_none(),
                "accepted corrupt record {corrupt}"
            );
        }
    }

    /// Storing replaces rather than accumulating, so a refresh cannot leave the
    /// previous access token recoverable from the store.
    #[test]
    fn storing_again_replaces_the_previous_record() {
        let (store, records, _context) = records();
        records
            .store(&TokenSet::new(
                CredentialSecret::new("at-old"),
                None,
                TokenExpiry::Unknown,
            ))
            .unwrap();
        records
            .store(&TokenSet::new(
                CredentialSecret::new("at-new"),
                None,
                TokenExpiry::Unknown,
            ))
            .unwrap();

        assert_eq!(store.len(), 1);
        assert_eq!(records.load().unwrap().unwrap().expose_access(), "at-new");
    }

    #[test]
    fn tokens_are_not_reused_after_the_resource_moves_to_another_issuer() {
        let (store, records, context) = records();
        records
            .store(&TokenSet::new(
                CredentialSecret::new("at-old-issuer"),
                Some(CredentialSecret::new("rt-old-issuer")),
                TokenExpiry::Unknown,
            ))
            .unwrap();

        let service = Arc::new(CredentialsService::new());
        service
            .register(&context, store as Arc<dyn CredentialProvider>)
            .unwrap();
        let redirect =
            heycode_mcp::oauth::OAuthRedirectUri::new("http://127.0.0.1:7777/callback").unwrap();
        let metadata = heycode_mcp::oauth::OAuthAuthorizationServerMetadata::parse(
            "https://new-auth.example.test",
            &serde_json::json!({
                "issuer": "https://new-auth.example.test",
                "authorization_endpoint": "https://new-auth.example.test/authorize",
                "token_endpoint": "https://new-auth.example.test/token",
                "code_challenge_methods_supported": ["S256"]
            }),
        )
        .unwrap();
        let registration = heycode_mcp::oauth::OAuthClientRegistration::pre_registered_public(
            "https://new-auth.example.test",
            "new-client",
            redirect.clone(),
        )
        .unwrap();
        let binding = heycode_mcp::oauth::OAuthAuthorizationBinding::new(
            metadata,
            registration,
            heycode_mcp::oauth::OAuthResource::new("https://mcp.example.test/mcp").unwrap(),
            redirect,
        )
        .unwrap();
        let moved = OAuthRecords::new(service, "example-server", &binding).unwrap();

        assert!(
            moved.load().unwrap().is_none(),
            "the old issuer's access and refresh tokens must force reauthorization"
        );
    }

    #[test]
    fn the_record_reference_does_not_disclose_the_tokens() {
        let (_store, records, _context) = records();
        records
            .store(&TokenSet::new(
                CredentialSecret::new("secret-canary"),
                None,
                TokenExpiry::Unknown,
            ))
            .unwrap();
        let rendered = format!("{records:?}");
        assert!(!rendered.contains("secret-canary"), "{rendered}");
        assert!(rendered.contains("mcp-oauth:example-server"));
    }
}

mod exchange {
    //! MCP04 token endpoint: what actually goes on the wire, and what a
    //! failing server is allowed to make this client believe.

    use std::sync::Mutex;
    use std::time::SystemTime;

    use heycode_http::{
        BufferedResponseFuture, HttpRequest, HttpResponse, HttpTransport, SseEventStream,
        TransportError,
    };
    use heycode_mcp::oauth::{OAuthFault, TokenExchange};
    use tokio_util::sync::CancellationToken;

    /// Records the request and replies with a canned answer.
    struct Recording {
        status: u16,
        body: Vec<u8>,
        seen: Mutex<Option<(String, String)>>,
    }

    impl Recording {
        fn new(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.as_bytes().to_vec(),
                seen: Mutex::new(None),
            }
        }
        fn body_seen(&self) -> String {
            self.seen.lock().unwrap().clone().unwrap().1
        }
        fn url_seen(&self) -> String {
            self.seen.lock().unwrap().clone().unwrap().0
        }
    }

    impl HttpTransport for Recording {
        fn send(
            &self,
            request: HttpRequest,
            _cancellation: CancellationToken,
        ) -> BufferedResponseFuture {
            *self.seen.lock().unwrap() = Some((
                request.url().to_owned(),
                String::from_utf8_lossy(request.body().unwrap_or_default()).into_owned(),
            ));
            let response = HttpResponse {
                status: self.status,
                content_type: Some("application/json".to_owned()),
                headers: std::collections::BTreeMap::new(),
                body: self.body.clone(),
            };
            Box::pin(async move { Ok(response) })
        }
        fn sse(
            &self,
            _request: heycode_http::HttpSseRequest,
            _c: CancellationToken,
        ) -> SseEventStream {
            Box::pin(futures::stream::empty())
        }
    }

    struct Broken;
    impl HttpTransport for Broken {
        fn send(&self, _r: HttpRequest, _c: CancellationToken) -> BufferedResponseFuture {
            Box::pin(async {
                Err(TransportError::InvalidRequest {
                    field: "test",
                    message: "no".to_owned(),
                })
            })
        }
        fn sse(
            &self,
            _request: heycode_http::HttpSseRequest,
            _c: CancellationToken,
        ) -> SseEventStream {
            Box::pin(futures::stream::empty())
        }
    }

    fn exchange() -> TokenExchange {
        TokenExchange::new(super::binding())
    }

    /// The verifier must actually reach the wire — a client that omits it has
    /// implemented the ceremony of PKCE without the proof.
    #[tokio::test]
    async fn redeeming_a_code_sends_the_verifier_redirect_uri_and_grant_type() {
        let binding = super::binding();
        let client = heycode_mcp::oauth::McpOAuthClient::new(binding.clone());
        let pending = client.begin();
        let transport = Recording::new(200, r#"{"access_token":"at-1","expires_in":60}"#);

        let tokens = TokenExchange::new(binding)
            .redeem_code(
                &transport,
                &pending,
                "the-code",
                SystemTime::now(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(tokens.expose_access(), "at-1");

        let body = transport.body_seen();
        assert_eq!(transport.url_seen(), "https://auth.example.test/token");
        assert!(body.contains("grant_type=authorization_code"), "{body}");
        assert!(body.contains("code=the-code"), "{body}");
        assert!(body.contains("client_id=heycode-public-client"), "{body}");
        assert!(
            body.contains(&format!("code_verifier={}", pending.expose_verifier())),
            "the verifier must be sent verbatim: {body}"
        );
        assert!(
            body.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A7777%2Fcallback"),
            "the redirect URI must be form-encoded: {body}"
        );
    }

    /// The exchange must send the verifier, never the challenge — sending the
    /// challenge would authenticate nothing.
    #[tokio::test]
    async fn the_exchange_sends_the_verifier_and_never_the_challenge() {
        let binding = super::binding();
        let client = heycode_mcp::oauth::McpOAuthClient::new(binding.clone());
        let pending = client.begin();
        let transport = Recording::new(200, r#"{"access_token":"at-1"}"#);
        TokenExchange::new(binding)
            .redeem_code(
                &transport,
                &pending,
                "c",
                SystemTime::now(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let body = transport.body_seen();
        assert!(!body.contains(&pending.code_challenge()), "{body}");
    }

    #[tokio::test]
    async fn refreshing_sends_the_refresh_grant_and_the_token() {
        let transport = Recording::new(200, r#"{"access_token":"at-2","expires_in":60}"#);
        exchange()
            .redeem_refresh(
                &transport,
                "rt-1",
                SystemTime::now(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let body = transport.body_seen();
        assert!(body.contains("grant_type=refresh_token"), "{body}");
        assert!(body.contains("refresh_token=rt-1"), "{body}");
        assert!(!body.contains("code_verifier"), "{body}");
    }

    /// A denial is a class. The server's error body is attacker-influenced and
    /// routinely echoes the request, so none of it is carried.
    #[tokio::test]
    async fn a_rejected_exchange_is_a_denial_and_carries_no_server_text() {
        for status in [400_u16, 401, 403, 500] {
            let transport = Recording::new(
                status,
                r#"{"error":"invalid_grant","error_description":"code sk-leaked-canary"}"#,
            );
            let error = exchange()
                .redeem_refresh(
                    &transport,
                    "rt-1",
                    SystemTime::now(),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(error, OAuthFault::Denied);
            assert!(!error.to_string().contains("sk-leaked-canary"));
        }
    }

    #[tokio::test]
    async fn a_two_hundred_that_is_not_a_token_response_is_malformed_not_accepted() {
        for body in [r#"{"token_type":"Bearer"}"#, "not json", "[]"] {
            let transport = Recording::new(200, body);
            assert_eq!(
                exchange()
                    .redeem_refresh(
                        &transport,
                        "rt-1",
                        SystemTime::now(),
                        CancellationToken::new()
                    )
                    .await
                    .unwrap_err(),
                OAuthFault::MalformedTokenResponse,
                "accepted {body}"
            );
        }
    }

    #[tokio::test]
    async fn a_transport_failure_is_unreachable_not_denied() {
        assert_eq!(
            exchange()
                .redeem_refresh(&Broken, "rt-1", SystemTime::now(), CancellationToken::new())
                .await
                .unwrap_err(),
            OAuthFault::Unreachable,
            "a network failure must not be reported as the server refusing"
        );
    }
}

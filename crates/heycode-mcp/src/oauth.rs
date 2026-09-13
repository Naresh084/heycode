//! MCP04 OAuth 2.1 authorization-code flow with PKCE, and the credential
//! record it produces.
//!
//! Four states have to hold: authorize, refresh, logout, reauthorize. They are
//! modelled as one enum with no way to reach a token except through a completed
//! exchange, because the interesting bugs in OAuth clients are all
//! state-machine bugs — refreshing after logout, exchanging a code against the
//! wrong verifier, treating an absent `expires_in` as "never expires".
//!
//! PKCE here is S256 only. RFC 7636 permits `plain`, and a server that offers
//! only `plain` is a server this client refuses rather than accommodates: the
//! whole point of the exchange is that an interceptor holding the code cannot
//! use it, and `plain` gives that away.

use std::time::{Duration, SystemTime};

use base64::Engine as _;
use heycode_credentials::CredentialSecret;
use sha2::{Digest as _, Sha256};

pub use crate::oauth_registration::{
    OAuthAuthorizationBinding, OAuthAuthorizationServerMetadata, OAuthClientMetadataDocument,
    OAuthClientRegistration, OAuthClientRegistrationMechanism, OAuthDiscovery,
    OAuthDynamicClientMetadata, OAuthIssuer, OAuthProtectedResourceMetadata, OAuthRedirectUri,
    OAuthResource, resolve_client_registration,
};

/// Why an authorization could not proceed.
///
/// A closed set. An OAuth error response is attacker-influenced text and a
/// callback URL carries the code itself, so neither is ever carried in a
/// variant — the class says what happened and the values stay out of logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OAuthFault {
    /// A metadata document or its HTTPS endpoints were malformed.
    InvalidMetadata,
    /// Protected-resource metadata named a different resource.
    ResourceMismatch,
    /// The requested MCP resource identifier was unsafe or malformed.
    InvalidResource,
    /// More than one authorization server was offered without an exact choice.
    AuthorizationServerSelectionRequired,
    /// No usable metadata document was found.
    DiscoveryUnavailable,
    /// Client metadata or a client identifier was malformed.
    InvalidClientMetadata,
    /// A CIMD document's `client_id` differed from its exact URL.
    ClientIdMismatch,
    /// No supported client-registration mechanism was available.
    ClientRegistrationUnavailable,
    /// The DCR endpoint rejected the registration.
    ClientRegistrationRejected,
    /// A successful DCR response did not match the requested public client.
    MalformedClientRegistration,
    /// Issuer-bound client credentials came from another authorization server.
    ClientIssuerMismatch,
    /// A redirect URI was insecure or malformed.
    InvalidRedirectUri,
    /// A callback or registration used a different redirect URI.
    RedirectMismatch,
    /// The server does not offer S256; this client will not downgrade.
    ChallengeMethodUnsupported,
    /// The callback's `state` did not match the pending authorization.
    StateMismatch,
    /// The callback's issuer did not exactly match the recorded issuer.
    IssuerMismatch,
    /// The server advertised RFC 9207 support but the callback omitted `iss`.
    IssuerMissing,
    /// The callback carried no authorization code.
    MissingCode,
    /// The authorization request carried an invalid scope token.
    InvalidScope,
    /// The callback and token exchange were built from different bindings.
    AuthorizationBindingMismatch,
    /// The server's error response denied the request.
    Denied,
    /// The token response was missing a field this client requires.
    MalformedTokenResponse,
    /// A refresh was attempted with no refresh token held.
    NotRefreshable,
    /// An operation requires an authorization this session does not hold.
    NotAuthorized,
    /// The transport failed before any answer arrived.
    Unreachable,
    /// The caller cancelled before the operation committed.
    Cancelled,
}

impl std::fmt::Display for OAuthFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidMetadata => "authorization metadata is invalid",
            Self::ResourceMismatch => "protected-resource metadata names another resource",
            Self::InvalidResource => "the OAuth resource identifier is invalid",
            Self::AuthorizationServerSelectionRequired => {
                "authorization server selection is required"
            }
            Self::DiscoveryUnavailable => "OAuth metadata discovery is unavailable",
            Self::InvalidClientMetadata => "OAuth client metadata is invalid",
            Self::ClientIdMismatch => "client metadata does not match its client id URL",
            Self::ClientRegistrationUnavailable => "no client registration mechanism is available",
            Self::ClientRegistrationRejected => "the authorization server rejected registration",
            Self::MalformedClientRegistration => "the client registration response is invalid",
            Self::ClientIssuerMismatch => {
                "client credentials belong to another authorization server"
            }
            Self::InvalidRedirectUri => "the OAuth redirect URI is invalid",
            Self::RedirectMismatch => "the OAuth redirect URI did not match the registered value",
            Self::ChallengeMethodUnsupported => {
                "the server does not support the S256 code challenge method"
            }
            Self::StateMismatch => "the callback state did not match the pending authorization",
            Self::IssuerMismatch => "the callback issuer did not match the authorization server",
            Self::IssuerMissing => "the callback omitted its required issuer",
            Self::MissingCode => "the callback carried no authorization code",
            Self::InvalidScope => "the authorization scope is invalid",
            Self::AuthorizationBindingMismatch => {
                "the authorization and token exchange bindings differ"
            }
            Self::Denied => "the authorization server denied the request",
            Self::MalformedTokenResponse => "the token response was missing a required field",
            Self::NotRefreshable => "no refresh token is held for this server",
            Self::NotAuthorized => "this server has no current authorization",
            Self::Unreachable => "the authorization server could not be reached",
            Self::Cancelled => "the authorization operation was cancelled",
        })
    }
}

impl std::error::Error for OAuthFault {}

/// PKCE code challenge methods.
///
/// Deliberately has no `Plain` variant. Representing it would invite a
/// negotiation path, and there is no circumstance in which this client should
/// take one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodeChallengeMethod {
    /// SHA-256 of the verifier, base64url-encoded without padding.
    S256,
}

impl CodeChallengeMethod {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::S256 => "S256",
        }
    }

    /// Choose a method from what a server advertises.
    ///
    /// # Errors
    /// [`OAuthFault::ChallengeMethodUnsupported`] unless S256 is offered. An
    /// empty list is also a refusal: a server that advertises nothing has not
    /// told us it supports S256, and "unknown" is not "supported".
    pub fn negotiate(advertised: &[&str]) -> Result<Self, OAuthFault> {
        if advertised.contains(&"S256") {
            return Ok(Self::S256);
        }
        Err(OAuthFault::ChallengeMethodUnsupported)
    }
}

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Cryptographically random bytes.
///
/// `uuid`'s v4 generator draws from the platform CSPRNG through `getrandom`,
/// and each UUID contributes 122 random bits — six of its 128 are the fixed
/// version and variant. Three of them give 366 bits, comfortably past RFC
/// 7636's recommendation of at least 256 for a code verifier.
fn random_bytes_48() -> [u8; 48] {
    let mut bytes = [0_u8; 48];
    for (index, chunk) in bytes.chunks_mut(16).enumerate() {
        let _ = index;
        chunk.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes
}

/// An authorization this client has started but not completed.
///
/// Holds the verifier that proves, at exchange time, that the party redeeming
/// the code is the party that requested it. Debug is hand-written: a verifier
/// in a log is the same disclosure as a leaked code.
pub struct PendingAuthorization {
    verifier: CredentialSecret,
    state: String,
    binding: OAuthAuthorizationBinding,
    method: CodeChallengeMethod,
}

impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("verifier", &"[REDACTED]")
            .field("state", &"[REDACTED]")
            .field("binding", &self.binding)
            .field("method", &self.method)
            .finish()
    }
}

impl PendingAuthorization {
    /// The challenge to send in the authorization request.
    #[must_use]
    pub fn code_challenge(&self) -> String {
        base64url(&Sha256::digest(self.verifier.expose().as_bytes()))
    }

    /// The negotiated challenge method.
    #[must_use]
    pub const fn method(&self) -> CodeChallengeMethod {
        self.method
    }

    /// The exact redirect URI this authorization was started with.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        self.binding.redirect_uri()
    }

    /// The opaque CSRF value the callback must echo.
    ///
    /// Exposed so the caller can build the authorization URL. Comparison is
    /// done by [`Self::accept_callback`], never by the caller.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }

    /// Build the authorization URL from the exact immutable binding.
    ///
    /// # Errors
    /// Invalid scope tokens or an impossible validated endpoint.
    pub fn authorization_url(&self, scopes: &[&str]) -> Result<String, OAuthFault> {
        if scopes.len() > 64 || scopes.iter().any(|scope| !valid_scope_token(scope)) {
            return Err(OAuthFault::InvalidScope);
        }
        let mut url = url::Url::parse(self.binding.authorization_endpoint())
            .map_err(|_| OAuthFault::InvalidMetadata)?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", self.binding.client_id());
            query.append_pair("redirect_uri", self.binding.redirect_uri());
            query.append_pair("code_challenge", &self.code_challenge());
            query.append_pair("code_challenge_method", self.method.as_str());
            query.append_pair("state", &self.state);
            query.append_pair("resource", self.binding.resource());
            if !scopes.is_empty() {
                query.append_pair("scope", &scopes.join(" "));
            }
        }
        Ok(url.into())
    }

    /// Validate a callback and yield the code to exchange.
    ///
    /// # Errors
    /// [`OAuthFault::RedirectMismatch`] for a different callback target;
    /// [`OAuthFault::StateMismatch`] when state differs; issuer faults for an
    /// absent/mismatched RFC 9207 value; [`OAuthFault::MissingCode`] last. The
    /// code is not read until every binding check passes.
    pub fn accept_callback(
        &self,
        callback_redirect_uri: &str,
        state: &str,
        code: Option<&str>,
        issuer: Option<&str>,
    ) -> Result<String, OAuthFault> {
        if callback_redirect_uri != self.binding.redirect_uri() {
            return Err(OAuthFault::RedirectMismatch);
        }
        if state != self.state {
            return Err(OAuthFault::StateMismatch);
        }
        match issuer {
            Some(issuer) if issuer != self.binding.issuer() => {
                return Err(OAuthFault::IssuerMismatch);
            }
            None if self.binding.response_issuer_required() => {
                return Err(OAuthFault::IssuerMissing);
            }
            Some(_) | None => {}
        }
        match code {
            Some(code) if !code.is_empty() => Ok(code.to_owned()),
            _ => Err(OAuthFault::MissingCode),
        }
    }

    /// The verifier, at the one boundary that must send it.
    #[must_use]
    pub fn expose_verifier(&self) -> &str {
        self.verifier.expose()
    }

    pub(crate) const fn binding(&self) -> &OAuthAuthorizationBinding {
        &self.binding
    }
}

fn valid_scope_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| matches!(byte, 0x21 | 0x23..=0x5B | 0x5D..=0x7E))
}

/// When an access token stops being usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenExpiry {
    /// The server stated a lifetime.
    At(SystemTime),
    /// The server stated none. `expires_in` is only RECOMMENDED by RFC 6749,
    /// and an absent one means unknown — never "never expires", and never
    /// "already expired".
    Unknown,
}

/// Whether a token can be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Freshness {
    /// Within its stated lifetime.
    Fresh,
    /// Past its stated lifetime.
    Expired,
    /// No lifetime was stated. Try the request; a 401 is the answer.
    Unknown,
}

/// Tokens held for one server.
pub struct TokenSet {
    access: CredentialSecret,
    refresh: Option<CredentialSecret>,
    expiry: TokenExpiry,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access", &"[REDACTED]")
            .field("refresh", &self.refresh.as_ref().map(|_| "[REDACTED]"))
            .field("expiry", &self.expiry)
            .finish()
    }
}

impl TokenSet {
    /// Build from a parsed token response.
    #[must_use]
    pub fn new(
        access: CredentialSecret,
        refresh: Option<CredentialSecret>,
        expiry: TokenExpiry,
    ) -> Self {
        Self {
            access,
            refresh,
            expiry,
        }
    }

    /// The access token, at the boundary that sends it.
    #[must_use]
    pub fn expose_access(&self) -> &str {
        self.access.expose()
    }

    /// Whether a refresh is possible without sending the user back to a browser.
    #[must_use]
    pub const fn refreshable(&self) -> bool {
        self.refresh.is_some()
    }

    /// Stated expiry.
    #[must_use]
    pub const fn expiry(&self) -> TokenExpiry {
        self.expiry
    }

    /// Usability at `now`.
    #[must_use]
    pub fn freshness(&self, now: SystemTime) -> Freshness {
        match self.expiry {
            TokenExpiry::Unknown => Freshness::Unknown,
            TokenExpiry::At(at) if now < at => Freshness::Fresh,
            TokenExpiry::At(_) => Freshness::Expired,
        }
    }

    /// Parse an RFC 6749 token response.
    ///
    /// # Errors
    /// [`OAuthFault::MalformedTokenResponse`] when `access_token` is absent or
    /// not a string. A missing `expires_in` is not an error — it is
    /// [`TokenExpiry::Unknown`].
    pub fn from_response(body: &serde_json::Value, now: SystemTime) -> Result<Self, OAuthFault> {
        let access = body
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or(OAuthFault::MalformedTokenResponse)?;
        let refresh = body
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .map(CredentialSecret::new);
        let expiry = body
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .map_or(TokenExpiry::Unknown, |seconds| {
                TokenExpiry::At(now + Duration::from_secs(seconds))
            });
        Ok(Self::new(CredentialSecret::new(access), refresh, expiry))
    }
}

/// The authorization state of one MCP server.
#[derive(Debug)]
#[non_exhaustive]
pub enum McpAuthState {
    /// Never authorized, or logged out. Reaching a token requires a full
    /// browser authorization.
    Unauthenticated,
    /// Holding tokens.
    Authorized(TokenSet),
}

impl McpAuthState {
    /// True only while tokens are held.
    #[must_use]
    pub const fn authorized(&self) -> bool {
        matches!(self, Self::Authorized(_))
    }

    /// The held tokens, if any.
    #[must_use]
    pub const fn tokens(&self) -> Option<&TokenSet> {
        match self {
            Self::Unauthenticated => None,
            Self::Authorized(tokens) => Some(tokens),
        }
    }
}

/// One server's OAuth client: starts authorizations and owns the resulting state.
#[derive(Debug)]
pub struct McpOAuthClient {
    binding: OAuthAuthorizationBinding,
    state: McpAuthState,
}

impl McpOAuthClient {
    /// A validated client that holds no authorization yet.
    #[must_use]
    pub fn new(binding: OAuthAuthorizationBinding) -> Self {
        Self {
            binding,
            state: McpAuthState::Unauthenticated,
        }
    }

    /// Exact immutable authorization binding.
    #[must_use]
    pub const fn binding(&self) -> &OAuthAuthorizationBinding {
        &self.binding
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> &McpAuthState {
        &self.state
    }

    /// Begin an authorization, generating a fresh verifier and CSRF state.
    ///
    /// Discovery already proved S256 support, so beginning cannot downgrade or
    /// fail on a second, caller-supplied capability list.
    #[must_use]
    pub fn begin(&self) -> PendingAuthorization {
        PendingAuthorization {
            verifier: CredentialSecret::new(base64url(&random_bytes_48())),
            state: base64url(uuid::Uuid::new_v4().as_bytes()),
            binding: self.binding.clone(),
            method: CodeChallengeMethod::S256,
        }
    }

    /// Adopt the tokens produced by a completed exchange.
    ///
    /// Takes `PendingAuthorization` by value: an authorization is single-use,
    /// and a verifier that can be replayed against a second code is a verifier
    /// that has stopped proving anything.
    ///
    /// # Errors
    /// [`OAuthFault::AuthorizationBindingMismatch`] when `pending` belongs to
    /// another issuer/client/resource binding.
    pub fn complete(
        &mut self,
        pending: PendingAuthorization,
        tokens: TokenSet,
    ) -> Result<(), OAuthFault> {
        if !self.binding.same_exchange(pending.binding()) {
            return Err(OAuthFault::AuthorizationBindingMismatch);
        }
        drop(pending);
        self.state = McpAuthState::Authorized(tokens);
        Ok(())
    }

    /// Replace held tokens after a refresh, preserving the refresh token when
    /// the server did not issue a new one.
    ///
    /// # Errors
    /// [`OAuthFault::NotAuthorized`] when nothing is held;
    /// [`OAuthFault::NotRefreshable`] when no refresh token is held.
    pub fn refreshed(&mut self, mut refreshed: TokenSet) -> Result<(), OAuthFault> {
        let current = match &self.state {
            McpAuthState::Unauthenticated => return Err(OAuthFault::NotAuthorized),
            McpAuthState::Authorized(tokens) => tokens,
        };
        if !current.refreshable() {
            return Err(OAuthFault::NotRefreshable);
        }
        if refreshed.refresh.is_none() {
            // RFC 6749 §6: a refresh response MAY omit a new refresh token, in
            // which case the existing one remains valid. Dropping it here would
            // silently downgrade the session to one-shot.
            refreshed.refresh =
                match std::mem::replace(&mut self.state, McpAuthState::Unauthenticated) {
                    McpAuthState::Authorized(tokens) => tokens.refresh,
                    McpAuthState::Unauthenticated => None,
                };
        }
        self.state = McpAuthState::Authorized(refreshed);
        Ok(())
    }

    /// The refresh token, at the boundary that sends it.
    ///
    /// # Errors
    /// [`OAuthFault::NotAuthorized`] when nothing is held;
    /// [`OAuthFault::NotRefreshable`] when the session holds no refresh token.
    pub fn expose_refresh_token(&self) -> Result<&str, OAuthFault> {
        match &self.state {
            McpAuthState::Unauthenticated => Err(OAuthFault::NotAuthorized),
            McpAuthState::Authorized(tokens) => tokens
                .refresh
                .as_ref()
                .map(CredentialSecret::expose)
                .ok_or(OAuthFault::NotRefreshable),
        }
    }

    /// Discard every token, returning to the unauthorized state.
    ///
    /// Idempotent, and total: after this, no held value can authorize a
    /// request, which is what makes "logged out" a fact rather than a flag.
    pub fn logout(&mut self) {
        self.state = McpAuthState::Unauthenticated;
    }
}

/// Durable storage for one MCP server's OAuth tokens.
///
/// The whole token set is stored as **one** credential record rather than one
/// record per token. Two reasons, and the second is the important one: the
/// stated expiry travels with the tokens it describes and cannot go stale
/// independently, and logout is a single delete. "Delete one thing" is provably
/// total; "delete three things and hope none is orphaned" is not, and a
/// forgotten refresh token after logout is a live credential the user believes
/// they revoked.
///
/// The record's value is a JSON document, so the expiry gets secret-grade
/// handling too. That is deliberately conservative: it costs nothing and keeps
/// one thing to protect instead of two.
pub struct OAuthRecords {
    service: std::sync::Arc<heycode_credentials::CredentialsService>,
    query: heycode_credentials::CredentialQuery,
    issuer: String,
    resource: String,
    client_id: String,
}

impl std::fmt::Debug for OAuthRecords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthRecords")
            .field("reference", &self.query.reference.as_str())
            .finish_non_exhaustive()
    }
}

/// The credential kind every MCP OAuth record is stored under.
pub const MCP_OAUTH_CREDENTIAL_KIND: &str = "mcp-oauth-tokens";

impl OAuthRecords {
    /// Bind storage for one server by name.
    ///
    /// # Errors
    /// [`heycode_credentials::CredentialsError`] when `server` is not a valid
    /// credential reference.
    pub fn new(
        service: std::sync::Arc<heycode_credentials::CredentialsService>,
        server: &str,
        binding: &OAuthAuthorizationBinding,
    ) -> Result<Self, heycode_credentials::CredentialsError> {
        let reference =
            heycode_credentials::CredentialReference::new(format!("mcp-oauth:{server}"))?;
        let kind = heycode_credentials::CredentialKind::new(MCP_OAUTH_CREDENTIAL_KIND)?;
        Ok(Self {
            service,
            query: heycode_credentials::CredentialQuery::new(reference, kind),
            issuer: binding.issuer().to_owned(),
            resource: binding.resource().to_owned(),
            client_id: binding.client_id().to_owned(),
        })
    }

    /// Persist a token set, replacing any previous one.
    ///
    /// # Errors
    /// Propagates a credential-provider failure. The text is provider-supplied
    /// and already redacted by the credentials layer.
    pub fn store(&self, tokens: &TokenSet) -> Result<(), heycode_credentials::CredentialsError> {
        let expires_at = match tokens.expiry() {
            TokenExpiry::Unknown => serde_json::Value::Null,
            TokenExpiry::At(at) => at
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(serde_json::Value::Null, |since| {
                    serde_json::Value::from(since.as_secs())
                }),
        };
        let document = serde_json::json!({
            "schema_version": 1,
            "issuer": self.issuer,
            "resource": self.resource,
            "client_id": self.client_id,
            "access_token": tokens.expose_access(),
            "refresh_token": tokens.refresh.as_ref().map(CredentialSecret::expose),
            "expires_at_unix": expires_at,
        });
        self.service
            .write(&self.query, &CredentialSecret::new(document.to_string()))
            .map(|_| ())
    }

    /// Load a previously stored token set.
    ///
    /// A record that cannot be parsed yields `None` rather than an error: a
    /// corrupt or older-format record must send the user through a fresh
    /// authorization, not wedge the server behind a failure they cannot clear.
    ///
    /// # Errors
    /// Propagates a credential-provider failure.
    pub fn load(&self) -> Result<Option<TokenSet>, heycode_credentials::CredentialsError> {
        let Some(secret) = self.service.resolve(&self.query)? else {
            return Ok(None);
        };
        let Ok(document) = serde_json::from_str::<serde_json::Value>(secret.expose()) else {
            return Ok(None);
        };
        if document
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
            || document.get("issuer").and_then(serde_json::Value::as_str)
                != Some(self.issuer.as_str())
            || document.get("resource").and_then(serde_json::Value::as_str)
                != Some(self.resource.as_str())
            || document
                .get("client_id")
                .and_then(serde_json::Value::as_str)
                != Some(self.client_id.as_str())
        {
            return Ok(None);
        }
        let Some(access) = document
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
        else {
            return Ok(None);
        };
        let refresh = document
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .map(CredentialSecret::new);
        let expiry = document
            .get("expires_at_unix")
            .and_then(serde_json::Value::as_u64)
            .map_or(TokenExpiry::Unknown, |seconds| {
                TokenExpiry::At(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
            });
        Ok(Some(TokenSet::new(
            CredentialSecret::new(access),
            refresh,
            expiry,
        )))
    }

    /// Remove the stored record.
    ///
    /// # Errors
    /// Propagates a credential-provider failure. Deleting an absent record is
    /// not an error — logout must be idempotent, because a user who logs out
    /// twice has not done anything wrong.
    pub fn clear(&self) -> Result<(), heycode_credentials::CredentialsError> {
        self.service.delete(&self.query).map(|_| ())
    }
}

/// Percent-encode one `application/x-www-form-urlencoded` value.
///
/// A verifier and a code are base64url, and a redirect URI carries `:` and `/`
/// — all of which must be escaped in a form body. Encoding everything outside
/// the unreserved set is the conservative direction: over-escaping is always
/// decoded correctly, while under-escaping silently corrupts the value.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn form_body(pairs: &[(&str, &str)]) -> Vec<u8> {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

/// Exchange requests against one authorization server's token endpoint.
///
/// Separate from [`McpOAuthClient`] so the state machine stays synchronous and
/// testable without a transport, and so the one place that puts a verifier or a
/// refresh token on the wire is a single, auditable function.
pub struct TokenExchange {
    binding: OAuthAuthorizationBinding,
}

impl std::fmt::Debug for TokenExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenExchange")
            .field("binding", &self.binding)
            .finish()
    }
}

impl TokenExchange {
    /// Bind the exact metadata/registration/resource tuple used to authorize.
    #[must_use]
    pub fn new(binding: OAuthAuthorizationBinding) -> Self {
        Self { binding }
    }

    /// Redeem an authorization code, proving possession of the verifier.
    ///
    /// # Errors
    /// [`OAuthFault::Unreachable`] when the transport fails, [`OAuthFault::Denied`]
    /// on a non-2xx answer, [`OAuthFault::MalformedTokenResponse`] when the body
    /// is not a token response.
    pub async fn redeem_code(
        &self,
        transport: &dyn heycode_http::HttpTransport,
        pending: &PendingAuthorization,
        code: &str,
        now: SystemTime,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<TokenSet, OAuthFault> {
        if !self.binding.same_exchange(pending.binding()) {
            return Err(OAuthFault::AuthorizationBindingMismatch);
        }
        if code.is_empty() {
            return Err(OAuthFault::MissingCode);
        }
        self.post(
            transport,
            vec![
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", pending.redirect_uri()),
                ("code_verifier", pending.expose_verifier()),
                ("resource", self.binding.resource()),
            ],
            now,
            cancellation,
        )
        .await
    }

    /// Exchange a refresh token for a new access token.
    ///
    /// # Errors
    /// As [`Self::redeem_code`].
    pub async fn redeem_refresh(
        &self,
        transport: &dyn heycode_http::HttpTransport,
        refresh_token: &str,
        now: SystemTime,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<TokenSet, OAuthFault> {
        if refresh_token.is_empty() {
            return Err(OAuthFault::NotRefreshable);
        }
        self.post(
            transport,
            vec![
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("resource", self.binding.resource()),
            ],
            now,
            cancellation,
        )
        .await
    }

    async fn post(
        &self,
        transport: &dyn heycode_http::HttpTransport,
        mut pairs: Vec<(&str, &str)>,
        now: SystemTime,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<TokenSet, OAuthFault> {
        if cancellation.is_cancelled() {
            return Err(OAuthFault::Cancelled);
        }
        let mut basic = None;
        match self.binding.authentication_method() {
            "none" => pairs.push(("client_id", self.binding.client_id())),
            "client_secret_post" => {
                pairs.push(("client_id", self.binding.client_id()));
                let Some(secret) = self.binding.client_secret() else {
                    return Err(OAuthFault::AuthorizationBindingMismatch);
                };
                pairs.push(("client_secret", secret));
            }
            "client_secret_basic" => {
                let Some(secret) = self.binding.client_secret() else {
                    return Err(OAuthFault::AuthorizationBindingMismatch);
                };
                let value = format!(
                    "{}:{}",
                    form_encode(self.binding.client_id()),
                    form_encode(secret)
                );
                basic = Some(format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
                ));
            }
            _ => return Err(OAuthFault::AuthorizationBindingMismatch),
        }
        let body = form_body(&pairs);
        let mut request = heycode_http::HttpRequest::post(self.binding.token_endpoint(), body)
            .and_then(|request| request.header("content-type", "application/x-www-form-urlencoded"))
            .and_then(|request| request.header("accept", "application/json"))
            .map_err(|_| OAuthFault::Unreachable)?;
        if let Some(basic) = basic {
            request = request
                .header("authorization", &basic)
                .map_err(|_| OAuthFault::Unreachable)?;
        }
        let response = transport
            .send(request, cancellation)
            .await
            .map_err(|_| OAuthFault::Unreachable)?;
        if !(200..300).contains(&response.status) {
            // The server's error body is attacker-influenced and may echo the
            // request; the status is enough to act on and safe to keep.
            return Err(OAuthFault::Denied);
        }
        let document = serde_json::from_slice::<serde_json::Value>(&response.body)
            .map_err(|_| OAuthFault::MalformedTokenResponse)?;
        TokenSet::from_response(&document, now)
    }
}

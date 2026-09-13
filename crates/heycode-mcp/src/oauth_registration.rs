//! MCP OAuth protected-resource discovery and client registration.
//!
//! The current protocol has three registration mechanisms with an explicit
//! priority: pre-registered information, Client ID Metadata Documents (CIMD),
//! then deprecated Dynamic Client Registration (DCR). Pre-registered and DCR
//! clients are bound to the authorization-server issuer that minted them;
//! CIMD client ids are portable HTTPS document URLs.

use std::collections::BTreeSet;
use std::sync::Arc;

use heycode_credentials::CredentialSecret;
use heycode_http::{HttpRequest, HttpResponse, HttpTransport};
use tokio_util::sync::CancellationToken;

use crate::oauth::OAuthFault;

const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_URL_BYTES: usize = 2 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 2 * 1024;
const MAX_CLIENT_NAME_BYTES: usize = 256;
const MAX_REDIRECT_URIS: usize = 16;
const MAX_AUTHORIZATION_SERVERS: usize = 16;
const MAX_METADATA_STRING_ROWS: usize = 64;

/// Validated exact OAuth authorization-server issuer.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OAuthIssuer(String);

impl OAuthIssuer {
    /// Validate an issuer used for exact RFC 9207 comparison.
    ///
    /// # Errors
    /// Issuers must be bounded HTTPS URLs with authority and no credentials,
    /// query or fragment.
    pub fn new(value: impl Into<String>) -> Result<Self, OAuthFault> {
        let value = value.into();
        validate_issuer(&value)?;
        Ok(Self(value))
    }

    /// Exact issuer spelling. Callers must not normalize it before comparison.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for OAuthIssuer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OAuthIssuer([REDACTED])")
    }
}

/// Validated canonical MCP resource indicator.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OAuthResource(String);

impl OAuthResource {
    /// Validate one resource identifier.
    ///
    /// # Errors
    /// Resources must be bounded HTTPS URLs with authority and no credentials,
    /// query or fragment.
    pub fn new(value: impl Into<String>) -> Result<Self, OAuthFault> {
        let value = value.into();
        validate_https_url(&value, false).map_err(|_| OAuthFault::InvalidResource)?;
        Ok(Self(value))
    }

    /// Exact resource spelling sent in authorization and token requests.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for OAuthResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OAuthResource([REDACTED])")
    }
}

/// Validated redirect URI for one native or web OAuth client.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OAuthRedirectUri(String);

impl OAuthRedirectUri {
    /// Validate an exact registered redirect URI.
    ///
    /// HTTPS redirects are accepted. HTTP is accepted only for `localhost` or
    /// a literal loopback address. Credentials and fragments are never valid.
    ///
    /// # Errors
    /// [`OAuthFault::InvalidRedirectUri`] for any unsafe or malformed URI.
    pub fn new(value: impl Into<String>) -> Result<Self, OAuthFault> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_URL_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(OAuthFault::InvalidRedirectUri);
        }
        let parsed = url::Url::parse(&value).map_err(|_| OAuthFault::InvalidRedirectUri)?;
        if parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(OAuthFault::InvalidRedirectUri);
        }
        let secure = parsed.scheme() == "https";
        let loopback = parsed.scheme() == "http" && redirect_host_is_loopback(&parsed);
        if !secure && !loopback {
            return Err(OAuthFault::InvalidRedirectUri);
        }
        Ok(Self(value))
    }

    /// Exact registered URI spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for OAuthRedirectUri {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OAuthRedirectUri([REDACTED])")
    }
}

fn redirect_host_is_loopback(parsed: &url::Url) -> bool {
    match parsed.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// Validated OAuth protected-resource metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthProtectedResourceMetadata {
    resource: OAuthResource,
    authorization_servers: Vec<OAuthIssuer>,
}

impl OAuthProtectedResourceMetadata {
    /// Parse a metadata document and bind it to the requested MCP resource.
    ///
    /// # Errors
    /// Missing, malformed or substituted resource/issuer metadata.
    pub fn parse(expected_resource: &str, value: &serde_json::Value) -> Result<Self, OAuthFault> {
        let expected = OAuthResource::new(expected_resource)?;
        let object = value.as_object().ok_or(OAuthFault::InvalidMetadata)?;
        if object.get("resource").and_then(serde_json::Value::as_str) != Some(expected.as_str()) {
            return Err(OAuthFault::ResourceMismatch);
        }
        let rows = string_array(
            object.get("authorization_servers"),
            MAX_AUTHORIZATION_SERVERS,
            MAX_URL_BYTES,
            OAuthFault::InvalidMetadata,
        )?;
        let mut seen = BTreeSet::new();
        let mut authorization_servers = Vec::with_capacity(rows.len());
        for row in rows {
            let issuer = OAuthIssuer::new(row)?;
            if !seen.insert(issuer.as_str().to_owned()) {
                return Err(OAuthFault::InvalidMetadata);
            }
            authorization_servers.push(issuer);
        }
        Ok(Self {
            resource: expected,
            authorization_servers,
        })
    }

    /// Exact resource identifier.
    #[must_use]
    pub fn resource(&self) -> &str {
        self.resource.as_str()
    }

    /// Authorization servers in declared order.
    #[must_use]
    pub fn authorization_servers(&self) -> &[OAuthIssuer] {
        &self.authorization_servers
    }
}

/// Validated authorization-server metadata used for one flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAuthorizationServerMetadata {
    issuer: OAuthIssuer,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    client_id_metadata_document_supported: bool,
    authorization_response_iss_parameter_supported: bool,
}

impl OAuthAuthorizationServerMetadata {
    /// Parse metadata fetched for `expected_issuer`.
    ///
    /// # Errors
    /// Issuer substitution, missing HTTPS endpoints, malformed capability
    /// fields, or absent S256 support.
    pub fn parse(expected_issuer: &str, value: &serde_json::Value) -> Result<Self, OAuthFault> {
        let expected = OAuthIssuer::new(expected_issuer)?;
        let object = value.as_object().ok_or(OAuthFault::InvalidMetadata)?;
        let Some(issuer) = object.get("issuer").and_then(serde_json::Value::as_str) else {
            return Err(OAuthFault::InvalidMetadata);
        };
        if issuer != expected.as_str() {
            return Err(OAuthFault::IssuerMismatch);
        }
        let authorization_endpoint = required_https_endpoint(object, "authorization_endpoint")?;
        let token_endpoint = required_https_endpoint(object, "token_endpoint")?;
        let registration_endpoint = match object.get("registration_endpoint") {
            None => None,
            Some(serde_json::Value::String(value)) => {
                validate_https_url(value, true).map_err(|_| OAuthFault::InvalidMetadata)?;
                Some(value.clone())
            }
            Some(_) => return Err(OAuthFault::InvalidMetadata),
        };
        let methods = string_array(
            object.get("code_challenge_methods_supported"),
            MAX_METADATA_STRING_ROWS,
            64,
            OAuthFault::ChallengeMethodUnsupported,
        )?;
        if !methods.iter().any(|method| method == "S256") {
            return Err(OAuthFault::ChallengeMethodUnsupported);
        }
        let client_id_metadata_document_supported = optional_bool(
            object,
            "client_id_metadata_document_supported",
            OAuthFault::InvalidMetadata,
        )?
        .unwrap_or(false);
        let authorization_response_iss_parameter_supported = optional_bool(
            object,
            "authorization_response_iss_parameter_supported",
            OAuthFault::InvalidMetadata,
        )?
        .unwrap_or(false);
        Ok(Self {
            issuer: expected,
            authorization_endpoint,
            token_endpoint,
            registration_endpoint,
            client_id_metadata_document_supported,
            authorization_response_iss_parameter_supported,
        })
    }

    /// Exact issuer identifier.
    #[must_use]
    pub fn issuer(&self) -> &str {
        self.issuer.as_str()
    }

    /// Validated authorization endpoint.
    #[must_use]
    pub fn authorization_endpoint(&self) -> &str {
        &self.authorization_endpoint
    }

    /// Validated token endpoint.
    #[must_use]
    pub fn token_endpoint(&self) -> &str {
        &self.token_endpoint
    }

    /// Optional deprecated DCR endpoint.
    #[must_use]
    pub fn registration_endpoint(&self) -> Option<&str> {
        self.registration_endpoint.as_deref()
    }

    /// Whether this authorization server advertises CIMD support.
    #[must_use]
    pub const fn client_id_metadata_document_supported(&self) -> bool {
        self.client_id_metadata_document_supported
    }

    /// Whether a callback without RFC 9207 `iss` must be rejected.
    #[must_use]
    pub const fn authorization_response_iss_parameter_supported(&self) -> bool {
        self.authorization_response_iss_parameter_supported
    }
}

/// Result of protected-resource and authorization-server discovery.
#[derive(Debug)]
pub struct OAuthDiscovery {
    protected_resource: OAuthProtectedResourceMetadata,
    authorization_server: OAuthAuthorizationServerMetadata,
}

impl OAuthDiscovery {
    /// Discover and validate the metadata chain for one MCP HTTPS resource.
    ///
    /// The optional `advertised_resource_metadata` is the URL extracted from a
    /// validated `WWW-Authenticate` challenge. Without it, the path-specific
    /// and root RFC 9728 well-known URLs are tried in order. Multiple declared
    /// authorization servers require an exact `selected_issuer`.
    ///
    /// # Errors
    /// Network/cancellation failures, missing metadata, resource/issuer
    /// substitution, ambiguous server selection or malformed documents.
    pub async fn discover(
        transport: &dyn HttpTransport,
        resource: &str,
        advertised_resource_metadata: Option<&str>,
        selected_issuer: Option<&str>,
        cancellation: CancellationToken,
    ) -> Result<Self, OAuthFault> {
        let resource = OAuthResource::new(resource)?;
        let protected_urls = match advertised_resource_metadata {
            Some(url) => {
                validate_https_url(url, true).map_err(|_| OAuthFault::InvalidMetadata)?;
                vec![url.to_owned()]
            }
            None => protected_resource_candidates(resource.as_str())?,
        };
        let protected_document = fetch_first_metadata(
            transport,
            &protected_urls,
            &cancellation,
            OAuthFault::DiscoveryUnavailable,
        )
        .await?;
        let protected_resource =
            OAuthProtectedResourceMetadata::parse(resource.as_str(), &protected_document)?;
        let issuer = select_issuer(&protected_resource, selected_issuer)?;
        let metadata_urls = authorization_server_candidates(issuer.as_str())?;
        let server_document = fetch_first_metadata(
            transport,
            &metadata_urls,
            &cancellation,
            OAuthFault::DiscoveryUnavailable,
        )
        .await?;
        let authorization_server =
            OAuthAuthorizationServerMetadata::parse(issuer.as_str(), &server_document)?;
        Ok(Self {
            protected_resource,
            authorization_server,
        })
    }

    /// Exact MCP resource identifier.
    #[must_use]
    pub fn resource(&self) -> &str {
        self.protected_resource.resource()
    }

    /// Selected, validated authorization-server metadata.
    #[must_use]
    pub const fn authorization_server(&self) -> &OAuthAuthorizationServerMetadata {
        &self.authorization_server
    }

    /// Consume the discovery into a resource and selected server metadata.
    #[must_use]
    pub fn into_parts(self) -> (OAuthResource, OAuthAuthorizationServerMetadata) {
        (self.protected_resource.resource, self.authorization_server)
    }
}

fn select_issuer<'a>(
    metadata: &'a OAuthProtectedResourceMetadata,
    selected: Option<&str>,
) -> Result<&'a OAuthIssuer, OAuthFault> {
    match (metadata.authorization_servers.as_slice(), selected) {
        ([only], None) => Ok(only),
        (_, Some(selected)) => metadata
            .authorization_servers
            .iter()
            .find(|issuer| issuer.as_str() == selected)
            .ok_or(OAuthFault::IssuerMismatch),
        _ => Err(OAuthFault::AuthorizationServerSelectionRequired),
    }
}

async fn fetch_first_metadata(
    transport: &dyn HttpTransport,
    candidates: &[String],
    cancellation: &CancellationToken,
    missing: OAuthFault,
) -> Result<serde_json::Value, OAuthFault> {
    for candidate in candidates {
        if cancellation.is_cancelled() {
            return Err(OAuthFault::Cancelled);
        }
        let request = HttpRequest::get(candidate)
            .and_then(|request| request.header("accept", "application/json"))
            .map(|request| request.with_max_response_bytes(MAX_METADATA_BYTES))
            .map_err(|_| OAuthFault::Unreachable)?;
        let response = transport
            .send(request, cancellation.clone())
            .await
            .map_err(|_| OAuthFault::Unreachable)?;
        if response.status == 404 {
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Err(missing);
        }
        ensure_json_response(&response)?;
        return serde_json::from_slice(&response.body).map_err(|_| OAuthFault::InvalidMetadata);
    }
    Err(missing)
}

fn ensure_json_response(response: &HttpResponse) -> Result<(), OAuthFault> {
    match response.content_type.as_deref().map(media_type) {
        Some("application/json") | Some("application/oauth-authz-req+jwt") | None => Ok(()),
        _ => Err(OAuthFault::InvalidMetadata),
    }
}

fn media_type(value: &str) -> &str {
    value.split(';').next().unwrap_or_default().trim()
}

fn protected_resource_candidates(resource: &str) -> Result<Vec<String>, OAuthFault> {
    let parsed = url::Url::parse(resource).map_err(|_| OAuthFault::InvalidResource)?;
    let path = parsed.path().trim_start_matches('/').to_owned();
    let mut specific = parsed.clone();
    specific.set_query(None);
    specific.set_fragment(None);
    let specific_path = if path.is_empty() {
        "/.well-known/oauth-protected-resource".to_owned()
    } else {
        format!("/.well-known/oauth-protected-resource/{path}")
    };
    specific.set_path(&specific_path);
    let mut root = parsed;
    root.set_query(None);
    root.set_fragment(None);
    root.set_path("/.well-known/oauth-protected-resource");
    let mut rows = vec![specific.to_string()];
    if root.as_str() != rows[0] {
        rows.push(root.to_string());
    }
    Ok(rows)
}

fn authorization_server_candidates(issuer: &str) -> Result<Vec<String>, OAuthFault> {
    let parsed = url::Url::parse(issuer).map_err(|_| OAuthFault::InvalidMetadata)?;
    let path = parsed.path().trim_matches('/').to_owned();
    let mut rows = Vec::with_capacity(3);
    for suffix in ["oauth-authorization-server", "openid-configuration"] {
        let mut inserted = parsed.clone();
        inserted.set_query(None);
        inserted.set_fragment(None);
        let inserted_path = if path.is_empty() {
            format!("/.well-known/{suffix}")
        } else {
            format!("/.well-known/{suffix}/{path}")
        };
        inserted.set_path(&inserted_path);
        rows.push(inserted.to_string());
    }
    if !path.is_empty() {
        let mut appended = parsed;
        appended.set_query(None);
        appended.set_fragment(None);
        appended.set_path(&format!("/{path}/.well-known/openid-configuration"));
        rows.push(appended.to_string());
    }
    Ok(rows)
}

/// Validated self-hosted Client ID Metadata Document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthClientMetadataDocument {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<OAuthRedirectUri>,
}

impl OAuthClientMetadataDocument {
    /// Parse one locally configured CIMD document.
    ///
    /// # Errors
    /// The document URL must be HTTPS with a path, `client_id` must equal that
    /// URL byte-for-byte, required fields must be present, and redirects must
    /// be transport-safe.
    pub fn parse(document_url: &str, value: &serde_json::Value) -> Result<Self, OAuthFault> {
        validate_cimd_url(document_url)?;
        let object = value.as_object().ok_or(OAuthFault::InvalidClientMetadata)?;
        let client_id = object
            .get("client_id")
            .and_then(serde_json::Value::as_str)
            .ok_or(OAuthFault::InvalidClientMetadata)?;
        if client_id != document_url {
            return Err(OAuthFault::ClientIdMismatch);
        }
        let client_name = object
            .get("client_name")
            .and_then(serde_json::Value::as_str)
            .filter(|value| safe_text(value, MAX_CLIENT_NAME_BYTES))
            .ok_or(OAuthFault::InvalidClientMetadata)?;
        let redirects = string_array(
            object.get("redirect_uris"),
            MAX_REDIRECT_URIS,
            MAX_URL_BYTES,
            OAuthFault::InvalidClientMetadata,
        )?;
        let mut seen = BTreeSet::new();
        let mut redirect_uris = Vec::with_capacity(redirects.len());
        for row in redirects {
            let redirect = OAuthRedirectUri::new(row)?;
            if !seen.insert(redirect.as_str().to_owned()) {
                return Err(OAuthFault::InvalidClientMetadata);
            }
            redirect_uris.push(redirect);
        }
        require_array_member(object, "grant_types", "authorization_code")?;
        require_array_member(object, "response_types", "code")?;
        if object
            .get("token_endpoint_auth_method")
            .and_then(serde_json::Value::as_str)
            != Some("none")
        {
            return Err(OAuthFault::InvalidClientMetadata);
        }
        Ok(Self {
            client_id: client_id.to_owned(),
            client_name: client_name.to_owned(),
            redirect_uris,
        })
    }

    /// HTTPS document URL used as the portable client id.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Bounded client label.
    #[must_use]
    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    /// Registered redirects.
    #[must_use]
    pub fn redirect_uris(&self) -> &[OAuthRedirectUri] {
        &self.redirect_uris
    }
}

/// DCR metadata emitted by this native public client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthDynamicClientMetadata {
    client_name: String,
    redirect_uri: OAuthRedirectUri,
}

impl OAuthDynamicClientMetadata {
    /// Construct native-client DCR metadata.
    ///
    /// # Errors
    /// Empty/control-bearing labels or invalid redirect URIs.
    pub fn native(
        client_name: impl Into<String>,
        redirect_uri: OAuthRedirectUri,
    ) -> Result<Self, OAuthFault> {
        let client_name = client_name.into();
        if !safe_text(&client_name, MAX_CLIENT_NAME_BYTES) {
            return Err(OAuthFault::InvalidClientMetadata);
        }
        Ok(Self {
            client_name,
            redirect_uri,
        })
    }

    /// Exact callback used for DCR and the subsequent authorization.
    #[must_use]
    pub const fn redirect_uri(&self) -> &OAuthRedirectUri {
        &self.redirect_uri
    }

    fn wire_value(&self) -> serde_json::Value {
        serde_json::json!({
            "client_name": self.client_name,
            "redirect_uris": [self.redirect_uri.as_str()],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "application_type": "native"
        })
    }
}

/// How one client id was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OAuthClientRegistrationMechanism {
    /// Static information supplied for one exact issuer.
    PreRegistered,
    /// Portable self-hosted HTTPS client metadata.
    ClientIdMetadataDocument,
    /// Deprecated dynamic registration, retained for compatibility.
    Dynamic,
}

enum OAuthClientAuthentication {
    Public,
    SecretPost(CredentialSecret),
    SecretBasic(CredentialSecret),
}

impl OAuthClientAuthentication {
    const fn method(&self) -> &'static str {
        match self {
            Self::Public => "none",
            Self::SecretPost(_) => "client_secret_post",
            Self::SecretBasic(_) => "client_secret_basic",
        }
    }
}

/// Validated client registration selected for one authorization.
pub struct OAuthClientRegistration {
    mechanism: OAuthClientRegistrationMechanism,
    issuer_binding: Option<OAuthIssuer>,
    client_id: String,
    authentication: OAuthClientAuthentication,
    redirect_uris: Vec<OAuthRedirectUri>,
}

impl std::fmt::Debug for OAuthClientRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthClientRegistration")
            .field("mechanism", &self.mechanism)
            .field("issuer_bound", &self.issuer_binding.is_some())
            .field("client_id", &"[REDACTED]")
            .field("authentication", &self.authentication.method())
            .field("redirects", &self.redirect_uris.len())
            .finish()
    }
}

impl OAuthClientRegistration {
    /// Construct a public pre-registered client bound to one issuer.
    ///
    /// # Errors
    /// Invalid issuer, client id or redirect URI.
    pub fn pre_registered_public(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        redirect_uri: OAuthRedirectUri,
    ) -> Result<Self, OAuthFault> {
        Self::pre_registered(
            issuer,
            client_id,
            OAuthClientAuthentication::Public,
            redirect_uri,
        )
    }

    /// Construct a `client_secret_post` pre-registered client.
    ///
    /// # Errors
    /// Invalid issuer, client id or redirect URI.
    pub fn pre_registered_secret_post(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        secret: CredentialSecret,
        redirect_uri: OAuthRedirectUri,
    ) -> Result<Self, OAuthFault> {
        Self::pre_registered(
            issuer,
            client_id,
            OAuthClientAuthentication::SecretPost(secret),
            redirect_uri,
        )
    }

    /// Construct a `client_secret_basic` pre-registered client.
    ///
    /// # Errors
    /// Invalid issuer, client id or redirect URI.
    pub fn pre_registered_secret_basic(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        secret: CredentialSecret,
        redirect_uri: OAuthRedirectUri,
    ) -> Result<Self, OAuthFault> {
        Self::pre_registered(
            issuer,
            client_id,
            OAuthClientAuthentication::SecretBasic(secret),
            redirect_uri,
        )
    }

    fn pre_registered(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        authentication: OAuthClientAuthentication,
        redirect_uri: OAuthRedirectUri,
    ) -> Result<Self, OAuthFault> {
        let client_id = client_id.into();
        validate_client_id(&client_id)?;
        Ok(Self {
            mechanism: OAuthClientRegistrationMechanism::PreRegistered,
            issuer_binding: Some(OAuthIssuer::new(issuer)?),
            client_id,
            authentication,
            redirect_uris: vec![redirect_uri],
        })
    }

    fn from_cimd(document: OAuthClientMetadataDocument) -> Self {
        Self {
            mechanism: OAuthClientRegistrationMechanism::ClientIdMetadataDocument,
            issuer_binding: None,
            client_id: document.client_id,
            authentication: OAuthClientAuthentication::Public,
            redirect_uris: document.redirect_uris,
        }
    }

    fn from_dynamic(
        issuer: &str,
        client_id: String,
        redirect_uri: OAuthRedirectUri,
    ) -> Result<Self, OAuthFault> {
        validate_client_id(&client_id)?;
        Ok(Self {
            mechanism: OAuthClientRegistrationMechanism::Dynamic,
            issuer_binding: Some(OAuthIssuer::new(issuer)?),
            client_id,
            authentication: OAuthClientAuthentication::Public,
            redirect_uris: vec![redirect_uri],
        })
    }

    /// Registration mechanism.
    #[must_use]
    pub const fn mechanism(&self) -> OAuthClientRegistrationMechanism {
        self.mechanism
    }

    /// Public client identifier.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Exact issuer binding for pre-registered and DCR credentials.
    #[must_use]
    pub fn issuer_binding(&self) -> Option<&str> {
        self.issuer_binding.as_ref().map(OAuthIssuer::as_str)
    }

    /// Registered redirect URIs.
    #[must_use]
    pub fn redirect_uris(&self) -> &[OAuthRedirectUri] {
        &self.redirect_uris
    }

    pub(crate) fn authentication_method(&self) -> &'static str {
        self.authentication.method()
    }

    pub(crate) fn client_secret(&self) -> Option<&str> {
        match &self.authentication {
            OAuthClientAuthentication::Public => None,
            OAuthClientAuthentication::SecretPost(secret)
            | OAuthClientAuthentication::SecretBasic(secret) => Some(secret.expose()),
        }
    }

    fn ensure_issuer(&self, issuer: &str) -> Result<(), OAuthFault> {
        if self.issuer_binding().is_some_and(|bound| bound != issuer) {
            return Err(OAuthFault::ClientIssuerMismatch);
        }
        Ok(())
    }

    fn ensure_redirect(&self, redirect: &OAuthRedirectUri) -> Result<(), OAuthFault> {
        if self.redirect_uris.iter().any(|row| row == redirect) {
            Ok(())
        } else {
            Err(OAuthFault::RedirectMismatch)
        }
    }
}

/// Select pre-registration, CIMD or DCR in the current MCP priority order.
///
/// # Errors
/// Issuer/redirect substitution, unavailable registration, cancellation,
/// transport failure or a malformed/rejected DCR response.
pub async fn resolve_client_registration(
    transport: &dyn HttpTransport,
    server: &OAuthAuthorizationServerMetadata,
    pre_registered: Option<OAuthClientRegistration>,
    cimd: Option<OAuthClientMetadataDocument>,
    dynamic: &OAuthDynamicClientMetadata,
    cancellation: CancellationToken,
) -> Result<OAuthClientRegistration, OAuthFault> {
    if let Some(pre_registered) = pre_registered {
        pre_registered.ensure_issuer(server.issuer())?;
        pre_registered.ensure_redirect(dynamic.redirect_uri())?;
        return Ok(pre_registered);
    }
    if server.client_id_metadata_document_supported()
        && let Some(cimd) = cimd
    {
        let registration = OAuthClientRegistration::from_cimd(cimd);
        registration.ensure_redirect(dynamic.redirect_uri())?;
        return Ok(registration);
    }
    let Some(endpoint) = server.registration_endpoint() else {
        return Err(OAuthFault::ClientRegistrationUnavailable);
    };
    register_dynamic(transport, endpoint, server.issuer(), dynamic, cancellation).await
}

async fn register_dynamic(
    transport: &dyn HttpTransport,
    endpoint: &str,
    issuer: &str,
    metadata: &OAuthDynamicClientMetadata,
    cancellation: CancellationToken,
) -> Result<OAuthClientRegistration, OAuthFault> {
    if cancellation.is_cancelled() {
        return Err(OAuthFault::Cancelled);
    }
    let body = serde_json::to_vec(&metadata.wire_value())
        .map_err(|_| OAuthFault::InvalidClientMetadata)?;
    let request = HttpRequest::post(endpoint, body)
        .and_then(|request| request.header("content-type", "application/json"))
        .and_then(|request| request.header("accept", "application/json"))
        .map(|request| request.with_max_response_bytes(MAX_METADATA_BYTES))
        .map_err(|_| OAuthFault::Unreachable)?;
    let response = transport
        .send(request, cancellation)
        .await
        .map_err(|_| OAuthFault::Unreachable)?;
    if !(200..300).contains(&response.status) {
        return Err(OAuthFault::ClientRegistrationRejected);
    }
    ensure_json_response(&response).map_err(|_| OAuthFault::MalformedClientRegistration)?;
    let document = serde_json::from_slice::<serde_json::Value>(&response.body)
        .map_err(|_| OAuthFault::MalformedClientRegistration)?;
    parse_dynamic_response(issuer, metadata, &document)
}

fn parse_dynamic_response(
    issuer: &str,
    requested: &OAuthDynamicClientMetadata,
    value: &serde_json::Value,
) -> Result<OAuthClientRegistration, OAuthFault> {
    let object = value
        .as_object()
        .ok_or(OAuthFault::MalformedClientRegistration)?;
    let client_id = object
        .get("client_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| safe_text(value, MAX_CLIENT_ID_BYTES))
        .ok_or(OAuthFault::MalformedClientRegistration)?;
    if object.contains_key("client_secret")
        || object
            .get("token_endpoint_auth_method")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value != "none")
        || object
            .get("application_type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value != "native")
    {
        return Err(OAuthFault::MalformedClientRegistration);
    }
    let redirects = string_array(
        object.get("redirect_uris"),
        MAX_REDIRECT_URIS,
        MAX_URL_BYTES,
        OAuthFault::MalformedClientRegistration,
    )?;
    if !redirects
        .iter()
        .any(|redirect| redirect == requested.redirect_uri.as_str())
    {
        return Err(OAuthFault::RedirectMismatch);
    }
    if object.contains_key("grant_types") {
        require_array_member_with_fault(
            object,
            "grant_types",
            "authorization_code",
            OAuthFault::MalformedClientRegistration,
        )?;
    }
    if object.contains_key("response_types") {
        require_array_member_with_fault(
            object,
            "response_types",
            "code",
            OAuthFault::MalformedClientRegistration,
        )?;
    }
    OAuthClientRegistration::from_dynamic(
        issuer,
        client_id.to_owned(),
        requested.redirect_uri.clone(),
    )
}

/// Immutable binding among resource, issuer, endpoints, client and redirect.
#[derive(Clone)]
pub struct OAuthAuthorizationBinding {
    server: OAuthAuthorizationServerMetadata,
    registration: Arc<OAuthClientRegistration>,
    resource: OAuthResource,
    redirect_uri: OAuthRedirectUri,
}

impl std::fmt::Debug for OAuthAuthorizationBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthAuthorizationBinding")
            .field("issuer", &"[REDACTED]")
            .field("resource", &"[REDACTED]")
            .field("registration", &self.registration)
            .field("redirect", &"[REDACTED]")
            .finish()
    }
}

impl OAuthAuthorizationBinding {
    /// Bind one validated registration to one discovered authorization server.
    ///
    /// # Errors
    /// Issuer-bound credentials from another server or an unregistered
    /// redirect URI.
    pub fn new(
        server: OAuthAuthorizationServerMetadata,
        registration: OAuthClientRegistration,
        resource: OAuthResource,
        redirect_uri: OAuthRedirectUri,
    ) -> Result<Self, OAuthFault> {
        registration.ensure_issuer(server.issuer())?;
        registration.ensure_redirect(&redirect_uri)?;
        Ok(Self {
            server,
            registration: Arc::new(registration),
            resource,
            redirect_uri,
        })
    }

    /// Exact authorization-server issuer.
    #[must_use]
    pub fn issuer(&self) -> &str {
        self.server.issuer()
    }

    /// Authorization endpoint.
    #[must_use]
    pub fn authorization_endpoint(&self) -> &str {
        self.server.authorization_endpoint()
    }

    /// Token endpoint.
    #[must_use]
    pub fn token_endpoint(&self) -> &str {
        self.server.token_endpoint()
    }

    /// Public client identifier.
    #[must_use]
    pub fn client_id(&self) -> &str {
        self.registration.client_id()
    }

    /// Exact redirect URI.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        self.redirect_uri.as_str()
    }

    /// Exact resource parameter.
    #[must_use]
    pub fn resource(&self) -> &str {
        self.resource.as_str()
    }

    /// Whether an authorization callback must carry `iss`.
    #[must_use]
    pub const fn response_issuer_required(&self) -> bool {
        self.server.authorization_response_iss_parameter_supported()
    }

    pub(crate) fn authentication_method(&self) -> &'static str {
        self.registration.authentication_method()
    }

    pub(crate) fn client_secret(&self) -> Option<&str> {
        self.registration.client_secret()
    }

    pub(crate) fn same_exchange(&self, other: &Self) -> bool {
        self.server.issuer == other.server.issuer
            && self.server.token_endpoint == other.server.token_endpoint
            && self.resource == other.resource
            && self.redirect_uri == other.redirect_uri
            && Arc::ptr_eq(&self.registration, &other.registration)
    }
}

fn validate_issuer(value: &str) -> Result<(), OAuthFault> {
    validate_https_url(value, false).map_err(|_| OAuthFault::InvalidMetadata)
}

fn validate_https_url(value: &str, allow_query: bool) -> Result<(), ()> {
    if value.is_empty()
        || value.len() > MAX_URL_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(());
    }
    let parsed = url::Url::parse(value).map_err(|_| ())?;
    if parsed.scheme() != "https"
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || (!allow_query && parsed.query().is_some())
    {
        return Err(());
    }
    Ok(())
}

fn validate_cimd_url(value: &str) -> Result<(), OAuthFault> {
    validate_https_url(value, false).map_err(|_| OAuthFault::InvalidClientMetadata)?;
    let parsed = url::Url::parse(value).map_err(|_| OAuthFault::InvalidClientMetadata)?;
    if parsed.path().is_empty() || parsed.path() == "/" {
        return Err(OAuthFault::InvalidClientMetadata);
    }
    Ok(())
}

fn validate_client_id(value: &str) -> Result<(), OAuthFault> {
    if safe_text(value, MAX_CLIENT_ID_BYTES) {
        Ok(())
    } else {
        Err(OAuthFault::InvalidClientMetadata)
    }
}

fn safe_text(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn required_https_endpoint(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, OAuthFault> {
    let value = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(OAuthFault::InvalidMetadata)?;
    validate_https_url(value, true).map_err(|_| OAuthFault::InvalidMetadata)?;
    Ok(value.to_owned())
}

fn optional_bool(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    fault: OAuthFault,
) -> Result<Option<bool>, OAuthFault> {
    match object.get(field) {
        None => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(fault),
    }
}

fn string_array(
    value: Option<&serde_json::Value>,
    max_rows: usize,
    max_bytes: usize,
    fault: OAuthFault,
) -> Result<Vec<String>, OAuthFault> {
    let rows = value.and_then(serde_json::Value::as_array).ok_or(fault)?;
    if rows.is_empty() || rows.len() > max_rows {
        return Err(fault);
    }
    let mut seen = BTreeSet::new();
    let mut output = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(row) = row.as_str().filter(|row| safe_text(row, max_bytes)) else {
            return Err(fault);
        };
        if !seen.insert(row.to_owned()) {
            return Err(fault);
        }
        output.push(row.to_owned());
    }
    Ok(output)
}

fn require_array_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    expected: &str,
) -> Result<(), OAuthFault> {
    require_array_member_with_fault(object, field, expected, OAuthFault::InvalidClientMetadata)
}

fn require_array_member_with_fault(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    expected: &str,
    fault: OAuthFault,
) -> Result<(), OAuthFault> {
    let rows = string_array(object.get(field), MAX_METADATA_STRING_ROWS, 64, fault)?;
    if rows.iter().any(|row| row == expected) {
        Ok(())
    } else {
        Err(fault)
    }
}

//! Live check of a pasted Gemini API key.
//!
//! Google authenticates with `x-goog-api-key`, not a bearer token, so the
//! shared bearer validator cannot speak for it. Without this, `heycode setup`
//! stored a Gemini key on shape alone and the typo surfaced on the first
//! turn instead of at the prompt.

use async_trait::async_trait;
use heycode_authorization::AuthorizationFlowFailure;
use heycode_authorization_api_key::{ApiKeyValidationFailure, ApiKeyValidator};
use heycode_credentials::CredentialSecret;
use heycode_http::{HttpRequest, HttpService, ReqwestHttpTransport, TransportError};
use tokio_util::sync::CancellationToken;

/// Most bytes read from a validation response. The body is never inspected
/// beyond its status and type; a provider that answers with a novel is not a
/// reason to hold it in memory.
const VALIDATION_RESPONSE_LIMIT: usize = 64 * 1024;

/// Validates a Gemini API key against the model listing.
pub struct GoogleApiKeyValidator {
    http: HttpService,
    list_url: String,
}

impl GoogleApiKeyValidator {
    /// Validate against the official Generative Language origin.
    ///
    /// # Errors
    /// Transport construction failure keeps the safe host class.
    pub fn new() -> Result<Self, AuthorizationFlowFailure> {
        Self::with_base_url(crate::catalog::DEFAULT_BASE_URL)
    }

    /// Validate against an explicit base URL (a proxy or gateway).
    ///
    /// # Errors
    /// Transport construction or an unusable endpoint keeps the safe host
    /// class.
    pub fn with_base_url(base_url: &str) -> Result<Self, AuthorizationFlowFailure> {
        let transport = ReqwestHttpTransport::new().map_err(|_| host_failure())?;
        Self::with_transport(HttpService::new(std::sync::Arc::new(transport)), base_url)
    }

    /// Validate over an already-composed HTTP service (tests, embedders).
    ///
    /// # Errors
    /// An endpoint this build cannot form keeps the safe host class.
    pub fn with_transport(
        http: HttpService,
        base_url: &str,
    ) -> Result<Self, AuthorizationFlowFailure> {
        let list_url = format!("{}/v1/models?pageSize=1", base_url.trim_end_matches('/'));
        HttpRequest::get(&list_url).map_err(|_| host_failure())?;
        Ok(Self { http, list_url })
    }

    /// Endpoint this validator contacts.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.list_url
    }
}

fn host_failure() -> AuthorizationFlowFailure {
    AuthorizationFlowFailure::new("host", "validation endpoint or response is invalid")
}

#[async_trait]
impl ApiKeyValidator for GoogleApiKeyValidator {
    async fn validate(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        if cancellation.is_cancelled() {
            return Err(ApiKeyValidationFailure::Cancelled);
        }
        let request = HttpRequest::get(&self.list_url)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header("x-goog-api-key", secret.expose()))
            .map(|request| request.with_max_response_bytes(VALIDATION_RESPONSE_LIMIT))
            .map_err(|_| ApiKeyValidationFailure::Host)?;
        let response = self
            .http
            .send(request, cancellation)
            .await
            .map_err(map_transport_error)?;
        match response.status {
            200..=299 => Ok(()),
            // Google answers a rejected key with 400 as often as 401, and the
            // body says which; the body is not evidence this layer reads, so
            // both classify as unauthorized rather than as a broken endpoint.
            400..=403 => Err(ApiKeyValidationFailure::Unauthorized),
            429 | 500..=599 => Err(ApiKeyValidationFailure::Network),
            _ => Err(ApiKeyValidationFailure::Host),
        }
    }
}

fn map_transport_error(error: TransportError) -> ApiKeyValidationFailure {
    match error {
        TransportError::Cancelled => ApiKeyValidationFailure::Cancelled,
        TransportError::Http {
            status: 400..=403, ..
        } => ApiKeyValidationFailure::Unauthorized,
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            ApiKeyValidationFailure::Network
        }
        TransportError::Network { .. } | TransportError::Timeout => {
            ApiKeyValidationFailure::Network
        }
        _ => ApiKeyValidationFailure::Host,
    }
}

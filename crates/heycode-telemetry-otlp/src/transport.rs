//! Credential-resolving OTLP/HTTP JSON transport.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hash as _, Hasher as _};
use std::time::{Duration, SystemTime};

use heycode_credentials::{CredentialSecret, CredentialsService};
use heycode_http::{HttpRequest, HttpResponse, HttpRetryAfter, HttpService, TransportError};
use heycode_telemetry::{
    OtlpAuth, OtlpCancellation, OtlpEndpoint, OtlpPayload, OtlpSendFault, OtlpTransport,
};
use tokio_util::sync::CancellationToken;

use crate::{OtlpAuthScheme, OtlpHttpConfig};

const CONTENT_TYPE_JSON: &str = "application/json";
const USER_AGENT: &str = concat!(
    "OTel-OTLP-Exporter-Rust/",
    env!("CARGO_PKG_VERSION"),
    " heycode/",
    env!("CARGO_PKG_VERSION")
);
const CANCELLATION_POLL: Duration = Duration::from_millis(5);

/// Production OTLP/HTTP JSON transport over the shared HTTP and credential
/// services.
pub struct HttpOtlpTransport {
    config: OtlpHttpConfig,
    http: HttpService,
    credentials: CredentialsService,
    authentication: OtlpAuth,
}

impl HttpOtlpTransport {
    /// Bind validated settings to the composed HTTP and credential services.
    #[must_use]
    pub fn new(config: OtlpHttpConfig, http: HttpService, credentials: CredentialsService) -> Self {
        let authentication =
            config
                .auth()
                .map_or(OtlpAuth::NoneConfigured, |auth| OtlpAuth::Configured {
                    header: auth.header().clone(),
                    credential: auth.credential_label().clone(),
                });
        Self {
            config,
            http,
            credentials,
            authentication,
        }
    }

    fn resolve_header(&self) -> Result<Option<String>, OtlpSendFault> {
        let Some(auth) = self.config.auth() else {
            return Ok(None);
        };
        let secret = self
            .credentials
            .resolve(auth.query())
            .map_err(|_| OtlpSendFault::Refused)?
            .ok_or(OtlpSendFault::Refused)?;
        if secret.expose().is_empty() {
            return Err(OtlpSendFault::Refused);
        }
        Ok(Some(header_value(auth.scheme(), &secret)))
    }

    fn send_payload(
        &self,
        payload: &OtlpPayload,
        cancellation: &OtlpCancellation,
    ) -> Result<(), OtlpSendFault> {
        if cancellation.is_cancelled() {
            return Err(OtlpSendFault::Cancelled);
        }
        if payload.data_point_count() == 0 {
            return Err(OtlpSendFault::Malformed);
        }
        let body = serde_json::to_vec(payload).map_err(|_| OtlpSendFault::Malformed)?;
        if body.len() > self.config.max_request_bytes() {
            return Err(OtlpSendFault::Malformed);
        }
        let header = self.resolve_header()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| OtlpSendFault::Unreachable)?;
        runtime.block_on(send_with_retries(
            &self.http,
            &self.config,
            body,
            header,
            cancellation.clone(),
        ))
    }
}

impl OtlpTransport for HttpOtlpTransport {
    fn destination(&self) -> &OtlpEndpoint {
        self.config.endpoint()
    }

    fn authentication(&self) -> &OtlpAuth {
        &self.authentication
    }

    fn send(
        &self,
        payload: &OtlpPayload,
        cancellation: &OtlpCancellation,
    ) -> Result<(), OtlpSendFault> {
        self.send_payload(payload, cancellation)
    }
}

impl std::fmt::Debug for HttpOtlpTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpOtlpTransport")
            .field("destination", &self.config.endpoint().as_address())
            .field("authenticated", &self.authentication.configured())
            .field("protocol", &"http/json")
            .field("timeout_ms", &self.config.timeout().as_millis())
            .field("max_attempts", &self.config.max_attempts())
            .finish()
    }
}

fn header_value(scheme: OtlpAuthScheme, secret: &CredentialSecret) -> String {
    match scheme {
        OtlpAuthScheme::Raw => secret.expose().to_owned(),
        OtlpAuthScheme::Bearer => format!("Bearer {}", secret.expose()),
    }
}

async fn send_with_retries(
    http: &HttpService,
    config: &OtlpHttpConfig,
    body: Vec<u8>,
    auth_header: Option<String>,
    cancellation: OtlpCancellation,
) -> Result<(), OtlpSendFault> {
    for attempt in 1..=config.max_attempts() {
        let request = build_request(config, body.clone(), auth_header.as_deref())?;
        match send_attempt(http, config, request, cancellation.clone()).await {
            AttemptOutcome::Accepted => return Ok(()),
            AttemptOutcome::Failed(failure) => {
                if !failure.retryable || attempt == config.max_attempts() {
                    return Err(failure.fault);
                }
                let delay = failure.retry_after.unwrap_or_else(|| {
                    jittered_backoff(config.initial_backoff(), config.max_backoff(), attempt)
                });
                if wait_or_cancel(delay, cancellation.clone()).await {
                    return Err(OtlpSendFault::Cancelled);
                }
            }
        }
    }
    Err(OtlpSendFault::Unreachable)
}

fn build_request(
    config: &OtlpHttpConfig,
    body: Vec<u8>,
    auth_header: Option<&str>,
) -> Result<HttpRequest, OtlpSendFault> {
    let mut request = HttpRequest::post(config.endpoint().as_address(), body)
        .and_then(|request| request.header("content-type", CONTENT_TYPE_JSON))
        .and_then(|request| request.header("accept", CONTENT_TYPE_JSON))
        .and_then(|request| request.header("user-agent", USER_AGENT))
        .map_err(|_| OtlpSendFault::Malformed)?
        .with_max_response_bytes(config.max_response_bytes());
    if let (Some(auth), Some(value)) = (config.auth(), auth_header) {
        request = request
            .header(auth.header().as_str(), value)
            .map_err(|_| OtlpSendFault::Malformed)?;
    }
    Ok(request)
}

async fn send_attempt(
    http: &HttpService,
    config: &OtlpHttpConfig,
    request: HttpRequest,
    cancellation: OtlpCancellation,
) -> AttemptOutcome {
    let operation_token = CancellationToken::new();
    let mut operation = http.send(request, operation_token.clone());
    let cancelled = wait_for_cancellation(cancellation);
    let timeout = tokio::time::sleep(config.timeout());
    tokio::pin!(cancelled);
    tokio::pin!(timeout);
    tokio::select! {
        biased;
        () = &mut cancelled => {
            operation_token.cancel();
            AttemptOutcome::failed(OtlpSendFault::Cancelled, false, None)
        }
        () = &mut timeout => {
            operation_token.cancel();
            AttemptOutcome::failed(OtlpSendFault::Unreachable, true, None)
        }
        result = operation.as_mut() => classify_transport_result(result, config.max_backoff()),
    }
}

async fn wait_for_cancellation(cancellation: OtlpCancellation) {
    loop {
        if cancellation.is_cancelled() {
            return;
        }
        tokio::time::sleep(CANCELLATION_POLL).await;
    }
}

async fn wait_or_cancel(delay: Duration, cancellation: OtlpCancellation) -> bool {
    let cancelled = wait_for_cancellation(cancellation);
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(cancelled);
    tokio::pin!(sleep);
    tokio::select! {
        biased;
        () = &mut cancelled => true,
        () = &mut sleep => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptOutcome {
    Accepted,
    Failed(AttemptFailure),
}

impl AttemptOutcome {
    const fn failed(fault: OtlpSendFault, retryable: bool, retry_after: Option<Duration>) -> Self {
        Self::Failed(AttemptFailure {
            fault,
            retryable,
            retry_after,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttemptFailure {
    fault: OtlpSendFault,
    retryable: bool,
    retry_after: Option<Duration>,
}

fn classify_transport_result(
    result: Result<HttpResponse, TransportError>,
    max_backoff: Duration,
) -> AttemptOutcome {
    match result {
        Ok(response) => classify_response(&response, max_backoff),
        Err(TransportError::Cancelled) => {
            AttemptOutcome::failed(OtlpSendFault::Cancelled, false, None)
        }
        Err(TransportError::Network { .. } | TransportError::Timeout) => {
            AttemptOutcome::failed(OtlpSendFault::Unreachable, true, None)
        }
        Err(TransportError::Http {
            status, metadata, ..
        }) => classify_status(status, retry_delay(metadata.retry_after(), max_backoff)),
        Err(
            TransportError::InvalidRequest { .. }
            | TransportError::InvalidSse { .. }
            | TransportError::ResponseTooLarge { .. },
        ) => AttemptOutcome::failed(OtlpSendFault::Malformed, false, None),
        Err(_) => AttemptOutcome::failed(OtlpSendFault::Refused, false, None),
    }
}

fn classify_response(response: &HttpResponse, max_backoff: Duration) -> AttemptOutcome {
    if response.status != 200 {
        return classify_status(
            response.status,
            retry_delay_from_header(response.header("retry-after"), max_backoff),
        );
    }
    if response.content_type.as_deref() != Some(CONTENT_TYPE_JSON) {
        return AttemptOutcome::failed(OtlpSendFault::Malformed, false, None);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&response.body) else {
        return AttemptOutcome::failed(OtlpSendFault::Malformed, false, None);
    };
    let Some(object) = value.as_object() else {
        return AttemptOutcome::failed(OtlpSendFault::Malformed, false, None);
    };
    if object.contains_key("partialSuccess") {
        return AttemptOutcome::failed(OtlpSendFault::Refused, false, None);
    }
    AttemptOutcome::Accepted
}

fn classify_status(status: u16, retry_after: Option<Duration>) -> AttemptOutcome {
    match status {
        429 | 503 => AttemptOutcome::failed(OtlpSendFault::Throttled, true, retry_after),
        502 | 504 => AttemptOutcome::failed(OtlpSendFault::Unreachable, true, retry_after),
        400 => AttemptOutcome::failed(OtlpSendFault::Malformed, false, None),
        _ => AttemptOutcome::failed(OtlpSendFault::Refused, false, None),
    }
}

fn retry_delay(advice: Option<HttpRetryAfter>, max_backoff: Duration) -> Option<Duration> {
    advice.map(|advice| match advice {
        HttpRetryAfter::Delay(delay) => delay.min(max_backoff),
        HttpRetryAfter::At(at) => at
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
            .min(max_backoff),
    })
}

fn retry_delay_from_header(value: Option<&str>, max_backoff: Duration) -> Option<Duration> {
    let value = value?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(max_backoff));
    }
    httpdate::parse_http_date(value)
        .ok()
        .and_then(|at| retry_delay(Some(HttpRetryAfter::At(at)), max_backoff))
}

fn jittered_backoff(initial: Duration, maximum: Duration, attempt: usize) -> Duration {
    let shift = u32::try_from(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX)
        .min(20);
    let ceiling_ms = u64::try_from(initial.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_mul(1_u64 << shift)
        .min(u64::try_from(maximum.as_millis()).unwrap_or(u64::MAX));
    let floor_ms = ceiling_ms / 2;
    let span = ceiling_ms.saturating_sub(floor_ms).saturating_add(1);
    let mut hasher = RandomState::new().build_hasher();
    attempt.hash(&mut hasher);
    SystemTime::now().hash(&mut hasher);
    let jitter_ms = hasher.finish() % span;
    Duration::from_millis(floor_ms.saturating_add(jitter_ms))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn response(status: u16, body: &[u8]) -> HttpResponse {
        HttpResponse {
            status,
            content_type: Some(CONTENT_TYPE_JSON.to_owned()),
            headers: Default::default(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn only_exact_200_json_without_partial_success_is_accepted() {
        assert_eq!(
            classify_response(&response(200, b"{}"), Duration::from_secs(1)),
            AttemptOutcome::Accepted
        );
        assert_eq!(
            classify_response(
                &response(
                    200,
                    br#"{"partialSuccess":{"rejectedDataPoints":"1","errorMessage":"secret"}}"#,
                ),
                Duration::from_secs(1),
            ),
            AttemptOutcome::failed(OtlpSendFault::Refused, false, None)
        );
        let mut wrong_type = response(200, b"{}");
        wrong_type.content_type = Some("text/plain".to_owned());
        assert_eq!(
            classify_response(&wrong_type, Duration::from_secs(1)),
            AttemptOutcome::failed(OtlpSendFault::Malformed, false, None)
        );
    }

    #[test]
    fn retryable_statuses_are_the_official_otlp_http_set() {
        for status in [429, 502, 503, 504] {
            let AttemptOutcome::Failed(failure) = classify_status(status, None) else {
                panic!("status {status} was accepted")
            };
            assert!(failure.retryable, "{status}");
        }
        for status in [400, 401, 403, 404, 500, 501, 505] {
            let AttemptOutcome::Failed(failure) = classify_status(status, None) else {
                panic!("status {status} was accepted")
            };
            assert!(!failure.retryable, "{status}");
        }
    }

    #[test]
    fn retry_after_and_jitter_never_exceed_the_configured_bound() {
        let maximum = Duration::from_millis(50);
        assert_eq!(retry_delay_from_header(Some("999"), maximum), Some(maximum));
        for attempt in 1..=10 {
            let delay = jittered_backoff(Duration::from_millis(10), maximum, attempt);
            assert!(delay <= maximum, "{attempt}: {delay:?}");
        }
    }

    #[test]
    fn debug_has_no_service_or_credential_internals() {
        let config = OtlpHttpConfig::from_value(&crate::config::default_value()).unwrap();
        let rendered = format!(
            "{:?}",
            HttpOtlpTransport::new(
                config,
                HttpService::new(Arc::new(NoHttp)),
                CredentialsService::new(),
            )
        );
        assert!(rendered.contains("http/json"), "{rendered}");
        assert!(!rendered.contains("CredentialsService"), "{rendered}");
        assert!(!rendered.contains("HttpService"), "{rendered}");
    }

    struct NoHttp;

    impl heycode_http::HttpTransport for NoHttp {
        fn sse(
            &self,
            _request: heycode_http::HttpSseRequest,
            _cancellation: CancellationToken,
        ) -> heycode_http::SseEventStream {
            Box::pin(futures::stream::empty())
        }
    }
}

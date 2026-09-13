//! Reqwest-backed raw HTTP/SSE transport.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::{Stream, StreamExt as _};
use tokio_util::sync::CancellationToken;

use crate::{SseDecodeError, SseDecoder, SseEvent};

const ERROR_TAIL_CHARS: usize = 2_048;
const ERROR_TAIL_BYTES: usize = 16 * 1024;
const ERROR_BODY_MAX_BYTES: usize = 1024 * 1024;
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(1);
const ERROR_HEADER_BYTES: usize = 128;
const MAX_HTTP_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Scheme family named by the buffered/SSE request builders' rejection.
pub(crate) const HTTP_SCHEME_LABEL: &str = "HTTP(S)";

/// Parse one absolute endpoint and reject anything a transport must not dial:
/// a relative URL, a foreign scheme, a hostless authority, or userinfo, which
/// would smuggle a credential into a URL that error paths and telemetry may
/// legitimately render. `schemes` and `label` are the only per-family inputs,
/// so every request type in this crate rejects identically and one audit
/// covers all of them.
pub(crate) fn validated_endpoint(
    url: &str,
    schemes: &[&str],
    label: &str,
) -> Result<reqwest::Url, TransportError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| TransportError::InvalidRequest {
        field: "url",
        message: "URL must be absolute".to_owned(),
    })?;
    if !schemes.contains(&parsed.scheme())
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(TransportError::InvalidRequest {
            field: "url",
            message: format!("URL must be host-qualified {label} without embedded credentials"),
        });
    }
    Ok(parsed)
}

/// Parsed semantic `Retry-After` response advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpRetryAfter {
    /// Delay relative to receipt of the response.
    Delay(Duration),
    /// Absolute HTTP-date.
    At(SystemTime),
}

/// Bounded semantic facts from an HTTP error response. Raw header text is
/// never retained.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpErrorMetadata {
    retry_after: Option<HttpRetryAfter>,
    should_retry: Option<bool>,
}

impl HttpErrorMetadata {
    /// Build metadata for an external/test transport from parsed facts.
    #[must_use]
    pub const fn new(retry_after: Option<HttpRetryAfter>, should_retry: Option<bool>) -> Self {
        Self {
            retry_after,
            should_retry,
        }
    }

    /// Provider-standard retry delay/date when valid.
    #[must_use]
    pub const fn retry_after(&self) -> Option<HttpRetryAfter> {
        self.retry_after
    }

    /// Explicit provider retry approval/veto when present.
    #[must_use]
    pub const fn should_retry(&self) -> Option<bool> {
        self.should_retry
    }
}

/// Explicitly accessed bounded HTTP error body. Ordinary Debug/Display never
/// expose provider response bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpErrorBody(String);

impl HttpErrorBody {
    /// Expose the bounded body to a protocol classifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn new(value: String) -> Self {
        Self(bound_error_text(value))
    }
}

impl std::fmt::Debug for HttpErrorBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HttpErrorBody([REDACTED])")
    }
}

/// Raw SSE event stream.
pub type SseEventStream = Pin<Box<dyn Stream<Item = Result<SseEvent, TransportError>> + Send>>;
/// One bounded buffered HTTP operation.
pub type BufferedResponseFuture =
    Pin<Box<dyn std::future::Future<Output = Result<HttpResponse, TransportError>> + Send>>;
/// Pull-driven chunks from one dynamically typed HTTP response.
pub type HttpBodyStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, TransportError>> + Send>>;
/// One response-head acquisition operation.
pub type StreamingResponseFuture = Pin<
    Box<dyn std::future::Future<Output = Result<HttpStreamingResponse, TransportError>> + Send>,
>;

/// Response head plus a single-owner, pull-driven body.
///
/// Header values and body bytes deliberately stay out of `Debug`. Dropping the
/// body drops the underlying response; no background task owns or drains it.
pub struct HttpStreamingResponse {
    status: u16,
    content_type: Option<String>,
    headers: BTreeMap<String, String>,
    body: HttpBodyStream,
}

impl HttpStreamingResponse {
    /// HTTP status observed before body polling.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Parsed response content type, without parameters.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// Case-insensitive bounded response-header lookup.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Pull the body under caller-owned backpressure.
    #[must_use = "the response body advances only while it is polled"]
    pub fn body_mut(&mut self) -> &mut HttpBodyStream {
        &mut self.body
    }
}

impl std::fmt::Debug for HttpStreamingResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpStreamingResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Response headers observed for one SSE exchange.
///
/// The headers arrive only when the response does, which is after the stream
/// is constructed, so this is a slot the driver fills once. `None` means the
/// headers are not yet known **or** the transport cannot report them — it never
/// means the response had no headers, because an empty map would read as fact.
#[derive(Clone, Debug, Default)]
pub struct SseResponseHeaders(Arc<std::sync::OnceLock<BTreeMap<String, String>>>);

impl SseResponseHeaders {
    /// A slot no transport will ever fill.
    #[must_use]
    pub fn unavailable() -> Self {
        let slot = Self::default();
        // Deliberately left unset: an unfillable slot and a not-yet-filled one
        // are the same observable state, which is exactly right — both mean
        // "unknown", and neither may be mistaken for "no headers".
        slot
    }

    /// Headers once the response has arrived, or `None` while unknown.
    #[must_use]
    pub fn get(&self) -> Option<&BTreeMap<String, String>> {
        self.0.get()
    }

    /// Look up one header by case-insensitive name, once known.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.get()?
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn publish(&self, headers: BTreeMap<String, String>) {
        let _first = self.0.set(headers);
    }
}

/// One SSE exchange: the event stream plus the response headers it will carry.
pub struct SseExchange {
    /// Bounded response headers, filled when the response arrives.
    pub headers: SseResponseHeaders,
    /// The decoded event stream.
    pub events: SseEventStream,
}

/// Provider-neutral transport failures.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// Request URL/header/body metadata was invalid before I/O.
    #[error("invalid HTTP request field `{field}`: {message}")]
    InvalidRequest {
        /// Stable field name.
        field: &'static str,
        /// Safe actionable detail.
        message: String,
    },
    /// DNS/connect/TLS/body-read failure.
    #[error("HTTP transport failed: {message}")]
    Network {
        /// Safe transport diagnostic.
        message: String,
    },
    /// Request or response I/O crossed the configured deadline.
    #[error("HTTP request timed out")]
    Timeout,
    /// Non-success response with an explicitly accessed bounded body and safe
    /// semantic metadata.
    #[error("HTTP {status}")]
    Http {
        /// Status code.
        status: u16,
        /// Last bounded response characters, redacted from Debug/Display.
        body: HttpErrorBody,
        /// Parsed bounded response advice.
        metadata: HttpErrorMetadata,
    },
    /// SSE framing failed before provider payload interpretation.
    #[error("invalid SSE framing: {message}")]
    InvalidSse {
        /// Safe framing detail.
        message: String,
    },
    /// Operation cancellation won before the next commit/yield.
    #[error("HTTP request cancelled")]
    Cancelled,
    /// Buffered response crossed the caller's cap.
    #[error("HTTP response exceeds the {max_bytes}-byte limit")]
    ResponseTooLarge {
        /// Caller-provided cap.
        max_bytes: usize,
    },
}

impl TransportError {
    /// Build a non-success response from bounded raw body text and parsed
    /// semantic metadata.
    #[must_use]
    pub fn http(status: u16, body: impl Into<String>, metadata: HttpErrorMetadata) -> Self {
        Self::Http {
            status,
            body: HttpErrorBody::new(body.into()),
            metadata,
        }
    }
}

/// HTTP method supported by the current raw SSE operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// DELETE. Bodyless; used by protocols that terminate a server-assigned
    /// session explicitly.
    Delete,
}

/// One validated request header. Deliberately has no Debug implementation
/// because `value` may contain a credential.
#[derive(Clone)]
pub struct HttpHeader {
    name: reqwest::header::HeaderName,
    value: reqwest::header::HeaderValue,
}

impl HttpHeader {
    /// Validate one name/value pair without exposing either in the error.
    pub(crate) fn new(name: &str, value: &str) -> Result<Self, TransportError> {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            TransportError::InvalidRequest {
                field: "headers",
                message: "header name is invalid".to_owned(),
            }
        })?;
        let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
            TransportError::InvalidRequest {
                field: "headers",
                message: "header value is invalid".to_owned(),
            }
        })?;
        Ok(Self { name, value })
    }

    /// Normalized lowercase header name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Explicitly expose the header value to a transport implementation.
    #[must_use]
    pub fn value(&self) -> &str {
        self.value.to_str().unwrap_or("")
    }
}

/// Opaque HTTP request bytes for an SSE response. Deliberately has no Debug
/// implementation because headers may contain authorization values.
pub struct HttpSseRequest {
    method: HttpMethod,
    url: reqwest::Url,
    headers: Vec<HttpHeader>,
    body: Option<Vec<u8>>,
}

impl HttpSseRequest {
    /// Build a GET request.
    ///
    /// # Errors
    /// URL must be absolute HTTP(S), host-qualified and credential-free.
    pub fn get(url: impl AsRef<str>) -> Result<Self, TransportError> {
        Self::build(HttpMethod::Get, url.as_ref(), None)
    }

    /// Build a POST request with opaque body bytes.
    ///
    /// # Errors
    /// URL must be absolute HTTP(S), host-qualified and credential-free.
    pub fn post(url: impl AsRef<str>, body: Vec<u8>) -> Result<Self, TransportError> {
        Self::build(HttpMethod::Post, url.as_ref(), Some(body))
    }

    fn build(method: HttpMethod, url: &str, body: Option<Vec<u8>>) -> Result<Self, TransportError> {
        let url = validated_endpoint(url, &["http", "https"], HTTP_SCHEME_LABEL)?;
        Ok(Self {
            method,
            url,
            headers: Vec::new(),
            body,
        })
    }

    /// Append one validated header without exposing its value in diagnostics.
    ///
    /// # Errors
    /// Invalid header name/value bytes.
    pub fn header(mut self, name: &str, value: &str) -> Result<Self, TransportError> {
        self.headers.push(HttpHeader::new(name, value)?);
        Ok(self)
    }

    /// Request method.
    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    /// Validated absolute URL.
    #[must_use]
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Validated headers. Reading values is an explicit exposure operation.
    #[must_use]
    pub fn headers(&self) -> &[HttpHeader] {
        &self.headers
    }

    /// Opaque request body bytes.
    #[must_use]
    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }

    fn into_request(self, client: &reqwest::Client) -> Result<reqwest::Request, TransportError> {
        let mut builder = match self.method {
            HttpMethod::Get => client.get(self.url),
            HttpMethod::Post => client.post(self.url),
            HttpMethod::Delete => client.delete(self.url),
        };
        for header in self.headers {
            builder = builder.header(header.name, header.value);
        }
        if let Some(body) = self.body {
            builder = builder.body(body);
        }
        builder.build().map_err(|_| TransportError::InvalidRequest {
            field: "request",
            message: "HTTP request could not be constructed".to_owned(),
        })
    }
}

/// Bounded buffered HTTP request. Deliberately has no Debug implementation
/// because headers/body may contain credentials or private provider data.
pub struct HttpRequest {
    method: HttpMethod,
    url: reqwest::Url,
    headers: Vec<HttpHeader>,
    body: Option<Vec<u8>>,
    max_response_bytes: usize,
}

impl HttpRequest {
    /// Build a GET request with a four-MiB response cap.
    ///
    /// # Errors
    /// URL must be absolute host-qualified HTTP(S) without credentials.
    pub fn get(url: impl AsRef<str>) -> Result<Self, TransportError> {
        Self::build(HttpMethod::Get, url.as_ref(), None)
    }

    /// Build a POST request with opaque bytes and a four-MiB response cap.
    ///
    /// # Errors
    /// URL must be absolute host-qualified HTTP(S) without credentials.
    pub fn post(url: impl AsRef<str>, body: Vec<u8>) -> Result<Self, TransportError> {
        Self::build(HttpMethod::Post, url.as_ref(), Some(body))
    }

    /// Build a bodyless DELETE request with a four-MiB response cap.
    ///
    /// # Errors
    /// URL must be absolute host-qualified HTTP(S) without credentials.
    pub fn delete(url: impl AsRef<str>) -> Result<Self, TransportError> {
        Self::build(HttpMethod::Delete, url.as_ref(), None)
    }

    fn build(method: HttpMethod, url: &str, body: Option<Vec<u8>>) -> Result<Self, TransportError> {
        let url = validated_endpoint(url, &["http", "https"], HTTP_SCHEME_LABEL)?;
        Ok(Self {
            method,
            url,
            headers: Vec::new(),
            body,
            max_response_bytes: 4 * 1024 * 1024,
        })
    }

    /// Append one validated header without exposing its value in errors.
    ///
    /// # Errors
    /// Invalid header name/value bytes.
    pub fn header(mut self, name: &str, value: &str) -> Result<Self, TransportError> {
        self.headers.push(HttpHeader::new(name, value)?);
        Ok(self)
    }

    /// Set the maximum accepted response body bytes.
    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// Request method.
    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    /// Validated absolute URL.
    #[must_use]
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Validated headers; values require explicit access.
    #[must_use]
    pub fn headers(&self) -> &[HttpHeader] {
        &self.headers
    }

    /// Opaque request body bytes.
    #[must_use]
    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }

    fn into_request(
        self,
        client: &reqwest::Client,
    ) -> Result<(reqwest::Request, usize), TransportError> {
        let mut builder = match self.method {
            HttpMethod::Get => client.get(self.url),
            HttpMethod::Post => client.post(self.url),
            HttpMethod::Delete => client.delete(self.url),
        };
        for header in self.headers {
            builder = builder.header(header.name, header.value);
        }
        if let Some(body) = self.body {
            builder = builder.body(body);
        }
        let request = builder
            .build()
            .map_err(|_| TransportError::InvalidRequest {
                field: "request",
                message: "HTTP request could not be constructed".to_owned(),
            })?;
        Ok((request, self.max_response_bytes))
    }
}

/// Largest number of response headers retained.
const MAX_RESPONSE_HEADERS: usize = 64;
/// Largest retained response-header value.
const MAX_RESPONSE_HEADER_BYTES: usize = 4 * 1024;

/// Bounded buffered HTTP response. All status codes return here for
/// provider-owned classification.
///
/// `Debug` is implemented by hand and deliberately renders no header value and
/// no body byte. A server controls both: response headers routinely carry
/// session identity (`Set-Cookie`, `Mcp-Session-Id`) and bodies carry private
/// data, so a derived `Debug` would put them into any log or panic message
/// that ever formats a response.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status.
    pub status: u16,
    /// Media type without parameters when present and valid.
    pub content_type: Option<String>,
    /// Bounded lowercase response headers, for protocols whose identity lives
    /// in a header rather than the body. Server-supplied and never a heycode
    /// secret, but still bounded in count and size, and non-UTF-8 or oversized
    /// values are dropped rather than truncated into something misleading.
    pub headers: std::collections::BTreeMap<String, String>,
    /// Complete body within the request cap.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl HttpResponse {
    /// Look up one response header by case-insensitive name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

fn bounded_response_headers(
    headers: &reqwest::header::HeaderMap,
) -> std::collections::BTreeMap<String, String> {
    let mut retained = std::collections::BTreeMap::new();
    for (name, value) in headers {
        if retained.len() >= MAX_RESPONSE_HEADERS {
            break;
        }
        let Ok(value) = value.to_str() else {
            continue;
        };
        if value.len() > MAX_RESPONSE_HEADER_BYTES {
            continue;
        }
        retained.insert(name.as_str().to_ascii_lowercase(), value.to_owned());
    }
    retained
}

/// Raw HTTP/SSE operation boundary.
pub trait HttpTransport: Send + Sync {
    /// Send one bounded buffered request. The default keeps test/external
    /// transports source-compatible until they opt into discovery calls.
    fn send(
        &self,
        _request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        Box::pin(async {
            Err(TransportError::InvalidRequest {
                field: "transport",
                message: "buffered HTTP is not implemented by this transport".to_owned(),
            })
        })
    }

    /// Send one request and return its response head before consuming its body.
    ///
    /// The default keeps external/test transports compatible by adapting their
    /// bounded buffered response into one body chunk. Transports that can
    /// genuinely stream override this method; callers can use the same API in
    /// either case without mistaking a buffered adapter for an empty stream.
    fn stream_response(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> StreamingResponseFuture {
        let response = self.send(request, cancellation);
        Box::pin(async move {
            let response = response.await?;
            let body: HttpBodyStream = if response.body.is_empty() {
                Box::pin(futures::stream::empty())
            } else {
                Box::pin(futures::stream::once(async move { Ok(response.body) }))
            };
            Ok(HttpStreamingResponse {
                status: response.status,
                content_type: response.content_type,
                headers: response.headers,
                body,
            })
        })
    }

    /// Send one request and yield provider-opaque SSE events.
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream;

    /// Run one SSE exchange and additionally expose its response headers.
    ///
    /// The default discards nothing it could have reported: it returns an
    /// `unavailable` slot, so a transport that cannot surface headers says
    /// "unknown" rather than "none". Only a transport that genuinely observes
    /// them overrides this.
    fn sse_exchange(
        &self,
        request: HttpSseRequest,
        cancellation: CancellationToken,
    ) -> SseExchange {
        SseExchange {
            headers: SseResponseHeaders::unavailable(),
            events: self.sse(request, cancellation),
        }
    }
}

/// Shared service wrapper around one HTTP transport implementation.
#[derive(Clone)]
pub struct HttpService(Arc<dyn HttpTransport>);

impl HttpService {
    /// Wrap one transport implementation.
    #[must_use]
    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        Self(transport)
    }

    /// Send one raw SSE request.
    #[must_use = "the request runs only while the stream is polled"]
    pub fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        self.0.sse(request, cancellation)
    }

    /// Run one SSE exchange and additionally observe its response headers.
    ///
    /// Consumers that read rate-limit or quota headers use this; the plain
    /// [`Self::sse`] stays the path for adapters that only decode events.
    #[must_use]
    pub fn sse_exchange(
        &self,
        request: HttpSseRequest,
        cancellation: CancellationToken,
    ) -> SseExchange {
        self.0.sse_exchange(request, cancellation)
    }

    /// Send one bounded buffered request.
    #[must_use = "the request runs only while the future is awaited"]
    pub fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.0.send(request, cancellation)
    }

    /// Acquire one response head and retain pull ownership of its body.
    #[must_use = "the request runs only while the future/body are polled"]
    pub fn stream_response(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> StreamingResponseFuture {
        self.0.stream_response(request, cancellation)
    }
}

/// Reqwest/rustls HTTP transport.
#[derive(Clone)]
pub struct ReqwestHttpTransport {
    client: reqwest::Client,
}

impl ReqwestHttpTransport {
    /// Build a default rustls client.
    ///
    /// # Errors
    /// Reqwest client construction failure.
    pub fn new() -> Result<Self, TransportError> {
        Self::build(None)
    }

    /// Build a rustls client with one positive bounded request deadline.
    ///
    /// # Errors
    /// Zero/>24-hour deadlines or client construction failure.
    pub fn with_timeout(timeout: Duration) -> Result<Self, TransportError> {
        if timeout.is_zero() || timeout > MAX_HTTP_TIMEOUT {
            return Err(TransportError::InvalidRequest {
                field: "timeout",
                message: "HTTP timeout must be positive and at most 24 hours".to_owned(),
            });
        }
        Self::build(Some(timeout))
    }

    fn build(timeout: Option<Duration>) -> Result<Self, TransportError> {
        // Never follow redirects automatically. reqwest strips `Authorization`
        // across origins but NOT custom headers, so an automatic hop would
        // re-send protocol identity such as a session id, and any provider
        // API key carried in a custom header, to whatever host the redirect
        // named. A 3xx therefore reaches the caller, which owns per-hop policy
        // exactly as the portable web provider already does.
        let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        let client = builder.build().map_err(reqwest_error)?;
        Ok(Self { client })
    }
}

impl HttpTransport for ReqwestHttpTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        let client = self.client.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let (request, max_response_bytes) = request.into_request(&client)?;
            let mut response = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(TransportError::Cancelled),
                response = client.execute(request) => response.map_err(reqwest_error)?,
            };
            let status = response.status().as_u16();
            let headers = bounded_response_headers(response.headers());
            let content_type = response_content_type(response.headers());
            let mut body = Vec::new();
            loop {
                let chunk = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(TransportError::Cancelled),
                    chunk = response.chunk() => chunk.map_err(reqwest_error)?,
                };
                let Some(chunk) = chunk else {
                    break;
                };
                if body.len().saturating_add(chunk.len()) > max_response_bytes {
                    return Err(TransportError::ResponseTooLarge {
                        max_bytes: max_response_bytes,
                    });
                }
                body.extend_from_slice(&chunk);
            }
            Ok(HttpResponse {
                status,
                content_type,
                headers,
                body,
            })
        })
    }

    fn stream_response(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> StreamingResponseFuture {
        let (request, max_response_bytes) = match request.into_request(&self.client) {
            Ok(request) => request,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let client = self.client.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let response = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(TransportError::Cancelled),
                response = client.execute(request) => response.map_err(reqwest_error)?,
            };
            let status = response.status().as_u16();
            let content_type = response_content_type(response.headers());
            let headers = bounded_response_headers(response.headers());
            let body = Box::pin(futures::stream::unfold(
                ResponseBodyState::Reading {
                    response,
                    cancellation,
                    received: 0,
                    max_response_bytes,
                },
                drive_response_body,
            ));
            Ok(HttpStreamingResponse {
                status,
                content_type,
                headers,
                body,
            })
        })
    }

    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        self.sse_exchange(request, cancellation).events
    }

    fn sse_exchange(
        &self,
        request: HttpSseRequest,
        cancellation: CancellationToken,
    ) -> SseExchange {
        let observed = SseResponseHeaders::default();
        let request = match request.into_request(&self.client) {
            Ok(request) => request,
            Err(error) => {
                return SseExchange {
                    headers: observed,
                    events: Box::pin(futures::stream::once(async move { Err(error) })),
                };
            }
        };
        let phase = Phase::Send {
            client: self.client.clone(),
            request,
            cancellation,
            observed: observed.clone(),
        };
        SseExchange {
            headers: observed,
            events: Box::pin(
                futures::stream::unfold(phase, drive_phase).flat_map(futures::stream::iter),
            ),
        }
    }
}

enum ResponseBodyState {
    Reading {
        response: reqwest::Response,
        cancellation: CancellationToken,
        received: usize,
        max_response_bytes: usize,
    },
    Done,
}

async fn drive_response_body(
    state: ResponseBodyState,
) -> Option<(Result<Vec<u8>, TransportError>, ResponseBodyState)> {
    let ResponseBodyState::Reading {
        mut response,
        cancellation,
        received,
        max_response_bytes,
    } = state
    else {
        return None;
    };
    if cancellation.is_cancelled() {
        return Some((Err(TransportError::Cancelled), ResponseBodyState::Done));
    }
    let chunk = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            return Some((Err(TransportError::Cancelled), ResponseBodyState::Done));
        }
        chunk = response.chunk() => chunk,
    };
    match chunk {
        Ok(Some(chunk)) => {
            let next = received.saturating_add(chunk.len());
            if next > max_response_bytes {
                return Some((
                    Err(TransportError::ResponseTooLarge {
                        max_bytes: max_response_bytes,
                    }),
                    ResponseBodyState::Done,
                ));
            }
            Some((
                Ok(chunk.to_vec()),
                ResponseBodyState::Reading {
                    response,
                    cancellation,
                    received: next,
                    max_response_bytes,
                },
            ))
        }
        Ok(None) => None,
        Err(error) => Some((Err(reqwest_error(error)), ResponseBodyState::Done)),
    }
}

fn response_content_type(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

enum Phase {
    Send {
        client: reqwest::Client,
        request: reqwest::Request,
        cancellation: CancellationToken,
        /// Slot the response's headers are published into, once observed.
        observed: SseResponseHeaders,
    },
    Decode {
        response: reqwest::Response,
        decoder: SseDecoder,
        cancellation: CancellationToken,
    },
    Done,
}

async fn drive_phase(phase: Phase) -> Option<(Vec<Result<SseEvent, TransportError>>, Phase)> {
    match phase {
        Phase::Send {
            client,
            request,
            cancellation,
            observed,
        } => {
            if cancellation.is_cancelled() {
                return Some((vec![Err(TransportError::Cancelled)], Phase::Done));
            }
            let response = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Some((vec![Err(TransportError::Cancelled)], Phase::Done));
                }
                response = client.execute(request) => response,
            };
            // Headers are published for every response that arrived, success
            // or not: a 429's rate-limit headers are exactly the ones a caller
            // most needs.
            if let Ok(response) = response.as_ref() {
                observed.publish(bounded_response_headers(response.headers()));
            }
            match response {
                Ok(response) if response.status().is_success() && response_is_sse(&response) => {
                    Some((
                        Vec::new(),
                        Phase::Decode {
                            response,
                            decoder: SseDecoder::new(),
                            cancellation,
                        },
                    ))
                }
                Ok(response) if response.status().is_success() => Some((
                    vec![Err(TransportError::InvalidSse {
                        message: "successful response content-type is not text/event-stream"
                            .to_owned(),
                    })],
                    Phase::Done,
                )),
                Ok(response) => {
                    let status = response.status().as_u16();
                    let metadata = http_error_metadata(&response);
                    let tail = read_error_tail(response, &cancellation).await;
                    let error = match tail {
                        Ok(body) => TransportError::http(status, body, metadata),
                        Err(error) => error,
                    };
                    Some((vec![Err(error)], Phase::Done))
                }
                Err(error) => Some((vec![Err(reqwest_error(error))], Phase::Done)),
            }
        }
        Phase::Decode {
            mut response,
            mut decoder,
            cancellation,
        } => {
            let chunk = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Some((vec![Err(TransportError::Cancelled)], Phase::Done));
                }
                chunk = response.chunk() => chunk,
            };
            match chunk {
                Ok(Some(bytes)) => match decoder.feed(&bytes) {
                    Ok(events) => Some((
                        events.into_iter().map(Ok).collect(),
                        Phase::Decode {
                            response,
                            decoder,
                            cancellation,
                        },
                    )),
                    Err(error) => Some((vec![Err(sse_error(error))], Phase::Done)),
                },
                Ok(None) => match decoder.finish() {
                    Ok(events) => Some((events.into_iter().map(Ok).collect(), Phase::Done)),
                    Err(error) => Some((vec![Err(sse_error(error))], Phase::Done)),
                },
                Err(error) => Some((vec![Err(reqwest_error(error))], Phase::Done)),
            }
        }
        Phase::Done => None,
    }
}

async fn read_error_tail(
    mut response: reqwest::Response,
    cancellation: &CancellationToken,
) -> Result<String, TransportError> {
    let mut tail = Vec::new();
    let mut total = 0_usize;
    let deadline = tokio::time::sleep(ERROR_BODY_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        let chunk = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(TransportError::Cancelled),
            () = &mut deadline => break,
            chunk = response.chunk() => chunk,
        };
        match chunk {
            Ok(Some(bytes)) => {
                let remaining = ERROR_BODY_MAX_BYTES.saturating_sub(total);
                let accepted = bytes.len().min(remaining);
                append_error_tail(&mut tail, &bytes[..accepted]);
                total = total.saturating_add(accepted);
                if accepted < bytes.len() || total >= ERROR_BODY_MAX_BYTES {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&tail);
    let skip = text.chars().count().saturating_sub(ERROR_TAIL_CHARS);
    Ok(text.chars().skip(skip).collect())
}

fn append_error_tail(tail: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() >= ERROR_TAIL_BYTES {
        tail.clear();
        tail.extend_from_slice(&bytes[bytes.len() - ERROR_TAIL_BYTES..]);
        return;
    }
    let excess = tail
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(ERROR_TAIL_BYTES);
    if excess > 0 {
        tail.drain(..excess);
    }
    tail.extend_from_slice(bytes);
}

fn http_error_metadata(response: &reqwest::Response) -> HttpErrorMetadata {
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    let should_retry = response
        .headers()
        .get("x-should-retry")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= ERROR_HEADER_BYTES && !value.chars().any(char::is_control))
        .and_then(|value| match value.trim() {
            value if value.eq_ignore_ascii_case("true") => Some(true),
            value if value.eq_ignore_ascii_case("false") => Some(false),
            _ => None,
        });
    HttpErrorMetadata::new(retry_after, should_retry)
}

fn parse_retry_after(value: &str) -> Option<HttpRetryAfter> {
    let value = value.trim();
    if value.is_empty() || value.len() > ERROR_HEADER_BYTES || value.chars().any(char::is_control) {
        return None;
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value
            .parse::<u64>()
            .ok()
            .map(Duration::from_secs)
            .map(HttpRetryAfter::Delay);
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(HttpRetryAfter::At)
}

fn reqwest_error(error: reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Network {
            message: "request could not be completed".to_owned(),
        }
    }
}

fn bound_error_text(value: String) -> String {
    let mut characters = value
        .chars()
        .rev()
        .take(ERROR_TAIL_CHARS)
        .collect::<Vec<_>>();
    characters.reverse();
    characters.into_iter().collect()
}

fn sse_error(error: SseDecodeError) -> TransportError {
    TransportError::InvalidSse {
        message: error.to_string(),
    }
}

fn response_is_sse(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
}

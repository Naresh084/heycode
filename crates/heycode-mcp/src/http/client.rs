//! The legacy-era Streamable HTTP protocol engine.
//!
//! Every JSON-RPC message is its own POST to the single MCP endpoint through
//! the composed `http` service. The server answers a request with either one
//! JSON object or an SSE response stream, and the client branches on the
//! response content type at runtime. A server-assigned session identifier is
//! echoed on every subsequent request; an HTTP 404 for a request carrying one
//! means the server terminated it, and the client starts a new session with a
//! fresh `InitializeRequest` that carries no session identifier.
//!
//! `heycode_http::HttpResponse` implements `Debug` and now carries response
//! headers, so this module must never render one: the session identifier lives
//! there. It is lifted into [`McpSessionId`] at the boundary and nowhere else.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::FuturesUnordered;
use futures::{StreamExt as _, TryStreamExt as _};
use heycode_http::{
    HttpRequest, HttpResponse, HttpService, HttpStreamingResponse, SseDecoder, TransportError,
};
use tokio_util::sync::CancellationToken;

use super::{McpHttpError, McpProtocolVersion, McpSessionId};
use crate::McpCredentialBindings;
use crate::channel::{McpChannelError, McpRequestChannel, McpServerHandshake};
use crate::notifications::McpNotificationRouter;
use crate::registry::{McpSecretReference, McpStreamableHttpTransport, McpTimeouts};

const ACCEPT: &str = "application/json, text/event-stream";
const CONTENT_TYPE_JSON: &str = "application/json";
const CONTENT_TYPE_SSE: &str = "text/event-stream";
const HEADER_ACCEPT: &str = "accept";
const HEADER_CONTENT_TYPE: &str = "content-type";
/// Header names are case-insensitive; the composed service lowercases them.
const HEADER_SESSION_ID: &str = "mcp-session-id";
const HEADER_PROTOCOL_VERSION: &str = "mcp-protocol-version";
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_IN_FLIGHT_CLIENT_REQUESTS: usize = 16;

#[derive(Default)]
struct SessionState {
    negotiated: Option<McpProtocolVersion>,
    session_id: Option<McpSessionId>,
}

enum Attempt {
    Failed(McpHttpError),
    SessionTerminated,
}

enum ClientRequestWork {
    Reply(crate::McpClientReply),
    Elicitation(crate::McpPendingElicitation),
}

type ClientRequestFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), McpHttpError>> + Send + 'a>>;
type ClientRequestFutures<'a> = FuturesUnordered<ClientRequestFuture<'a>>;

/// One legacy-era Streamable HTTP MCP session.
pub struct McpStreamableHttpClient {
    http: HttpService,
    url: String,
    router: McpNotificationRouter,
    timeouts: McpTimeouts,
    next_id: AtomicU64,
    session: Mutex<SessionState>,
    credential_headers: std::collections::BTreeMap<String, McpSecretReference>,
    credentials: Option<(
        heycode_credentials::CredentialsService,
        McpCredentialBindings,
    )>,
}

impl McpStreamableHttpClient {
    /// Bind the composed HTTP service to one validated endpoint definition.
    ///
    /// # Errors
    /// Credential-reference headers cannot be resolved until an authorization
    /// provider exists, so such a definition is refused rather than connected
    /// without its credentials.
    pub fn new(
        http: HttpService,
        definition: &McpStreamableHttpTransport,
        router: McpNotificationRouter,
        timeouts: McpTimeouts,
    ) -> Result<Self, McpHttpError> {
        if !definition.headers().is_empty() {
            return Err(McpHttpError::Unauthorized);
        }
        Ok(Self {
            http,
            url: definition.url().to_owned(),
            router,
            timeouts,
            next_id: AtomicU64::new(1),
            session: Mutex::new(SessionState::default()),
            credential_headers: std::collections::BTreeMap::new(),
            credentials: None,
        })
    }

    /// Bind credential-reference headers to operation-time registry queries.
    ///
    /// This constructor validates only safe references. Values are resolved
    /// independently for each HTTP operation and are retained only by that
    /// request.
    ///
    /// # Errors
    /// Any definition reference without an exact safe binding.
    pub fn new_with_credentials(
        http: HttpService,
        definition: &McpStreamableHttpTransport,
        router: McpNotificationRouter,
        timeouts: McpTimeouts,
        credentials: heycode_credentials::CredentialsService,
        bindings: McpCredentialBindings,
    ) -> Result<Self, McpHttpError> {
        if definition
            .headers()
            .values()
            .any(|reference| !bindings.contains(reference))
        {
            return Err(McpHttpError::Unauthorized);
        }
        Ok(Self {
            http,
            url: definition.url().to_owned(),
            router,
            timeouts,
            next_id: AtomicU64::new(1),
            session: Mutex::new(SessionState::default()),
            credential_headers: definition.headers().clone(),
            credentials: Some((credentials, bindings)),
        })
    }

    /// Run the `initialize` handshake and confirm it.
    ///
    /// Any previously captured session is dropped first, so the request never
    /// carries a session identifier. The negotiated revision must belong to the
    /// closed supported set; otherwise the client disconnects without sending
    /// `notifications/initialized`.
    ///
    /// # Errors
    /// Transport, timeout, cancellation, unusable status, JSON-RPC errors and
    /// protocol contract violations.
    pub async fn initialize(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<McpServerHandshake, McpHttpError> {
        self.reset_session()?;
        let id = self.next_id();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": McpProtocolVersion::LATEST.as_str(),
                "capabilities": self.router.client_capabilities(),
                "clientInfo": {"name": "heycode", "version": env!("CARGO_PKG_VERSION")}
            }
        });
        let response = self
            .dispatch(
                self.post_request(&body)?,
                self.timeouts.startup_ms(),
                cancellation,
            )
            .await?;
        if response.status != 200 {
            return Err(status_error(response.status));
        }
        // Lifted at the boundary: a session id outside visible ASCII is refused
        // instead of being echoed back into a request header.
        if let Some(session) = response.header(HEADER_SESSION_ID) {
            self.store_session(McpSessionId::new(session)?)?;
        }
        let result = self.message_result(&response, id, cancellation).await?;
        let handshake = McpServerHandshake::from_initialize_result(&result)?;
        let Some(negotiated) = McpProtocolVersion::parse(handshake.protocol_version()) else {
            self.reset_session()?;
            return Err(McpHttpError::protocol(
                "negotiated protocol version is outside the supported set",
            ));
        };
        self.store_version(negotiated)?;
        if handshake.capabilities().logging {
            self.router.enable_logging();
        }
        self.confirm_initialized(cancellation).await?;
        Ok(handshake)
    }

    /// Send one JSON-RPC request and return its `result`.
    ///
    /// A terminated session is replaced once and the request is retried on the
    /// new session; a second termination fails instead of looping.
    ///
    /// # Errors
    /// Transport, timeout, cancellation, unusable status, JSON-RPC errors and
    /// protocol contract violations.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpHttpError> {
        match self.attempt(method, &params, cancellation).await {
            Ok(value) => Ok(value),
            Err(Attempt::Failed(error)) => Err(error),
            Err(Attempt::SessionTerminated) => {
                self.initialize(cancellation).await?;
                match self.attempt(method, &params, cancellation).await {
                    Ok(value) => Ok(value),
                    Err(Attempt::Failed(error)) => Err(error),
                    Err(Attempt::SessionTerminated) => Err(McpHttpError::Status { status: 404 }),
                }
            }
        }
    }

    /// Explicitly terminate the session when the server assigned one.
    ///
    /// A server that does not allow client termination answers `405`, and a
    /// server that already forgot the session answers `404`; both are success.
    ///
    /// # Errors
    /// Transport, timeout, cancellation and any other unusable status.
    pub async fn terminate(&self, cancellation: &CancellationToken) -> Result<(), McpHttpError> {
        if !self.has_session()? {
            return Ok(());
        }
        let response = self
            .dispatch(
                self.delete_request()?,
                self.timeouts.shutdown_ms(),
                cancellation,
            )
            .await?;
        self.reset_session()?;
        match response.status {
            200..=299 | 404 | 405 => Ok(()),
            status => Err(McpHttpError::Status { status }),
        }
    }

    async fn confirm_initialized(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), McpHttpError> {
        let body = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let response = self
            .dispatch(
                self.post_request(&body)?,
                self.timeouts.request_ms(),
                cancellation,
            )
            .await?;
        if (200..300).contains(&response.status) {
            Ok(())
        } else {
            Err(status_error(response.status))
        }
    }

    async fn attempt(
        &self,
        method: &str,
        params: &serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, Attempt> {
        let id = self.next_id();
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        });
        let request = self.post_request(&body).map_err(Attempt::Failed)?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(self.timeouts.request_ms());
        let response = self
            .dispatch_streaming(request, deadline, cancellation)
            .await
            .map_err(Attempt::Failed)?;
        if response.status() == 404 && self.has_session().map_err(Attempt::Failed)? {
            return Err(Attempt::SessionTerminated);
        }
        if response.status() != 200 {
            return Err(Attempt::Failed(status_error(response.status())));
        }
        tokio::time::timeout_at(
            deadline,
            self.streaming_message_result(response, id, cancellation),
        )
        .await
        .map_err(|_| Attempt::Failed(McpHttpError::TimedOut))?
        .map_err(Attempt::Failed)
    }

    async fn dispatch_streaming(
        &self,
        request: HttpRequest,
        deadline: tokio::time::Instant,
        cancellation: &CancellationToken,
    ) -> Result<HttpStreamingResponse, McpHttpError> {
        if cancellation.is_cancelled() {
            return Err(McpHttpError::Cancelled);
        }
        let send = self.http.stream_response(request, cancellation.clone());
        match tokio::time::timeout_at(deadline, send).await {
            Ok(response) => response.map_err(transport_error),
            Err(_) => Err(McpHttpError::TimedOut),
        }
    }

    async fn dispatch(
        &self,
        request: HttpRequest,
        budget_ms: u64,
        cancellation: &CancellationToken,
    ) -> Result<HttpResponse, McpHttpError> {
        if cancellation.is_cancelled() {
            return Err(McpHttpError::Cancelled);
        }
        let send = self.http.send(request, cancellation.clone());
        match tokio::time::timeout(Duration::from_millis(budget_ms), send).await {
            Ok(response) => response.map_err(transport_error),
            Err(_) => Err(McpHttpError::TimedOut),
        }
    }

    fn post_request(&self, body: &serde_json::Value) -> Result<HttpRequest, McpHttpError> {
        let bytes = serde_json::to_vec(body)
            .map_err(|_| McpHttpError::protocol("request body could not be serialized"))?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(McpHttpError::protocol(
                "request body exceeds the configured byte bound",
            ));
        }
        let request = HttpRequest::post(&self.url, bytes)
            .map_err(transport_error)?
            .header(HEADER_CONTENT_TYPE, CONTENT_TYPE_JSON)
            .map_err(transport_error)?;
        self.apply_session_headers(request)
    }

    fn delete_request(&self) -> Result<HttpRequest, McpHttpError> {
        let request = HttpRequest::delete(&self.url).map_err(transport_error)?;
        self.apply_session_headers(request)
    }

    fn apply_session_headers(&self, request: HttpRequest) -> Result<HttpRequest, McpHttpError> {
        // No `Origin`: validating it is a server obligation in this transport,
        // and a non-browser client asserting one only weakens that check.
        let mut request = request
            .with_max_response_bytes(MAX_MESSAGE_BYTES)
            .header(HEADER_ACCEPT, ACCEPT)
            .map_err(transport_error)?;
        let state = self.state()?;
        if let Some(session) = &state.session_id {
            request = request
                .header(HEADER_SESSION_ID, session.expose())
                .map_err(transport_error)?;
        }
        if let Some(version) = state.negotiated {
            request = request
                .header(HEADER_PROTOCOL_VERSION, version.as_str())
                .map_err(transport_error)?;
        }
        drop(state);
        if !self.credential_headers.is_empty() {
            let (credentials, bindings) = self
                .credentials
                .as_ref()
                .ok_or(McpHttpError::Unauthorized)?;
            for (name, reference) in &self.credential_headers {
                let value = bindings
                    .materialize(credentials, reference)
                    .map_err(|_| McpHttpError::Unauthorized)?;
                request = request.header(name, &value).map_err(transport_error)?;
            }
        }
        Ok(request)
    }

    async fn message_result(
        &self,
        response: &HttpResponse,
        id: u64,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpHttpError> {
        let media = response.content_type.as_ref().map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        });
        match media.as_deref() {
            Some(CONTENT_TYPE_JSON) => {
                let value: serde_json::Value = serde_json::from_slice(&response.body)
                    .map_err(|_| McpHttpError::protocol("response body is not valid JSON"))?;
                if value.get("id").is_some() && value.get("method").is_some() {
                    let work = self.prepare_client_request(&value);
                    self.answer_client_request(work, cancellation).await?;
                    Err(McpHttpError::protocol(
                        "response ended without the matching JSON-RPC response",
                    ))
                } else {
                    response_result(&value, id)
                }
            }
            Some(CONTENT_TYPE_SSE) => self.stream_result(&response.body, id, cancellation).await,
            _ => Err(McpHttpError::protocol(
                "response content type must be application/json or text/event-stream",
            )),
        }
    }

    async fn streaming_message_result(
        &self,
        mut response: HttpStreamingResponse,
        id: u64,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpHttpError> {
        match response.content_type() {
            Some(CONTENT_TYPE_JSON) => {
                let body = response
                    .body_mut()
                    .map_err(transport_error)
                    .try_fold(Vec::new(), |mut body, chunk| async move {
                        body.extend_from_slice(&chunk);
                        Ok(body)
                    })
                    .await?;
                let value: serde_json::Value = serde_json::from_slice(&body)
                    .map_err(|_| McpHttpError::protocol("response body is not valid JSON"))?;
                if value.get("id").is_some() && value.get("method").is_some() {
                    let work = self.prepare_client_request(&value);
                    self.answer_client_request(work, cancellation).await?;
                    Err(McpHttpError::protocol(
                        "response ended without the matching JSON-RPC response",
                    ))
                } else {
                    response_result(&value, id)
                }
            }
            Some(CONTENT_TYPE_SSE) => {
                self.open_stream_result(response.body_mut(), id, cancellation)
                    .await
            }
            _ => Err(McpHttpError::protocol(
                "response content type must be application/json or text/event-stream",
            )),
        }
    }

    async fn open_stream_result(
        &self,
        body: &mut heycode_http::HttpBodyStream,
        id: u64,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpHttpError> {
        let mut decoder = Some(SseDecoder::new());
        let mut works = ClientRequestFutures::new();
        let mut result = None;
        let mut body_done = false;

        loop {
            if body_done && works.is_empty() {
                break;
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(McpHttpError::Cancelled),
                settled = works.next(), if !works.is_empty() => {
                    if let Some(settled) = settled {
                        settled?;
                    }
                }
                chunk = body.next(), if !body_done => {
                    match chunk {
                        Some(Ok(chunk)) => {
                            let events = decoder
                                .as_mut()
                                .ok_or_else(|| {
                                    McpHttpError::protocol("response stream decoder is settled")
                                })?
                                .feed(&chunk)
                                .map_err(|_| {
                                McpHttpError::protocol("response stream framing is invalid")
                            })?;
                            self.route_open_events(events, id, &mut result, &mut works, cancellation)
                                .await?;
                        }
                        Some(Err(error)) => return Err(transport_error(error)),
                        None => {
                            body_done = true;
                            let events = decoder
                                .take()
                                .ok_or_else(|| {
                                    McpHttpError::protocol("response stream decoder is settled")
                                })?
                                .finish()
                                .map_err(|_| {
                                McpHttpError::protocol("response stream framing is invalid")
                            })?;
                            self.route_open_events(events, id, &mut result, &mut works, cancellation)
                                .await?;
                        }
                    }
                }
            }
        }
        result.ok_or_else(|| {
            McpHttpError::protocol("response stream ended without the matching response")
        })
    }

    async fn route_open_events<'a>(
        &'a self,
        events: Vec<heycode_http::SseEvent>,
        id: u64,
        result: &mut Option<serde_json::Value>,
        works: &mut ClientRequestFutures<'a>,
        cancellation: &'a CancellationToken,
    ) -> Result<(), McpHttpError> {
        for event in events {
            let message = serde_json::from_str::<serde_json::Value>(&event.data).map_err(|_| {
                McpHttpError::protocol("response stream carried a non-JSON message")
            })?;
            if message.get("id").is_some() && message.get("method").is_some() {
                if works.len() >= MAX_IN_FLIGHT_CLIENT_REQUESTS {
                    self.answer_client_request(
                        ClientRequestWork::Reply(crate::McpClientReply::overloaded(&message)),
                        cancellation,
                    )
                    .await?;
                } else {
                    let work = self.prepare_client_request(&message);
                    works.push(Box::pin(self.answer_client_request(work, cancellation)));
                }
            } else if message.get("id").is_some() {
                if result.is_some() {
                    return Err(McpHttpError::protocol(
                        "response stream carried more than one matching response",
                    ));
                }
                *result = Some(response_result(&message, id)?);
            } else {
                self.observe_notification(&message);
            }
        }
        Ok(())
    }

    async fn stream_result(
        &self,
        body: &[u8],
        id: u64,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpHttpError> {
        let mut decoder = SseDecoder::new();
        let mut events = decoder
            .feed(body)
            .map_err(|_| McpHttpError::protocol("response stream framing is invalid"))?;
        events.extend(
            decoder
                .finish()
                .map_err(|_| McpHttpError::protocol("response stream framing is invalid"))?,
        );
        let mut requests = Vec::new();
        let mut result = None;
        for event in events {
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                return Err(McpHttpError::protocol(
                    "response stream carried a non-JSON message",
                ));
            };
            if message.get("id").is_some() && message.get("method").is_some() {
                requests.push(self.prepare_client_request(&message));
            } else if message.get("id").is_some() {
                if result.is_some() {
                    return Err(McpHttpError::protocol(
                        "response stream carried more than one matching response",
                    ));
                }
                result = Some(response_result(&message, id)?);
            } else {
                self.observe_notification(&message);
            }
        }
        for request in requests {
            self.answer_client_request(request, cancellation).await?;
        }
        result.ok_or_else(|| {
            McpHttpError::protocol("response stream ended without the matching response")
        })
    }

    fn prepare_client_request(&self, message: &serde_json::Value) -> ClientRequestWork {
        let method = message.get("method").and_then(serde_json::Value::as_str);
        if method == Some("ping") {
            return ClientRequestWork::Reply(crate::McpClientReply::empty_result(message));
        }
        if let Some(client_events) = self.router.client_events() {
            return match client_events.admit_request(message) {
                Ok(Some(pending)) => ClientRequestWork::Elicitation(pending),
                Ok(None) => {
                    ClientRequestWork::Reply(crate::McpClientReply::method_not_found(message))
                }
                Err(reply) => ClientRequestWork::Reply(reply),
            };
        }
        ClientRequestWork::Reply(crate::McpClientReply::method_not_found(message))
    }

    async fn answer_client_request(
        &self,
        work: ClientRequestWork,
        cancellation: &CancellationToken,
    ) -> Result<(), McpHttpError> {
        let reply = match work {
            ClientRequestWork::Reply(reply) => Some(reply),
            ClientRequestWork::Elicitation(pending) => pending.resolve().await,
        };
        let Some(reply) = reply else {
            return Ok(());
        };
        let response = self
            .dispatch(
                self.post_request(reply.as_json())?,
                self.timeouts.request_ms(),
                cancellation,
            )
            .await?;
        if (200..300).contains(&response.status) {
            Ok(())
        } else {
            Err(status_error(response.status))
        }
    }

    /// Route one notification that arrived on a response stream.
    ///
    /// All four families go to the same router, so an SSE stream cannot advance
    /// the tool epoch while a resource update on the same stream is dropped.
    fn observe_notification(&self, message: &serde_json::Value) {
        let _routed = self.router.observe(message);
    }

    /// This session's routing plane, for the generation owners it feeds.
    #[must_use]
    pub fn router(&self) -> McpNotificationRouter {
        self.router.clone()
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, SessionState>, McpHttpError> {
        self.session
            .lock()
            .map_err(|_| McpHttpError::protocol("session state is unavailable"))
    }

    fn store_session(&self, session_id: McpSessionId) -> Result<(), McpHttpError> {
        self.state()?.session_id = Some(session_id);
        Ok(())
    }

    fn store_version(&self, negotiated: McpProtocolVersion) -> Result<(), McpHttpError> {
        self.state()?.negotiated = Some(negotiated);
        Ok(())
    }

    fn reset_session(&self) -> Result<(), McpHttpError> {
        let mut state = self.state()?;
        state.session_id = None;
        state.negotiated = None;
        Ok(())
    }

    fn has_session(&self) -> Result<bool, McpHttpError> {
        Ok(self.state()?.session_id.is_some())
    }
}

impl Drop for McpStreamableHttpClient {
    fn drop(&mut self) {
        self.router.shutdown();
    }
}

#[async_trait]
impl McpRequestChannel for McpStreamableHttpClient {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        self.request(method, params, cancellation)
            .await
            .map_err(McpChannelError::from)
    }
}

const fn status_error(status: u16) -> McpHttpError {
    match status {
        401 | 403 => McpHttpError::Unauthorized,
        status => McpHttpError::Status { status },
    }
}

fn response_result(value: &serde_json::Value, id: u64) -> Result<serde_json::Value, McpHttpError> {
    let message = value
        .as_object()
        .ok_or(McpHttpError::protocol("JSON-RPC message must be an object"))?;
    if message.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
        return Err(McpHttpError::protocol(
            "JSON-RPC response id does not match the request",
        ));
    }
    if let Some(error) = message.get("error") {
        return Err(McpHttpError::Rpc {
            code: error
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default(),
        });
    }
    Ok(message
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Project composed-transport failures onto the bounded MCP classes. The
/// composed service's safe field labels are kept; its messages are not.
fn transport_error(error: TransportError) -> McpHttpError {
    match error {
        TransportError::InvalidRequest { field, .. } => McpHttpError::invalid(
            match field {
                "url" => "endpoint",
                "headers" => "header",
                _ => "request",
            },
            "the composed HTTP service rejected the request field",
        ),
        TransportError::Timeout => McpHttpError::TimedOut,
        TransportError::Cancelled => McpHttpError::Cancelled,
        TransportError::Http { status, .. } => McpHttpError::Status { status },
        TransportError::InvalidSse { .. } => {
            McpHttpError::protocol("response stream framing is invalid")
        }
        TransportError::ResponseTooLarge { .. } => {
            McpHttpError::protocol("response exceeds the configured byte bound")
        }
        TransportError::Network { .. } => McpHttpError::Transport,
        // `TransportError` is `#[non_exhaustive]`: an unknown future class is a
        // transport failure, never a protocol claim.
        _ => McpHttpError::Transport,
    }
}

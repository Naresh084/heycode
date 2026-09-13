//! P13 — one streaming session that prefers a WebSocket where an endpoint
//! offers one and falls back to HTTP+SSE where it does not.
//!
//! **No inference protocol heycode currently speaks offers a WebSocket wire.**
//! OpenAI Responses, OpenAI Chat Completions, Anthropic Messages, Gemini
//! `generateContent` and Bedrock `Converse` are all an HTTP request whose
//! streaming response is `text/event-stream`; Bedrock's bidirectional
//! operation is HTTP/2, not a WebSocket. The WebSocket endpoints these vendors
//! do publish — OpenAI Realtime and Gemini `BidiGenerateContent` — are
//! separate real-time protocols with their own message schemas, not an
//! alternative transport for the request shapes this crate carries. So the
//! branch that runs in every heycode deployment today is the fallback, and it is
//! the branch these tests hold down hardest.
//!
//! The library is deliberately absent. [`WebSocketConnector`] is the entire
//! seam: it is a trait, this crate names no WebSocket implementation, and the
//! fallback decision, the reconnect bound and the metrics are all expressed
//! against that trait. Choosing a library later adds one implementor and
//! changes nothing here.
//!
//! Two rules keep the machine honest:
//!
//! 1. **Fallback is a decision, not an accident.** It happens only before any
//!    connection is established, it is recorded in both [`StreamOutcome`] and
//!    [`StreamTransportMetrics`], and a caller that declared it needs the
//!    client-to-server channel is failed rather than silently handed a
//!    one-directional HTTP stream.
//! 2. **Reconnect covers the open handshake only.** The connector cannot see
//!    opening frames. Once a connection is established the transport commits
//!    to it, sends each opening frame exactly once and treats any setup-send
//!    failure as terminal. Replaying a possibly processed frame — on a second
//!    socket or on HTTP — could duplicate a provider request. Resumption past
//!    that point needs sequence identity, which is the protocol adapter's
//!    knowledge, not this crate's.
//!
//! Together those bound the work: at most `1 + max_reconnects` WebSocket opens
//! and then at most one HTTP request, with no path back to the WebSocket.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::{Stream, StreamExt as _};
use tokio_util::sync::CancellationToken;

use crate::SseEvent;
use crate::transport::{
    HttpHeader, HttpService, HttpSseRequest, SseEventStream, TransportError, validated_endpoint,
};

/// Largest accepted reconnect budget. Matches the provider retry ceiling so a
/// transport-level loop can never outlast the policy layer above it.
const MAX_RECONNECTS: u32 = 8;
/// Largest accepted reconnect backoff.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(60);
/// Opening setup is bounded before any connector sees the handshake.
const MAX_OPEN_FRAMES: usize = 16;
/// One opening frame is bounded like one default SSE event.
const MAX_OPEN_FRAME_BYTES: usize = 1024 * 1024;
/// Aggregate opening data is independently bounded.
const MAX_OPEN_FRAMES_BYTES: usize = 4 * 1024 * 1024;
/// Scheme family named by [`WebSocketRequest`]'s rejection.
const WEBSOCKET_SCHEME_LABEL: &str = "WebSocket (ws/wss)";

/// Which wire carried one streaming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamWire {
    /// An established WebSocket connection.
    WebSocket,
    /// One HTTP request whose response was framed as `text/event-stream`.
    HttpSse,
}

/// Why a session that offered a WebSocket did not run on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// No WebSocket connector is installed. Every heycode build is in this state
    /// today, because no configured provider offers a WebSocket wire.
    NoConnector,
    /// The single permitted open failed and the policy allowed no reconnect.
    OpenFailed,
    /// Every permitted open — the first and each reconnect — failed.
    ReconnectBudgetExhausted,
}

impl FallbackReason {
    /// Fixed safe description. Never carries endpoint or provider text.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::NoConnector => "no WebSocket connector is installed",
            Self::OpenFailed => "the WebSocket connection could not be opened",
            Self::ReconnectBudgetExhausted => "every permitted WebSocket open attempt failed",
        }
    }
}

impl std::fmt::Display for FallbackReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.describe())
    }
}

/// What the caller needs from the wire, which decides whether HTTP is an
/// acceptable substitute for a WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRequirement {
    /// A server-to-client event stream and nothing more. HTTP+SSE delivers
    /// exactly that, so falling back withdraws no capability.
    ServerStream,
    /// The caller will also send frames after the stream opens. HTTP+SSE has
    /// no client-to-server channel at all, so a fallback would quietly delete
    /// half of the protocol; the session fails instead.
    Bidirectional,
}

/// Settled facts about how one session chose its wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOutcomeFacts {
    wire: Option<StreamWire>,
    fallback: Option<FallbackReason>,
    websocket_attempts: u32,
    reconnects: u32,
}

impl StreamOutcomeFacts {
    /// The wire that carried the session, or `None` when none did because the
    /// WebSocket was unavailable and the caller forbade the HTTP fallback.
    #[must_use]
    pub const fn wire(&self) -> Option<StreamWire> {
        self.wire
    }

    /// Why the WebSocket was not used, when one was offered and not used.
    #[must_use]
    pub const fn fallback(&self) -> Option<FallbackReason> {
        self.fallback
    }

    /// Whether the session ran on HTTP after a WebSocket was offered.
    #[must_use]
    pub const fn fell_back(&self) -> bool {
        self.fallback.is_some() && matches!(self.wire, Some(StreamWire::HttpSse))
    }

    /// Whether the WebSocket was unavailable and no fallback was permitted.
    #[must_use]
    pub const fn refused_fallback(&self) -> bool {
        self.fallback.is_some() && self.wire.is_none()
    }

    /// WebSocket opens attempted, counting the first attempt.
    #[must_use]
    pub const fn websocket_attempts(&self) -> u32 {
        self.websocket_attempts
    }

    /// Opens beyond the first, that is, reconnect attempts.
    #[must_use]
    pub const fn reconnects(&self) -> u32 {
        self.reconnects
    }
}

/// The wire one session settled on, filled once the decision is taken.
///
/// Like [`crate::SseResponseHeaders`] this is a slot the driver fills once,
/// and `None` means the session has not decided yet — a session decides only
/// when it is polled. It never means "no decision was required".
#[derive(Clone, Debug, Default)]
pub struct StreamOutcome(Arc<std::sync::OnceLock<StreamOutcomeFacts>>);

impl StreamOutcome {
    /// Settled facts once the session has chosen a wire, else `None`.
    #[must_use]
    pub fn get(&self) -> Option<StreamOutcomeFacts> {
        self.0.get().copied()
    }

    fn publish(&self, facts: StreamOutcomeFacts) {
        let _first = self.0.set(facts);
    }
}

/// One inbound WebSocket message, already unframed by the connector. Control
/// frames — ping, pong, close — are the connector's business and never reach
/// this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketMessage {
    /// A text frame payload, provider-opaque exactly like an SSE `data` value.
    Text(String),
    /// A binary frame payload.
    Binary(Vec<u8>),
}

/// Inbound messages of one established WebSocket connection.
pub type WebSocketMessageStream =
    Pin<Box<dyn Stream<Item = Result<WebSocketMessage, TransportError>> + Send>>;

/// One in-flight client-to-server send.
pub type WebSocketSendFuture<'sink> =
    Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + 'sink>>;

/// One in-flight WebSocket open.
pub type WebSocketConnectFuture =
    Pin<Box<dyn std::future::Future<Output = Result<WebSocketConnection, TransportError>> + Send>>;

/// The client-to-server channel of an established WebSocket connection. This
/// is the capability HTTP cannot supply, and therefore the capability whose
/// loss a fallback is not allowed to hide.
pub trait WebSocketSink: Send + Sync {
    /// Send one text frame.
    ///
    /// # Errors
    /// The connection is closed or the send crossed the caller's cancellation.
    fn send_text(&self, text: String, cancellation: CancellationToken) -> WebSocketSendFuture<'_>;
}

/// One established WebSocket connection.
pub struct WebSocketConnection {
    /// Client-to-server channel.
    pub sink: Arc<dyn WebSocketSink>,
    /// Server-to-client messages.
    pub messages: WebSocketMessageStream,
}

/// Opaque handshake-only request. Deliberately has no Debug implementation
/// because headers may carry authorization values.
///
/// Opening frames are absent by construction. The transport sends them only
/// after this handshake succeeds, which makes a retry incapable of replaying
/// application data through a connection that may already have processed it.
#[derive(Clone)]
pub struct WebSocketConnectRequest {
    url: reqwest::Url,
    headers: Vec<HttpHeader>,
}

impl WebSocketConnectRequest {
    /// Validated absolute URL.
    #[must_use]
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Validated handshake headers; reading a value is explicit exposure.
    #[must_use]
    pub fn headers(&self) -> &[HttpHeader] {
        &self.headers
    }
}

/// The library seam. Implementing this is the whole cost of adopting a
/// WebSocket crate; nothing else in heycode-http names one.
pub trait WebSocketConnector: Send + Sync {
    /// Open one connection. This handshake request structurally contains no
    /// application frames; [`StreamTransport`] sends them after success.
    ///
    /// The request is passed by value on every attempt, including reconnects,
    /// so an implementation never has to cache one.
    fn connect(
        &self,
        request: WebSocketConnectRequest,
        cancellation: CancellationToken,
    ) -> WebSocketConnectFuture;
}

/// Slot holding the client-to-server channel of a session that opened a
/// WebSocket.
///
/// `None` means the session has not opened one *yet* or never will; consult
/// [`StreamOutcome`] to tell those apart, exactly as with response headers.
#[derive(Clone, Default)]
pub struct WebSocketSinkSlot(Arc<std::sync::OnceLock<Arc<dyn WebSocketSink>>>);

impl WebSocketSinkSlot {
    /// The client-to-server channel once a WebSocket has opened.
    #[must_use]
    pub fn get(&self) -> Option<Arc<dyn WebSocketSink>> {
        self.0.get().map(Arc::clone)
    }

    fn publish(&self, sink: Arc<dyn WebSocketSink>) {
        let _first = self.0.set(sink);
    }
}

impl std::fmt::Debug for WebSocketSinkSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("WebSocketSinkSlot")
            .field(&self.0.get().is_some())
            .finish()
    }
}

/// Opaque WebSocket open request. Deliberately has no Debug implementation
/// because headers and opening frames may carry authorization values.
#[derive(Clone)]
pub struct WebSocketRequest {
    url: reqwest::Url,
    headers: Vec<HttpHeader>,
    open_frames: Vec<String>,
}

impl WebSocketRequest {
    /// Build a request for one absolute `ws`/`wss` endpoint.
    ///
    /// # Errors
    /// URL must be absolute, host-qualified `ws`/`wss` and credential-free —
    /// the same rejection the HTTP request builders apply, so a WebSocket URL
    /// cannot reach a socket through a laxer door than an HTTP one.
    pub fn new(url: impl AsRef<str>) -> Result<Self, TransportError> {
        Ok(Self {
            url: validated_endpoint(url.as_ref(), &["ws", "wss"], WEBSOCKET_SCHEME_LABEL)?,
            headers: Vec::new(),
            open_frames: Vec::new(),
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

    /// Append one text frame the transport sends immediately after a
    /// successful handshake.
    ///
    /// These are the WebSocket analogue of an HTTP request body: the protocol
    /// setup a bidirectional endpoint expects first. A failed handshake sends
    /// none; once any connection opens, these frames are attempted exactly
    /// once and a send failure is terminal.
    ///
    /// # Errors
    /// More than sixteen frames, one frame over one MiB, or aggregate opening
    /// data over four MiB.
    pub fn open_frame(mut self, text: impl Into<String>) -> Result<Self, TransportError> {
        let text = text.into();
        let aggregate = self
            .open_frames
            .iter()
            .map(String::len)
            .sum::<usize>()
            .saturating_add(text.len());
        if self.open_frames.len() >= MAX_OPEN_FRAMES
            || text.len() > MAX_OPEN_FRAME_BYTES
            || aggregate > MAX_OPEN_FRAMES_BYTES
        {
            return Err(TransportError::InvalidRequest {
                field: "open_frames",
                message: "opening frames exceed the count or byte limit".to_owned(),
            });
        }
        self.open_frames.push(text);
        Ok(self)
    }

    /// Validated absolute URL.
    #[must_use]
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Validated headers; reading a value is an explicit exposure operation.
    #[must_use]
    pub fn headers(&self) -> &[HttpHeader] {
        &self.headers
    }

    /// Frames to send once the connection opens, in order.
    #[must_use]
    pub fn open_frames(&self) -> &[String] {
        &self.open_frames
    }

    fn into_parts(self) -> (WebSocketConnectRequest, Vec<String>) {
        (
            WebSocketConnectRequest {
                url: self.url,
                headers: self.headers,
            },
            self.open_frames,
        )
    }
}

/// Bounded reconnect budget for the WebSocket open handshake.
///
/// Backoff is exponential from `base_backoff`, capped at `max_backoff`, and
/// deliberately un-jittered: this is a tight local loop of at most nine opens
/// against one endpoint, and the jittered, `Retry-After`-aware policy that
/// protects a provider from a fleet lives one layer up in `heycode-llm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    max_reconnects: u32,
    base_backoff: Duration,
    max_backoff: Duration,
}

impl ReconnectPolicy {
    /// Validate one bounded policy.
    ///
    /// # Errors
    /// More than eight reconnects, an inverted base/max pair, or a maximum
    /// backoff above sixty seconds.
    pub fn new(
        max_reconnects: u32,
        base_backoff: Duration,
        max_backoff: Duration,
    ) -> Result<Self, TransportError> {
        if max_reconnects > MAX_RECONNECTS {
            return Err(TransportError::InvalidRequest {
                field: "max_reconnects",
                message: "reconnect budget must be at most 8".to_owned(),
            });
        }
        if base_backoff > max_backoff || max_backoff > MAX_RECONNECT_BACKOFF {
            return Err(TransportError::InvalidRequest {
                field: "reconnect_backoff",
                message: "backoff must rise from base to a maximum of at most 60 seconds"
                    .to_owned(),
            });
        }
        Ok(Self {
            max_reconnects,
            base_backoff,
            max_backoff,
        })
    }

    /// A policy that permits one open and no reconnect.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            max_reconnects: 0,
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        }
    }

    /// Reconnects permitted after the first open.
    #[must_use]
    pub const fn max_reconnects(&self) -> u32 {
        self.max_reconnects
    }

    /// Delay before the next open, or `None` when the budget is spent.
    /// `completed` counts opens already attempted.
    fn backoff_after(&self, completed: u32) -> Option<Duration> {
        if completed >= self.max_reconnects.saturating_add(1) {
            return None;
        }
        let factor = 1_u32
            .checked_shl(completed.saturating_sub(1).min(31))
            .unwrap_or(u32::MAX);
        Some(
            self.base_backoff
                .checked_mul(factor)
                .unwrap_or(self.max_backoff)
                .min(self.max_backoff),
        )
    }
}

impl Default for ReconnectPolicy {
    /// Three reconnects rising from 100ms, capped at five seconds.
    fn default() -> Self {
        Self {
            max_reconnects: 3,
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    sessions: AtomicU64,
    websocket_attempts: AtomicU64,
    websocket_opens: AtomicU64,
    reconnects: AtomicU64,
    reconnect_budget_exhausted: AtomicU64,
    websocket_sessions: AtomicU64,
    websocket_stream_failures: AtomicU64,
    fallbacks: AtomicU64,
    fallbacks_refused: AtomicU64,
    http_sessions: AtomicU64,
}

/// Counters shared by every session one [`StreamTransport`] starts. Cloning
/// shares the counts rather than copying them.
#[derive(Clone, Debug, Default)]
pub struct StreamTransportMetrics {
    counters: Arc<Counters>,
}

impl StreamTransportMetrics {
    /// Read every counter. Settled-session tests get exact accounting;
    /// concurrently running sessions may advance between individual atomic
    /// loads, so this is an operational observation rather than a transaction.
    #[must_use]
    pub fn snapshot(&self) -> StreamTransportMetricsSnapshot {
        let counters = &self.counters;
        StreamTransportMetricsSnapshot {
            sessions: counters.sessions.load(Ordering::Relaxed),
            websocket_attempts: counters.websocket_attempts.load(Ordering::Relaxed),
            websocket_opens: counters.websocket_opens.load(Ordering::Relaxed),
            reconnects: counters.reconnects.load(Ordering::Relaxed),
            reconnect_budget_exhausted: counters.reconnect_budget_exhausted.load(Ordering::Relaxed),
            websocket_sessions: counters.websocket_sessions.load(Ordering::Relaxed),
            websocket_stream_failures: counters.websocket_stream_failures.load(Ordering::Relaxed),
            fallbacks: counters.fallbacks.load(Ordering::Relaxed),
            fallbacks_refused: counters.fallbacks_refused.load(Ordering::Relaxed),
            http_sessions: counters.http_sessions.load(Ordering::Relaxed),
        }
    }

    fn bump<F>(&self, counter: F)
    where
        F: FnOnce(&Counters) -> &AtomicU64,
    {
        counter(&self.counters).fetch_add(1, Ordering::Relaxed);
    }
}

/// One instant's reading of [`StreamTransportMetrics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamTransportMetricsSnapshot {
    /// Sessions that began running. A session counts when it is first polled,
    /// because that is when it does anything.
    pub sessions: u64,
    /// WebSocket opens attempted, first attempts and reconnects together.
    pub websocket_attempts: u64,
    /// WebSocket opens that succeeded.
    pub websocket_opens: u64,
    /// Opens beyond the first within a session, that is, reconnect attempts.
    pub reconnects: u64,
    /// Sessions where a non-zero reconnect budget was spent in full.
    pub reconnect_budget_exhausted: u64,
    /// Sessions that ran on a WebSocket.
    pub websocket_sessions: u64,
    /// Established WebSocket connections that failed after opening. These are
    /// terminal by design: no reconnect and no fallback follows one.
    pub websocket_stream_failures: u64,
    /// Sessions that offered a WebSocket and ran on HTTP instead.
    pub fallbacks: u64,
    /// Sessions failed because the caller's requirement forbade the fallback.
    pub fallbacks_refused: u64,
    /// Sessions that ran on HTTP, whether by plan or by fallback.
    pub http_sessions: u64,
}

/// One session's transport plan. The constructor chosen fixes the
/// requirement, so a plan cannot ask for a bidirectional stream and also
/// carry an HTTP request that could never satisfy it.
pub struct StreamPlan {
    http: Option<HttpSseRequest>,
    websocket: Option<WebSocketRequest>,
    requirement: StreamRequirement,
}

impl StreamPlan {
    /// HTTP+SSE only: the shape every configured heycode provider uses today.
    /// Running on HTTP here is not a fallback and is never counted as one.
    #[must_use]
    pub fn http_only(http: HttpSseRequest) -> Self {
        Self {
            http: Some(http),
            websocket: None,
            requirement: StreamRequirement::ServerStream,
        }
    }

    /// Prefer the WebSocket, fall back to the equivalent HTTP request. Both
    /// must be able to carry the same server-to-client stream.
    #[must_use]
    pub fn preferring_websocket(websocket: WebSocketRequest, http: HttpSseRequest) -> Self {
        Self {
            http: Some(http),
            websocket: Some(websocket),
            requirement: StreamRequirement::ServerStream,
        }
    }

    /// Require the WebSocket, because the caller will send frames after the
    /// stream opens. There is no HTTP request to fall back to, so the silent
    /// downgrade is impossible by construction rather than by discipline.
    #[must_use]
    pub fn requiring_websocket(websocket: WebSocketRequest) -> Self {
        Self {
            http: None,
            websocket: Some(websocket),
            requirement: StreamRequirement::Bidirectional,
        }
    }

    /// What this plan needs from the wire.
    #[must_use]
    pub const fn requirement(&self) -> StreamRequirement {
        self.requirement
    }
}

/// One streaming session: the wire it settled on, the client-to-server
/// channel when it has one, and the unified event stream.
pub struct StreamSession {
    /// Which wire carried the session, filled once decided.
    pub outcome: StreamOutcome,
    /// Client-to-server channel, filled only on a WebSocket session.
    pub sink: WebSocketSinkSlot,
    /// Decoded events, identical in shape whichever wire delivered them.
    pub events: SseEventStream,
}

/// A transport that runs one stream over a WebSocket where one is available
/// and over HTTP+SSE otherwise.
#[derive(Clone)]
pub struct StreamTransport {
    http: HttpService,
    connector: Option<Arc<dyn WebSocketConnector>>,
    policy: ReconnectPolicy,
    metrics: StreamTransportMetrics,
}

impl StreamTransport {
    /// Build a transport with no WebSocket connector. This is heycode's current
    /// production configuration, so the fallback is the live path, not a
    /// contingency.
    #[must_use]
    pub fn http_only(http: HttpService) -> Self {
        Self {
            http,
            connector: None,
            policy: ReconnectPolicy::default(),
            metrics: StreamTransportMetrics::default(),
        }
    }

    /// Build a transport that will try the supplied connector first.
    #[must_use]
    pub fn with_connector(http: HttpService, connector: Arc<dyn WebSocketConnector>) -> Self {
        Self {
            http,
            connector: Some(connector),
            policy: ReconnectPolicy::default(),
            metrics: StreamTransportMetrics::default(),
        }
    }

    /// Replace the reconnect budget.
    #[must_use]
    pub fn with_reconnect_policy(mut self, policy: ReconnectPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Shared counters. The handle shares the counts; it does not copy them.
    #[must_use]
    pub fn metrics(&self) -> StreamTransportMetrics {
        self.metrics.clone()
    }

    /// Start one session. Nothing is dialled and nothing is counted until the
    /// event stream is polled.
    #[must_use = "the session runs only while its event stream is polled"]
    pub fn open(&self, plan: StreamPlan, cancellation: CancellationToken) -> StreamSession {
        let outcome = StreamOutcome::default();
        let sink = WebSocketSinkSlot::default();
        let state = OpenState {
            http_service: self.http.clone(),
            http: plan.http,
            websocket: plan.websocket,
            connector: self.connector.clone(),
            requirement: plan.requirement,
            policy: self.policy,
            metrics: self.metrics.clone(),
            outcome: outcome.clone(),
            sink: sink.clone(),
            cancellation,
        };
        StreamSession {
            outcome,
            sink,
            events: Box::pin(
                futures::stream::unfold(Phase::Open(Box::new(state)), drive_stream)
                    .flat_map(futures::stream::iter),
            ),
        }
    }
}

struct OpenState {
    http_service: HttpService,
    http: Option<HttpSseRequest>,
    websocket: Option<WebSocketRequest>,
    connector: Option<Arc<dyn WebSocketConnector>>,
    requirement: StreamRequirement,
    policy: ReconnectPolicy,
    metrics: StreamTransportMetrics,
    outcome: StreamOutcome,
    sink: WebSocketSinkSlot,
    cancellation: CancellationToken,
}

enum Phase {
    Open(Box<OpenState>),
    WebSocket {
        messages: WebSocketMessageStream,
        cancellation: CancellationToken,
        metrics: StreamTransportMetrics,
    },
    Http {
        events: SseEventStream,
    },
    Done,
}

type Batch = Vec<Result<SseEvent, TransportError>>;

async fn drive_stream(phase: Phase) -> Option<(Batch, Phase)> {
    match phase {
        Phase::Open(state) => Some(open_session(*state).await),
        Phase::WebSocket {
            mut messages,
            cancellation,
            metrics,
        } => {
            let message = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Some((vec![Err(TransportError::Cancelled)], Phase::Done));
                }
                message = messages.next() => message,
            };
            match message {
                Some(Ok(WebSocketMessage::Text(text))) => Some((
                    vec![Ok(SseEvent {
                        event: "message".to_owned(),
                        data: text,
                        id: None,
                        retry_ms: None,
                    })],
                    Phase::WebSocket {
                        messages,
                        cancellation,
                        metrics,
                    },
                )),
                // A binary frame is a framing failure, not an event with
                // guessed-at text. Inventing a lossy decode here would put
                // fabricated bytes into a protocol adapter.
                Some(Ok(WebSocketMessage::Binary(_))) => {
                    metrics.bump(|counters| &counters.websocket_stream_failures);
                    Some((
                        vec![Err(TransportError::InvalidSse {
                            message: "WebSocket binary frame cannot be framed as an event"
                                .to_owned(),
                        })],
                        Phase::Done,
                    ))
                }
                // The connection is established, so this is terminal: see the
                // committed-session rule in the module documentation.
                Some(Err(error)) => {
                    metrics.bump(|counters| &counters.websocket_stream_failures);
                    Some((vec![Err(error)], Phase::Done))
                }
                None => Some((Vec::new(), Phase::Done)),
            }
        }
        // Cancellation on this path is owned by the HTTP transport, which
        // already received the same token.
        Phase::Http { mut events } => events
            .next()
            .await
            .map(|item| (vec![item], Phase::Http { events })),
        Phase::Done => None,
    }
}

async fn open_session(state: OpenState) -> (Batch, Phase) {
    let OpenState {
        http_service,
        http,
        websocket,
        connector,
        requirement,
        policy,
        metrics,
        outcome,
        sink,
        cancellation,
    } = state;
    metrics.bump(|counters| &counters.sessions);
    if cancellation.is_cancelled() {
        return (vec![Err(TransportError::Cancelled)], Phase::Done);
    }

    let Some(request) = websocket else {
        // HTTP was the entire plan. No capability was offered and none was
        // withdrawn, so this is not a fallback and must not inflate that count.
        return start_http(
            &metrics,
            &outcome,
            &http_service,
            http,
            StreamOutcomeFacts {
                wire: Some(StreamWire::HttpSse),
                fallback: None,
                websocket_attempts: 0,
                reconnects: 0,
            },
            cancellation,
        );
    };

    let (connect_request, open_frames) = request.into_parts();
    let mut attempts = 0_u32;
    let reason = match connector {
        None => FallbackReason::NoConnector,
        Some(connector) => loop {
            attempts = attempts.saturating_add(1);
            metrics.bump(|counters| &counters.websocket_attempts);
            if attempts > 1 {
                metrics.bump(|counters| &counters.reconnects);
            }
            let connected = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return (vec![Err(TransportError::Cancelled)], Phase::Done);
                }
                connected = connector.connect(connect_request.clone(), cancellation.clone()) => connected,
            };
            match connected {
                Ok(connection) => {
                    metrics.bump(|counters| &counters.websocket_opens);
                    outcome.publish(StreamOutcomeFacts {
                        wire: Some(StreamWire::WebSocket),
                        fallback: None,
                        websocket_attempts: attempts,
                        reconnects: attempts.saturating_sub(1),
                    });
                    for frame in &open_frames {
                        let sent = tokio::select! {
                            biased;
                            () = cancellation.cancelled() => Err(TransportError::Cancelled),
                            sent = connection.sink.send_text(frame.clone(), cancellation.clone()) => sent,
                        };
                        if let Err(error) = sent {
                            if !matches!(error, TransportError::Cancelled) {
                                metrics.bump(|counters| &counters.websocket_stream_failures);
                            }
                            return (vec![Err(error)], Phase::Done);
                        }
                    }
                    metrics.bump(|counters| &counters.websocket_sessions);
                    sink.publish(connection.sink);
                    return (
                        Vec::new(),
                        Phase::WebSocket {
                            messages: connection.messages,
                            cancellation,
                            metrics,
                        },
                    );
                }
                Err(TransportError::Cancelled) => {
                    return (vec![Err(TransportError::Cancelled)], Phase::Done);
                }
                Err(_) => {
                    let Some(backoff) = policy.backoff_after(attempts) else {
                        break if policy.max_reconnects() == 0 {
                            FallbackReason::OpenFailed
                        } else {
                            metrics.bump(|counters| &counters.reconnect_budget_exhausted);
                            FallbackReason::ReconnectBudgetExhausted
                        };
                    };
                    let slept = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => false,
                        () = tokio::time::sleep(backoff) => true,
                    };
                    if !slept {
                        return (vec![Err(TransportError::Cancelled)], Phase::Done);
                    }
                }
            }
        },
    };

    // Every permitted open is spent. The decision below is one-way: this
    // session never returns to the WebSocket, which is what bounds the whole
    // machine at `1 + max_reconnects` opens plus at most one HTTP request.
    let facts = StreamOutcomeFacts {
        wire: None,
        fallback: Some(reason),
        websocket_attempts: attempts,
        reconnects: attempts.saturating_sub(1),
    };
    match (requirement, http) {
        (StreamRequirement::ServerStream, Some(http)) => {
            metrics.bump(|counters| &counters.fallbacks);
            start_http(
                &metrics,
                &outcome,
                &http_service,
                Some(http),
                StreamOutcomeFacts {
                    wire: Some(StreamWire::HttpSse),
                    ..facts
                },
                cancellation,
            )
        }
        (StreamRequirement::Bidirectional, _) => refuse(
            &metrics,
            &outcome,
            facts,
            "HTTP has no client-to-server channel, so it cannot carry a bidirectional stream",
        ),
        // Unreachable through the public constructors, which always give a
        // plan a usable wire. Reported rather than panicked.
        (StreamRequirement::ServerStream, None) => refuse(
            &metrics,
            &outcome,
            facts,
            "the plan supplied no HTTP fallback request",
        ),
    }
}

fn start_http(
    metrics: &StreamTransportMetrics,
    outcome: &StreamOutcome,
    http_service: &HttpService,
    request: Option<HttpSseRequest>,
    facts: StreamOutcomeFacts,
    cancellation: CancellationToken,
) -> (Batch, Phase) {
    let Some(request) = request else {
        return refuse(
            metrics,
            outcome,
            StreamOutcomeFacts {
                wire: None,
                ..facts
            },
            "the plan supplied no HTTP request",
        );
    };
    metrics.bump(|counters| &counters.http_sessions);
    outcome.publish(facts);
    (
        Vec::new(),
        Phase::Http {
            events: http_service.sse(request, cancellation),
        },
    )
}

fn refuse(
    metrics: &StreamTransportMetrics,
    outcome: &StreamOutcome,
    facts: StreamOutcomeFacts,
    detail: &str,
) -> (Batch, Phase) {
    metrics.bump(|counters| &counters.fallbacks_refused);
    let reason = facts
        .fallback
        .map_or("the WebSocket was unavailable", FallbackReason::describe);
    let message = format!("WebSocket transport unavailable ({reason}); {detail}");
    outcome.publish(facts);
    (
        vec![Err(TransportError::InvalidRequest {
            field: "transport",
            message,
        })],
        Phase::Done,
    )
}

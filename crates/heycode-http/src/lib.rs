//! Plugin-owned HTTP transport, semantic body-redacted error metadata and
//! provider-neutral SSE framing.

mod plugin;
mod sse;
mod stream;
mod transport;

pub use plugin::http_plugin;
pub use sse::{SseDecodeError, SseDecoder, SseEvent};
pub use stream::{
    FallbackReason, ReconnectPolicy, StreamOutcome, StreamOutcomeFacts, StreamPlan,
    StreamRequirement, StreamSession, StreamTransport, StreamTransportMetrics,
    StreamTransportMetricsSnapshot, StreamWire, WebSocketConnectFuture, WebSocketConnectRequest,
    WebSocketConnection, WebSocketConnector, WebSocketMessage, WebSocketMessageStream,
    WebSocketRequest, WebSocketSendFuture, WebSocketSink, WebSocketSinkSlot,
};
pub use transport::{
    BufferedResponseFuture, HttpBodyStream, HttpErrorBody, HttpErrorMetadata, HttpHeader,
    HttpMethod, HttpRequest, HttpResponse, HttpRetryAfter, HttpService, HttpSseRequest,
    HttpStreamingResponse, HttpTransport, ReqwestHttpTransport, SseEventStream, SseExchange,
    SseResponseHeaders, StreamingResponseFuture, TransportError,
};

/// Shared HTTP transport service.
pub const SERVICE_HTTP: heycode_core::ServiceKey = heycode_core::ServiceKey::new("http");

//! MCP11 client-side interaction and human-event routing.
//!
//! MCP connections are protocol objects, while elicitation and human progress
//! belong to one product session. This module joins those facts with an opaque
//! route and never guesses from whichever UI happens to be focused. Server
//! requests, progress tokens and URL-elicitation ids are exact owned rows:
//! completion, cancellation or transport shutdown retires only the row that
//! operation created.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use futures::FutureExt as _;
use tokio_util::sync::CancellationToken;

use crate::McpServerId;

const MAX_ROUTE_BYTES: usize = 256;
const MAX_REQUEST_ID_BYTES: usize = 256;
const MAX_ELICITATION_ID_BYTES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_SCHEMA_BYTES: usize = 128 * 1024;
const MAX_FORM_PROPERTIES: usize = 64;
const MAX_LOG_BYTES: usize = 64 * 1024;
const MAX_LOGGER_BYTES: usize = 256;
const MAX_PROGRESS_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_PENDING_ELICITATIONS: usize = 16;

/// Opaque product-session/UI route for one MCP connection.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpClientRoute(String);

impl McpClientRoute {
    /// Validate a route minted by the product host.
    ///
    /// # Errors
    /// Routes must be 1..=256 visible ASCII bytes. They are never accepted from
    /// the MCP server and never rendered in `Debug`.
    pub fn new(value: impl Into<String>) -> Result<Self, McpClientEventError> {
        let value = value.into();
        if !bounded_visible_ascii(&value, MAX_ROUTE_BYTES) {
            return Err(McpClientEventError::InvalidRoute);
        }
        Ok(Self(value))
    }

    /// Explicit host-only access to the route value.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for McpClientRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("McpClientRoute([REDACTED])")
    }
}

/// Which 2025-11-25 elicitation modes the client advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpElicitationCapabilities {
    form: bool,
    url: bool,
}

impl McpElicitationCapabilities {
    /// Form mode only.
    #[must_use]
    pub const fn form_only() -> Self {
        Self {
            form: true,
            url: false,
        }
    }

    /// Form and URL modes.
    #[must_use]
    pub const fn form_and_url() -> Self {
        Self {
            form: true,
            url: true,
        }
    }

    /// Whether form elicitation is advertised.
    #[must_use]
    pub const fn form(self) -> bool {
        self.form
    }

    /// Whether URL elicitation is advertised.
    #[must_use]
    pub const fn url(self) -> bool {
        self.url
    }

    /// Exact `ClientCapabilities.elicitation` object.
    #[must_use]
    pub fn wire_value(self) -> serde_json::Value {
        let mut modes = serde_json::Map::new();
        if self.form {
            modes.insert("form".to_owned(), serde_json::json!({}));
        }
        if self.url {
            modes.insert("url".to_owned(), serde_json::json!({}));
        }
        serde_json::Value::Object(modes)
    }
}

/// Stable MCP logging severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpLogLevel {
    /// Detailed debugging output.
    Debug,
    /// General information.
    Info,
    /// Normal but significant event.
    Notice,
    /// Warning condition.
    Warning,
    /// Operation error.
    Error,
    /// Critical failure.
    Critical,
    /// Immediate action is required.
    Alert,
    /// The server considers itself unusable.
    Emergency,
}

impl McpLogLevel {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "notice" => Some(Self::Notice),
            "warning" => Some(Self::Warning),
            "error" => Some(Self::Error),
            "critical" => Some(Self::Critical),
            "alert" => Some(Self::Alert),
            "emergency" => Some(Self::Emergency),
            _ => None,
        }
    }

    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
            Self::Alert => "alert",
            Self::Emergency => "emergency",
        }
    }
}

/// Progress notification routed to one product session.
#[derive(Clone, PartialEq)]
pub struct McpProgressEvent {
    route: McpClientRoute,
    server: McpServerId,
    progress: f64,
    total: Option<f64>,
    message: Option<String>,
}

impl McpProgressEvent {
    /// Owning product route.
    #[must_use]
    pub const fn route(&self) -> &McpClientRoute {
        &self.route
    }

    /// Server that emitted the update.
    #[must_use]
    pub const fn server(&self) -> &McpServerId {
        &self.server
    }

    /// Monotonic progress value.
    #[must_use]
    pub const fn progress(&self) -> f64 {
        self.progress
    }

    /// Optional total.
    #[must_use]
    pub const fn total(&self) -> Option<f64> {
        self.total
    }

    /// Optional bounded human message.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

impl std::fmt::Debug for McpProgressEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpProgressEvent")
            .field("route", &self.route)
            .field("server", &self.server)
            .field("progress", &self.progress)
            .field("total", &self.total)
            .field("message_len", &self.message.as_ref().map(String::len))
            .finish()
    }
}

/// Bounded structured log routed to one product session.
#[derive(Clone, PartialEq)]
pub struct McpLogEvent {
    route: McpClientRoute,
    server: McpServerId,
    level: McpLogLevel,
    logger: Option<String>,
    data: serde_json::Value,
}

impl McpLogEvent {
    /// Owning product route.
    #[must_use]
    pub const fn route(&self) -> &McpClientRoute {
        &self.route
    }

    /// Emitting server.
    #[must_use]
    pub const fn server(&self) -> &McpServerId {
        &self.server
    }

    /// Structured severity.
    #[must_use]
    pub const fn level(&self) -> McpLogLevel {
        self.level
    }

    /// Optional logger name.
    #[must_use]
    pub fn logger(&self) -> Option<&str> {
        self.logger.as_deref()
    }

    /// Server-controlled JSON for an explicitly human-only log surface.
    #[must_use]
    pub const fn data(&self) -> &serde_json::Value {
        &self.data
    }
}

impl std::fmt::Debug for McpLogEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpLogEvent")
            .field("route", &self.route)
            .field("server", &self.server)
            .field("level", &self.level)
            .field("logger_len", &self.logger.as_ref().map(String::len))
            .field("data_bytes", &json_len(&self.data))
            .finish()
    }
}

/// Human-only MCP event. This type has no session/model projection.
#[derive(Clone, PartialEq)]
pub enum McpClientEvent {
    /// Request-scoped progress.
    Progress(McpProgressEvent),
    /// Connection-scoped structured logging.
    Log(McpLogEvent),
    /// A previously accepted URL elicitation completed out of band.
    ElicitationComplete {
        /// Owning product route.
        route: McpClientRoute,
        /// Emitting server.
        server: McpServerId,
    },
}

impl McpClientEvent {
    /// Owning product route.
    #[must_use]
    pub const fn route(&self) -> &McpClientRoute {
        match self {
            Self::Progress(event) => event.route(),
            Self::Log(event) => event.route(),
            Self::ElicitationComplete { route, .. } => route,
        }
    }
}

impl std::fmt::Debug for McpClientEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Progress(event) => event.fmt(formatter),
            Self::Log(event) => event.fmt(formatter),
            Self::ElicitationComplete { route, server } => formatter
                .debug_struct("McpElicitationComplete")
                .field("route", route)
                .field("server", server)
                .finish(),
        }
    }
}

/// Product-owned human event sink.
pub trait McpClientEventSink: Send + Sync {
    /// Publish one already-routed, bounded human-only event.
    fn publish(&self, event: McpClientEvent);
}

/// Form or URL elicitation request after strict wire validation.
#[derive(Clone)]
pub struct McpElicitationRequest {
    route: McpClientRoute,
    server: McpServerId,
    message: String,
    mode: McpElicitationMode,
}

impl McpElicitationRequest {
    /// Exact product route.
    #[must_use]
    pub const fn route(&self) -> &McpClientRoute {
        &self.route
    }

    /// Requesting server.
    #[must_use]
    pub const fn server(&self) -> &McpServerId {
        &self.server
    }

    /// Human explanation supplied by the server.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Validated mode and mode-specific fields.
    #[must_use]
    pub const fn mode(&self) -> &McpElicitationMode {
        &self.mode
    }
}

impl std::fmt::Debug for McpElicitationRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpElicitationRequest")
            .field("route", &self.route)
            .field("server", &self.server)
            .field("message_len", &self.message.len())
            .field("mode", &self.mode)
            .finish()
    }
}

/// Validated elicitation mode.
#[derive(Clone)]
pub enum McpElicitationMode {
    /// In-band flat primitive form.
    Form {
        /// Restricted JSON Schema.
        schema: McpElicitationSchema,
    },
    /// Out-of-band HTTPS interaction.
    Url {
        /// Opaque server-generated lifecycle id.
        elicitation_id: McpElicitationId,
        /// Credential-free HTTPS URL shown to the user before navigation.
        url: String,
    },
}

impl std::fmt::Debug for McpElicitationMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Form { schema } => formatter
                .debug_struct("Form")
                .field("properties", &schema.property_count())
                .finish(),
            Self::Url { elicitation_id, .. } => formatter
                .debug_struct("Url")
                .field("elicitation_id", elicitation_id)
                .field("url", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Opaque URL-elicitation lifecycle id.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct McpElicitationId(String);

impl McpElicitationId {
    /// Explicit host access to the opaque id.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for McpElicitationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("McpElicitationId([REDACTED])")
    }
}

/// Restricted form schema retained exactly for a UI form renderer.
#[derive(Clone)]
pub struct McpElicitationSchema(serde_json::Value);

impl McpElicitationSchema {
    /// Exact validated schema.
    #[must_use]
    pub const fn value(&self) -> &serde_json::Value {
        &self.0
    }

    fn property_count(&self) -> usize {
        self.0
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .map_or(0, serde_json::Map::len)
    }
}

impl std::fmt::Debug for McpElicitationSchema {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpElicitationSchema")
            .field("properties", &self.property_count())
            .field("bytes", &json_len(&self.0))
            .finish()
    }
}

/// Product response to one elicitation.
#[derive(Clone)]
pub struct McpElicitationResponse {
    action: McpElicitationAction,
    content: Option<serde_json::Value>,
}

impl McpElicitationResponse {
    /// Explicit form acceptance carrying reviewed content.
    #[must_use]
    pub fn accept(content: serde_json::Value) -> Self {
        Self {
            action: McpElicitationAction::Accept,
            content: Some(content),
        }
    }

    /// Accept a URL-mode interaction, which carries no in-band content.
    #[must_use]
    pub const fn accept_url() -> Self {
        Self {
            action: McpElicitationAction::Accept,
            content: None,
        }
    }

    /// Explicit decline.
    #[must_use]
    pub const fn decline() -> Self {
        Self {
            action: McpElicitationAction::Decline,
            content: None,
        }
    }

    /// Dismiss without a decision.
    #[must_use]
    pub const fn cancel() -> Self {
        Self {
            action: McpElicitationAction::Cancel,
            content: None,
        }
    }
}

impl std::fmt::Debug for McpElicitationResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpElicitationResponse")
            .field("action", &self.action)
            .field("content_bytes", &self.content.as_ref().map(json_len))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpElicitationAction {
    Accept,
    Decline,
    Cancel,
}

impl McpElicitationAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

/// Closed handler failure. No handler text enters the protocol response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpElicitationFailure {
    /// No eligible UI exists for this route.
    Unavailable,
    /// Host UI failed before reaching a decision.
    Failed,
    /// The operation was cancelled.
    Cancelled,
}

/// Product-owned UI broker for server elicitation.
#[async_trait]
pub trait McpElicitationHandler: Send + Sync {
    /// Ask the user and return one typed response.
    ///
    /// # Errors
    /// Closed UI/lifecycle failure. Free-form handler errors are deliberately
    /// absent so they cannot become server-visible protocol text.
    async fn elicit(
        &self,
        request: McpElicitationRequest,
        cancellation: CancellationToken,
    ) -> Result<McpElicitationResponse, McpElicitationFailure>;
}

/// Safe MCP11 boundary failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpClientEventError {
    /// Invalid host route.
    #[error("MCP client route is invalid")]
    InvalidRoute,
}

/// JSON-RPC response to a server-originated request.
#[derive(Clone)]
pub struct McpClientReply(serde_json::Value);

impl McpClientReply {
    /// Exact bounded response frame.
    #[must_use]
    pub const fn as_json(&self) -> &serde_json::Value {
        &self.0
    }

    /// Consume into the exact response frame for a transport writer.
    #[must_use]
    pub fn into_json(self) -> serde_json::Value {
        self.0
    }

    pub(crate) fn method_not_found(frame: &serde_json::Value) -> Self {
        let id = frame.get("id").and_then(McpJsonRpcId::parse);
        Self::error(id.as_ref(), -32601, "client method is not supported")
    }

    pub(crate) fn empty_result(frame: &serde_json::Value) -> Self {
        let id = frame.get("id").and_then(McpJsonRpcId::parse);
        id.as_ref().map_or_else(
            || Self::error(None, -32600, "request id is invalid"),
            |id| Self::result(id, serde_json::json!({})),
        )
    }

    pub(crate) fn overloaded(frame: &serde_json::Value) -> Self {
        let id = frame.get("id").and_then(McpJsonRpcId::parse);
        Self::error(
            id.as_ref(),
            -32000,
            "too many MCP client requests are pending",
        )
    }

    fn result(id: &McpJsonRpcId, result: serde_json::Value) -> Self {
        Self(serde_json::json!({"jsonrpc":"2.0","id":id.to_json(),"result":result}))
    }

    fn error(id: Option<&McpJsonRpcId>, code: i64, message: &'static str) -> Self {
        Self(serde_json::json!({
            "jsonrpc":"2.0",
            "id":id.map_or(serde_json::Value::Null, McpJsonRpcId::to_json),
            "error":{"code":code,"message":message}
        }))
    }
}

impl std::fmt::Debug for McpClientReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpClientReply")
            .field("is_error", &self.0.get("error").is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum McpJsonRpcId {
    Signed(i64),
    Unsigned(u64),
    String(String),
}

impl McpJsonRpcId {
    fn parse(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(value)
                if bounded_opaque_text(value, MAX_REQUEST_ID_BYTES) =>
            {
                Some(Self::String(value.clone()))
            }
            serde_json::Value::Number(value) => value
                .as_i64()
                .map(Self::Signed)
                .or_else(|| value.as_u64().map(Self::Unsigned)),
            _ => None,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Signed(value) => serde_json::json!(value),
            Self::Unsigned(value) => serde_json::json!(value),
            Self::String(value) => serde_json::json!(value),
        }
    }
}

/// Opaque progress token generated by the client for one active request.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct McpProgressToken(String);

impl McpProgressToken {
    /// Exact JSON value inserted into request `_meta.progressToken`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!(self.0)
    }

    /// Explicit transport-only wire access.
    #[must_use]
    pub fn expose_for_wire(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for McpProgressToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("McpProgressToken([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ObservedProgressToken {
    Signed(i64),
    Unsigned(u64),
    String(String),
}

impl ObservedProgressToken {
    fn parse(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(value)
                if bounded_opaque_text(value, MAX_REQUEST_ID_BYTES) =>
            {
                Some(Self::String(value.clone()))
            }
            serde_json::Value::Number(value) => value
                .as_i64()
                .map(Self::Signed)
                .or_else(|| value.as_u64().map(Self::Unsigned)),
            _ => None,
        }
    }
}

struct PendingRow {
    serial: u64,
    cancellation: CancellationToken,
}

struct ProgressRow {
    serial: u64,
    last: Option<f64>,
}

struct RouterInner {
    server: McpServerId,
    route: McpClientRoute,
    capabilities: McpElicitationCapabilities,
    handler: Arc<dyn McpElicitationHandler>,
    sink: Arc<dyn McpClientEventSink>,
    pending: Mutex<HashMap<McpJsonRpcId, PendingRow>>,
    progress: Mutex<HashMap<ObservedProgressToken, ProgressRow>>,
    url_elicitations: Mutex<HashMap<McpElicitationId, u64>>,
    serial: AtomicU64,
    logging: AtomicBool,
    shutdown: AtomicBool,
}

/// One connection's exact MCP11 routing and pending-interaction owner.
#[derive(Clone)]
pub struct McpClientEventRouter {
    inner: Arc<RouterInner>,
}

impl std::fmt::Debug for McpClientEventRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpClientEventRouter")
            .field("server", &self.inner.server)
            .field("route", &self.inner.route)
            .field("pending", &self.pending_elicitations())
            .finish_non_exhaustive()
    }
}

impl McpClientEventRouter {
    /// Bind one server connection to one exact product route and UI broker.
    #[must_use]
    pub fn new(
        server: McpServerId,
        route: McpClientRoute,
        capabilities: McpElicitationCapabilities,
        handler: Arc<dyn McpElicitationHandler>,
        sink: Arc<dyn McpClientEventSink>,
    ) -> Self {
        Self {
            inner: Arc::new(RouterInner {
                server,
                route,
                capabilities,
                handler,
                sink,
                pending: Mutex::new(HashMap::new()),
                progress: Mutex::new(HashMap::new()),
                url_elicitations: Mutex::new(HashMap::new()),
                serial: AtomicU64::new(1),
                logging: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
            }),
        }
    }

    /// Client capability object to include in `initialize`.
    #[must_use]
    pub fn client_capabilities(&self) -> serde_json::Value {
        serde_json::json!({"elicitation": self.inner.capabilities.wire_value()})
    }

    /// Server identity this route is bound to.
    #[must_use]
    pub fn server(&self) -> &McpServerId {
        &self.inner.server
    }

    /// Mark server logging as negotiated and human-visible.
    pub fn enable_logging(&self) {
        self.inner.logging.store(true, Ordering::SeqCst);
    }

    /// Number of currently pending server elicitation requests.
    #[must_use]
    pub fn pending_elicitations(&self) -> usize {
        self.inner.pending.lock().map_or(0, |pending| pending.len())
    }

    /// Reserve a unique progress token for one active client request.
    #[must_use]
    pub fn begin_progress(&self) -> McpProgressRegistration {
        let serial = self.next_serial();
        let token = McpProgressToken(format!("heycode-{}", uuid::Uuid::new_v4()));
        let key = ObservedProgressToken::String(token.0.clone());
        if !self.inner.shutdown.load(Ordering::SeqCst)
            && let Ok(mut progress) = self.inner.progress.lock()
        {
            progress.insert(key.clone(), ProgressRow { serial, last: None });
        }
        McpProgressRegistration {
            router: Arc::downgrade(&self.inner),
            key,
            token,
            serial,
            active: true,
        }
    }

    /// Admit one server-originated request.
    ///
    /// `Ok(None)` means the method belongs to another client feature. A
    /// protocol error is already correlated to the request id and safe to send.
    ///
    /// # Errors
    /// Malformed, unsupported or duplicate elicitation requests.
    pub fn admit_request(
        &self,
        frame: &serde_json::Value,
    ) -> Result<Option<McpPendingElicitation>, McpClientReply> {
        let method = frame.get("method").and_then(serde_json::Value::as_str);
        if method != Some("elicitation/create") {
            return Ok(None);
        }
        let id = frame.get("id").and_then(McpJsonRpcId::parse);
        let Some(id) = id else {
            return Err(McpClientReply::error(
                None,
                -32600,
                "elicitation request id is invalid",
            ));
        };
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(McpClientReply::error(
                Some(&id),
                -32603,
                "MCP client interaction owner is shutting down",
            ));
        }
        let request = parse_elicitation_request(
            frame,
            self.inner.route.clone(),
            self.inner.server.clone(),
            self.inner.capabilities,
        )
        .map_err(|()| McpClientReply::error(Some(&id), -32602, "elicitation request is invalid"))?;
        if let McpElicitationMode::Url { elicitation_id, .. } = request.mode()
            && self
                .inner
                .url_elicitations
                .lock()
                .is_ok_and(|ids| ids.contains_key(elicitation_id))
        {
            return Err(McpClientReply::error(
                Some(&id),
                -32602,
                "elicitation id is already active",
            ));
        }
        let serial = self.next_serial();
        let cancellation = CancellationToken::new();
        enum Admission {
            Inserted,
            Duplicate,
            Full,
            Unavailable,
        }
        let admission = self
            .inner
            .pending
            .lock()
            .map_or(Admission::Unavailable, |mut pending| {
                use std::collections::hash_map::Entry;
                let full = pending.len() >= MAX_PENDING_ELICITATIONS;
                match pending.entry(id.clone()) {
                    Entry::Vacant(_) if full => Admission::Full,
                    Entry::Vacant(slot) => {
                        slot.insert(PendingRow {
                            serial,
                            cancellation: cancellation.clone(),
                        });
                        Admission::Inserted
                    }
                    Entry::Occupied(_) => Admission::Duplicate,
                }
            });
        match admission {
            Admission::Inserted => {}
            Admission::Duplicate => {
                return Err(McpClientReply::error(
                    Some(&id),
                    -32600,
                    "request id is already pending",
                ));
            }
            Admission::Full => {
                return Err(McpClientReply::error(
                    Some(&id),
                    -32000,
                    "too many MCP elicitation requests are pending",
                ));
            }
            Admission::Unavailable => {
                return Err(McpClientReply::error(
                    Some(&id),
                    -32603,
                    "MCP client interaction registry is unavailable",
                ));
            }
        }
        Ok(Some(McpPendingElicitation {
            router: Arc::downgrade(&self.inner),
            handler: Arc::clone(&self.inner.handler),
            id,
            serial,
            request: Some(request),
            cancellation,
            active: true,
        }))
    }

    /// Observe a progress/logging/cancellation/completion notification.
    ///
    /// Returns true only when an exact active row accepted the notification or
    /// a negotiated log was published. Unknown, late and malformed messages
    /// are ignored as MCP cancellation/progress require.
    pub fn observe_notification(&self, frame: &serde_json::Value) -> bool {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return false;
        }
        if frame.get("id").is_some() {
            return false;
        }
        match frame.get("method").and_then(serde_json::Value::as_str) {
            Some("notifications/cancelled") => self.cancel_from_notification(frame),
            Some("notifications/progress") => self.observe_progress(frame),
            Some("notifications/message") => self.observe_log(frame),
            Some("notifications/elicitation/complete") => self.observe_elicitation_complete(frame),
            _ => false,
        }
    }

    /// Retire every transport-owned pending row and cancel exact handlers.
    ///
    /// Terminal: the route is disposed and every later request, notification
    /// and event is refused. Use [`Self::retire_pending`] when the transport
    /// underneath the route is being replaced rather than retired.
    pub fn shutdown(&self) {
        if self.inner.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        self.retire_rows();
    }

    /// Retire the rows one dead transport owned, keeping the route usable.
    ///
    /// A stdio child that crashes is not the end of the routing plane: the
    /// bounded reconnect supervisor spawns a replacement that reuses this exact
    /// router, and the product UI holds the same handle for the whole session.
    /// Rows correlated to the dead child cannot be answered, so they are
    /// cancelled — but the route itself must survive, or the recovered
    /// connection would look Ready while every elicitation was refused and
    /// every log and progress frame silently dropped.
    pub fn retire_pending(&self) {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        self.retire_rows();
    }

    fn retire_rows(&self) {
        if let Ok(mut pending) = self.inner.pending.lock() {
            for row in pending.values() {
                row.cancellation.cancel();
            }
            pending.clear();
        }
        if let Ok(mut progress) = self.inner.progress.lock() {
            progress.clear();
        }
        if let Ok(mut ids) = self.inner.url_elicitations.lock() {
            ids.clear();
        }
    }

    fn next_serial(&self) -> u64 {
        self.inner.serial.fetch_add(1, Ordering::SeqCst)
    }

    fn cancel_from_notification(&self, frame: &serde_json::Value) -> bool {
        let Some(id) = frame
            .get("params")
            .and_then(|params| params.get("requestId"))
            .and_then(McpJsonRpcId::parse)
        else {
            return false;
        };
        let row = self
            .inner
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&id));
        if let Some(row) = row {
            row.cancellation.cancel();
            true
        } else {
            false
        }
    }

    fn observe_progress(&self, frame: &serde_json::Value) -> bool {
        let Some(params) = frame.get("params").and_then(serde_json::Value::as_object) else {
            return false;
        };
        let Some(token) = params
            .get("progressToken")
            .and_then(ObservedProgressToken::parse)
        else {
            return false;
        };
        let Some(progress_value) = params.get("progress").and_then(serde_json::Value::as_f64)
        else {
            return false;
        };
        if !progress_value.is_finite() {
            return false;
        }
        let total = match params.get("total") {
            None => None,
            Some(value) => match value.as_f64() {
                Some(total) if total.is_finite() => Some(total),
                _ => return false,
            },
        };
        let message = match params.get("message") {
            None => None,
            Some(serde_json::Value::String(message))
                if bounded_human_text(message, MAX_PROGRESS_MESSAGE_BYTES) =>
            {
                Some(message.clone())
            }
            Some(_) => return false,
        };
        let accepted = self.inner.progress.lock().is_ok_and(|mut active| {
            let Some(row) = active.get_mut(&token) else {
                return false;
            };
            if row.last.is_some_and(|last| progress_value <= last) {
                return false;
            }
            row.last = Some(progress_value);
            true
        });
        if !accepted {
            return false;
        }
        self.publish(McpClientEvent::Progress(McpProgressEvent {
            route: self.inner.route.clone(),
            server: self.inner.server.clone(),
            progress: progress_value,
            total,
            message,
        }));
        true
    }

    fn observe_log(&self, frame: &serde_json::Value) -> bool {
        if !self.inner.logging.load(Ordering::SeqCst) {
            return false;
        }
        let Some(params) = frame.get("params").and_then(serde_json::Value::as_object) else {
            return false;
        };
        let Some(level) = params
            .get("level")
            .and_then(serde_json::Value::as_str)
            .and_then(McpLogLevel::parse)
        else {
            return false;
        };
        let logger = match params.get("logger") {
            None => None,
            Some(serde_json::Value::String(logger))
                if bounded_human_text(logger, MAX_LOGGER_BYTES) && !logger.contains('\n') =>
            {
                Some(logger.clone())
            }
            Some(_) => return false,
        };
        let Some(data) = params.get("data") else {
            return false;
        };
        if json_len(data) > MAX_LOG_BYTES {
            return false;
        }
        self.publish(McpClientEvent::Log(McpLogEvent {
            route: self.inner.route.clone(),
            server: self.inner.server.clone(),
            level,
            logger,
            data: data.clone(),
        }));
        true
    }

    fn observe_elicitation_complete(&self, frame: &serde_json::Value) -> bool {
        let Some(id) = frame
            .get("params")
            .and_then(|params| params.get("elicitationId"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| bounded_opaque_text(id, MAX_ELICITATION_ID_BYTES))
            .map(|id| McpElicitationId(id.to_owned()))
        else {
            return false;
        };
        let removed = self
            .inner
            .url_elicitations
            .lock()
            .is_ok_and(|mut active| active.remove(&id).is_some());
        if removed {
            self.publish(McpClientEvent::ElicitationComplete {
                route: self.inner.route.clone(),
                server: self.inner.server.clone(),
            });
        }
        removed
    }

    fn publish(&self, event: McpClientEvent) {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let sink = Arc::clone(&self.inner.sink);
        let _contained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sink.publish(event);
        }));
    }
}

/// Exact ownership handle for one active progress token.
pub struct McpProgressRegistration {
    router: Weak<RouterInner>,
    key: ObservedProgressToken,
    token: McpProgressToken,
    serial: u64,
    active: bool,
}

impl McpProgressRegistration {
    /// Token inserted into the request metadata.
    #[must_use]
    pub const fn token(&self) -> &McpProgressToken {
        &self.token
    }
}

impl std::fmt::Debug for McpProgressRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpProgressRegistration")
            .field("token", &self.token)
            .field("active", &self.active)
            .finish()
    }
}

impl Drop for McpProgressRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(router) = self.router.upgrade() else {
            return;
        };
        if let Ok(mut progress) = router.progress.lock()
            && progress
                .get(&self.key)
                .is_some_and(|row| row.serial == self.serial)
        {
            progress.remove(&self.key);
        }
    }
}

/// One admitted server elicitation. Dropping it cancels and retires its row.
pub struct McpPendingElicitation {
    router: Weak<RouterInner>,
    handler: Arc<dyn McpElicitationHandler>,
    id: McpJsonRpcId,
    serial: u64,
    request: Option<McpElicitationRequest>,
    cancellation: CancellationToken,
    active: bool,
}

impl McpPendingElicitation {
    /// Run the product handler and build the exact JSON-RPC response.
    ///
    /// `None` means cancellation won; MCP requires no later response for a
    /// cancelled request.
    #[must_use]
    pub async fn resolve(mut self) -> Option<McpClientReply> {
        let request = self.request.take()?;
        let request_for_validation = request.clone();
        let invocation =
            std::panic::AssertUnwindSafe(self.handler.elicit(request, self.cancellation.clone()))
                .catch_unwind();
        let result = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(McpElicitationFailure::Cancelled),
            result = invocation => match result {
                Ok(result) => result,
                Err(_) => Err(McpElicitationFailure::Failed),
            },
        };
        let reply = match result {
            Err(McpElicitationFailure::Cancelled) => None,
            Err(_) => Some(McpClientReply::error(
                Some(&self.id),
                -32603,
                "elicitation handler failed",
            )),
            Ok(response) => match validated_response(&request_for_validation, response) {
                Ok((result, completed_url)) => {
                    if let Some(elicitation_id) = completed_url {
                        let inserted = self.router.upgrade().is_some_and(|router| {
                            router.url_elicitations.lock().is_ok_and(|mut active| {
                                active.insert(elicitation_id, self.serial).is_none()
                            })
                        });
                        if !inserted {
                            Some(McpClientReply::error(
                                Some(&self.id),
                                -32603,
                                "elicitation lifecycle conflicted",
                            ))
                        } else {
                            Some(McpClientReply::result(&self.id, result))
                        }
                    } else {
                        Some(McpClientReply::result(&self.id, result))
                    }
                }
                Err(()) => Some(McpClientReply::error(
                    Some(&self.id),
                    -32603,
                    "elicitation response is invalid",
                )),
            },
        };
        self.settle(false);
        reply
    }

    fn settle(&mut self, cancel: bool) {
        if !self.active {
            return;
        }
        self.active = false;
        if cancel {
            self.cancellation.cancel();
        }
        let Some(router) = self.router.upgrade() else {
            return;
        };
        if let Ok(mut pending) = router.pending.lock()
            && pending
                .get(&self.id)
                .is_some_and(|row| row.serial == self.serial)
        {
            pending.remove(&self.id);
        }
    }
}

impl std::fmt::Debug for McpPendingElicitation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpPendingElicitation")
            .field("active", &self.active)
            .finish()
    }
}

impl Drop for McpPendingElicitation {
    fn drop(&mut self) {
        self.settle(true);
    }
}

fn parse_elicitation_request(
    frame: &serde_json::Value,
    route: McpClientRoute,
    server: McpServerId,
    capabilities: McpElicitationCapabilities,
) -> Result<McpElicitationRequest, ()> {
    let params = frame
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or(())?;
    let message = params
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|message| bounded_human_text(message, MAX_MESSAGE_BYTES))
        .ok_or(())?
        .to_owned();
    let mode = params
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("form");
    let mode = match mode {
        "form" if capabilities.form => {
            let schema = params.get("requestedSchema").ok_or(())?.clone();
            validate_form_schema(&schema)?;
            McpElicitationMode::Form {
                schema: McpElicitationSchema(schema),
            }
        }
        "url" if capabilities.url => {
            let elicitation_id = params
                .get("elicitationId")
                .and_then(serde_json::Value::as_str)
                .filter(|id| bounded_opaque_text(id, MAX_ELICITATION_ID_BYTES))
                .ok_or(())?;
            let url = params
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or(())?;
            validate_elicitation_url(url)?;
            McpElicitationMode::Url {
                elicitation_id: McpElicitationId(elicitation_id.to_owned()),
                url: url.to_owned(),
            }
        }
        _ => return Err(()),
    };
    Ok(McpElicitationRequest {
        route,
        server,
        message,
        mode,
    })
}

fn validated_response(
    request: &McpElicitationRequest,
    response: McpElicitationResponse,
) -> Result<(serde_json::Value, Option<McpElicitationId>), ()> {
    match (response.action, request.mode(), response.content) {
        (McpElicitationAction::Accept, McpElicitationMode::Form { schema }, Some(content)) => {
            validate_form_content(schema.value(), &content)?;
            Ok((
                serde_json::json!({"action":"accept","content":content}),
                None,
            ))
        }
        (McpElicitationAction::Accept, McpElicitationMode::Url { elicitation_id, .. }, None) => {
            Ok((
                serde_json::json!({"action":"accept"}),
                Some(elicitation_id.clone()),
            ))
        }
        (McpElicitationAction::Decline, _, None) | (McpElicitationAction::Cancel, _, None) => {
            Ok((serde_json::json!({"action":response.action.as_str()}), None))
        }
        _ => Err(()),
    }
}

fn validate_elicitation_url(value: &str) -> Result<(), ()> {
    if value.len() > 4 * 1024 {
        return Err(());
    }
    let url = url::Url::parse(value).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    Ok(())
}

fn validate_form_schema(schema: &serde_json::Value) -> Result<(), ()> {
    if json_len(schema) > MAX_SCHEMA_BYTES {
        return Err(());
    }
    let object = schema.as_object().ok_or(())?;
    only_keys(object, &["$schema", "type", "properties", "required"])?;
    if object.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        return Err(());
    }
    let properties = object
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or(())?;
    if properties.len() > MAX_FORM_PROPERTIES {
        return Err(());
    }
    for (name, definition) in properties {
        if !bounded_visible_ascii(name, 128) {
            return Err(());
        }
        validate_primitive_schema(definition)?;
    }
    if let Some(required) = object.get("required") {
        let required = required.as_array().ok_or(())?;
        let mut seen = std::collections::BTreeSet::new();
        for name in required {
            let name = name.as_str().ok_or(())?;
            if !properties.contains_key(name) || !seen.insert(name) {
                return Err(());
            }
        }
    }
    Ok(())
}

fn validate_primitive_schema(value: &serde_json::Value) -> Result<(), ()> {
    let schema = value.as_object().ok_or(())?;
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => {
            only_keys(
                schema,
                &[
                    "type",
                    "title",
                    "description",
                    "minLength",
                    "maxLength",
                    "pattern",
                    "format",
                    "default",
                    "enum",
                    "oneOf",
                ],
            )?;
            if let Some(pattern) = schema.get("pattern").and_then(serde_json::Value::as_str) {
                regex::Regex::new(pattern).map_err(|_| ())?;
            }
            if let Some(format) = schema.get("format").and_then(serde_json::Value::as_str)
                && !matches!(format, "email" | "uri" | "date" | "date-time")
            {
                return Err(());
            }
            validate_choice_schema(schema)?;
            validate_default(schema)
        }
        Some("number" | "integer") => {
            only_keys(
                schema,
                &[
                    "type",
                    "title",
                    "description",
                    "minimum",
                    "maximum",
                    "default",
                ],
            )?;
            validate_default(schema)
        }
        Some("boolean") => {
            only_keys(schema, &["type", "title", "description", "default"])?;
            validate_default(schema)
        }
        Some("array") => {
            only_keys(
                schema,
                &[
                    "type",
                    "title",
                    "description",
                    "minItems",
                    "maxItems",
                    "items",
                    "default",
                ],
            )?;
            let items = schema
                .get("items")
                .and_then(serde_json::Value::as_object)
                .ok_or(())?;
            only_keys(items, &["type", "enum", "anyOf"])?;
            if items.get("type").and_then(serde_json::Value::as_str) != Some("string")
                || (!items.contains_key("enum") && !items.contains_key("anyOf"))
            {
                return Err(());
            }
            validate_choice_schema(items)?;
            validate_default(schema)
        }
        _ => Err(()),
    }
}

fn validate_form_content(
    schema: &serde_json::Value,
    content: &serde_json::Value,
) -> Result<(), ()> {
    let schema = schema.as_object().ok_or(())?;
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or(())?;
    let content = content.as_object().ok_or(())?;
    if content.keys().any(|name| !properties.contains_key(name)) {
        return Err(());
    }
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        for name in required {
            if !name.as_str().is_some_and(|name| content.contains_key(name)) {
                return Err(());
            }
        }
    }
    for (name, value) in content {
        validate_primitive_value(properties.get(name).ok_or(())?, value)?;
    }
    Ok(())
}

fn validate_primitive_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<(), ()> {
    let schema = schema.as_object().ok_or(())?;
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => {
            let value = value.as_str().ok_or(())?;
            if schema
                .get("minLength")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|min| value.chars().count() < min as usize)
                || schema
                    .get("maxLength")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|max| value.chars().count() > max as usize)
            {
                return Err(());
            }
            if let Some(pattern) = schema.get("pattern").and_then(serde_json::Value::as_str) {
                let pattern = regex::Regex::new(pattern).map_err(|_| ())?;
                if !pattern.is_match(value) {
                    return Err(());
                }
            }
            if let Some(format) = schema.get("format").and_then(serde_json::Value::as_str) {
                let valid = match format {
                    "email" => {
                        let mut parts = value.split('@');
                        parts.next().is_some_and(|part| !part.is_empty())
                            && parts.next().is_some_and(|part| {
                                !part.is_empty() && part.contains('.') && !part.contains(' ')
                            })
                            && parts.next().is_none()
                    }
                    "uri" => url::Url::parse(value).is_ok(),
                    "date" => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok(),
                    "date-time" => chrono::DateTime::parse_from_rfc3339(value).is_ok(),
                    _ => false,
                };
                if !valid {
                    return Err(());
                }
            }
            validate_enum_member(schema, &serde_json::Value::String(value.to_owned()))
        }
        Some("number") if value.as_f64().is_some_and(f64::is_finite) => {
            validate_numeric_bounds(schema, value.as_f64().ok_or(())?)
        }
        Some("integer") if value.as_i64().is_some() || value.as_u64().is_some() => {
            validate_numeric_bounds(schema, value.as_f64().ok_or(())?)
        }
        Some("boolean") if value.is_boolean() => Ok(()),
        Some("array") => {
            let values = value.as_array().ok_or(())?;
            if schema
                .get("minItems")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|min| values.len() < min as usize)
                || schema
                    .get("maxItems")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|max| values.len() > max as usize)
            {
                return Err(());
            }
            let items = schema.get("items").ok_or(())?;
            for item in values {
                if !item.is_string() {
                    return Err(());
                }
                validate_enum_member(items.as_object().ok_or(())?, item)?;
            }
            Ok(())
        }
        _ => Err(()),
    }
}

fn validate_numeric_bounds(
    schema: &serde_json::Map<String, serde_json::Value>,
    value: f64,
) -> Result<(), ()> {
    if schema
        .get("minimum")
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|min| value < min)
        || schema
            .get("maximum")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|max| value > max)
    {
        return Err(());
    }
    Ok(())
}

fn validate_enum_member(
    schema: &serde_json::Map<String, serde_json::Value>,
    value: &serde_json::Value,
) -> Result<(), ()> {
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array)
        && !values.contains(value)
    {
        return Err(());
    }
    if let Some(values) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(serde_json::Value::as_array)
    {
        let matches = values
            .iter()
            .filter(|entry| entry.get("const") == Some(value))
            .count();
        if matches != 1 {
            return Err(());
        }
    }
    Ok(())
}

fn validate_choice_schema(schema: &serde_json::Map<String, serde_json::Value>) -> Result<(), ()> {
    if schema.contains_key("enum") && (schema.contains_key("oneOf") || schema.contains_key("anyOf"))
    {
        return Err(());
    }
    if let Some(values) = schema.get("enum") {
        let values = values.as_array().ok_or(())?;
        let mut seen = std::collections::BTreeSet::new();
        for value in values {
            let value = value.as_str().ok_or(())?;
            if !seen.insert(value) {
                return Err(());
            }
        }
    }
    if let Some(values) = schema.get("oneOf").or_else(|| schema.get("anyOf")) {
        let values = values.as_array().ok_or(())?;
        let mut seen = std::collections::BTreeSet::new();
        for value in values {
            let value = value.as_object().ok_or(())?;
            only_keys(value, &["const", "title"])?;
            let constant = value
                .get("const")
                .and_then(serde_json::Value::as_str)
                .ok_or(())?;
            if !seen.insert(constant) {
                return Err(());
            }
            if value.get("title").is_some_and(|title| !title.is_string()) {
                return Err(());
            }
        }
    }
    Ok(())
}

fn validate_default(schema: &serde_json::Map<String, serde_json::Value>) -> Result<(), ()> {
    match schema.get("default") {
        Some(value) => validate_primitive_value(&serde_json::Value::Object(schema.clone()), value),
        None => Ok(()),
    }
}

fn only_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
) -> Result<(), ()> {
    if object.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(())
    }
}

fn bounded_visible_ascii(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn bounded_opaque_text(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len()) && !value.chars().any(char::is_control)
}

fn bounded_human_text(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
}

fn json_len(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

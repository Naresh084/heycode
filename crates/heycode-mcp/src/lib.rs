//! heycode-mcp — provider-neutral MCP registry, the stdio bridge and the
//! Streamable HTTP transport.
//!
//! [`McpRegistry`] is the stable service plane. Exact server definitions stay
//! private to trusted transport providers; deterministic schema-versioned
//! snapshots redact literal argument/environment values. Transport providers
//! receive token-guarded publishers and atomically replace complete successful
//! generations. Context disposal removes definitions/connections and makes
//! retained publishers stale.
//!
//! The `mcp` plugin is the current stdio connection provider beneath that
//! registry. It runs one JSON-RPC child per configured server through the
//! shared subprocess/sandbox service, performs initialize + tools/list/call,
//! publishes a successful registry generation, and exposes qualified
//! `mcp__<server>__<tool>` tools. Each connection owns a dedicated driver
//! thread/runtime so composition never nests `block_on` and context shutdown
//! reaps the complete process tree.
//!
//! Transports differ in framing, not in the JSON-RPC contract, so both publish
//! one [`McpRequestChannel`]. [`McpToolGenerationOwner`] consumes that channel:
//! it walks the complete paginated `tools/list`, registers the whole candidate
//! atomically and publishes exactly one registry generation, so a list change,
//! a racing refresh or a mid-walk failure can never leave a partial one.
//!
//! MCP14 consumes only S13's value-minimized competitor preview. Clean rows can
//! produce exact disabled-reconnect definitions; credential-bearing exclusions
//! become unresolved [`McpSecretReference`] requests, and no enabled row with
//! missing trust, executable authority or excluded metadata can produce a
//! definition.

mod bound_connection;
mod channel;
mod client_events;
mod competitor_import;
mod credential_binding;
mod generation;
mod http;
mod lifecycle_hooks;
pub mod management;
mod model_tools;
mod notifications;
pub mod oauth;
mod oauth_registration;
pub mod prompts;
mod registry;
pub mod resources;
pub mod results;
mod runtime_control;
mod supervisor;
pub mod testing;
mod tool_security;

pub use bound_connection::{
    McpBoundEnvironmentSource, McpBoundServer, McpBoundServerError, mcp_bound_servers_plugin,
    mcp_bound_servers_product_plugin,
};
pub use channel::McpSiblingContributions;
pub use channel::{McpChannelError, McpRequestChannel, McpServerHandshake};
pub use client_events::{
    McpClientEvent, McpClientEventError, McpClientEventRouter, McpClientEventSink, McpClientReply,
    McpClientRoute, McpElicitationCapabilities, McpElicitationFailure, McpElicitationHandler,
    McpElicitationId, McpElicitationMode, McpElicitationRequest, McpElicitationResponse,
    McpElicitationSchema, McpLogEvent, McpLogLevel, McpPendingElicitation, McpProgressEvent,
    McpProgressRegistration, McpProgressToken,
};
pub use competitor_import::{
    McpCompetitorImportError, McpCompetitorImportPreview, McpCompetitorImportRow,
    McpImportAuthRole, McpImportAuthorityRequirement, McpUnresolvedAuthReference,
    preview_competitor_mcp_import,
};
pub use credential_binding::{
    McpCredentialBinding, McpCredentialBindings, McpCredentialEncoding, McpCredentialError,
};
pub use generation::{McpListChangeWatch, McpToolDef, McpToolGenerationOwner, McpToolListLimits};
pub use http::{McpHttpError, McpProtocolVersion, McpSessionId, McpStreamableHttpClient};
pub use lifecycle_hooks::{
    McpLifecycleHookDecision, McpLifecycleHookPhase, McpLifecycleHookPort, McpLifecycleHookReport,
    McpLifecycleHookRequest,
};
pub use management::SERVICE_MCP_MANAGEMENT;
pub use notifications::{
    McpNotificationKind, McpNotificationRouter, TOOLS_LIST_CHANGED_NOTIFICATION,
};
pub use registry::{
    MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalMode, McpArgument, McpAuthenticationState,
    McpCapabilitySet, McpConnectionGeneration, McpConnectionProviderId, McpConnectionPublisher,
    McpConnectionState, McpContributionCounts, McpDefinitionScope, McpEnvironmentValue,
    McpExposurePolicy, McpFailureCode, McpGenerationCandidate, McpGenerationRetention,
    McpNamedValueSnapshot, McpReconnectPolicy, McpRegistry, McpRegistryError, McpSecretReference,
    McpServerDefinition, McpServerDefinitionSnapshot, McpServerId, McpServerSnapshot, McpSnapshot,
    McpStdioTransport, McpStreamableHttpTransport, McpTimeouts, McpToolPolicy,
    McpTransportDefinition, McpTransportKind, McpTransportSnapshot, McpValueSourceKind,
    McpValueSourceSnapshot, mcp_registry_plugin,
};
pub use runtime_control::{McpRuntimeControl, McpRuntimeControlError, SERVICE_MCP_RUNTIME_CONTROL};
pub use supervisor::{
    McpConnectionAttempt, McpReconnect, McpReconnectSupervisor, McpRecoveryAdmission,
    McpRecoveryOutcome,
};
pub use tool_security::{
    McpToolAdmission, McpToolAnnotations, McpToolApprovalDecision, McpToolApprovalHandler,
    McpToolApprovalRequest, resolve_mcp_tool_admission,
};

/// Lifecycle-managed MCP server definition and connection-generation registry.
pub const SERVICE_MCP: heycode_core::ServiceKey = heycode_core::ServiceKey::new("mcp");

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt as _};
use heycode_core::{CoreError, CoreResult, Plugin};
use heycode_exec::{ManagedProcess, ProcessInput, ProcessLines, SubprocessService};
use heycode_tools::ToolRegistry;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// One configured MCP server process.
#[derive(Clone)]
pub struct McpServerConfig {
    /// Executable to spawn (resolved via PATH).
    pub command: String,
    /// Argument vector after the command.
    pub args: Vec<String>,
    /// Extra environment variables layered onto the scrubbed parent env.
    pub env: HashMap<String, String>,
    /// Whether a failure to connect this server fails the whole composition.
    ///
    /// Off by default: a server is user data, and a mistyped command or a
    /// binary not installed yet must show as a failed row in `/mcp`, never stop
    /// heycode from starting. `[mcp.servers.<name>] required = true` opts a
    /// server in when the session is meaningless without it.
    pub required: bool,
}

/// One configured MCP server and the transport it selected.
///
/// The composition root resolves the choice, so the plugin receives a fully
/// resolved specification rather than deciding a default itself.
///
/// Deliberately no `Debug`: a stdio server's `env` may carry credentials, which
/// is why [`McpServerConfig`] has none either.
#[derive(Clone)]
pub enum McpServerSpec {
    /// Spawn a local process and speak JSON-RPC over its stdio.
    Stdio(McpServerConfig),
    /// Speak Streamable HTTP to a validated remote endpoint.
    StreamableHttp {
        /// Absolute endpoint URL, validated at definition time.
        url: String,
        /// Whether a failure to connect fails the composition; see
        /// [`McpServerConfig::required`].
        required: bool,
    },
    /// Fully resolved definition supplied by another trusted plugin host.
    ///
    /// This is the PL04 seam: bundled metadata and policy are validated before
    /// the stdio provider sees them, while transport ownership remains here.
    Definition(McpServerDefinition),
}

/// Connection failures and protocol errors.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// The child could not be spawned or its streams died.
    #[error("transport: {0}")]
    Transport(String),
    /// The server answered with a JSON-RPC error object.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// Server-provided message.
        message: String,
    },
    /// No response within the request budget.
    #[error("timed out waiting for `{method}` response")]
    Timeout {
        /// Method that timed out (diagnostics).
        method: String,
    },
    /// The server violated the initialize/tool synchronization contract.
    #[error("protocol: {0}")]
    Protocol(&'static str),
    /// Caller cancellation retired the exact pending request.
    #[error("MCP request was cancelled")]
    Cancelled,
}

const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Messages the per-connection driver understands.
enum DriverMsg {
    Request {
        id: u64,
        method: String,
        params: serde_json::Value,
        /// Resolves with the RAW JSON-RPC frame (error key included).
        reply: tokio::sync::oneshot::Sender<serde_json::Value>,
    },
    Notify {
        method: String,
    },
    CancelRequest {
        id: u64,
    },
}

type PendingMap = Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>>;

/// A live connection to one MCP server process. Cloneable; killing any clone
/// kills the child.
pub struct McpConnection {
    name: String,
    tx: mpsc::Sender<DriverMsg>,
    process: Mutex<Option<ManagedProcess>>,
    driver_stop: tokio_util::sync::CancellationToken,
    driver_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    // Dropped from a plain thread; Tokio forbids runtime drop in async context.
    driver: Mutex<Option<Arc<tokio::runtime::Runtime>>>,
    request_timeout: std::time::Duration,
}

struct ChildStreams {
    stdin: ProcessInput,
    stdout: ProcessLines,
}

struct McpInitializedConnection {
    connection: McpConnection,
    handshake: McpServerHandshake,
}

struct SpawnDriverRequest<'a> {
    name: &'a str,
    transport: &'a McpStdioTransport,
    driver: Arc<tokio::runtime::Runtime>,
    subprocess: SubprocessService,
    operation_credentials: Option<(
        heycode_credentials::CredentialsService,
        McpCredentialBindings,
    )>,
    router: McpNotificationRouter,
    startup_timeout: std::time::Duration,
    request_timeout: std::time::Duration,
    /// Fired once if this connection's driver loop ends without being asked to.
    liveness: McpLiveness,
}

/// What a connection fires when its transport ends without being asked to.
type McpLivenessSignal = Arc<dyn Fn() + Send + Sync>;

/// The one place a connection's death becomes an event.
///
/// A stdio child can die at any moment, and the driver loop is the only code
/// that observes it. Before this slot existed the loop simply ended: the
/// registry went on publishing `Ready` for a transport that no longer had a
/// process, and no reconnect could ever begin because nothing called
/// `recover()`. The slot is armed *after* the generation owner and supervisor
/// exist, which is why it is a slot rather than a constructor argument.
#[derive(Clone, Default)]
pub(crate) struct McpLiveness(Arc<Mutex<Option<McpLivenessSignal>>>);

impl McpLiveness {
    /// Install the signal this connection fires when its transport ends.
    fn arm(&self, signal: McpLivenessSignal) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(signal);
        }
    }

    /// Fire the armed signal, if the connection got far enough to arm one.
    fn fire(&self) {
        let signal = self.0.lock().ok().and_then(|slot| slot.clone());
        if let Some(signal) = signal {
            signal();
        }
    }
}

impl McpConnection {
    /// Spawn the server and run the initialize handshake.
    ///
    /// # Errors
    /// Spawn failures, stream failures, protocol errors, timeouts.
    pub async fn spawn(name: &str, cfg: &McpServerConfig) -> Result<Self, McpError> {
        Self::spawn_with_router(name, cfg, McpNotificationRouter::new()).await
    }

    /// Spawn with an exact MCP11 product-session event and elicitation route.
    ///
    /// # Errors
    /// Spawn failures, stream failures, protocol errors and timeouts.
    pub async fn spawn_with_client_events(
        name: &str,
        cfg: &McpServerConfig,
        client_events: McpClientEventRouter,
    ) -> Result<Self, McpError> {
        Self::spawn_with_router(
            name,
            cfg,
            McpNotificationRouter::with_client_events(
                resources::McpResourceListLimits::default(),
                client_events,
            ),
        )
        .await
    }

    async fn spawn_with_router(
        name: &str,
        cfg: &McpServerConfig,
        router: McpNotificationRouter,
    ) -> Result<Self, McpError> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map_err(|e| McpError::Transport(format!("driver runtime: {e}")))?,
        );
        let cwd = std::env::current_dir()
            .map_err(|_| McpError::Transport("working directory unavailable".to_owned()))?;
        let transport = stdio_transport(cfg, &cwd)
            .map_err(|_| McpError::Protocol("invalid MCP stdio definition"))?;
        let spawned = Self::spawn_driverless(SpawnDriverRequest {
            name,
            transport: &transport,
            driver: runtime.clone(),
            subprocess: SubprocessService::local(),
            operation_credentials: None,
            router,
            startup_timeout: std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
            request_timeout: std::time::Duration::from_secs(600),
            liveness: McpLiveness::default(),
        })
        .await;
        match spawned {
            Ok(initialized) => Ok(initialized.connection),
            Err(error) => {
                // A failed spawn leaves this the last reference to the driver
                // runtime, and dropping a runtime inside an async context
                // panics. Release it on a plain thread — the `DriverRuntime`
                // idiom — so a missing binary is an error, not a crash.
                let _released = std::thread::spawn(move || drop(runtime)).join();
                Err(error)
            }
        }
    }

    /// Spawn one exact stdio definition with operation-time credential
    /// bindings and an optional MCP11 route.
    ///
    /// # Errors
    /// Missing bindings/credentials, spawn failures, protocol errors and
    /// timeouts.
    pub async fn spawn_definition_with_credentials(
        name: &str,
        transport: &McpStdioTransport,
        credentials: heycode_credentials::CredentialsService,
        bindings: McpCredentialBindings,
        client_events: Option<McpClientEventRouter>,
    ) -> Result<Self, McpError> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map_err(|error| McpError::Transport(format!("driver runtime: {error}")))?,
        );
        let router = client_events.map_or_else(McpNotificationRouter::new, |events| {
            McpNotificationRouter::with_client_events(
                resources::McpResourceListLimits::default(),
                events,
            )
        });
        Ok(Self::spawn_driverless(SpawnDriverRequest {
            name,
            transport,
            driver: runtime,
            subprocess: SubprocessService::local(),
            operation_credentials: Some((credentials, bindings)),
            router,
            startup_timeout: std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
            request_timeout: std::time::Duration::from_secs(600),
            liveness: McpLiveness::default(),
        })
        .await?
        .connection)
    }

    /// Send one request and await its response `result`.
    #[cfg(test)]
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        self.request_with_timeout(
            method,
            params,
            self.request_timeout,
            &CancellationToken::new(),
        )
        .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpError> {
        if cancellation.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let id = next_request_id();
        let (reply_tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
        self.tx
            .clone()
            .send(DriverMsg::Request {
                id,
                method: method.to_owned(),
                params,
                reply: reply_tx,
            })
            .await
            .map_err(|_| McpError::Transport("driver stopped".into()))?;
        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let _sent = self.tx.clone().send(DriverMsg::CancelRequest { id }).await;
                return Err(McpError::Cancelled);
            }
            response = tokio::time::timeout(timeout, rx) => match response {
                Ok(response) => response
                    .map_err(|_| McpError::Transport("driver dropped reply".into()))?,
                Err(_) => {
                    let _sent = self.tx.clone().send(DriverMsg::CancelRequest { id }).await;
                    return Err(McpError::Timeout { method: method.to_owned() });
                }
            }
        };
        if let Some(err) = response.get("error") {
            return Err(McpError::Rpc {
                code: err
                    .get("code")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(-1),
                message: err
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("(no message)")
                    .to_owned(),
            });
        }
        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Fire-and-forget notification.
    async fn notify(&self, method: &str) -> Result<(), McpError> {
        self.tx
            .clone()
            .send(DriverMsg::Notify {
                method: method.to_owned(),
            })
            .await
            .map_err(|_| McpError::Transport("driver stopped".into()))
    }

    /// Terminate the child (idempotent, sync-safe).
    pub fn kill(&self) {
        self.driver_stop.cancel();
        let process = self.process.lock().ok().and_then(|mut slot| slot.take());
        if let Some(process) = process {
            drop(process);
        }
        if let Ok(mut slot) = self.driver_thread.lock()
            && let Some(thread) = slot.take()
        {
            let _joined = thread.join();
        }
        if let Ok(mut slot) = self.driver.lock()
            && let Some(runtime) = slot.take()
        {
            let _dropped = std::thread::spawn(move || drop(runtime)).join();
        }
    }

    /// Server name this connection serves.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        self.kill(); // never leak children, even outside a runtime
    }
}

/// Everything the driver loop owns; dies with the loop when the runtime drops.
async fn driver_loop(
    name: String,
    mut streams: ChildStreams,
    mut rx: mpsc::Receiver<DriverMsg>,
    pending: PendingMap,
    router: McpNotificationRouter,
    stop: tokio_util::sync::CancellationToken,
    liveness: McpLiveness,
) {
    let mut lines = streams.stdout;
    type ClientRequestFuture =
        std::pin::Pin<Box<dyn Future<Output = Option<McpClientReply>> + Send>>;
    let mut client_requests = FuturesUnordered::<ClientRequestFuture>::new();

    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            reply = client_requests.next(), if !client_requests.is_empty() => {
                if let Some(Some(reply)) = reply {
                    let _ = write_frame(&mut streams.stdin, reply.as_json()).await;
                }
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue; // tolerate non-JSON noise between frames
                    };
                    if value.get("id").is_some() && value.get("method").is_some() {
                        let method = value
                            .get("method")
                            .and_then(serde_json::Value::as_str);
                        if method == Some("ping") {
                            let _ = write_frame(
                                &mut streams.stdin,
                                McpClientReply::empty_result(&value).as_json(),
                            )
                            .await;
                            continue;
                        }
                        let Some(client_events) = router.client_events() else {
                            let _ = write_frame(
                                &mut streams.stdin,
                                McpClientReply::method_not_found(&value).as_json(),
                            )
                            .await;
                            continue;
                        };
                        match client_events.admit_request(&value) {
                            Ok(Some(pending_request)) => {
                                client_requests.push(Box::pin(pending_request.resolve()));
                            }
                            Ok(None) => {
                                let _ = write_frame(
                                    &mut streams.stdin,
                                    McpClientReply::method_not_found(&value).as_json(),
                                )
                                .await;
                            }
                            Err(reply) => {
                                let _ = write_frame(&mut streams.stdin, reply.as_json()).await;
                            }
                        }
                        continue;
                    }
                    let Some(id) = value.get("id").and_then(serde_json::Value::as_u64) else {
                        // Notifications carry no id. The router owns which
                        // family each one advances; an unmodelled method is
                        // left alone rather than failing the connection.
                        let _routed = router.observe(&value);
                        continue;
                    };
                    let sender =
                        pending.lock().ok().and_then(|mut p| p.remove(&id));
                    if let Some(sender) = sender {
                        let _ = sender.send(value);
                    }
                }
                _ => break, // child closed stdout or read failed
            },
            msg = rx.recv() => match msg {
                Some(DriverMsg::Request { id, method, params, reply }) => {
                    let frame = serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "method": method, "params": params,
                    });
                    let inserted = pending
                        .lock()
                        .map(|mut p| p.insert(id, reply).is_none())
                        .unwrap_or(false);
                    if !inserted {
                        break; // lock poisoned or duplicate id: stop driving
                    }
                    if write_frame(&mut streams.stdin, &frame).await.is_err() {
                        fail_pending(&pending, id, &name);
                        continue;
                    }
                }
                Some(DriverMsg::Notify { method }) => {
                    let frame = serde_json::json!({"jsonrpc":"2.0","method":method});
                    let _ = write_frame(&mut streams.stdin, &frame).await;
                }
                Some(DriverMsg::CancelRequest { id }) => {
                    let removed = pending.lock().ok().and_then(|mut pending| pending.remove(&id));
                    if removed.is_some() {
                        let frame = serde_json::json!({
                            "jsonrpc":"2.0",
                            "method":"notifications/cancelled",
                            "params":{"requestId":id}
                        });
                        let _ = write_frame(&mut streams.stdin, &frame).await;
                    }
                }
                None => break, // all senders gone: connection retired
            },
        }
    }
    // Retire the rows this dead child owned, but never the route: the bounded
    // reconnect supervisor hands the replacement child this same router, and
    // the product UI holds it for the whole session. Terminal disposal belongs
    // to `EstablishedGeneration::dispose`.
    router.retire_pending();
    while client_requests.next().await.is_some() {}
    // A cancelled token means the host retired this connection on purpose, and
    // an intentional shutdown is not a failure to report. Anything else means
    // the transport is gone while the registry still claims it is Ready.
    if !stop.is_cancelled() {
        liveness.fire();
    }
}

static REQUEST_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> u64 {
    REQUEST_SEQ.fetch_add(1, Ordering::SeqCst)
}

fn fail_pending(pending: &PendingMap, id: u64, name: &str) {
    let sender = pending.lock().ok().and_then(|mut p| p.remove(&id));
    if let Some(sender) = sender {
        let _ = sender.send(serde_json::json!({
            "error": {"code": -32000, "message": format!("write to mcp server `{name}` failed")}
        }));
    }
}

async fn write_frame(stdin: &mut ProcessInput, frame: &serde_json::Value) -> Result<(), McpError> {
    stdin
        .write_line(&frame.to_string())
        .await
        .map_err(|e| McpError::Transport(format!("write: {e}")))
}

/// Plugin named `"mcp"`: connect every configured server at load time and
/// publish their tools. Children die with the context (effect disposers +
/// `Drop`).
///
/// Composition may run inside OR outside a tokio runtime: each connection
/// carries its own driver runtime, so spawning here never nests runtimes.
pub fn mcp_plugin(
    servers: HashMap<String, McpServerSpec>,
    cwd: std::path::PathBuf,
) -> Box<dyn Plugin> {
    build_mcp_plugin(
        servers,
        cwd,
        None,
        HashMap::new(),
        None,
        McpHostMode::Bridge,
    )
}

/// Build a configured product MCP plugin with exact MCP11/MCP13/O09 adapters.
///
/// # Errors
/// Configured servers and client-event routes are not an exact one-to-one set.
pub fn mcp_product_plugin(
    servers: HashMap<String, McpServerSpec>,
    cwd: std::path::PathBuf,
    approval: Arc<dyn McpToolApprovalHandler>,
    client_events: HashMap<String, McpClientEventRouter>,
    lifecycle_hooks: Arc<dyn McpLifecycleHookPort>,
) -> Result<Box<dyn Plugin>, McpProductPluginError> {
    if servers.len() != client_events.len()
        || servers.iter().any(|(name, _)| {
            client_events
                .get(name)
                .is_none_or(|router| router.server().as_str() != name)
        })
        || client_events.keys().any(|name| !servers.contains_key(name))
    {
        return Err(McpProductPluginError::RouteSetMismatch);
    }
    Ok(build_mcp_plugin(
        servers,
        cwd,
        Some(approval),
        client_events,
        Some(lifecycle_hooks),
        McpHostMode::Product,
    ))
}

/// Configured MCP product attachment failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpProductPluginError {
    /// The configured server and router sets differ.
    #[error("MCP configured servers and client-event routes do not match")]
    RouteSetMismatch,
}

/// What the host composing `mcp` is able to supply behind it.
///
/// [`McpHostMode::Bridge`] is the PL04 seam: a trusted plugin host hands over
/// exact definitions and receives stdio connections, and that is all the
/// services it has. [`McpHostMode::Product`] is the composition root, which
/// also owns the HTTP service and the settings-backed management store — so it
/// is the only mode that can connect a Streamable HTTP endpoint or honour a
/// server the user added with `heycode mcp add`. Making this a mode rather than a
/// runtime `if let Some(service)` is deliberate: a missing service in product
/// mode is a composition error, not a silently reduced feature set.
#[derive(Clone, Copy, PartialEq, Eq)]
enum McpHostMode {
    /// Definitions in, stdio connections out.
    Bridge,
    /// The composition root, with management and HTTP behind it.
    Product,
}

const BRIDGE_INJECTS: &[heycode_core::ServiceKey] = &[
    heycode_tools::SERVICE_TOOLS,
    heycode_exec::SERVICE_SUBPROCESS,
    crate::SERVICE_MCP,
];

const PRODUCT_INJECTS: &[heycode_core::ServiceKey] = &[
    heycode_tools::SERVICE_TOOLS,
    heycode_exec::SERVICE_SUBPROCESS,
    crate::SERVICE_MCP,
    heycode_http::SERVICE_HTTP,
    management::SERVICE_MCP_MANAGEMENT,
];

const BRIDGE_CONTRIBUTION_KINDS: &[heycode_core::PluginContributionKind] = &[
    heycode_core::PluginContributionKind::Tool,
    heycode_core::PluginContributionKind::ExternalProcess,
];

const PRODUCT_CONTRIBUTION_KINDS: &[heycode_core::PluginContributionKind] = &[
    heycode_core::PluginContributionKind::Tool,
    heycode_core::PluginContributionKind::ExternalProcess,
    heycode_core::PluginContributionKind::Service,
];

const NO_PROVIDED_SERVICES: &[heycode_core::ServiceKey] = &[];
const PRODUCT_PROVIDES: &[heycode_core::ServiceKey] = &[SERVICE_MCP_RUNTIME_CONTROL];

/// One server the session will connect, and where its definition came from.
struct ResolvedServer {
    spec: McpServerSpec,
    /// `true` for a `[mcp.servers]` entry the host passed in, `false` for a
    /// definition adopted from the management store during apply.
    declared: bool,
}

fn build_mcp_plugin(
    servers: HashMap<String, McpServerSpec>,
    cwd: std::path::PathBuf,
    approval: Option<Arc<dyn McpToolApprovalHandler>>,
    client_events: HashMap<String, McpClientEventRouter>,
    lifecycle_hooks: Option<Arc<dyn McpLifecycleHookPort>>,
    mode: McpHostMode,
) -> Box<dyn Plugin> {
    struct McpPlugin(
        HashMap<String, McpServerSpec>,
        std::path::PathBuf,
        Option<Arc<dyn McpToolApprovalHandler>>,
        HashMap<String, McpClientEventRouter>,
        Option<Arc<dyn McpLifecycleHookPort>>,
        McpHostMode,
    );
    impl Plugin for McpPlugin {
        fn name(&self) -> &'static str {
            "mcp"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "mcp",
                env!("CARGO_PKG_VERSION"),
                match self.5 {
                    McpHostMode::Bridge => BRIDGE_CONTRIBUTION_KINDS,
                    McpHostMode::Product => PRODUCT_CONTRIBUTION_KINDS,
                },
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            let mut names: Vec<_> = self.0.keys().cloned().collect();
            names.sort();
            let mut rows: Vec<_> = names
                .into_iter()
                .map(|name| {
                    // A stdio server really is a managed child process; an HTTP
                    // endpoint is not one, and saying so in `doctor` and
                    // `/plugins` was simply untrue.
                    let kind = match &self.0[&name] {
                        McpServerSpec::StreamableHttp { .. } => {
                            heycode_core::ContributionKind::McpServer
                        }
                        McpServerSpec::Definition(definition) => match definition.transport() {
                            McpTransportDefinition::StreamableHttp(_) => {
                                heycode_core::ContributionKind::McpServer
                            }
                            McpTransportDefinition::Stdio(_) => {
                                heycode_core::ContributionKind::ExternalProcess
                            }
                        },
                        McpServerSpec::Stdio(_) => heycode_core::ContributionKind::ExternalProcess,
                    };
                    heycode_core::PluginContributionSpec::new(kind, name)
                })
                .collect();
            rows.extend(model_tools::MODEL_RESOURCE_TOOL_NAMES.map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    name,
                )
            }));
            rows
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            match self.5 {
                McpHostMode::Bridge => BRIDGE_INJECTS,
                McpHostMode::Product => PRODUCT_INJECTS,
            }
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            match self.5 {
                McpHostMode::Bridge => NO_PROVIDED_SERVICES,
                McpHostMode::Product => PRODUCT_PROVIDES,
            }
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> CoreResult<()> {
            for (name, route) in &self.3 {
                if !self.0.contains_key(name) || route.server().as_str() != name {
                    return Err(CoreError::other(
                        "MCP client-event route does not match a configured server",
                    ));
                }
            }
            let registry = ctx
                .get::<McpRegistry>(crate::SERVICE_MCP)
                .ok_or_else(|| CoreError::other("mcp registry service missing"))?;
            let tools: Arc<ToolRegistry> = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tools service missing"))?;
            let model_tool_owner = registry
                .model_resource_tools(&tools)
                .map_err(|error| CoreError::other(error.to_string()))?;
            let model_resources = model_tool_owner.hub();
            ctx.effect(move || drop(model_tool_owner));

            // One server set, resolved before anything connects. Configured
            // entries come first and win a name collision, because their
            // transport is exact where a stored row is a bare command or URL.
            let mut resolved: BTreeMap<String, ResolvedServer> = self
                .0
                .iter()
                .map(|(name, spec)| {
                    (
                        name.clone(),
                        ResolvedServer {
                            spec: spec.clone(),
                            declared: true,
                        },
                    )
                })
                .collect();
            let management = match self.5 {
                McpHostMode::Bridge => None,
                McpHostMode::Product => Some(
                    ctx.get::<management::McpManagement>(management::SERVICE_MCP_MANAGEMENT)
                        .ok_or_else(|| CoreError::other("mcp management service missing"))?,
                ),
            };
            if let Some(management) = management.as_ref() {
                // Both directions of the same join. The management surface
                // learns what this session is actually running, and the
                // connection path honours what the user added through it.
                management
                    .adopt_configured_servers(configured_rows(&self.0)?)
                    .map_err(|error| CoreError::other(error.to_string()))?;
                for stored in management
                    .connectable()
                    .map_err(|error| CoreError::other(error.to_string()))?
                {
                    let spec = stored_server_spec(&stored);
                    resolved
                        .entry(stored.name.clone())
                        .or_insert(ResolvedServer {
                            spec,
                            declared: false,
                        });
                }
                let released = Arc::clone(management);
                ctx.effect(move || {
                    let _released = released.adopt_configured_servers(BTreeMap::new());
                });
            }
            let runtime_control = match self.5 {
                McpHostMode::Bridge => None,
                McpHostMode::Product => {
                    let control = McpRuntimeControl::new();
                    ctx.provide(SERVICE_MCP_RUNTIME_CONTROL, self.name(), control.clone())?;
                    let shutdown = control.clone();
                    ctx.effect(move || shutdown.shutdown());
                    Some(control)
                }
            };
            if resolved.is_empty() {
                return Ok(());
            }
            let subprocess = ctx
                .get::<SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                .ok_or_else(|| CoreError::other("subprocess service missing"))?;
            let http = match self.5 {
                McpHostMode::Bridge => None,
                McpHostMode::Product => Some(
                    ctx.get::<heycode_http::HttpService>(heycode_http::SERVICE_HTTP)
                        .ok_or_else(|| CoreError::other("http service missing"))?,
                ),
            };

            // Dedicated runtime for ALL connection drivers, alive until the
            // connections release it — reader/writer tasks keep their home. The
            // guard drops it from a plain thread on every path, including the
            // early return of a connection that could not be established.
            let driver_guard = DriverRuntime::new()?;
            let driver_rt = driver_guard.handle()?;

            // First pass registers definitions and connection rows (both need
            // `ctx`), and prepares one connect future per server; the second
            // runs them together.
            let mut pending: Vec<PendingConnection> = Vec::new();
            for (server_name, entry) in resolved {
                let id = McpServerId::new(server_name.clone()).map_err(registry_core_error)?;
                let (built, required) = match &entry.spec {
                    McpServerSpec::Stdio(cfg) => {
                        (stdio_definition(&server_name, cfg, &self.1), cfg.required)
                    }
                    McpServerSpec::StreamableHttp { required, .. } => {
                        (http_definition(&server_name, &entry.spec), *required)
                    }
                    McpServerSpec::Definition(definition) => {
                        (Ok(definition.clone()), definition.required())
                    }
                };
                let definition = match built {
                    Ok(definition) => definition,
                    Err(error) if required => {
                        return Err(CoreError::other(format!(
                            "mcp server `{server_name}` cannot be defined: {error}"
                        )));
                    }
                    // A row this build cannot even define (a hand-edited store
                    // with a bad URL) is skipped rather than bricking startup;
                    // `heycode mcp add` refuses the same value up front.
                    Err(_) => continue,
                };
                registry
                    .register_definition(ctx, definition)
                    .map_err(registry_core_error)?;
                if !entry.declared {
                    // Adopted rows are discovered during apply, so they cannot
                    // appear in the pre-apply inventory the way configured ones
                    // do. Recording them keeps `doctor --composition` honest
                    // about every server this session actually runs.
                    ctx.contribute(
                        heycode_core::ContributionKind::McpServer,
                        server_name.clone(),
                    )?;
                }
                let exact_definition = registry
                    .definition(&id)
                    .map_err(registry_core_error)?
                    .ok_or_else(|| CoreError::other("mcp definition disappeared"))?;
                if let Some(runtime_control) = runtime_control.as_ref() {
                    runtime_control.declare(&exact_definition);
                }
                let is_http = matches!(
                    exact_definition.transport(),
                    McpTransportDefinition::StreamableHttp(_)
                );
                let required = exact_definition.required();
                let resources_exposed = exact_definition.exposure().resources;
                let target_for_error = match exact_definition.transport() {
                    McpTransportDefinition::Stdio(stdio) => stdio.command().to_owned(),
                    McpTransportDefinition::StreamableHttp(http) => http.url().to_owned(),
                };
                let Some(provider) = connection_provider(is_http, self.5) else {
                    // A bridge host has no HTTP service, so an HTTP endpoint
                    // stays an inspectable definition with no connection. The
                    // product root does connect it.
                    continue;
                };
                let started_at_ms = unix_time_ms();
                let publisher = registry
                    .register_connection(
                        ctx,
                        &id,
                        McpConnectionProviderId::new(provider).map_err(registry_core_error)?,
                        started_at_ms,
                    )
                    .map_err(registry_core_error)?;
                let rt_for_spawn = driver_rt.clone();
                let subprocess_for_spawn = (*subprocess).clone();
                let tools_for_reg = tools.clone();
                let approval = self.2.clone();
                let client_events = self.3.get(&server_name).cloned();
                let lifecycle_hooks = self.4.clone();
                let http_for_spawn = http.as_deref().cloned();
                pending.push(PendingConnection {
                    server_name,
                    required,
                    target_for_error,
                    resources_exposed,
                    connect: Box::pin(async move {
                        if is_http {
                            let http = http_for_spawn.ok_or(McpError::Protocol(
                                "MCP Streamable HTTP requires the http service",
                            ))?;
                            establish_http_generation(HttpGenerationRequest {
                                definition: exact_definition,
                                http,
                                tools: tools_for_reg,
                                publisher,
                                started_at_ms,
                                approval,
                                client_events,
                                lifecycle_hooks,
                                credentials: None,
                            })
                            .await
                        } else {
                            establish_stdio_generation(StdioGenerationRequest {
                                definition: exact_definition,
                                driver: rt_for_spawn,
                                subprocess: subprocess_for_spawn,
                                tools: tools_for_reg,
                                publisher,
                                started_at_ms,
                                approval,
                                client_events,
                                lifecycle_hooks,
                                credentials: None,
                                credential_bindings: McpCredentialBindings::new(),
                            })
                            .await
                        }
                    }),
                });
            }

            // Every handshake at once, on ONE plain thread driving the
            // dedicated runtime — `apply` may itself sit on a runtime worker,
            // where a nested `block_on` is illegal. Waiting for three slow
            // servers in series used to hold the shell blank for their total
            // startup budget.
            let (metadata, connects): (Vec<_>, Vec<_>) = pending
                .into_iter()
                .map(|entry| {
                    (
                        (
                            entry.server_name,
                            entry.required,
                            entry.target_for_error,
                            entry.resources_exposed,
                        ),
                        entry.connect,
                    )
                })
                .unzip();
            let outcomes = drive_all_on_plain_thread(&driver_rt, connects);

            // Disposal is registered for every server that DID start before
            // any required failure is reported, so a rollback still kills the
            // children this apply created.
            let mut required_failure = None;
            for ((server_name, required, target_for_error, resources_exposed), outcome) in
                metadata.into_iter().zip(outcomes)
            {
                let established = match outcome {
                    Ok(established) => established,
                    // The generation owner already published the classified
                    // failure, so the row is visible in `/mcp` as Failed. A
                    // server the host did not declare must not take the whole
                    // session down with it — the same rule the bound-server
                    // provider applies one file over.
                    Err(error) if !required => {
                        let _classified = error;
                        continue;
                    }
                    Err(error) => {
                        required_failure.get_or_insert(CoreError::other(format!(
                            "required mcp server `{server_name}` ({target_for_error}) could not start: {error}"
                        )));
                        continue;
                    }
                };
                let tool_names = established.generation.tool_names();
                let resource_binding = match model_resources.bind(
                    server_name.clone(),
                    established.router.resources(),
                    resources_exposed,
                ) {
                    Ok(binding) => binding,
                    Err(error) => {
                        established.dispose(&driver_rt);
                        required_failure.get_or_insert(CoreError::other(error));
                        continue;
                    }
                };
                if let Some(runtime_control) = runtime_control.as_ref() {
                    runtime_control.attach(&server_name, established.supervisor.as_ref());
                }
                let shutdown_runtime = driver_rt.clone();
                ctx.effect(move || {
                    // Kill the transport first, then release the listings it
                    // served. Dropping a catalog while the driver could still
                    // route a notification into its epoch would be a teardown
                    // race for no benefit.
                    established.dispose(&shutdown_runtime);
                    drop(resource_binding);
                    // This may be the last reference to the driver runtime, and
                    // effects unwind inside the host's own runtime, where a
                    // blocking runtime shutdown panics. Release it on a plain
                    // thread — the `DriverRuntime::drop` idiom.
                    let _released = std::thread::spawn(move || drop(shutdown_runtime)).join();
                });
                for tool_name in tool_names {
                    ctx.contribute(heycode_core::ContributionKind::Tool, tool_name)?;
                }
            }
            if let Some(failure) = required_failure {
                return Err(failure);
            }
            Ok(())
        }
    }
    Box::new(McpPlugin(
        servers,
        cwd,
        approval,
        client_events,
        lifecycle_hooks,
        mode,
    ))
}

/// One server whose row is registered and whose handshake has not run yet.
struct PendingConnection {
    server_name: String,
    required: bool,
    target_for_error: String,
    resources_exposed: bool,
    connect: ConnectFuture<EstablishedGeneration>,
}

/// The live transport behind one published generation.
pub(crate) enum EstablishedTransport {
    /// The slot the reconnect supervisor swaps, so disposal always kills the
    /// connection that is current rather than the one this apply first made.
    Stdio(Arc<Mutex<Option<Arc<McpConnection>>>>),
    /// A Streamable HTTP session, terminated with an explicit `DELETE`.
    Http(Arc<McpStreamableHttpClient>),
}

/// One connected server: its transport and every owner that dies with it.
pub(crate) struct EstablishedGeneration {
    transport: EstablishedTransport,
    pub(crate) generation: Arc<McpToolGenerationOwner>,
    /// Retained so the prompt catalog stays readable for the connection's life
    /// and is dropped with it. MCP09's owner holds no process; dropping it early
    /// would retire a listing the published generation still counts.
    prompts: Arc<prompts::McpPromptGenerationOwner>,
    /// Retained for the same reason, and because it owns the resource epoch the
    /// driver routes notifications into.
    router: McpNotificationRouter,
    /// Present exactly when the definition permits bounded reconnect.
    supervisor: Option<Arc<McpReconnectSupervisor>>,
}

impl EstablishedGeneration {
    /// Retire everything this connection owns, in the only safe order.
    ///
    /// The supervisor stops first: a retry episode that outlived the context
    /// would republish a generation into a registry that has already retired
    /// the server. Then the transport dies, and only then do the listings it
    /// served, because routing a notification into a dropped epoch is a
    /// teardown race for no benefit.
    pub(crate) fn dispose(self, runtime: &Arc<tokio::runtime::Runtime>) {
        if let Some(supervisor) = &self.supervisor {
            supervisor.shutdown();
        }
        match self.transport {
            EstablishedTransport::Stdio(slot) => {
                let connection = slot.lock().ok().and_then(|mut slot| slot.take());
                if let Some(connection) = connection {
                    connection.kill();
                }
            }
            EstablishedTransport::Http(client) => {
                let runtime = Arc::clone(runtime);
                let _joined = std::thread::spawn(move || {
                    let cancellation = CancellationToken::new();
                    let _terminated = runtime.block_on(client.terminate(&cancellation));
                })
                .join();
            }
        }
        // Only now is the route itself finished: no replacement child can
        // follow a disposed connection.
        self.router.shutdown();
        drop(self.supervisor);
        drop(self.generation);
        drop(self.prompts);
        drop(self.router);
    }
}

struct StdioGenerationRequest {
    definition: Arc<McpServerDefinition>,
    driver: Arc<tokio::runtime::Runtime>,
    subprocess: SubprocessService,
    tools: Arc<ToolRegistry>,
    publisher: McpConnectionPublisher,
    started_at_ms: u64,
    approval: Option<Arc<dyn McpToolApprovalHandler>>,
    client_events: Option<McpClientEventRouter>,
    lifecycle_hooks: Option<Arc<dyn McpLifecycleHookPort>>,
    credentials: Option<heycode_credentials::CredentialsService>,
    credential_bindings: McpCredentialBindings,
}

/// Everything one Streamable HTTP connection needs to publish a generation.
pub(crate) struct HttpGenerationRequest {
    pub(crate) definition: Arc<McpServerDefinition>,
    pub(crate) http: heycode_http::HttpService,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) publisher: McpConnectionPublisher,
    pub(crate) started_at_ms: u64,
    pub(crate) approval: Option<Arc<dyn McpToolApprovalHandler>>,
    pub(crate) client_events: Option<McpClientEventRouter>,
    pub(crate) lifecycle_hooks: Option<Arc<dyn McpLifecycleHookPort>>,
    /// Present only for a credential-bearing definition.
    pub(crate) credentials: Option<(
        heycode_credentials::CredentialsService,
        McpCredentialBindings,
    )>,
}

fn http_definition(server: &str, spec: &McpServerSpec) -> CoreResult<McpServerDefinition> {
    let McpServerSpec::StreamableHttp { url, required } = spec else {
        return Err(CoreError::other("expected a streamable HTTP server spec"));
    };
    let transport = McpStreamableHttpTransport::new(url, std::collections::BTreeMap::new())
        .map_err(registry_core_error)?;
    // A Streamable HTTP session has no persistent transport that can die
    // between calls, so there is no liveness event for a supervisor to react
    // to and nothing to reconnect *from*. Claiming otherwise would put a
    // policy on the snapshot that nothing could ever act on.
    let reconnect = McpReconnectPolicy::new(false, 500, 30_000, 10).map_err(registry_core_error)?;
    McpServerDefinition::new(
        server,
        server,
        McpDefinitionScope::User,
        McpTransportDefinition::StreamableHttp(transport),
    )
    .map(|definition| {
        definition
            .with_required(*required)
            .with_reconnect(reconnect)
            .with_exposure(McpExposurePolicy {
                resources: true,
                prompts: false,
                instructions: false,
            })
    })
    .map_err(registry_core_error)
}

/// Build the exact stdio definition for one server this session will connect.
///
/// Every server is user data — a `[mcp.servers]` entry as much as a row typed
/// into `heycode mcp add` — so a typo or a not-yet-installed binary degrades to a
/// visible failed row rather than aborting composition. Only an explicit
/// `required = true` on the configured entry makes its failure the host's.
fn stdio_definition(
    server: &str,
    config: &McpServerConfig,
    cwd: &std::path::Path,
) -> CoreResult<McpServerDefinition> {
    let transport = stdio_transport(config, cwd).map_err(registry_core_error)?;
    // A child that crashes mid-session must not leave the user with a dead
    // server until they restart heycode. The budget bounds one recovery episode;
    // a later healthy-then-crash cycle deliberately re-arms it.
    let reconnect = McpReconnectPolicy::new(true, 500, 30_000, 10).map_err(registry_core_error)?;
    McpServerDefinition::new(
        server,
        server,
        McpDefinitionScope::User,
        McpTransportDefinition::Stdio(transport),
    )
    .map(|definition| {
        definition
            .with_required(config.required)
            .with_reconnect(reconnect)
            .with_exposure(McpExposurePolicy {
                resources: true,
                prompts: false,
                instructions: false,
            })
    })
    .map_err(registry_core_error)
}

fn stdio_transport(
    config: &McpServerConfig,
    cwd: &std::path::Path,
) -> Result<McpStdioTransport, McpRegistryError> {
    let arguments = config
        .args
        .iter()
        .cloned()
        .map(McpArgument::literal)
        .collect::<Result<Vec<_>, _>>()?;
    let environment = config
        .env
        .iter()
        .map(|(name, value)| {
            McpEnvironmentValue::literal(value.clone()).map(|value| (name.clone(), value))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    McpStdioTransport::new(
        config.command.clone(),
        cwd.to_path_buf(),
        arguments,
        environment,
    )
}

pub(crate) async fn establish_stdio_generation(
    request: StdioGenerationRequest,
) -> Result<EstablishedGeneration, McpError> {
    request
        .publisher
        .report_starting(request.started_at_ms)
        .map_err(|_| McpError::Protocol("MCP starting state publication failed"))?;
    // One routing plane per connection: three independent list-change epochs
    // and the resource registry the driver feeds. Nothing here can hand the
    // same epoch to two families.
    let router = request
        .client_events
        .map_or_else(McpNotificationRouter::new, |events| {
            McpNotificationRouter::with_client_events(
                resources::McpResourceListLimits::default(),
                events,
            )
        });
    let transport = match request.definition.transport() {
        McpTransportDefinition::Stdio(transport) => transport,
        McpTransportDefinition::StreamableHttp(_) => {
            return failed_stdio_attempt(
                &request.publisher,
                McpError::Protocol("stdio provider received a non-stdio definition"),
            );
        }
    };
    let operation_credentials = request
        .credentials
        .map(|credentials| (credentials, request.credential_bindings));
    let liveness = McpLiveness::default();
    let initialized = match McpConnection::spawn_driverless(SpawnDriverRequest {
        name: request.definition.id().as_str(),
        transport,
        driver: Arc::clone(&request.driver),
        subprocess: request.subprocess.clone(),
        operation_credentials: operation_credentials.clone(),
        router: router.clone(),
        startup_timeout: std::time::Duration::from_millis(
            request.definition.timeouts().startup_ms(),
        ),
        request_timeout: std::time::Duration::from_millis(
            request.definition.timeouts().request_ms(),
        ),
        liveness: liveness.clone(),
    })
    .await
    {
        Ok(initialized) => initialized,
        Err(error) => return failed_stdio_attempt(&request.publisher, error),
    };
    let connection = Arc::new(initialized.connection);
    let live: Arc<Mutex<Option<Arc<McpConnection>>>> =
        Arc::new(Mutex::new(Some(Arc::clone(&connection))));
    let channel = Arc::clone(&connection) as Arc<dyn McpRequestChannel>;
    let cancellation = CancellationToken::new();

    // Sibling listings first: the tool owner publishes the connection's single
    // generation, and it cannot state a resource or prompt count it has not
    // been given. A listing the server advertised but this walk skipped is
    // refused at `candidate`, never reported as zero.
    let prompt_owner = Arc::new(prompts::McpPromptGenerationOwner::new(
        router.prompts(),
        prompts::McpPromptListLimits::default(),
    ));
    let siblings = match walk_sibling_listings(
        &router,
        &prompt_owner,
        &channel,
        &initialized.handshake,
        &cancellation,
    )
    .await
    {
        Ok(siblings) => siblings,
        Err(error) => {
            connection.kill();
            return Err(stdio_error(error));
        }
    };

    // The generation owner owns publication and failure reporting from here,
    // so this provider must not report the same attempt twice. Both
    // constructors enforce the definition's tool policy; only the approval
    // broker differs.
    let publisher = request.publisher.clone();
    let generation = Arc::new(match request.approval {
        None => McpToolGenerationOwner::new(
            &request.definition,
            request.tools,
            request.publisher,
            router.tools(),
            McpToolListLimits::default(),
        ),
        Some(approval) => McpToolGenerationOwner::new_with_product_policy(
            Arc::clone(&request.definition),
            request.tools,
            request.publisher,
            router.tools(),
            McpToolListLimits::default(),
            approval,
            router.client_events(),
            request.lifecycle_hooks,
        ),
    });
    if let Err(error) = generation
        .refresh(channel, &initialized.handshake, siblings, &cancellation)
        .await
    {
        connection.kill();
        return Err(stdio_error(error));
    }

    // From here the connection is published, so its death is the registry's
    // problem. Arm the liveness slot: report the failure first — a Degraded row
    // is the truth whether or not recovery is permitted — then ask the
    // supervisor for a bounded episode.
    let supervisor = request.definition.reconnect().enabled().then(|| {
        Arc::new(McpReconnectSupervisor::new(
            Arc::clone(&generation),
            publisher.clone(),
            Arc::new(StdioReconnector {
                definition: Arc::clone(&request.definition),
                driver: Arc::clone(&request.driver),
                subprocess: request.subprocess,
                operation_credentials,
                router: router.clone(),
                prompts: Arc::clone(&prompt_owner),
                liveness: liveness.clone(),
                live: Arc::clone(&live),
            }) as Arc<dyn McpReconnect>,
            request.definition.reconnect(),
            &CancellationToken::new(),
        ))
    });
    let weak_supervisor = supervisor.as_ref().map(Arc::downgrade);
    liveness.arm(Arc::new(move || {
        let _published = publisher.report_failure(
            McpFailureCode::Transport,
            unix_time_ms(),
            McpGenerationRetention::KeepLastGood,
        );
        if let Some(supervisor) = weak_supervisor.as_ref().and_then(std::sync::Weak::upgrade) {
            let _admission = supervisor.recover();
        }
    }));

    Ok(EstablishedGeneration {
        transport: EstablishedTransport::Stdio(live),
        generation,
        prompts: prompt_owner,
        router,
        supervisor,
    })
}

/// Re-establishes one stdio child for the bounded reconnect supervisor.
///
/// It owns exactly the inputs the first spawn used, so a recovered connection
/// is the same definition, the same routing plane and the same prompt catalog —
/// only the process is new. The previous child is killed on a blocking task
/// rather than inline, because the driver runtime has a single worker and
/// joining a driver thread from it would stall every other connection.
struct StdioReconnector {
    definition: Arc<McpServerDefinition>,
    driver: Arc<tokio::runtime::Runtime>,
    subprocess: SubprocessService,
    operation_credentials: Option<(
        heycode_credentials::CredentialsService,
        McpCredentialBindings,
    )>,
    router: McpNotificationRouter,
    prompts: Arc<prompts::McpPromptGenerationOwner>,
    liveness: McpLiveness,
    live: Arc<Mutex<Option<Arc<McpConnection>>>>,
}

#[async_trait]
impl McpReconnect for StdioReconnector {
    async fn connect(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<McpConnectionAttempt, McpChannelError> {
        let previous = self
            .live
            .lock()
            .map_err(|_| McpChannelError::protocol("MCP connection slot is unusable"))?
            .take();
        if let Some(previous) = previous {
            let _reaped = tokio::task::spawn_blocking(move || previous.kill()).await;
        }
        let McpTransportDefinition::Stdio(transport) = self.definition.transport() else {
            return Err(McpChannelError::protocol(
                "stdio reconnect requires a stdio definition",
            ));
        };
        let initialized = McpConnection::spawn_driverless(SpawnDriverRequest {
            name: self.definition.id().as_str(),
            transport,
            driver: Arc::clone(&self.driver),
            subprocess: self.subprocess.clone(),
            operation_credentials: self.operation_credentials.clone(),
            router: self.router.clone(),
            startup_timeout: std::time::Duration::from_millis(
                self.definition.timeouts().startup_ms(),
            ),
            request_timeout: std::time::Duration::from_millis(
                self.definition.timeouts().request_ms(),
            ),
            liveness: self.liveness.clone(),
        })
        .await
        .map_err(channel_error)?;
        let connection = Arc::new(initialized.connection);
        let channel = Arc::clone(&connection) as Arc<dyn McpRequestChannel>;
        let siblings = walk_sibling_listings(
            &self.router,
            &self.prompts,
            &channel,
            &initialized.handshake,
            cancellation,
        )
        .await?;
        // Publish the new child into the slot only once it has handshaken and
        // its sibling listings are complete: a half-established connection must
        // never become the one disposal would kill.
        self.live
            .lock()
            .map_err(|_| McpChannelError::protocol("MCP connection slot is unusable"))?
            .replace(connection);
        Ok(McpConnectionAttempt::new(channel, initialized.handshake).with_siblings(siblings))
    }
}

/// Connect one Streamable HTTP endpoint and publish its generation.
///
/// Shares every owner with the stdio path; only the transport differs. There is
/// no reconnect supervisor here because an HTTP session has no persistent
/// transport whose death could arm one.
pub(crate) async fn establish_http_generation(
    request: HttpGenerationRequest,
) -> Result<EstablishedGeneration, McpError> {
    request
        .publisher
        .report_starting(request.started_at_ms)
        .map_err(|_| McpError::Protocol("MCP starting state publication failed"))?;
    let router = request
        .client_events
        .map_or_else(McpNotificationRouter::new, |events| {
            McpNotificationRouter::with_client_events(
                resources::McpResourceListLimits::default(),
                events,
            )
        });
    let McpTransportDefinition::StreamableHttp(transport) = request.definition.transport() else {
        return failed_stdio_attempt(
            &request.publisher,
            McpError::Protocol("HTTP provider received a non-HTTP definition"),
        );
    };
    let client = match request.credentials {
        Some((credentials, bindings)) => McpStreamableHttpClient::new_with_credentials(
            request.http,
            transport,
            router.clone(),
            request.definition.timeouts(),
            credentials,
            bindings,
        ),
        None => McpStreamableHttpClient::new(
            request.http,
            transport,
            router.clone(),
            request.definition.timeouts(),
        ),
    };
    let client = match client {
        Ok(client) => Arc::new(client),
        Err(error) => return failed_stdio_attempt(&request.publisher, http_error(error)),
    };
    let cancellation = CancellationToken::new();
    let handshake = match client.initialize(&cancellation).await {
        Ok(handshake) => handshake,
        Err(error) => return failed_stdio_attempt(&request.publisher, http_error(error)),
    };
    let channel = Arc::clone(&client) as Arc<dyn McpRequestChannel>;
    let prompt_owner = Arc::new(prompts::McpPromptGenerationOwner::new(
        router.prompts(),
        prompts::McpPromptListLimits::default(),
    ));
    let siblings =
        match walk_sibling_listings(&router, &prompt_owner, &channel, &handshake, &cancellation)
            .await
        {
            Ok(siblings) => siblings,
            Err(error) => return failed_stdio_attempt(&request.publisher, stdio_error(error)),
        };
    let generation = Arc::new(match request.approval {
        None => McpToolGenerationOwner::new(
            &request.definition,
            request.tools,
            request.publisher,
            router.tools(),
            McpToolListLimits::default(),
        ),
        Some(approval) => McpToolGenerationOwner::new_with_product_policy(
            Arc::clone(&request.definition),
            request.tools,
            request.publisher,
            router.tools(),
            McpToolListLimits::default(),
            approval,
            router.client_events(),
            request.lifecycle_hooks,
        ),
    });
    generation
        .refresh(channel, &handshake, siblings, &cancellation)
        .await
        .map_err(stdio_error)?;
    Ok(EstablishedGeneration {
        transport: EstablishedTransport::Http(client),
        generation,
        prompts: prompt_owner,
        router,
        supervisor: None,
    })
}

/// Project an MCP03 HTTP failure onto the bridge's stable classes.
fn http_error(error: crate::McpHttpError) -> McpError {
    match error {
        crate::McpHttpError::TimedOut => McpError::Protocol("MCP HTTP request timed out"),
        crate::McpHttpError::Cancelled => McpError::Protocol("MCP HTTP request was cancelled"),
        crate::McpHttpError::Unauthorized => McpError::Protocol("MCP authorization failed"),
        crate::McpHttpError::Protocol { requirement } => McpError::Protocol(requirement),
        crate::McpHttpError::Rpc { .. }
        | crate::McpHttpError::Status { .. }
        | crate::McpHttpError::Transport
        | crate::McpHttpError::InvalidField { .. } => McpError::Protocol("MCP HTTP failed"),
    }
}

/// Which registry connection provider owns this transport, if any.
const fn connection_provider(is_http: bool, mode: McpHostMode) -> Option<&'static str> {
    match (is_http, mode) {
        (false, _) => Some("stdio-local"),
        (true, McpHostMode::Product) => Some("streamable-http"),
        (true, McpHostMode::Bridge) => None,
    }
}

/// Project the session's configured servers into management's row shape.
fn configured_rows(
    servers: &HashMap<String, McpServerSpec>,
) -> CoreResult<BTreeMap<String, management::StoredServer>> {
    let mut rows = BTreeMap::new();
    for (name, spec) in servers {
        let (transport, target) = match spec {
            McpServerSpec::Stdio(config) => (McpTransportKind::Stdio, config.command.clone()),
            McpServerSpec::StreamableHttp { url, .. } => {
                (McpTransportKind::StreamableHttp, url.clone())
            }
            McpServerSpec::Definition(definition) => match definition.transport() {
                McpTransportDefinition::Stdio(stdio) => {
                    (McpTransportKind::Stdio, stdio.command().to_owned())
                }
                McpTransportDefinition::StreamableHttp(http) => {
                    (McpTransportKind::StreamableHttp, http.url().to_owned())
                }
            },
        };
        let mut row = management::StoredServer::new(name.clone(), transport, target)
            .map_err(|error| CoreError::other(error.to_string()))?;
        if let McpServerSpec::Stdio(config) = spec {
            row.args = config.args.clone();
            row.env = config
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
        }
        // A configured server is declared by a file this surface does not own,
        // so it is a project-scoped fact rather than a user-store row.
        row.scope = McpDefinitionScope::Project;
        rows.insert(name.clone(), row);
    }
    Ok(rows)
}

/// Project one stored management row onto a connectable server specification.
///
/// A stored row is never required: it is a target the user typed, so a failure
/// to connect is a failed row in `/mcp`, not a failed session.
#[must_use]
pub fn stored_server_spec(server: &management::StoredServer) -> McpServerSpec {
    match server.transport {
        McpTransportKind::Stdio => McpServerSpec::Stdio(McpServerConfig {
            command: server.target.clone(),
            args: server.args.clone(),
            env: server
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            required: false,
        }),
        McpTransportKind::StreamableHttp => McpServerSpec::StreamableHttp {
            url: server.target.clone(),
            required: false,
        },
    }
}

/// Walk the resource and prompt listings this server advertised.
///
/// Advertised listings are walked; unadvertised ones are not, and stay absent
/// rather than becoming a zero the registry would publish as fact. A failure
/// here fails the whole connection generation deliberately: the registry calls
/// its counts "complete", and a generation missing an advertised listing is not
/// complete. Retention keeps the previous generation live, so a transient
/// failure degrades the connection instead of misreporting it.
pub(crate) async fn walk_sibling_listings(
    router: &McpNotificationRouter,
    prompt_owner: &prompts::McpPromptGenerationOwner,
    channel: &Arc<dyn McpRequestChannel>,
    handshake: &McpServerHandshake,
    cancellation: &CancellationToken,
) -> Result<McpSiblingContributions, McpChannelError> {
    let mut siblings = McpSiblingContributions::none();
    if handshake.resources().resources().is_supported() {
        // Committing a listing is what binds the channel, so this must precede
        // any `resources/read` or `resources/subscribe`.
        let generation = router
            .resources()
            .refresh(Arc::clone(channel), handshake.resources(), cancellation)
            .await?;
        siblings = siblings.with_resources(count_of(generation.resources().len())?);
    }
    if handshake.prompts().prompts().is_supported() {
        let catalog = prompt_owner
            .refresh(channel.as_ref(), handshake.prompts(), cancellation)
            .await
            .map_err(prompt_channel_error)?;
        siblings = siblings.with_prompts(catalog.count());
    }
    Ok(siblings)
}

fn count_of(len: usize) -> Result<u32, McpChannelError> {
    u32::try_from(len)
        .map_err(|_| McpChannelError::protocol("listing count exceeds the supported range"))
}

/// Project an MCP09 failure onto the shared channel classes.
fn prompt_channel_error(error: prompts::McpPromptError) -> McpChannelError {
    match error {
        prompts::McpPromptError::Channel(channel) => channel,
        prompts::McpPromptError::Protocol { requirement } => {
            McpChannelError::Protocol { requirement }
        }
        _ => McpChannelError::protocol("prompt listing failed"),
    }
}

/// Project a transport-neutral channel failure onto the stdio bridge's own
/// stable classes. No server body or argv text may cross this boundary.
const fn stdio_error(error: McpChannelError) -> McpError {
    match error {
        McpChannelError::Transport => McpError::Protocol("MCP stdio transport failed"),
        McpChannelError::TimedOut => McpError::Protocol("MCP tool listing timed out"),
        McpChannelError::Cancelled => McpError::Protocol("MCP tool generation was cancelled"),
        McpChannelError::Rpc { .. } => McpError::Protocol("MCP server returned a JSON-RPC error"),
        McpChannelError::Protocol { requirement } => McpError::Protocol(requirement),
        McpChannelError::Unauthorized => McpError::Protocol("MCP server requires authorization"),
        McpChannelError::Conflict => McpError::Protocol("MCP tool generation conflicted"),
    }
}

fn failed_stdio_attempt<T>(
    publisher: &McpConnectionPublisher,
    error: McpError,
) -> Result<T, McpError> {
    let code = match &error {
        McpError::Transport(_) => McpFailureCode::Transport,
        McpError::Timeout { .. } => McpFailureCode::TimedOut,
        McpError::Rpc { .. } | McpError::Protocol(_) => McpFailureCode::Protocol,
        McpError::Cancelled => McpFailureCode::Internal,
    };
    publisher
        .report_failure(code, unix_time_ms(), McpGenerationRetention::KeepLastGood)
        .map_err(|_| McpError::Protocol("MCP failure state publication failed"))?;
    Err(error)
}

fn registry_core_error(error: McpRegistryError) -> CoreError {
    CoreError::other(error.to_string())
}

pub(crate) fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

/// Drive `fut` on `rt` from a plain OS thread — legal even when the caller
/// currently sits on some other runtime's worker.
/// Owns the shared connection-driver runtime and always drops it off-thread.
///
/// `apply` may itself run inside a caller's async context, and Tokio panics if a
/// runtime is dropped there. The success path is safe by accident — a live
/// connection holds a clone, so the local drop is only a refcount decrement —
/// but the failure path is not: a connection that cannot be established kills
/// itself, releasing the last clone, and unwinding `apply` then dropped the
/// runtime in place. Owning it here makes the drop unconditional and correct.
struct DriverRuntime(Option<Arc<tokio::runtime::Runtime>>);

impl DriverRuntime {
    /// Build the one runtime every connection driver of this plugin shares.
    ///
    /// # Errors
    /// The runtime could not be built.
    fn new() -> CoreResult<Self> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map(|runtime| Self(Some(Arc::new(runtime))))
            .map_err(|e| CoreError::other(format!("mcp driver runtime: {e}")))
    }

    /// A handle for one connection driver.
    fn handle(&self) -> CoreResult<Arc<tokio::runtime::Runtime>> {
        self.0
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| CoreError::other("mcp driver runtime is gone"))
    }
}

impl Drop for DriverRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            // Legal even when this drop runs on a runtime worker: the blocking
            // shutdown happens on the plain thread, not here. Same idiom as
            // `McpConnection::kill`.
            let _dropped = std::thread::spawn(move || drop(runtime)).join();
        }
    }
}

/// One connect attempt, boxed so stdio and HTTP futures share a queue.
type ConnectFuture<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, McpError>> + Send>>;

/// Drive many futures to completion together on `rt` from one plain thread,
/// answering in input order.
///
/// Startup connects every configured MCP server, and each connection is a
/// process spawn plus a network-shaped handshake with a ten-second budget.
/// Serially that is N × the slowest server before the shell appears; here it
/// is about one. The thread is plain because `apply` may already sit on a
/// runtime worker, where a nested `block_on` is illegal.
fn drive_all_on_plain_thread<T>(
    rt: &Arc<tokio::runtime::Runtime>,
    futures: Vec<ConnectFuture<T>>,
) -> Vec<Result<T, CoreError>>
where
    T: Send + 'static,
{
    if futures.is_empty() {
        return Vec::new();
    }
    let rt = rt.clone();
    let count = futures.len();
    std::thread::spawn(move || rt.block_on(futures::future::join_all(futures)))
        .join()
        .map_or_else(
            |_| {
                (0..count)
                    .map(|_| Err(CoreError::other("mcp connect thread panicked")))
                    .collect()
            },
            |outcomes| outcomes.into_iter().map(classify_connect).collect(),
        )
}

fn classify_connect<T>(outcome: Result<T, McpError>) -> Result<T, CoreError> {
    outcome.map_err(|error| {
        CoreError::other(match error {
            McpError::Transport(_) => "mcp transport failed",
            McpError::Timeout { .. } => "mcp request timed out",
            McpError::Rpc { .. } | McpError::Protocol(_) => "mcp protocol failed",
            McpError::Cancelled => "mcp request cancelled",
        })
    })
}

fn drive_on_plain_thread<F, T>(rt: &Arc<tokio::runtime::Runtime>, fut: F) -> Result<T, CoreError>
where
    F: std::future::Future<Output = Result<T, McpError>> + Send + 'static,
    T: Send + 'static,
{
    let rt = rt.clone();
    match std::thread::spawn(move || rt.block_on(fut)).join() {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(CoreError::other(match error {
            McpError::Transport(_) => "mcp transport failed",
            McpError::Timeout { .. } => "mcp request timed out",
            McpError::Rpc { .. } | McpError::Protocol(_) => "mcp protocol failed",
            McpError::Cancelled => "mcp request cancelled",
        })),
        Err(_) => Err(CoreError::other("mcp connect thread panicked")),
    }
}

impl McpConnection {
    /// Spawn variant used by the plugin: the caller owns the driver runtime.
    async fn spawn_driverless(
        request: SpawnDriverRequest<'_>,
    ) -> Result<McpInitializedConnection, McpError> {
        let SpawnDriverRequest {
            name,
            transport,
            driver,
            subprocess,
            operation_credentials,
            router,
            startup_timeout,
            request_timeout,
            liveness,
        } = request;
        let program = subprocess
            .resolve_program(std::ffi::OsStr::new(transport.command()))
            .map_err(|_| McpError::Transport("mcp executable unavailable".to_owned()))?;
        let materialize = |reference: &McpSecretReference| {
            let (credentials, bindings) =
                operation_credentials.as_ref().ok_or(McpError::Protocol(
                    "credential-backed stdio values require an authorization provider",
                ))?;
            bindings
                .materialize(credentials, reference)
                .map_err(|_| McpError::Protocol("MCP credential resolution failed"))
        };
        let arguments = transport
            .arguments()
            .iter()
            .map(|argument| match argument.literal_value() {
                Some(value) => Ok(value.to_owned()),
                None => argument
                    .credential_reference()
                    .ok_or(McpError::Protocol("MCP stdio argument source is invalid"))
                    .and_then(&materialize),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut environment: std::collections::BTreeMap<_, _> =
            heycode_exec::safe_environment_snapshot()
                .into_iter()
                .collect();
        for (name, value) in transport.environment() {
            let value = match value.literal_value() {
                Some(value) => value.to_owned(),
                None => value
                    .credential_reference()
                    .ok_or(McpError::Protocol(
                        "MCP stdio environment source is invalid",
                    ))
                    .and_then(&materialize)?,
            };
            environment.insert(name.into(), value.into());
        }
        let spec = heycode_exec::ProcessSpec::new(program, transport.cwd().to_path_buf())
            .and_then(|spec| spec.with_args(arguments))
            .and_then(|spec| spec.with_environment(environment))
            .and_then(|spec| spec.with_output_limit_bytes(1024 * 1024))
            .map(|spec| {
                spec.with_output_overflow_policy(heycode_exec::OutputOverflowPolicy::Tail)
                    .with_interactive_stdio()
            })
            .map_err(|_| McpError::Transport("invalid mcp process specification".to_owned()))?;
        let interactive = subprocess
            .spawn_interactive(spec, tokio_util::sync::CancellationToken::new())
            .await
            .map_err(|_| McpError::Transport("mcp subprocess failed".to_owned()))?;
        let (process, stdin, stdout) = interactive.into_parts();
        let streams = ChildStreams { stdin, stdout };
        let (tx, rx) = mpsc::channel::<DriverMsg>(32);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let driver_stop = tokio_util::sync::CancellationToken::new();
        let rt = driver.clone();
        let pending_for_driver = pending.clone();
        let conn_name = name.to_owned();
        let stop = driver_stop.clone();
        let router_for_driver = router.clone();
        let client_capabilities = router.client_capabilities();
        let driver_thread = std::thread::spawn(move || {
            rt.block_on(driver_loop(
                conn_name,
                streams,
                rx,
                pending_for_driver,
                router_for_driver,
                stop,
                liveness,
            ));
        });
        let conn = Self {
            name: name.to_owned(),
            tx,
            process: Mutex::new(Some(process)),
            driver_stop,
            driver_thread: Mutex::new(Some(driver_thread)),
            driver: Mutex::new(Some(driver)),
            request_timeout,
        };
        let initialize = conn
            .request_with_timeout(
                "initialize",
                serde_json::json!({
                    "protocolVersion": McpProtocolVersion::LATEST.as_str(),
                    "capabilities": client_capabilities,
                    "clientInfo": {"name": "heycode", "version": "0.2.0"}
                }),
                startup_timeout,
                &CancellationToken::new(),
            )
            .await?;
        let handshake =
            McpServerHandshake::from_initialize_result(&initialize).map_err(stdio_error)?;
        if handshake.capabilities().logging {
            router.enable_logging();
        }
        conn.notify("notifications/initialized").await?;
        Ok(McpInitializedConnection {
            connection: conn,
            handshake,
        })
    }
}

#[async_trait]
impl McpRequestChannel for McpConnection {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        self.request_with_timeout(method, params, self.request_timeout, cancellation)
            .await
            .map_err(channel_error)
    }
}

/// Project stdio bridge failures onto the transport-neutral channel classes.
fn channel_error(error: McpError) -> McpChannelError {
    match error {
        McpError::Transport(_) => McpChannelError::Transport,
        McpError::Timeout { .. } => McpChannelError::TimedOut,
        McpError::Rpc { code, .. } => McpChannelError::Rpc { code },
        McpError::Protocol(requirement) => McpChannelError::Protocol { requirement },
        McpError::Cancelled => McpChannelError::Cancelled,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use heycode_tools::{Tool, ToolCtx, ToolError};

    use super::*;

    struct StaticTool(heycode_core::ToolSpec);

    #[async_trait]
    impl Tool for StaticTool {
        fn spec(&self) -> heycode_core::ToolSpec {
            self.0.clone()
        }

        async fn run(
            &self,
            args: serde_json::Value,
            _cx: &ToolCtx,
        ) -> Result<serde_json::Value, ToolError> {
            Ok(args)
        }
    }

    struct RecordingSandbox(Arc<Mutex<usize>>);

    impl heycode_exec::Sandbox for RecordingSandbox {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn capabilities(&self) -> heycode_exec::SandboxBackendCapabilities {
            heycode_exec::SandboxBackendCapabilities {
                read_only: heycode_exec::SandboxSupport::Supported,
                workspace_write: heycode_exec::SandboxSupport::Supported,
                network_isolation: heycode_exec::SandboxSupport::Unsupported,
            }
        }

        fn confine(
            &self,
            argv: &[String],
            _policy: &heycode_exec::SandboxPolicy,
        ) -> Result<Vec<String>, heycode_exec::SandboxError> {
            *self.0.lock().unwrap() += 1;
            Ok(argv.to_vec())
        }
    }

    const MOCK: &str = r#"
import sys, json
for line in sys.stdin:
    try:
        req = json.loads(line)
    except Exception:
        continue
    m = req.get("method"); i = req.get("id")
    if m == "initialize":
        resp = {"jsonrpc":"2.0","id":i,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"mock","version":"0"}}}
    elif m == "tools/list":
        resp = {"jsonrpc":"2.0","id":i,"result":{"tools":[
            {"name":"echo","description":"Echo the text back",
             "inputSchema":{"type":"object","additionalProperties":False,
                            "required":["text"],
                            "properties":{"text":{"type":"string"}}}}]}}
    elif m == "tools/call":
        t = req["params"]["arguments"].get("text","")
        resp = {"jsonrpc":"2.0","id":i,"result":{
            "content":[{"type":"text","text":"ECHO:"+t}],"isError":False}}
    else:
        resp = {"jsonrpc":"2.0","id":i,"error":{"code":-32601,"message":"unknown method"}}
    sys.stdout.write(json.dumps(resp)+"\n"); sys.stdout.flush()
"#;

    fn mock_cfg() -> McpServerConfig {
        McpServerConfig {
            command: "python3".into(),
            args: vec!["-u".into(), "-c".into(), MOCK.into()],
            env: HashMap::new(),
            required: false,
        }
    }

    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn handshake_list_and_call_round_trip() {
        if !python3_available() {
            return;
        }
        let conn = McpConnection::spawn("mock", &mock_cfg()).await.unwrap();
        assert_eq!(conn.name(), "mock");

        let cancellation = CancellationToken::new();
        let listed = conn
            .call("tools/list", serde_json::json!({}), &cancellation)
            .await
            .unwrap();
        assert_eq!(listed["tools"].as_array().unwrap().len(), 1);
        assert_eq!(listed["tools"][0]["name"], "echo");

        let called = conn
            .call(
                "tools/call",
                serde_json::json!({"name": "echo", "arguments": {"text": "hello"}}),
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(called["content"][0]["text"], "ECHO:hello");

        cancellation.cancel();
        let cancelled = conn
            .call("tools/list", serde_json::json!({}), &cancellation)
            .await
            .unwrap_err();
        assert_eq!(cancelled, McpChannelError::Cancelled);
        conn.kill();
    }

    #[tokio::test]
    async fn unknown_method_surfaces_rpc_error() {
        if !python3_available() {
            return;
        }
        let conn = McpConnection::spawn("mock", &mock_cfg()).await.unwrap();
        let err = conn
            .request("bogus/method", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::Rpc { code: -32601, .. }));
        conn.kill();
    }

    #[tokio::test]
    async fn failed_initialize_publishes_failed_then_retains_last_good() {
        if !python3_available() {
            return;
        }
        let script = r#"
import sys, json
for line in sys.stdin:
    req = json.loads(line)
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":req.get("id"),"result":{"capabilities":{},"serverInfo":{"name":"broken","version":"1"}}})+"\n")
    sys.stdout.flush()
"#;
        let config = McpServerConfig {
            command: "python3".to_owned(),
            args: vec!["-u".to_owned(), "-c".to_owned(), script.to_owned()],
            env: HashMap::new(),
            required: false,
        };
        let cwd = std::env::current_dir().unwrap();
        let registry = McpRegistry::new();
        let mut owner = heycode_core::Context::new();
        registry
            .register_definition(&owner, stdio_definition("broken", &config, &cwd).unwrap())
            .unwrap();
        let publisher = registry
            .register_connection(
                &owner,
                &McpServerId::new("broken").unwrap(),
                McpConnectionProviderId::new("stdio-local").unwrap(),
                10,
            )
            .unwrap();
        let exact_definition = registry
            .definition(&McpServerId::new("broken").unwrap())
            .unwrap()
            .unwrap();
        let starting = registry.snapshot().unwrap();
        assert_eq!(
            starting.servers()[0].state(),
            &McpConnectionState::Starting { since_ms: 10 }
        );
        assert_eq!(
            starting.servers()[0].authentication(),
            McpAuthenticationState::NotRequired
        );
        let driver = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        );
        let tools = Arc::new(ToolRegistry::new());

        let first = match establish_stdio_generation(StdioGenerationRequest {
            definition: exact_definition.clone(),
            driver: driver.clone(),
            subprocess: SubprocessService::local(),
            tools: tools.clone(),
            publisher: publisher.clone(),
            started_at_ms: 10,
            approval: None,
            client_events: None,
            lifecycle_hooks: None,
            credentials: None,
            credential_bindings: McpCredentialBindings::new(),
        })
        .await
        {
            Ok(_) => panic!("invalid initialize result must fail"),
            Err(error) => error,
        };
        assert!(matches!(first, McpError::Protocol(_)));
        let failed = registry.snapshot().unwrap();
        assert!(matches!(
            failed.servers()[0].state(),
            McpConnectionState::Failed {
                code: McpFailureCode::Protocol,
                ..
            }
        ));
        assert!(failed.servers()[0].last_good_generation().is_none());

        publisher
            .publish_generation(
                McpGenerationCandidate::new(
                    "2024-11-05",
                    "seed",
                    "1",
                    McpCapabilitySet::default(),
                    McpContributionCounts::default(),
                    20,
                )
                .unwrap(),
            )
            .unwrap();
        let second = match establish_stdio_generation(StdioGenerationRequest {
            definition: exact_definition,
            driver: driver.clone(),
            subprocess: SubprocessService::local(),
            tools,
            publisher,
            started_at_ms: 30,
            approval: None,
            client_events: None,
            lifecycle_hooks: None,
            credentials: None,
            credential_bindings: McpCredentialBindings::new(),
        })
        .await
        {
            Ok(_) => panic!("invalid reinitialize result must fail"),
            Err(error) => error,
        };
        assert!(matches!(second, McpError::Protocol(_)));
        let degraded = registry.snapshot().unwrap();
        assert!(matches!(
            degraded.servers()[0].state(),
            McpConnectionState::Degraded {
                code: McpFailureCode::Protocol,
                ..
            }
        ));
        assert_eq!(
            degraded.servers()[0]
                .last_good_generation()
                .unwrap()
                .number(),
            1
        );
        owner.shutdown();
        std::thread::spawn(move || drop(driver)).join().unwrap();
    }

    #[tokio::test]
    async fn stdio_provider_uses_the_exact_definition_startup_budget() {
        if !python3_available() {
            return;
        }
        let script = r#"
import sys, json, time
for line in sys.stdin:
    req = json.loads(line); time.sleep(0.2)
    result = {"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"slow","version":"1"}}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":req.get("id"),"result":result})+"\n"); sys.stdout.flush()
"#;
        let config = McpServerConfig {
            command: "python3".to_owned(),
            args: vec!["-u".to_owned(), "-c".to_owned(), script.to_owned()],
            env: HashMap::new(),
            required: false,
        };
        let cwd = std::env::current_dir().unwrap();
        let definition = stdio_definition("slow", &config, &cwd)
            .unwrap()
            .with_timeouts(McpTimeouts::new(25, 30_000, 60_000, 5_000).unwrap());
        let registry = McpRegistry::new();
        let mut owner = heycode_core::Context::new();
        registry.register_definition(&owner, definition).unwrap();
        let id = McpServerId::new("slow").unwrap();
        let exact = registry.definition(&id).unwrap().unwrap();
        let publisher = registry
            .register_connection(
                &owner,
                &id,
                McpConnectionProviderId::new("stdio-local").unwrap(),
                1,
            )
            .unwrap();
        let driver = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        );
        let outcome = establish_stdio_generation(StdioGenerationRequest {
            definition: exact,
            driver: driver.clone(),
            subprocess: SubprocessService::local(),
            tools: Arc::new(ToolRegistry::new()),
            publisher,
            started_at_ms: 1,
            approval: None,
            client_events: None,
            lifecycle_hooks: None,
            credentials: None,
            credential_bindings: McpCredentialBindings::new(),
        })
        .await;
        assert!(matches!(outcome, Err(McpError::Timeout { .. })));
        let snapshot = registry.snapshot().unwrap();
        assert!(matches!(
            snapshot.servers()[0].state(),
            McpConnectionState::Failed {
                code: McpFailureCode::TimedOut,
                ..
            }
        ));
        owner.shutdown();
        std::thread::spawn(move || drop(driver)).join().unwrap();
    }

    #[tokio::test]
    async fn partial_tool_collision_removes_every_candidate_registration() {
        if !python3_available() {
            return;
        }
        let script = r#"
import sys, json
for line in sys.stdin:
    req = json.loads(line); method = req.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"partial","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[
          {"name":"first","description":"first","inputSchema":{"type":"object"}},
          {"name":"collision","description":"collision","inputSchema":{"type":"object"}}
        ]}
    else: result = {}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":req.get("id"),"result":result})+"\n"); sys.stdout.flush()
"#;
        let config = McpServerConfig {
            command: "python3".to_owned(),
            args: vec!["-u".to_owned(), "-c".to_owned(), script.to_owned()],
            env: HashMap::new(),
            required: false,
        };
        let cwd = std::env::current_dir().unwrap();
        let registry = McpRegistry::new();
        let mut owner = heycode_core::Context::new();
        registry
            .register_definition(&owner, stdio_definition("partial", &config, &cwd).unwrap())
            .unwrap();
        let id = McpServerId::new("partial").unwrap();
        let exact = registry.definition(&id).unwrap().unwrap();
        let publisher = registry
            .register_connection(
                &owner,
                &id,
                McpConnectionProviderId::new("stdio-local").unwrap(),
                1,
            )
            .unwrap();
        let tools = Arc::new(ToolRegistry::new());
        tools
            .register_shared(Arc::new(StaticTool(heycode_core::ToolSpec {
                name: "mcp__partial__collision".to_owned(),
                description: "foreign squatter".to_owned(),
                parameters: serde_json::json!({"type":"object"}),
            })))
            .unwrap();
        let driver = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        );
        let outcome = establish_stdio_generation(StdioGenerationRequest {
            definition: exact,
            driver: driver.clone(),
            subprocess: SubprocessService::local(),
            tools: tools.clone(),
            publisher,
            started_at_ms: 1,
            approval: None,
            client_events: None,
            lifecycle_hooks: None,
            credentials: None,
            credential_bindings: McpCredentialBindings::new(),
        })
        .await;
        assert!(matches!(outcome, Err(McpError::Protocol(_))));
        assert!(tools.get("mcp__partial__first").is_none());
        assert!(tools.get("mcp__partial__collision").is_some());
        assert_eq!(tools.names(), ["mcp__partial__collision"]);
        let snapshot = registry.snapshot().unwrap();
        assert!(snapshot.servers()[0].last_good_generation().is_none());
        // A contested tool name is a contribution conflict, not a server
        // protocol violation.
        assert!(matches!(
            snapshot.servers()[0].state(),
            McpConnectionState::Failed {
                code: McpFailureCode::Conflict,
                ..
            }
        ));
        owner.shutdown();
        std::thread::spawn(move || drop(driver)).join().unwrap();
    }

    #[test]
    fn plugin_inventory_attributes_process_and_discovered_tool() {
        if !python3_available() {
            return;
        }
        let mut servers = HashMap::new();
        servers.insert("mock".to_owned(), McpServerSpec::Stdio(mock_cfg()));
        let sandbox_calls = Arc::new(Mutex::new(0));
        let cwd = std::env::current_dir().unwrap();
        let shell = heycode_exec::LocalShellConfig::platform(
            cwd.clone(),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        let sandbox = heycode_exec::SandboxService::new(
            heycode_exec::SandboxMode::ReadOnly,
            cwd.clone(),
            Some(Arc::new(RecordingSandbox(sandbox_calls.clone()))),
        )
        .unwrap();
        let plugins: Vec<Box<dyn Plugin>> = vec![
            heycode_exec::local_execution_plugin_with_sandbox(shell, sandbox),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
                web_enabled: false,
                ..heycode_tools::ToolsConfig::default()
            }),
            mcp_registry_plugin(),
            mcp_plugin(servers, cwd),
        ];
        let mut context = heycode_core::compose(&plugins).unwrap();
        let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
        assert!(matches!(
            registry.snapshot().unwrap().servers()[0].state(),
            McpConnectionState::Ready { .. }
        ));
        let rows = context.plugin_inventory().snapshot().unwrap().contributions;
        assert!(rows.iter().any(|row| {
            row.plugin == "mcp"
                && row.kind == heycode_core::ContributionKind::ExternalProcess
                && row.name == "mock"
        }));
        assert!(rows.iter().any(|row| {
            row.plugin == "mcp"
                && row.kind == heycode_core::ContributionKind::Tool
                && row.name == "mcp__mock__echo"
        }));
        assert_eq!(*sandbox_calls.lock().unwrap(), 1);
        context.shutdown();
    }

    #[test]
    fn plugin_shutdown_kills_the_mcp_descendant_tree() {
        if !python3_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("mcp-descendant-survived");
        let ready = dir.path().join("mcp-descendant-started");
        let script = r#"
import os, sys, json, subprocess
subprocess.Popen([sys.executable, "-c", "import os,time; time.sleep(0.65); open(os.environ['TREE_MARKER'],'w').write('survived')"], env=os.environ.copy())
open(os.environ["TREE_READY"], "w").write("ready")
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if ident is None: continue
    if method == "initialize": result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"tree","version":"0"}}
    elif method == "tools/list": result = {"tools":[]}
    else: result = {}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;
        let mut env = HashMap::new();
        env.insert(
            "TREE_MARKER".to_owned(),
            marker.to_string_lossy().into_owned(),
        );
        env.insert(
            "TREE_READY".to_owned(),
            ready.to_string_lossy().into_owned(),
        );
        let mut servers = HashMap::new();
        servers.insert(
            "tree".to_owned(),
            McpServerSpec::Stdio(McpServerConfig {
                command: "python3".to_owned(),
                args: vec!["-u".to_owned(), "-c".to_owned(), script.to_owned()],
                env,
                required: false,
            }),
        );
        let cwd = std::env::current_dir().unwrap();
        let plugins: Vec<Box<dyn Plugin>> = vec![
            heycode_exec::local_execution_plugin(
                heycode_exec::LocalShellConfig::platform(
                    cwd.clone(),
                    std::time::Duration::from_secs(30),
                )
                .unwrap(),
            ),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
                web_enabled: false,
                ..heycode_tools::ToolsConfig::default()
            }),
            mcp_registry_plugin(),
            mcp_plugin(servers, cwd),
        ];
        let mut context = heycode_core::compose(&plugins).unwrap();
        let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
        assert!(
            registry.snapshot().unwrap().servers()[0]
                .last_good_generation()
                .is_some()
        );
        assert!(ready.exists(), "mcp parent never spawned its descendant");
        context.shutdown();
        assert!(!registry.snapshot().unwrap().active());
        assert!(registry.snapshot().unwrap().servers().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(900));
        assert!(!marker.exists(), "mcp descendant escaped context shutdown");
    }

    #[test]
    fn plugin_activation_failure_redacts_server_rpc_message() {
        if !python3_available() {
            return;
        }
        let script = r#"
import sys, json
for line in sys.stdin:
    req = json.loads(line)
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":req.get("id"),"error":{"code":-32000,"message":"RPC_SECRET_CANARY"}})+"\n")
    sys.stdout.flush()
"#;
        let mut servers = HashMap::new();
        servers.insert(
            "redacted".to_owned(),
            McpServerSpec::Stdio(McpServerConfig {
                command: "python3".to_owned(),
                args: vec!["-u".to_owned(), "-c".to_owned(), script.to_owned()],
                env: HashMap::new(),
                // Only a required server aborts activation; that is the path
                // whose error text must stay free of the server's message.
                required: true,
            }),
        );
        let cwd = std::env::current_dir().unwrap();
        let plugins: Vec<Box<dyn Plugin>> = vec![
            heycode_exec::local_execution_plugin(
                heycode_exec::LocalShellConfig::platform(
                    cwd.clone(),
                    std::time::Duration::from_secs(30),
                )
                .unwrap(),
            ),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
                web_enabled: false,
                ..heycode_tools::ToolsConfig::default()
            }),
            mcp_registry_plugin(),
            mcp_plugin(servers, cwd),
        ];
        let error = match heycode_core::compose(&plugins) {
            Ok(_) => panic!("initialize RPC failure must abort required MCP activation"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("mcp protocol failed"), "{error}");
        assert!(error.contains("`redacted`"), "names the server: {error}");
        assert!(!error.contains("RPC_SECRET_CANARY"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod concurrent_connect_tests {
    use super::*;

    /// Startup waits on every server at once, not one after another, and the
    /// answers still line up with the servers that produced them.
    #[test]
    fn driving_many_connects_overlaps_their_waits_and_keeps_input_order() {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
        );
        let futures: Vec<ConnectFuture<u8>> = (0_u8..6)
            .map(|index| {
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    if index == 3 {
                        Err(McpError::Cancelled)
                    } else {
                        Ok(index)
                    }
                }) as ConnectFuture<u8>
            })
            .collect();

        let started = std::time::Instant::now();
        let outcomes = drive_all_on_plain_thread(&runtime, futures);
        let elapsed = started.elapsed();

        assert_eq!(outcomes.len(), 6);
        for (index, outcome) in outcomes.iter().enumerate() {
            match (index, outcome) {
                (3, Err(error)) => assert!(
                    error.to_string().contains("cancelled"),
                    "one server's failure is classified without disturbing the others: {error}"
                ),
                (index, Ok(value)) => {
                    assert_eq!(usize::from(*value), index, "answers stay in order")
                }
                (index, other) => panic!("unexpected outcome at {index}: {other:?}"),
            }
        }
        assert!(
            elapsed < std::time::Duration::from_millis(700),
            "six 200ms handshakes overlap instead of costing 1.2s: {elapsed:?}"
        );
        assert!(
            drive_all_on_plain_thread::<u8>(&runtime, Vec::new()).is_empty(),
            "no servers, no thread"
        );
        let _released = std::thread::spawn(move || drop(runtime)).join();
    }
}

//! Stable heycode-local JSON-RPC v1 and in-process client.

mod controls;
/// Authenticated detached loopback hosting over the existing AppServer.
mod stdio;

pub use controls::app_server_controls_plugin;
pub use heycode_sdk::*;
pub use stdio::serve_stdio_transport;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock as AsyncRwLock, mpsc};
use tokio_util::sync::CancellationToken;

use heycode_runtime::{
    AgentRuntime, AgentRuntimeRegistry, RuntimeErrorCode, RuntimeEvent, RuntimeEventKind,
    RuntimeFinishReason, RuntimeInput, RuntimeResume, RuntimeSession, RuntimeSessionId,
    RuntimeStart,
};

const AGENT_PERMISSION_PREFIX: &str = "agent-request-";
const AGENT_QUESTION_PREFIX: &str = "agent-question-";

/// Context service key for the stable local app-server.
pub const SERVICE_APP_SERVER: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("app-server");

/// Event sink that round-trips every notification through the stable JSON wire.
#[derive(Clone)]
pub struct AppEventSink {
    method: &'static str,
    session_id: Option<String>,
    sequence: Arc<AtomicU64>,
    sender: mpsc::Sender<AppServerNotification>,
}

impl AppEventSink {
    fn session(
        session_id: String,
        sequence: Arc<AtomicU64>,
        sender: mpsc::Sender<AppServerNotification>,
    ) -> Self {
        Self {
            method: "session/event",
            session_id: Some(session_id),
            sequence,
            sender,
        }
    }

    pub(crate) fn control(
        sequence: Arc<AtomicU64>,
        sender: mpsc::Sender<AppServerNotification>,
    ) -> Self {
        Self {
            method: "control/event",
            session_id: None,
            sequence,
            sender,
        }
    }

    pub(crate) async fn emit(&self, event: AppServerEvent) -> Result<(), AppServerError> {
        let notification = AppServerNotification {
            jsonrpc: "2.0".to_owned(),
            method: self.method.to_owned(),
            params: AppServerEventParams {
                session_id: self.session_id.clone(),
                sequence: self.sequence.fetch_add(1, Ordering::SeqCst),
                event,
            },
        };
        let wire = serde_json::to_vec(&notification).map_err(|_| AppServerError::invalid())?;
        let verified = serde_json::from_slice::<AppServerNotification>(&wire)
            .map_err(|_| AppServerError::invalid())?;
        let _sent = self.sender.send(verified).await;
        Ok(())
    }
}

#[async_trait]
trait AppBackend: Send + Sync {
    async fn open(
        &self,
        configuration: AppRuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<AppSessionInfo, AppServerError>;

    async fn configure(
        &self,
        configuration: AppRuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<AppSessionInfo, AppServerError>;

    async fn turn(
        &self,
        text: String,
        attachments: Vec<heycode_core::AttachmentMetadata>,
        pending_message_id: Option<String>,
        sink: AppEventSink,
        cancellation: CancellationToken,
    ) -> Result<AppTurnResult, AppServerError>;

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), AppServerError>;

    async fn respond_permission(
        &self,
        request_id: String,
        decision: AppPermissionDecision,
        cancellation: CancellationToken,
    ) -> Result<(), AppServerError>;

    async fn respond_question(
        &self,
        request_id: String,
        answer: Option<String>,
        cancellation: CancellationToken,
    ) -> Result<(), AppServerError>;

    async fn respond_question_selected(
        &self,
        _request_id: String,
        _answers: Vec<String>,
        _cancellation: CancellationToken,
    ) -> Result<(), AppServerError> {
        Err(AppServerError::invalid())
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), AppServerError>;

    /// Cancel and close only an already-created session, then reject reuse of
    /// this backend generation. Unlike ordinary `cancel`/`close`, retirement
    /// must never create a session merely to tear it down.
    async fn retire(&self) -> Result<(), AppServerError>;
}

trait AppBackendFactory: Send + Sync {
    fn native_workspace(&self) -> Option<PathBuf> {
        None
    }
    fn build(
        &self,
        runtime: Arc<dyn AgentRuntime>,
        workspace: PathBuf,
        model: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Arc<dyn AppBackend>;

    fn validate_route_change(&self) -> Result<(), AppServerError>;

    fn validate_runtime_selection(&self, runtime_id: &str) -> Result<(), AppServerError>;
}

struct NativeBackendFactory {
    agent: Arc<heycode_agent::Agent>,
    approval: Option<Arc<heycode_agent::InteractiveApproval>>,
    questions: Arc<heycode_agent::InteractiveQuestion>,
    lifecycle: CancellationToken,
}

impl AppBackendFactory for NativeBackendFactory {
    fn native_workspace(&self) -> Option<PathBuf> {
        Some(self.agent.cwd())
    }
    fn build(
        &self,
        runtime: Arc<dyn AgentRuntime>,
        workspace: PathBuf,
        model: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Arc<dyn AppBackend> {
        Arc::new(NativeBackend::new(
            self,
            runtime,
            workspace,
            model,
            reasoning_effort,
        ))
    }

    fn validate_route_change(&self) -> Result<(), AppServerError> {
        if self.agent.token().is_turn_active() {
            return Err(AppServerError::classified(AppServerErrorCode::Conflict));
        }
        Ok(())
    }

    fn validate_runtime_selection(&self, runtime_id: &str) -> Result<(), AppServerError> {
        self.validate_route_change()?;
        let session = self
            .agent
            .session()
            .lock()
            .map_err(|_| AppServerError::unavailable())?;
        match session.runtime_link() {
            Some((linked, _)) if linked != runtime_id => {
                Err(AppServerError::classified(AppServerErrorCode::Conflict))
            }
            None if !session.is_fresh() && runtime_id != self.agent.runtime_id() => {
                Err(AppServerError::classified(AppServerErrorCode::Conflict))
            }
            _ => Ok(()),
        }
    }
}

/// Effect-owned stable protocol service.
pub struct AppServer {
    backend: std::sync::RwLock<BackendEntry>,
    connection_enabled: AtomicBool,
    next_backend_generation: AtomicU64,
    native_backend: Arc<dyn AppBackend>,
    native_runtime: Arc<dyn AgentRuntime>,
    backend_factory: Arc<dyn AppBackendFactory>,
    composed_workspace: PathBuf,
    operation_gate: Arc<AsyncRwLock<()>>,
    event_sequence: Arc<AtomicU64>,
    lifecycle: CancellationToken,
    controls: std::sync::Mutex<Option<ControlEntry>>,
}

struct BackendEntry {
    backend: Arc<dyn AppBackend>,
    runtime: Arc<dyn AgentRuntime>,
    workspace: PathBuf,
    workspace_selected: bool,
    model: Option<String>,
    reasoning_effort: Option<String>,
    opened: bool,
    session_id: Option<String>,
    generation: u64,
    token: Arc<()>,
}

struct BackendHandle {
    backend: Arc<dyn AppBackend>,
    token: Arc<()>,
}

struct ControlEntry {
    plane: Arc<controls::AppControlPlane>,
    token: Arc<()>,
}

/// Owned protocol-admission fence for a native workspace transition.
pub struct AppWorkspacePause {
    _operation: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

impl AppServer {
    /// Stop admission under the quiescence permit before releasing it for
    /// ordinary Context teardown. No session is created or detached here.
    pub fn close_for_recomposition(&self) {
        self.connection_enabled.store(false, Ordering::SeqCst);
        self.lifecycle.cancel();
    }

    /// Fence all protocol admissions for a human-requested full recomposition.
    /// Idle delegated sessions may be retired by their existing owning context.
    pub fn pause_for_recomposition(&self) -> Result<AppWorkspacePause, AppServerError> {
        let operation = self
            .operation_gate
            .clone()
            .try_write_owned()
            .map_err(|_| AppServerError::classified(AppServerErrorCode::Conflict))?;
        Ok(AppWorkspacePause {
            _operation: Some(operation),
        })
    }

    fn effective_native_workspace(&self) -> PathBuf {
        self.backend_factory
            .native_workspace()
            .unwrap_or_else(|| self.composed_workspace.clone())
    }

    /// Fence protocol admission while a host changes native workspace authority.
    /// A model tool may keep its already-running native turn; route changes are
    /// independently refused while that exact Agent turn remains active.
    pub fn pause_workspace(
        &self,
        native_tool_barrier: bool,
    ) -> Result<AppWorkspacePause, AppServerError> {
        let operation = if native_tool_barrier {
            None
        } else {
            Some(
                self.operation_gate
                    .clone()
                    .try_write_owned()
                    .map_err(|_| AppServerError::classified(AppServerErrorCode::Conflict))?,
            )
        };
        let current = self
            .backend
            .read()
            .map_err(|_| AppServerError::unavailable())?;
        if current.runtime.descriptor().kind() != heycode_runtime::AgentRuntimeKind::Native {
            return Err(AppServerError::classified(AppServerErrorCode::Conflict));
        }
        Ok(AppWorkspacePause {
            _operation: operation,
        })
    }

    fn runtime(
        agent: Arc<heycode_agent::Agent>,
        approval: Option<Arc<heycode_agent::InteractiveApproval>>,
        questions: Arc<heycode_agent::InteractiveQuestion>,
        runtime: Arc<dyn AgentRuntime>,
    ) -> Self {
        let lifecycle = CancellationToken::new();
        let composed_workspace = agent.cwd().to_path_buf();
        let backend_factory: Arc<dyn AppBackendFactory> = Arc::new(NativeBackendFactory {
            agent,
            approval,
            questions,
            lifecycle: lifecycle.clone(),
        });
        Self::with_factory(backend_factory, runtime, composed_workspace, lifecycle)
    }

    fn with_factory(
        backend_factory: Arc<dyn AppBackendFactory>,
        native_runtime: Arc<dyn AgentRuntime>,
        composed_workspace: PathBuf,
        lifecycle: CancellationToken,
    ) -> Self {
        let native_backend = backend_factory.build(
            native_runtime.clone(),
            composed_workspace.clone(),
            None,
            None,
        );
        Self {
            backend: std::sync::RwLock::new(BackendEntry {
                backend: native_backend.clone(),
                runtime: native_runtime.clone(),
                workspace: composed_workspace.clone(),
                workspace_selected: false,
                model: None,
                reasoning_effort: None,
                opened: false,
                session_id: None,
                generation: 1,
                token: Arc::new(()),
            }),
            connection_enabled: AtomicBool::new(true),
            next_backend_generation: AtomicU64::new(2),
            native_backend,
            native_runtime,
            backend_factory,
            composed_workspace,
            operation_gate: Arc::new(AsyncRwLock::new(())),
            event_sequence: Arc::new(AtomicU64::new(0)),
            lifecycle,
            controls: std::sync::Mutex::new(None),
        }
    }

    fn backend(&self) -> Result<BackendHandle, AppServerError> {
        if !self.connection_enabled.load(Ordering::SeqCst) {
            return Err(AppServerError::classified(AppServerErrorCode::Closed));
        }
        self.backend
            .read()
            .map(|entry| BackendHandle {
                backend: entry.backend.clone(),
                token: entry.token.clone(),
            })
            .map_err(|_| AppServerError::unavailable())
    }

    fn allocate_backend_generation(&self) -> Result<u64, AppServerError> {
        self.next_backend_generation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |generation| {
                generation.checked_add(1)
            })
            .map_err(|_| AppServerError::unavailable())
    }

    fn opened_backend(&self, session_id: &str) -> Result<BackendHandle, AppServerError> {
        if !self.connection_enabled.load(Ordering::SeqCst) {
            return Err(AppServerError::classified(AppServerErrorCode::Closed));
        }
        let current = self
            .backend
            .read()
            .map_err(|_| AppServerError::unavailable())?;
        if !current.opened {
            return Err(AppServerError::classified(AppServerErrorCode::Conflict));
        }
        if current.session_id.as_deref() != Some(session_id) {
            return Err(AppServerError::invalid());
        }
        Ok(BackendHandle {
            backend: current.backend.clone(),
            token: current.token.clone(),
        })
    }

    fn mark_opened(&self, handle: &BackendHandle, session_id: &str) -> Result<(), AppServerError> {
        let mut current = self
            .backend
            .write()
            .map_err(|_| AppServerError::unavailable())?;
        if !Arc::ptr_eq(&current.token, &handle.token) {
            return Err(AppServerError::classified(AppServerErrorCode::Conflict));
        }
        current.opened = true;
        current.session_id = Some(session_id.to_owned());
        Ok(())
    }

    async fn open_backend(
        &self,
        configuration: AppRuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<(BackendHandle, AppSessionInfo), AppServerError> {
        let backend = self.backend()?;
        let info = backend.backend.open(configuration, cancellation).await?;
        self.mark_opened(&backend, &info.session_id)?;
        Ok((backend, info))
    }

    async fn invalidate_backend_configuration(
        &self,
        expected_runtime: &str,
        expected_generation: u64,
        durable_model: Option<String>,
        durable_reasoning_effort: Option<String>,
    ) -> Result<(), AppServerError> {
        let _operation = self.operation_gate.write().await;
        let retired = {
            let mut current = self
                .backend
                .write()
                .map_err(|_| AppServerError::unavailable())?;
            if current.runtime.descriptor().id().as_str() != expected_runtime
                || current.generation != expected_generation
            {
                return Ok(());
            }
            let replacement = self.backend_factory.build(
                current.runtime.clone(),
                if current.runtime.descriptor().kind() == heycode_runtime::AgentRuntimeKind::Native
                {
                    self.effective_native_workspace()
                } else {
                    current.workspace.clone()
                },
                durable_model.clone(),
                durable_reasoning_effort.clone(),
            );
            let generation = self.allocate_backend_generation()?;
            let retired = current.backend.clone();
            current.backend = replacement;
            current.opened = false;
            current.session_id = None;
            current.generation = generation;
            current.token = Arc::new(());
            retired
        };
        // Detachment is the safety boundary. A close failure may leak an
        // upstream process, but no app-server request can reach it again.
        let _closed = retired.close(CancellationToken::new()).await;
        Ok(())
    }

    async fn disconnect_backend(&self, expected_runtime: &str) -> Result<(), AppServerError> {
        let backend = {
            let current = self
                .backend
                .read()
                .map_err(|_| AppServerError::unavailable())?;
            if current.runtime.descriptor().id().as_str() != expected_runtime {
                return Err(AppServerError::classified(AppServerErrorCode::Conflict));
            }
            current.backend.clone()
        };
        if self
            .connection_enabled
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }

        // Disable new calls before cancelling the active backend. The write
        // gate then waits for every in-flight request to settle before the
        // backend generation is detached and closed.
        let _retired = backend.retire().await;
        let _operation = self.operation_gate.write().await;
        {
            let mut current = self
                .backend
                .write()
                .map_err(|_| AppServerError::unavailable())?;
            current.opened = false;
            current.session_id = None;
            current.generation = self.allocate_backend_generation()?;
            current.token = Arc::new(());
        }
        Ok(())
    }

    fn commit_route<T>(
        &self,
        commit: impl FnOnce() -> Result<T, AppServerError>,
    ) -> Result<T, AppServerError> {
        let _operation = self
            .operation_gate
            .try_write()
            .map_err(|_| AppServerError::classified(AppServerErrorCode::Conflict))?;
        self.backend_factory.validate_route_change()?;
        commit()
    }

    fn select_runtime_after<T>(
        &self,
        runtime: Arc<dyn AgentRuntime>,
        commit: impl FnOnce() -> Result<T, AppServerError>,
    ) -> Result<T, AppServerError> {
        let _operation = self
            .operation_gate
            .try_write()
            .map_err(|_| AppServerError::classified(AppServerErrorCode::Conflict))?;
        self.backend_factory
            .validate_runtime_selection(runtime.descriptor().id().as_str())?;
        let mut current = self
            .backend
            .write()
            .map_err(|_| AppServerError::unavailable())?;
        if current.opened {
            return Err(AppServerError::classified(AppServerErrorCode::Conflict));
        }
        let kind = runtime.descriptor().kind();
        let (workspace, workspace_selected) = if kind == heycode_runtime::AgentRuntimeKind::Native
            || current.runtime.descriptor().kind() == heycode_runtime::AgentRuntimeKind::Native
        {
            (self.effective_native_workspace(), false)
        } else {
            (current.workspace.clone(), current.workspace_selected)
        };
        let candidate = self
            .backend_factory
            .build(runtime.clone(), workspace.clone(), None, None);
        let generation = self.allocate_backend_generation()?;
        let committed = commit()?;
        *current = BackendEntry {
            backend: candidate,
            runtime,
            workspace,
            workspace_selected,
            model: None,
            reasoning_effort: None,
            opened: false,
            session_id: None,
            generation,
            token: Arc::new(()),
        };
        Ok(committed)
    }

    fn select_workspace(&self, requested: &Path) -> Result<AppWorkspaceSelection, AppServerError> {
        let composed_workspace = self.effective_native_workspace();
        let workspace = canonical_allowed_workspace(requested, &composed_workspace)?;
        let _operation = self
            .operation_gate
            .try_write()
            .map_err(|_| AppServerError::classified(AppServerErrorCode::Conflict))?;
        self.backend_factory.validate_route_change()?;
        let mut current = self
            .backend
            .write()
            .map_err(|_| AppServerError::unavailable())?;
        let runtime_id = current.runtime.descriptor().id().as_str().to_owned();
        if current.runtime.descriptor().kind() == heycode_runtime::AgentRuntimeKind::Native {
            if workspace != composed_workspace {
                return Err(AppServerError::classified(AppServerErrorCode::Unsupported));
            }
            return Ok(AppWorkspaceSelection {
                cwd: composed_workspace,
                runtime: runtime_id,
                selected: false,
            });
        }
        if current.opened {
            return Err(AppServerError::classified(AppServerErrorCode::Conflict));
        }
        let runtime = current.runtime.clone();
        let candidate = self.backend_factory.build(
            runtime.clone(),
            workspace.clone(),
            current.model.clone(),
            current.reasoning_effort.clone(),
        );
        let generation = self.allocate_backend_generation()?;
        *current = BackendEntry {
            backend: candidate,
            runtime,
            workspace: workspace.clone(),
            workspace_selected: true,
            model: current.model.clone(),
            reasoning_effort: current.reasoning_effort.clone(),
            opened: false,
            session_id: None,
            generation,
            token: Arc::new(()),
        };
        Ok(AppWorkspaceSelection {
            cwd: workspace,
            runtime: runtime_id,
            selected: true,
        })
    }

    pub(crate) fn bind_runtime(
        self: &Arc<Self>,
        context: &heycode_core::Context,
        runtime: Arc<dyn AgentRuntime>,
        model: Option<String>,
        reasoning_effort: Option<String>,
    ) -> heycode_core::CoreResult<()> {
        let _operation = self
            .operation_gate
            .try_write()
            .map_err(|_| heycode_core::CoreError::other("app-server runtime selection is busy"))?;
        let token = Arc::new(());
        let generation = self.allocate_backend_generation().map_err(|_| {
            heycode_core::CoreError::other("app-server backend generation exhausted")
        })?;
        let backend = self.backend_factory.build(
            runtime.clone(),
            self.effective_native_workspace(),
            model.clone(),
            reasoning_effort.clone(),
        );
        {
            let mut current = self
                .backend
                .write()
                .map_err(|_| heycode_core::CoreError::other("app-server backend unavailable"))?;
            if current.opened {
                return Err(heycode_core::CoreError::other(
                    "app-server runtime session is already open",
                ));
            }
            *current = BackendEntry {
                backend,
                runtime,
                workspace: self.effective_native_workspace(),
                workspace_selected: false,
                model,
                reasoning_effort,
                opened: false,
                session_id: None,
                generation,
                token: token.clone(),
            };
        }
        let server = Arc::downgrade(self);
        context.effect(move || {
            let Some(server) = server.upgrade() else {
                return;
            };
            let Ok(mut current) = server.backend.write() else {
                return;
            };
            if Arc::ptr_eq(&current.token, &token) {
                let Ok(generation) = server.allocate_backend_generation() else {
                    return;
                };
                *current = BackendEntry {
                    backend: server.native_backend.clone(),
                    runtime: server.native_runtime.clone(),
                    workspace: server.effective_native_workspace(),
                    workspace_selected: false,
                    model: None,
                    reasoning_effort: None,
                    opened: false,
                    session_id: None,
                    generation,
                    token: Arc::new(()),
                };
            }
        });
        Ok(())
    }

    fn control_plane(&self) -> Result<Option<Arc<controls::AppControlPlane>>, AppServerError> {
        self.controls
            .lock()
            .map(|entry| entry.as_ref().map(|entry| entry.plane.clone()))
            .map_err(|_| AppServerError::unavailable())
    }

    pub(crate) fn register_controls(
        self: &Arc<Self>,
        context: &heycode_core::Context,
        plane: Arc<controls::AppControlPlane>,
    ) -> heycode_core::CoreResult<()> {
        let token = Arc::new(());
        {
            let mut controls = self
                .controls
                .lock()
                .map_err(|_| heycode_core::CoreError::other("app-server controls unavailable"))?;
            if controls.is_some() {
                return Err(heycode_core::CoreError::other(
                    "app-server controls are already registered",
                ));
            }
            *controls = Some(ControlEntry {
                plane,
                token: token.clone(),
            });
        }
        let server = Arc::downgrade(self);
        context.effect(move || {
            let Some(server) = server.upgrade() else {
                return;
            };
            let Ok(mut controls) = server.controls.lock() else {
                return;
            };
            if controls
                .as_ref()
                .is_some_and(|entry| Arc::ptr_eq(&entry.token, &token))
            {
                controls.take();
            }
        });
        Ok(())
    }

    /// Dispatch one JSON-RPC v1 request and return its JSON-RPC response.
    /// Events generated while the request runs are verified stable
    /// `session/event` notifications sent to `events`.
    pub async fn request(
        &self,
        request: &str,
        events: mpsc::Sender<AppServerNotification>,
        cancellation: CancellationToken,
    ) -> String {
        let response = match parse_request(request) {
            Ok(request) => self.dispatch(request, events, cancellation).await,
            Err(error) => RpcResponse::error(Value::Null, &error),
        };
        serde_json::to_string(&response).unwrap_or_else(|_| {
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"app-server response failed\"}}".to_owned()
        })
    }

    async fn dispatch(
        &self,
        request: RpcRequest,
        events: mpsc::Sender<AppServerNotification>,
        cancellation: CancellationToken,
    ) -> RpcResponse {
        if self.lifecycle.is_cancelled() {
            if request.method == "session/close" {
                // Teardown remains available after admission closes. Retire
                // never creates a session and is idempotent for a closed one.
                let backend = self.backend.read().ok().map(|entry| entry.backend.clone());
                return match backend {
                    Some(backend) => match backend.retire().await {
                        Ok(()) => RpcResponse::result(request.id, Value::Null),
                        Err(error) => RpcResponse::error(request.id, &error),
                    },
                    None => RpcResponse::error(request.id, &AppServerError::unavailable()),
                };
            }
            return RpcResponse::error(
                request.id,
                &AppServerError::classified(AppServerErrorCode::Closed),
            );
        }
        if !self.connection_enabled.load(Ordering::SeqCst) {
            if request.method == "session/close" {
                return RpcResponse::result(request.id, Value::Null);
            }
            return RpcResponse::error(
                request.id,
                &AppServerError::classified(AppServerErrorCode::Closed),
            );
        }
        let result = match request.method.as_str() {
            "initialize" => self.control_plane().and_then(|controls| {
                serde_json::to_value(controls::initialize_result(controls.is_some()))
                    .map_err(|_| AppServerError::invalid())
            }),
            "session/open" => {
                let params = serde_json::from_value::<SessionOpenParams>(request.params)
                    .map_err(|_| AppServerError::invalid());
                let _operation = self.operation_gate.read().await;
                match params {
                    Ok(params) => self
                        .open_backend(params.configuration, cancellation)
                        .await
                        .and_then(|(_, value)| {
                            serde_json::to_value(value).map_err(|_| AppServerError::invalid())
                        }),
                    Err(error) => Err(error),
                }
            }
            "session/configure" => {
                let params = serde_json::from_value::<SessionConfigureParams>(request.params)
                    .map_err(|_| AppServerError::invalid());
                match params {
                    Ok(params) => {
                        async {
                            let _operation = self.operation_gate.read().await;
                            let backend = self.opened_backend(&params.session_id)?;
                            let info = backend
                                .backend
                                .configure(params.configuration, cancellation)
                                .await?;
                            if info.session_id != params.session_id {
                                return Err(AppServerError::invalid());
                            }
                            serde_json::to_value(info).map_err(|_| AppServerError::invalid())
                        }
                        .await
                    }
                    Err(error) => Err(error),
                }
            }
            "runtime/models" => {
                let params = serde_json::from_value::<EmptyParams>(request.params)
                    .map_err(|_| AppServerError::invalid());
                match params {
                    Ok(_) => {
                        let runtime = self
                            .backend
                            .read()
                            .map_err(|_| AppServerError::unavailable())
                            .map(|entry| entry.runtime.clone());
                        match runtime {
                            Ok(runtime) => runtime
                                .model_configurations(cancellation)
                                .await
                                .map_err(map_runtime_error)
                                .and_then(|rows| {
                                    serde_json::to_value(
                                        rows.into_iter()
                                            .map(app_runtime_model_configuration)
                                            .collect::<Vec<_>>(),
                                    )
                                    .map_err(|_| AppServerError::invalid())
                                }),
                            Err(error) => Err(error),
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            "turn/start" | "turn/follow-up" => {
                let params = if request.method == "turn/follow-up" {
                    serde_json::from_value::<PendingTurnParams>(request.params)
                        .map(|p| TurnStartParams {
                            session_id: p.session_id,
                            text: String::new(),
                            attachments: Vec::new(),
                            pending_message_id: Some(p.message_id),
                        })
                        .map_err(|_| AppServerError::invalid())
                } else {
                    serde_json::from_value::<TurnStartParams>(request.params)
                        .map_err(|_| AppServerError::invalid())
                };
                match params {
                    Ok(params) => {
                        async {
                            let _operation = self.operation_gate.read().await;
                            let (backend, info) = self
                                .open_backend(
                                    AppRuntimeConfiguration::default(),
                                    cancellation.child_token(),
                                )
                                .await?;
                            if info.session_id != params.session_id {
                                return Err(AppServerError::invalid());
                            }
                            let sink = AppEventSink::session(
                                info.session_id,
                                self.event_sequence.clone(),
                                events,
                            );
                            backend
                                .backend
                                .turn(
                                    params.text,
                                    params.attachments,
                                    params.pending_message_id,
                                    sink,
                                    cancellation,
                                )
                                .await
                                .and_then(|value| {
                                    serde_json::to_value(value)
                                        .map_err(|_| AppServerError::invalid())
                                })
                        }
                        .await
                    }
                    Err(error) => Err(error),
                }
            }
            "turn/cancel" => {
                let _operation = self.operation_gate.read().await;
                match self
                    .open_backend(
                        AppRuntimeConfiguration::default(),
                        cancellation.child_token(),
                    )
                    .await
                {
                    Ok((backend, _)) => backend
                        .backend
                        .cancel(cancellation)
                        .await
                        .map(|()| Value::Null),
                    Err(error) => Err(error),
                }
            }
            "session/permission/respond" => {
                let params = serde_json::from_value::<PermissionResponseParams>(request.params)
                    .map_err(|_| AppServerError::invalid());
                match params {
                    Ok(params) => {
                        async {
                            let _operation = self.operation_gate.read().await;
                            let (backend, info) = self
                                .open_backend(
                                    AppRuntimeConfiguration::default(),
                                    cancellation.child_token(),
                                )
                                .await?;
                            if info.session_id != params.session_id {
                                return Err(AppServerError::invalid());
                            }
                            backend
                                .backend
                                .respond_permission(
                                    params.request_id,
                                    params.decision,
                                    cancellation,
                                )
                                .await
                                .map(|()| Value::Null)
                        }
                        .await
                    }
                    Err(error) => Err(error),
                }
            }
            "session/question/respond" => {
                let params = serde_json::from_value::<QuestionResponseParams>(request.params)
                    .map_err(|_| AppServerError::invalid());
                match params {
                    Ok(params)
                        if usize::from(params.answer.is_some())
                            + usize::from(params.selected_answers.is_some())
                            + usize::from(params.cancelled)
                            == 1 =>
                    {
                        async {
                            let _operation = self.operation_gate.read().await;
                            let (backend, info) = self
                                .open_backend(
                                    AppRuntimeConfiguration::default(),
                                    cancellation.child_token(),
                                )
                                .await?;
                            if info.session_id != params.session_id {
                                return Err(AppServerError::invalid());
                            }
                            if let Some(answers) = params.selected_answers {
                                backend
                                    .backend
                                    .respond_question_selected(
                                        params.request_id,
                                        answers,
                                        cancellation,
                                    )
                                    .await
                            } else {
                                backend
                                    .backend
                                    .respond_question(
                                        params.request_id,
                                        params.answer,
                                        cancellation,
                                    )
                                    .await
                            }
                            .map(|()| Value::Null)
                        }
                        .await
                    }
                    Ok(_) => Err(AppServerError::invalid()),
                    Err(error) => Err(error),
                }
            }
            "session/close" => {
                let _operation = self.operation_gate.read().await;
                match self
                    .open_backend(
                        AppRuntimeConfiguration::default(),
                        cancellation.child_token(),
                    )
                    .await
                {
                    Ok((backend, _)) => backend
                        .backend
                        .close(cancellation)
                        .await
                        .map(|()| Value::Null),
                    Err(error) => Err(error),
                }
            }
            method if APP_CONTROL_METHODS.contains(&method) => match self.control_plane() {
                Ok(Some(controls)) => {
                    let operation = cancellation.clone();
                    let dispatched = controls.dispatch(
                        method,
                        request.params,
                        events,
                        self.event_sequence.clone(),
                        operation.clone(),
                    );
                    tokio::pin!(dispatched);
                    tokio::select! {
                        result = &mut dispatched => result,
                        () = self.lifecycle.cancelled() => {
                            operation.cancel();
                            let _settled = dispatched.await;
                            Err(AppServerError::classified(AppServerErrorCode::Closed))
                        }
                    }
                }
                Ok(None) => Err(AppServerError::unavailable()),
                Err(error) => Err(error),
            },
            _ => Err(AppServerError::classified(
                AppServerErrorCode::MethodNotFound,
            )),
        };
        match result {
            Ok(result) => RpcResponse::result(request.id, result),
            Err(error) => RpcResponse::error(request.id, &error),
        }
    }
}

pub(crate) struct RoutingControlHandle {
    server: std::sync::Weak<AppServer>,
    routing: Arc<heycode_routing::RoutingService>,
}

impl RoutingControlHandle {
    pub(crate) fn new(
        server: &Arc<AppServer>,
        routing: Arc<heycode_routing::RoutingService>,
    ) -> Self {
        Self {
            server: Arc::downgrade(server),
            routing,
        }
    }

    fn server(&self) -> Result<Arc<AppServer>, String> {
        self.server
            .upgrade()
            .ok_or_else(|| "app-server is closed".to_owned())
    }
}

#[async_trait]
impl heycode_routing::DelegatedRuntimeControls for RoutingControlHandle {
    fn active_runtime_id(&self) -> Result<String, String> {
        let server = self.server()?;
        if !server.connection_enabled.load(Ordering::SeqCst) {
            return Err("app-server connection is closed".to_owned());
        }
        server
            .backend
            .read()
            .map(|entry| entry.runtime.descriptor().id().as_str().to_owned())
            .map_err(|_| "app-server backend is unavailable".to_owned())
    }

    async fn models(
        &self,
        expected_runtime: &str,
        cancellation: CancellationToken,
    ) -> Result<heycode_llm::CatalogSnapshot, String> {
        let server = self.server()?;
        let _operation = server.operation_gate.read().await;
        let runtime = server
            .backend
            .read()
            .map_err(|_| "app-server backend is unavailable".to_owned())?
            .runtime
            .clone();
        if runtime.descriptor().id().as_str() != expected_runtime {
            return Err("active delegated runtime changed".to_owned());
        }
        runtime
            .models(cancellation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn model_configurations(
        &self,
        expected_runtime: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<heycode_runtime::RuntimeModelConfiguration>, String> {
        let server = self.server()?;
        let _operation = server.operation_gate.read().await;
        let runtime = server
            .backend
            .read()
            .map_err(|_| "app-server backend is unavailable".to_owned())?
            .runtime
            .clone();
        if runtime.descriptor().id().as_str() != expected_runtime {
            return Err("active delegated runtime changed".to_owned());
        }
        runtime
            .model_configurations(cancellation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn configure(
        &self,
        expected_runtime: &str,
        update: heycode_runtime::RuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<heycode_routing::AppliedDelegatedConfiguration, String> {
        let server = self.server()?;
        let operation = server.operation_gate.read().await;
        let (backend, generation) = {
            let current = server
                .backend
                .read()
                .map_err(|_| "app-server backend is unavailable".to_owned())?;
            if current.runtime.descriptor().id().as_str() != expected_runtime {
                return Err("active delegated runtime changed".to_owned());
            }
            if !current.opened {
                return Err("delegated runtime session is not open".to_owned());
            }
            (current.backend.clone(), current.generation)
        };
        let info = backend
            .configure(app_runtime_configuration(&update), cancellation)
            .await
            .map_err(|error| error.to_string())?;
        if info.runtime_id != expected_runtime {
            drop(operation);
            let _retired = self.invalidate(expected_runtime, generation).await;
            return Err(
                "configured delegated runtime identity changed; the session was retired and must reopen"
                    .to_owned(),
            );
        }
        let configuration = match runtime_configuration_from_app(&info.configuration) {
            Ok(configuration) => configuration,
            Err(_) => {
                drop(operation);
                let _retired = self.invalidate(expected_runtime, generation).await;
                return Err(
                    "configured delegated runtime returned invalid state; the session was retired and must reopen"
                        .to_owned(),
                );
            }
        };
        Ok(heycode_routing::AppliedDelegatedConfiguration::new(
            configuration,
            generation,
        ))
    }

    async fn invalidate(
        &self,
        expected_runtime: &str,
        backend_generation: u64,
    ) -> Result<(), String> {
        let (model, effort) = self
            .routing
            .selection()
            .ok()
            .filter(|selection| selection.runtime() == expected_runtime)
            .map_or((None, None), |selection| {
                (
                    selection.runtime_model().map(str::to_owned),
                    selection.runtime_effort().map(str::to_owned),
                )
            });
        self.server()?
            .invalidate_backend_configuration(expected_runtime, backend_generation, model, effort)
            .await
            .map_err(|error| error.to_string())
    }

    async fn disconnect(&self, expected_runtime: &str) -> Result<(), String> {
        self.server()?
            .disconnect_backend(expected_runtime)
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcRequest {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

fn parse_request(value: &str) -> Result<RpcRequest, AppServerError> {
    if value.is_empty() || value.len() > 4 * 1024 * 1024 {
        return Err(AppServerError::invalid());
    }
    let request =
        serde_json::from_str::<RpcRequest>(value).map_err(|_| AppServerError::invalid())?;
    if request.jsonrpc != "2.0"
        || request.method.is_empty()
        || request.method.len() > 128
        || request.method.chars().any(char::is_control)
        || !matches!(request.id, Value::String(_) | Value::Number(_))
    {
        return Err(AppServerError::invalid());
    }
    Ok(request)
}

fn canonical_allowed_workspace(
    requested: &Path,
    allowed_root: &Path,
) -> Result<PathBuf, AppServerError> {
    let raw = requested.to_str().ok_or_else(AppServerError::invalid)?;
    if !requested.is_absolute()
        || raw
            .split(['/', '\\'])
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(AppServerError::invalid());
    }
    let allowed = std::fs::canonicalize(allowed_root).map_err(|_| AppServerError::unavailable())?;
    let candidate = std::fs::canonicalize(requested).map_err(|_| AppServerError::invalid())?;
    if !candidate.is_dir() || !candidate.starts_with(&allowed) {
        return Err(AppServerError::invalid());
    }
    Ok(candidate)
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

impl RpcResponse {
    fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, error: &AppServerError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code: rpc_code(error.code()),
                message: error.message(),
                data: error.detail().map(|detail| RpcErrorData {
                    detail: detail.to_owned(),
                }),
            }),
        }
    }
}

#[derive(Serialize)]
struct RpcError {
    code: i64,
    message: &'static str,
    /// The safe cause, when the error carries one; absent otherwise so the v1
    /// wire shape is unchanged for detail-free errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<RpcErrorData>,
}

#[derive(Serialize)]
struct RpcErrorData {
    detail: String,
}

fn rpc_code(code: AppServerErrorCode) -> i64 {
    match code {
        AppServerErrorCode::InvalidRequest => -32602,
        AppServerErrorCode::MethodNotFound => -32601,
        AppServerErrorCode::Conflict => -32001,
        AppServerErrorCode::Cancelled => -32800,
        AppServerErrorCode::Unavailable => -32002,
        AppServerErrorCode::Closed => -32004,
        AppServerErrorCode::Internal => -32603,
        // Matches `heycode_sdk::client`, which maps -32003 back to `Unsupported`.
        // Distinct from `MethodNotFound`: the selection is a name this build
        // knows, it is simply not installed here, and that half is actionable.
        AppServerErrorCode::Unsupported => -32003,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionOpenParams {
    #[serde(default)]
    configuration: AppRuntimeConfiguration,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionConfigureParams {
    session_id: String,
    configuration: AppRuntimeConfiguration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingTurnParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "messageId")]
    message_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnStartParams {
    #[serde(skip)]
    pending_message_id: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: String,
    text: String,
    #[serde(default)]
    attachments: Vec<heycode_core::AttachmentMetadata>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PermissionResponseParams {
    session_id: String,
    request_id: String,
    decision: AppPermissionDecision,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QuestionResponseParams {
    session_id: String,
    request_id: String,
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    selected_answers: Option<Vec<String>>,
    #[serde(default)]
    cancelled: bool,
}

struct NativeBackend {
    agent: Arc<heycode_agent::Agent>,
    approval: Option<Arc<heycode_agent::InteractiveApproval>>,
    questions: Arc<heycode_agent::InteractiveQuestion>,
    /// Per-backend random namespace keeps provider-native opaque request ids
    /// from colliding with bridged Agent approval ids.
    agent_permission_prefix: String,
    /// Agent asks that this backend actually exposed to its current client.
    pending_agent_permissions: std::sync::Mutex<HashSet<u64>>,
    /// Agent questions that this backend actually exposed to its client.
    pending_agent_questions: std::sync::Mutex<HashMap<u64, u64>>,
    /// Random namespace separating host-tool questions from runtime ids.
    agent_question_prefix: String,
    runtime: Arc<dyn AgentRuntime>,
    workspace: PathBuf,
    session: Mutex<Option<Arc<dyn RuntimeSession>>>,
    turn_events: Mutex<TurnEvents>,
    lifecycle: CancellationToken,
    delegated: bool,
    model: Option<String>,
    reasoning_effort: Option<String>,
    configuration: Mutex<Option<heycode_runtime::RuntimeConfiguration>>,
    configuration_gate: Mutex<()>,
}

impl NativeBackend {
    fn effective_workspace(&self) -> PathBuf {
        if self.delegated {
            self.workspace.clone()
        } else {
            self.agent.cwd()
        }
    }
    fn new(
        factory: &NativeBackendFactory,
        runtime: Arc<dyn AgentRuntime>,
        workspace: PathBuf,
        model: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Self {
        let delegated = runtime.descriptor().kind() == heycode_runtime::AgentRuntimeKind::Delegated;
        let namespace = heycode_core::CallId::generate();
        Self {
            agent: factory.agent.clone(),
            approval: factory.approval.clone(),
            questions: factory.questions.clone(),
            agent_permission_prefix: format!("{AGENT_PERMISSION_PREFIX}{}-", namespace.as_str()),
            pending_agent_permissions: std::sync::Mutex::new(HashSet::new()),
            pending_agent_questions: std::sync::Mutex::new(HashMap::new()),
            agent_question_prefix: format!("{AGENT_QUESTION_PREFIX}{}-", namespace.as_str()),
            runtime,
            workspace,
            session: Mutex::new(None),
            turn_events: Mutex::new(TurnEvents::default()),
            lifecycle: factory.lifecycle.clone(),
            delegated,
            model,
            reasoning_effort,
            configuration: Mutex::new(None),
            configuration_gate: Mutex::new(()),
        }
    }

    fn forget_agent_permission(&self, id: u64) -> Result<bool, AppServerError> {
        self.pending_agent_permissions
            .lock()
            .map(|mut pending| pending.remove(&id))
            .map_err(|_| AppServerError::unavailable())
    }

    fn clear_agent_permissions(&self) {
        if let Ok(mut pending) = self.pending_agent_permissions.lock() {
            pending.clear();
        }
    }

    fn forget_agent_question(&self, id: u64) -> Result<Option<u64>, AppServerError> {
        self.pending_agent_questions
            .lock()
            .map(|mut pending| pending.remove(&id))
            .map_err(|_| AppServerError::unavailable())
    }

    fn agent_question_owner(&self, id: u64) -> Result<Option<u64>, AppServerError> {
        self.pending_agent_questions
            .lock()
            .map(|pending| pending.get(&id).copied())
            .map_err(|_| AppServerError::unavailable())
    }

    fn clear_agent_questions(&self) {
        if let Ok(mut pending) = self.pending_agent_questions.lock() {
            pending.clear();
        }
    }

    async fn session(
        &self,
        requested: &AppRuntimeConfiguration,
        cancellation: &CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, AppServerError> {
        if self.lifecycle.is_cancelled() || cancellation.is_cancelled() {
            return Err(AppServerError::cancelled());
        }
        let mut slot = self.session.lock().await;
        if let Some(session) = slot.as_ref() {
            if requested != &AppRuntimeConfiguration::default() {
                let expected = self.runtime_configuration(requested)?;
                let current = self.configuration.lock().await;
                if current.as_ref() != Some(&expected) {
                    return Err(AppServerError::classified(AppServerErrorCode::Conflict));
                }
            }
            return Ok(session.clone());
        }
        let configuration = self.runtime_configuration(requested)?;
        let (session_id, fresh) = {
            let session = self
                .agent
                .session()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (session.id().clone(), session.is_fresh())
        };
        let runtime_id = self.runtime.descriptor().id().as_str().to_owned();
        let records_configuration = self.delegated || !configuration.is_empty();
        let session = if fresh {
            let mut start = RuntimeStart::new(session_id, self.effective_workspace())
                .map_err(|_| AppServerError::invalid())?;
            if self.delegated {
                let executor: Arc<dyn heycode_runtime::RuntimeToolExecutor> = self.agent.clone();
                self.record_runtime_configuration(
                    &configuration,
                    heycode_session::RuntimeConfigurationState::Attempted,
                )?;
                start = start
                    .with_configuration(configuration.clone())
                    .with_tool_executor(executor);
            } else if !configuration.is_empty() {
                self.record_runtime_configuration(
                    &configuration,
                    heycode_session::RuntimeConfigurationState::Attempted,
                )?;
                start = start.with_configuration(configuration.clone());
            }
            let session = self.runtime.start(start, cancellation.clone()).await;
            match session {
                Ok(session) => {
                    let linked = self
                        .agent
                        .session()
                        .lock()
                        .map_err(|_| AppServerError::unavailable())?
                        .append(heycode_session::SessionEventKind::RuntimeLinked {
                            runtime: runtime_id.clone(),
                            runtime_session_id: session.id().as_str().to_owned(),
                        });
                    if linked.is_err() {
                        if records_configuration {
                            let _recorded = self.record_runtime_configuration(
                                &configuration,
                                heycode_session::RuntimeConfigurationState::Failed,
                            );
                        }
                        let _settled = session.close(CancellationToken::new()).await;
                        return Err(AppServerError::unavailable());
                    }
                    if records_configuration
                        && self
                            .record_runtime_configuration(
                                &configuration,
                                heycode_session::RuntimeConfigurationState::Committed,
                            )
                            .is_err()
                    {
                        let _settled = session.close(CancellationToken::new()).await;
                        return Err(AppServerError::unavailable());
                    }
                    Ok(session)
                }
                Err(error) => {
                    if records_configuration {
                        self.record_runtime_configuration(
                            &configuration,
                            heycode_session::RuntimeConfigurationState::Failed,
                        )?;
                    }
                    Err(map_runtime_error(error))
                }
            }
        } else {
            let linked = self
                .agent
                .session()
                .lock()
                .map_err(|_| AppServerError::unavailable())?
                .runtime_link()
                .map(|(runtime, session)| (runtime.to_owned(), session.to_owned()));
            let runtime_session = match linked {
                Some((linked_runtime, linked_session)) if linked_runtime == runtime_id => {
                    linked_session
                }
                None if runtime_id == "native" => session_id.as_str().to_owned(),
                _ => return Err(AppServerError::unavailable()),
            };
            let runtime_id =
                RuntimeSessionId::new(runtime_session).map_err(|_| AppServerError::invalid())?;
            let mut resume = RuntimeResume::new(session_id, self.effective_workspace(), runtime_id)
                .map_err(|_| AppServerError::invalid())?;
            if self.delegated {
                let executor: Arc<dyn heycode_runtime::RuntimeToolExecutor> = self.agent.clone();
                self.record_runtime_configuration(
                    &configuration,
                    heycode_session::RuntimeConfigurationState::Attempted,
                )?;
                resume = resume
                    .with_configuration(configuration.clone())
                    .with_tool_executor(executor);
            } else if !configuration.is_empty() {
                self.record_runtime_configuration(
                    &configuration,
                    heycode_session::RuntimeConfigurationState::Attempted,
                )?;
                resume = resume.with_configuration(configuration.clone());
            }
            match self.runtime.resume(resume, cancellation.clone()).await {
                Ok(session) => {
                    if records_configuration
                        && self
                            .record_runtime_configuration(
                                &configuration,
                                heycode_session::RuntimeConfigurationState::Committed,
                            )
                            .is_err()
                    {
                        let _settled = session.close(CancellationToken::new()).await;
                        return Err(AppServerError::unavailable());
                    }
                    Ok(session)
                }
                Err(error) => {
                    if records_configuration {
                        self.record_runtime_configuration(
                            &configuration,
                            heycode_session::RuntimeConfigurationState::Failed,
                        )?;
                    }
                    Err(map_runtime_error(error))
                }
            }
        }?;
        *self.configuration.lock().await = Some(configuration);
        *slot = Some(session.clone());
        Ok(session)
    }

    fn runtime_configuration(
        &self,
        requested: &AppRuntimeConfiguration,
    ) -> Result<heycode_runtime::RuntimeConfiguration, AppServerError> {
        if self.delegated {
            let model = requested.model.as_deref().or(self.model.as_deref());
            let reasoning_effort = requested
                .reasoning_effort
                .as_deref()
                .or(self.reasoning_effort.as_deref());
            let available = self
                .agent
                .delegated_runtime_configuration(model, reasoning_effort)
                .map_err(|_| AppServerError::invalid())?;
            let support = self.runtime.descriptor().configuration_capabilities();
            return select_delegated_configuration(&available, requested, support);
        }
        let mut unsupported = Vec::new();
        if requested.system_prompt.is_some() {
            unsupported.push("system_prompt");
        }
        if requested.tools.is_some() {
            unsupported.push("tools");
        }
        if !unsupported.is_empty() {
            return Err(AppServerError::classified(AppServerErrorCode::Unsupported)
                .with_detail(format!(
                    "runtime configuration fields are unsupported: {}",
                    unsupported.join(", ")
                ))
                .unwrap_or_else(|error| error));
        }
        let mut configuration = heycode_runtime::RuntimeConfiguration::new();
        if let Some(model) = requested.model.as_deref().or(self.model.as_deref()) {
            configuration = configuration
                .with_model(model)
                .map_err(|_| AppServerError::invalid())?;
        }
        if let Some(effort) = requested
            .reasoning_effort
            .as_deref()
            .or(self.reasoning_effort.as_deref())
        {
            configuration = configuration
                .with_reasoning_effort(effort)
                .map_err(|_| AppServerError::invalid())?;
        }
        Ok(configuration)
    }

    fn runtime_update(
        &self,
        requested: &AppRuntimeConfiguration,
    ) -> Result<heycode_runtime::RuntimeConfiguration, AppServerError> {
        let mut update = heycode_runtime::RuntimeConfiguration::new();
        if let Some(prompt) = &requested.system_prompt {
            update = update
                .with_system_prompt(prompt.clone())
                .map_err(|_| AppServerError::invalid())?;
        }
        if let Some(tools) = &requested.tools {
            let known = self
                .agent
                .delegated_runtime_configuration(None, None)
                .map_err(|_| AppServerError::invalid())?;
            if tools
                .iter()
                .any(|tool| !known.tools().iter().any(|candidate| candidate == tool))
            {
                return Err(AppServerError::invalid());
            }
            update = update
                .with_tools(tools.clone())
                .map_err(|_| AppServerError::invalid())?;
        }
        if let Some(model) = &requested.model {
            update = update
                .with_model(model.clone())
                .map_err(|_| AppServerError::invalid())?;
        }
        if let Some(effort) = &requested.reasoning_effort {
            update = update
                .with_reasoning_effort(effort.clone())
                .map_err(|_| AppServerError::invalid())?;
        }
        Ok(update)
    }

    fn session_info(
        &self,
        session: &Arc<dyn RuntimeSession>,
        configuration: &heycode_runtime::RuntimeConfiguration,
    ) -> AppSessionInfo {
        let model_configuration = configuration
            .model()
            .and_then(|model| session.model_configuration(model))
            .map(app_runtime_model_configuration);
        AppSessionInfo {
            session_id: session.id().as_str().to_owned(),
            runtime_id: session.runtime_id().as_str().to_owned(),
            cwd: self.effective_workspace(),
            configuration: app_runtime_configuration(configuration),
            configuration_capabilities: app_configuration_capabilities(
                self.runtime.descriptor().configuration_capabilities(),
            ),
            model_configuration,
        }
    }

    fn record_runtime_configuration(
        &self,
        configuration: &heycode_runtime::RuntimeConfiguration,
        state: heycode_session::RuntimeConfigurationState,
    ) -> Result<(), AppServerError> {
        self.agent
            .session()
            .lock()
            .map_err(|_| AppServerError::unavailable())?
            .append(heycode_session::SessionEventKind::RuntimeConfigured {
                state,
                system_prompt: configuration.system_prompt().map(str::to_owned),
                tools: configuration
                    .tools_configured()
                    .then(|| configuration.tools().to_vec()),
                model: configuration.model().map(str::to_owned),
                reasoning_effort: configuration.reasoning_effort().map(str::to_owned),
            })
            .map(|_| ())
            .map_err(|_| AppServerError::unavailable())
    }
}

#[async_trait]
impl AppBackend for NativeBackend {
    async fn open(
        &self,
        configuration: AppRuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<AppSessionInfo, AppServerError> {
        let session = self.session(&configuration, &cancellation).await?;
        let effective = self.configuration.lock().await.clone().unwrap_or_default();
        Ok(self.session_info(&session, &effective))
    }

    async fn configure(
        &self,
        configuration: AppRuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<AppSessionInfo, AppServerError> {
        let _configuration_gate = self.configuration_gate.lock().await;
        let session = self
            .session(&AppRuntimeConfiguration::default(), &cancellation)
            .await?;
        let update = self.runtime_update(&configuration)?;
        let current = self.configuration.lock().await.clone().unwrap_or_default();
        if update.is_empty() {
            return Ok(self.session_info(&session, &current));
        }
        let intended = current.merged_with(&update);
        // The durable audit boundary precedes the child control write. This is
        // also the recovery evidence if the process applies a request and dies
        // before its acknowledgement reaches us.
        self.record_runtime_configuration(
            &intended,
            heycode_session::RuntimeConfigurationState::Attempted,
        )?;
        let effective = match session.configure(update, cancellation).await {
            Ok(effective) => effective,
            Err(error) => {
                self.record_runtime_configuration(
                    &intended,
                    heycode_session::RuntimeConfigurationState::Failed,
                )?;
                return Err(map_runtime_error(error));
            }
        };
        if effective != intended {
            self.record_runtime_configuration(
                &intended,
                heycode_session::RuntimeConfigurationState::Failed,
            )?;
            let _settled = session.close(CancellationToken::new()).await;
            return Err(AppServerError::unavailable());
        }
        if self
            .record_runtime_configuration(
                &effective,
                heycode_session::RuntimeConfigurationState::Committed,
            )
            .is_err()
        {
            let mut slot = self.session.lock().await;
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                slot.take();
            }
            drop(slot);
            let _settled = session.close(CancellationToken::new()).await;
            return Err(AppServerError::unavailable());
        }
        *self.configuration.lock().await = Some(effective.clone());
        Ok(self.session_info(&session, &effective))
    }

    async fn turn(
        &self,
        text: String,
        attachments: Vec<heycode_core::AttachmentMetadata>,
        pending_message_id: Option<String>,
        sink: AppEventSink,
        cancellation: CancellationToken,
    ) -> Result<AppTurnResult, AppServerError> {
        let session = self
            .session(&AppRuntimeConfiguration::default(), &cancellation)
            .await?;
        if self.delegated && pending_message_id.is_some() {
            return Err(AppServerError::classified(AppServerErrorCode::Unsupported));
        }
        if self.delegated && !attachments.is_empty() {
            return Err(AppServerError::classified(AppServerErrorCode::Unavailable));
        }
        let local_turn = if self.delegated {
            let mut durable = self
                .agent
                .session()
                .lock()
                .map_err(|_| AppServerError::unavailable())?;
            durable
                .append(heycode_session::SessionEventKind::UserMessage { text: text.clone() })
                .map_err(|_| AppServerError::unavailable())?;
            Some(next_local_turn(durable.events()))
        } else {
            None
        };
        let input = if pending_message_id.is_some() {
            None
        } else {
            Some(
                RuntimeInput::with_attachments(text, attachments)
                    .map_err(|_| AppServerError::invalid())?,
            )
        };
        let session_event_baseline = self
            .agent
            .session()
            .lock()
            .map_err(|_| AppServerError::unavailable())?
            .events()
            .len();
        let mut pump = NativePump {
            local_turn,
            session_event_baseline,
            ..NativePump::default()
        };
        // One turn at a time owns the session's stream: a second concurrent
        // turn is a caller mistake and is refused loudly instead of silently
        // splitting the session's events across two pumps.
        let mut turn_events = self
            .turn_events
            .try_lock()
            .map_err(|_| AppServerError::classified(AppServerErrorCode::Conflict))?;
        if !self.delegated {
            let durable = self
                .agent
                .session()
                .lock()
                .map_err(|_| AppServerError::unavailable())?;
            turn_events.skip_native_history(durable.events());
        }
        let settled = {
            let mut turn_pump = match (input, pending_message_id) {
                (Some(input), None) => TurnPump::new(
                    session.as_ref(),
                    turn_events.stream(session.as_ref()),
                    input,
                    cancellation.clone(),
                ),
                (None, Some(id)) => TurnPump::pending(
                    session.as_ref(),
                    turn_events.stream(session.as_ref()),
                    id,
                    cancellation.clone(),
                ),
                _ => return Err(AppServerError::invalid()),
            };
            // InteractiveApproval mirrors asks to the UI/runtime bus and this
            // typed channel. The host owns namespaced typed waiters for both
            // native and delegated host tools; the native runtime's bus copy
            // is suppressed below because it cannot answer that waiter.
            // Subscribe only while this backend owns a turn. The default
            // composition also exposes ACP; a process-lifetime AppServer
            // subscription would retain every ACP-only notification forever.
            let mut approval_requests = self
                .approval
                .as_ref()
                .and_then(|approval| approval.take_subscription());
            let mut question_requests = self.questions.take_subscription();
            let question_owner = question_requests
                .as_ref()
                .map(heycode_agent::QuestionSubscription::owner_id);
            let projected: Result<(), AppServerError> = async {
                enum Projection {
                    Runtime(Option<RuntimeEvent>),
                    Approval(heycode_agent::AskNotification),
                    Question(heycode_agent::QuestionNotification),
                }
                loop {
                    let next = match (approval_requests.as_mut(), question_requests.as_mut()) {
                        (Some(approvals), Some(questions)) => tokio::select! {
                            event = turn_pump.next() => Projection::Runtime(event?),
                            request = approvals.recv() => Projection::Approval(
                                request.ok_or_else(AppServerError::unavailable)?
                            ),
                            request = questions.recv() => Projection::Question(
                                request.ok_or_else(AppServerError::unavailable)?
                            ),
                        },
                        (Some(approvals), None) => tokio::select! {
                            event = turn_pump.next() => Projection::Runtime(event?),
                            request = approvals.recv() => Projection::Approval(
                                request.ok_or_else(AppServerError::unavailable)?
                            ),
                        },
                        (None, Some(questions)) => tokio::select! {
                            event = turn_pump.next() => Projection::Runtime(event?),
                            request = questions.recv() => Projection::Question(
                                request.ok_or_else(AppServerError::unavailable)?
                            ),
                        },
                        (None, None) => Projection::Runtime(turn_pump.next().await?),
                    };
                    match next {
                        Projection::Runtime(Some(event)) => {
                            if !self.delegated && matches!(event.kind(), RuntimeEventKind::PermissionRequested { request_id, .. } if request_id.as_str().starts_with("approval-")) {
                                continue;
                            }
                            self.process_event(event, &sink, &mut pump).await?;
                        }
                        Projection::Runtime(None) => return Ok(()),
                        Projection::Approval(request) => {
                            let still_pending = self
                                .approval
                                .as_ref()
                                .is_some_and(|approval| approval.is_pending(request.id));
                            if !still_pending {
                                continue;
                            }
                            let detail = request.args_preview.trim();
                            self.pending_agent_permissions
                                .lock()
                                .map_err(|_| AppServerError::unavailable())?
                                .insert(request.id);
                            let emitted = sink
                                .emit(AppServerEvent::PermissionRequested {
                                    request_id: format!(
                                        "{}{}",
                                        self.agent_permission_prefix, request.id
                                    ),
                                    action: request.name,
                                    detail: if detail.is_empty() {
                                        "(no arguments)".to_owned()
                                    } else {
                                        detail.to_owned()
                                    },
                                })
                                .await;
                            if let Err(error) = emitted {
                                let _forgotten = self.forget_agent_permission(request.id);
                                return Err(error);
                            }
                        }
                        Projection::Question(request) => {
                            if !self.questions.is_pending(request.id) {
                                continue;
                            }
                            self.pending_agent_questions
                                .lock()
                                .map_err(|_| AppServerError::unavailable())?
                                .insert(
                                    request.id,
                                    question_owner.ok_or_else(AppServerError::unavailable)?,
                                );
                            let choices = request
                                .choices
                                .iter()
                                .map(|choice| choice.label.clone())
                                .collect();
                            let choice_descriptions = request
                                .choices
                                .iter()
                                .map(|choice| choice.description.clone())
                                .collect();
                            let emitted = sink
                                .emit(AppServerEvent::QuestionRequested {
                                    owner_session_id:request.owner_session_id,
                                    mode: request.mode,
                                    progress: request.progress,
                                    request_id: format!(
                                        "{}{}",
                                        self.agent_question_prefix, request.id
                                    ),
                                    header: request.header,
                                    prompt: request.prompt,
                                    choices,
                                    choice_descriptions,
                                })
                                .await;
                            if let Err(error) = emitted {
                                let _forgotten = self.forget_agent_question(request.id);
                                return Err(error);
                            }
                        }
                    }
                }
            }
            .await;
            match projected {
                Ok(()) => turn_pump.settle(),
                Err(error) => {
                    // The turn is over for this client; stop the runtime's work
                    // and forget the subscription so the next turn starts clean.
                    drop(turn_pump);
                    let _settled = session.cancel(CancellationToken::new()).await;
                    self.clear_agent_permissions();
                    self.clear_agent_questions();
                    turn_events.reset();
                    return Err(error);
                }
            }
        };
        self.clear_agent_permissions();
        self.clear_agent_questions();
        let settled = settled?;
        drop(turn_events);
        let send_result = settled.send;
        let reason = settled.terminal.unwrap_or(RuntimeFinishReason::Error);
        if let Some(local_turn) = pump.local_turn
            && !pump.local_turn_started
            && send_result.is_err()
        {
            self.append_delegated(heycode_session::SessionEventKind::TurnStart {
                turn: local_turn,
            })?;
            self.append_delegated(heycode_session::SessionEventKind::TurnEnd {
                turn: local_turn,
                reason: if reason == RuntimeFinishReason::Cancelled {
                    heycode_session::TurnEndReason::Aborted
                } else {
                    heycode_session::TurnEndReason::Error
                },
            })?;
        }
        if let Err(error) = &send_result
            && error.code() != RuntimeErrorCode::Cancelled
            && reason != RuntimeFinishReason::Cancelled
        {
            return Err(map_runtime_error(error.clone()));
        }
        Ok(AppTurnResult {
            turn_id: send_result.ok().map(|turn| turn.as_str().to_owned()),
            reason: map_finish(reason),
        })
    }

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), AppServerError> {
        let session = self
            .session(&AppRuntimeConfiguration::default(), &cancellation)
            .await?;
        let result = session
            .cancel(cancellation)
            .await
            .map_err(map_runtime_error);
        self.clear_agent_permissions();
        self.clear_agent_questions();
        result
    }

    async fn respond_permission(
        &self,
        request_id: String,
        decision: AppPermissionDecision,
        cancellation: CancellationToken,
    ) -> Result<(), AppServerError> {
        if let Some(id) = request_id.strip_prefix(&self.agent_permission_prefix) {
            if cancellation.is_cancelled() {
                return Err(AppServerError::cancelled());
            }
            let id = id.parse::<u64>().map_err(|_| AppServerError::invalid())?;
            let approval = self.approval.as_ref().ok_or_else(AppServerError::invalid)?;
            if !approval.is_pending(id) || !self.forget_agent_permission(id)? {
                return Err(AppServerError::classified(AppServerErrorCode::Conflict));
            }
            let answer = match decision {
                AppPermissionDecision::AllowOnce => heycode_agent::AskAnswer::Allow,
                AppPermissionDecision::AllowSession => heycode_agent::AskAnswer::AllowSession,
                AppPermissionDecision::Deny => heycode_agent::AskAnswer::Deny,
            };
            approval.answer_with(id, answer);
            return Ok(());
        }
        let session = self
            .session(&AppRuntimeConfiguration::default(), &cancellation)
            .await?;
        let request_id = heycode_runtime::RuntimeRequestId::new(request_id)
            .map_err(|_| AppServerError::invalid())?;
        let decision = match decision {
            AppPermissionDecision::AllowOnce => {
                heycode_runtime::RuntimePermissionDecision::AllowOnce
            }
            AppPermissionDecision::AllowSession => {
                heycode_runtime::RuntimePermissionDecision::AllowSession
            }
            AppPermissionDecision::Deny => heycode_runtime::RuntimePermissionDecision::Deny,
        };
        session
            .respond_permission(
                heycode_runtime::RuntimePermissionResponse::new(request_id, decision),
                cancellation,
            )
            .await
            .map_err(map_runtime_error)
    }

    async fn respond_question_selected(
        &self,
        request_id: String,
        answers: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<(), AppServerError> {
        if cancellation.is_cancelled() {
            return Err(AppServerError::cancelled());
        }
        if let Some(id) = request_id.strip_prefix(&self.agent_question_prefix) {
            let id = id.parse::<u64>().map_err(|_| AppServerError::invalid())?;
            let owner = self
                .agent_question_owner(id)?
                .ok_or_else(|| AppServerError::classified(AppServerErrorCode::Conflict))?;
            if !self.questions.answer_owned(
                owner,
                id,
                heycode_agent::QuestionAnswer::Selected(answers),
            ) {
                return Err(AppServerError::classified(AppServerErrorCode::Conflict));
            }
            self.forget_agent_question(id)?;
            return Ok(());
        }
        let session = self
            .session(&AppRuntimeConfiguration::default(), &cancellation)
            .await?;
        let request_id = heycode_runtime::RuntimeRequestId::new(request_id)
            .map_err(|_| AppServerError::invalid())?;
        let response = heycode_runtime::RuntimeQuestionResponse::selected(request_id, answers)
            .map_err(|_| AppServerError::invalid())?;
        session
            .respond_question(response, cancellation)
            .await
            .map_err(map_runtime_error)
    }

    async fn respond_question(
        &self,
        request_id: String,
        answer: Option<String>,
        cancellation: CancellationToken,
    ) -> Result<(), AppServerError> {
        if let Some(id) = request_id.strip_prefix(&self.agent_question_prefix) {
            if cancellation.is_cancelled() {
                return Err(AppServerError::cancelled());
            }
            let id = id.parse::<u64>().map_err(|_| AppServerError::invalid())?;
            let Some(owner) = self.agent_question_owner(id)? else {
                return Err(AppServerError::classified(AppServerErrorCode::Conflict));
            };
            if !self.questions.is_pending(id) {
                return Err(AppServerError::classified(AppServerErrorCode::Conflict));
            }
            let answer = answer.map_or(
                heycode_agent::QuestionAnswer::Cancelled,
                heycode_agent::QuestionAnswer::Answer,
            );
            if !self.questions.answer_owned(owner, id, answer) {
                return Err(AppServerError::classified(AppServerErrorCode::Conflict));
            }
            let _settled = self.forget_agent_question(id)?;
            return Ok(());
        }
        let session = self
            .session(&AppRuntimeConfiguration::default(), &cancellation)
            .await?;
        let request_id = heycode_runtime::RuntimeRequestId::new(request_id)
            .map_err(|_| AppServerError::invalid())?;
        let response = match answer {
            Some(answer) => heycode_runtime::RuntimeQuestionResponse::new(request_id, answer)
                .map_err(|_| AppServerError::invalid())?,
            None => heycode_runtime::RuntimeQuestionResponse::cancelled(request_id),
        };
        session
            .respond_question(response, cancellation)
            .await
            .map_err(map_runtime_error)
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), AppServerError> {
        let session = self
            .session(&AppRuntimeConfiguration::default(), &cancellation)
            .await?;
        let result = session.close(cancellation).await.map_err(map_runtime_error);
        self.clear_agent_permissions();
        self.clear_agent_questions();
        result
    }

    async fn retire(&self) -> Result<(), AppServerError> {
        let session = self.session.lock().await.take();
        if let Some(session) = session {
            let _cancelled = session.cancel(CancellationToken::new()).await;
            let _closed = session.close(CancellationToken::new()).await;
        }
        self.turn_events.lock().await.reset();
        *self.configuration.lock().await = None;
        self.clear_agent_permissions();
        self.clear_agent_questions();
        Ok(())
    }
}

#[derive(Default)]
struct NativePump {
    tool_names: HashMap<String, String>,
    pending_untrusted_web: bool,
    latest_usage: Option<heycode_core::TokenUsage>,
    saw_assistant_delta: bool,
    local_turn: Option<u64>,
    local_turn_started: bool,
    session_event_baseline: usize,
}

/// How one turn settled: the runtime's answer to `send` and the finish reason
/// its event stream carried.
struct TurnSettlement {
    send: Result<heycode_runtime::RuntimeTurnId, heycode_runtime::RuntimeError>,
    terminal: Option<RuntimeFinishReason>,
}

/// The one runtime subscription every turn of a session shares.
///
/// A subscription is contiguous from sequence zero for its whole life, and a
/// hub that has evicted past `RUNTIME_EVENT_HISTORY` renumbers each new
/// subscription from zero as well. A sequence baseline carried from an earlier
/// subscription is therefore meaningless: applied to a fresh one it would sit
/// above the whole replayed window and skip every live event of a long
/// session's later turns, stalling the turn until its event timeout. The
/// session keeps one stream instead. Native direct commands can also publish
/// into that stream while this backend is idle; their durable turn identities
/// form a catch-up boundary before the next AppServer send.
///
/// When a turn's subscription breaks, `reset` forgets it; the next turn
/// resubscribes and the hub replays the retained window, so the pump skips
/// every replayed event up to the first `TurnStarted` it has not seen before —
/// one broken turn never poisons the session and never duplicates history.
#[derive(Default)]
struct TurnEvents {
    stream: Option<heycode_runtime::NormalizedRuntimeEventStream>,
    /// Every turn id a pump on this session has already projected.
    seen_turns: std::collections::BTreeSet<heycode_runtime::RuntimeTurnId>,
    /// Set by `reset`: the next stream begins with replay that must be skipped.
    catching_up: bool,
}

impl TurnEvents {
    /// Native slash commands can run directly through Agent between AppServer
    /// turns. Their committed events are still queued in this shared runtime
    /// subscription. They belong to the existing journal/UI owner, not to the
    /// next send. Use durable turn identities, never subscription sequence
    /// numbers, so trimmed/replayed hubs retain the same boundary.
    fn skip_native_history(&mut self, events: &[heycode_session::SessionEvent]) {
        self.seen_turns
            .extend(events.iter().filter_map(|event| match event.kind {
                heycode_session::SessionEventKind::TurnStart { turn } => {
                    Some(heycode_runtime::RuntimeTurnId::from_native_turn(turn))
                }
                _ => None,
            }));
        self.catching_up = true;
    }

    /// The session's stream, subscribed on the first turn that needs it.
    fn stream(&mut self, session: &dyn RuntimeSession) -> TurnStream<'_> {
        let stream = self.stream.get_or_insert_with(|| {
            heycode_runtime::normalize_runtime_event_stream(session.subscribe())
        });
        TurnStream {
            stream,
            seen_turns: &mut self.seen_turns,
            catching_up: &mut self.catching_up,
        }
    }

    /// Forget a broken subscription so the next turn opens a fresh one.
    fn reset(&mut self) {
        self.stream = None;
        self.catching_up = true;
    }
}

/// One turn's borrowed view of the session stream plus its catch-up state.
struct TurnStream<'a> {
    stream: &'a mut heycode_runtime::NormalizedRuntimeEventStream,
    seen_turns: &'a mut std::collections::BTreeSet<heycode_runtime::RuntimeTurnId>,
    catching_up: &'a mut bool,
}

/// One turn's settlement over the session's event stream.
///
/// The pump owns the `send` future and the turn's finish reason so the caller
/// only ever projects events: `next` yields this turn's events in order and
/// answers `None` once `send` has settled and the stream has carried the
/// turn's `TurnFinished`. A `send` cancelled before the stream said so settles
/// the turn as cancelled rather than waiting for an event that will not come.
struct TurnPump<'a> {
    events: TurnStream<'a>,
    send: futures::future::BoxFuture<
        'a,
        Result<heycode_runtime::RuntimeTurnId, heycode_runtime::RuntimeError>,
    >,
    send_result: Option<Result<heycode_runtime::RuntimeTurnId, heycode_runtime::RuntimeError>>,
    terminal: Option<RuntimeFinishReason>,
    cancellation: CancellationToken,
    session: &'a dyn RuntimeSession,
    cancellation_settled: bool,
}

impl<'a> TurnPump<'a> {
    fn new(
        session: &'a dyn RuntimeSession,
        events: TurnStream<'a>,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Self {
        let send = session.send(input, cancellation.clone());
        Self {
            events,
            send,
            send_result: None,
            terminal: None,
            cancellation,
            session,
            cancellation_settled: false,
        }
    }

    fn pending(
        session: &'a dyn RuntimeSession,
        events: TurnStream<'a>,
        message_id: String,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            events,
            send: session.send_pending(message_id, cancellation.clone()),
            send_result: None,
            terminal: None,
            cancellation,
            session,
            cancellation_settled: false,
        }
    }

    /// The next event this turn projects, or `None` once the turn settled.
    ///
    /// # Errors
    /// A failed or ended stream is unavailable. A successful `send` only
    /// acknowledges acceptance: model work, tools and human decisions can be
    /// silent until `TurnFinished`. The caller still owns cancellation. A
    /// failed send gets only a bounded drain for any final queued events.
    async fn next(&mut self) -> Result<Option<RuntimeEvent>, AppServerError> {
        loop {
            if self.send_result.is_some() && self.terminal.is_some() {
                return Ok(None);
            }
            if let Some(result) = self.send_result.as_ref()
                && result
                    .as_ref()
                    .is_err_and(|error| error.code() == RuntimeErrorCode::Cancelled)
                && self.terminal.is_none()
            {
                self.terminal = Some(RuntimeFinishReason::Cancelled);
                return Ok(None);
            }
            let next = if self.send_result.is_some() {
                if self.cancellation_settled
                    || self.send_result.as_ref().is_some_and(Result::is_err)
                {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        self.events.stream.next(),
                    )
                    .await
                    .map_err(|_| AppServerError::unavailable())?
                } else {
                    tokio::select! {
                        biased;
                        () = self.cancellation.cancelled() => {
                            self.session.cancel(CancellationToken::new()).await.map_err(map_runtime_error)?;
                            self.cancellation_settled = true;
                            continue;
                        },
                        event = self.events.stream.next() => event,
                    }
                }
            } else {
                let send = &mut self.send;
                let events = &mut self.events.stream;
                tokio::select! {
                    result = send => {
                        self.send_result = Some(result);
                        continue;
                    }
                    event = events.next() => event,
                }
            };
            let event = next
                .ok_or_else(AppServerError::unavailable)?
                .map_err(map_runtime_error)?
                .into_event();
            if let RuntimeEventKind::TurnStarted { turn } = event.kind() {
                let unseen = self.events.seen_turns.insert(turn.clone());
                if *self.events.catching_up {
                    if !unseen {
                        continue;
                    }
                    *self.events.catching_up = false;
                }
            } else if *self.events.catching_up {
                // Replayed history from before the reset: already projected.
                continue;
            }
            if let RuntimeEventKind::TurnFinished { reason, .. } = event.kind() {
                self.terminal = Some(*reason);
            }
            return Ok(Some(event));
        }
    }

    /// How the settled turn ended.
    ///
    /// # Errors
    /// Unavailable when the pump stopped before `send` ever answered.
    fn settle(self) -> Result<TurnSettlement, AppServerError> {
        Ok(TurnSettlement {
            send: self.send_result.ok_or_else(AppServerError::unavailable)?,
            terminal: self.terminal,
        })
    }
}

impl NativeBackend {
    async fn process_event(
        &self,
        event: RuntimeEvent,
        sink: &AppEventSink,
        pump: &mut NativePump,
    ) -> Result<(), AppServerError> {
        match event.kind() {
            RuntimeEventKind::SessionReady => {}
            RuntimeEventKind::TurnStarted { turn } => {
                if let Some(local_turn) = pump.local_turn {
                    self.append_delegated(heycode_session::SessionEventKind::TurnStart {
                        turn: local_turn,
                    })?;
                    pump.local_turn_started = true;
                }
                if let Some((text, attachments, document_routes)) =
                    latest_committed_input(&self.agent)
                {
                    sink.emit(AppServerEvent::UserInput {
                        text,
                        attachments,
                        document_routes,
                    })
                    .await?;
                }
                sink.emit(AppServerEvent::TurnStarted {
                    turn_id: turn.as_str().to_owned(),
                })
                .await?;
            }
            RuntimeEventKind::CommentaryDelta { text } => {
                if let Some(turn) = pump.local_turn {
                    self.append_delegated(heycode_session::SessionEventKind::AssistantChunk {
                        turn,
                        step: 0,
                        text: Some(text.clone()),
                        reasoning: None,
                    })?;
                }
                pump.saw_assistant_delta = true;
                sink.emit(AppServerEvent::AssistantDelta { text: text.clone() })
                    .await?;
            }
            RuntimeEventKind::ReasoningDelta { text } => {
                if let Some(turn) = pump.local_turn {
                    self.append_delegated(heycode_session::SessionEventKind::AssistantChunk {
                        turn,
                        step: 0,
                        text: None,
                        reasoning: Some(text.clone()),
                    })?;
                }
                sink.emit(AppServerEvent::ReasoningDelta { text: text.clone() })
                    .await?;
            }
            RuntimeEventKind::FinalMessage { text } if !pump.saw_assistant_delta => {
                self.append_delegated_final(pump, text)?;
                sink.emit(AppServerEvent::AssistantDelta { text: text.clone() })
                    .await?;
            }
            RuntimeEventKind::FinalMessage { text } => {
                self.append_delegated_final(pump, text)?;
            }
            RuntimeEventKind::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                if let Some(turn) = pump.local_turn {
                    self.append_delegated(heycode_session::SessionEventKind::ToolCall {
                        turn,
                        call_id: call_id.clone(),
                        name: name.clone(),
                        args: arguments.clone(),
                    })?;
                }
                pump.tool_names
                    .insert(call_id.as_str().to_owned(), name.clone());
                sink.emit(AppServerEvent::ToolStarted {
                    call_id: call_id.as_str().to_owned(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                })
                .await?;
            }
            RuntimeEventKind::ToolResult {
                call_id,
                result,
                is_error,
            } => {
                let name = pump
                    .tool_names
                    .remove(call_id.as_str())
                    .unwrap_or_else(|| "tool".to_owned());
                let untrusted_content = if pump.pending_untrusted_web {
                    Some(heycode_core::UntrustedContentBoundary::web())
                } else {
                    self.committed_tool_boundary(call_id)?
                };
                pump.pending_untrusted_web = false;
                if pump.local_turn.is_some() {
                    self.append_delegated(heycode_session::SessionEventKind::ToolResult {
                        call_id: call_id.clone(),
                        content: runtime_tool_result_content(result)?,
                        is_error: *is_error,
                        untrusted_content,
                    })?;
                }
                sink.emit(AppServerEvent::ToolFinished {
                    call_id: call_id.as_str().to_owned(),
                    name,
                    result: result.clone(),
                    ok: !*is_error,
                    untrusted_content,
                })
                .await?;
            }
            RuntimeEventKind::ContextBudgetChanged { budget } => {
                sink.emit(AppServerEvent::ContextBudgetChanged {
                    budget: Box::new(budget.clone()),
                })
                .await?;
            }
            RuntimeEventKind::Usage { usage, context } => {
                pump.latest_usage = Some(*usage);
                sink.emit(AppServerEvent::Usage {
                    usage: *usage,
                    context: context.as_ref().map(|context| AppRuntimeContextUsage {
                        resolved_model: context.resolved_model.clone(),
                        tokens: context.tokens,
                        context_window: context.context_window,
                    }),
                })
                .await?;
            }
            RuntimeEventKind::TurnFinished { turn, reason } => {
                if let Some(local_turn) = pump.local_turn {
                    self.append_delegated(heycode_session::SessionEventKind::TurnEnd {
                        turn: local_turn,
                        reason: match reason {
                            RuntimeFinishReason::Stop => heycode_session::TurnEndReason::Stop,
                            RuntimeFinishReason::Limit => heycode_session::TurnEndReason::MaxTokens,
                            RuntimeFinishReason::Cancelled => {
                                heycode_session::TurnEndReason::Aborted
                            }
                            RuntimeFinishReason::Error => heycode_session::TurnEndReason::Error,
                        },
                    })?;
                }
                for attachments in assistant_audio_since(&self.agent, pump.session_event_baseline)?
                {
                    sink.emit(AppServerEvent::AssistantAudio { attachments })
                        .await?;
                }
                pump.session_event_baseline = self
                    .agent
                    .session()
                    .lock()
                    .map_err(|_| AppServerError::unavailable())?
                    .events()
                    .len();
                sink.emit(AppServerEvent::TurnFinished {
                    turn_id: turn.as_str().to_owned(),
                    reason: map_finish(*reason),
                    usage: pump.latest_usage,
                })
                .await?;
            }
            RuntimeEventKind::Notice { code, .. } if code == "content.untrusted.web" => {
                pump.pending_untrusted_web = true;
            }
            RuntimeEventKind::Notice { code, .. } if code == "native.plan.enabled" => {
                sink.emit(AppServerEvent::PlanChanged { active: true })
                    .await?;
            }
            RuntimeEventKind::Notice { code, .. } if code == "native.plan.disabled" => {
                sink.emit(AppServerEvent::PlanChanged { active: false })
                    .await?;
            }
            RuntimeEventKind::PermissionRequested {
                request_id,
                action,
                detail,
            } => {
                sink.emit(AppServerEvent::PermissionRequested {
                    request_id: request_id.as_str().to_owned(),
                    action: action.clone(),
                    detail: detail.clone(),
                })
                .await?;
            }
            RuntimeEventKind::QuestionRequested {
                request_id,
                header,
                prompt,
                choices,
                choice_descriptions,
                mode,
                progress,
            } => {
                let owner_session_id = self
                    .agent
                    .session()
                    .lock()
                    .map_err(|_| AppServerError::unavailable())?
                    .id()
                    .to_string();
                sink.emit(AppServerEvent::QuestionRequested {
                    owner_session_id: Some(owner_session_id),
                    mode: *mode,
                    progress: *progress,
                    request_id: request_id.as_str().to_owned(),
                    header: header.clone(),
                    prompt: prompt.clone(),
                    choices: choices.clone(),
                    choice_descriptions: choice_descriptions.clone(),
                })
                .await?;
            }
            RuntimeEventKind::Notice { code, message } => {
                sink.emit(AppServerEvent::Notice {
                    code: code.clone(),
                    message: message.clone(),
                })
                .await?;
            }
        }
        Ok(())
    }
}

fn assistant_audio_since(
    agent: &heycode_agent::Agent,
    baseline: usize,
) -> Result<Vec<Vec<heycode_core::AttachmentMetadata>>, AppServerError> {
    let session = agent
        .session()
        .lock()
        .map_err(|_| AppServerError::unavailable())?;
    Ok(assistant_audio_after(session.events(), baseline))
}

fn assistant_audio_after(
    events: &[heycode_session::SessionEvent],
    baseline: usize,
) -> Vec<Vec<heycode_core::AttachmentMetadata>> {
    events
        .get(baseline..)
        .unwrap_or_default()
        .iter()
        .filter_map(|event| match &event.kind {
            heycode_session::SessionEventKind::AssistantAudio { attachments, .. } => {
                Some(attachments.clone())
            }
            _ => None,
        })
        .collect()
}

impl NativeBackend {
    fn append_delegated(
        &self,
        kind: heycode_session::SessionEventKind,
    ) -> Result<(), AppServerError> {
        if !self.delegated {
            return Ok(());
        }
        let mut session = self
            .agent
            .session()
            .lock()
            .map_err(|_| AppServerError::unavailable())?;
        match delegated_event_commit_state(session.events(), &kind) {
            DelegatedCommitState::Exact => return Ok(()),
            DelegatedCommitState::Conflict => return Err(AppServerError::unavailable()),
            DelegatedCommitState::Missing => {}
        }
        session
            .append(kind)
            .map(|_| ())
            .map_err(|_| AppServerError::unavailable())
    }

    fn committed_tool_boundary(
        &self,
        call_id: &heycode_core::CallId,
    ) -> Result<Option<heycode_core::UntrustedContentBoundary>, AppServerError> {
        let session = self
            .agent
            .session()
            .lock()
            .map_err(|_| AppServerError::unavailable())?;
        Ok(committed_tool_boundary(session.events(), call_id))
    }

    fn append_delegated_final(&self, pump: &NativePump, text: &str) -> Result<(), AppServerError> {
        if let Some(turn) = pump.local_turn {
            self.append_delegated(heycode_session::SessionEventKind::AssistantMessage {
                turn,
                step: 0,
                content: text.to_owned(),
                reasoning: None,
                tool_calls: None,
                usage: pump.latest_usage,
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegatedCommitState {
    Missing,
    Exact,
    Conflict,
}

fn delegated_event_commit_state(
    events: &[heycode_session::SessionEvent],
    candidate: &heycode_session::SessionEventKind,
) -> DelegatedCommitState {
    let tool_scope = match candidate {
        heycode_session::SessionEventKind::ToolCall { turn, .. } => events
            .iter()
            .rposition(|event| {
                matches!(&event.kind, heycode_session::SessionEventKind::TurnStart { turn: found } if *found == *turn)
            })
            .map_or(&[][..], |start| &events[start..]),
        heycode_session::SessionEventKind::ToolResult { .. }
        | heycode_session::SessionEventKind::RichToolResult { .. } => events
            .iter()
            .rposition(|event| {
                matches!(&event.kind, heycode_session::SessionEventKind::TurnStart { .. })
            })
            .map_or(&[][..], |start| &events[start..]),
        _ => events,
    };
    let candidates = if matches!(
        candidate,
        heycode_session::SessionEventKind::ToolCall { .. }
            | heycode_session::SessionEventKind::ToolResult { .. }
            | heycode_session::SessionEventKind::RichToolResult { .. }
    ) {
        tool_scope
    } else {
        events
    };
    for event in candidates {
        let state = match (&event.kind, candidate) {
            (
                heycode_session::SessionEventKind::TurnStart { turn: left },
                heycode_session::SessionEventKind::TurnStart { turn: right },
            ) if left == right => DelegatedCommitState::Exact,
            (
                heycode_session::SessionEventKind::TurnEnd {
                    turn: left,
                    reason: left_reason,
                },
                heycode_session::SessionEventKind::TurnEnd {
                    turn: right,
                    reason: right_reason,
                },
            ) if left == right => {
                if left_reason == right_reason {
                    DelegatedCommitState::Exact
                } else {
                    DelegatedCommitState::Conflict
                }
            }
            (
                heycode_session::SessionEventKind::ToolCall {
                    turn: left_turn,
                    call_id: left,
                    name: left_name,
                    args: left_args,
                },
                heycode_session::SessionEventKind::ToolCall {
                    turn: right_turn,
                    call_id: right,
                    name: right_name,
                    args: right_args,
                },
            ) if left == right => {
                if left_turn == right_turn && left_name == right_name && left_args == right_args {
                    DelegatedCommitState::Exact
                } else {
                    DelegatedCommitState::Conflict
                }
            }
            (
                heycode_session::SessionEventKind::ToolResult {
                    call_id: left,
                    content: left_content,
                    is_error: left_error,
                    untrusted_content: left_untrusted,
                },
                heycode_session::SessionEventKind::ToolResult {
                    call_id: right,
                    content: right_content,
                    is_error: right_error,
                    untrusted_content: right_untrusted,
                },
            ) if left == right => {
                if left_content == right_content
                    && left_error == right_error
                    && left_untrusted == right_untrusted
                {
                    DelegatedCommitState::Exact
                } else {
                    DelegatedCommitState::Conflict
                }
            }
            (
                heycode_session::SessionEventKind::RichToolResult {
                    call_id: left,
                    result,
                    is_error: left_error,
                    untrusted_content: left_untrusted,
                },
                heycode_session::SessionEventKind::ToolResult {
                    call_id: right,
                    content: right_content,
                    is_error: right_error,
                    untrusted_content: right_untrusted,
                },
            ) if left == right => {
                if result.render_for_model() == *right_content
                    && left_error == right_error
                    && left_untrusted == right_untrusted
                {
                    DelegatedCommitState::Exact
                } else {
                    DelegatedCommitState::Conflict
                }
            }
            _ => continue,
        };
        return state;
    }
    DelegatedCommitState::Missing
}

fn runtime_tool_result_content(result: &serde_json::Value) -> Result<String, AppServerError> {
    match result {
        serde_json::Value::String(content) => Ok(content.clone()),
        value => serde_json::to_string(value).map_err(|_| AppServerError::invalid()),
    }
}

fn committed_tool_boundary(
    events: &[heycode_session::SessionEvent],
    call_id: &heycode_core::CallId,
) -> Option<heycode_core::UntrustedContentBoundary> {
    let current_turn = events
        .iter()
        .rposition(|event| {
            matches!(
                event.kind,
                heycode_session::SessionEventKind::TurnStart { .. }
            )
        })
        .map_or(&[][..], |start| &events[start..]);
    current_turn
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            heycode_session::SessionEventKind::ToolResult {
                call_id: found,
                untrusted_content,
                ..
            }
            | heycode_session::SessionEventKind::RichToolResult {
                call_id: found,
                untrusted_content,
                ..
            } if found == call_id => Some(*untrusted_content),
            _ => None,
        })
        .flatten()
}

fn select_delegated_configuration(
    available: &heycode_runtime::RuntimeConfiguration,
    requested: &AppRuntimeConfiguration,
    support: &heycode_runtime::RuntimeConfigurationCapabilities,
) -> Result<heycode_runtime::RuntimeConfiguration, AppServerError> {
    let mut configuration = heycode_runtime::RuntimeConfiguration::new();
    let prompt = requested.system_prompt.as_deref().or_else(|| {
        (support.system_prompt == heycode_llm::CapabilitySupport::Supported)
            .then(|| available.system_prompt())
            .flatten()
    });
    if let Some(prompt) = prompt {
        configuration = configuration
            .with_system_prompt(prompt)
            .map_err(|_| AppServerError::invalid())?;
    }
    if let Some(tools) = &requested.tools {
        if tools
            .iter()
            .any(|tool| !available.tools().iter().any(|candidate| candidate == tool))
        {
            return Err(AppServerError::invalid());
        }
        configuration = configuration
            .with_tools(tools.clone())
            .map_err(|_| AppServerError::invalid())?;
    } else if support.tools == heycode_llm::CapabilitySupport::Supported {
        configuration = configuration
            .with_tools(available.tools().to_vec())
            .map_err(|_| AppServerError::invalid())?;
    }
    if let Some(model) = available.model() {
        configuration = configuration
            .with_model(model)
            .map_err(|_| AppServerError::invalid())?;
    }
    if let Some(effort) = available.reasoning_effort() {
        configuration = configuration
            .with_reasoning_effort(effort)
            .map_err(|_| AppServerError::invalid())?;
    }
    Ok(configuration)
}

fn next_local_turn(events: &[heycode_session::SessionEvent]) -> u64 {
    events
        .iter()
        .filter_map(|event| match event.kind {
            heycode_session::SessionEventKind::TurnStart { turn } => Some(turn),
            _ => None,
        })
        .max()
        .map_or(0, |turn| turn.saturating_add(1))
}

fn latest_committed_input(
    agent: &heycode_agent::Agent,
) -> Option<(
    String,
    Vec<heycode_core::AttachmentMetadata>,
    Vec<heycode_core::DocumentInputRoute>,
)> {
    let session = agent
        .session()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let events = session.events();
    let turn = events.iter().rposition(|event| {
        matches!(
            event.kind,
            heycode_session::SessionEventKind::TurnStart { .. }
        )
    })?;
    let user = events[..turn].iter().rposition(|event| {
        matches!(
            event.kind,
            heycode_session::SessionEventKind::UserMessage { .. }
        )
    })?;
    let heycode_session::SessionEventKind::UserMessage { text } = &events[user].kind else {
        return None;
    };
    let (attachments, routes) = if user > 0 {
        match &events[user - 1].kind {
            heycode_session::SessionEventKind::UserAttachments {
                attachments,
                document_routes,
            } => (attachments.clone(), document_routes.clone()),
            _ => (Vec::new(), Vec::new()),
        }
    } else {
        (Vec::new(), Vec::new())
    };
    Some((text.clone(), attachments, routes))
}

/// Classify a runtime failure and keep its own redacted one-line message as the
/// cause, so "app-server is unavailable" can say *what* is unavailable.
fn map_runtime_error(error: heycode_runtime::RuntimeError) -> AppServerError {
    let classified = match error.code() {
        RuntimeErrorCode::Cancelled => AppServerError::cancelled(),
        RuntimeErrorCode::Conflict => AppServerError::classified(AppServerErrorCode::Conflict),
        RuntimeErrorCode::Closed => AppServerError::classified(AppServerErrorCode::Closed),
        RuntimeErrorCode::InvalidRequest => AppServerError::invalid(),
        RuntimeErrorCode::Unsupported => {
            AppServerError::classified(AppServerErrorCode::Unsupported)
        }
        RuntimeErrorCode::Unavailable
        | RuntimeErrorCode::Unauthorized
        | RuntimeErrorCode::NotFound
        | RuntimeErrorCode::Protocol => AppServerError::unavailable(),
        RuntimeErrorCode::Internal => AppServerError::classified(AppServerErrorCode::Internal),
    };
    // RuntimeError messages are validated safe one-liners by construction.
    classified
        .clone()
        .with_detail(error.message())
        .unwrap_or(classified)
}

fn app_runtime_configuration(
    configuration: &heycode_runtime::RuntimeConfiguration,
) -> AppRuntimeConfiguration {
    AppRuntimeConfiguration {
        system_prompt: configuration.system_prompt().map(str::to_owned),
        tools: configuration
            .tools_configured()
            .then(|| configuration.tools().to_vec()),
        model: configuration.model().map(str::to_owned),
        reasoning_effort: configuration.reasoning_effort().map(str::to_owned),
    }
}

fn app_runtime_model_configuration(
    configuration: heycode_runtime::RuntimeModelConfiguration,
) -> AppRuntimeModelConfiguration {
    AppRuntimeModelConfiguration {
        model: configuration.model,
        display_name: configuration.display_name,
        resolved_model: configuration.resolved_model,
        description: configuration.description,
        context_window: configuration.context_window,
        default_reasoning_effort: configuration.default_reasoning_effort,
        reasoning_efforts: configuration.reasoning_efforts,
    }
}

fn runtime_configuration_from_app(
    configuration: &AppRuntimeConfiguration,
) -> Result<heycode_runtime::RuntimeConfiguration, String> {
    let mut runtime = heycode_runtime::RuntimeConfiguration::new();
    if let Some(prompt) = &configuration.system_prompt {
        runtime = runtime
            .with_system_prompt(prompt.clone())
            .map_err(|error| error.to_string())?;
    }
    if let Some(tools) = &configuration.tools {
        runtime = runtime
            .with_tools(tools.clone())
            .map_err(|error| error.to_string())?;
    }
    if let Some(model) = &configuration.model {
        runtime = runtime
            .with_model(model.clone())
            .map_err(|error| error.to_string())?;
    }
    if let Some(effort) = &configuration.reasoning_effort {
        runtime = runtime
            .with_reasoning_effort(effort.clone())
            .map_err(|error| error.to_string())?;
    }
    Ok(runtime)
}

fn app_configuration_capabilities(
    capabilities: &heycode_runtime::RuntimeConfigurationCapabilities,
) -> AppRuntimeConfigurationCapabilities {
    let evidence = |support| match support {
        heycode_llm::CapabilitySupport::Supported => AppCapabilityEvidence::Supported,
        heycode_llm::CapabilitySupport::Unsupported => AppCapabilityEvidence::Unsupported,
        heycode_llm::CapabilitySupport::Unknown => AppCapabilityEvidence::Unknown,
    };
    AppRuntimeConfigurationCapabilities {
        system_prompt: evidence(capabilities.system_prompt),
        tools: evidence(capabilities.tools),
        model: evidence(capabilities.model),
        reasoning_effort: evidence(capabilities.reasoning_effort),
    }
}

const fn map_finish(reason: RuntimeFinishReason) -> AppTurnReason {
    match reason {
        RuntimeFinishReason::Stop => AppTurnReason::Stop,
        RuntimeFinishReason::Limit => AppTurnReason::Limit,
        RuntimeFinishReason::Cancelled => AppTurnReason::Cancelled,
        RuntimeFinishReason::Error => AppTurnReason::Error,
    }
}

/// Transport-neutral SDK client bound directly to the local composed server.
pub type LocalAppClient = heycode_sdk::AppClient<AppServer>;

#[async_trait]
impl heycode_sdk::AppTransport for AppServer {
    async fn exchange(
        &self,
        request: String,
        notifications: mpsc::Sender<String>,
        cancellation: CancellationToken,
    ) -> Result<String, AppServerError> {
        let (typed_tx, mut typed_rx) = mpsc::channel(32);
        let response = self.request(&request, typed_tx, cancellation);
        tokio::pin!(response);
        let mut notifications_open = true;
        loop {
            tokio::select! {
                maybe_notification = typed_rx.recv(), if notifications_open => {
                    match maybe_notification {
                        Some(notification) => {
                            let raw = serde_json::to_string(&notification)
                            .map_err(|_| AppServerError::invalid())?;
                            let _sent = notifications.send(raw).await;
                        }
                        None => notifications_open = false,
                    }
                }
                result = &mut response => {
                    while let Ok(notification) = typed_rx.try_recv() {
                        let raw = serde_json::to_string(&notification)
                            .map_err(|_| AppServerError::invalid())?;
                        let _sent = notifications.send(raw).await;
                    }
                    return Ok(result);
                }
            }
        }
    }
}

/// Compose the stable local app-server service.
#[must_use]
pub fn app_server_plugin() -> Box<dyn heycode_core::Plugin> {
    struct AppServerPlugin;

    impl heycode_core::Plugin for AppServerPlugin {
        fn name(&self) -> &'static str {
            "app-server"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_APP_SERVER]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_agent::SERVICE_AGENT,
                heycode_agent::SERVICE_APPROVAL,
                heycode_agent::SERVICE_QUESTIONS,
                heycode_runtime::SERVICE_RUNTIMES,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let agent = context
                .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service type mismatch"))?;
            let runtimes = context
                .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
                .ok_or_else(|| heycode_core::CoreError::other("runtime service type mismatch"))?;
            let runtime = runtimes
                .get("native")
                .map_err(|_| heycode_core::CoreError::other("runtime registry unavailable"))?
                .ok_or_else(|| heycode_core::CoreError::other("native runtime is missing"))?;
            let approval = context.get::<heycode_agent::InteractiveApproval>(
                heycode_agent::SERVICE_APPROVAL_INTERACTIVE,
            );
            let questions = context
                .get::<heycode_agent::InteractiveQuestion>(heycode_agent::SERVICE_QUESTIONS)
                .ok_or_else(|| heycode_core::CoreError::other("question service type mismatch"))?;
            let server = AppServer::runtime(agent, approval, questions, runtime);
            let lifecycle = server.lifecycle.clone();
            context.provide(SERVICE_APP_SERVER, self.name(), server)?;
            context.effect(move || lifecycle.cancel());
            Ok(())
        }
    }

    Box::new(AppServerPlugin)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use tokio::sync::Notify;

    #[test]
    fn runtime_model_wire_preserves_provider_metadata_without_inference() {
        let wire = app_runtime_model_configuration(heycode_runtime::RuntimeModelConfiguration {
            model: "opus[1m]".to_owned(),
            display_name: "Opus (1M context)".to_owned(),
            resolved_model: Some("claude-opus-5[1m]".to_owned()),
            description: Some("Provider supplied description".to_owned()),
            context_window: None,
            default_reasoning_effort: None,
            reasoning_efforts: vec!["low".to_owned(), "high".to_owned()],
        });

        assert_eq!(wire.model, "opus[1m]");
        assert_eq!(wire.display_name, "Opus (1M context)");
        assert_eq!(wire.resolved_model.as_deref(), Some("claude-opus-5[1m]"));
        assert_eq!(wire.context_window, None);
        assert_eq!(wire.default_reasoning_effort, None);
    }

    fn committed_event(
        seq: u64,
        kind: heycode_session::SessionEventKind,
    ) -> heycode_session::SessionEvent {
        heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq,
            time_ms: 1,
            kind,
        }
    }

    #[test]
    fn delegated_tool_dedupe_is_scoped_to_the_current_turn() {
        let call_id = heycode_core::CallId::from_raw("provider-reused-id");
        let mut events = vec![
            committed_event(0, heycode_session::SessionEventKind::TurnStart { turn: 0 }),
            committed_event(
                1,
                heycode_session::SessionEventKind::ToolCall {
                    turn: 0,
                    call_id: call_id.clone(),
                    name: "first".to_owned(),
                    args: serde_json::json!({}),
                },
            ),
            committed_event(
                2,
                heycode_session::SessionEventKind::ToolResult {
                    call_id: call_id.clone(),
                    content: "first".to_owned(),
                    is_error: false,
                    untrusted_content: Some(heycode_core::UntrustedContentBoundary::mcp()),
                },
            ),
            committed_event(
                3,
                heycode_session::SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: heycode_session::TurnEndReason::Stop,
                },
            ),
            committed_event(4, heycode_session::SessionEventKind::TurnStart { turn: 1 }),
        ];
        assert_eq!(committed_tool_boundary(&events, &call_id), None);
        let second_call = heycode_session::SessionEventKind::ToolCall {
            turn: 1,
            call_id: call_id.clone(),
            name: "second".to_owned(),
            args: serde_json::json!({}),
        };
        assert_eq!(
            delegated_event_commit_state(&events, &second_call),
            DelegatedCommitState::Missing
        );
        events.push(committed_event(5, second_call.clone()));
        assert_eq!(
            delegated_event_commit_state(&events, &second_call),
            DelegatedCommitState::Exact
        );
        let mismatched_call = heycode_session::SessionEventKind::ToolCall {
            turn: 1,
            call_id: call_id.clone(),
            name: "different".to_owned(),
            args: serde_json::json!({}),
        };
        assert_eq!(
            delegated_event_commit_state(&events, &mismatched_call),
            DelegatedCommitState::Conflict
        );

        let second_result = heycode_session::SessionEventKind::ToolResult {
            call_id: call_id.clone(),
            content: "second".to_owned(),
            is_error: false,
            untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
        };
        assert_eq!(
            delegated_event_commit_state(&events, &second_result),
            DelegatedCommitState::Missing
        );
        events.push(committed_event(6, second_result.clone()));
        assert_eq!(
            delegated_event_commit_state(&events, &second_result),
            DelegatedCommitState::Exact
        );
        assert_eq!(
            committed_tool_boundary(&events, &call_id),
            Some(heycode_core::UntrustedContentBoundary::web())
        );
        let mismatched_result = heycode_session::SessionEventKind::ToolResult {
            call_id: call_id.clone(),
            content: "different".to_owned(),
            is_error: false,
            untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
        };
        assert_eq!(
            delegated_event_commit_state(&events, &mismatched_result),
            DelegatedCommitState::Conflict
        );
        let mismatched_boundary = heycode_session::SessionEventKind::ToolResult {
            call_id,
            content: "second".to_owned(),
            is_error: false,
            untrusted_content: Some(heycode_core::UntrustedContentBoundary::mcp()),
        };
        assert_eq!(
            delegated_event_commit_state(&events, &mismatched_boundary),
            DelegatedCommitState::Conflict
        );
        assert_eq!(
            runtime_tool_result_content(&serde_json::json!("second")).unwrap(),
            "second"
        );
    }

    #[test]
    fn delegated_defaults_include_only_positively_supported_host_controls() {
        let tool = heycode_core::ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one file".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        };
        let available = heycode_runtime::RuntimeConfiguration::new()
            .with_system_prompt("host instructions")
            .unwrap()
            .with_tools(vec![tool])
            .unwrap()
            .with_model("provider-model")
            .unwrap();
        let support = heycode_runtime::RuntimeConfigurationCapabilities {
            system_prompt: heycode_llm::CapabilitySupport::Unsupported,
            tools: heycode_llm::CapabilitySupport::Unsupported,
            model: heycode_llm::CapabilitySupport::Supported,
            reasoning_effort: heycode_llm::CapabilitySupport::Unsupported,
        };
        let selected = select_delegated_configuration(
            &available,
            &AppRuntimeConfiguration::default(),
            &support,
        )
        .unwrap();
        assert_eq!(selected.model(), Some("provider-model"));
        assert_eq!(selected.system_prompt(), None);
        assert!(!selected.tools_configured());

        let explicit = select_delegated_configuration(
            &available,
            &AppRuntimeConfiguration {
                system_prompt: Some("explicit instructions".to_owned()),
                tools: Some(Vec::new()),
                ..AppRuntimeConfiguration::default()
            },
            &support,
        )
        .unwrap();
        assert_eq!(explicit.system_prompt(), Some("explicit instructions"));
        assert!(explicit.tools_configured());
        assert!(explicit.tools().is_empty());
    }

    pub(crate) struct FakeBackend {
        runtime_id: String,
        cwd: PathBuf,
        configuration: StdMutex<AppRuntimeConfiguration>,
        cancelled: AtomicBool,
        closed: AtomicBool,
        permission_waiting: AtomicBool,
        permission_answered: AtomicBool,
        permission_notify: Notify,
        question_answers: StdMutex<Vec<(String, Option<String>)>>,
    }

    #[async_trait]
    impl AppBackend for FakeBackend {
        async fn open(
            &self,
            configuration: AppRuntimeConfiguration,
            cancellation: CancellationToken,
        ) -> Result<AppSessionInfo, AppServerError> {
            if cancellation.is_cancelled() {
                return Err(AppServerError::cancelled());
            }
            *self.configuration.lock().unwrap() = configuration.clone();
            Ok(AppSessionInfo {
                session_id: "session-1".to_owned(),
                runtime_id: self.runtime_id.clone(),
                cwd: self.cwd.clone(),
                configuration,
                configuration_capabilities: fake_configuration_capabilities(),
                model_configuration: None,
            })
        }

        async fn configure(
            &self,
            configuration: AppRuntimeConfiguration,
            cancellation: CancellationToken,
        ) -> Result<AppSessionInfo, AppServerError> {
            self.open(configuration, cancellation).await
        }

        async fn turn(
            &self,
            text: String,
            _attachments: Vec<heycode_core::AttachmentMetadata>,
            _pending_message_id: Option<String>,
            sink: AppEventSink,
            cancellation: CancellationToken,
        ) -> Result<AppTurnResult, AppServerError> {
            if cancellation.is_cancelled() {
                return Err(AppServerError::cancelled());
            }
            if text == "permission" {
                self.permission_waiting.store(true, Ordering::SeqCst);
                sink.emit(AppServerEvent::PermissionRequested {
                    request_id: "permission-7".to_owned(),
                    action: "Run fixture".to_owned(),
                    detail: "Exact IDE choice required".to_owned(),
                })
                .await?;
                tokio::select! {
                    () = cancellation.cancelled() => {
                        self.permission_waiting.store(false, Ordering::SeqCst);
                        return Err(AppServerError::cancelled());
                    }
                    () = self.permission_notify.notified() => {}
                }
                self.permission_waiting.store(false, Ordering::SeqCst);
                sink.emit(AppServerEvent::AssistantDelta {
                    text: "approved".to_owned(),
                })
                .await?;
                sink.emit(AppServerEvent::TurnFinished {
                    turn_id: "permission-turn".to_owned(),
                    reason: AppTurnReason::Stop,
                    usage: None,
                })
                .await?;
                return Ok(AppTurnResult {
                    turn_id: Some("permission-turn".to_owned()),
                    reason: AppTurnReason::Stop,
                });
            }
            if text == "audio" {
                let attachment = heycode_core::AttachmentMetadata::new_audio(
                    heycode_core::AttachmentContentId::from_sha256([0x72; 32]),
                    heycode_core::AttachmentMediaType::new("audio/wav")
                        .map_err(|_| AppServerError::invalid())?,
                    16_044,
                    Some("answer.wav".to_owned()),
                    heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16)
                        .map_err(|_| AppServerError::invalid())?,
                )
                .map_err(|_| AppServerError::invalid())?;
                sink.emit(AppServerEvent::UserInput {
                    text,
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                })
                .await?;
                sink.emit(AppServerEvent::TurnStarted {
                    turn_id: "audio-turn".to_owned(),
                })
                .await?;
                sink.emit(AppServerEvent::AssistantAudio {
                    attachments: vec![attachment],
                })
                .await?;
                sink.emit(AppServerEvent::TurnFinished {
                    turn_id: "audio-turn".to_owned(),
                    reason: AppTurnReason::Stop,
                    usage: None,
                })
                .await?;
                return Ok(AppTurnResult {
                    turn_id: Some("audio-turn".to_owned()),
                    reason: AppTurnReason::Stop,
                });
            }
            sink.emit(AppServerEvent::UserInput {
                text,
                attachments: Vec::new(),
                document_routes: Vec::new(),
            })
            .await?;
            sink.emit(AppServerEvent::TurnStarted {
                turn_id: "1".to_owned(),
            })
            .await?;
            sink.emit(AppServerEvent::AssistantDelta {
                text: "hello".to_owned(),
            })
            .await?;
            sink.emit(AppServerEvent::TurnFinished {
                turn_id: "1".to_owned(),
                reason: AppTurnReason::Stop,
                usage: Some(heycode_core::TokenUsage {
                    prompt_tokens: 2,
                    completion_tokens: 1,
                }),
            })
            .await?;
            Ok(AppTurnResult {
                turn_id: Some("1".to_owned()),
                reason: AppTurnReason::Stop,
            })
        }

        async fn cancel(&self, _cancellation: CancellationToken) -> Result<(), AppServerError> {
            self.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn respond_permission(
            &self,
            request_id: String,
            decision: AppPermissionDecision,
            _cancellation: CancellationToken,
        ) -> Result<(), AppServerError> {
            if request_id == "permission-7" {
                if !self.permission_waiting.load(Ordering::SeqCst)
                    || decision != AppPermissionDecision::AllowOnce
                    || self
                        .permission_answered
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                {
                    return Err(AppServerError::classified(AppServerErrorCode::Conflict));
                }
                self.permission_notify.notify_waiters();
            }
            Ok(())
        }

        async fn respond_question(
            &self,
            request_id: String,
            answer: Option<String>,
            _cancellation: CancellationToken,
        ) -> Result<(), AppServerError> {
            self.question_answers
                .lock()
                .unwrap()
                .push((request_id, answer));
            Ok(())
        }

        async fn close(&self, _cancellation: CancellationToken) -> Result<(), AppServerError> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn retire(&self) -> Result<(), AppServerError> {
            self.cancelled.store(true, Ordering::SeqCst);
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeRuntime {
        descriptor: heycode_runtime::AgentRuntimeDescriptor,
    }

    #[async_trait]
    impl AgentRuntime for FakeRuntime {
        fn descriptor(&self) -> &heycode_runtime::AgentRuntimeDescriptor {
            &self.descriptor
        }

        async fn account(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<heycode_runtime::AccountState, heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn models(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<heycode_llm::CatalogSnapshot, heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn start(
            &self,
            _request: RuntimeStart,
            _cancellation: CancellationToken,
        ) -> Result<Arc<dyn RuntimeSession>, heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn resume(
            &self,
            _request: RuntimeResume,
            _cancellation: CancellationToken,
        ) -> Result<Arc<dyn RuntimeSession>, heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn fork(
            &self,
            _request: heycode_runtime::RuntimeFork,
            _cancellation: CancellationToken,
        ) -> Result<Arc<dyn RuntimeSession>, heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }
    }

    fn fake_runtime(id: &str, kind: heycode_runtime::AgentRuntimeKind) -> Arc<dyn AgentRuntime> {
        Arc::new(FakeRuntime {
            descriptor: heycode_runtime::AgentRuntimeDescriptor::new(
                id,
                format!("{id} runtime"),
                kind,
                heycode_runtime::RuntimeCapabilities::unknown(),
            )
            .unwrap(),
        })
    }

    struct FakeBackendFactory {
        latest: StdMutex<Option<Arc<FakeBackend>>>,
    }

    impl FakeBackendFactory {
        fn latest(&self) -> Arc<FakeBackend> {
            self.latest.lock().unwrap().clone().unwrap()
        }
    }

    impl AppBackendFactory for FakeBackendFactory {
        fn build(
            &self,
            runtime: Arc<dyn AgentRuntime>,
            workspace: PathBuf,
            _model: Option<String>,
            _reasoning_effort: Option<String>,
        ) -> Arc<dyn AppBackend> {
            let backend = Arc::new(FakeBackend {
                runtime_id: runtime.descriptor().id().as_str().to_owned(),
                cwd: workspace,
                configuration: StdMutex::new(AppRuntimeConfiguration::default()),
                cancelled: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                permission_waiting: AtomicBool::new(false),
                permission_answered: AtomicBool::new(false),
                permission_notify: Notify::new(),
                question_answers: StdMutex::new(Vec::new()),
            });
            *self.latest.lock().unwrap() = Some(backend.clone());
            backend
        }

        fn validate_route_change(&self) -> Result<(), AppServerError> {
            Ok(())
        }

        fn validate_runtime_selection(&self, _runtime_id: &str) -> Result<(), AppServerError> {
            Ok(())
        }
    }

    fn fake_configuration_capabilities() -> AppRuntimeConfigurationCapabilities {
        AppRuntimeConfigurationCapabilities {
            system_prompt: AppCapabilityEvidence::Supported,
            tools: AppCapabilityEvidence::Supported,
            model: AppCapabilityEvidence::Supported,
            reasoning_effort: AppCapabilityEvidence::Supported,
        }
    }

    pub(crate) fn server() -> (Arc<AppServer>, Arc<FakeBackend>) {
        let factory = Arc::new(FakeBackendFactory {
            latest: StdMutex::new(None),
        });
        let server = Arc::new(AppServer::with_factory(
            factory.clone(),
            fake_runtime("native", heycode_runtime::AgentRuntimeKind::Native),
            std::path::PathBuf::from("/workspace"),
            CancellationToken::new(),
        ));
        (server, factory.latest())
    }

    fn server_at(workspace: &Path) -> (Arc<AppServer>, Arc<FakeBackendFactory>) {
        let factory = Arc::new(FakeBackendFactory {
            latest: StdMutex::new(None),
        });
        let server = Arc::new(AppServer::with_factory(
            factory.clone(),
            fake_runtime("native", heycode_runtime::AgentRuntimeKind::Native),
            std::fs::canonicalize(workspace).unwrap(),
            CancellationToken::new(),
        ));
        (server, factory)
    }

    #[tokio::test]
    async fn local_client_round_trips_every_request_response_and_event_through_json_v1() {
        let (server, backend) = server();
        let client = LocalAppClient::new(server);
        let opened = client.open().await.unwrap();
        assert_eq!(opened.session_id, "session-1");
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let turn = client
            .turn("hi", Vec::new(), events_tx, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(turn.reason, AppTurnReason::Stop);
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].params.sequence, 0);
        assert!(matches!(
            events[0].params.event,
            AppServerEvent::UserInput { ref text, .. } if text == "hi"
        ));
        assert!(matches!(
            events[3].params.event,
            AppServerEvent::TurnFinished {
                reason: AppTurnReason::Stop,
                ..
            }
        ));
        client.cancel().await.unwrap();
        client
            .respond_permission("request-1", AppPermissionDecision::AllowOnce)
            .await
            .unwrap();
        client.respond_question("request-2", "Yes").await.unwrap();
        client.cancel_question("request-3").await.unwrap();
        client.close().await.unwrap();
        assert!(backend.cancelled.load(Ordering::SeqCst));
        assert!(backend.closed.load(Ordering::SeqCst));
        assert_eq!(
            *backend.question_answers.lock().unwrap(),
            vec![
                ("request-2".to_owned(), Some("Yes".to_owned())),
                ("request-3".to_owned(), None),
            ]
        );
    }

    #[tokio::test]
    async fn logout_disconnect_retires_backend_rejects_new_calls_and_keeps_close_idempotent() {
        let (server, backend) = server();
        let client = LocalAppClient::new(server.clone());
        client.open().await.unwrap();

        server.disconnect_backend("native").await.unwrap();

        assert!(backend.cancelled.load(Ordering::SeqCst));
        assert!(backend.closed.load(Ordering::SeqCst));
        assert_eq!(
            client.open().await.unwrap_err().code(),
            AppServerErrorCode::Closed
        );
        let (events, _receiver) = mpsc::channel(1);
        assert_eq!(
            client
                .turn("must not run", Vec::new(), events, CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            AppServerErrorCode::Closed
        );
        client.close().await.unwrap();
        server.disconnect_backend("native").await.unwrap();
    }

    #[tokio::test]
    async fn question_response_requires_exactly_one_answer_or_cancellation() {
        let (server, backend) = server();
        LocalAppClient::new(server.clone()).open().await.unwrap();
        for params in [
            serde_json::json!({
                "sessionId":"session-1","requestId":"request-1",
                "answer":"Yes","cancelled":true
            }),
            serde_json::json!({
                "sessionId":"session-1","requestId":"request-1"
            }),
            serde_json::json!({
                "sessionId":"session-1","requestId":"request-1",
                "answer":"Yes","selectedAnswers":["Yes"]
            }),
            serde_json::json!({
                "sessionId":"session-1","requestId":"request-1",
                "cancelled":true,"selectedAnswers":["Yes"]
            }),
        ] {
            let (events, _receiver) = mpsc::channel(1);
            let response = server
                .request(
                    &serde_json::json!({
                        "jsonrpc":"2.0","id":99,
                        "method":"session/question/respond","params":params
                    })
                    .to_string(),
                    events,
                    CancellationToken::new(),
                )
                .await;
            let response: Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["error"]["code"], -32602);
        }
        assert!(backend.question_answers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_client_receives_audio_metadata_without_encoded_bytes() {
        let (server, _backend) = server();
        let client = LocalAppClient::new(server);
        client.open().await.unwrap();
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let turn = client
            .turn("audio", Vec::new(), events_tx, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(turn.reason, AppTurnReason::Stop);
        let mut audio = None;
        while let Some(event) = events_rx.recv().await {
            let raw = serde_json::to_string(&event).unwrap();
            assert!(!raw.contains("RAW-AUDIO"));
            assert!(!raw.contains("base64"));
            if let AppServerEvent::AssistantAudio { attachments } = event.params.event {
                audio = Some(attachments);
            }
        }
        let attachments = audio.expect("audio event must cross the stable wire");
        assert_eq!(attachments[0].media_type().as_str(), "audio/wav");
        assert_eq!(attachments[0].audio().unwrap().duration_ms(), 1_000);
    }

    #[test]
    fn native_audio_scan_emits_only_post_baseline_durable_associations() {
        let metadata = heycode_core::AttachmentMetadata::new_audio(
            heycode_core::AttachmentContentId::from_sha256([0x73; 32]),
            heycode_core::AttachmentMediaType::new("audio/wav").unwrap(),
            16_044,
            None,
            heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
        )
        .unwrap();
        let events = vec![
            heycode_session::SessionEvent {
                v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                seq: 0,
                time_ms: 1,
                kind: heycode_session::SessionEventKind::AssistantAudio {
                    turn: 1,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("request-old"),
                    attachments: vec![metadata.clone()],
                },
            },
            heycode_session::SessionEvent {
                v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                seq: 1,
                time_ms: 2,
                kind: heycode_session::SessionEventKind::AssistantAudio {
                    turn: 2,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("request-new"),
                    attachments: vec![metadata.clone()],
                },
            },
        ];
        assert_eq!(assistant_audio_after(&events, 1), [vec![metadata]]);
    }

    #[tokio::test]
    async fn stdio_transport_multiplexes_one_real_host_session_and_bounded_event_stream() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let (server, backend) = server();
        let (client, host) = tokio::io::duplex(64 * 1024);
        let (host_read, host_write) = tokio::io::split(host);
        let cancellation = CancellationToken::new();
        let serving = tokio::spawn(serve_stdio_transport(
            server,
            host_read,
            host_write,
            cancellation,
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut lines = BufReader::new(client_read).lines();

        async fn request(
            writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
            operation: u64,
            method: &str,
            params: Value,
        ) {
            let frame = serde_json::json!({
                "kind":"request",
                "operation":operation,
                "request":{"jsonrpc":"2.0","id":operation,"method":method,"params":params}
            });
            writer
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
        }

        request(&mut client_write, 1, "initialize", serde_json::json!({})).await;
        let initialized: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(initialized["kind"], "response");
        assert_eq!(initialized["operation"], 1);
        assert_eq!(initialized["response"]["id"], 1);

        request(&mut client_write, 2, "session/open", serde_json::json!({})).await;
        let opened: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(opened["operation"], 2);
        assert_eq!(opened["response"]["result"]["sessionId"], "session-1");

        request(
            &mut client_write,
            3,
            "turn/start",
            serde_json::json!({"sessionId":"session-1","text":"from VS Code","attachments":[]}),
        )
        .await;
        let mut event_types = Vec::new();
        loop {
            let frame: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(frame["operation"], 3);
            if frame["kind"] == "response" {
                assert_eq!(frame["response"]["result"]["reason"], "stop");
                break;
            }
            assert_eq!(frame["kind"], "notification");
            event_types.push(
                frame["notification"]["params"]["event"]["type"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        assert_eq!(
            event_types,
            [
                "user_input",
                "turn_started",
                "assistant_delta",
                "turn_finished"
            ]
        );

        request(&mut client_write, 4, "session/close", serde_json::json!({})).await;
        let closed: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(closed["operation"], 4);
        assert_eq!(closed["response"]["result"], Value::Null);
        assert!(backend.closed.load(Ordering::SeqCst));

        client_write.shutdown().await.unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stdio_transport_forwards_exact_correlated_permission_and_rejects_duplicate() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let (server, _backend) = server();
        let (client, host) = tokio::io::duplex(64 * 1024);
        let (host_read, host_write) = tokio::io::split(host);
        let serving = tokio::spawn(serve_stdio_transport(
            server,
            host_read,
            host_write,
            CancellationToken::new(),
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut lines = BufReader::new(client_read).lines();

        async fn send(
            writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
            operation: u64,
            method: &str,
            params: Value,
        ) {
            let frame = serde_json::json!({
                "kind":"request",
                "operation":operation,
                "request":{"jsonrpc":"2.0","id":operation,"method":method,"params":params}
            });
            writer
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
        }

        send(&mut client_write, 1, "initialize", serde_json::json!({})).await;
        lines.next_line().await.unwrap().unwrap();
        send(&mut client_write, 2, "session/open", serde_json::json!({})).await;
        lines.next_line().await.unwrap().unwrap();
        send(
            &mut client_write,
            3,
            "turn/start",
            serde_json::json!({"sessionId":"session-1","text":"permission","attachments":[]}),
        )
        .await;
        let requested: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(requested["kind"], "notification");
        assert_eq!(requested["operation"], 3);
        assert_eq!(
            requested["notification"]["params"]["event"]["request_id"],
            "permission-7"
        );

        send(
            &mut client_write,
            4,
            "session/permission/respond",
            serde_json::json!({
                "sessionId":"session-1",
                "requestId":"permission-7",
                "decision":"allow_once"
            }),
        )
        .await;
        let mut permission_settled = false;
        let mut turn_settled = false;
        let mut turn_events = Vec::new();
        while !permission_settled || !turn_settled {
            let frame: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            match (frame["operation"].as_u64(), frame["kind"].as_str()) {
                (Some(4), Some("response")) => {
                    assert_eq!(frame["response"]["result"], Value::Null);
                    permission_settled = true;
                }
                (Some(3), Some("notification")) => turn_events.push(
                    frame["notification"]["params"]["event"]["type"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                ),
                (Some(3), Some("response")) => {
                    assert_eq!(frame["response"]["result"]["turnId"], "permission-turn");
                    turn_settled = true;
                }
                _ => panic!("unexpected stdio frame shape"),
            }
        }
        assert_eq!(turn_events, ["assistant_delta", "turn_finished"]);

        send(
            &mut client_write,
            5,
            "session/permission/respond",
            serde_json::json!({
                "sessionId":"session-1",
                "requestId":"permission-7",
                "decision":"allow_once"
            }),
        )
        .await;
        let duplicate: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(duplicate["operation"], 5);
        assert_eq!(duplicate["response"]["error"]["code"], -32001);

        send(&mut client_write, 6, "session/close", serde_json::json!({})).await;
        lines.next_line().await.unwrap().unwrap();
        client_write.shutdown().await.unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn malformed_stdio_frame_cancels_and_joins_an_admitted_operation() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let (server, backend) = server();
        let (client, host) = tokio::io::duplex(64 * 1024);
        let (host_read, host_write) = tokio::io::split(host);
        let serving = tokio::spawn(serve_stdio_transport(
            server,
            host_read,
            host_write,
            CancellationToken::new(),
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut lines = BufReader::new(client_read).lines();

        for (operation, method, params) in [
            (1, "initialize", serde_json::json!({})),
            (2, "session/open", serde_json::json!({})),
            (
                3,
                "turn/start",
                serde_json::json!({
                    "sessionId":"session-1","text":"permission","attachments":[]
                }),
            ),
        ] {
            let frame = serde_json::json!({
                "kind":"request",
                "operation":operation,
                "request":{"jsonrpc":"2.0","id":operation,"method":method,"params":params}
            });
            client_write
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
            lines.next_line().await.unwrap().unwrap();
        }
        assert!(backend.permission_waiting.load(Ordering::SeqCst));

        client_write
            .write_all(
                b"{\"kind\":\"request\",\"operation\":9,\"request\":{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"initialize\",\"params\":{}}}\n",
            )
            .await
            .unwrap();
        client_write.shutdown().await.unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), serving)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), AppServerErrorCode::InvalidRequest);
        assert!(!backend.permission_waiting.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn malformed_version_unknown_method_and_cancelled_turn_fail_with_stable_codes() {
        let (server, _backend) = server();
        let (events, _receiver) = mpsc::channel(1);
        let malformed = server
            .request("not-json", events.clone(), CancellationToken::new())
            .await;
        let malformed: Value = serde_json::from_str(&malformed).unwrap();
        assert_eq!(malformed["error"]["code"], -32602);
        let unknown = server
            .request(
                &serde_json::json!({
                    "jsonrpc":"2.0","id":7,"method":"unknown","params":{}
                })
                .to_string(),
                events,
                CancellationToken::new(),
            )
            .await;
        let unknown: Value = serde_json::from_str(&unknown).unwrap();
        assert_eq!(unknown["id"], 7);
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn configure_requires_an_explicitly_opened_session_without_mutating_the_backend() {
        let (server, backend) = server();
        let (events, _receiver) = mpsc::channel(1);
        let response = server
            .request(
                &serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":7,
                    "method":"session/configure",
                    "params":{
                        "sessionId":"session-1",
                        "configuration":{"model":"changed"}
                    }
                })
                .to_string(),
                events,
                CancellationToken::new(),
            )
            .await;
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["error"]["code"], -32001);
        assert_eq!(
            *backend.configuration.lock().unwrap(),
            AppRuntimeConfiguration::default()
        );
    }

    #[tokio::test]
    async fn configure_rejects_a_mismatched_session_id_before_mutating_the_backend() {
        let (server, backend) = server();
        let client = LocalAppClient::new(server.clone());
        let original = AppRuntimeConfiguration {
            model: Some("original".to_owned()),
            ..AppRuntimeConfiguration::default()
        };
        client
            .start_with_configuration(original.clone())
            .await
            .unwrap();

        let (events, _receiver) = mpsc::channel(1);
        let response = server
            .request(
                &serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":8,
                    "method":"session/configure",
                    "params":{
                        "sessionId":"another-session",
                        "configuration":{"model":"changed"}
                    }
                })
                .to_string(),
                events,
                CancellationToken::new(),
            )
            .await;
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(*backend.configuration.lock().unwrap(), original);
    }

    #[tokio::test]
    async fn committed_runtime_and_allowed_workspace_reach_the_next_opened_session() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let (server, _factory) = server_at(root.path());
        let delegated = fake_runtime("delegated", heycode_runtime::AgentRuntimeKind::Delegated);

        let committed = server
            .select_runtime_after(delegated, || Ok("committed"))
            .unwrap();
        assert_eq!(committed, "committed");
        let selected = server.select_workspace(&nested).unwrap();
        assert!(selected.selected);
        assert_eq!(selected.runtime, "delegated");
        assert_eq!(selected.cwd, std::fs::canonicalize(&nested).unwrap());

        let opened = LocalAppClient::new(server).open().await.unwrap();
        assert_eq!(opened.runtime_id, "delegated");
        assert_eq!(opened.cwd, selected.cwd);
    }

    #[tokio::test]
    async fn configuration_invalidation_detaches_only_the_receipted_backend_generation() {
        let root = tempfile::tempdir().unwrap();
        let (server, factory) = server_at(root.path());
        let delegated = fake_runtime("delegated", heycode_runtime::AgentRuntimeKind::Delegated);
        server.select_runtime_after(delegated, || Ok(())).unwrap();
        let affected = factory.latest();
        LocalAppClient::new(server.clone()).open().await.unwrap();
        let affected_generation = server.backend.read().unwrap().generation;

        server
            .invalidate_backend_configuration("delegated", affected_generation, None, None)
            .await
            .unwrap();
        assert!(affected.closed.load(Ordering::SeqCst));
        let replacement = factory.latest();
        assert!(!Arc::ptr_eq(&affected, &replacement));
        assert!(!replacement.closed.load(Ordering::SeqCst));
        assert!(!server.backend.read().unwrap().opened);

        server
            .invalidate_backend_configuration("delegated", affected_generation, None, None)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&factory.latest(), &replacement));
        assert!(!replacement.closed.load(Ordering::SeqCst));
        let reopened = LocalAppClient::new(server.clone()).open().await.unwrap();
        assert_eq!(reopened.runtime_id, "delegated");
        assert!(server.backend.read().unwrap().opened);
    }

    #[tokio::test]
    async fn failed_commit_active_turn_and_open_session_publish_no_runtime_change() {
        let root = tempfile::tempdir().unwrap();
        let (server, _factory) = server_at(root.path());
        let delegated = fake_runtime("delegated", heycode_runtime::AgentRuntimeKind::Delegated);
        let error = server
            .select_runtime_after(delegated.clone(), || {
                Err::<(), _>(AppServerError::unavailable())
            })
            .unwrap_err();
        assert_eq!(error.code(), AppServerErrorCode::Unavailable);

        let route_called = AtomicBool::new(false);
        let active = server.operation_gate.read().await;
        let error = server
            .commit_route(|| {
                route_called.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.code(), AppServerErrorCode::Conflict);
        assert!(!route_called.load(Ordering::SeqCst));
        drop(active);

        let client = LocalAppClient::new(server.clone());
        let opened = client.open().await.unwrap();
        assert_eq!(opened.runtime_id, "native");
        let error = server
            .select_runtime_after(delegated, || Ok(()))
            .unwrap_err();
        assert_eq!(error.code(), AppServerErrorCode::Conflict);
        assert_eq!(client.open().await.unwrap().runtime_id, "native");
    }

    #[test]
    fn workspace_selection_rejects_relative_traversing_outside_and_native_relocation() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (server, _factory) = server_at(root.path());

        for rejected in [
            PathBuf::from("relative"),
            root.path().join("nested/../nested"),
            outside.path().to_path_buf(),
        ] {
            let error = server.select_workspace(&rejected).unwrap_err();
            assert_eq!(error.code(), AppServerErrorCode::InvalidRequest);
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
            let error = server
                .select_workspace(&root.path().join("escape"))
                .unwrap_err();
            assert_eq!(error.code(), AppServerErrorCode::InvalidRequest);
        }
        let error = server.select_workspace(&nested).unwrap_err();
        assert_eq!(error.code(), AppServerErrorCode::Unsupported);
        let current = server.select_workspace(root.path()).unwrap();
        assert!(!current.selected);
        assert_eq!(current.runtime, "native");
    }

    /// Session whose events go through a real hub, so a turn long enough to
    /// pass `RUNTIME_EVENT_HISTORY` trims and renumbers exactly as production
    /// does. The first turn emits `deltas` commentary events; every later turn
    /// emits one.
    struct HubSession {
        id: RuntimeSessionId,
        runtime: heycode_runtime::AgentRuntimeId,
        capabilities: heycode_runtime::RuntimeCapabilities,
        hub: heycode_runtime::RuntimeEventHub,
        turns: AtomicU64,
        deltas: usize,
        /// Non-zero makes the first `subscribe` hand out a stream that errors
        /// after three items; later subscriptions are plain.
        break_first_subscription_after: AtomicU64,
        cancel_turn: Mutex<Option<heycode_runtime::RuntimeTurnId>>,
    }

    impl HubSession {
        fn new(deltas: usize) -> Arc<Self> {
            let session = Self {
                id: RuntimeSessionId::new("hub-session").unwrap(),
                runtime: heycode_runtime::AgentRuntimeId::new("native").unwrap(),
                capabilities: heycode_runtime::RuntimeCapabilities::unknown(),
                hub: heycode_runtime::RuntimeEventHub::new(),
                turns: AtomicU64::new(0),
                deltas,
                break_first_subscription_after: AtomicU64::new(0),
                cancel_turn: Mutex::new(None),
            };
            session.hub.emit(RuntimeEventKind::SessionReady).unwrap();
            Arc::new(session)
        }

        fn with_broken_first_subscription(deltas: usize) -> Arc<Self> {
            let session = Self::new(deltas);
            session
                .break_first_subscription_after
                .store(1, Ordering::SeqCst);
            session
        }
    }

    #[async_trait]
    impl RuntimeSession for HubSession {
        fn id(&self) -> &RuntimeSessionId {
            &self.id
        }

        fn runtime_id(&self) -> &heycode_runtime::AgentRuntimeId {
            &self.runtime
        }

        fn capabilities(&self) -> &heycode_runtime::RuntimeCapabilities {
            &self.capabilities
        }

        fn subscribe(&self) -> heycode_runtime::RuntimeEventStream {
            let stream = self.hub.subscribe();
            if self
                .break_first_subscription_after
                .swap(0, Ordering::SeqCst)
                == 0
            {
                return stream;
            }
            // The first subscription fails after its third item, modelling a
            // consumer-side break while the hub itself stays healthy.
            let mut remaining = 3_usize;
            Box::pin(stream.map(move |item| {
                if remaining == 0 {
                    return Err(heycode_runtime::RuntimeError::protocol());
                }
                remaining -= 1;
                item
            }))
        }

        async fn send(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<heycode_runtime::RuntimeTurnId, heycode_runtime::RuntimeError> {
            let index = self.turns.fetch_add(1, Ordering::SeqCst);
            let turn = heycode_runtime::RuntimeTurnId::new(format!("turn-{index}")).unwrap();
            self.hub
                .emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })?;
            let deltas = if index == 0 { self.deltas } else { 1 };
            for delta in 0..deltas {
                self.hub.emit(RuntimeEventKind::CommentaryDelta {
                    text: format!("{index}:{delta}"),
                })?;
            }
            self.hub.emit(RuntimeEventKind::FinalMessage {
                text: format!("final-{index}"),
            })?;
            self.hub.emit(RuntimeEventKind::TurnFinished {
                turn: turn.clone(),
                reason: RuntimeFinishReason::Stop,
            })?;
            Ok(turn)
        }

        async fn steer(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<(), heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn follow_up(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<(), heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn cancel(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<(), heycode_runtime::RuntimeError> {
            if let Some(turn) = self.cancel_turn.lock().await.take() {
                self.hub.emit(RuntimeEventKind::TurnFinished {
                    turn,
                    reason: RuntimeFinishReason::Cancelled,
                })?;
            }
            Ok(())
        }

        async fn respond_permission(
            &self,
            _response: heycode_runtime::RuntimePermissionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn respond_question(
            &self,
            _response: heycode_runtime::RuntimeQuestionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn compact(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<heycode_runtime::RuntimeCompactOutcome, heycode_runtime::RuntimeError> {
            Err(heycode_runtime::RuntimeError::unsupported())
        }

        async fn close(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<(), heycode_runtime::RuntimeError> {
            Ok(())
        }
    }

    fn event_label(event: &RuntimeEvent) -> String {
        match event.kind() {
            RuntimeEventKind::SessionReady => "session_ready".to_owned(),
            RuntimeEventKind::TurnStarted { .. } => "turn_started".to_owned(),
            RuntimeEventKind::CommentaryDelta { text } => format!("commentary:{text}"),
            RuntimeEventKind::FinalMessage { text } => format!("final:{text}"),
            RuntimeEventKind::TurnFinished { .. } => "turn_finished".to_owned(),
            other => format!("{other:?}"),
        }
    }

    async fn drain_turn(pump: &mut TurnPump<'_>) -> Vec<String> {
        let mut seen = Vec::new();
        while let Some(event) = pump.next().await.unwrap() {
            seen.push(event_label(&event));
        }
        seen
    }

    #[tokio::test(start_paused = true)]
    async fn accepted_turn_waits_for_human_and_terminal_event_without_idle_deadline() {
        let session = HubSession::new(0);
        let mut events = TurnEvents::default();
        let turn = heycode_runtime::RuntimeTurnId::new("delayed-turn").unwrap();
        session
            .hub
            .emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
            .unwrap();
        let mut pump = TurnPump {
            events: events.stream(session.as_ref()),
            send: Box::pin(std::future::ready(Ok(turn.clone()))),
            send_result: None,
            terminal: None,
            cancellation: CancellationToken::new(),
            session: session.as_ref(),
            cancellation_settled: false,
        };
        assert_eq!(
            event_label(&pump.next().await.unwrap().unwrap()),
            "session_ready"
        );
        assert_eq!(
            event_label(&pump.next().await.unwrap().unwrap()),
            "turn_started"
        );
        let delayed_finish = async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            session
                .hub
                .emit(RuntimeEventKind::FinalMessage {
                    text: "done".into(),
                })
                .unwrap();
            session
                .hub
                .emit(RuntimeEventKind::TurnFinished {
                    turn,
                    reason: RuntimeFinishReason::Stop,
                })
                .unwrap();
        };
        let (event, ()) = tokio::join!(pump.next(), delayed_finish);
        assert_eq!(event_label(&event.unwrap().unwrap()), "final:done");
        assert_eq!(
            event_label(&pump.next().await.unwrap().unwrap()),
            "turn_finished"
        );
        assert!(pump.next().await.unwrap().is_none());
        assert_eq!(
            pump.settle().unwrap().terminal,
            Some(RuntimeFinishReason::Stop)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accepted_silent_turn_remains_cancellable_and_stream_failure_is_terminal() {
        let session = HubSession::new(0);
        let mut events = TurnEvents::default();
        let turn = heycode_runtime::RuntimeTurnId::new("cancel-turn").unwrap();
        session
            .hub
            .emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
            .unwrap();
        *session.cancel_turn.lock().await = Some(turn.clone());
        let cancellation = CancellationToken::new();
        let mut pump = TurnPump {
            events: events.stream(session.as_ref()),
            send: Box::pin(std::future::ready(Ok(turn))),
            send_result: None,
            terminal: None,
            cancellation: cancellation.clone(),
            session: session.as_ref(),
            cancellation_settled: false,
        };
        assert_eq!(
            event_label(&pump.next().await.unwrap().unwrap()),
            "session_ready"
        );
        assert_eq!(
            event_label(&pump.next().await.unwrap().unwrap()),
            "turn_started"
        );
        let cancel = async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            cancellation.cancel();
        };
        let (result, ()) = tokio::join!(pump.next(), cancel);
        assert!(matches!(
            result.unwrap().unwrap().kind(),
            RuntimeEventKind::TurnFinished {
                reason: RuntimeFinishReason::Cancelled,
                ..
            }
        ));
        assert!(pump.next().await.unwrap().is_none());
        assert_eq!(
            pump.settle().unwrap().terminal,
            Some(RuntimeFinishReason::Cancelled)
        );

        let mut pump = TurnPump::new(
            session.as_ref(),
            events.stream(session.as_ref()),
            RuntimeInput::new("failure").unwrap(),
            CancellationToken::new(),
        );
        session
            .hub
            .fail(heycode_runtime::RuntimeError::unavailable());
        assert_eq!(
            pump.next().await.unwrap_err().code(),
            AppServerErrorCode::Unavailable
        );
    }

    #[tokio::test]
    async fn a_turn_after_the_retained_event_window_still_delivers_its_own_events() {
        let session: Arc<dyn RuntimeSession> =
            HubSession::new(2 * heycode_runtime::RUNTIME_EVENT_HISTORY);
        let mut events = TurnEvents::default();
        let mut first = TurnPump::new(
            session.as_ref(),
            events.stream(session.as_ref()),
            RuntimeInput::new("first").unwrap(),
            CancellationToken::new(),
        );
        let flooded = drain_turn(&mut first).await;
        let first = first.settle().unwrap();
        assert_eq!(first.terminal, Some(RuntimeFinishReason::Stop));
        assert!(flooded.len() > heycode_runtime::RUNTIME_EVENT_HISTORY);

        let mut second = TurnPump::new(
            session.as_ref(),
            events.stream(session.as_ref()),
            RuntimeInput::new("second").unwrap(),
            CancellationToken::new(),
        );
        let seen = drain_turn(&mut second).await;
        let second = second.settle().unwrap();
        assert_eq!(second.terminal, Some(RuntimeFinishReason::Stop));
        assert_eq!(
            seen,
            vec![
                "turn_started".to_owned(),
                "commentary:1:0".to_owned(),
                "final:final-1".to_owned(),
                "turn_finished".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn direct_native_command_history_cannot_finish_the_next_appserver_turn() {
        for reset in [false, true] {
            let session = HubSession::new(1);
            let mut events = TurnEvents::default();
            let mut first = TurnPump::new(
                session.as_ref(),
                events.stream(session.as_ref()),
                RuntimeInput::new("first").unwrap(),
                CancellationToken::new(),
            );
            drain_turn(&mut first).await;
            first.settle().unwrap();

            // A direct /skill completed without an AppServer pump consuming
            // the native runtime's copy of its session events.
            let command_turn = heycode_runtime::RuntimeTurnId::from_native_turn(12);
            session
                .hub
                .emit(RuntimeEventKind::TurnStarted {
                    turn: command_turn.clone(),
                })
                .unwrap();
            session
                .hub
                .emit(RuntimeEventKind::CommentaryDelta {
                    text: "stale skill answer".into(),
                })
                .unwrap();
            session
                .hub
                .emit(RuntimeEventKind::FinalMessage {
                    text: "stale skill final".into(),
                })
                .unwrap();
            session
                .hub
                .emit(RuntimeEventKind::TurnFinished {
                    turn: command_turn,
                    reason: RuntimeFinishReason::Stop,
                })
                .unwrap();
            if reset {
                events.reset();
            }
            events.skip_native_history(&[heycode_session::SessionEvent {
                v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                seq: 1,
                time_ms: 1,
                kind: heycode_session::SessionEventKind::TurnStart { turn: 12 },
            }]);
            let mut second = TurnPump::new(
                session.as_ref(),
                events.stream(session.as_ref()),
                RuntimeInput::new("after skill").unwrap(),
                CancellationToken::new(),
            );
            let seen =
                tokio::time::timeout(std::time::Duration::from_secs(2), drain_turn(&mut second))
                    .await
                    .unwrap();
            assert_eq!(
                seen,
                [
                    "turn_started",
                    "commentary:1:0",
                    "final:final-1",
                    "turn_finished"
                ]
            );
            assert_eq!(
                second.settle().unwrap().terminal,
                Some(RuntimeFinishReason::Stop)
            );
        }
    }

    /// A subscription that breaks mid-turn ends only that turn. After the
    /// backend resets it, the next turn resubscribes, skips the replayed
    /// history of earlier turns and projects exactly its own events.
    #[tokio::test]
    async fn a_broken_subscription_ends_one_turn_and_the_next_turn_sees_only_itself() {
        let session: Arc<dyn RuntimeSession> = HubSession::with_broken_first_subscription(4);
        let mut events = TurnEvents::default();
        let mut first = TurnPump::new(
            session.as_ref(),
            events.stream(session.as_ref()),
            RuntimeInput::new("first").unwrap(),
            CancellationToken::new(),
        );
        let mut failed = false;
        loop {
            match first.next().await {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) => {
                    assert_eq!(error.code(), AppServerErrorCode::Unavailable);
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed, "the first subscription must break mid-turn");
        drop(first);
        events.reset();

        let mut second = TurnPump::new(
            session.as_ref(),
            events.stream(session.as_ref()),
            RuntimeInput::new("second").unwrap(),
            CancellationToken::new(),
        );
        let seen = drain_turn(&mut second).await;
        let second = second.settle().unwrap();
        assert_eq!(second.terminal, Some(RuntimeFinishReason::Stop));
        assert_eq!(
            seen,
            vec![
                "turn_started".to_owned(),
                "commentary:1:0".to_owned(),
                "final:final-1".to_owned(),
                "turn_finished".to_owned()
            ],
            "replayed events of the failed first turn must not leak into the second"
        );
    }

    #[test]
    fn runtime_errors_keep_their_cause_as_detail_and_reach_the_wire() {
        let runtime_error = heycode_runtime::RuntimeError::try_new(
            RuntimeErrorCode::Unavailable,
            "Codex CLI is unavailable",
        )
        .unwrap();
        let mapped = map_runtime_error(runtime_error);
        assert_eq!(mapped.code(), AppServerErrorCode::Unavailable);
        assert_eq!(mapped.detail(), Some("Codex CLI is unavailable"));
        let wire = serde_json::to_value(RpcResponse::error(Value::from(3), &mapped)).unwrap();
        assert_eq!(wire["error"]["code"], -32002);
        assert_eq!(wire["error"]["message"], "app-server is unavailable");
        assert_eq!(wire["error"]["data"]["detail"], "Codex CLI is unavailable");
        let plain = serde_json::to_value(RpcResponse::error(
            Value::from(4),
            &AppServerError::invalid(),
        ))
        .unwrap();
        assert!(
            plain["error"].get("data").is_none(),
            "detail-free errors keep the v1 shape"
        );
    }

    #[test]
    fn event_debug_and_wire_do_not_lose_untrusted_or_document_metadata() {
        let event = AppServerEvent::ToolFinished {
            call_id: "call-1".to_owned(),
            name: "web_fetch".to_owned(),
            result: serde_json::json!("external"),
            ok: true,
            untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
        };
        let wire = serde_json::to_vec(&event).unwrap();
        let decoded: AppServerEvent = serde_json::from_slice(&wire).unwrap();
        assert_eq!(decoded, event);
    }
}

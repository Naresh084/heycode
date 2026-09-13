//! R09 — Claude Code primary-runtime sessions.
//!
//! One session owns one long-lived `claude --print --input-format stream-json
//! --output-format stream-json` process, its driver task and one lifecycle
//! child token for the session's whole life. Turns, steering, follow-up,
//! interruption, compaction and permission/question callbacks all ride that
//! single NDJSON stream in both directions.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{StreamExt, stream::FuturesUnordered};
use heycode_exec::{ManagedProcess, ProcessInput, ProcessLines};
use heycode_llm::CapabilitySupport;
use heycode_runtime::{
    RuntimeCapabilities, RuntimeCompactOutcome, RuntimeError, RuntimeEventHub, RuntimeEventKind,
    RuntimeEventStream, RuntimeFinishReason, RuntimeInput, RuntimePermissionDecision,
    RuntimePermissionResponse, RuntimeQuestionResponse, RuntimeRequestId, RuntimeSession,
    RuntimeSessionId, RuntimeToolCall, RuntimeToolExecutor, RuntimeTurnId,
};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use crate::wire::{
    AssistantBlock, CONTROL_CANCEL_REQUEST, CONTROL_REQUEST, CONTROL_RESPONSE, ClaudeFrame,
    ClaudeMessage, DeltaKind, FrameError, MAX_DETAIL_BYTES, SUBTYPE_CAN_USE_TOOL,
    SUBTYPE_INTERRUPT, SUBTYPE_REQUEST_USER_DIALOG, parse_frame,
};

/// Most simultaneously pending human requests before the session fails closed.
const MAX_PENDING_HUMAN_REQUESTS: usize = 64;
/// Official Agent SDK control handshake required before long-lived input.
const SUBTYPE_INITIALIZE: &str = "initialize";
const SUBTYPE_MCP_MESSAGE: &str = "mcp_message";
const HEYCODE_MCP_SERVER: &str = "heycode";
/// Slash command that asks the CLI to compact its own context.
const COMPACT_COMMAND: &str = "/compact";

type ControlResponse = oneshot::Sender<Result<Value, String>>;
type ControlWaiterMap = BTreeMap<String, ControlResponse>;
type ControlWaiters = Arc<Mutex<ControlWaiterMap>>;

/// What kind of human answer one pending control request expects.
#[derive(Debug, Clone)]
enum PendingKind {
    /// A `can_use_tool` prompt answered with allow/deny.
    Permission,
    /// A `request_user_dialog` prompt answered with a chosen option.
    Question,
}

#[derive(Debug, Clone)]
struct PendingRequest {
    /// The CLI's own `request_id`, echoed verbatim in the response.
    upstream: String,
    kind: PendingKind,
    /// Exact input returned unchanged when a permission is allowed.
    permission_input: Option<Value>,
}

#[derive(Default)]
struct SessionState {
    /// Turn currently accepting deltas, if any.
    active_turn: Option<RuntimeTurnId>,
    /// The active turn was explicitly interrupted by the host.
    cancel_requested: bool,
    /// Main-thread input occupancy reported by the latest `message_start`.
    latest_request_context: Option<(String, u64)>,
    /// Human requests awaiting an answer, keyed by heycode-facing id.
    pending: BTreeMap<RuntimeRequestId, PendingRequest>,
    /// Monotonic source for heycode-facing request ids.
    next_request: u64,
    /// Monotonic source for host-originated control request ids.
    next_control: u64,
    /// Incremented by every observed `compact_boundary`.
    compact_generation: u64,
    /// Terminal failure that settled the driver, if any.
    failure: Option<RuntimeError>,
    /// Claude model tool-use ids owned by the SDK-hosted heycode MCP bridge.
    hosted_tool_uses: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ClaudeAdvertisedModel {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) resolved_model: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) reasoning_efforts: Vec<String>,
}

impl ClaudeAdvertisedModel {
    pub(crate) fn configuration(&self) -> heycode_runtime::RuntimeModelConfiguration {
        self.configuration_for(&self.id)
    }

    fn configuration_for(&self, control_model: &str) -> heycode_runtime::RuntimeModelConfiguration {
        heycode_runtime::RuntimeModelConfiguration {
            model: control_model.to_owned(),
            display_name: self.display_name.clone(),
            resolved_model: self.resolved_model.clone(),
            description: self.description.clone(),
            // Claude's initialize model rows currently express large-context
            // variants in labels/ids, not as a structured numeric field.
            context_window: None,
            default_reasoning_effort: None,
            reasoning_efforts: self.reasoning_efforts.clone(),
        }
    }
}

/// Preserve an already accepted bare Claude selector when initialize now
/// advertises only one bracket-qualified variant of it. The provider row is
/// reused only when the match is unique; multiple variants remain ambiguous.
pub(crate) fn advertised_model_configurations(
    rows: &[ClaudeAdvertisedModel],
) -> Vec<heycode_runtime::RuntimeModelConfiguration> {
    let mut configurations = rows
        .iter()
        .map(ClaudeAdvertisedModel::configuration)
        .collect::<Vec<_>>();
    let mut aliases = BTreeMap::<String, Option<&ClaudeAdvertisedModel>>::new();
    for row in rows {
        let Some((bare, suffix)) = row.id.split_once('[') else {
            continue;
        };
        if bare.is_empty() || !suffix.ends_with(']') || rows.iter().any(|row| row.id == bare) {
            continue;
        }
        aliases
            .entry(bare.to_owned())
            .and_modify(|candidate| *candidate = None)
            .or_insert(Some(row));
    }
    configurations.extend(
        aliases
            .into_iter()
            .filter_map(|(alias, row)| row.map(|row| row.configuration_for(&alias))),
    );
    configurations
}

/// One live Claude Code primary session.
pub(crate) struct ClaudeSession {
    id: RuntimeSessionId,
    runtime_id: heycode_runtime::AgentRuntimeId,
    capabilities: RuntimeCapabilities,
    input: Arc<tokio::sync::Mutex<Option<ProcessInput>>>,
    process: Mutex<Option<ManagedProcess>>,
    driver: Mutex<Option<tokio::task::JoinHandle<()>>>,
    state: Arc<Mutex<SessionState>>,
    changed: Arc<Notify>,
    events: Arc<RuntimeEventHub>,
    lifecycle: CancellationToken,
    /// Serializes turn-opening and compaction so neither can begin while the
    /// other is in flight.
    operation_gate: tokio::sync::Mutex<()>,
    /// Replies to host-originated control requests, keyed by request id.
    control: ControlWaiters,
    configuration: Mutex<heycode_runtime::RuntimeConfiguration>,
    models: Mutex<Vec<ClaudeAdvertisedModel>>,
    mcp_calls: Arc<Mutex<BTreeMap<String, CancellationToken>>>,
}

/// Everything `start`/`resume`/`fork` share once the process exists.
pub(crate) struct SessionParts {
    pub(crate) process: ManagedProcess,
    pub(crate) input: ProcessInput,
    pub(crate) lines: ProcessLines,
    pub(crate) lifecycle: CancellationToken,
    pub(crate) runtime_id: heycode_runtime::AgentRuntimeId,
    /// Provider-native id this session must observe on `system/init`; `None`
    /// accepts whatever the CLI reports.
    pub(crate) expected_session: Option<String>,
    /// New sessions know their host-minted id but the current CLI emits init
    /// only after its first user frame, so they must not block start on init.
    pub(crate) lazy_init: bool,
    pub(crate) configuration: heycode_runtime::RuntimeConfiguration,
    pub(crate) tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
}

/// Capabilities every Claude primary session advertises.
pub(crate) const fn session_capabilities() -> RuntimeCapabilities {
    let supported = CapabilitySupport::Supported;
    RuntimeCapabilities {
        // R09 owns sessions only; runtime-native model discovery is separate.
        models: CapabilitySupport::Unsupported,
        resume: supported,
        fork: supported,
        steer: supported,
        follow_up: supported,
        permissions: supported,
        questions: supported,
        compaction: supported,
    }
}

/// Runtime-level operations, including the initialize-probed model catalog.
pub(crate) const fn runtime_capabilities() -> RuntimeCapabilities {
    let mut capabilities = session_capabilities();
    capabilities.models = CapabilitySupport::Supported;
    capabilities
}

impl ClaudeSession {
    /// Drive the process to `system/init`, then publish a ready session.
    pub(crate) async fn open(
        parts: SessionParts,
        cancellation: CancellationToken,
    ) -> Result<Arc<Self>, RuntimeError> {
        let SessionParts {
            process,
            input,
            mut lines,
            lifecycle,
            runtime_id,
            expected_session,
            lazy_init,
            configuration,
            tool_executor,
        } = parts;
        let session_id = if lazy_init {
            match expected_session.clone() {
                Some(expected) => expected,
                None => {
                    lifecycle.cancel();
                    settle_process(process, input).await;
                    return Err(RuntimeError::protocol());
                }
            }
        } else {
            let init = read_init(&mut lines, &lifecycle, &cancellation).await;
            let (session_id, _model) = match init {
                Ok(found) => found,
                Err(error) => {
                    lifecycle.cancel();
                    settle_process(process, input).await;
                    return Err(error);
                }
            };
            if let Some(expected) = expected_session.as_deref()
                && expected != session_id
            {
                lifecycle.cancel();
                settle_process(process, input).await;
                return Err(RuntimeError::protocol());
            }
            session_id
        };
        let id = match RuntimeSessionId::new(session_id.clone()) {
            Ok(id) => id,
            Err(_) => {
                lifecycle.cancel();
                settle_process(process, input).await;
                return Err(RuntimeError::protocol());
            }
        };
        let state = Arc::new(Mutex::new(SessionState::default()));
        let changed = Arc::new(Notify::new());
        let events = Arc::new(RuntimeEventHub::new());
        let control = Arc::new(Mutex::new(BTreeMap::new()));
        let input = Arc::new(tokio::sync::Mutex::new(Some(input)));
        let mcp_calls = Arc::new(Mutex::new(BTreeMap::new()));
        let session = Arc::new(Self {
            id,
            runtime_id,
            capabilities: session_capabilities(),
            input: Arc::clone(&input),
            process: Mutex::new(Some(process)),
            driver: Mutex::new(None),
            state: state.clone(),
            changed: changed.clone(),
            events: events.clone(),
            lifecycle: lifecycle.clone(),
            operation_gate: tokio::sync::Mutex::new(()),
            control: control.clone(),
            configuration: Mutex::new(configuration.clone()),
            models: Mutex::new(Vec::new()),
            mcp_calls: Arc::clone(&mcp_calls),
        });
        let driver = tokio::spawn(drive(
            lines,
            session_id,
            state,
            changed,
            events.clone(),
            control,
            input,
            configuration.tools().to_vec(),
            tool_executor,
            mcp_calls,
            lifecycle,
        ));
        if let Ok(mut slot) = session.driver.lock() {
            *slot = Some(driver);
        }
        let mut initialize = Map::new();
        initialize.insert("hooks".to_owned(), json!({}));
        if !configuration.tools().is_empty() {
            initialize.insert("sdkMcpServers".to_owned(), json!([HEYCODE_MCP_SERVER]));
        }
        let initialized = session
            .control_request(SUBTYPE_INITIALIZE, initialize, &cancellation)
            .await;
        let models = match initialized {
            Ok(response) => match parse_initialize_models(&response) {
                Ok(models) => models,
                Err(error) => {
                    let _ = session.shutdown().await;
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = session.shutdown().await;
                return Err(error);
            }
        };
        if let Ok(mut catalog) = session.models.lock() {
            *catalog = models;
        } else {
            let _ = session.shutdown().await;
            return Err(RuntimeError::internal("claude model catalog"));
        }
        if let Err(error) = events.emit(RuntimeEventKind::SessionReady) {
            let _ = session.shutdown().await;
            return Err(error);
        }
        Ok(session)
    }

    fn check(&self, cancellation: &CancellationToken) -> Result<(), RuntimeError> {
        if let Some(failure) = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("claude session state"))?
            .failure
            .clone()
        {
            return Err(failure);
        }
        if self.lifecycle.is_cancelled() || cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Ok(())
    }

    fn active_turn(&self) -> Result<Option<RuntimeTurnId>, RuntimeError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("claude session state"))?
            .active_turn
            .clone())
    }

    pub(crate) fn advertised_models(&self) -> Result<Vec<ClaudeAdvertisedModel>, RuntimeError> {
        self.models
            .lock()
            .map(|models| models.clone())
            .map_err(|_| RuntimeError::internal("claude model catalog"))
    }

    async fn write(&self, frame: &Value) -> Result<(), RuntimeError> {
        let line = serde_json::to_string(frame).map_err(|_| RuntimeError::protocol())?;
        let mut guard = self.input.lock().await;
        let input = guard.as_mut().ok_or_else(RuntimeError::cancelled)?;
        input
            .write_line(&line)
            .await
            .map_err(|_| RuntimeError::unavailable())
    }

    /// Write one user frame; `query` false appends without starting a turn.
    async fn write_user(&self, text: &str, _uuid: &str, query: bool) -> Result<(), RuntimeError> {
        let mut frame = Map::new();
        frame.insert("type".to_owned(), json!("user"));
        // Match the official Agent SDK transport exactly. The CLI owns and
        // overwrites session identity; a nonempty/replayed identity or a
        // string shorthand can stay queued until stdin closes in current
        // long-lived stream-json mode.
        frame.insert("session_id".to_owned(), json!(""));
        frame.insert("parent_tool_use_id".to_owned(), Value::Null);
        frame.insert(
            "message".to_owned(),
            json!({"role":"user","content":[{"type":"text","text":text}]}),
        );
        if !query {
            frame.insert("shouldQuery".to_owned(), json!(false));
        }
        self.write(&Value::Object(frame)).await
    }

    /// Send one control request and wait for its correlated response.
    async fn control_request(
        &self,
        subtype: &str,
        extra: Map<String, Value>,
        cancellation: &CancellationToken,
    ) -> Result<Value, RuntimeError> {
        let request_id = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| RuntimeError::internal("claude control state"))?;
            let value = state.next_control;
            state.next_control = state
                .next_control
                .checked_add(1)
                .ok_or_else(RuntimeError::protocol)?;
            format!("heycode-control-{value}")
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut control = self
                .control
                .lock()
                .map_err(|_| RuntimeError::internal("claude control table"))?;
            control.insert(request_id.clone(), sender);
        }
        let mut request = extra;
        request.insert("subtype".to_owned(), json!(subtype));
        let frame = json!({
            "type": CONTROL_REQUEST,
            "request_id": request_id,
            "request": Value::Object(request),
        });
        // A failed write must not leave a waiter that can never settle.
        if let Err(error) = self.write(&frame).await {
            self.forget_control(&request_id);
            return Err(error);
        }
        let outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                self.forget_control(&request_id);
                // Tell the peer we no longer need the answer, exactly as the
                // protocol's cancel frame documents.
                let _sent = self
                    .write(&json!({"type": CONTROL_CANCEL_REQUEST, "request_id": request_id}))
                    .await;
                return Err(RuntimeError::cancelled());
            }
            () = self.lifecycle.cancelled() => {
                self.forget_control(&request_id);
                return Err(RuntimeError::cancelled());
            }
            received = receiver => received,
        };
        match outcome {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_message)) => Err(RuntimeError::protocol()),
            Err(_dropped) => Err(RuntimeError::protocol()),
        }
    }

    fn forget_control(&self, request_id: &str) {
        if let Ok(mut control) = self.control.lock() {
            control.remove(request_id);
        }
    }

    /// Answer one pending CLI-originated control request.
    async fn respond(
        &self,
        request_id: &RuntimeRequestId,
        expected: &PendingKind,
        mut response: Value,
    ) -> Result<(), RuntimeError> {
        let pending = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| RuntimeError::internal("claude pending state"))?;
            let entry = state.pending.get(request_id).cloned();
            match (&entry, expected) {
                (
                    Some(PendingRequest {
                        kind: PendingKind::Permission,
                        ..
                    }),
                    PendingKind::Permission,
                )
                | (
                    Some(PendingRequest {
                        kind: PendingKind::Question,
                        ..
                    }),
                    PendingKind::Question,
                ) => {}
                _ => return Err(RuntimeError::conflict()),
            }
            state.pending.remove(request_id);
            entry.ok_or_else(RuntimeError::conflict)?
        };
        if matches!(expected, PendingKind::Permission)
            && response.get("behavior").and_then(Value::as_str) == Some("allow")
        {
            let input = pending
                .permission_input
                .clone()
                .ok_or_else(RuntimeError::protocol)?;
            response
                .as_object_mut()
                .ok_or_else(RuntimeError::protocol)?
                .insert("updatedInput".to_owned(), input);
        }
        self.write(&json!({
            "type": CONTROL_RESPONSE,
            "response": {
                "subtype": "success",
                "request_id": pending.upstream,
                "response": response,
            }
        }))
        .await
    }

    async fn wait_turn_settled(
        &self,
        turn: &RuntimeTurnId,
        cancellation: &CancellationToken,
    ) -> Result<(), RuntimeError> {
        loop {
            if self.active_turn()?.as_ref() != Some(turn) {
                return Ok(());
            }
            self.check(cancellation)?;
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
                () = self.lifecycle.cancelled() => return Err(RuntimeError::cancelled()),
                () = self.changed.notified() => {}
            }
        }
    }

    async fn shutdown(&self) -> Result<(), RuntimeError> {
        self.lifecycle.cancel();
        let input = { self.input.lock().await.take() };
        if let Some(input) = input {
            let _settled = input.finish().await;
        }
        let driver = self.driver.lock().ok().and_then(|mut slot| slot.take());
        if let Some(driver) = driver {
            let _joined = driver.await;
        }
        let process = self.process.lock().ok().and_then(|mut slot| slot.take());
        if let Some(process) = process {
            let _settled = process.cancel().await;
        }
        Ok(())
    }
}

#[async_trait]
impl RuntimeSession for ClaudeSession {
    fn id(&self) -> &RuntimeSessionId {
        &self.id
    }

    fn runtime_id(&self) -> &heycode_runtime::AgentRuntimeId {
        &self.runtime_id
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    fn model_configuration(
        &self,
        model: &str,
    ) -> Option<heycode_runtime::RuntimeModelConfiguration> {
        advertised_model_configurations(&self.models.lock().ok()?)
            .into_iter()
            .find(|row| row.model == model)
    }

    fn subscribe(&self) -> RuntimeEventStream {
        self.events.subscribe()
    }

    async fn send(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        let _gate = self.operation_gate.lock().await;
        self.check(&cancellation)?;
        if !input.attachments().is_empty() {
            return Err(RuntimeError::unsupported());
        }
        if self.active_turn()?.is_some() {
            return Err(RuntimeError::conflict());
        }
        // The CLI has no synchronous turn ack, so the host-minted uuid is the
        // correlation handle: it comes back as `user_message_uuid` and names
        // the turn in an interrupt receipt.
        let uuid = uuid_v4();
        let turn = RuntimeTurnId::new(uuid.clone()).map_err(|_| RuntimeError::protocol())?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| RuntimeError::internal("claude session state"))?;
            state.active_turn = Some(turn.clone());
            state.cancel_requested = false;
            state.latest_request_context = None;
        }
        if let Err(error) = self
            .events
            .emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
        {
            if let Ok(mut state) = self.state.lock() {
                state.active_turn = None;
                state.cancel_requested = false;
                state.latest_request_context = None;
            }
            self.changed.notify_waiters();
            return Err(error);
        }
        if let Err(error) = self.write_user(input.text(), &uuid, true).await {
            if let Ok(mut state) = self.state.lock() {
                state.active_turn = None;
                state.cancel_requested = false;
                state.latest_request_context = None;
            }
            self.events.emit(RuntimeEventKind::TurnFinished {
                turn,
                reason: RuntimeFinishReason::Error,
            })?;
            self.changed.notify_waiters();
            return Err(error);
        }
        Ok(turn)
    }

    async fn configure(
        &self,
        configuration: heycode_runtime::RuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<heycode_runtime::RuntimeConfiguration, RuntimeError> {
        let mut unsupported = Vec::new();
        if configuration.system_prompt().is_some() {
            unsupported.push("system_prompt");
        }
        if configuration.tools_configured() {
            unsupported.push("tools");
        }
        if !unsupported.is_empty() {
            return Err(RuntimeError::unsupported_field_names(&unsupported));
        }
        self.check(&cancellation)?;
        if self.active_turn()?.is_some() {
            return Err(RuntimeError::conflict());
        }
        let _gate = self.operation_gate.lock().await;
        self.check(&cancellation)?;
        if self.active_turn()?.is_some() {
            return Err(RuntimeError::conflict());
        }
        if configuration.is_empty() {
            return self
                .configuration
                .lock()
                .map(|current| current.clone())
                .map_err(|_| RuntimeError::internal("claude session configuration"));
        }
        let current = self
            .configuration
            .lock()
            .map_err(|_| RuntimeError::internal("claude session configuration"))?
            .clone();
        let effective = current.merged_with(&configuration);
        if let Some(model) = configuration.model() {
            let mut request = Map::new();
            request.insert("model".to_owned(), json!(model));
            if let Err(error) = self
                .control_request("set_model", request, &cancellation)
                .await
            {
                // A written control may have taken effect even when its reply
                // was cancelled or malformed. Retire the session so a failed
                // atomic update can never leak into a later turn.
                let _settled = self.shutdown().await;
                return Err(error);
            }
        }
        if let Some(effort) = configuration.reasoning_effort() {
            let mut request = Map::new();
            request.insert("settings".to_owned(), json!({"effortLevel":effort}));
            if let Err(error) = self
                .control_request("apply_flag_settings", request, &cancellation)
                .await
            {
                // Model and effort are separate official SDK controls. If the
                // second one fails after the first succeeded, closing is the
                // only fail-closed way to preserve configure's atomic contract.
                let _settled = self.shutdown().await;
                return Err(error);
            }
        }
        *self
            .configuration
            .lock()
            .map_err(|_| RuntimeError::internal("claude session configuration"))? =
            effective.clone();
        Ok(effective)
    }

    async fn steer(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        if !input.attachments().is_empty() {
            return Err(RuntimeError::unsupported());
        }
        // Steering is only meaningful against a running turn; the CLI would
        // otherwise silently start a new one.
        if self.active_turn()?.is_none() {
            return Err(RuntimeError::conflict());
        }
        self.write_user(input.text(), &uuid_v4(), true).await
    }

    async fn follow_up(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        if !input.attachments().is_empty() {
            return Err(RuntimeError::unsupported());
        }
        // `shouldQuery:false` appends to the transcript without starting a
        // turn; the text merges into the next real turn.
        self.write_user(input.text(), &uuid_v4(), false).await
    }

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        let Some(turn) = self.active_turn()? else {
            return Ok(());
        };
        self.state
            .lock()
            .map_err(|_| RuntimeError::internal("claude session state"))?
            .cancel_requested = true;
        if let Ok(calls) = self.mcp_calls.lock() {
            for call in calls.values() {
                call.cancel();
            }
        }
        let mut extra = Map::new();
        extra.insert("cancel_queued".to_owned(), json!(true));
        self.control_request(SUBTYPE_INTERRUPT, extra, &cancellation)
            .await?;
        self.wait_turn_settled(&turn, &cancellation).await
    }

    async fn respond_permission(
        &self,
        response: RuntimePermissionResponse,
        _cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        // Both allow decisions map to a bare allow. Claude's `updatedPermissions`
        // path writes a persistent rule into the project's settings file, which
        // outlives the session and is strictly broader authority than heycode's
        // "allow for this session" means — so it is never sent.
        let payload = match response.decision() {
            RuntimePermissionDecision::AllowOnce | RuntimePermissionDecision::AllowSession => {
                json!({"behavior":"allow"})
            }
            RuntimePermissionDecision::Deny => {
                json!({"behavior":"deny","message":"Denied by the heycode operator."})
            }
        };
        self.respond(response.request_id(), &PendingKind::Permission, payload)
            .await
    }

    async fn respond_question(
        &self,
        response: RuntimeQuestionResponse,
        _cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if response
            .selected_answers()
            .is_some_and(|labels| labels.len() != 1)
        {
            return Err(RuntimeError::unsupported());
        }
        let answer = response
            .answer()
            .or_else(|| {
                response
                    .selected_answers()
                    .and_then(|labels| labels.first().map(String::as_str))
            })
            .unwrap_or("Cancelled");
        self.respond(
            response.request_id(),
            &PendingKind::Question,
            json!({"response": answer}),
        )
        .await
    }

    async fn compact(
        &self,
        cancellation: CancellationToken,
    ) -> Result<RuntimeCompactOutcome, RuntimeError> {
        let _gate = self.operation_gate.lock().await;
        self.check(&cancellation)?;
        if self.active_turn()?.is_some() {
            return Err(RuntimeError::conflict());
        }
        let baseline = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("claude compact state"))?
            .compact_generation;
        let uuid = uuid_v4();
        let turn = RuntimeTurnId::new(uuid.clone()).map_err(|_| RuntimeError::protocol())?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| RuntimeError::internal("claude session state"))?;
            state.active_turn = Some(turn.clone());
            state.cancel_requested = false;
            state.latest_request_context = None;
        }
        if let Err(error) = self
            .events
            .emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
        {
            if let Ok(mut state) = self.state.lock() {
                state.active_turn = None;
                state.cancel_requested = false;
                state.latest_request_context = None;
            }
            self.changed.notify_waiters();
            return Err(error);
        }
        if let Err(error) = self.write_user(COMPACT_COMMAND, &uuid, true).await {
            if let Ok(mut state) = self.state.lock() {
                state.active_turn = None;
                state.cancel_requested = false;
                state.latest_request_context = None;
            }
            self.events.emit(RuntimeEventKind::TurnFinished {
                turn,
                reason: RuntimeFinishReason::Error,
            })?;
            self.changed.notify_waiters();
            return Err(error);
        }
        self.wait_turn_settled(&turn, &cancellation).await?;
        let applied = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("claude compact state"))?
            .compact_generation
            > baseline;
        // Only an observed `compact_boundary` proves compaction happened; the
        // command settling is not itself evidence.
        Ok(if applied {
            RuntimeCompactOutcome::Applied
        } else {
            RuntimeCompactOutcome::Noop
        })
    }

    async fn close(&self, _cancellation: CancellationToken) -> Result<(), RuntimeError> {
        self.shutdown().await
    }
}

async fn read_init(
    lines: &mut ProcessLines,
    lifecycle: &CancellationToken,
    cancellation: &CancellationToken,
) -> Result<(String, Option<String>), RuntimeError> {
    loop {
        let line = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
            () = lifecycle.cancelled() => return Err(RuntimeError::cancelled()),
            line = lines.next_line() => line,
        };
        let line = line.map_err(|_| RuntimeError::unavailable())?;
        // EOF before init means the CLI refused to start this session.
        let Some(line) = line else {
            return Err(RuntimeError::protocol());
        };
        match parse_frame(&line, None) {
            Ok(ClaudeFrame::Message(ClaudeMessage::Init { session_id, model })) => {
                return Ok((session_id, model));
            }
            // Hook and plugin-install frames legitimately precede init, so a
            // strict "init must be first" reader would break real sessions.
            Ok(_) => {}
            Err(FrameError::Oversized | FrameError::Malformed | FrameError::ForeignSession) => {
                return Err(RuntimeError::protocol());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    mut lines: ProcessLines,
    session_id: String,
    state: Arc<Mutex<SessionState>>,
    changed: Arc<Notify>,
    events: Arc<RuntimeEventHub>,
    control: ControlWaiters,
    input: Arc<tokio::sync::Mutex<Option<ProcessInput>>>,
    tools: Vec<heycode_core::ToolSpec>,
    tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
    mcp_calls: Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    lifecycle: CancellationToken,
) {
    // Keep the reader responsive while a hosted tool waits for human input.
    // These futures are owned by this driver, never detached. EOF, vendor
    // cancellation and session close must still reach the active call.
    let mut pending =
        FuturesUnordered::<futures::future::BoxFuture<'static, Result<(), RuntimeError>>>::new();
    let failure = loop {
        let line = tokio::select! {
            biased;
            () = lifecycle.cancelled() => break None,
            result = pending.next(), if !pending.is_empty() => {
                if let Some(Err(error)) = result {
                    break Some(error);
                }
                continue;
            }
            line = lines.next_line() => line,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) | Err(_) => break Some(RuntimeError::unavailable()),
        };
        let frame = match parse_frame(&line, Some(&session_id)) {
            Ok(frame) => frame,
            Err(_) => break Some(RuntimeError::protocol()),
        };
        if let ClaudeFrame::ControlRequest {
            request_id,
            subtype,
            request,
        } = &frame
            && subtype == SUBTYPE_MCP_MESSAGE
        {
            if pending.len() >= MAX_PENDING_HUMAN_REQUESTS {
                break Some(RuntimeError::protocol());
            }
            let (request_id, request) = (request_id.clone(), request.clone());
            let (input, events, tools, executor, calls, cancellation) = (
                input.clone(),
                events.clone(),
                tools.clone(),
                tool_executor.clone(),
                mcp_calls.clone(),
                lifecycle.clone(),
            );
            pending.push(Box::pin(async move {
                handle_mcp_message(
                    &request_id,
                    &request,
                    &input,
                    &events,
                    &tools,
                    executor.as_ref(),
                    &calls,
                    &cancellation,
                )
                .await
            }));
            continue;
        }
        if let Err(error) = handle_frame(
            frame,
            &state,
            &changed,
            &events,
            &control,
            &input,
            &tools,
            tool_executor.as_ref(),
            &mcp_calls,
            &lifecycle,
        )
        .await
        {
            break Some(error);
        }
    };
    if let Some(error) = failure
        && !lifecycle.is_cancelled()
    {
        settle_failure(&state, &changed, &events, error);
    }
    lifecycle.cancel();
    // Cooperative executors receive cancellation before their owning futures
    // are dropped. A broken executor cannot indefinitely prevent process reap.
    let _drained = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while pending.next().await.is_some() {}
    })
    .await;
}

fn settle_failure(
    state: &Mutex<SessionState>,
    changed: &Notify,
    events: &RuntimeEventHub,
    error: RuntimeError,
) {
    if let Ok(mut state) = state.lock() {
        if state.failure.is_none() {
            state.failure = Some(error.clone());
        }
        state.active_turn = None;
        state.cancel_requested = false;
        state.latest_request_context = None;
        state.pending.clear();
    }
    changed.notify_waiters();
    events.fail(error);
}

#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    frame: ClaudeFrame,
    state: &Mutex<SessionState>,
    changed: &Notify,
    events: &RuntimeEventHub,
    control: &Mutex<ControlWaiterMap>,
    input: &Arc<tokio::sync::Mutex<Option<ProcessInput>>>,
    tools: &[heycode_core::ToolSpec],
    tool_executor: Option<&Arc<dyn RuntimeToolExecutor>>,
    mcp_calls: &Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    lifecycle: &CancellationToken,
) -> Result<(), RuntimeError> {
    match frame {
        ClaudeFrame::Ignored => Ok(()),
        ClaudeFrame::ControlResponse {
            request_id,
            response,
        } => {
            let sender = control
                .lock()
                .map_err(|_| RuntimeError::internal("claude control table"))?
                .remove(&request_id);
            if let Some(sender) = sender {
                let _delivered = sender.send(response);
            }
            // A response for an id we are no longer waiting on is explicitly
            // ignorable per the protocol.
            Ok(())
        }
        ClaudeFrame::ControlCancel { request_id } => {
            if let Some(call) = mcp_calls
                .lock()
                .map_err(|_| RuntimeError::internal("claude MCP call table"))?
                .remove(&request_id)
            {
                call.cancel();
            }
            let sender = control
                .lock()
                .map_err(|_| RuntimeError::internal("claude control table"))?
                .remove(&request_id);
            if let Some(sender) = sender {
                let _delivered = sender.send(Err("withdrawn".to_owned()));
            }
            Ok(())
        }
        ClaudeFrame::ControlRequest {
            request_id,
            subtype,
            request,
        } if subtype == SUBTYPE_MCP_MESSAGE => {
            handle_mcp_message(
                &request_id,
                &request,
                input,
                events,
                tools,
                tool_executor,
                mcp_calls,
                lifecycle,
            )
            .await
        }
        ClaudeFrame::ControlRequest {
            request_id,
            subtype,
            request,
        } => handle_control_request(&request_id, &subtype, &request, state, events),
        ClaudeFrame::Message(message) => handle_message(message, state, changed, events),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_mcp_message(
    request_id: &str,
    request: &Map<String, Value>,
    input: &Arc<tokio::sync::Mutex<Option<ProcessInput>>>,
    events: &RuntimeEventHub,
    tools: &[heycode_core::ToolSpec],
    tool_executor: Option<&Arc<dyn RuntimeToolExecutor>>,
    mcp_calls: &Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    lifecycle: &CancellationToken,
) -> Result<(), RuntimeError> {
    if request.get("server_name").and_then(Value::as_str) != Some(HEYCODE_MCP_SERVER)
        || tools.is_empty()
    {
        return Err(RuntimeError::protocol());
    }
    let message = request
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RuntimeError::protocol());
    }
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    let id = message.get("id").cloned();
    let response = match method {
        "initialize" => {
            let id = id.ok_or_else(RuntimeError::protocol)?;
            json!({
                "jsonrpc":"2.0", "id":id,
                "result":{
                    "protocolVersion":"2025-11-25",
                    "capabilities":{"tools":{"listChanged":false}},
                    "serverInfo":{"name":"heycode","version":env!("CARGO_PKG_VERSION")}
                }
            })
        }
        // The official Agent SDK acknowledges inbound MCP notifications with
        // one dummy response id rather than omitting `id` altogether.
        "notifications/initialized" => json!({"jsonrpc":"2.0","id":0,"result":{}}),
        "tools/list" => {
            let id = id.ok_or_else(RuntimeError::protocol)?;
            let rows = tools
                .iter()
                .map(|tool| {
                    json!({
                        "name":tool.name,
                        "description":tool.description,
                        "inputSchema":tool.parameters,
                    })
                })
                .collect::<Vec<_>>();
            json!({"jsonrpc":"2.0","id":id,"result":{"tools":rows}})
        }
        "tools/call" => {
            let id = id.ok_or_else(RuntimeError::protocol)?;
            handle_mcp_tool_call(
                id,
                message.get("params").and_then(Value::as_object),
                request_id,
                events,
                tools,
                tool_executor,
                mcp_calls,
                lifecycle,
            )
            .await?
        }
        _ => match id {
            Some(id) => json!({
                "jsonrpc":"2.0", "id":id,
                "error":{"code":-32601,"message":"MCP method is unsupported"}
            }),
            None => json!({"jsonrpc":"2.0","id":0,"result":{}}),
        },
    };
    write_shared(
        input,
        &json!({
            "type":CONTROL_RESPONSE,
            "response":{
                "subtype":"success",
                "request_id":request_id,
                "response":{"mcp_response":response}
            }
        }),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn handle_mcp_tool_call(
    id: Value,
    params: Option<&Map<String, Value>>,
    request_id: &str,
    events: &RuntimeEventHub,
    tools: &[heycode_core::ToolSpec],
    tool_executor: Option<&Arc<dyn RuntimeToolExecutor>>,
    mcp_calls: &Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    lifecycle: &CancellationToken,
) -> Result<Value, RuntimeError> {
    let Some(params) = params else {
        return Ok(mcp_error(id, -32602, "MCP tool parameters are invalid"));
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Ok(mcp_error(id, -32602, "MCP tool parameters are invalid"));
    };
    if !tools.iter().any(|tool| tool.name == name) {
        return Ok(mcp_error(id, -32602, "MCP tool is not available"));
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if !arguments.is_object() {
        return Ok(mcp_error(id, -32602, "MCP tool parameters are invalid"));
    }
    let Some(executor) = tool_executor else {
        return Ok(mcp_error(id, -32603, "MCP tool executor is unavailable"));
    };

    // JSON-RPC ids need only be unique while a request is pending and Claude
    // may reuse one later in the same turn. Runtime call ids are session-wide,
    // so mint an independent correlation id.
    let call_id = heycode_core::CallId::from_raw(format!("claude-mcp-{}", uuid_v4()));
    let call_cancellation = lifecycle.child_token();
    {
        let mut calls = mcp_calls
            .lock()
            .map_err(|_| RuntimeError::internal("claude MCP call table"))?;
        if calls.contains_key(request_id) {
            return Ok(mcp_error(id, -32600, "MCP request is already pending"));
        }
        calls.insert(request_id.to_owned(), call_cancellation.clone());
    }
    events.emit(RuntimeEventKind::ToolCall {
        call_id: call_id.clone(),
        name: name.to_owned(),
        arguments: arguments.clone(),
    })?;
    let output = executor
        .execute(
            RuntimeToolCall {
                call_id: call_id.clone(),
                name: name.to_owned(),
                arguments,
            },
            call_cancellation,
        )
        .await;
    mcp_calls
        .lock()
        .map_err(|_| RuntimeError::internal("claude MCP call table"))?
        .remove(request_id);

    let (content, is_error) = match output {
        Ok(output) => (output.content, output.is_error),
        Err(_) => ("host tool execution failed".to_owned(), true),
    };
    events.emit(RuntimeEventKind::ToolResult {
        call_id,
        result: Value::String(content.clone()),
        is_error,
    })?;
    Ok(json!({
        "jsonrpc":"2.0", "id":id,
        "result":{
            "content":[{"type":"text","text":bounded_model_output(&content)}],
            "isError":is_error
        }
    }))
}

fn mcp_error(id: Value, code: i32, message: &'static str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

async fn write_shared(
    input: &Arc<tokio::sync::Mutex<Option<ProcessInput>>>,
    frame: &Value,
) -> Result<(), RuntimeError> {
    let line = serde_json::to_string(frame).map_err(|_| RuntimeError::protocol())?;
    let mut input = input.lock().await;
    input
        .as_mut()
        .ok_or_else(RuntimeError::cancelled)?
        .write_line(&line)
        .await
        .map_err(|_| RuntimeError::unavailable())
}

fn handle_control_request(
    request_id: &str,
    subtype: &str,
    request: &Map<String, Value>,
    state: &Mutex<SessionState>,
    events: &RuntimeEventHub,
) -> Result<(), RuntimeError> {
    let (kind, event, permission_input) = match subtype {
        SUBTYPE_CAN_USE_TOOL => {
            let name = request
                .get("tool_name")
                .and_then(Value::as_str)
                .ok_or_else(RuntimeError::protocol)?;
            let input = request
                .get("input")
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(RuntimeError::protocol)?;
            let input = Value::Object(input);
            let rendered_input =
                serde_json::to_string(&input).map_err(|_| RuntimeError::protocol())?;
            let reason = request
                .get("decision_reason")
                .or_else(|| request.get("blocked_path"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let detail = if reason.is_empty() {
                format!("input: {rendered_input}")
            } else {
                format!(
                    "{}; input: {rendered_input}",
                    reason.replace(['\n', '\r'], " ")
                )
            };
            if detail.len() > MAX_DETAIL_BYTES {
                // The operator and approval policy must never authorize input
                // whose decisive tail was hidden by UI truncation.
                return Err(RuntimeError::protocol());
            }
            (
                PendingKind::Permission,
                RuntimeEventKind::PermissionRequested {
                    request_id: RuntimeRequestId::new("placeholder")
                        .map_err(|_| RuntimeError::protocol())?,
                    action: format!("Claude tool `{}`", bounded(name)),
                    detail,
                },
                Some(input),
            )
        }
        SUBTYPE_REQUEST_USER_DIALOG => {
            validate_dialog_shape(request)?;
            let header = request.get("header").and_then(Value::as_str).map(bounded);
            let prompt = request
                .get("message")
                .or_else(|| request.get("question"))
                .and_then(Value::as_str)
                .map(bounded)
                .ok_or_else(RuntimeError::protocol)?;
            let (choices, choice_descriptions) = request
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    let mut choices = Vec::with_capacity(options.len());
                    let mut descriptions = Vec::with_capacity(options.len());
                    for option in options {
                        if let Some(label) = option.as_str() {
                            choices.push(bounded(label));
                            descriptions.push(None);
                            continue;
                        }
                        let object = option.as_object().ok_or_else(RuntimeError::protocol)?;
                        let label = object
                            .get("label")
                            .and_then(Value::as_str)
                            .ok_or_else(RuntimeError::protocol)?;
                        let description = object
                            .get("description")
                            .map(|value| {
                                value
                                    .as_str()
                                    .map(bounded)
                                    .ok_or_else(RuntimeError::protocol)
                            })
                            .transpose()?;
                        choices.push(bounded(label));
                        descriptions.push(description);
                    }
                    Ok((choices, descriptions))
                })
                .transpose()?
                .unwrap_or_default();
            (
                PendingKind::Question,
                RuntimeEventKind::QuestionRequested {
                    mode: if choices.is_empty() {
                        heycode_core::QuestionMode::FreeText
                    } else {
                        heycode_core::QuestionMode::SingleChoice
                    },
                    progress: (1, 1),
                    request_id: RuntimeRequestId::new("placeholder")
                        .map_err(|_| RuntimeError::protocol())?,
                    header,
                    prompt,
                    choices,
                    choice_descriptions,
                },
                None,
            )
        }
        // Every other CLI-originated request — notably credential and OAuth
        // token refresh — is never serviced. Answering one would end
        // credential-blindness, and silence would hang the runtime, so the
        // session fails loudly instead.
        _ => return Err(RuntimeError::unsupported()),
    };
    let runtime_id = {
        let mut state = state
            .lock()
            .map_err(|_| RuntimeError::internal("claude pending state"))?;
        if state.pending.len() >= MAX_PENDING_HUMAN_REQUESTS {
            return Err(RuntimeError::protocol());
        }
        let value = state.next_request;
        state.next_request = state
            .next_request
            .checked_add(1)
            .ok_or_else(RuntimeError::protocol)?;
        let runtime_id = RuntimeRequestId::new(format!("claude-request-{value}"))
            .map_err(|_| RuntimeError::protocol())?;
        // Redelivered ids are documented; an already-pending upstream id must
        // not create a second heycode-facing request.
        if state
            .pending
            .values()
            .any(|pending| pending.upstream == request_id)
        {
            return Ok(());
        }
        state.pending.insert(
            runtime_id.clone(),
            PendingRequest {
                upstream: request_id.to_owned(),
                kind,
                permission_input,
            },
        );
        runtime_id
    };
    let event = match event {
        RuntimeEventKind::PermissionRequested { action, detail, .. } => {
            RuntimeEventKind::PermissionRequested {
                request_id: runtime_id,
                action,
                detail,
            }
        }
        RuntimeEventKind::QuestionRequested {
            mode,
            progress,
            header,
            prompt,
            choices,
            choice_descriptions,
            ..
        } => RuntimeEventKind::QuestionRequested {
            mode,
            progress,
            request_id: runtime_id,
            header,
            prompt,
            choices,
            choice_descriptions,
        },
        other => other,
    };
    events.emit(event)
}

fn validate_dialog_shape(request: &Map<String, Value>) -> Result<(), RuntimeError> {
    // The pinned dialog protocol accepts one scalar response. A batch or
    // multi-selection must not silently become one text answer.
    if request.contains_key("questions") {
        return Err(RuntimeError::unsupported());
    }
    for key in ["multiSelect", "multi_select", "multiple"] {
        if let Some(value) = request.get(key)
            && value.as_bool().ok_or_else(RuntimeError::protocol)?
        {
            return Err(RuntimeError::unsupported());
        }
    }
    if let Some(mode) = request.get("mode")
        && !matches!(mode.as_str(), Some("single_choice" | "free_text"))
    {
        return Err(RuntimeError::unsupported());
    }
    if request
        .get("options")
        .is_some_and(|options| !options.is_null() && !options.is_array())
    {
        return Err(RuntimeError::protocol());
    }
    Ok(())
}

fn handle_message(
    message: ClaudeMessage,
    state: &Mutex<SessionState>,
    changed: &Notify,
    events: &RuntimeEventHub,
) -> Result<(), RuntimeError> {
    match message {
        // A second init on the same stream would mean the CLI re-initialized
        // under us; identity was already validated at the frame boundary.
        ClaudeMessage::Init { .. } => Ok(()),
        ClaudeMessage::Notice { code } => events.emit(RuntimeEventKind::Notice {
            code: code.to_owned(),
            message: "Claude reported transient API retry progress.".to_owned(),
        }),
        ClaudeMessage::CompactBoundary { trigger } => {
            {
                let mut state = state
                    .lock()
                    .map_err(|_| RuntimeError::internal("claude compact state"))?;
                state.compact_generation = state.compact_generation.saturating_add(1);
            }
            changed.notify_waiters();
            events.emit(RuntimeEventKind::Notice {
                code: format!("claude.compacted.{trigger}"),
                message: "Claude session context was compacted.".to_owned(),
            })
        }
        ClaudeMessage::Delta { kind, text } => events.emit(match kind {
            DeltaKind::Text => RuntimeEventKind::CommentaryDelta { text },
            DeltaKind::Thinking => RuntimeEventKind::ReasoningDelta { text },
        }),
        ClaudeMessage::MessageStart {
            model,
            input_tokens,
        } => {
            let mut state = state
                .lock()
                .map_err(|_| RuntimeError::internal("claude context state"))?;
            if state.active_turn.is_none() {
                return Err(RuntimeError::protocol());
            }
            state.latest_request_context = Some((model, input_tokens));
            Ok(())
        }
        // A subagent frame's tool ids belong to a nested context, so emitting
        // them here would correlate a nested call against this session's
        // results and desynchronize the turn.
        ClaudeMessage::Assistant { subagent: true, .. } => Ok(()),
        ClaudeMessage::Assistant { blocks, .. } => {
            for AssistantBlock::ToolUse {
                id,
                name,
                arguments,
            } in blocks
            {
                if name.starts_with("mcp__heycode__") {
                    state
                        .lock()
                        .map_err(|_| RuntimeError::internal("claude hosted tool state"))?
                        .hosted_tool_uses
                        .insert(id);
                    continue;
                }
                events.emit(RuntimeEventKind::ToolCall {
                    call_id: heycode_core::CallId::from_raw(id),
                    name,
                    arguments,
                })?;
            }
            Ok(())
        }
        ClaudeMessage::ToolResults { results } => {
            for result in results {
                if state
                    .lock()
                    .map_err(|_| RuntimeError::internal("claude hosted tool state"))?
                    .hosted_tool_uses
                    .remove(&result.tool_use_id)
                {
                    continue;
                }
                events.emit(RuntimeEventKind::ToolResult {
                    call_id: heycode_core::CallId::from_raw(result.tool_use_id),
                    result: Value::String(result.text),
                    is_error: result.is_error,
                })?;
            }
            Ok(())
        }
        ClaudeMessage::Result {
            subtype,
            is_error,
            text,
            usage,
            model_context_windows,
        } => {
            let (turn, cancel_requested, request_context) = {
                let mut state = state
                    .lock()
                    .map_err(|_| RuntimeError::internal("claude session state"))?;
                let turn = state.active_turn.take();
                let cancel_requested = state.cancel_requested;
                state.cancel_requested = false;
                let request_context = state.latest_request_context.take();
                (turn, cancel_requested, request_context)
            };
            let Some(turn) = turn else {
                // A settlement with no open turn means correlation was lost.
                return Err(RuntimeError::protocol());
            };
            let reason = match subtype.as_str() {
                _ if cancel_requested => RuntimeFinishReason::Cancelled,
                _ if is_error => RuntimeFinishReason::Error,
                "success" => RuntimeFinishReason::Stop,
                "error_max_turns" | "error_max_budget_usd" => RuntimeFinishReason::Limit,
                _ => RuntimeFinishReason::Error,
            };
            // The CLI settles a `success` result with no text whenever it goes
            // idle without an answer. That is still a stopped turn, and R02
            // requires a stopped turn to publish its final message, so the
            // empty text is published rather than dropped.
            let text = text.unwrap_or_default();
            if reason == RuntimeFinishReason::Stop || !text.is_empty() {
                events.emit(RuntimeEventKind::FinalMessage { text })?;
            }
            let context = exact_context_usage(request_context, &model_context_windows);
            if usage.is_some() || context.is_some() {
                events.emit(RuntimeEventKind::Usage {
                    usage: usage.unwrap_or(heycode_core::TokenUsage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                    }),
                    context,
                })?;
            }
            events.emit(RuntimeEventKind::TurnFinished { turn, reason })?;
            changed.notify_waiters();
            Ok(())
        }
    }
}

fn exact_context_usage(
    request: Option<(String, u64)>,
    windows: &[crate::wire::ClaudeModelContextWindow],
) -> Option<heycode_runtime::RuntimeContextUsage> {
    let (model, tokens) = request?;
    let mut matches = windows
        .iter()
        .filter(|row| row.model == model || row.canonical_model.as_deref() == Some(model.as_str()));
    let row = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(heycode_runtime::RuntimeContextUsage {
        resolved_model: Some(row.model.clone()),
        tokens,
        context_window: row.context_window,
    })
}

async fn settle_process(process: ManagedProcess, input: ProcessInput) {
    let _settled = input.finish().await;
    let _reaped = process.cancel().await;
}

fn parse_initialize_models(response: &Value) -> Result<Vec<ClaudeAdvertisedModel>, RuntimeError> {
    let Some(rows) = response.get("models") else {
        return Ok(Vec::new());
    };
    let rows = rows
        .as_array()
        .filter(|rows| rows.len() <= 256)
        .ok_or_else(RuntimeError::protocol)?;
    let mut seen = HashSet::new();
    let mut models = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row.as_object().ok_or_else(RuntimeError::protocol)?;
        let id = safe_catalog_text(row.get("value"), 256)?;
        if !seen.insert(id.clone()) {
            return Err(RuntimeError::protocol());
        }
        let display_name = match row.get("displayName") {
            Some(value) => safe_catalog_text(Some(value), 256)?,
            None => id.clone(),
        };
        let resolved_model = row
            .get("resolvedModel")
            .map(|value| safe_catalog_text(Some(value), 256))
            .transpose()?;
        let description = row
            .get("description")
            .map(|value| safe_catalog_text(Some(value), 512))
            .transpose()?;
        let efforts = match row.get("supportedEffortLevels") {
            None => Vec::new(),
            Some(Value::Array(values)) if values.len() <= 16 => {
                let mut seen = HashSet::new();
                let mut efforts = Vec::with_capacity(values.len());
                for value in values {
                    let effort = safe_catalog_text(Some(value), 64)?;
                    if !seen.insert(effort.clone()) {
                        return Err(RuntimeError::protocol());
                    }
                    efforts.push(effort);
                }
                efforts
            }
            Some(_) => return Err(RuntimeError::protocol()),
        };
        models.push(ClaudeAdvertisedModel {
            id,
            display_name,
            resolved_model,
            description,
            reasoning_efforts: efforts,
        });
    }
    Ok(models)
}

fn safe_catalog_text(value: Option<&Value>, maximum: usize) -> Result<String, RuntimeError> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    if value.is_empty()
        || value.trim() != value
        || value.len() > maximum
        || value.chars().any(char::is_control)
    {
        return Err(RuntimeError::protocol());
    }
    Ok(value.to_owned())
}

fn bounded(value: &str) -> String {
    let value = value.replace(['\n', '\r'], " ");
    if value.len() <= MAX_DETAIL_BYTES {
        return value;
    }
    let mut end = MAX_DETAIL_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn bounded_model_output(value: &str) -> String {
    if value.len() <= 256 * 1024 {
        return value.to_owned();
    }
    let mut end = 256 * 1024;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Random RFC 4122 version-4 UUID, the identity form `--session-id` requires.
pub(crate) fn uuid_v4() -> String {
    let mut bytes = [0_u8; 16];
    getrandom_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn getrandom_bytes(bytes: &mut [u8; 16]) {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut offset = 0;
    while offset < bytes.len() {
        let value = RandomState::new().build_hasher().finish().to_le_bytes();
        let take = value.len().min(bytes.len() - offset);
        bytes[offset..offset + take].copy_from_slice(&value[..take]);
        offset += take;
    }
}

/// Argv shared by start, resume and fork.
pub(crate) fn session_args(
    identity: SessionIdentity<'_>,
    configuration: &heycode_runtime::RuntimeConfiguration,
) -> Vec<OsString> {
    let mut args: Vec<String> = vec![
        "--print".to_owned(),
        "--input-format".to_owned(),
        "stream-json".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--verbose".to_owned(),
        "--include-partial-messages".to_owned(),
        // Keep settings, extensions and ambient MCP servers outside the hosted
        // boundary for every identity, including resume and fork.
        "--safe-mode".to_owned(),
        "--strict-mcp-config".to_owned(),
        "--mcp-config".to_owned(),
        r#"{"mcpServers":{}}"#.to_owned(),
        "--disable-slash-commands".to_owned(),
        "--no-chrome".to_owned(),
        // Manual mode forwards undecided vendor permissions to the SDK host.
        // Configured host tools run through the heycode MCP executor below.
        "--permission-mode".to_owned(),
        "manual".to_owned(),
        "--permission-prompts".to_owned(),
        "host".to_owned(),
        // This hidden CLI switch is what the official Agent SDK uses to bind
        // its can_use_tool callback to the stream-JSON control protocol.
        "--permission-prompt-tool".to_owned(),
        "stdio".to_owned(),
    ];
    if configuration.tools_configured() {
        // The exact host catalog is carried by sdkMcpServers. Disable Claude's
        // similarly named built-ins so tool execution cannot bypass the host
        // executor, approval policy, sandbox or durable session log.
        args.push("--tools".to_owned());
        args.push(String::new());
        if !configuration.tools().is_empty() {
            // Claude applies its own permission gate before an SDK MCP request
            // reaches the host. Pre-authorize only the exact configured MCP
            // names; the host executor remains the authoritative heycode approval
            // and sandbox boundary.
            let allowed = configuration
                .tools()
                .iter()
                .map(|tool| format!("mcp__{HEYCODE_MCP_SERVER}__{}", tool.name))
                .collect::<Vec<_>>()
                .join(",");
            args.push("--allowedTools".to_owned());
            args.push(allowed);
        }
    }
    match identity {
        SessionIdentity::Ephemeral { session_id } => {
            args.extend([
                "--no-session-persistence".to_owned(),
                "--session-id".to_owned(),
                session_id.to_owned(),
            ]);
        }
        SessionIdentity::New { session_id } => {
            args.push("--session-id".to_owned());
            args.push(session_id.to_owned());
        }
        SessionIdentity::Resume { session_id } => {
            args.push("--resume".to_owned());
            args.push(session_id.to_owned());
        }
        SessionIdentity::Fork { session_id } => {
            args.push("--resume".to_owned());
            args.push(session_id.to_owned());
            args.push("--fork-session".to_owned());
        }
    }
    if let Some(prompt) = configuration.system_prompt() {
        args.push("--system-prompt".to_owned());
        args.push(prompt.to_owned());
    }
    if let Some(model) = configuration.model() {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    if let Some(effort) = configuration.reasoning_effort() {
        args.push("--effort".to_owned());
        args.push(effort.to_owned());
    }
    args.into_iter().map(OsString::from).collect()
}

/// Which identity mode a session launch uses.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SessionIdentity<'a> {
    /// One print-mode process with no resumable transcript.
    Ephemeral { session_id: &'a str },
    /// Host-chosen UUID for a brand-new session.
    New { session_id: &'a str },
    /// Continue an existing provider-native session in place.
    Resume { session_id: &'a str },
    /// Continue an existing session under a fresh provider-native id.
    Fork { session_id: &'a str },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn dialog_rejects_unrepresentable_shapes_instead_of_flattening() {
        for value in [
            json!({"questions":[{"question":"one"},{"question":"two"}]}),
            json!({"message":"Choose", "multiSelect":true}),
            json!({"message":"Choose", "mode":"multiple_choice"}),
        ] {
            assert_eq!(
                validate_dialog_shape(value.as_object().unwrap())
                    .unwrap_err()
                    .code(),
                heycode_runtime::RuntimeErrorCode::Unsupported
            );
        }
        assert!(
            validate_dialog_shape(
                json!({"message":"Choose", "options":["One", "Two"]})
                    .as_object()
                    .unwrap()
            )
            .is_ok()
        );
        assert!(
            validate_dialog_shape(
                json!({"message":"Choose", "options":{"label":"One"}})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn generated_session_ids_are_version_four_uuids() {
        let value = uuid_v4();
        assert_eq!(value.len(), 36);
        let parts: Vec<&str> = value.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(value.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert!(parts[2].starts_with('4'), "version nibble must be 4");
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "variant nibble must be 8..b"
        );
        assert_ne!(value, uuid_v4(), "ids must not repeat");
    }

    #[test]
    fn identity_flags_match_the_pinned_cli_surface() {
        let render = |identity| {
            session_args(identity, &heycode_runtime::RuntimeConfiguration::new())
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let new = render(SessionIdentity::New { session_id: "u" });
        assert!(new.windows(2).any(|pair| pair == ["--session-id", "u"]));
        assert!(!new.iter().any(|arg| arg == "--resume"));
        for required in [
            "--safe-mode",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-chrome",
        ] {
            assert!(new.iter().any(|arg| arg == required), "missing {required}");
        }
        assert!(
            new.windows(2)
                .any(|pair| pair == ["--mcp-config", r#"{"mcpServers":{}}"#])
        );

        let ephemeral = render(SessionIdentity::Ephemeral { session_id: "e" });
        let required = "--no-session-persistence";
        assert!(
            ephemeral.iter().any(|arg| arg == required),
            "missing ephemeral {required}"
        );
        assert!(
            ephemeral
                .windows(2)
                .any(|pair| pair == ["--session-id", "e"])
        );
        assert!(
            !ephemeral
                .iter()
                .any(|arg| { matches!(arg.as_str(), "--resume" | "--fork-session") })
        );
        assert!(
            ephemeral
                .windows(2)
                .any(|pair| pair == ["--mcp-config", r#"{"mcpServers":{}}"#])
        );

        let resume = render(SessionIdentity::Resume { session_id: "u" });
        assert!(resume.windows(2).any(|pair| pair == ["--resume", "u"]));
        assert!(!resume.iter().any(|arg| arg == "--fork-session"));

        let fork = render(SessionIdentity::Fork { session_id: "u" });
        assert!(fork.windows(2).any(|pair| pair == ["--resume", "u"]));
        assert!(fork.iter().any(|arg| arg == "--fork-session"));

        // Streaming both ways plus partial messages is what makes deltas and
        // control frames possible at all.
        for required in [
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "--verbose",
            "--include-partial-messages",
        ] {
            assert!(new.iter().any(|arg| arg == required), "missing {required}");
        }
        // Undecided vendor permissions are forwarded to the SDK host.
        let mode = new
            .iter()
            .position(|arg| arg == "--permission-mode")
            .unwrap();
        assert_eq!(new[mode + 1], "manual");
        assert!(
            new.windows(2)
                .any(|pair| pair == ["--permission-prompts", "host"])
        );
        assert!(
            new.windows(2)
                .any(|pair| pair == ["--permission-prompt-tool", "stdio"])
        );

        let configuration = heycode_runtime::RuntimeConfiguration::new()
            .with_system_prompt("exact prompt")
            .unwrap()
            .with_tools(Vec::new())
            .unwrap()
            .with_model("opus")
            .unwrap()
            .with_reasoning_effort("high")
            .unwrap();
        let with_model = session_args(SessionIdentity::New { session_id: "u" }, &configuration);
        let rendered: Vec<String> = with_model
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.windows(2).any(|pair| pair == ["--model", "opus"]));
        assert!(
            rendered
                .windows(2)
                .any(|pair| pair == ["--system-prompt", "exact prompt"])
        );
        assert!(rendered.windows(2).any(|pair| pair == ["--effort", "high"]));
        assert!(rendered.windows(2).any(|pair| pair == ["--tools", ""]));

        let with_tool = heycode_runtime::RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "read".to_owned(),
                description: "Read a file".to_owned(),
                parameters: json!({"type":"object"}),
            }])
            .unwrap();
        let rendered = session_args(SessionIdentity::New { session_id: "u" }, &with_tool)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            rendered
                .windows(2)
                .any(|pair| pair == ["--allowedTools", "mcp__heycode__read"])
        );
    }

    #[test]
    fn session_capabilities_advertise_only_implemented_controls() {
        let capabilities = session_capabilities();
        for supported in [
            capabilities.resume,
            capabilities.fork,
            capabilities.steer,
            capabilities.follow_up,
            capabilities.permissions,
            capabilities.questions,
            capabilities.compaction,
        ] {
            assert_eq!(supported, CapabilitySupport::Supported);
        }
        assert_eq!(capabilities.models, CapabilitySupport::Unsupported);
    }

    #[test]
    fn initialize_catalog_preserves_provider_native_effort_values() {
        let models = parse_initialize_models(&json!({
            "models":[{
                "value":"opus[1m]",
                "resolvedModel":"claude-opus-future[1m]",
                "displayName":"Opus (1M context)",
                "description":"Provider supplied description",
                "supportedEffortLevels":["adaptive","provider-native"]
            }]
        }))
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "opus[1m]");
        assert_eq!(models[0].display_name, "Opus (1M context)");
        assert_eq!(
            models[0].resolved_model.as_deref(),
            Some("claude-opus-future[1m]")
        );
        assert_eq!(
            models[0].description.as_deref(),
            Some("Provider supplied description")
        );
        assert_eq!(models[0].reasoning_efforts, ["adaptive", "provider-native"]);
        let configuration = models[0].configuration();
        assert_eq!(configuration.model, "opus[1m]");
        assert_eq!(configuration.display_name, "Opus (1M context)");
        assert_eq!(configuration.context_window, None);
        assert_eq!(configuration.default_reasoning_effort, None);
    }

    #[test]
    fn one_bracketed_variant_preserves_an_already_accepted_bare_selector() {
        let rows = vec![ClaudeAdvertisedModel {
            id: "opus[1m]".to_owned(),
            display_name: "Opus (1M context)".to_owned(),
            resolved_model: Some("claude-opus-5[1m]".to_owned()),
            description: None,
            reasoning_efforts: vec!["low".to_owned(), "medium".to_owned()],
        }];

        let configurations = advertised_model_configurations(&rows);
        assert!(configurations.iter().any(|row| row.model == "opus[1m]"));
        let alias = configurations
            .iter()
            .find(|row| row.model == "opus")
            .unwrap();
        assert_eq!(alias.display_name, "Opus (1M context)");
        assert_eq!(alias.reasoning_efforts, ["low", "medium"]);

        let ambiguous = advertised_model_configurations(&[
            rows[0].clone(),
            ClaudeAdvertisedModel {
                id: "opus[extended]".to_owned(),
                ..rows[0].clone()
            },
        ]);
        assert!(!ambiguous.iter().any(|row| row.model == "opus"));

        let exact = advertised_model_configurations(&[
            rows[0].clone(),
            ClaudeAdvertisedModel {
                id: "opus".to_owned(),
                display_name: "Opus".to_owned(),
                ..rows[0].clone()
            },
        ]);
        assert_eq!(exact.iter().filter(|row| row.model == "opus").count(), 1);
        assert_eq!(
            exact
                .iter()
                .find(|row| row.model == "opus")
                .unwrap()
                .display_name,
            "Opus"
        );
    }

    #[test]
    fn exact_context_requires_one_matching_authoritative_window() {
        let canonical = crate::wire::ClaudeModelContextWindow {
            model: "claude-opus-5[1m]".to_owned(),
            canonical_model: Some("claude-opus-5".to_owned()),
            context_window: 1_000_000,
        };
        assert_eq!(
            exact_context_usage(
                Some(("claude-opus-5".to_owned(), 2_767)),
                std::slice::from_ref(&canonical),
            ),
            Some(heycode_runtime::RuntimeContextUsage {
                resolved_model: Some("claude-opus-5[1m]".to_owned()),
                tokens: 2_767,
                context_window: 1_000_000,
            })
        );

        let duplicate = crate::wire::ClaudeModelContextWindow {
            model: "claude-opus-5".to_owned(),
            canonical_model: None,
            context_window: 200_000,
        };
        assert_eq!(
            exact_context_usage(
                Some(("claude-opus-5".to_owned(), 2_767)),
                &[canonical, duplicate],
            ),
            None,
            "ambiguous matching rows must fail closed",
        );
    }
}

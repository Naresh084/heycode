//! Pinned primary Codex thread/session bridge.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::{CallId, TokenUsage};
use heycode_runtime::{
    AgentRuntime, RuntimeCapabilities, RuntimeCompactOutcome, RuntimeError, RuntimeEventHub,
    RuntimeEventKind, RuntimeEventStream, RuntimeFinishReason, RuntimeFork, RuntimeInput,
    RuntimePermissionDecision, RuntimePermissionResponse, RuntimeQuestionResponse,
    RuntimeRequestId, RuntimeResume, RuntimeSession, RuntimeSessionId, RuntimeStart,
    RuntimeToolCall, RuntimeToolExecutor, RuntimeTurnId,
};
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::runtime::runtime_error;
use crate::{
    CodexAppServerClient, CodexAppServerError, CodexAppServerErrorCode, CodexInboundEvent,
    CodexRequestId, CodexResponsePayload, CodexRuntime, CodexServerRequest,
};

const MAX_PENDING_HUMAN_REQUESTS: usize = 128;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_DETAIL_BYTES: usize = 4 * 1024;
const MAX_CHOICES: usize = 16;

fn validate_tool_bridge(
    configuration: &heycode_runtime::RuntimeConfiguration,
    executor: Option<&Arc<dyn RuntimeToolExecutor>>,
) -> Result<(), RuntimeError> {
    if !configuration.tools().is_empty() && executor.is_none() {
        Err(RuntimeError::invalid_request())
    } else {
        Ok(())
    }
}

fn apply_thread_configuration(
    params: &mut Value,
    configuration: &heycode_runtime::RuntimeConfiguration,
) -> Result<(), RuntimeError> {
    let object = params
        .as_object_mut()
        .ok_or_else(|| RuntimeError::internal("codex thread parameters"))?;
    if let Some(model) = configuration.model() {
        object.insert("model".to_owned(), Value::String(model.to_owned()));
    }
    if let Some(prompt) = configuration.system_prompt() {
        object.insert(
            "baseInstructions".to_owned(),
            Value::String(prompt.to_owned()),
        );
    }
    if let Some(effort) = configuration.reasoning_effort() {
        object.insert(
            "config".to_owned(),
            json!({"model_reasoning_effort":effort}),
        );
    }
    Ok(())
}

struct OpenSessionRequest<'a> {
    method: &'a str,
    params: Value,
    expected_thread: Option<&'a str>,
    expected_cwd: &'a str,
    configuration: &'a heycode_runtime::RuntimeConfiguration,
    tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
    cancellation: CancellationToken,
}

pub(crate) async fn start(
    runtime: &CodexRuntime,
    request: RuntimeStart,
    cancellation: CancellationToken,
) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
    let configuration = request.configuration().clone();
    let tool_executor = request.tool_executor().cloned();
    validate_tool_bridge(&configuration, tool_executor.as_ref())?;
    let cwd = utf8_workspace(request.workspace())?;
    let mut params = Map::new();
    params.insert("cwd".to_owned(), Value::String(cwd.to_owned()));
    params.insert("ephemeral".to_owned(), Value::Bool(request.ephemeral()));
    params.insert(
        "approvalPolicy".to_owned(),
        Value::String("on-request".to_owned()),
    );
    params.insert("sandbox".to_owned(), Value::String("read-only".to_owned()));
    if let Some(model) = configuration.model() {
        params.insert("model".to_owned(), Value::String(model.to_owned()));
    }
    if let Some(prompt) = configuration.system_prompt() {
        params.insert(
            "baseInstructions".to_owned(),
            Value::String(prompt.to_owned()),
        );
    }
    if let Some(effort) = configuration.reasoning_effort() {
        params.insert(
            "config".to_owned(),
            json!({"model_reasoning_effort":effort}),
        );
    }
    if configuration.tools_configured() {
        params.insert(
            "dynamicTools".to_owned(),
            Value::Array(
                configuration
                    .tools()
                    .iter()
                    .map(|tool| {
                        json!({
                            "type":"function",
                            "name":tool.name,
                            "description":tool.description,
                            "inputSchema":tool.parameters,
                        })
                    })
                    .collect(),
            ),
        );
    }
    open_session(
        runtime,
        OpenSessionRequest {
            method: "thread/start",
            params: Value::Object(params),
            expected_thread: None,
            expected_cwd: cwd,
            configuration: &configuration,
            tool_executor,
            cancellation,
        },
    )
    .await
}

pub(crate) async fn resume(
    runtime: &CodexRuntime,
    request: RuntimeResume,
    cancellation: CancellationToken,
) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
    let configuration = request.configuration().clone();
    let tool_executor = request.tool_executor().cloned();
    if configuration.tools_configured() {
        return Err(RuntimeError::unsupported_field_names(&["tools"]));
    }
    validate_tool_bridge(&configuration, tool_executor.as_ref())?;
    let cwd = utf8_workspace(request.workspace())?;
    let mut params = json!({
        "threadId":request.runtime_session_id().as_str(),
        "cwd":cwd,
        "approvalPolicy":"on-request",
        "sandbox":"read-only",
    });
    apply_thread_configuration(&mut params, &configuration)?;
    open_session(
        runtime,
        OpenSessionRequest {
            method: "thread/resume",
            params,
            expected_thread: Some(request.runtime_session_id().as_str()),
            expected_cwd: cwd,
            configuration: &configuration,
            tool_executor,
            cancellation,
        },
    )
    .await
}

pub(crate) async fn fork(
    runtime: &CodexRuntime,
    request: RuntimeFork,
    cancellation: CancellationToken,
) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
    let configuration = request.configuration().clone();
    let tool_executor = request.tool_executor().cloned();
    if configuration.tools_configured() {
        return Err(RuntimeError::unsupported_field_names(&["tools"]));
    }
    validate_tool_bridge(&configuration, tool_executor.as_ref())?;
    let cwd = utf8_workspace(request.workspace())?;
    let mut params = json!({
        "threadId":request.source_runtime_session_id().as_str(),
        "cwd":cwd,
        "ephemeral":false,
        "approvalPolicy":"on-request",
        "sandbox":"read-only",
    });
    apply_thread_configuration(&mut params, &configuration)?;
    let session = open_session(
        runtime,
        OpenSessionRequest {
            method: "thread/fork",
            params,
            expected_thread: None,
            expected_cwd: cwd,
            configuration: &configuration,
            tool_executor,
            cancellation,
        },
    )
    .await?;
    if session.id() == request.source_runtime_session_id() {
        session.close(CancellationToken::new()).await?;
        return Err(RuntimeError::protocol());
    }
    Ok(session)
}

async fn open_session(
    runtime: &CodexRuntime,
    request: OpenSessionRequest<'_>,
) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
    let OpenSessionRequest {
        method,
        params,
        expected_thread,
        expected_cwd,
        configuration,
        tool_executor,
        cancellation,
    } = request;
    runtime.ensure_operation(&cancellation)?;
    let client = runtime
        .connect_with_experimental(configuration.tools_configured(), cancellation.clone())
        .await
        .map_err(runtime_error)?;
    let result = client.request(method, params, cancellation.clone()).await;
    let payload = match result {
        Ok(payload) => payload,
        Err(error) => {
            let _settled = client.close(CancellationToken::new()).await;
            return Err(runtime_error(error));
        }
    };
    let thread = match parse_thread_response(
        payload,
        expected_thread,
        expected_cwd,
        configuration.model(),
        configuration.reasoning_effort(),
    ) {
        Ok(thread) => thread,
        Err(error) => {
            let _settled = client.close(CancellationToken::new()).await;
            return Err(runtime_error(error));
        }
    };
    if cancellation.is_cancelled() {
        let _settled = client.close(CancellationToken::new()).await;
        return Err(RuntimeError::cancelled());
    }
    CodexSession::spawn(
        client,
        thread,
        runtime.descriptor().id().clone(),
        runtime.descriptor().capabilities().clone(),
        configuration.clone(),
        tool_executor,
    )
}

fn parse_thread_response(
    payload: CodexResponsePayload,
    expected_thread: Option<&str>,
    expected_cwd: &str,
    expected_model: Option<&str>,
    expected_effort: Option<&str>,
) -> Result<RuntimeSessionId, CodexAppServerError> {
    let value = payload.into_value();
    let object = value.as_object().ok_or_else(protocol)?;
    let thread = object
        .get("thread")
        .and_then(Value::as_object)
        .ok_or_else(protocol)?;
    let id = required_id(thread, "id")?;
    if expected_thread.is_some_and(|expected| expected != id) {
        return Err(protocol());
    }
    let cwd = object
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| valid_text(value, MAX_DETAIL_BYTES, false))
        .ok_or_else(protocol)?;
    if Path::new(cwd) != Path::new(expected_cwd) {
        return Err(protocol());
    }
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| valid_id(value))
        .ok_or_else(protocol)?;
    if expected_model.is_some_and(|expected| expected != model) {
        return Err(protocol());
    }
    if let Some(expected) = expected_effort
        && object.get("reasoningEffort").and_then(Value::as_str) != Some(expected)
    {
        return Err(protocol());
    }
    RuntimeSessionId::new(id).map_err(|_| protocol())
}

struct CodexSession {
    id: RuntimeSessionId,
    runtime_id: heycode_runtime::AgentRuntimeId,
    capabilities: RuntimeCapabilities,
    client: CodexAppServerClient,
    lifecycle: CancellationToken,
    closed: AtomicBool,
    close_gate: AsyncMutex<()>,
    operation_gate: AsyncMutex<()>,
    response_gate: AsyncMutex<()>,
    state: Arc<Mutex<SessionState>>,
    changed: Arc<Notify>,
    events: Arc<RuntimeEventHub>,
    driver: Mutex<Option<JoinHandle<()>>>,
    configuration: AsyncMutex<heycode_runtime::RuntimeConfiguration>,
}

struct SessionState {
    active_turn: Option<RuntimeTurnId>,
    active_tool_cancellation: Option<CancellationToken>,
    turn_started_emitted: bool,
    final_text: Option<String>,
    pending: BTreeMap<RuntimeRequestId, PendingRequest>,
    next_request: u64,
    compact_generation: u64,
    compact_pending: bool,
    compact_turn: Option<RuntimeTurnId>,
}

struct CodexDriver {
    client: CodexAppServerClient,
    thread_id: String,
    state: Arc<Mutex<SessionState>>,
    changed: Arc<Notify>,
    events: Arc<RuntimeEventHub>,
    allowed_tools: HashSet<String>,
    tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
    lifecycle: CancellationToken,
}

#[derive(Clone)]
struct PendingRequest {
    upstream: CodexRequestId,
    kind: PendingKind,
}

#[derive(Clone)]
enum PendingKind {
    Command,
    FileChange,
    Permissions(Value),
    Question {
        questions: Vec<ProviderQuestion>,
        index: usize,
        answers: Map<String, Value>,
    },
}

#[derive(Clone)]
struct ProviderQuestion {
    id: String,
    header: Option<String>,
    prompt: String,
    choices: Vec<String>,
    descriptions: Vec<Option<String>>,
    mode: heycode_core::QuestionMode,
}

impl ProviderQuestion {
    fn event(&self, request_id: RuntimeRequestId, progress: (usize, usize)) -> RuntimeEventKind {
        RuntimeEventKind::QuestionRequested {
            request_id,
            header: self.header.clone(),
            prompt: self.prompt.clone(),
            choices: self.choices.clone(),
            choice_descriptions: self.descriptions.clone(),
            mode: self.mode,
            progress,
        }
    }
}

impl CodexSession {
    fn spawn(
        client: CodexAppServerClient,
        id: RuntimeSessionId,
        runtime_id: heycode_runtime::AgentRuntimeId,
        capabilities: RuntimeCapabilities,
        configuration: heycode_runtime::RuntimeConfiguration,
        tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        let lifecycle = CancellationToken::new();
        let events = Arc::new(RuntimeEventHub::new());
        events.emit(RuntimeEventKind::SessionReady)?;
        let allowed_tools = configuration
            .tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<HashSet<_>>();
        let state = Arc::new(Mutex::new(SessionState {
            active_turn: None,
            active_tool_cancellation: None,
            turn_started_emitted: false,
            final_text: None,
            pending: BTreeMap::new(),
            next_request: 1,
            compact_generation: 0,
            compact_pending: false,
            compact_turn: None,
        }));
        let changed = Arc::new(Notify::new());
        let session = Arc::new(Self {
            id,
            runtime_id,
            capabilities,
            client: client.clone(),
            lifecycle: lifecycle.clone(),
            closed: AtomicBool::new(false),
            close_gate: AsyncMutex::new(()),
            operation_gate: AsyncMutex::new(()),
            response_gate: AsyncMutex::new(()),
            state: state.clone(),
            changed: changed.clone(),
            events: events.clone(),
            driver: Mutex::new(None),
            configuration: AsyncMutex::new(configuration),
        });
        let thread_id = session.id.as_str().to_owned();
        let driver = tokio::spawn(async move {
            drive(CodexDriver {
                client,
                thread_id,
                state,
                changed,
                events,
                allowed_tools,
                tool_executor,
                lifecycle,
            })
            .await;
        });
        *session
            .driver
            .lock()
            .map_err(|_| RuntimeError::internal("codex driver owner"))? = Some(driver);
        Ok(session)
    }

    fn check(&self, cancellation: &CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else if self.closed.load(Ordering::Acquire) || self.lifecycle.is_cancelled() {
            Err(RuntimeError::closed())
        } else {
            Ok(())
        }
    }

    fn active_turn(&self) -> Result<Option<RuntimeTurnId>, RuntimeError> {
        self.state
            .lock()
            .map(|state| state.active_turn.clone())
            .map_err(|_| RuntimeError::internal("codex session state"))
    }

    fn admit_turn(&self, turn: RuntimeTurnId) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("codex session state"))?;
        match state.active_turn.as_ref() {
            Some(active) if active != &turn => return Err(RuntimeError::protocol()),
            Some(_) => {}
            None => {
                state.active_turn = Some(turn.clone());
                state.active_tool_cancellation = Some(self.lifecycle.child_token());
            }
        }
        if !state.turn_started_emitted {
            state.turn_started_emitted = true;
            drop(state);
            self.events.emit(RuntimeEventKind::TurnStarted { turn })?;
        }
        self.changed.notify_waiters();
        Ok(())
    }

    async fn wait_turn_settled(
        &self,
        expected: &RuntimeTurnId,
        cancellation: &CancellationToken,
    ) -> Result<(), RuntimeError> {
        loop {
            let changed = self.changed.notified();
            if self.active_turn()?.as_ref() != Some(expected) {
                return Ok(());
            }
            tokio::select! {
                () = changed => {}
                () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
                () = self.lifecycle.cancelled() => return Err(RuntimeError::closed()),
            }
        }
    }

    async fn respond(
        &self,
        request_id: &RuntimeRequestId,
        result: Value,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let _gate = self.response_gate.lock().await;
        self.check(&cancellation)?;
        let pending = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("codex pending request"))?
            .pending
            .get(request_id)
            .cloned()
            .ok_or_else(RuntimeError::conflict)?;
        self.client
            .respond_success(&pending.upstream, result, cancellation)
            .await
            .map_err(runtime_error)?;
        self.state
            .lock()
            .map_err(|_| RuntimeError::internal("codex pending request"))?
            .pending
            .remove(request_id);
        Ok(())
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<CodexResponsePayload, RuntimeError> {
        match self.client.request(method, params, cancellation).await {
            Ok(payload) => Ok(payload),
            Err(error) => {
                let code = error.code();
                let mapped = runtime_error(error);
                if matches!(
                    code,
                    CodexAppServerErrorCode::Cancelled
                        | CodexAppServerErrorCode::Process
                        | CodexAppServerErrorCode::Protocol
                        | CodexAppServerErrorCode::Closed
                ) {
                    let _settled = self.shutdown().await;
                }
                Err(mapped)
            }
        }
    }

    async fn shutdown(&self) -> Result<(), RuntimeError> {
        let _gate = self.close_gate.lock().await;
        self.closed.store(true, Ordering::Release);
        self.lifecycle.cancel();
        let close = self
            .client
            .close(CancellationToken::new())
            .await
            .map_err(runtime_error);
        let driver = self
            .driver
            .lock()
            .map_err(|_| RuntimeError::internal("codex driver owner"))?
            .take();
        if let Some(driver) = driver {
            driver
                .await
                .map_err(|_| RuntimeError::internal("codex driver task"))?;
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("codex session state"))?;
        if let Some(cancellation) = state.active_tool_cancellation.take() {
            cancellation.cancel();
        }
        state.pending.clear();
        close
    }
}

#[async_trait]
impl RuntimeSession for CodexSession {
    fn id(&self) -> &RuntimeSessionId {
        &self.id
    }

    fn runtime_id(&self) -> &heycode_runtime::AgentRuntimeId {
        &self.runtime_id
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
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
        let payload = self
            .request(
                "turn/start",
                json!({
                    "threadId":self.id.as_str(),
                    "input":[{"type":"text","text":input.text()}],
                }),
                cancellation.clone(),
            )
            .await?;
        let turn = parse_turn_response(payload)?;
        self.admit_turn(turn.clone())?;
        Ok(turn)
    }

    async fn configure(
        &self,
        update: heycode_runtime::RuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<heycode_runtime::RuntimeConfiguration, RuntimeError> {
        let _gate = self.operation_gate.lock().await;
        self.check(&cancellation)?;
        if self.active_turn()?.is_some() {
            return Err(RuntimeError::conflict());
        }
        let mut unsupported = Vec::new();
        if update.system_prompt().is_some() {
            unsupported.push("system_prompt");
        }
        if update.tools_configured() {
            unsupported.push("tools");
        }
        if !unsupported.is_empty() {
            return Err(RuntimeError::unsupported_field_names(&unsupported));
        }
        if update.is_empty() {
            return Ok(self.configuration.lock().await.clone());
        }
        let mut params = Map::new();
        params.insert(
            "threadId".to_owned(),
            Value::String(self.id.as_str().to_owned()),
        );
        if let Some(model) = update.model() {
            params.insert("model".to_owned(), Value::String(model.to_owned()));
        }
        if let Some(effort) = update.reasoning_effort() {
            params.insert("effort".to_owned(), Value::String(effort.to_owned()));
        }
        let payload = self
            .request(
                "thread/settings/update",
                Value::Object(params),
                cancellation,
            )
            .await?;
        if let Err(error) = ensure_empty_object(payload) {
            let _settled = self.shutdown().await;
            return Err(error);
        }
        let mut configuration = self.configuration.lock().await;
        *configuration = configuration.merged_with(&update);
        Ok(configuration.clone())
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
        let turn = self.active_turn()?.ok_or_else(RuntimeError::conflict)?;
        let payload = self
            .request(
                "turn/steer",
                json!({
                    "threadId":self.id.as_str(),
                    "expectedTurnId":turn.as_str(),
                    "input":[{"type":"text","text":input.text()}],
                }),
                cancellation,
            )
            .await?;
        let value = payload.into_value();
        let returned = value
            .get("turnId")
            .and_then(Value::as_str)
            .filter(|id| valid_id(id))
            .ok_or_else(RuntimeError::protocol)?;
        if returned != turn.as_str() {
            return Err(RuntimeError::protocol());
        }
        Ok(())
    }

    async fn follow_up(
        &self,
        _input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        Err(RuntimeError::unsupported())
    }

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        let turn = {
            let state = self
                .state
                .lock()
                .map_err(|_| RuntimeError::internal("codex session state"))?;
            let Some(turn) = state.active_turn.clone() else {
                return Ok(());
            };
            if let Some(tool_cancellation) = &state.active_tool_cancellation {
                tool_cancellation.cancel();
            }
            turn
        };
        let payload = self
            .request(
                "turn/interrupt",
                json!({"threadId":self.id.as_str(),"turnId":turn.as_str()}),
                cancellation.clone(),
            )
            .await?;
        ensure_empty_object(payload)?;
        self.wait_turn_settled(&turn, &cancellation).await
    }

    async fn respond_permission(
        &self,
        response: RuntimePermissionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let pending = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("codex pending permission"))?
            .pending
            .get(response.request_id())
            .cloned()
            .ok_or_else(RuntimeError::conflict)?;
        let result = match pending.kind {
            PendingKind::Command | PendingKind::FileChange => json!({
                "decision":match response.decision() {
                    RuntimePermissionDecision::AllowOnce => "accept",
                    RuntimePermissionDecision::AllowSession => "acceptForSession",
                    RuntimePermissionDecision::Deny => "decline",
                }
            }),
            PendingKind::Permissions(requested) => json!({
                "permissions":match response.decision() {
                    RuntimePermissionDecision::AllowOnce | RuntimePermissionDecision::AllowSession => requested,
                    RuntimePermissionDecision::Deny => json!({}),
                },
                "scope":if response.decision() == RuntimePermissionDecision::AllowSession {
                    "session"
                } else {
                    "turn"
                }
            }),
            PendingKind::Question { .. } => return Err(RuntimeError::conflict()),
        };
        self.respond(response.request_id(), result, cancellation)
            .await
    }

    async fn respond_question(
        &self,
        response: RuntimeQuestionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let gate = self.response_gate.lock().await;
        self.check(&cancellation)?;
        let pending = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("codex pending question"))?
            .pending
            .get(response.request_id())
            .cloned()
            .ok_or_else(RuntimeError::conflict)?;
        let PendingKind::Question {
            questions,
            index,
            mut answers,
        } = pending.kind
        else {
            return Err(RuntimeError::conflict());
        };
        let question = &questions[index];
        let cancelled = response.answer().is_none() && response.selected_answers().is_none();
        let values = if let Some(labels) = response.selected_answers() {
            if (question.mode != heycode_core::QuestionMode::MultipleChoice && labels.len() != 1)
                || labels.iter().any(|label| !question.choices.contains(label))
            {
                return Err(RuntimeError::invalid_request());
            }
            labels.to_vec()
        } else {
            response
                .answer()
                .map_or_else(Vec::new, |answer| vec![answer.to_owned()])
        };
        answers.insert(question.id.clone(), json!({"answers": values}));
        if !cancelled && index + 1 < questions.len() {
            let next_id = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| RuntimeError::internal("codex pending question"))?;
                let value = state.next_request;
                state.next_request = value.checked_add(1).ok_or_else(RuntimeError::protocol)?;
                let next_id = RuntimeRequestId::new(format!("codex-request-{value}"))
                    .map_err(|_| RuntimeError::protocol())?;
                state.pending.remove(response.request_id());
                state.pending.insert(
                    next_id.clone(),
                    PendingRequest {
                        upstream: pending.upstream,
                        kind: PendingKind::Question {
                            questions: questions.clone(),
                            index: index + 1,
                            answers,
                        },
                    },
                );
                next_id
            };
            return self
                .events
                .emit(questions[index + 1].event(next_id, (index + 2, questions.len())));
        }
        for remaining in questions.iter().skip(index + 1) {
            answers.insert(remaining.id.clone(), json!({"answers": []}));
        }
        drop(gate);
        self.respond(
            response.request_id(),
            json!({"answers": answers}),
            cancellation,
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
            .map_err(|_| RuntimeError::internal("codex compact state"))?
            .compact_generation;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| RuntimeError::internal("codex compact state"))?;
            state.compact_pending = true;
            state.compact_turn = None;
        }
        let payload = self
            .request(
                "thread/compact/start",
                json!({"threadId":self.id.as_str()}),
                cancellation.clone(),
            )
            .await;
        let payload = match payload {
            Ok(payload) => payload,
            Err(error) => {
                if let Ok(mut state) = self.state.lock() {
                    state.compact_pending = false;
                    state.compact_turn = None;
                }
                return Err(error);
            }
        };
        ensure_empty_object(payload)?;
        loop {
            let changed = self.changed.notified();
            let complete = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| RuntimeError::internal("codex compact state"))?;
                let complete = state.compact_generation > baseline && state.compact_turn.is_none();
                if complete {
                    state.compact_pending = false;
                }
                complete
            };
            if complete {
                return Ok(RuntimeCompactOutcome::Applied);
            }
            tokio::select! {
                () = changed => {}
                () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
                () = self.lifecycle.cancelled() => return Err(RuntimeError::closed()),
            }
        }
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        self.shutdown().await
    }
}

async fn drive(driver: CodexDriver) {
    loop {
        let inbound = tokio::select! {
            biased;
            () = driver.lifecycle.cancelled() => return,
            inbound = driver.client.next_event(driver.lifecycle.clone()) => inbound,
        };
        let result = match inbound {
            Ok(CodexInboundEvent::Notification(notification)) => handle_notification(
                &driver.thread_id,
                notification.method(),
                notification.params(),
                &driver.state,
                &driver.changed,
                &driver.events,
                &driver.lifecycle,
            ),
            Ok(CodexInboundEvent::Request(request)) => {
                handle_server_request(&driver, request).await
            }
            Err(_error) if driver.lifecycle.is_cancelled() => return,
            Err(error) => Err(runtime_error(error)),
        };
        if let Err(error) = result {
            driver.events.fail(error);
            driver.lifecycle.cancel();
            let _settled = driver.client.close(CancellationToken::new()).await;
            return;
        }
    }
}

fn handle_notification(
    thread_id: &str,
    method: &str,
    params: &Value,
    state: &Mutex<SessionState>,
    changed: &Notify,
    events: &RuntimeEventHub,
    lifecycle: &CancellationToken,
) -> Result<(), RuntimeError> {
    match method {
        "thread/started" => {
            let id = params
                .get("thread")
                .and_then(Value::as_object)
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
                .filter(|id| valid_id(id))
                .ok_or_else(RuntimeError::protocol)?;
            if id == thread_id {
                Ok(())
            } else {
                Err(RuntimeError::protocol())
            }
        }
        "turn/started" => {
            let turn = notification_turn(params, thread_id, "inProgress")?;
            admit_driver_turn(state, changed, events, lifecycle, turn)
        }
        "item/agentMessage/delta" => {
            let object = correlated(params, thread_id)?;
            if is_compact_event(object, state)? {
                return Ok(());
            }
            let delta = required_output(object, "delta", 64 * 1024, false)?;
            events.emit(RuntimeEventKind::CommentaryDelta {
                text: delta.to_owned(),
            })
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            let object = correlated(params, thread_id)?;
            if is_compact_event(object, state)? {
                return Ok(());
            }
            let delta = required_output(object, "delta", 64 * 1024, false)?;
            events.emit(RuntimeEventKind::ReasoningDelta {
                text: delta.to_owned(),
            })
        }
        "item/started" => handle_item(params, thread_id, true, state, changed, events),
        "item/completed" => handle_item(params, thread_id, false, state, changed, events),
        "thread/tokenUsage/updated" => handle_usage(params, thread_id, state, events),
        "turn/completed" => complete_turn(params, thread_id, state, changed, events),
        "thread/compacted" => {
            validate_thread(params, thread_id)?;
            let mut state = state
                .lock()
                .map_err(|_| RuntimeError::internal("codex compact state"))?;
            state.compact_generation = state.compact_generation.saturating_add(1);
            drop(state);
            changed.notify_waiters();
            events.emit(RuntimeEventKind::Notice {
                code: "codex.compacted".to_owned(),
                message: "Codex session context was compacted.".to_owned(),
            })
        }
        "turn/plan/updated" | "item/plan/delta" => {
            correlated(params, thread_id)?;
            events.emit(RuntimeEventKind::Notice {
                code: "codex.plan.updated".to_owned(),
                message: "Codex updated its active plan.".to_owned(),
            })
        }
        "error" => events.emit(RuntimeEventKind::Notice {
            code: "codex.error".to_owned(),
            message: "Codex reported a turn error.".to_owned(),
        }),
        "serverRequest/resolved" => {
            let object = params.as_object().ok_or_else(RuntimeError::protocol)?;
            validate_thread_value(object, thread_id)?;
            if let Some(request_id) = object.get("requestId") {
                let mut state = state
                    .lock()
                    .map_err(|_| RuntimeError::internal("codex pending request"))?;
                state
                    .pending
                    .retain(|_, pending| !pending.upstream.matches_json(request_id));
            }
            Ok(())
        }
        method if PINNED_IGNORED_NOTIFICATIONS.contains(&method) => Ok(()),
        _ => Err(RuntimeError::protocol()),
    }
}

fn handle_item(
    params: &Value,
    thread_id: &str,
    started: bool,
    state: &Mutex<SessionState>,
    changed: &Notify,
    events: &RuntimeEventHub,
) -> Result<(), RuntimeError> {
    let object = correlated(params, thread_id)?;
    let item = object
        .get("item")
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    let item_type = required_id(item, "type").map_err(runtime_error)?;
    let id = required_id(item, "id").map_err(runtime_error)?;
    if is_compact_event(object, state)? {
        if !started && item_type == "contextCompaction" {
            mark_compacted(state, changed)?;
        }
        return Ok(());
    }
    if item_type == "agentMessage" && !started {
        let text = required_output(item, "text", MAX_TEXT_BYTES, true)?;
        let phase = item.get("phase").and_then(Value::as_str);
        if !matches!(phase, Some("commentary")) {
            state
                .lock()
                .map_err(|_| RuntimeError::internal("codex final message"))?
                .final_text = Some(text.to_owned());
        }
        return Ok(());
    }
    if matches!(
        item_type,
        "userMessage"
            | "hookPrompt"
            | "agentMessage"
            | "plan"
            | "reasoning"
            | "subAgentActivity"
            | "imageView"
            | "sleep"
            | "imageGeneration"
            | "enteredReviewMode"
            | "exitedReviewMode"
            | "contextCompaction"
            | "dynamicToolCall"
    ) {
        return Ok(());
    }
    let (name, arguments) = tool_projection(item_type, item)?;
    let call_id = external_call_id(id)?;
    if started {
        events.emit(RuntimeEventKind::ToolCall {
            call_id,
            name,
            arguments,
        })
    } else {
        let is_error = item
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| !matches!(status, "completed"));
        events.emit(RuntimeEventKind::ToolResult {
            call_id,
            result: Value::Object(item.clone()),
            is_error,
        })
    }
}

fn tool_projection(
    item_type: &str,
    item: &Map<String, Value>,
) -> Result<(String, Value), RuntimeError> {
    let (name, keys): (&str, &[&str]) = match item_type {
        "commandExecution" => ("codex.command", &["command", "cwd"]),
        "fileChange" => ("codex.file_change", &["changes"]),
        "mcpToolCall" => ("codex.mcp", &["server", "tool", "arguments"]),
        "collabAgentToolCall" => ("codex.subagent", &["tool", "receiverThreadIds"]),
        "webSearch" => ("codex.web_search", &["query"]),
        _ => return Err(RuntimeError::protocol()),
    };
    let mut arguments = Map::new();
    for key in keys {
        if let Some(value) = item.get(*key) {
            arguments.insert((*key).to_owned(), value.clone());
        }
    }
    Ok((name.to_owned(), Value::Object(arguments)))
}

fn handle_usage(
    params: &Value,
    thread_id: &str,
    state: &Mutex<SessionState>,
    events: &RuntimeEventHub,
) -> Result<(), RuntimeError> {
    let object = correlated(params, thread_id)?;
    if is_compact_event(object, state)? {
        return Ok(());
    }
    let last = object
        .get("tokenUsage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("last"))
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    let prompt_tokens = nonnegative_u64(last, "inputTokens")?;
    let completion_tokens = nonnegative_u64(last, "outputTokens")?;
    events.emit(RuntimeEventKind::Usage {
        usage: TokenUsage {
            prompt_tokens,
            completion_tokens,
        },
        context: None,
    })
}

fn complete_turn(
    params: &Value,
    thread_id: &str,
    state: &Mutex<SessionState>,
    changed: &Notify,
    events: &RuntimeEventHub,
) -> Result<(), RuntimeError> {
    let object = params.as_object().ok_or_else(RuntimeError::protocol)?;
    validate_thread_value(object, thread_id)?;
    let turn_object = object
        .get("turn")
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    let turn = RuntimeTurnId::new(required_id(turn_object, "id").map_err(runtime_error)?)
        .map_err(|_| RuntimeError::protocol())?;
    let reason = match turn_object.get("status").and_then(Value::as_str) {
        Some("completed") => RuntimeFinishReason::Stop,
        Some("interrupted") => RuntimeFinishReason::Cancelled,
        Some("failed") => RuntimeFinishReason::Error,
        _ => return Err(RuntimeError::protocol()),
    };
    {
        let mut session = state
            .lock()
            .map_err(|_| RuntimeError::internal("codex turn state"))?;
        if session.compact_turn.as_ref() == Some(&turn) {
            if !matches!(
                reason,
                RuntimeFinishReason::Stop | RuntimeFinishReason::Cancelled
            ) {
                return Err(RuntimeError::protocol());
            }
            session.compact_turn = None;
            session.compact_pending = false;
            session.compact_generation = session.compact_generation.saturating_add(1);
            drop(session);
            changed.notify_waiters();
            return Ok(());
        }
    }
    let (final_text, active) = {
        let mut state = state
            .lock()
            .map_err(|_| RuntimeError::internal("codex turn state"))?;
        let active = state
            .active_turn
            .take()
            .ok_or_else(RuntimeError::protocol)?;
        if active != turn {
            return Err(RuntimeError::protocol());
        }
        state.turn_started_emitted = false;
        if let Some(cancellation) = state.active_tool_cancellation.take() {
            cancellation.cancel();
        }
        state.pending.clear();
        (state.final_text.take(), active)
    };
    // A thread can complete without an agent message; that is still a stopped
    // turn, and R02 requires a stopped turn to publish its final message, so
    // the empty text is published instead of killing the session.
    if reason == RuntimeFinishReason::Stop {
        events.emit(RuntimeEventKind::FinalMessage {
            text: final_text.unwrap_or_default(),
        })?;
    }
    events.emit(RuntimeEventKind::TurnFinished {
        turn: active,
        reason,
    })?;
    changed.notify_waiters();
    Ok(())
}

async fn handle_server_request(
    driver: &CodexDriver,
    request: CodexServerRequest,
) -> Result<(), RuntimeError> {
    let object = request
        .params()
        .as_object()
        .ok_or_else(RuntimeError::protocol)?;
    validate_thread_value(object, &driver.thread_id)?;
    if request.method() == "item/tool/call" {
        return handle_dynamic_tool_call(driver, &request, object).await;
    }
    let action;
    let detail;
    let kind;
    let question;
    match request.method() {
        "item/commandExecution/requestApproval" => {
            action = "Codex command execution".to_owned();
            detail = safe_detail(object.get("reason").or_else(|| object.get("command")));
            kind = PendingKind::Command;
            question = None;
        }
        "item/fileChange/requestApproval" => {
            action = "Codex file change".to_owned();
            detail = safe_detail(object.get("reason").or_else(|| object.get("grantRoot")));
            kind = PendingKind::FileChange;
            question = None;
        }
        "item/permissions/requestApproval" => {
            action = "Codex permission grant".to_owned();
            detail = safe_detail(object.get("reason"));
            let permissions = object
                .get("permissions")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(RuntimeError::protocol)?;
            kind = PendingKind::Permissions(permissions);
            question = None;
        }
        "item/tool/requestUserInput" => {
            let questions = parse_questions(object)?;
            action = String::new();
            detail = String::new();
            question = Some((questions[0].clone(), questions.len()));
            kind = PendingKind::Question {
                questions,
                index: 0,
                answers: Map::new(),
            };
        }
        _ => return Err(RuntimeError::protocol()),
    }
    let (runtime_id, pending) = {
        let mut state = driver
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("codex pending request"))?;
        if state.pending.len() >= MAX_PENDING_HUMAN_REQUESTS {
            return Err(RuntimeError::protocol());
        }
        let value = state.next_request;
        state.next_request = state
            .next_request
            .checked_add(1)
            .ok_or_else(RuntimeError::protocol)?;
        let runtime_id = RuntimeRequestId::new(format!("codex-request-{value}"))
            .map_err(|_| RuntimeError::protocol())?;
        let pending = PendingRequest {
            upstream: request.id().clone(),
            kind,
        };
        if state
            .pending
            .insert(runtime_id.clone(), pending.clone())
            .is_some()
        {
            return Err(RuntimeError::protocol());
        }
        (runtime_id, pending)
    };
    match question {
        Some((question, total)) => driver.events.emit(question.event(runtime_id, (1, total))),
        None => {
            let _retained = pending;
            driver.events.emit(RuntimeEventKind::PermissionRequested {
                request_id: runtime_id,
                action,
                detail,
            })
        }
    }
}

async fn handle_dynamic_tool_call(
    driver: &CodexDriver,
    request: &CodexServerRequest,
    object: &Map<String, Value>,
) -> Result<(), RuntimeError> {
    let executor = driver
        .tool_executor
        .as_ref()
        .ok_or_else(RuntimeError::protocol)?;
    if !object.get("namespace").is_none_or(Value::is_null) {
        return Err(RuntimeError::protocol());
    }
    let turn = required_id(object, "turnId").map_err(runtime_error)?;
    let tool_cancellation = {
        let state = driver
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("codex tool state"))?;
        if state
            .active_turn
            .as_ref()
            .is_none_or(|active| active.as_str() != turn)
        {
            return Err(RuntimeError::protocol());
        }
        state
            .active_tool_cancellation
            .clone()
            .ok_or_else(RuntimeError::protocol)?
    };
    let call_id = external_call_id(required_id(object, "callId").map_err(runtime_error)?)?;
    let name = required_id(object, "tool")
        .map_err(runtime_error)?
        .to_owned();
    if !driver.allowed_tools.contains(&name) {
        return Err(RuntimeError::protocol());
    }
    let arguments = object
        .get("arguments")
        .cloned()
        .ok_or_else(RuntimeError::protocol)?;
    if !arguments.is_object() {
        return Err(RuntimeError::protocol());
    }
    driver.events.emit(RuntimeEventKind::ToolCall {
        call_id: call_id.clone(),
        name: name.clone(),
        arguments: arguments.clone(),
    })?;
    let output = executor
        .execute(
            RuntimeToolCall {
                call_id: call_id.clone(),
                name,
                arguments,
            },
            tool_cancellation.child_token(),
        )
        .await?;
    driver.events.emit(RuntimeEventKind::ToolResult {
        call_id,
        result: Value::String(output.content.clone()),
        is_error: output.is_error,
    })?;
    driver
        .client
        .respond_success(
            request.id(),
            json!({
                "contentItems":[{"type":"inputText","text":output.content}],
                "success":!output.is_error,
            }),
            driver.lifecycle.child_token(),
        )
        .await
        .map_err(runtime_error)
}

fn admit_driver_turn(
    state: &Mutex<SessionState>,
    changed: &Notify,
    events: &RuntimeEventHub,
    lifecycle: &CancellationToken,
    turn: RuntimeTurnId,
) -> Result<(), RuntimeError> {
    let mut state = state
        .lock()
        .map_err(|_| RuntimeError::internal("codex turn state"))?;
    if state.compact_pending {
        match state.compact_turn.as_ref() {
            Some(active) if active != &turn => return Err(RuntimeError::protocol()),
            Some(_) => {}
            None => state.compact_turn = Some(turn),
        }
        drop(state);
        changed.notify_waiters();
        return Ok(());
    }
    match state.active_turn.as_ref() {
        Some(active) if active != &turn => return Err(RuntimeError::protocol()),
        Some(_) => {}
        None => {
            state.active_turn = Some(turn.clone());
            state.active_tool_cancellation = Some(lifecycle.child_token());
        }
    }
    if !state.turn_started_emitted {
        state.turn_started_emitted = true;
        drop(state);
        events.emit(RuntimeEventKind::TurnStarted { turn })?;
    }
    changed.notify_waiters();
    Ok(())
}

fn is_compact_event(
    object: &Map<String, Value>,
    state: &Mutex<SessionState>,
) -> Result<bool, RuntimeError> {
    let turn = object
        .get("turnId")
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    Ok(state
        .lock()
        .map_err(|_| RuntimeError::internal("codex compact state"))?
        .compact_turn
        .as_ref()
        .is_some_and(|active| active.as_str() == turn))
}

fn mark_compacted(state: &Mutex<SessionState>, changed: &Notify) -> Result<(), RuntimeError> {
    let mut state = state
        .lock()
        .map_err(|_| RuntimeError::internal("codex compact state"))?;
    state.compact_generation = state.compact_generation.saturating_add(1);
    drop(state);
    changed.notify_waiters();
    Ok(())
}

fn parse_turn_response(payload: CodexResponsePayload) -> Result<RuntimeTurnId, RuntimeError> {
    let value = payload.into_value();
    let turn = value
        .get("turn")
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    if turn.get("status").and_then(Value::as_str) != Some("inProgress") {
        return Err(RuntimeError::protocol());
    }
    RuntimeTurnId::new(required_id(turn, "id").map_err(runtime_error)?)
        .map_err(|_| RuntimeError::protocol())
}

fn notification_turn(
    params: &Value,
    thread_id: &str,
    status: &str,
) -> Result<RuntimeTurnId, RuntimeError> {
    let object = params.as_object().ok_or_else(RuntimeError::protocol)?;
    validate_thread_value(object, thread_id)?;
    let turn = object
        .get("turn")
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    if turn.get("status").and_then(Value::as_str) != Some(status) {
        return Err(RuntimeError::protocol());
    }
    RuntimeTurnId::new(required_id(turn, "id").map_err(runtime_error)?)
        .map_err(|_| RuntimeError::protocol())
}

fn correlated<'a>(
    params: &'a Value,
    thread_id: &str,
) -> Result<&'a Map<String, Value>, RuntimeError> {
    let object = params.as_object().ok_or_else(RuntimeError::protocol)?;
    validate_thread_value(object, thread_id)?;
    let turn = object
        .get("turnId")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .ok_or_else(RuntimeError::protocol)?;
    let active = object;
    let _validated_turn = turn;
    Ok(active)
}

fn validate_thread(params: &Value, expected: &str) -> Result<(), RuntimeError> {
    let object = params.as_object().ok_or_else(RuntimeError::protocol)?;
    validate_thread_value(object, expected)
}

fn validate_thread_value(object: &Map<String, Value>, expected: &str) -> Result<(), RuntimeError> {
    if object.get("threadId").and_then(Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(RuntimeError::protocol())
    }
}

fn required_id<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, CodexAppServerError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| valid_id(value))
        .ok_or_else(protocol)
}

fn external_call_id(value: &str) -> Result<CallId, RuntimeError> {
    if valid_id(value) {
        Ok(CallId::from_raw(value))
    } else {
        Err(RuntimeError::protocol())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn required_output<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    max: usize,
    allow_empty: bool,
) -> Result<&'a str, RuntimeError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| valid_text(value, max, allow_empty))
        .ok_or_else(RuntimeError::protocol)
}

fn valid_text(value: &str, max: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty())
        && value.len() <= max
        && !value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
}

fn safe_detail(value: Option<&Value>) -> String {
    let raw = value
        .and_then(Value::as_str)
        .unwrap_or("Review the requested Codex action.");
    let mut detail = String::with_capacity(raw.len().min(MAX_DETAIL_BYTES));
    for character in raw.chars() {
        if detail.len() >= MAX_DETAIL_BYTES {
            break;
        }
        detail.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        "Review the requested Codex action.".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn parse_questions(object: &Map<String, Value>) -> Result<Vec<ProviderQuestion>, RuntimeError> {
    let questions = object
        .get("questions")
        .and_then(Value::as_array)
        .filter(|questions| !questions.is_empty() && questions.len() <= 16)
        .ok_or_else(RuntimeError::protocol)?;
    let mut ids = HashSet::new();
    questions
        .iter()
        .map(|value| {
            let question = value.as_object().ok_or_else(RuntimeError::protocol)?;
            if question.get("isSecret").and_then(Value::as_bool) == Some(true)
                || question.get("isOther").and_then(Value::as_bool) == Some(false)
            {
                // The connected human surface permits custom text and has no
                // secret input lane. Refuse incompatible semantics explicitly.
                return Err(RuntimeError::unsupported());
            }
            let id = required_id(question, "id")
                .map_err(runtime_error)?
                .to_owned();
            if !ids.insert(id.clone()) {
                return Err(RuntimeError::protocol());
            }
            let prompt = required_output(question, "question", MAX_DETAIL_BYTES, false)?.to_owned();
            let header = question
                .get("header")
                .map(|_| {
                    required_output(question, "header", MAX_DETAIL_BYTES, false).map(str::to_owned)
                })
                .transpose()?;
            let mut choices = Vec::new();
            let mut descriptions = Vec::new();
            if let Some(options) = question.get("options").filter(|options| !options.is_null()) {
                let options = options
                    .as_array()
                    .filter(|options| options.len() <= MAX_CHOICES)
                    .ok_or_else(RuntimeError::protocol)?;
                let mut unique = HashSet::new();
                for option in options {
                    let option = option.as_object().ok_or_else(RuntimeError::protocol)?;
                    let label =
                        required_output(option, "label", MAX_DETAIL_BYTES, false)?.to_owned();
                    if !valid_id(&label) || !unique.insert(label.clone()) {
                        return Err(RuntimeError::protocol());
                    }
                    let description = option
                        .get("description")
                        .map(|_| {
                            required_output(option, "description", MAX_DETAIL_BYTES, true)
                                .map(str::to_owned)
                        })
                        .transpose()?;
                    choices.push(label);
                    descriptions.push(description);
                }
            }
            let multiple = question
                .get("multiSelect")
                .map(|value| value.as_bool().ok_or_else(RuntimeError::protocol))
                .transpose()?
                .unwrap_or(false);
            let mode = if choices.is_empty() {
                heycode_core::QuestionMode::FreeText
            } else if multiple {
                heycode_core::QuestionMode::MultipleChoice
            } else {
                heycode_core::QuestionMode::SingleChoice
            };
            Ok(ProviderQuestion {
                id,
                header,
                prompt,
                choices,
                descriptions,
                mode,
            })
        })
        .collect()
}

fn nonnegative_u64(object: &Map<String, Value>, field: &'static str) -> Result<u64, RuntimeError> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(RuntimeError::protocol)
}

fn ensure_empty_object(payload: CodexResponsePayload) -> Result<(), RuntimeError> {
    if payload.into_value().as_object().is_some_and(Map::is_empty) {
        Ok(())
    } else {
        Err(RuntimeError::protocol())
    }
}

fn utf8_workspace(path: &Path) -> Result<&str, RuntimeError> {
    path.to_str().ok_or_else(RuntimeError::invalid_request)
}

fn protocol() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::Protocol)
}

/// Pinned 0.153.2 server notifications that carry no state this session owns.
///
/// Ignoring is deliberate and closed: a method outside the pinned union is a
/// version-drift protocol error, but a pinned method heycode does not model must
/// never terminate an otherwise healthy primary session. Codex auto-names
/// threads and reports MCP startup/skill changes during ordinary turns.
const PINNED_IGNORED_NOTIFICATIONS: &[&str] = &[
    "account/login/completed",
    "account/rateLimits/updated",
    "account/updated",
    "app/list/updated",
    "autoApprovalReview/strictReviewRequired",
    "command/exec/outputDelta",
    "configWarning",
    "deprecationNotice",
    "externalAgentConfig/import/completed",
    "externalAgentConfig/import/progress",
    "fs/changed",
    "fuzzyFileSearch/sessionCompleted",
    "fuzzyFileSearch/sessionUpdated",
    "guardianWarning",
    "hook/completed",
    "hook/started",
    "item/autoApprovalReview/completed",
    "item/autoApprovalReview/started",
    "item/commandExecution/outputDelta",
    "item/commandExecution/terminalInteraction",
    "item/fileChange/outputDelta",
    "item/fileChange/patchUpdated",
    "item/mcpToolCall/progress",
    "item/reasoning/summaryPartAdded",
    "mcpServer/event/stream/notification",
    "mcpServer/oauthLogin/completed",
    "mcpServer/startupStatus/updated",
    "model/rerouted",
    "model/safetyBuffering/updated",
    "model/verification",
    "modelProvider/authRecoveryCompleted",
    "modelProvider/authRecoveryStarted",
    "process/exited",
    "process/outputDelta",
    "project/changed",
    "remoteControl/status/changed",
    "skills/changed",
    "thread/archived",
    "thread/closed",
    "thread/deleted",
    "thread/environment/connected",
    "thread/environment/disconnected",
    "thread/goal/cleared",
    "thread/goal/updated",
    "thread/name/updated",
    "thread/project/updated",
    "thread/queue/changed",
    "thread/realtime/closed",
    "thread/realtime/error",
    "thread/realtime/item/completed",
    "thread/realtime/item/started",
    "thread/realtime/item/transcript/delta",
    "thread/realtime/itemAdded",
    "thread/realtime/outputAudio/delta",
    "thread/realtime/sdp",
    "thread/realtime/started",
    "thread/realtime/transcript/delta",
    "thread/realtime/transcript/done",
    "thread/reverted",
    "thread/settings/updated",
    "thread/status/changed",
    "thread/unarchived",
    "turn/diff/updated",
    "turn/moderationMetadata",
    "warning",
    "windows/worldWritableWarning",
    "windowsSandbox/setupCompleted",
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::PINNED_IGNORED_NOTIFICATIONS;

    #[test]
    fn provider_question_batches_preserve_modes_descriptions_and_reject_unsupported_shapes() {
        use serde_json::json;
        let value = json!({"questions":[{"id":"one","header":"Intent","question":"Choose","multiSelect":true,"options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]},{"id":"two","question":"Custom text","options":[]}]});
        let questions = super::parse_questions(value.as_object().unwrap()).unwrap();
        assert_eq!(questions.len(), 2);
        assert_eq!(
            questions[0].mode,
            heycode_core::QuestionMode::MultipleChoice
        );
        assert_eq!(questions[0].header.as_deref(), Some("Intent"));
        assert_eq!(
            questions[0].descriptions,
            vec![Some("First".into()), Some("Second".into())]
        );
        assert_eq!(questions[1].mode, heycode_core::QuestionMode::FreeText);
        for field in ["isSecret", "isOther"] {
            let mut unsupported =
                json!({"questions":[{"id":"one","question":"Choose","options":[{"label":"A"}]}]});
            unsupported["questions"][0][field] = json!(field == "isSecret");
            assert!(super::parse_questions(unsupported.as_object().unwrap()).is_err());
        }
        let duplicate = json!({"questions":[{"id":"same","question":"First"},{"id":"same","question":"Second"}]});
        assert!(super::parse_questions(duplicate.as_object().unwrap()).is_err());
    }

    use std::collections::BTreeSet;

    /// Pinned 0.153.2 server notifications `handle_notification` dispatches
    /// into normalized runtime events. Audited against the pinned union so the
    /// handled and ignored sets together stay closed over the protocol.
    const PINNED_HANDLED_NOTIFICATIONS: &[&str] = &[
        "thread/started",
        "turn/started",
        "turn/completed",
        "thread/tokenUsage/updated",
        "thread/compacted",
        "item/started",
        "item/completed",
        "item/agentMessage/delta",
        "item/reasoning/summaryTextDelta",
        "item/reasoning/textDelta",
        "turn/plan/updated",
        "item/plan/delta",
        "serverRequest/resolved",
        "error",
    ];

    /// Exact `ServerNotification` union of the pinned `codex-cli 0.153.2`
    /// app-server protocol, generated with
    /// `codex app-server generate-json-schema --out <tmp>` under an empty
    /// `CODEX_HOME` and read from `ServerNotification.json`.
    const PINNED_SERVER_NOTIFICATIONS: &[&str] = &[
        "account/login/completed",
        "account/rateLimits/updated",
        "account/updated",
        "app/list/updated",
        "autoApprovalReview/strictReviewRequired",
        "command/exec/outputDelta",
        "configWarning",
        "deprecationNotice",
        "error",
        "externalAgentConfig/import/completed",
        "externalAgentConfig/import/progress",
        "fs/changed",
        "fuzzyFileSearch/sessionCompleted",
        "fuzzyFileSearch/sessionUpdated",
        "guardianWarning",
        "hook/completed",
        "hook/started",
        "item/agentMessage/delta",
        "item/autoApprovalReview/completed",
        "item/autoApprovalReview/started",
        "item/commandExecution/outputDelta",
        "item/commandExecution/terminalInteraction",
        "item/completed",
        "item/fileChange/outputDelta",
        "item/fileChange/patchUpdated",
        "item/mcpToolCall/progress",
        "item/plan/delta",
        "item/reasoning/summaryPartAdded",
        "item/reasoning/summaryTextDelta",
        "item/reasoning/textDelta",
        "item/started",
        "mcpServer/event/stream/notification",
        "mcpServer/oauthLogin/completed",
        "mcpServer/startupStatus/updated",
        "model/rerouted",
        "model/safetyBuffering/updated",
        "model/verification",
        "modelProvider/authRecoveryCompleted",
        "modelProvider/authRecoveryStarted",
        "process/exited",
        "process/outputDelta",
        "project/changed",
        "remoteControl/status/changed",
        "serverRequest/resolved",
        "skills/changed",
        "thread/archived",
        "thread/closed",
        "thread/compacted",
        "thread/deleted",
        "thread/environment/connected",
        "thread/environment/disconnected",
        "thread/goal/cleared",
        "thread/goal/updated",
        "thread/name/updated",
        "thread/project/updated",
        "thread/queue/changed",
        "thread/realtime/closed",
        "thread/realtime/error",
        "thread/realtime/item/completed",
        "thread/realtime/item/started",
        "thread/realtime/item/transcript/delta",
        "thread/realtime/itemAdded",
        "thread/realtime/outputAudio/delta",
        "thread/realtime/sdp",
        "thread/realtime/started",
        "thread/realtime/transcript/delta",
        "thread/realtime/transcript/done",
        "thread/reverted",
        "thread/settings/updated",
        "thread/started",
        "thread/status/changed",
        "thread/tokenUsage/updated",
        "thread/unarchived",
        "turn/completed",
        "turn/diff/updated",
        "turn/moderationMetadata",
        "turn/plan/updated",
        "turn/started",
        "warning",
        "windows/worldWritableWarning",
        "windowsSandbox/setupCompleted",
    ];

    #[test]
    fn pinned_notification_union_is_closed_and_dispatched() {
        let handled: BTreeSet<&str> = PINNED_HANDLED_NOTIFICATIONS.iter().copied().collect();
        let ignored: BTreeSet<&str> = PINNED_IGNORED_NOTIFICATIONS.iter().copied().collect();
        let pinned: BTreeSet<&str> = PINNED_SERVER_NOTIFICATIONS.iter().copied().collect();

        assert_eq!(
            handled.len(),
            PINNED_HANDLED_NOTIFICATIONS.len(),
            "handled list must not repeat a method"
        );
        assert_eq!(
            ignored.len(),
            PINNED_IGNORED_NOTIFICATIONS.len(),
            "ignored list must not repeat a method"
        );
        assert!(
            handled.is_disjoint(&ignored),
            "a method cannot be both handled and ignored"
        );

        let covered: BTreeSet<&str> = handled.union(&ignored).copied().collect();
        let uncovered: Vec<&str> = pinned.difference(&covered).copied().collect();
        assert!(
            uncovered.is_empty(),
            "pinned notifications would terminate a healthy session: {uncovered:?}"
        );
        let unpinned: Vec<&str> = covered.difference(&pinned).copied().collect();
        assert!(
            unpinned.is_empty(),
            "listed methods are absent from the pinned protocol: {unpinned:?}"
        );
    }

    #[test]
    fn every_handled_notification_has_a_dispatch_arm() {
        let source = include_str!("session.rs");
        let body = source
            .split_once("fn handle_notification(")
            .expect("handle_notification must exist")
            .1
            .split_once("\nfn handle_item(")
            .expect("handle_notification must be followed by handle_item")
            .0;
        for method in PINNED_HANDLED_NOTIFICATIONS {
            assert!(
                body.contains(&format!("\"{method}\"")),
                "{method} is listed as handled but has no dispatch arm"
            );
        }
        for method in PINNED_IGNORED_NOTIFICATIONS {
            assert!(
                !body.contains(&format!("\"{method}\"")),
                "{method} is listed as ignored but has a dispatch arm"
            );
        }
    }
}

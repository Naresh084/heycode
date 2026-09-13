//! ACP v1 over newline-delimited JSON-RPC stdio.
//!
//! One composed Context and native RuntimeSession back each ACP session. Prompt
//! tasks are owned by the server JoinSet, session cancellation remains
//! responsive while a prompt runs, and shutdown cancels/joins work before
//! closing runtimes and unwinding Context effects.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use futures::{FutureExt as _, StreamExt as _};
use serde_json::Value;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader,
};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use heycode_agent::Agent;
use heycode_runtime::{
    AgentRuntimeRegistry, RuntimeErrorCode, RuntimeEvent, RuntimeEventKind, RuntimeFinishReason,
    RuntimeInput, RuntimeSession, RuntimeStart,
};

const MAX_PROMPT_BLOCKS: usize = 64;
const MAX_PROMPT_TEXT_BYTES: usize = 1024 * 1024;
const MAX_MEDIA_BYTES: usize = 32 * 1024 * 1024;
const MAX_BASE64_BYTES: usize = 45 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 48 * 1024 * 1024;
const ACP_PROTOCOL_VERSION: u16 = 1;

/// World factory bound shared by the server state.
pub trait WorldFactory:
    Fn(Option<std::path::PathBuf>) -> Result<heycode_core::Context, anyhow::Error> + Send + Sync
{
}

impl<F> WorldFactory for F where
    F: Fn(Option<std::path::PathBuf>) -> Result<heycode_core::Context, anyhow::Error> + Send + Sync
{
}

struct ActivePrompt {
    request_id: Value,
    cancellation: Arc<CancellationToken>,
}

struct AcpSession {
    context: heycode_core::Context,
    agent: Arc<Agent>,
    runtime: Arc<dyn RuntimeSession>,
    attachments: Arc<heycode_attachments::AttachmentStore>,
    policy: Option<Arc<heycode_agent::InteractiveApproval>>,
    active: Option<ActivePrompt>,
    events: Arc<Mutex<heycode_runtime::RuntimeEventStream>>,
    approval_cancellation: CancellationToken,
    approval_task: Option<JoinHandle<()>>,
}

struct AcpState {
    make_world: Arc<dyn WorldFactory>,
    sessions: Mutex<HashMap<String, AcpSession>>,
    out_tx: mpsc::Sender<String>,
    next_id: AtomicU64,
    pending_out: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

impl AcpState {
    fn next_out_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    async fn send_value(&self, value: Value) {
        let _sent = self.out_tx.send(value.to_string()).await;
    }

    async fn send_notification(&self, method: &str, params: Value) {
        self.send_value(serde_json::json!({
            "jsonrpc":"2.0",
            "method":method,
            "params":params,
        }))
        .await;
    }

    async fn request_client(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
        operation_cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<Value> {
        let id = self.next_out_id();
        let (tx, rx) = oneshot::channel();
        self.pending_out.lock().await.insert(id, tx);
        self.send_value(serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":method,
            "params":params,
        }))
        .await;
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(anyhow::anyhow!("client request cancelled")),
            () = async {
                match operation_cancellation {
                    Some(cancellation) => cancellation.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => Err(anyhow::anyhow!("client request cancelled")),
            result = tokio::time::timeout(std::time::Duration::from_secs(300), rx) => {
                Ok(result??)
            }
        };
        self.pending_out.lock().await.remove(&id);
        result
    }
}

struct PromptCompletion {
    session_id: String,
    request_id: Value,
    cancellation: Arc<CancellationToken>,
    result: Result<Value, DispatchError>,
}

#[derive(Clone)]
struct DispatchError(i64, String);

impl DispatchError {
    fn invalid(message: impl Into<String>) -> Self {
        Self(-32602, message.into())
    }

    fn internal(message: &'static str) -> Self {
        Self(-32000, message.to_owned())
    }
}

impl From<anyhow::Error> for DispatchError {
    fn from(error: anyhow::Error) -> Self {
        Self(-32000, error.to_string())
    }
}

impl std::fmt::Debug for DispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DispatchError")
            .field("code", &self.0)
            .field("message_bytes", &self.1.len())
            .finish()
    }
}

/// Run ACP on process stdio until stdin closes.
///
/// # Errors
/// Fatal transport I/O or cleanup failures.
pub async fn serve(
    make_world: impl Fn(Option<std::path::PathBuf>) -> Result<heycode_core::Context, anyhow::Error>
    + Send
    + Sync
    + 'static,
) -> anyhow::Result<i32> {
    serve_io(make_world, tokio::io::stdin(), tokio::io::stdout()).await
}

async fn serve_io<R, W>(
    make_world: impl Fn(Option<std::path::PathBuf>) -> Result<heycode_core::Context, anyhow::Error>
    + Send
    + Sync
    + 'static,
    input: R,
    mut output: W,
) -> anyhow::Result<i32>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let state = Arc::new(AcpState {
        make_world: Arc::new(make_world),
        sessions: Mutex::new(HashMap::new()),
        out_tx,
        next_id: AtomicU64::new(1000),
        pending_out: Mutex::new(HashMap::new()),
    });
    let mut prompts = JoinSet::<PromptCompletion>::new();
    let mut reader = BufReader::new(input);

    let loop_result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                joined = prompts.join_next(), if !prompts.is_empty() => {
                    if let Some(joined) = joined {
                        let completion = joined
                            .map_err(|_| anyhow::anyhow!("ACP prompt task failed"))?;
                        finish_prompt(&state, completion).await;
                    }
                }
                frame = out_rx.recv() => {
                    let Some(frame) = frame else { break };
                    write_frame(&mut output, &frame).await?;
                }
                line = read_frame(&mut reader) => {
                    let Some(line) = line? else { break };
                    handle_line(&state, &mut prompts, &line).await;
                }
            }
        }
        Ok(())
    }
    .await;

    cancel_all(&state).await;
    while let Some(joined) = prompts.join_next().await {
        if let Ok(completion) = joined {
            finish_prompt(&state, completion).await;
        }
        while let Ok(frame) = out_rx.try_recv() {
            let _ignored = write_frame(&mut output, &frame).await;
        }
    }
    shutdown_sessions(&state).await;
    while let Ok(frame) = out_rx.try_recv() {
        let _ignored = write_frame(&mut output, &frame).await;
    }
    output.flush().await?;
    loop_result?;
    Ok(0)
}

async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(buffer.len(), |index| index.saturating_add(1));
        if bytes
            .len()
            .checked_add(take)
            .is_none_or(|length| length > MAX_FRAME_BYTES)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ACP frame exceeds the supported limit",
            ));
        }
        bytes.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if newline.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "ACP frame is not UTF-8"))
}

async fn write_frame<W: AsyncWrite + Unpin>(output: &mut W, frame: &str) -> anyhow::Result<()> {
    output.write_all(frame.as_bytes()).await?;
    output.write_all(b"\n").await?;
    output.flush().await?;
    Ok(())
}

async fn handle_line(state: &Arc<AcpState>, prompts: &mut JoinSet<PromptCompletion>, line: &str) {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let has_method = message.get("method").is_some();
    let has_result = message.get("result").is_some();
    let has_error = message.get("error").is_some();
    if !has_method && message.get("id").is_some() {
        if message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && has_result != has_error
            && let Some(id) = message.get("id").and_then(Value::as_u64)
            && let Some(sender) = state.pending_out.lock().await.remove(&id)
        {
            let _sent = sender.send(message);
        }
        return;
    }

    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let id = message.get("id").cloned();
    if method == "session/prompt" {
        let Some(id) = id else { return };
        if let Err(error) = start_prompt(state, prompts, id.clone(), params).await {
            send_error(state, id, error).await;
        }
        return;
    }
    if method == "session/cancel" {
        let result = cancel_session(state, &params).await;
        if let Some(id) = id {
            match result {
                Ok(()) => send_result(state, id, Value::Null).await,
                Err(error) => send_error(state, id, error).await,
            }
        }
        return;
    }
    if method == "$/cancel_request" {
        cancel_request(state, &params).await;
        return;
    }

    match dispatch(state, method, params).await {
        Ok(result) => {
            if let Some(id) = id {
                send_result(state, id, result).await;
            }
        }
        Err(error) => {
            if let Some(id) = id {
                send_error(state, id, error).await;
            }
        }
    }
}

async fn dispatch(
    state: &Arc<AcpState>,
    method: &str,
    params: Value,
) -> Result<Value, DispatchError> {
    match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {
                    "image": true,
                    "audio": false,
                    "embeddedContext": true,
                },
                "mcpCapabilities": {"http": false, "sse": false},
                "sessionCapabilities": {},
                "auth": {},
            },
            "authMethods": [],
            "agentInfo": {"name":"heycode","version":env!("CARGO_PKG_VERSION")},
        })),
        "session/new" => new_session(state, &params).await,
        other => Err(DispatchError(-32601, format!("method `{other}` not found"))),
    }
}

async fn new_session(state: &Arc<AcpState>, params: &Value) -> Result<Value, DispatchError> {
    let cwd = str_param(params, "cwd")?;
    if params
        .get("mcpServers")
        .and_then(Value::as_array)
        .is_some_and(|servers| !servers.is_empty())
    {
        return Err(DispatchError::invalid(
            "client-supplied MCP servers are not supported by this ACP profile",
        ));
    }
    if params
        .get("additionalDirectories")
        .and_then(Value::as_array)
        .is_some_and(|directories| !directories.is_empty())
    {
        return Err(DispatchError::invalid(
            "additional ACP workspace directories are not supported",
        ));
    }
    let mut context = (state.make_world)(Some(std::path::PathBuf::from(cwd)))
        .map_err(|_| DispatchError::internal("world boot failed"))?;
    let result = start_context_session(&context).await;
    let (session_id, agent, runtime, attachments, policy, ask_rx) = match result {
        Ok(value) => value,
        Err(error) => {
            context.shutdown();
            return Err(error);
        }
    };
    let approval_cancellation = CancellationToken::new();
    let approval_task = ask_rx.map(|receiver| {
        tokio::spawn(forward_approvals(
            Arc::clone(state),
            session_id.clone(),
            policy.clone(),
            receiver,
            approval_cancellation.clone(),
        ))
    });
    let mut sessions = state.sessions.lock().await;
    if sessions.contains_key(&session_id) {
        drop(sessions);
        approval_cancellation.cancel();
        if let Some(task) = approval_task {
            let _settled = task.await;
        }
        let _closed = runtime.close(CancellationToken::new()).await;
        context.shutdown();
        return Err(DispatchError::invalid("ACP session id collision"));
    }
    // One subscription for the session's life: see `PromptRun::events`.
    let events = Arc::new(Mutex::new(runtime.subscribe()));
    sessions.insert(
        session_id.clone(),
        AcpSession {
            context,
            agent,
            runtime,
            attachments,
            policy: policy.clone(),
            active: None,
            events,
            approval_cancellation,
            approval_task,
        },
    );
    Ok(serde_json::json!({"sessionId":session_id}))
}

type StartedSession = (
    String,
    Arc<Agent>,
    Arc<dyn RuntimeSession>,
    Arc<heycode_attachments::AttachmentStore>,
    Option<Arc<heycode_agent::InteractiveApproval>>,
    Option<heycode_agent::AskSubscription>,
);

async fn start_context_session(
    context: &heycode_core::Context,
) -> Result<StartedSession, DispatchError> {
    let agent = context
        .get::<Agent>(heycode_agent::SERVICE_AGENT)
        .ok_or_else(|| DispatchError::internal("agent service missing"))?;
    let attachments = context
        .get::<heycode_attachments::AttachmentStore>(heycode_attachments::SERVICE_ATTACHMENTS)
        .ok_or_else(|| DispatchError::internal("attachment service missing"))?;
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .ok_or_else(|| DispatchError::internal("runtime registry missing"))?;
    let runtime = runtimes
        .get("native")
        .map_err(|_| DispatchError::internal("runtime registry unavailable"))?
        .ok_or_else(|| DispatchError::internal("native runtime missing"))?;
    let heycode_session_id = agent
        .session()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .id()
        .clone();
    let start = RuntimeStart::new(heycode_session_id, agent.cwd())
        .map_err(|_| DispatchError::internal("native runtime request invalid"))?;
    let runtime = runtime
        .start(start, CancellationToken::new())
        .await
        .map_err(|_| DispatchError::internal("native runtime start failed"))?;
    let session_id = runtime.id().as_str().to_owned();
    let policy = context
        .get::<heycode_agent::InteractiveApproval>(heycode_agent::SERVICE_APPROVAL_INTERACTIVE);
    let ask_rx = policy
        .as_ref()
        .and_then(|policy| policy.take_subscription());
    Ok((session_id, agent, runtime, attachments, policy, ask_rx))
}

async fn forward_approvals(
    state: Arc<AcpState>,
    session_id: String,
    policy: Option<Arc<heycode_agent::InteractiveApproval>>,
    mut receiver: heycode_agent::AskSubscription,
    cancellation: CancellationToken,
) {
    loop {
        let ask = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                while let Ok(ask) = receiver.try_recv() {
                    if let Some(policy) = &policy {
                        policy.answer(ask.id, false);
                    }
                }
                break;
            }
            ask = receiver.recv() => match ask {
                Some(ask) => ask,
                None => break,
            },
        };
        let operation_cancellation =
            state
                .sessions
                .lock()
                .await
                .get(&session_id)
                .and_then(|session| {
                    session
                        .active
                        .as_ref()
                        .map(|active| active.cancellation.clone())
                });
        let reply = state
            .request_client(
                "session/request_permission",
                serde_json::json!({
                    "sessionId":session_id,
                    "toolCall": {
                        "toolCallId":format!("approval-{}", ask.id),
                        "title":format!("{}({})", ask.name, ask.args_preview),
                        "status":"pending",
                    },
                    "options": [
                        {"optionId":"allow_once","name":"Allow","kind":"allow_once"},
                        {"optionId":"deny_once","name":"Deny","kind":"reject_once"},
                    ],
                }),
                &cancellation,
                operation_cancellation.as_deref(),
            )
            .await;
        let allow = reply.as_ref().is_ok_and(permission_response_allows);
        if let Some(policy) = &policy {
            policy.answer(ask.id, allow);
        }
    }
}

fn permission_response_allows(value: &Value) -> bool {
    value
        .pointer("/result/outcome/outcome")
        .and_then(Value::as_str)
        == Some("selected")
        && value
            .pointer("/result/outcome/optionId")
            .and_then(Value::as_str)
            == Some("allow_once")
}

async fn start_prompt(
    state: &Arc<AcpState>,
    prompts: &mut JoinSet<PromptCompletion>,
    request_id: Value,
    params: Value,
) -> Result<(), DispatchError> {
    let session_id = str_param(&params, "sessionId")?;
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| DispatchError::invalid("unknown ACP session"))?;
    if session.active.is_some() {
        return Err(DispatchError::invalid(
            "ACP session already has active work",
        ));
    }
    let cancellation = Arc::new(CancellationToken::new());
    session.active = Some(ActivePrompt {
        request_id: request_id.clone(),
        cancellation: cancellation.clone(),
    });
    let runtime = session.runtime.clone();
    let attachments = session.attachments.clone();
    let context_window = session.agent.context_window();
    let events = session.events.clone();
    drop(sessions);

    let task_state = Arc::clone(state);
    let task_session_id = session_id.clone();
    let task_request_id = request_id.clone();
    let task_cancellation = cancellation.clone();
    prompts.spawn(async move {
        let future = run_prompt(
            &task_state,
            &task_session_id,
            &params,
            PromptRun {
                runtime,
                attachments,
                context_window,
                events,
                cancellation: task_cancellation.as_ref().clone(),
            },
        );
        let result = std::panic::AssertUnwindSafe(future)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(DispatchError::internal("ACP prompt task panicked")));
        PromptCompletion {
            session_id: task_session_id,
            request_id: task_request_id,
            cancellation: task_cancellation,
            result,
        }
    });
    Ok(())
}

async fn run_prompt(
    state: &Arc<AcpState>,
    session_id: &str,
    params: &Value,
    run: PromptRun,
) -> Result<Value, DispatchError> {
    let parsed = parse_prompt(params.get("prompt"), &run.attachments, &run.cancellation)?;
    let mut events = run.events.lock().await;
    let send = run.runtime.send(parsed.input, run.cancellation.clone());
    tokio::pin!(send);
    let mut send_result: Option<
        Result<heycode_runtime::RuntimeTurnId, heycode_runtime::RuntimeError>,
    > = None;
    let mut pump = PromptPump::new(parsed.echo_blocks);

    loop {
        if send_result.is_some() && pump.terminal.is_some() {
            break;
        }
        if let Some(result) = send_result.as_ref()
            && result
                .as_ref()
                .is_err_and(|error| error.code() == RuntimeErrorCode::Cancelled)
            && pump.terminal.is_none()
        {
            pump.terminal = Some(RuntimeFinishReason::Cancelled);
            break;
        }
        if send_result.is_some() {
            let item = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
                .await
                .map_err(|_| DispatchError::internal("runtime terminal event timed out"))?
                .ok_or_else(|| DispatchError::internal("runtime event stream ended"))?;
            process_runtime_event(
                state,
                session_id,
                item.map_err(|_| DispatchError::internal("runtime event failed"))?,
                run.context_window,
                &mut pump,
            )
            .await;
            continue;
        }
        tokio::select! {
            biased;
            result = &mut send => send_result = Some(result),
            item = events.next() => {
                let item = item
                    .ok_or_else(|| DispatchError::internal("runtime event stream ended"))?
                    .map_err(|_| DispatchError::internal("runtime event failed"))?;
                process_runtime_event(
                    state,
                    session_id,
                    item,
                    run.context_window,
                    &mut pump,
                )
                .await;
            }
        }
    }

    let result =
        send_result.ok_or_else(|| DispatchError::internal("runtime send did not settle"))?;
    let reason = pump.terminal.unwrap_or(RuntimeFinishReason::Error);
    if let Err(error) = result
        && error.code() != RuntimeErrorCode::Cancelled
        && reason != RuntimeFinishReason::Cancelled
    {
        return Err(DispatchError::internal("runtime send failed"));
    }
    let stop = match reason {
        RuntimeFinishReason::Stop => "end_turn",
        RuntimeFinishReason::Limit => "max_tokens",
        RuntimeFinishReason::Cancelled => "cancelled",
        RuntimeFinishReason::Error => return Err(DispatchError::internal("runtime turn failed")),
    };
    Ok(serde_json::json!({"stopReason":stop}))
}

struct PromptRun {
    runtime: Arc<dyn RuntimeSession>,
    attachments: Arc<heycode_attachments::AttachmentStore>,
    context_window: u64,
    /// The session's one subscription, shared by every prompt it runs.
    ///
    /// A subscription is contiguous from sequence zero for its whole life, and
    /// a hub that evicted past `RUNTIME_EVENT_HISTORY` renumbers each new
    /// subscription from zero as well, so a sequence baseline carried from an
    /// earlier subscription is meaningless: applied to a fresh one it sits
    /// above the entire replayed window and skips every live event of a long
    /// session's later prompts. The session keeps one stream instead, so every
    /// event it yields is new and no cross-prompt dedupe exists to go wrong.
    events: Arc<Mutex<heycode_runtime::RuntimeEventStream>>,
    cancellation: CancellationToken,
}

struct PromptPump {
    echo_blocks: Vec<Value>,
    echoed: bool,
    saw_message_chunk: bool,
    pending_untrusted_web: bool,
    context_budget_seen: bool,
    terminal: Option<RuntimeFinishReason>,
}

impl PromptPump {
    fn new(echo_blocks: Vec<Value>) -> Self {
        Self {
            echo_blocks,
            echoed: false,
            saw_message_chunk: false,
            pending_untrusted_web: false,
            context_budget_seen: false,
            terminal: None,
        }
    }
}

async fn process_runtime_event(
    state: &Arc<AcpState>,
    session_id: &str,
    event: RuntimeEvent,
    context_window: u64,
    pump: &mut PromptPump,
) {
    match event.kind() {
        RuntimeEventKind::SessionReady => {}
        RuntimeEventKind::TurnStarted { .. } => {
            if !pump.echoed {
                for content in &pump.echo_blocks {
                    send_update(
                        state,
                        session_id,
                        serde_json::json!({
                            "sessionUpdate":"user_message_chunk",
                            "content":content,
                        }),
                    )
                    .await;
                }
                pump.echoed = true;
            }
        }
        RuntimeEventKind::CommentaryDelta { text } => {
            pump.saw_message_chunk = true;
            send_text_update(state, session_id, "agent_message_chunk", text).await;
        }
        RuntimeEventKind::ReasoningDelta { text } => {
            send_text_update(state, session_id, "agent_thought_chunk", text).await;
        }
        RuntimeEventKind::FinalMessage { text } if !pump.saw_message_chunk => {
            send_text_update(state, session_id, "agent_message_chunk", text).await;
        }
        RuntimeEventKind::FinalMessage { .. } => {}
        RuntimeEventKind::ToolCall {
            call_id,
            name,
            arguments,
        } => {
            send_update(
                state,
                session_id,
                serde_json::json!({
                    "sessionUpdate":"tool_call",
                    "toolCallId":call_id.as_str(),
                    "title":name,
                    "kind":tool_kind(name),
                    "status":"in_progress",
                    "rawInput":arguments,
                }),
            )
            .await;
        }
        RuntimeEventKind::ToolResult {
            call_id,
            result,
            is_error,
        } => {
            let text = result
                .as_str()
                .map_or_else(|| result.to_string(), str::to_owned);
            let meta = pump
                .pending_untrusted_web
                .then(|| serde_json::json!({"heycode":{"untrustedContent":{"source":"web"}}}));
            send_update(
                state,
                session_id,
                serde_json::json!({
                    "sessionUpdate":"tool_call_update",
                    "toolCallId":call_id.as_str(),
                    "status":if *is_error {"failed"} else {"completed"},
                    "content":[{
                        "type":"content",
                        "content":{"type":"text","text":text},
                    }],
                    "rawOutput":result,
                    "_meta":meta,
                }),
            )
            .await;
            pump.pending_untrusted_web = false;
        }
        RuntimeEventKind::ContextBudgetChanged { budget } => {
            pump.context_budget_seen = true;
            if let Some(window) = budget.window {
                send_update(
                    state,
                    session_id,
                    serde_json::json!({
                        "sessionUpdate": "usage_update", "used": budget.used, "size": window,
                        "_meta": { "contextBudget": budget },
                    }),
                )
                .await;
            }
        }
        RuntimeEventKind::Usage { usage, context } => {
            if pump.context_budget_seen {
                return;
            }
            let (used, context_window) = context.as_ref().map_or_else(
                || {
                    (
                        usage.prompt_tokens.saturating_add(usage.completion_tokens),
                        context_window,
                    )
                },
                |context| (context.tokens, context.context_window),
            );
            if context_window == 0 {
                return;
            }
            send_update(
                state,
                session_id,
                serde_json::json!({
                    "sessionUpdate":"usage_update",
                    "used":used,
                    "size":context_window,
                }),
            )
            .await;
        }
        RuntimeEventKind::TurnFinished { reason, .. } => pump.terminal = Some(*reason),
        RuntimeEventKind::Notice { code, .. } if code == "content.untrusted.web" => {
            pump.pending_untrusted_web = true;
        }
        RuntimeEventKind::Notice { code, .. } if code == "native.plan.enabled" => {
            send_update(
                state,
                session_id,
                serde_json::json!({
                    "sessionUpdate":"plan",
                    "entries":[{
                        "content":"Plan mode is active",
                        "priority":"high",
                        "status":"in_progress",
                    }],
                }),
            )
            .await;
        }
        RuntimeEventKind::Notice { code, .. } if code == "native.plan.disabled" => {
            send_update(
                state,
                session_id,
                serde_json::json!({"sessionUpdate":"plan","entries":[]}),
            )
            .await;
        }
        RuntimeEventKind::PermissionRequested { .. }
        | RuntimeEventKind::QuestionRequested { .. }
        | RuntimeEventKind::Notice { .. } => {}
    }
}

async fn send_text_update(state: &AcpState, session_id: &str, kind: &str, text: &str) {
    send_update(
        state,
        session_id,
        serde_json::json!({
            "sessionUpdate":kind,
            "content":{"type":"text","text":text},
        }),
    )
    .await;
}

async fn send_update(state: &AcpState, session_id: &str, update: Value) {
    state
        .send_notification(
            "session/update",
            serde_json::json!({"sessionId":session_id,"update":update}),
        )
        .await;
}

fn tool_kind(name: &str) -> &'static str {
    match name {
        "read" => "read",
        "write" | "edit" => "edit",
        "glob" | "grep" | "web_search" => "search",
        "web_fetch" => "fetch",
        "bash" => "execute",
        "exit_plan_mode" => "think",
        _ => "other",
    }
}

struct ParsedPrompt {
    input: RuntimeInput,
    echo_blocks: Vec<Value>,
}

fn parse_prompt(
    value: Option<&Value>,
    store: &heycode_attachments::AttachmentStore,
    cancellation: &CancellationToken,
) -> Result<ParsedPrompt, DispatchError> {
    let blocks = match value {
        Some(Value::String(text)) => vec![serde_json::json!({"type":"text","text":text})],
        Some(Value::Array(blocks)) if !blocks.is_empty() && blocks.len() <= MAX_PROMPT_BLOCKS => {
            blocks.clone()
        }
        _ => {
            return Err(DispatchError::invalid(
                "prompt must be one to 64 content blocks",
            ));
        }
    };
    let mut text = String::new();
    let mut attachments = Vec::new();
    let mut echo_blocks = Vec::with_capacity(blocks.len());
    for block in blocks {
        if cancellation.is_cancelled() {
            return Err(DispatchError(-32800, "prompt cancelled".to_owned()));
        }
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| DispatchError::invalid("prompt content type is missing"))?;
        match kind {
            "text" => {
                let value = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DispatchError::invalid("text content is invalid"))?;
                push_prompt_text(&mut text, value)?;
                echo_blocks.push(serde_json::json!({"type":"text","text":value}));
            }
            "image" => {
                let data = required_block_str(&block, "data")?;
                let mime = required_block_str(&block, "mimeType")?;
                let bytes = decode_base64(data)?;
                let name = match mime {
                    "image/png" => "acp-image.png",
                    "image/jpeg" => "acp-image.jpg",
                    "image/gif" => "acp-image.gif",
                    "image/webp" => "acp-image.webp",
                    _ => return Err(DispatchError::invalid("ACP image MIME is unsupported")),
                };
                let admission = store
                    .admit(
                        heycode_attachments::AttachmentInput::new(bytes, Some(mime), Some(name))
                            .map_err(|_| DispatchError::invalid("ACP image input is invalid"))?,
                        cancellation.clone(),
                    )
                    .map_err(|_| DispatchError::invalid("ACP image admission failed"))?;
                attachments.push(admission.metadata().clone());
                echo_blocks.push(serde_json::json!({
                    "type":"image",
                    "data":data,
                    "mimeType":mime,
                }));
            }
            "resource" => {
                parse_embedded_resource(&block, store, cancellation, &mut text, &mut attachments)?;
                echo_blocks.push(block);
            }
            "resource_link" => {
                let name = required_block_str(&block, "name")?;
                let uri = required_block_str(&block, "uri")?;
                validate_display(name, 512, "resource name")?;
                validate_display(uri, 4_096, "resource URI")?;
                push_prompt_text(&mut text, &format!("Resource link: {name} ({uri})"))?;
                echo_blocks.push(block);
            }
            "audio" => return Err(DispatchError::invalid("ACP audio prompts are unsupported")),
            _ => {
                return Err(DispatchError::invalid(
                    "ACP prompt content type is unsupported",
                ));
            }
        }
    }
    let input = RuntimeInput::with_attachments(text, attachments)
        .map_err(|_| DispatchError::invalid("ACP prompt input is invalid"))?;
    Ok(ParsedPrompt { input, echo_blocks })
}

fn parse_embedded_resource(
    block: &Value,
    store: &heycode_attachments::AttachmentStore,
    cancellation: &CancellationToken,
    text: &mut String,
    attachments: &mut Vec<heycode_core::AttachmentMetadata>,
) -> Result<(), DispatchError> {
    let resource = block
        .get("resource")
        .and_then(Value::as_object)
        .ok_or_else(|| DispatchError::invalid("embedded resource is invalid"))?;
    if let Some(value) = resource.get("text").and_then(Value::as_str) {
        let mime = resource
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("text/plain");
        let media = heycode_core::AttachmentMediaType::new(mime)
            .map_err(|_| DispatchError::invalid("embedded text MIME is invalid"))?;
        push_prompt_text(
            text,
            &format!(
                "<embedded_resource media_type=\"{}\">\n{}\n</embedded_resource>",
                media.as_str(),
                value,
            ),
        )?;
        return Ok(());
    }
    let blob = resource
        .get("blob")
        .and_then(Value::as_str)
        .ok_or_else(|| DispatchError::invalid("embedded resource has no text/blob"))?;
    let mime = resource
        .get("mimeType")
        .and_then(Value::as_str)
        .ok_or_else(|| DispatchError::invalid("embedded blob MIME is missing"))?;
    let bytes = decode_base64(blob)?;
    let name = match mime {
        "application/pdf" => "acp-resource.pdf",
        "text/html" | "application/xhtml+xml" => "acp-resource.html",
        "image/png" => "acp-resource.png",
        "image/jpeg" => "acp-resource.jpg",
        "image/gif" => "acp-resource.gif",
        "image/webp" => "acp-resource.webp",
        _ => return Err(DispatchError::invalid("embedded blob MIME is unsupported")),
    };
    let admission = store
        .admit(
            heycode_attachments::AttachmentInput::new(bytes, Some(mime), Some(name))
                .map_err(|_| DispatchError::invalid("embedded resource is invalid"))?,
            cancellation.clone(),
        )
        .map_err(|_| DispatchError::invalid("embedded resource admission failed"))?;
    attachments.push(admission.metadata().clone());
    Ok(())
}

fn required_block_str<'a>(block: &'a Value, field: &str) -> Result<&'a str, DispatchError> {
    block
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| DispatchError::invalid(format!("content `{field}` is invalid")))
}

fn decode_base64(value: &str) -> Result<Vec<u8>, DispatchError> {
    if value.is_empty() || value.len() > MAX_BASE64_BYTES {
        return Err(DispatchError::invalid(
            "base64 content exceeds the ACP limit",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| DispatchError::invalid("base64 content is malformed"))?;
    if bytes.is_empty() || bytes.len() > MAX_MEDIA_BYTES {
        return Err(DispatchError::invalid(
            "decoded media exceeds the ACP limit",
        ));
    }
    Ok(bytes)
}

fn push_prompt_text(target: &mut String, value: &str) -> Result<(), DispatchError> {
    if value.contains('\0') {
        return Err(DispatchError::invalid("prompt text contains NUL"));
    }
    let separator = usize::from(!target.is_empty()) * 2;
    if target
        .len()
        .checked_add(separator)
        .and_then(|size| size.checked_add(value.len()))
        .is_none_or(|size| size > MAX_PROMPT_TEXT_BYTES)
    {
        return Err(DispatchError::invalid("prompt text exceeds 1 MiB"));
    }
    if !target.is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(value);
    Ok(())
}

fn validate_display(value: &str, maximum: usize, field: &str) -> Result<(), DispatchError> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(|character| character.is_control())
    {
        Err(DispatchError::invalid(format!("ACP {field} is invalid")))
    } else {
        Ok(())
    }
}

async fn cancel_session(state: &Arc<AcpState>, params: &Value) -> Result<(), DispatchError> {
    let session_id = str_param(params, "sessionId")?;
    let (runtime, active, policy) = {
        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| DispatchError::invalid("unknown ACP session"))?;
        (
            session.runtime.clone(),
            session
                .active
                .as_ref()
                .map(|active| active.cancellation.clone()),
            session.policy.clone(),
        )
    };
    if let Some(policy) = policy {
        let _denied = policy.deny_all_pending();
    }
    if let Some(active) = active {
        active.cancel();
        runtime
            .cancel(CancellationToken::new())
            .await
            .map_err(|_| DispatchError::internal("runtime cancellation failed"))?;
    }
    Ok(())
}

async fn cancel_request(state: &Arc<AcpState>, params: &Value) {
    let Some(id) = params.get("id") else { return };
    let target = {
        let sessions = state.sessions.lock().await;
        sessions.values().find_map(|session| {
            let active = session.active.as_ref()?;
            (active.request_id == *id).then(|| {
                (
                    active.cancellation.clone(),
                    session.runtime.clone(),
                    session.policy.clone(),
                )
            })
        })
    };
    if let Some((active, runtime, policy)) = target {
        if let Some(policy) = policy {
            let _denied = policy.deny_all_pending();
        }
        active.cancel();
        let _cancelled = runtime.cancel(CancellationToken::new()).await;
    }
}

async fn finish_prompt(state: &Arc<AcpState>, completion: PromptCompletion) {
    if let Some(session) = state.sessions.lock().await.get_mut(&completion.session_id)
        && session
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.cancellation, &completion.cancellation))
    {
        session.active = None;
    }
    match completion.result {
        Ok(result) => send_result(state, completion.request_id, result).await,
        Err(error) => send_error(state, completion.request_id, error).await,
    }
}

async fn send_result(state: &AcpState, id: Value, result: Value) {
    state
        .send_value(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}))
        .await;
}

async fn send_error(state: &AcpState, id: Value, error: DispatchError) {
    state
        .send_value(serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "error":{"code":error.0,"message":error.1},
        }))
        .await;
}

async fn cancel_all(state: &AcpState) {
    let operations = {
        let sessions = state.sessions.lock().await;
        sessions
            .values()
            .map(|session| {
                if let Some(policy) = &session.policy {
                    let _denied = policy.deny_all_pending();
                }
                if let Some(active) = &session.active {
                    active.cancellation.cancel();
                }
                session.runtime.clone()
            })
            .collect::<Vec<_>>()
    };
    for runtime in operations {
        let _cancelled = runtime.cancel(CancellationToken::new()).await;
    }
}

async fn shutdown_sessions(state: &AcpState) {
    let sessions = {
        let mut sessions = state.sessions.lock().await;
        sessions
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>()
    };
    for mut session in sessions {
        session.approval_cancellation.cancel();
        if let Some(task) = session.approval_task.take() {
            let _settled = task.await;
        }
        let _closed = session.runtime.close(CancellationToken::new()).await;
        session.context.shutdown();
    }
}

fn str_param(params: &Value, key: &str) -> Result<String, DispatchError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| DispatchError::invalid(format!("missing string param `{key}`")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use heycode_core::ToolSpec;
    use heycode_runtime::{
        AgentRuntimeId, RuntimeCapabilities, RuntimeCompactOutcome, RuntimeError,
        RuntimeEventStream, RuntimePermissionResponse, RuntimeQuestionResponse, RuntimeSessionId,
        RuntimeTurnId,
    };
    use heycode_tools::{Tool, ToolCtx, ToolError};
    use tokio::sync::{Notify, broadcast};

    use super::*;

    fn state() -> (Arc<AcpState>, mpsc::Receiver<String>) {
        let (out_tx, out_rx) = mpsc::channel(64);
        let factory =
            |_cwd: Option<std::path::PathBuf>| Err(anyhow::anyhow!("unused test factory"));
        (
            Arc::new(AcpState {
                make_world: Arc::new(factory),
                sessions: Mutex::new(HashMap::new()),
                out_tx,
                next_id: AtomicU64::new(1000),
                pending_out: Mutex::new(HashMap::new()),
            }),
            out_rx,
        )
    }

    fn attachment_world() -> (
        tempfile::TempDir,
        heycode_core::Context,
        Arc<heycode_attachments::AttachmentStore>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let plugins = vec![
            heycode_session::session_plugin(root.path().join("sessions")),
            heycode_attachments::local_attachment_plugin(
                heycode_attachments::AttachmentStoreConfig::new(
                    root.path().join("attachments"),
                    heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
                )
                .unwrap(),
            ),
        ];
        let context = heycode_core::compose(&plugins).unwrap();
        let store = context
            .get::<heycode_attachments::AttachmentStore>(heycode_attachments::SERVICE_ATTACHMENTS)
            .unwrap();
        (root, context, store)
    }

    struct HangingProvider;

    impl heycode_llm::Provider for HangingProvider {
        fn info(&self) -> heycode_llm::ProviderInfo {
            heycode_llm::ProviderInfo {
                name: "acp-hang".to_owned(),
                default_model: "hang".to_owned(),
            }
        }

        fn stream(&self, _request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
            Box::pin(futures::stream::unfold(Some(()), |state| async move {
                match state {
                    Some(()) => Some((
                        Ok(heycode_llm::StreamChunk::TextDelta("working".to_owned())),
                        None,
                    )),
                    None => {
                        std::future::pending::<()>().await;
                        None
                    }
                }
            }))
        }
    }

    fn hanging_world(root: &std::path::Path) -> heycode_core::Context {
        let config = heycode_config::Config::defaults();
        let trust = heycode_trust::WorkspaceTrustService::memory(
            root,
            heycode_cli::project_content_policy(),
        )
        .unwrap();
        heycode_cli::compose_world(&heycode_cli::WorldOptions {
            config: &config,
            trust,
            config_migration: None,
            profile_layers: &[],
            sessions_dir: root.join("sessions"),
            attachments_dir: root.join("attachments"),
            attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
            session_source: heycode_session::SessionSource::Acp,
            approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
            settings_user_path: root.join("settings.toml"),
            credentials_root: root.join("credentials"),
            catalog_cache_path: root.join("cache/models.json"),
            settings_watch: false,
            onboarding_required: false,
            credential_validated_at_ms: None,
            cwd: root.to_path_buf(),
            fake: Some(Arc::new(HangingProvider)),
            resume: None,
        })
        .unwrap()
    }

    struct PermissionProbeTool {
        runs: Arc<AtomicUsize>,
        started: Arc<Notify>,
    }

    #[async_trait]
    impl Tool for PermissionProbeTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "permission_probe".to_owned(),
                description: "Record one approved permission fixture invocation.".to_owned(),
                parameters: serde_json::json!({
                    "type":"object",
                    "properties":{},
                    "additionalProperties":false,
                }),
            }
        }

        async fn run(&self, _args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
            let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            self.started.notify_waiters();
            Ok(serde_json::json!({"run":run}))
        }
    }

    fn permission_tool_script(call_id: &str) -> Vec<heycode_llm::StreamChunk> {
        vec![
            heycode_llm::StreamChunk::ToolCallDelta {
                index: 0,
                id: Some(call_id.to_owned()),
                name: Some("permission_probe".to_owned()),
                arguments_delta: "{}".to_owned(),
            },
            heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ]
    }

    fn final_script(text: &str) -> Vec<heycode_llm::StreamChunk> {
        vec![
            heycode_llm::StreamChunk::TextDelta(text.to_owned()),
            heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::Stop),
        ]
    }

    async fn write_json<W: AsyncWrite + Unpin>(writer: &mut W, value: Value) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
    }

    async fn read_json<R: AsyncBufRead + Unpin>(reader: &mut R) -> Value {
        let mut line = String::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reader.read_line(&mut line),
        )
        .await
        .expect("ACP fixture timed out")
        .unwrap();
        assert!(read > 0, "ACP fixture reached EOF");
        serde_json::from_str(line.trim()).unwrap()
    }

    async fn read_response<R: AsyncBufRead + Unpin>(reader: &mut R, id: u64) -> Value {
        loop {
            let value = read_json(reader).await;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }

    async fn read_permission_request<R: AsyncBufRead + Unpin>(reader: &mut R) -> Value {
        loop {
            let value = read_json(reader).await;
            if value.get("method").and_then(Value::as_str) == Some("session/request_permission") {
                return value;
            }
        }
    }

    #[tokio::test]
    async fn scripted_tool_permissions_correlate_allow_deny_cancel_and_resume_once() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let scripts = vec![
            permission_tool_script("allow-call"),
            final_script("allowed"),
            permission_tool_script("deny-call"),
            final_script("denied"),
            permission_tool_script("cancel-call"),
            final_script("cancelled permission"),
            final_script("still healthy"),
        ];
        let tool_runs = Arc::new(AtomicUsize::new(0));
        let tool_started = Arc::new(Notify::new());
        let factory_runs = Arc::clone(&tool_runs);
        let factory_started = Arc::clone(&tool_started);
        let factory_root = root_path.clone();
        let (client, server) = tokio::io::duplex(512 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let serve = tokio::spawn(serve_io(
            move |_cwd| {
                let mut config = heycode_config::Config::defaults();
                config.approval.mode = Some(heycode_config::ApprovalMode::Ask);
                let trust = heycode_trust::WorkspaceTrustService::memory(
                    &factory_root,
                    heycode_cli::project_content_policy(),
                )?;
                let context = heycode_cli::compose_world(&heycode_cli::WorldOptions {
                    config: &config,
                    trust,
                    config_migration: None,
                    profile_layers: &[],
                    sessions_dir: factory_root.join("sessions"),
                    attachments_dir: factory_root.join("attachments"),
                    attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
                    session_source: heycode_session::SessionSource::Acp,
                    approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
                    settings_user_path: factory_root.join("settings.toml"),
                    credentials_root: factory_root.join("credentials"),
                    catalog_cache_path: factory_root.join("cache/models.json"),
                    settings_watch: false,
                    onboarding_required: false,
                    credential_validated_at_ms: None,
                    cwd: factory_root.clone(),
                    fake: Some(Arc::new(heycode_llm::testing::FakeProvider::new(
                        scripts.clone(),
                    ))),
                    resume: None,
                })?;
                let registry = context
                    .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                    .ok_or_else(|| anyhow::anyhow!("tool registry missing"))?;
                registry.register_shared(Arc::new(PermissionProbeTool {
                    runs: Arc::clone(&factory_runs),
                    started: Arc::clone(&factory_started),
                }))?;
                Ok(context)
            },
            server_read,
            server_write,
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);

        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":1,"method":"session/new",
                "params":{"cwd":root_path,"mcpServers":[]}
            }),
        )
        .await;
        let created = read_response(&mut client_read, 1).await;
        let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();

        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":10,"method":"session/prompt",
                "params":{"sessionId":session_id,"prompt":"allow"}
            }),
        )
        .await;
        let allow = read_permission_request(&mut client_read).await;
        let allow_id = allow["id"].as_u64().unwrap();
        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":allow_id + 999,
                "result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}
            }),
        )
        .await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                tool_started.notified(),
            )
            .await
            .is_err(),
            "a response with the wrong id resumed the tool"
        );
        assert_eq!(tool_runs.load(Ordering::SeqCst), 0);
        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":allow_id,
                "result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}
            }),
        )
        .await;
        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":allow_id,
                "result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}
            }),
        )
        .await;
        assert_eq!(
            read_response(&mut client_read, 10).await["result"]["stopReason"],
            "end_turn"
        );
        assert_eq!(tool_runs.load(Ordering::SeqCst), 1);

        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":11,"method":"session/prompt",
                "params":{"sessionId":session_id,"prompt":"deny"}
            }),
        )
        .await;
        let deny = read_permission_request(&mut client_read).await;
        let deny_id = deny["id"].as_u64().unwrap();
        assert_ne!(deny_id, allow_id);
        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":deny_id,
                "result":{"outcome":{"outcome":"selected","optionId":"deny_once"}}
            }),
        )
        .await;
        assert_eq!(
            read_response(&mut client_read, 11).await["result"]["stopReason"],
            "end_turn"
        );
        assert_eq!(tool_runs.load(Ordering::SeqCst), 1);

        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":12,"method":"session/prompt",
                "params":{"sessionId":session_id,"prompt":"cancel permission"}
            }),
        )
        .await;
        let cancel = read_permission_request(&mut client_read).await;
        let cancel_id = cancel["id"].as_u64().unwrap();
        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":cancel_id,
                "result":{"outcome":{"outcome":"cancelled"}}
            }),
        )
        .await;
        assert_eq!(
            read_response(&mut client_read, 12).await["result"]["stopReason"],
            "end_turn"
        );
        assert_eq!(tool_runs.load(Ordering::SeqCst), 1);

        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":cancel_id,
                "result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}
            }),
        )
        .await;
        write_json(
            &mut client_write,
            serde_json::json!({
                "jsonrpc":"2.0","id":13,"method":"session/prompt",
                "params":{"sessionId":session_id,"prompt":"after late response"}
            }),
        )
        .await;
        assert_eq!(
            read_response(&mut client_read, 13).await["result"]["stopReason"],
            "end_turn"
        );
        assert_eq!(tool_runs.load(Ordering::SeqCst), 1);

        client_write.shutdown().await.unwrap();
        drop(client_write);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), serve)
                .await
                .expect("ACP permission fixture did not settle")
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[test]
    fn permission_decision_requires_the_exact_offered_selected_option() {
        assert!(permission_response_allows(&serde_json::json!({
            "result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}
        })));
        for response in [
            serde_json::json!({
                "result":{"outcome":{"outcome":"selected","optionId":"allow_forever"}}
            }),
            serde_json::json!({
                "result":{"outcome":{"outcome":"cancelled","optionId":"allow_once"}}
            }),
            serde_json::json!({"result":{"optionId":"allow_once"}}),
        ] {
            assert!(!permission_response_allows(&response));
        }
    }

    #[tokio::test]
    async fn rich_runtime_events_map_to_schema_v1_tool_plan_usage_and_untrusted_updates() {
        let (state, mut output) = state();
        let mut pump = PromptPump::new(Vec::new());
        let events = vec![
            RuntimeEvent::new(
                1,
                RuntimeEventKind::ToolCall {
                    call_id: heycode_core::CallId::from_raw("call_1"),
                    name: "web_fetch".to_owned(),
                    arguments: serde_json::json!({"url":"https://example.test"}),
                },
            ),
            RuntimeEvent::new(
                2,
                RuntimeEventKind::Notice {
                    code: "native.plan.enabled".to_owned(),
                    message: "Plan mode is active.".to_owned(),
                },
            ),
            RuntimeEvent::new(
                3,
                RuntimeEventKind::Notice {
                    code: "content.untrusted.web".to_owned(),
                    message: "untrusted".to_owned(),
                },
            ),
            RuntimeEvent::new(
                4,
                RuntimeEventKind::ToolResult {
                    call_id: heycode_core::CallId::from_raw("call_1"),
                    result: serde_json::json!("external"),
                    is_error: false,
                },
            ),
            RuntimeEvent::new(
                5,
                RuntimeEventKind::Usage {
                    usage: heycode_core::TokenUsage {
                        prompt_tokens: 8,
                        completion_tokens: 4,
                    },
                    context: None,
                },
            ),
        ];
        for event in events {
            process_runtime_event(&state, "session_1", event, 128_000, &mut pump).await;
        }
        let mut updates = Vec::new();
        while let Ok(frame) = output.try_recv() {
            updates.push(serde_json::from_str::<Value>(&frame).unwrap());
        }
        let kinds = updates
            .iter()
            .filter_map(|frame| frame.pointer("/params/update/sessionUpdate")?.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            ["tool_call", "plan", "tool_call_update", "usage_update"]
        );
        let tool = updates
            .iter()
            .find(|frame| {
                frame.pointer("/params/update/sessionUpdate")
                    == Some(&Value::String("tool_call_update".to_owned()))
            })
            .unwrap();
        assert_eq!(
            tool.pointer("/params/update/_meta/heycode/untrustedContent/source")
                .and_then(Value::as_str),
            Some("web")
        );
        let usage = updates.last().unwrap();
        assert_eq!(
            usage.pointer("/params/update/used").and_then(Value::as_u64),
            Some(12)
        );
        assert_eq!(
            usage.pointer("/params/update/size").and_then(Value::as_u64),
            Some(128_000)
        );
    }

    #[test]
    fn image_content_block_is_bounded_admitted_and_retained_for_runtime_input() {
        let (_root, _context, store) = attachment_world();
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let params = serde_json::json!({"prompt":[
            {"type":"text","text":"describe"},
            {"type":"image","data":png,"mimeType":"image/png"}
        ]});
        let parsed = parse_prompt(params.get("prompt"), &store, &CancellationToken::new()).unwrap();
        assert_eq!(parsed.input.text(), "describe");
        assert_eq!(parsed.input.attachments().len(), 1);
        assert_eq!(
            parsed.input.attachments()[0].media_type().as_str(),
            "image/png"
        );
        assert_eq!(parsed.echo_blocks[1]["type"], "image");
    }

    struct CancelRuntime {
        id: RuntimeSessionId,
        runtime_id: AgentRuntimeId,
        capabilities: RuntimeCapabilities,
        sender: broadcast::Sender<RuntimeEvent>,
        started: Notify,
        cancel_calls: AtomicUsize,
    }

    impl CancelRuntime {
        fn new() -> Arc<Self> {
            let (sender, _) = broadcast::channel(16);
            Arc::new(Self {
                id: RuntimeSessionId::new("cancel-session").unwrap(),
                runtime_id: AgentRuntimeId::new("native").unwrap(),
                capabilities: RuntimeCapabilities::unknown(),
                sender,
                started: Notify::new(),
                cancel_calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl RuntimeSession for CancelRuntime {
        fn id(&self) -> &RuntimeSessionId {
            &self.id
        }

        fn runtime_id(&self) -> &AgentRuntimeId {
            &self.runtime_id
        }

        fn capabilities(&self) -> &RuntimeCapabilities {
            &self.capabilities
        }

        fn subscribe(&self) -> RuntimeEventStream {
            let receiver = self.sender.subscribe();
            Box::pin(futures::stream::unfold(
                receiver,
                |mut receiver| async move {
                    match receiver.recv().await {
                        Ok(event) => Some((Ok(event), receiver)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            Some((Err(RuntimeError::protocol()), receiver))
                        }
                        Err(broadcast::error::RecvError::Closed) => None,
                    }
                },
            ))
        }

        async fn send(
            &self,
            _input: RuntimeInput,
            cancellation: CancellationToken,
        ) -> Result<RuntimeTurnId, RuntimeError> {
            let turn = RuntimeTurnId::new("1").unwrap();
            let _sent = self.sender.send(RuntimeEvent::new(
                1,
                RuntimeEventKind::TurnStarted { turn: turn.clone() },
            ));
            self.started.notify_one();
            cancellation.cancelled().await;
            let _sent = self.sender.send(RuntimeEvent::new(
                2,
                RuntimeEventKind::TurnFinished {
                    turn,
                    reason: RuntimeFinishReason::Cancelled,
                },
            ));
            Err(RuntimeError::cancelled())
        }

        async fn steer(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn follow_up(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn cancel(&self, _cancellation: CancellationToken) -> Result<(), RuntimeError> {
            self.cancel_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn respond_permission(
            &self,
            _response: RuntimePermissionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn respond_question(
            &self,
            _response: RuntimeQuestionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn compact(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<RuntimeCompactOutcome, RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn close(&self, _cancellation: CancellationToken) -> Result<(), RuntimeError> {
            Ok(())
        }
    }

    /// Session whose events go through a real event hub, so a prompt long
    /// enough to pass `RUNTIME_EVENT_HISTORY` trims and renumbers the retained
    /// window exactly as a production adapter does. The first prompt emits
    /// `deltas` commentary events; every later prompt emits one.
    struct HubSession {
        id: RuntimeSessionId,
        runtime: AgentRuntimeId,
        capabilities: RuntimeCapabilities,
        hub: heycode_runtime::RuntimeEventHub,
        prompts: AtomicU64,
        deltas: usize,
    }

    impl HubSession {
        fn new(deltas: usize) -> Arc<Self> {
            let session = Self {
                id: RuntimeSessionId::new("hub-session").unwrap(),
                runtime: AgentRuntimeId::new("native").unwrap(),
                capabilities: RuntimeCapabilities::unknown(),
                hub: heycode_runtime::RuntimeEventHub::new(),
                prompts: AtomicU64::new(0),
                deltas,
            };
            session.hub.emit(RuntimeEventKind::SessionReady).unwrap();
            Arc::new(session)
        }
    }

    #[async_trait]
    impl RuntimeSession for HubSession {
        fn id(&self) -> &RuntimeSessionId {
            &self.id
        }

        fn runtime_id(&self) -> &AgentRuntimeId {
            &self.runtime
        }

        fn capabilities(&self) -> &RuntimeCapabilities {
            &self.capabilities
        }

        fn subscribe(&self) -> RuntimeEventStream {
            self.hub.subscribe()
        }

        async fn send(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<RuntimeTurnId, RuntimeError> {
            let index = self.prompts.fetch_add(1, Ordering::SeqCst);
            let turn = RuntimeTurnId::new(format!("turn-{index}")).unwrap();
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
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn follow_up(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn cancel(&self, _cancellation: CancellationToken) -> Result<(), RuntimeError> {
            Ok(())
        }

        async fn respond_permission(
            &self,
            _response: RuntimePermissionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn respond_question(
            &self,
            _response: RuntimeQuestionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn compact(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<RuntimeCompactOutcome, RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn close(&self, _cancellation: CancellationToken) -> Result<(), RuntimeError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_prompt_after_the_retained_event_window_still_streams_and_settles() {
        let (_root, _context, store) = attachment_world();
        let session: Arc<dyn RuntimeSession> =
            HubSession::new(2 * heycode_runtime::RUNTIME_EVENT_HISTORY);
        let (state, mut output) = state();
        let drain = tokio::spawn(async move {
            let mut texts = Vec::new();
            while let Some(frame) = output.recv().await {
                let value: Value = serde_json::from_str(&frame).unwrap();
                if value
                    .pointer("/params/update/sessionUpdate")
                    .and_then(Value::as_str)
                    == Some("agent_message_chunk")
                {
                    texts.push(
                        value
                            .pointer("/params/update/content/text")
                            .and_then(Value::as_str)
                            .unwrap()
                            .to_owned(),
                    );
                }
            }
            texts
        });
        let events = Arc::new(Mutex::new(session.subscribe()));
        for text in ["first", "second"] {
            let stop = run_prompt(
                &state,
                "hub-session",
                &serde_json::json!({"prompt":[{"type":"text","text":text}]}),
                PromptRun {
                    runtime: session.clone(),
                    attachments: store.clone(),
                    context_window: 128_000,
                    events: events.clone(),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
            assert_eq!(stop["stopReason"], "end_turn");
        }
        drop(state);
        let texts = drain.await.unwrap();
        assert!(texts.len() > heycode_runtime::RUNTIME_EVENT_HISTORY);
        assert!(texts.contains(&"0:0".to_owned()));
        assert_eq!(texts.last().map(String::as_str), Some("1:0"));
    }

    #[tokio::test]
    async fn cancelled_runtime_prompt_returns_the_required_cancelled_stop_reason() {
        let (_root, _context, store) = attachment_world();
        let runtime = CancelRuntime::new();
        let (state, _output) = state();
        let cancellation = CancellationToken::new();
        let params = serde_json::json!({"prompt":[{"type":"text","text":"wait"}]});
        let run = run_prompt(
            &state,
            "cancel-session",
            &params,
            PromptRun {
                runtime: runtime.clone(),
                attachments: store,
                context_window: 128_000,
                events: Arc::new(Mutex::new(runtime.subscribe())),
                cancellation: cancellation.clone(),
            },
        );
        tokio::pin!(run);
        tokio::select! {
            () = runtime.started.notified() => {}
            result = &mut run => panic!("prompt settled before cancellation: {result:?}"),
        }
        cancellation.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), run)
            .await
            .expect("cancelled ACP prompt did not settle")
            .unwrap();
        assert_eq!(result["stopReason"], "cancelled");
    }

    #[tokio::test]
    async fn session_cancel_interrupts_a_live_prompt_and_server_shutdown_is_quiescent() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let factory_root = root_path.clone();
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let serve = tokio::spawn(serve_io(
            move |_cwd| Ok(hanging_world(&factory_root)),
            server_read,
            server_write,
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);

        client_write
            .write_all(
                serde_json::json!({
                    "jsonrpc":"2.0","id":1,"method":"session/new",
                    "params":{"cwd":root_path,"mcpServers":[]}
                })
                .to_string()
                .as_bytes(),
            )
            .await
            .unwrap();
        client_write.write_all(b"\n").await.unwrap();
        client_write.flush().await.unwrap();
        let mut line = String::new();
        client_read.read_line(&mut line).await.unwrap();
        let created: Value = serde_json::from_str(line.trim()).unwrap();
        let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();

        client_write
            .write_all(
                serde_json::json!({
                    "jsonrpc":"2.0","id":2,"method":"session/prompt",
                    "params":{"sessionId":session_id,"prompt":[{"type":"text","text":"wait"}]}
                })
                .to_string()
                .as_bytes(),
            )
            .await
            .unwrap();
        client_write.write_all(b"\n").await.unwrap();
        client_write.flush().await.unwrap();
        loop {
            line.clear();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client_read.read_line(&mut line),
            )
            .await
            .expect("ACP hanging prompt produced no update")
            .unwrap();
            let value: Value = serde_json::from_str(line.trim()).unwrap();
            if value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                == Some("working")
            {
                break;
            }
        }
        client_write
            .write_all(
                serde_json::json!({
                    "jsonrpc":"2.0","method":"session/cancel",
                    "params":{"sessionId":session_id}
                })
                .to_string()
                .as_bytes(),
            )
            .await
            .unwrap();
        client_write.write_all(b"\n").await.unwrap();
        client_write.flush().await.unwrap();
        let stop = loop {
            line.clear();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client_read.read_line(&mut line),
            )
            .await
            .expect("ACP cancel produced no response")
            .unwrap();
            let value: Value = serde_json::from_str(line.trim()).unwrap();
            if value.get("id").and_then(Value::as_u64) == Some(2) {
                break value;
            }
        };
        assert_eq!(stop["result"]["stopReason"], "cancelled");
        client_write.shutdown().await.unwrap();
        drop(client_write);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), serve)
                .await
                .expect("ACP server did not settle after EOF")
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[test]
    fn runtime_input_debug_never_exposes_prompt_text() {
        let input = RuntimeInput::new("private prompt").unwrap();
        assert!(!format!("{input:?}").contains("private prompt"));
    }
}

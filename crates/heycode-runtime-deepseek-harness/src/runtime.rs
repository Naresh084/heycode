//! Harness SDK runtime, session lifecycle, event validation and R02 mapping.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::Read as _;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};

use async_trait::async_trait;
use futures::Stream;
use futures::channel::mpsc;
use heycode_core::{CallId, TokenUsage};
use heycode_exec::SubprocessService;
use heycode_llm::{CapabilitySupport, CatalogSnapshot};
use heycode_runtime::{
    AccountState, AccountStatus, AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeId,
    AgentRuntimeKind, NormalizedRuntimeEvent, RuntimeCapabilities, RuntimeCompactOutcome,
    RuntimeConfiguration, RuntimeConfigurationCapabilities, RuntimeError, RuntimeErrorCode,
    RuntimeEvent, RuntimeEventKind, RuntimeEventNormalizer, RuntimeEventStream,
    RuntimeFinishReason, RuntimeFork, RuntimeInput, RuntimePermissionResponse,
    RuntimeQuestionResponse, RuntimeResume, RuntimeSession, RuntimeSessionId, RuntimeStart,
    RuntimeTurnId,
};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::DEEPSEEK_HARNESS_RUNTIME_ID;
use crate::config::DeepSeekHarnessRuntimeConfig;
use crate::process::spawn_sdk_process;
use crate::protocol::{
    HANDLED_DSH_SESSION_EVENT_TYPES, IGNORED_DSH_SESSION_EVENT_TYPES,
    PINNED_DSH_SESSION_EVENT_TYPES, SdkPeer, validate_text,
};

const EVENT_HISTORY_CAPACITY: usize = 1024;
const EVENT_SUBSCRIBER_CAPACITY: usize = 1024;
const MAX_TRACKED_CHILDREN: usize = 256;
const MAX_RECEIPT_IDS: usize = 256;
const MAX_CONTENT_BLOCKS: usize = 1024;
const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;

/// Delegated DeepSeek Harness SDK runtime over one process per heycode session.
pub struct DeepSeekHarnessRuntime {
    subprocess: SubprocessService,
    program: PathBuf,
    executable_digest: [u8; 32],
    artifacts: Vec<BoundArtifact>,
    config: Arc<DeepSeekHarnessRuntimeConfig>,
    descriptor: AgentRuntimeDescriptor,
    lifecycle: CancellationToken,
}

impl DeepSeekHarnessRuntime {
    /// Bind an exact resolved executable to the SDK runtime contract.
    ///
    /// # Errors
    /// Invalid descriptors or non-absolute/unreadable program paths fail before
    /// publication.
    pub fn new(
        subprocess: SubprocessService,
        program: PathBuf,
        config: DeepSeekHarnessRuntimeConfig,
    ) -> Result<Self, heycode_runtime::RuntimeContractError> {
        if !program.is_absolute() {
            return Err(heycode_runtime::RuntimeContractError::InvalidField {
                field: "DeepSeek Harness resolved program",
                requirement: "an absolute executable path",
            });
        }
        let executable_digest = hash_bound_file(&program).map_err(|_| {
            heycode_runtime::RuntimeContractError::InvalidField {
                field: "DeepSeek Harness resolved program",
                requirement: "a readable non-empty regular file at most 512 MiB",
            }
        })?;
        let artifacts = config
            .artifacts()
            .iter()
            .map(|path| {
                hash_bound_file(path)
                    .map(|digest| BoundArtifact {
                        path: path.clone(),
                        digest,
                    })
                    .map_err(|_| heycode_runtime::RuntimeContractError::InvalidField {
                        field: "DeepSeek Harness artifact",
                        requirement: "a readable non-empty regular file at most 512 MiB",
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let descriptor = deepseek_harness_runtime_descriptor()?;
        Ok(Self {
            subprocess,
            program,
            executable_digest,
            artifacts,
            config: Arc::new(config),
            descriptor,
            lifecycle: CancellationToken::new(),
        })
    }

    /// Cancel every process owned by this runtime plugin generation.
    pub fn shutdown(&self) {
        self.lifecycle.cancel();
    }

    fn verify_launch_identity(&self) -> Result<(), RuntimeError> {
        if hash_bound_file(&self.program)? == self.executable_digest {
            for artifact in &self.artifacts {
                if hash_bound_file(&artifact.path)? != artifact.digest {
                    return Err(RuntimeError::protocol());
                }
            }
            return Ok(());
        }
        Err(RuntimeError::protocol())
    }
}

struct BoundArtifact {
    path: PathBuf,
    digest: [u8; 32],
}

/// Construct the immutable capability row shared by available and missing
/// optional Harness SDK registrations.
///
/// # Errors
/// The compile-time descriptor constants must satisfy runtime validation.
pub fn deepseek_harness_runtime_descriptor()
-> Result<AgentRuntimeDescriptor, heycode_runtime::RuntimeContractError> {
    let unsupported = CapabilitySupport::Unsupported;
    AgentRuntimeDescriptor::new(
        DEEPSEEK_HARNESS_RUNTIME_ID,
        "DeepSeek Harness SDK",
        AgentRuntimeKind::Delegated,
        RuntimeCapabilities {
            models: unsupported,
            resume: unsupported,
            fork: unsupported,
            steer: unsupported,
            follow_up: unsupported,
            permissions: unsupported,
            questions: unsupported,
            compaction: unsupported,
        },
    )
    .map(|descriptor| {
        descriptor.with_configuration_capabilities(RuntimeConfigurationCapabilities {
            system_prompt: unsupported,
            tools: unsupported,
            model: CapabilitySupport::Supported,
            reasoning_effort: unsupported,
        })
    })
}

fn validate_configuration(configuration: &RuntimeConfiguration) -> Result<(), RuntimeError> {
    let mut unsupported = Vec::new();
    if configuration.system_prompt().is_some() {
        unsupported.push("system_prompt");
    }
    if configuration.tools_configured() {
        unsupported.push("tools");
    }
    if configuration.reasoning_effort().is_some() {
        unsupported.push("reasoning_effort");
    }
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError::unsupported_field_names(&unsupported))
    }
}

#[async_trait]
impl AgentRuntime for DeepSeekHarnessRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.descriptor
    }

    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if self.lifecycle.is_cancelled() {
            return Err(RuntimeError::closed());
        }
        Ok(AccountState::without_label(AccountStatus::Unknown))
    }

    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn start(
        &self,
        request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if self.lifecycle.is_cancelled() {
            return Err(RuntimeError::closed());
        }
        if request.ephemeral() {
            return Err(RuntimeError::unsupported());
        }
        validate_configuration(request.configuration())?;
        self.verify_launch_identity()?;
        let session_id = RuntimeSessionId::new(request.session_id().as_str().to_owned())
            .map_err(|_| RuntimeError::invalid_request())?;
        let model = request.model().unwrap_or_else(|| self.config.model());
        let lifecycle = self.lifecycle.child_token();
        let process = spawn_sdk_process(
            &self.subprocess,
            &self.program,
            self.config.args(),
            self.config.environment(),
            request.workspace(),
            lifecycle.clone(),
            cancellation.clone(),
        )
        .await?;
        let peer = Arc::new(SdkPeer::new(process)?);
        let initialized = peer
            .initialize(
                request.workspace(),
                self.config.provider(),
                model,
                self.config.max_tokens(),
                cancellation,
            )
            .await;
        if let Err(error) = initialized {
            let _closed = peer.close(CancellationToken::new()).await;
            return Err(error);
        }
        let session = DeepSeekHarnessSession::new(
            peer,
            session_id,
            self.descriptor.id().clone(),
            self.descriptor.capabilities().clone(),
            lifecycle,
        )?;
        Ok(Arc::new(session))
    }

    async fn resume(
        &self,
        _request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn fork(
        &self,
        _request: RuntimeFork,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }
}

fn hash_bound_file(path: &std::path::Path) -> Result<[u8; 32], RuntimeError> {
    let metadata = std::fs::metadata(path).map_err(|_| RuntimeError::unavailable())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_EXECUTABLE_BYTES {
        return Err(RuntimeError::unavailable());
    }
    let mut file = File::open(path).map_err(|_| RuntimeError::unavailable())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| RuntimeError::unavailable())?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(RuntimeError::unavailable)?;
        if total > MAX_EXECUTABLE_BYTES {
            return Err(RuntimeError::unavailable());
        }
        hasher.update(&buffer[..read]);
    }
    if total != metadata.len() {
        return Err(RuntimeError::protocol());
    }
    Ok(hasher.finalize().into())
}

struct Subscriber {
    sender: mpsc::Sender<Result<RuntimeEvent, RuntimeError>>,
    terminal: Arc<Mutex<Option<RuntimeError>>>,
}

struct HubState {
    history: VecDeque<RuntimeEvent>,
    subscribers: Vec<Subscriber>,
    closed: bool,
    terminal_error: Option<RuntimeError>,
}

struct EventHub {
    state: Mutex<HubState>,
}

impl EventHub {
    fn new() -> Self {
        Self {
            state: Mutex::new(HubState {
                history: VecDeque::new(),
                subscribers: Vec::new(),
                closed: false,
                terminal_error: None,
            }),
        }
    }

    fn publish(&self, event: RuntimeEvent) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("event hub unavailable"))?;
        if state.closed {
            return Err(RuntimeError::closed());
        }
        state.history.push_back(event.clone());
        if state.history.len() > EVENT_HISTORY_CAPACITY {
            state.history.pop_front();
        }
        state.subscribers.retain_mut(|subscriber| {
            match subscriber.sender.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(error) if error.is_full() => {
                    if let Ok(mut terminal) = subscriber.terminal.lock() {
                        *terminal = Some(RuntimeError::protocol());
                    }
                    subscriber.sender.close_channel();
                    false
                }
                Err(_) => false,
            }
        });
        Ok(())
    }

    fn fail(&self, error: RuntimeError) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        state.closed = true;
        state.terminal_error = Some(error.clone());
        for mut subscriber in state.subscribers.drain(..) {
            if let Ok(mut terminal) = subscriber.terminal.lock() {
                *terminal = Some(error.clone());
            }
            subscriber.sender.close_channel();
        }
    }

    fn close(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        state.closed = true;
        for mut subscriber in state.subscribers.drain(..) {
            subscriber.sender.close_channel();
        }
    }

    fn subscribe(&self) -> RuntimeEventStream {
        let (sender, receiver) = mpsc::channel(EVENT_SUBSCRIBER_CAPACITY);
        let terminal = Arc::new(Mutex::new(None));
        let mut history = VecDeque::new();
        let mut closed = false;
        match self.state.lock() {
            Ok(mut state) => {
                history.extend(state.history.iter().cloned().map(Ok));
                if state.closed {
                    closed = true;
                    if let Some(error) = &state.terminal_error
                        && let Ok(mut slot) = terminal.lock()
                    {
                        *slot = Some(error.clone());
                    }
                } else {
                    state.subscribers.push(Subscriber {
                        sender,
                        terminal: Arc::clone(&terminal),
                    });
                }
            }
            Err(_) => {
                closed = true;
                if let Ok(mut slot) = terminal.lock() {
                    *slot = Some(RuntimeError::internal("event hub unavailable"));
                }
            }
        }
        Box::pin(HubSubscription {
            history,
            receiver: if closed { None } else { Some(receiver) },
            terminal,
            terminal_emitted: false,
        })
    }
}

struct HubSubscription {
    history: VecDeque<Result<RuntimeEvent, RuntimeError>>,
    receiver: Option<mpsc::Receiver<Result<RuntimeEvent, RuntimeError>>>,
    terminal: Arc<Mutex<Option<RuntimeError>>>,
    terminal_emitted: bool,
}

impl Stream for HubSubscription {
    type Item = Result<RuntimeEvent, RuntimeError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(item) = this.history.pop_front() {
            return Poll::Ready(Some(item));
        }
        if let Some(receiver) = &mut this.receiver {
            match Pin::new(receiver).poll_next(context) {
                Poll::Ready(Some(item)) => return Poll::Ready(Some(item)),
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => this.receiver = None,
            }
        }
        if !this.terminal_emitted {
            let error = this
                .terminal
                .lock()
                .ok()
                .and_then(|terminal| terminal.clone());
            if let Some(error) = error {
                this.terminal_emitted = true;
                return Poll::Ready(Some(Err(error)));
            }
        }
        Poll::Ready(None)
    }
}

struct EventState {
    next_sequence: u64,
    normalizer: RuntimeEventNormalizer,
}

struct ActiveOperation {
    cancellation: CancellationToken,
}

struct DeepSeekHarnessSession {
    peer: Arc<SdkPeer>,
    id: RuntimeSessionId,
    runtime_id: AgentRuntimeId,
    capabilities: RuntimeCapabilities,
    hub: EventHub,
    events: Mutex<EventState>,
    external_sequences: Mutex<BTreeMap<String, u64>>,
    children: Mutex<BTreeSet<String>>,
    operation_gate: AsyncMutex<()>,
    close_gate: AsyncMutex<()>,
    active: Mutex<Option<Arc<ActiveOperation>>>,
    lifecycle: CancellationToken,
    closing: AtomicBool,
    closed: AtomicBool,
}

impl DeepSeekHarnessSession {
    fn new(
        peer: Arc<SdkPeer>,
        id: RuntimeSessionId,
        runtime_id: AgentRuntimeId,
        capabilities: RuntimeCapabilities,
        lifecycle: CancellationToken,
    ) -> Result<Self, RuntimeError> {
        let mut external_sequences = BTreeMap::new();
        external_sequences.insert(id.as_str().to_owned(), 0);
        let session = Self {
            peer,
            id,
            runtime_id,
            capabilities,
            hub: EventHub::new(),
            events: Mutex::new(EventState {
                next_sequence: 0,
                normalizer: RuntimeEventNormalizer::new(),
            }),
            external_sequences: Mutex::new(external_sequences),
            children: Mutex::new(BTreeSet::new()),
            operation_gate: AsyncMutex::new(()),
            close_gate: AsyncMutex::new(()),
            active: Mutex::new(None),
            lifecycle,
            closing: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        };
        session.publish(RuntimeEventKind::SessionReady)?;
        Ok(session)
    }

    fn publish(&self, kind: RuntimeEventKind) -> Result<NormalizedRuntimeEvent, RuntimeError> {
        let mut state = self
            .events
            .lock()
            .map_err(|_| RuntimeError::internal("event state unavailable"))?;
        let event = RuntimeEvent::new(state.next_sequence, kind);
        let event = state
            .normalizer
            .push(event)
            .map_err(|_| RuntimeError::protocol())?;
        state.next_sequence = state
            .next_sequence
            .checked_add(1)
            .ok_or_else(RuntimeError::protocol)?;
        let raw = event.clone().into_event();
        drop(state);
        self.hub.publish(raw)?;
        Ok(event)
    }

    async fn terminate(&self, error: RuntimeError) -> RuntimeError {
        self.closing.store(true, Ordering::SeqCst);
        self.closed.store(true, Ordering::SeqCst);
        self.lifecycle.cancel();
        self.hub.fail(error.clone());
        let _closed = self.peer.close(CancellationToken::new()).await;
        error
    }

    fn set_active(&self, active: Arc<ActiveOperation>) -> Result<ActiveGuard<'_>, RuntimeError> {
        let mut slot = self
            .active
            .lock()
            .map_err(|_| RuntimeError::internal("operation state unavailable"))?;
        if slot.is_some() {
            return Err(RuntimeError::conflict());
        }
        *slot = Some(Arc::clone(&active));
        Ok(ActiveGuard {
            slot: &self.active,
            active,
        })
    }

    fn active_operation(&self) -> Result<Option<Arc<ActiveOperation>>, RuntimeError> {
        self.active
            .lock()
            .map(|active| active.clone())
            .map_err(|_| RuntimeError::internal("operation state unavailable"))
    }

    async fn send_prompt(
        &self,
        input: RuntimeInput,
        caller: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        if caller.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if self.closed.load(Ordering::SeqCst)
            || self.closing.load(Ordering::SeqCst)
            || self.lifecycle.is_cancelled()
        {
            return Err(RuntimeError::closed());
        }
        if !input.attachments().is_empty() {
            return Err(RuntimeError::unsupported());
        }
        let gate = lock_operation(&self.operation_gate, &caller).await?;
        if self.closed.load(Ordering::SeqCst) || self.closing.load(Ordering::SeqCst) {
            return Err(RuntimeError::closed());
        }
        let active = Arc::new(ActiveOperation {
            cancellation: self.lifecycle.child_token(),
        });
        let active_guard = self.set_active(Arc::clone(&active))?;
        let request_id = self.peer.next_request_id()?;
        let params = serde_json::json!({
            "sessionId":self.id.as_str(),
            "contentBlocks":[{"type":"text","text":input.text()}],
        });
        if let Err(error) = self
            .peer
            .write_request(request_id, "session/prompt", params, caller.clone())
            .await
        {
            return Err(self.terminate(error).await);
        }
        let mut prompt = PromptState::default();
        loop {
            let message = read_prompt_message(&self.peer, &active.cancellation, &caller).await;
            let message = match message {
                Ok(Some(message)) => message,
                Ok(None) => return Err(self.terminate(RuntimeError::protocol()).await),
                Err(error) if error.code() == RuntimeErrorCode::Cancelled => {
                    return Err(self.terminate(RuntimeError::cancelled()).await);
                }
                Err(error) => return Err(self.terminate(error).await),
            };
            let handled = if message.get("method").is_some() {
                self.handle_notification(&message, &mut prompt)
            } else {
                let result = self.peer.response_result(&message, request_id);
                result.and_then(|result| prompt.set_response(&result))
            };
            if let Err(error) = handled {
                return Err(self.terminate(error).await);
            }
            if prompt.is_complete()? {
                break;
            }
        }
        let turn = prompt.turn.clone().ok_or_else(RuntimeError::protocol)?;
        if let Some(text) = prompt.final_text()
            && let Err(error) = self.publish(RuntimeEventKind::FinalMessage { text })
        {
            return Err(self.terminate(error).await);
        }
        let reason = prompt.finish.ok_or_else(RuntimeError::protocol)?;
        if let Err(error) = self.publish(RuntimeEventKind::TurnFinished {
            turn: turn.clone(),
            reason,
        }) {
            return Err(self.terminate(error).await);
        }
        drop(active_guard);
        drop(gate);
        match reason {
            RuntimeFinishReason::Cancelled => Err(RuntimeError::cancelled()),
            RuntimeFinishReason::Error => Err(RuntimeError::unavailable()),
            RuntimeFinishReason::Stop | RuntimeFinishReason::Limit => Ok(turn),
        }
    }

    fn handle_notification(
        &self,
        message: &Value,
        prompt: &mut PromptState,
    ) -> Result<(), RuntimeError> {
        if message.get("id").is_some() {
            return Err(RuntimeError::protocol());
        }
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(RuntimeError::protocol)?;
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        match method {
            "session.event" => self.handle_session_event(params, prompt),
            "session.status" => self.handle_session_status(params, prompt),
            "subagent.started" => self.handle_subagent_started(params),
            "subagent.finished" => self.handle_subagent_finished(params),
            _ => Err(RuntimeError::protocol()),
        }
    }

    fn handle_session_event(
        &self,
        params: &Map<String, Value>,
        prompt: &mut PromptState,
    ) -> Result<(), RuntimeError> {
        let session_id = safe_field(params, "sessionId", 256)?;
        let event = params
            .get("event")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        let event_type = safe_field(event, "type", 128)?;
        let sequence = event
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(RuntimeError::protocol)?;
        event
            .get("time")
            .and_then(Value::as_u64)
            .ok_or_else(RuntimeError::protocol)?;
        let data = event
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        if event
            .get("ignorable")
            .is_some_and(|value| value != &Value::Bool(true))
        {
            return Err(RuntimeError::protocol());
        }
        self.advance_external_sequence(session_id, sequence)?;
        let known = PINNED_DSH_SESSION_EVENT_TYPES.contains(&event_type);
        if !known {
            return if event.get("ignorable") == Some(&Value::Bool(true)) {
                Ok(())
            } else {
                Err(RuntimeError::protocol())
            };
        }
        if session_id != self.id.as_str() {
            return Ok(());
        }
        if IGNORED_DSH_SESSION_EVENT_TYPES.contains(&event_type) {
            return Ok(());
        }
        if !HANDLED_DSH_SESSION_EVENT_TYPES.contains(&event_type) {
            return Err(RuntimeError::protocol());
        }
        match event_type {
            "agent/inbox/spliced" => prompt.observe_receipts(data),
            "turn/start" => {
                let turn = number_field(data, "turn")?;
                prompt.start_turn(turn)?;
                self.publish(RuntimeEventKind::TurnStarted {
                    turn: RuntimeTurnId::new(turn.to_string())
                        .map_err(|_| RuntimeError::protocol())?,
                })?;
                Ok(())
            }
            "assistant/chunk" => self.handle_assistant_chunk(data, prompt),
            "assistant/message" => self.handle_assistant_message(data, prompt),
            "tool/call" => self.handle_tool_call(data, prompt),
            "tool/result" => self.handle_tool_result(data, prompt),
            "turn/end" => prompt.end_turn(data),
            _ => Err(RuntimeError::protocol()),
        }
    }

    fn handle_session_status(
        &self,
        params: &Map<String, Value>,
        prompt: &mut PromptState,
    ) -> Result<(), RuntimeError> {
        let session_id = safe_field(params, "sessionId", 256)?;
        let status = safe_field(params, "status", 32)?;
        self.require_known_session(session_id)?;
        if session_id != self.id.as_str() {
            return Ok(());
        }
        match status {
            "running" if !prompt.running && !prompt.idle => prompt.running = true,
            "idle" if prompt.running && !prompt.idle => prompt.idle = true,
            "idle"
                if !prompt.running
                    && !prompt.idle
                    && prompt.receipt_ids.is_empty()
                    && prompt.response_message_id.is_none()
                    && prompt.turn.is_none() => {}
            _ => return Err(RuntimeError::protocol()),
        }
        Ok(())
    }

    fn handle_subagent_started(&self, params: &Map<String, Value>) -> Result<(), RuntimeError> {
        let parent = safe_field(params, "parentSessionId", 256)?;
        let child = safe_field(params, "childSessionId", 256)?;
        self.require_known_session(parent)?;
        let mut children = self
            .children
            .lock()
            .map_err(|_| RuntimeError::internal("child state unavailable"))?;
        if children.len() >= MAX_TRACKED_CHILDREN || !children.insert(child.to_owned()) {
            return Err(RuntimeError::protocol());
        }
        self.external_sequences
            .lock()
            .map_err(|_| RuntimeError::internal("sequence state unavailable"))?
            .insert(child.to_owned(), 0);
        Ok(())
    }

    fn handle_subagent_finished(&self, params: &Map<String, Value>) -> Result<(), RuntimeError> {
        let parent = safe_field(params, "parentSessionId", 256)?;
        let child = safe_field(params, "childSessionId", 256)?;
        let agent = safe_field(params, "agentId", 256)?;
        safe_field(params, "provider", 256)?;
        let status = safe_field(params, "status", 32)?;
        let stop_reason = safe_field(params, "stopReason", 64)?;
        if !matches!(status, "ok" | "error") || agent != child || stop_reason.is_empty() {
            return Err(RuntimeError::protocol());
        }
        self.require_known_session(parent)?;
        if let Some(blocks) = params.get("lastAssistantMessage") {
            validate_content_blocks(blocks)?;
        }
        let removed = self
            .children
            .lock()
            .map_err(|_| RuntimeError::internal("child state unavailable"))?
            .remove(child);
        if !removed {
            return Err(RuntimeError::protocol());
        }
        self.external_sequences
            .lock()
            .map_err(|_| RuntimeError::internal("sequence state unavailable"))?
            .remove(child);
        Ok(())
    }

    fn handle_assistant_chunk(
        &self,
        data: &Map<String, Value>,
        prompt: &mut PromptState,
    ) -> Result<(), RuntimeError> {
        prompt.require_turn(data)?;
        number_field(data, "step")?;
        let chunk = data
            .get("chunk")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        let kind = safe_field(chunk, "type", 64)?;
        match kind {
            "text-delta" => {
                number_field(chunk, "index")?;
                let text = safe_multiline_field(chunk, "text", 1024 * 1024)?;
                prompt.streamed_text.push_str(text);
                self.publish(RuntimeEventKind::CommentaryDelta {
                    text: text.to_owned(),
                })?;
            }
            "reasoning-delta" => {
                number_field(chunk, "index")?;
                let text = safe_multiline_field(chunk, "text", 1024 * 1024)?;
                self.publish(RuntimeEventKind::ReasoningDelta {
                    text: text.to_owned(),
                })?;
            }
            "block-start" | "block-end" | "tool-call-delta" | "usage" | "finish" => {}
            _ => return Err(RuntimeError::protocol()),
        }
        Ok(())
    }

    fn handle_assistant_message(
        &self,
        data: &Map<String, Value>,
        prompt: &mut PromptState,
    ) -> Result<(), RuntimeError> {
        prompt.require_turn(data)?;
        number_field(data, "step")?;
        let message = data
            .get("message")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return Err(RuntimeError::protocol());
        }
        let blocks = message.get("content").ok_or_else(RuntimeError::protocol)?;
        let text = content_text(blocks)?;
        if !text.is_empty() {
            prompt.last_assistant_text = Some(text);
        }
        if let Some(usage) = data.get("usage") {
            self.publish(RuntimeEventKind::Usage {
                usage: parse_usage(usage)?,
                context: None,
            })?;
        }
        Ok(())
    }

    fn handle_tool_call(
        &self,
        data: &Map<String, Value>,
        prompt: &mut PromptState,
    ) -> Result<(), RuntimeError> {
        prompt.require_turn(data)?;
        number_field(data, "step")?;
        let call_id = safe_field(data, "callId", 256)?;
        let name = safe_field(data, "name", 128)?;
        let arguments = safe_multiline_field(data, "arguments", 1024 * 1024)?;
        let arguments: Value =
            serde_json::from_str(arguments).map_err(|_| RuntimeError::protocol())?;
        validate_json_shape(&arguments)?;
        self.publish(RuntimeEventKind::ToolCall {
            call_id: CallId::from_raw(call_id),
            name: name.to_owned(),
            arguments,
        })?;
        Ok(())
    }

    fn handle_tool_result(
        &self,
        data: &Map<String, Value>,
        prompt: &mut PromptState,
    ) -> Result<(), RuntimeError> {
        prompt.require_turn(data)?;
        number_field(data, "step")?;
        let message = data
            .get("message")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            return Err(RuntimeError::protocol());
        }
        let call_id = safe_field(message, "toolCallId", 256)?;
        validate_content_blocks(message.get("content").ok_or_else(RuntimeError::protocol)?)?;
        let is_error = match message.get("isError") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err(RuntimeError::protocol()),
        };
        self.publish(RuntimeEventKind::ToolResult {
            call_id: CallId::from_raw(call_id),
            result: Value::Object(message.clone()),
            is_error,
        })?;
        Ok(())
    }

    fn advance_external_sequence(&self, session: &str, sequence: u64) -> Result<(), RuntimeError> {
        self.require_known_session(session)?;
        let mut sequences = self
            .external_sequences
            .lock()
            .map_err(|_| RuntimeError::internal("sequence state unavailable"))?;
        let next = sequences
            .get_mut(session)
            .ok_or_else(RuntimeError::protocol)?;
        if *next != sequence {
            return Err(RuntimeError::protocol());
        }
        *next = next.checked_add(1).ok_or_else(RuntimeError::protocol)?;
        Ok(())
    }

    fn require_known_session(&self, session: &str) -> Result<(), RuntimeError> {
        if session == self.id.as_str() {
            return Ok(());
        }
        let known = self
            .children
            .lock()
            .map_err(|_| RuntimeError::internal("child state unavailable"))?
            .contains(session);
        if known {
            Ok(())
        } else {
            Err(RuntimeError::protocol())
        }
    }
}

struct ActiveGuard<'a> {
    slot: &'a Mutex<Option<Arc<ActiveOperation>>>,
    active: Arc<ActiveOperation>,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        let Ok(mut slot) = self.slot.lock() else {
            return;
        };
        if slot
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.active))
        {
            *slot = None;
        }
    }
}

#[derive(Default)]
struct PromptState {
    response_message_id: Option<String>,
    receipt_ids: BTreeSet<String>,
    running: bool,
    idle: bool,
    turn_number: Option<u64>,
    turn: Option<RuntimeTurnId>,
    streamed_text: String,
    last_assistant_text: Option<String>,
    finish: Option<RuntimeFinishReason>,
}

impl PromptState {
    fn set_response(&mut self, result: &Value) -> Result<(), RuntimeError> {
        if self.response_message_id.is_some() {
            return Err(RuntimeError::protocol());
        }
        let object = result.as_object().ok_or_else(RuntimeError::protocol)?;
        if object.len() != 1 {
            return Err(RuntimeError::protocol());
        }
        self.response_message_id = Some(safe_field(object, "messageId", 256)?.to_owned());
        Ok(())
    }

    fn observe_receipts(&mut self, data: &Map<String, Value>) -> Result<(), RuntimeError> {
        let inserted = data
            .get("inserted")
            .and_then(Value::as_array)
            .ok_or_else(RuntimeError::protocol)?;
        if inserted.len() > MAX_RECEIPT_IDS {
            return Err(RuntimeError::protocol());
        }
        for message in inserted {
            let message = message.as_object().ok_or_else(RuntimeError::protocol)?;
            let id = safe_field(message, "id", 256)?;
            if !self.receipt_ids.insert(id.to_owned()) {
                return Err(RuntimeError::protocol());
            }
        }
        Ok(())
    }

    fn start_turn(&mut self, turn: u64) -> Result<(), RuntimeError> {
        if self.turn.is_some() || self.finish.is_some() {
            return Err(RuntimeError::protocol());
        }
        self.turn_number = Some(turn);
        self.turn =
            Some(RuntimeTurnId::new(turn.to_string()).map_err(|_| RuntimeError::protocol())?);
        Ok(())
    }

    fn require_turn(&self, data: &Map<String, Value>) -> Result<(), RuntimeError> {
        let turn = number_field(data, "turn")?;
        if self.turn_number != Some(turn) || self.finish.is_some() {
            return Err(RuntimeError::protocol());
        }
        Ok(())
    }

    fn end_turn(&mut self, data: &Map<String, Value>) -> Result<(), RuntimeError> {
        self.require_turn(data)?;
        let reason = data
            .get("reason")
            .and_then(Value::as_object)
            .and_then(|reason| reason.get("kind"))
            .and_then(Value::as_str)
            .ok_or_else(RuntimeError::protocol)?;
        self.finish = Some(match reason {
            "completed" => RuntimeFinishReason::Stop,
            "max-tokens" => RuntimeFinishReason::Limit,
            "aborted" => RuntimeFinishReason::Cancelled,
            "blocked" | "error" | "interrupted" => RuntimeFinishReason::Error,
            _ => return Err(RuntimeError::protocol()),
        });
        Ok(())
    }

    fn final_text(&self) -> Option<String> {
        self.last_assistant_text
            .clone()
            .or_else(|| (!self.streamed_text.is_empty()).then(|| self.streamed_text.clone()))
    }

    fn is_complete(&self) -> Result<bool, RuntimeError> {
        let Some(message_id) = &self.response_message_id else {
            return Ok(false);
        };
        if self.idle
            && self.turn.is_some()
            && self.finish.is_some()
            && !self.receipt_ids.contains(message_id)
        {
            return Err(RuntimeError::protocol());
        }
        Ok(self.running
            && self.idle
            && self.turn.is_some()
            && self.finish.is_some()
            && self.receipt_ids.contains(message_id))
    }
}

#[async_trait]
impl RuntimeSession for DeepSeekHarnessSession {
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
        self.hub.subscribe()
    }

    async fn send(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        self.send_prompt(input, cancellation).await
    }

    async fn steer(
        &self,
        _input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn follow_up(
        &self,
        _input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn respond_permission(
        &self,
        _response: RuntimePermissionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn respond_question(
        &self,
        _response: RuntimeQuestionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn compact(
        &self,
        cancellation: CancellationToken,
    ) -> Result<RuntimeCompactOutcome, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Err(RuntimeError::unsupported())
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        let _close = self.close_gate.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.closing.store(true, Ordering::SeqCst);
        if let Some(active) = self.active_operation()? {
            active.cancellation.cancel();
        }
        let _operation = self.operation_gate.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let caller_cancelled = cancellation.is_cancelled();
        let shutdown = if caller_cancelled {
            Err(RuntimeError::cancelled())
        } else {
            self.peer.shutdown(cancellation).await
        };
        self.closed.store(true, Ordering::SeqCst);
        self.lifecycle.cancel();
        let close = self.peer.close(CancellationToken::new()).await;
        self.hub.close();
        match (shutdown, close) {
            (Err(error), _) => Err(error),
            (Ok(()), result) => result,
        }
    }
}

async fn lock_operation<'a>(
    gate: &'a AsyncMutex<()>,
    cancellation: &CancellationToken,
) -> Result<tokio::sync::MutexGuard<'a, ()>, RuntimeError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(RuntimeError::cancelled()),
        guard = gate.lock() => Ok(guard),
    }
}

async fn read_prompt_message(
    peer: &SdkPeer,
    operation: &CancellationToken,
    caller: &CancellationToken,
) -> Result<Option<Value>, RuntimeError> {
    let read_cancellation = CancellationToken::new();
    let read = peer.read_value(read_cancellation.clone());
    tokio::pin!(read);
    tokio::select! {
        biased;
        () = caller.cancelled() => {
            read_cancellation.cancel();
            let _settled = read.await;
            Err(RuntimeError::cancelled())
        }
        () = operation.cancelled() => {
            read_cancellation.cancel();
            let _settled = read.await;
            Err(RuntimeError::cancelled())
        }
        value = &mut read => value,
    }
}

fn safe_field<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    maximum: usize,
) -> Result<&'a str, RuntimeError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    validate_text(value, maximum)?;
    Ok(value)
}

fn safe_multiline_field<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    maximum: usize,
) -> Result<&'a str, RuntimeError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    if value.is_empty()
        || value.len() > maximum
        || value.contains('\0')
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(RuntimeError::protocol());
    }
    Ok(value)
}

fn number_field(object: &Map<String, Value>, field: &'static str) -> Result<u64, RuntimeError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(RuntimeError::protocol)
}

fn validate_content_blocks(value: &Value) -> Result<(), RuntimeError> {
    let blocks = value.as_array().ok_or_else(RuntimeError::protocol)?;
    if blocks.len() > MAX_CONTENT_BLOCKS {
        return Err(RuntimeError::protocol());
    }
    for block in blocks {
        let block = block.as_object().ok_or_else(RuntimeError::protocol)?;
        safe_field(block, "type", 64)?;
    }
    Ok(())
}

fn content_text(value: &Value) -> Result<String, RuntimeError> {
    validate_content_blocks(value)?;
    let blocks = value.as_array().ok_or_else(RuntimeError::protocol)?;
    let mut text = String::new();
    for block in blocks {
        let block = block.as_object().ok_or_else(RuntimeError::protocol)?;
        if block.get("type").and_then(Value::as_str) == Some("text") {
            let part = safe_multiline_field(block, "text", 1024 * 1024)?;
            if text.len().saturating_add(part.len()) > 1024 * 1024 {
                return Err(RuntimeError::protocol());
            }
            text.push_str(part);
        }
    }
    Ok(text)
}

fn parse_usage(value: &Value) -> Result<TokenUsage, RuntimeError> {
    let usage = value.as_object().ok_or_else(RuntimeError::protocol)?;
    let uncached = number_field(usage, "inputTokens")?;
    let cache_read = optional_number(usage, "cacheReadTokens")?;
    let cache_write = optional_number(usage, "cacheWriteTokens")?;
    let prompt_tokens = uncached
        .checked_add(cache_read)
        .and_then(|value| value.checked_add(cache_write))
        .ok_or_else(RuntimeError::protocol)?;
    Ok(TokenUsage {
        prompt_tokens,
        completion_tokens: number_field(usage, "outputTokens")?,
    })
}

fn optional_number(object: &Map<String, Value>, field: &'static str) -> Result<u64, RuntimeError> {
    match object.get(field) {
        None => Ok(0),
        Some(value) => value.as_u64().ok_or_else(RuntimeError::protocol),
    }
}

fn validate_json_shape(value: &Value) -> Result<(), RuntimeError> {
    fn visit(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), RuntimeError> {
        if depth > 64 || *nodes >= 65_536 {
            return Err(RuntimeError::protocol());
        }
        *nodes = nodes.checked_add(1).ok_or_else(RuntimeError::protocol)?;
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, depth.saturating_add(1), nodes)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if key.is_empty() || key.len() > 1024 || key.chars().any(char::is_control) {
                        return Err(RuntimeError::protocol());
                    }
                    visit(value, depth.saturating_add(1), nodes)?;
                }
            }
            Value::String(value) => {
                if value.len() > 1024 * 1024 || value.contains('\0') {
                    return Err(RuntimeError::protocol());
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }

    let mut nodes = 0;
    visit(value, 0, &mut nodes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn handled_and_ignored_event_sets_close_the_pinned_sdk_union() {
        let pinned = PINNED_DSH_SESSION_EVENT_TYPES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let handled = HANDLED_DSH_SESSION_EVENT_TYPES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let ignored = IGNORED_DSH_SESSION_EVENT_TYPES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert!(handled.is_disjoint(&ignored));
        assert_eq!(
            handled.union(&ignored).copied().collect::<BTreeSet<_>>(),
            pinned
        );
    }
}

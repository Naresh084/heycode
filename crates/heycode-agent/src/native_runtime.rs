//! Existing native Agent adapted to the generic runtime contract.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use heycode_runtime::{
    AccountState, AccountStatus, AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeId,
    AgentRuntimeKind, AgentRuntimeRegistry, RuntimeCapabilities, RuntimeCompactOutcome,
    RuntimeConfiguration, RuntimeConfigurationCapabilities, RuntimeError, RuntimeEventKind,
    RuntimeEventStream, RuntimeFinishReason, RuntimeFork, RuntimeInput, RuntimePermissionResponse,
    RuntimeQuestionResponse, RuntimeResume, RuntimeSession, RuntimeSessionId, RuntimeStart,
    RuntimeTurnId,
};
use heycode_session::{SessionEvent, SessionEventKind, TurnEndReason};

use crate::{Agent, UiEvent};

/// Adapter exposing one composed native [`Agent`] through `AgentRuntime`.
pub struct NativeAgentRuntime {
    descriptor: AgentRuntimeDescriptor,
    agent: Arc<Agent>,
    models: Arc<heycode_llm::CatalogRegistry>,
    session: Arc<NativeRuntimeSession>,
}

impl NativeAgentRuntime {
    fn new(
        agent: Arc<Agent>,
        models: Arc<heycode_llm::CatalogRegistry>,
        context: &heycode_core::Context,
    ) -> Result<Self, heycode_runtime::RuntimeContractError> {
        let descriptor = AgentRuntimeDescriptor::new(
            "native",
            "heycode native agent",
            AgentRuntimeKind::Native,
            RuntimeCapabilities {
                models: heycode_llm::CapabilitySupport::Supported,
                resume: heycode_llm::CapabilitySupport::Supported,
                fork: heycode_llm::CapabilitySupport::Unsupported,
                steer: heycode_llm::CapabilitySupport::Unsupported,
                follow_up: heycode_llm::CapabilitySupport::Unsupported,
                permissions: heycode_llm::CapabilitySupport::Unsupported,
                questions: heycode_llm::CapabilitySupport::Unsupported,
                compaction: heycode_llm::CapabilitySupport::Supported,
            },
        )?
        .with_configuration_capabilities(RuntimeConfigurationCapabilities {
            system_prompt: heycode_llm::CapabilitySupport::Unsupported,
            tools: heycode_llm::CapabilitySupport::Unsupported,
            model: heycode_llm::CapabilitySupport::Supported,
            reasoning_effort: heycode_llm::CapabilitySupport::Supported,
        });
        let session = NativeRuntimeSession::new(agent.clone(), descriptor.id().clone(), context)?;
        Ok(Self {
            descriptor,
            agent,
            models,
            session,
        })
    }

    fn request_matches(
        &self,
        session_id: &heycode_core::SessionId,
        workspace: &std::path::Path,
    ) -> bool {
        session_id.as_str() == self.session.id.as_str() && workspace == self.agent.cwd()
    }

    fn has_durable_history(&self) -> bool {
        !self
            .agent
            .session()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_fresh()
    }
}

#[async_trait]
impl AgentRuntime for NativeAgentRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.descriptor
    }

    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Ok(AccountState::without_label(AccountStatus::Unknown))
    }

    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<heycode_llm::CatalogSnapshot, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let provider = self.agent.selection().provider_name;
        self.models
            .refresh(
                &provider,
                heycode_llm::CatalogRefreshMode::PreferCache,
                cancellation,
            )
            .await
            .map(|view| (*view.snapshot).clone())
            .map_err(|_| RuntimeError::unavailable())
    }

    async fn start(
        &self,
        request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if !self.request_matches(request.session_id(), request.workspace()) {
            return Err(RuntimeError::invalid_request());
        }
        if self.has_durable_history() {
            return Err(RuntimeError::conflict());
        }
        self.session
            .configure(request.configuration().clone(), cancellation)
            .await?;
        Ok(self.session.clone())
    }

    async fn resume(
        &self,
        request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if !self.request_matches(request.session_id(), request.workspace())
            || request.runtime_session_id() != &self.session.id
        {
            return Err(RuntimeError::invalid_request());
        }
        self.session
            .configure(request.configuration().clone(), cancellation)
            .await?;
        Ok(self.session.clone())
    }

    async fn fork(
        &self,
        _request: RuntimeFork,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else {
            Err(RuntimeError::unsupported())
        }
    }
}

enum NativeTurnInput {
    Fresh(RuntimeInput),
    Pending(heycode_session::InboxMessageId),
}

struct NativeRuntimeSession {
    id: RuntimeSessionId,
    runtime_id: AgentRuntimeId,
    capabilities: RuntimeCapabilities,
    agent: Arc<Agent>,
    events: Arc<heycode_runtime::RuntimeEventHub>,
    send_gate: tokio::sync::Mutex<()>,
    configuration: Mutex<RuntimeConfiguration>,
    operation: Mutex<Option<Arc<CancellationToken>>>,
    active: AtomicUsize,
    settled: Notify,
    closed: AtomicBool,
}

impl NativeRuntimeSession {
    fn new(
        agent: Arc<Agent>,
        runtime_id: AgentRuntimeId,
        context: &heycode_core::Context,
    ) -> Result<Arc<Self>, heycode_runtime::RuntimeContractError> {
        let (id, session_bus) = {
            let session = agent
                .session()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (RuntimeSessionId::new(session.id().as_str())?, session.bus())
        };
        let events = Arc::new(heycode_runtime::RuntimeEventHub::new());
        let session_events = events.clone();
        let translator = Mutex::new(NativeEventTranslator::default());
        let budget_agent = Arc::downgrade(&agent);
        let last_session_sequence = Mutex::new(None::<u64>);
        session_bus.on_effect::<SessionEvent>(context, move |event| {
            // The durable sequence is session-scoped. Re-delivery must not
            // repeat runtime events, billing totals, or projected growth.
            {
                let Ok(mut last) = last_session_sequence.lock() else {
                    return;
                };
                if last.is_some_and(|sequence| event.seq <= sequence) {
                    return;
                }
                *last = Some(event.seq);
            }
            let mut kinds = match translator.lock() {
                Ok(mut translator) => translator.translate(event),
                Err(_) => return,
            };
            if let Some(growth) = heycode_session::retained_context_growth(&event.kind)
                && let Some(agent) = budget_agent.upgrade()
                && let Some(budget) = agent.record_context_growth(growth.tokens, growth.uncounted)
            {
                kinds.push(RuntimeEventKind::ContextBudgetChanged { budget });
            }
            for kind in kinds {
                if let Err(error) = session_events.emit(kind) {
                    // An R02 violation here is a native-runtime bug: settle
                    // this session's subscribers loudly at the producer rather
                    // than poisoning them later in a distant consumer.
                    session_events.fail(error);
                    return;
                }
            }
        });
        let ui_events = events.clone();
        agent.ui().on_effect::<UiEvent>(context, move |event| {
            for kind in normalize_ui_event(event) {
                if let Err(error) = ui_events.emit(kind) {
                    ui_events.fail(error);
                    return;
                }
            }
        });
        // The first emission on a fresh hub cannot violate R02; the arm exists
        // so the Result is settled the same way as every other emission.
        if let Err(error) = events.emit(RuntimeEventKind::SessionReady) {
            events.fail(error);
        }
        Ok(Arc::new(Self {
            id,
            runtime_id,
            capabilities: RuntimeCapabilities {
                models: heycode_llm::CapabilitySupport::Supported,
                resume: heycode_llm::CapabilitySupport::Supported,
                fork: heycode_llm::CapabilitySupport::Unsupported,
                steer: heycode_llm::CapabilitySupport::Unsupported,
                follow_up: heycode_llm::CapabilitySupport::Unsupported,
                permissions: heycode_llm::CapabilitySupport::Unsupported,
                questions: heycode_llm::CapabilitySupport::Unsupported,
                compaction: heycode_llm::CapabilitySupport::Supported,
            },
            agent,
            events,
            send_gate: tokio::sync::Mutex::new(()),
            configuration: Mutex::new(RuntimeConfiguration::new()),
            operation: Mutex::new(None),
            active: AtomicUsize::new(0),
            settled: Notify::new(),
            closed: AtomicBool::new(false),
        }))
    }

    async fn send_operation(
        &self,
        input: NativeTurnInput,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        self.check(&cancellation)?;
        if self.agent.token().is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let _gate = tokio::select! {
            guard = self.send_gate.lock() => guard,
            () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
        };
        self.check(&cancellation)?;
        let turn = RuntimeTurnId::new(self.next_turn().to_string())
            .map_err(|_| RuntimeError::internal("native turn id"))?;
        let (operation, _active) = self.begin_operation(&cancellation)?;
        let pending_id = match &input {
            NativeTurnInput::Pending(id) => Some(id.clone()),
            _ => None,
        };
        let result = match input {
            NativeTurnInput::Fresh(input) => {
                self.agent
                    .send_with_attachments_cancellable(
                        input.text(),
                        input.attachments().to_vec(),
                        (*operation).clone(),
                    )
                    .await
            }
            NativeTurnInput::Pending(id) => {
                self.agent
                    .send_inbox_id_cancellable(&id, (*operation).clone())
                    .await
            }
        };
        if operation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        match result {
            Ok(report) if report.reason == "aborted" => Err(RuntimeError::cancelled()),
            Ok(_) => Ok(turn),
            Err(error) if error.downcast_ref::<crate::FollowUpError>().is_some() => pending_id
                .as_ref()
                .and_then(|id| self.claimed_input_turn(id))
                .ok_or_else(|| RuntimeError::internal("pending input has no admitted turn")),
            Err(_) => Err(RuntimeError::internal("native agent send")),
        }
    }

    fn claimed_input_turn(&self, id: &heycode_session::InboxMessageId) -> Option<RuntimeTurnId> {
        let session = self.agent.session().lock().ok()?;
        let seq = session
            .inbox()
            .claimed()
            .iter()
            .find(|claim| claim.message().id() == id)?
            .seq();
        let mut active = None;
        for event in session.events() {
            match &event.kind {
                SessionEventKind::TurnStart { turn } => {
                    if event.seq > seq {
                        return RuntimeTurnId::new(turn.to_string()).ok();
                    }
                    active = Some(*turn);
                }
                SessionEventKind::TurnEnd { .. } if event.seq < seq => active = None,
                _ => {}
            }
            if event.seq == seq
                && let Some(turn) = active
            {
                return RuntimeTurnId::new(turn.to_string()).ok();
            }
        }
        None
    }

    fn check(&self, cancellation: &CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else if self.closed.load(Ordering::SeqCst) {
            Err(RuntimeError::closed())
        } else {
            Ok(())
        }
    }

    fn next_turn(&self) -> u64 {
        self.agent
            .session()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                SessionEventKind::TurnStart { turn } => Some(*turn),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    fn unsupported(&self, cancellation: &CancellationToken) -> Result<(), RuntimeError> {
        self.check(cancellation)?;
        Err(RuntimeError::unsupported())
    }

    fn validate_configuration(configuration: &RuntimeConfiguration) -> Result<(), RuntimeError> {
        let mut unsupported = Vec::new();
        if configuration.system_prompt().is_some() {
            unsupported.push("system_prompt");
        }
        if configuration.tools_configured() {
            unsupported.push("tools");
        }
        if unsupported.is_empty() {
            Ok(())
        } else {
            Err(RuntimeError::unsupported_field_names(&unsupported))
        }
    }

    fn begin_operation(
        &self,
        caller: &CancellationToken,
    ) -> Result<(Arc<CancellationToken>, ActiveOperation<'_>), RuntimeError> {
        self.check(caller)?;
        let operation = Arc::new(caller.child_token());
        let mut active = self
            .operation
            .lock()
            .map_err(|_| RuntimeError::internal("native operation state"))?;
        if active.is_some() {
            return Err(RuntimeError::conflict());
        }
        *active = Some(operation.clone());
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok((
            operation.clone(),
            ActiveOperation {
                session: self,
                operation,
            },
        ))
    }
}

struct ActiveOperation<'a> {
    session: &'a NativeRuntimeSession,
    operation: Arc<CancellationToken>,
}

impl Drop for ActiveOperation<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.session.operation.lock()
            && active
                .as_ref()
                .is_some_and(|operation| Arc::ptr_eq(operation, &self.operation))
        {
            *active = None;
        }
        self.session.active.fetch_sub(1, Ordering::SeqCst);
        self.session.settled.notify_waiters();
    }
}

#[async_trait]
impl RuntimeSession for NativeRuntimeSession {
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
        self.events.subscribe()
    }

    async fn send(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        self.send_operation(NativeTurnInput::Fresh(input), cancellation)
            .await
    }

    async fn send_pending(
        &self,
        message_id: String,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        let id = heycode_session::InboxMessageId::new(message_id)
            .map_err(|_| RuntimeError::internal("invalid pending input id"))?;
        self.send_operation(NativeTurnInput::Pending(id), cancellation)
            .await
    }

    async fn configure(
        &self,
        update: RuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<RuntimeConfiguration, RuntimeError> {
        Self::validate_configuration(&update)?;
        self.check(&cancellation)?;
        let _gate = self
            .send_gate
            .try_lock()
            .map_err(|_| RuntimeError::conflict())?;
        self.check(&cancellation)?;
        let mut current = self
            .configuration
            .lock()
            .map_err(|_| RuntimeError::internal("native session configuration"))?;
        if update.is_empty() {
            return Ok(current.clone());
        }
        let effective = current.merged_with(&update);
        self.agent.apply_native_runtime_configuration(&update)?;
        *current = effective.clone();
        Ok(effective)
    }

    async fn steer(
        &self,
        _input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.unsupported(&cancellation)
    }

    async fn follow_up(
        &self,
        _input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.unsupported(&cancellation)
    }

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        let operation = self
            .operation
            .lock()
            .map_err(|_| RuntimeError::internal("native operation state"))?
            .clone();
        if let Some(operation) = operation {
            operation.cancel();
        }
        Ok(())
    }

    async fn respond_permission(
        &self,
        _response: RuntimePermissionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.unsupported(&cancellation)
    }

    async fn respond_question(
        &self,
        _response: RuntimeQuestionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.unsupported(&cancellation)
    }

    async fn compact(
        &self,
        cancellation: CancellationToken,
    ) -> Result<RuntimeCompactOutcome, RuntimeError> {
        self.check(&cancellation)?;
        let _gate = tokio::select! {
            guard = self.send_gate.lock() => guard,
            () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
        };
        self.check(&cancellation)?;
        let (operation, _active) = self.begin_operation(&cancellation)?;
        let compact = self.agent.compact(
            crate::PortableCompaction::ID,
            crate::compact::DEFAULT_KEEP_TURNS,
            operation.as_ref().clone(),
        );
        tokio::pin!(compact);
        let outcome = tokio::select! {
            biased;
            result = &mut compact => result.map_err(|_| RuntimeError::internal("native compact"))?,
            () = operation.cancelled() => return Err(RuntimeError::cancelled()),
        };
        Ok(match outcome {
            crate::CompactionOutcome::Noop { .. } => RuntimeCompactOutcome::Noop,
            crate::CompactionOutcome::Applied { .. } => RuntimeCompactOutcome::Applied,
        })
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        self.closed.store(true, Ordering::SeqCst);
        let operation = self
            .operation
            .lock()
            .map_err(|_| RuntimeError::internal("native operation state"))?
            .clone();
        if let Some(operation) = operation {
            operation.cancel();
        }
        tokio::select! {
            () = self.agent.shutdown_and_wait() => {},
            () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
        }
        loop {
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            if self.active.load(Ordering::SeqCst) == 0 {
                self.events.close();
                return Ok(());
            }
            tokio::select! {
                () = settled => {}
                () = cancellation.cancelled() => return Err(RuntimeError::cancelled()),
            }
        }
    }
}

/// Translates committed session events into the R02 runtime vocabulary.
///
/// Intermediate assistant text is commentary and exactly one final message
/// closes the turn, so a step that speaks and then calls a tool — the normal
/// shape of every real turn — yields `CommentaryDelta`, `ToolCall`, … and one
/// `FinalMessage` immediately before `TurnFinished`. An empty stop reply still
/// carries an (empty) final message, which the normalizer permits. The
/// translator therefore holds each assistant message until the turn shows
/// whether it was the last step or a preface to a tool call.
#[derive(Default)]
pub(crate) struct NativeEventTranslator {
    /// Text of the latest assistant message, awaiting the step's outcome.
    pending_final: Option<String>,
    /// Whether the current step already streamed its text as chunks, so the
    /// held message must not be replayed as a delta.
    step_streamed: bool,
}

impl NativeEventTranslator {
    /// Runtime events for one committed session event, in emission order.
    pub(crate) fn translate(&mut self, event: &SessionEvent) -> Vec<RuntimeEventKind> {
        match &event.kind {
            SessionEventKind::TurnStart { turn } => {
                self.pending_final = None;
                self.step_streamed = false;
                vec![RuntimeEventKind::TurnStarted {
                    turn: RuntimeTurnId::from_native_turn(*turn),
                }]
            }
            SessionEventKind::StepStart { .. } => {
                self.step_streamed = false;
                Vec::new()
            }
            SessionEventKind::TurnEnd { turn, reason } => {
                let mut events = self.unstreamed_text_as_commentary();
                let final_text = self.pending_final.take();
                let reason = finish_reason(reason);
                if reason == RuntimeFinishReason::Stop || final_text.is_some() {
                    events.push(RuntimeEventKind::FinalMessage {
                        text: final_text.unwrap_or_default(),
                    });
                }
                events.push(RuntimeEventKind::TurnFinished {
                    turn: RuntimeTurnId::from_native_turn(*turn),
                    reason,
                });
                events
            }
        SessionEventKind::AssistantChunk {
            text, reasoning, ..
        } => {
            let mut events = Vec::new();
            if let Some(text) = text {
                self.step_streamed = true;
                events.push(RuntimeEventKind::CommentaryDelta { text: text.clone() });
            }
            if let Some(text) = reasoning {
                events.push(RuntimeEventKind::ReasoningDelta { text: text.clone() });
            }
            events
        }
        SessionEventKind::AssistantMessage { content, usage, .. } => {
            if !content.is_empty() {
                self.pending_final = Some(content.clone());
            }
            usage
                .map(|usage| RuntimeEventKind::Usage {
                    usage,
                    context: None,
                })
                .into_iter()
                .collect()
        }
        SessionEventKind::ToolCall {
            call_id,
            name,
            args,
            ..
        } => {
            let mut events = self.unstreamed_text_as_commentary();
            self.pending_final = None;
            events.push(RuntimeEventKind::ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: args.clone(),
            });
            events
        }
        SessionEventKind::ToolResult {
            call_id,
            content,
            is_error,
            untrusted_content,
        } => {
            let mut events = Vec::new();
            if let Some(boundary) = untrusted_content {
                events.push(untrusted_notice(*boundary));
            }
            events.push(RuntimeEventKind::ToolResult {
                call_id: call_id.clone(),
                result: heycode_session::tool_result_value(content),
                is_error: *is_error,
            });
            events
        }
        SessionEventKind::RichToolResult {
            call_id,
            result,
            is_error,
            untrusted_content,
        } => {
            let mut events = Vec::new();
            if let Some(boundary) = untrusted_content {
                events.push(untrusted_notice(*boundary));
            }
            events.push(RuntimeEventKind::ToolResult {
                call_id: call_id.clone(),
                result: serde_json::to_value(result.as_ref()).unwrap_or_else(|_| {
                    serde_json::Value::String(result.render_for_model())
                }),
                is_error: *is_error,
            });
            events
        }
        SessionEventKind::CompactionApplied { .. }
        | SessionEventKind::NativeCompactionApplied { .. } => vec![RuntimeEventKind::Notice {
            code: "native.compacted".to_owned(),
            message: "Native session history was compacted.".to_owned(),
        }],
        SessionEventKind::PlanReview { .. } => Vec::new(),
        SessionEventKind::PlanMode { active } => vec![RuntimeEventKind::Notice {
            code: if *active {
                "native.plan.enabled".to_owned()
            } else {
                "native.plan.disabled".to_owned()
            },
            message: if *active {
                "Plan mode is active.".to_owned()
            } else {
                "Plan mode is inactive.".to_owned()
            },
        }],
        SessionEventKind::SessionCreated { .. }
        | SessionEventKind::RuntimeLinked { .. }
        | SessionEventKind::StepEnd { .. }
        | SessionEventKind::AgentInboxSplice { .. }
        | SessionEventKind::GoalChange { .. }
        | SessionEventKind::WorkflowChange { .. }
        | SessionEventKind::ScheduleChange { .. }
        | SessionEventKind::TeamChange { .. }
        | SessionEventKind::WorkChange { .. }
        | SessionEventKind::ReviewChange { .. }
        | SessionEventKind::CodeModeChange { .. }
        // Hook contributions are model input through the durable request
        // projection. Runtime events are an output stream, so replaying the
        // text here would duplicate input and expose it on the wrong plane.
        | SessionEventKind::HookContribution { .. }
        | SessionEventKind::RequestHeader { .. }
        | SessionEventKind::RequestContext { .. }
        | SessionEventKind::UserMessage { .. }
        | SessionEventKind::UserAttachments { .. }
        // ATT02/X03 own model and delegated-client media projection.
        | SessionEventKind::AttachmentAdded { .. }
        // Runtime v1 has no raw/audio event. App-server/TUI project this
        // durable metadata directly, never by smuggling bytes through Notice.
        | SessionEventKind::AssistantAudio { .. }
        | SessionEventKind::AssistantProviderItem { .. }
        | SessionEventKind::AssistantResponseMetadata { .. }
        | SessionEventKind::ServerToolCall { .. }
        | SessionEventKind::ServerToolResult { .. }
        | SessionEventKind::ServerToolUsage { .. }
        | SessionEventKind::AssistantCitation { .. }
        | SessionEventKind::SessionTitle { .. }
        | SessionEventKind::SessionActivated {}
        | SessionEventKind::RuntimeConfigured { .. } => Vec::new(),
        }
    }

    /// A held assistant message that was never streamed as chunks is surfaced
    /// as commentary so stream consumers still see the text; a streamed one is
    /// already on the wire.
    fn unstreamed_text_as_commentary(&self) -> Vec<RuntimeEventKind> {
        match &self.pending_final {
            Some(text) if !self.step_streamed => {
                vec![RuntimeEventKind::CommentaryDelta { text: text.clone() }]
            }
            _ => Vec::new(),
        }
    }
}

fn finish_reason(reason: &TurnEndReason) -> RuntimeFinishReason {
    match reason {
        TurnEndReason::Stop => RuntimeFinishReason::Stop,
        TurnEndReason::MaxTokens
        | TurnEndReason::MaxSteps
        | TurnEndReason::MaxElapsed
        | TurnEndReason::MaxToolCalls
        | TurnEndReason::UnreportedTokenUsage
        | TurnEndReason::ClockUnavailable => RuntimeFinishReason::Limit,
        TurnEndReason::Error => RuntimeFinishReason::Error,
        TurnEndReason::Aborted => RuntimeFinishReason::Cancelled,
    }
}

fn untrusted_notice(boundary: heycode_core::UntrustedContentBoundary) -> RuntimeEventKind {
    match boundary.source() {
        heycode_core::UntrustedContentSource::ToolOrchestration => RuntimeEventKind::Notice {
            code: "content.untrusted.orchestration".into(),
            message:
                "The next result contains derived tool data, not instructions or authorization."
                    .into(),
        },
        heycode_core::UntrustedContentSource::Web => RuntimeEventKind::Notice {
            code: "content.untrusted.web".to_owned(),
            message: "The next tool result contains untrusted web data.".to_owned(),
        },
        heycode_core::UntrustedContentSource::Mcp => RuntimeEventKind::Notice {
            code: "content.untrusted.mcp".to_owned(),
            message: "The next tool result contains untrusted MCP server data.".to_owned(),
        },
        heycode_core::UntrustedContentSource::Lsp => RuntimeEventKind::Notice {
            code: "content.untrusted.lsp".to_owned(),
            message: "The next tool result contains untrusted language-server data.".to_owned(),
        },
    }
}

fn normalize_ui_event(event: &UiEvent) -> Vec<RuntimeEventKind> {
    match event {
        UiEvent::ContextBudgetChanged { budget } => vec![RuntimeEventKind::ContextBudgetChanged { budget: budget.clone() }],
        UiEvent::ApprovalRequested {
            id,
            name,
            args_preview,
            ..
        } => heycode_runtime::RuntimeRequestId::new(format!("approval-{id}"))
            .ok()
            .map(|request_id| RuntimeEventKind::PermissionRequested {
                request_id,
                action: name.clone(),
                // R02 requires a non-empty detail block; a tool called with no
                // arguments still needs a permission card the user can read.
                detail: if args_preview.trim().is_empty() {
                    "(no arguments)".to_owned()
                } else {
                    args_preview.trim().to_owned()
                },
            })
            .into_iter()
            .collect(),
        UiEvent::ApprovalResolved { id, allowed } => vec![RuntimeEventKind::Notice {
            code: format!(
                "native.permission.{}",
                if *allowed { "allowed" } else { "denied" }
            ),
            message: format!("Permission request {id} was resolved."),
        }],
        UiEvent::PermissionModeChanged { .. }
        | UiEvent::PlanReviewRequested { .. }
        | UiEvent::PlanReviewResolved { .. }
        | UiEvent::RuntimePermissionRequested { .. }
        | UiEvent::RuntimeQuestionRequested { .. }
        | UiEvent::OptionalQuestionRequested { .. }
        | UiEvent::OptionalQuestionSettled { .. }
        | UiEvent::FindingsReported { .. }
        // A03 inbox state is operational, not model-visible; a claimed message
        // reaches the runtime stream as its durable `user/message` instead.
        | UiEvent::InboxUpdated { .. }
        | UiEvent::RuntimeTurnStarted { .. }
        | UiEvent::RuntimeContextMeasured { .. } => Vec::new(),
        UiEvent::Error { message } => vec![RuntimeEventKind::Notice {
            code: "native.error".to_owned(),
            message: message.clone(),
        }],
        UiEvent::Info { text } => vec![RuntimeEventKind::Notice {
            code: "native.info".to_owned(),
            message: text.clone(),
        }],
        UiEvent::SettingsShellRequested { tab, snapshot } => vec![RuntimeEventKind::Notice {
            code: format!("native.settings.{}", tab.label().to_ascii_lowercase()),
            message: snapshot.plain_text_for(*tab),
        }],
        UiEvent::HelpRequested { header, commands } => vec![RuntimeEventKind::Notice {
            code: "native.help".to_owned(),
            message: format!("{header}\n\n{}", commands.iter().map(crate::CommandCatalogEntry::help_line).collect::<Vec<_>>().join("\n")),
        }],
        UiEvent::SandboxPanelRequested { report } => vec![RuntimeEventKind::Notice {
            code: "native.sandbox".to_owned(),
            message: format!(
                "Sandbox: {}\nActive backend: {}\nAvailable backend: {}\nMode changes require a restart.",
                report.effective_mode.as_str(),
                report.active_backend.unwrap_or("none"),
                report.available_backend.unwrap_or("none"),
            ),
        }],
        UiEvent::AutoCompactPickerRequested { enabled, current_tokens } => vec![RuntimeEventKind::Notice {
            code: "native.autocompact".to_owned(),
            message: format!("Auto-compact window: {}{}", current_tokens.map_or_else(|| "auto".to_owned(), |tokens| format!("{tokens} input tokens")), if *enabled { "" } else { "; automatic compaction is disabled by the active profile" }),
        }],
        UiEvent::TurnStarted { .. }
        | UiEvent::UserEcho { .. }
        | UiEvent::UserAttachmentsEcho { .. }
        | UiEvent::AttachmentComposerRequested { .. }
        | UiEvent::AssistantDelta { .. }
        | UiEvent::AssistantAudio { .. }
        | UiEvent::ReasoningDelta { .. }
        | UiEvent::ToolStarted { .. }
        | UiEvent::ToolFinished { .. }
        | UiEvent::Status { .. }
        | UiEvent::TurnFinished { .. }
        | UiEvent::ProfilePickerRequested
        | UiEvent::ProfileSelected { .. }
        | UiEvent::ModelPickerRequested { .. }
        | UiEvent::EffortPickerRequested { .. }
        | UiEvent::RoutePickerRequested { .. }
        | UiEvent::PermissionPickerRequested { .. }
        | UiEvent::CapabilityPanelRequested { .. }
        | UiEvent::ConnectRequested
        | UiEvent::LoggedOut { .. }
        | UiEvent::QuitRequested => Vec::new(),
    }
}

/// Register the composed native Agent as runtime `native`.
#[must_use]
pub fn native_runtime_plugin() -> Box<dyn heycode_core::Plugin> {
    struct NativeRuntimePlugin;

    impl heycode_core::Plugin for NativeRuntimePlugin {
        fn name(&self) -> &'static str {
            "runtime-native"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::AgentRuntime,
                "native",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_runtime::SERVICE_RUNTIMES,
                crate::SERVICE_AGENT,
                heycode_llm::SERVICE_MODELS,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let runtimes = context
                .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
                .ok_or_else(|| heycode_core::CoreError::other("runtimes missing"))?;
            let agent = context
                .get::<Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent missing"))?;
            let models = context
                .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
                .ok_or_else(|| heycode_core::CoreError::other("models missing"))?;
            let runtime = NativeAgentRuntime::new(agent, models, context)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            runtimes
                .register(context, Arc::new(runtime))
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    Box::new(NativeRuntimePlugin)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn settings_shell_request_has_an_honest_runtime_fallback() {
        let snapshot = crate::ui::SettingsShellSnapshot {
            status: crate::ui::SettingsShellSection::Ready {
                text: "status\nruntime: native".to_owned(),
            },
            usage: crate::ui::SettingsShellSection::Ready {
                text: "usage\nturns: 0".to_owned(),
            },
            stats: crate::ui::SettingsShellSection::Unavailable {
                reason: "stats unavailable: no cross-session aggregate owner".to_owned(),
            },
            stats_snapshot: None,
        };
        let events = normalize_ui_event(&UiEvent::SettingsShellRequested {
            tab: crate::ui::SettingsShellTab::Stats,
            snapshot,
        });
        let [RuntimeEventKind::Notice { code, message }] = events.as_slice() else {
            panic!("one fallback notice expected: {events:?}");
        };
        assert_eq!(code, "native.settings.stats");
        assert!(message.contains("active: Stats"), "{message}");
        assert!(
            message.contains("no cross-session aggregate owner"),
            "{message}"
        );
        assert!(!message.contains("turns: 0"), "{message}");
    }

    /// R02 requires a non-empty permission detail. A tool asked for approval
    /// with no arguments must still produce a valid, readable request.
    #[test]
    fn permission_requests_for_argument_free_tools_carry_a_readable_detail() {
        let events = normalize_ui_event(&UiEvent::ApprovalRequested {
            owner_session: None,
            id: 7,
            name: "list_tasks".to_owned(),
            args_preview: String::new(),
        });
        let [RuntimeEventKind::PermissionRequested { detail, action, .. }] = events.as_slice()
        else {
            panic!("one permission request expected: {events:?}");
        };
        assert_eq!(action, "list_tasks");
        assert_eq!(detail, "(no arguments)");
        let hub = heycode_runtime::RuntimeEventHub::new();
        hub.emit(RuntimeEventKind::SessionReady).unwrap();
        hub.emit(RuntimeEventKind::TurnStarted {
            turn: RuntimeTurnId::from_native_turn(1),
        })
        .unwrap();
        for kind in events {
            hub.emit(kind).expect("the request passes R02 validation");
        }
    }

    /// A step that speaks and then calls a tool yields commentary, the call,
    /// and no final until the turn ends; an empty stop still yields a final.
    #[test]
    fn translator_holds_the_final_until_the_turn_ends() {
        let mut translator = NativeEventTranslator::default();
        let event = |kind| SessionEvent {
            v: 2,
            seq: 0,
            time_ms: 0,
            kind,
        };
        assert!(matches!(
            translator
                .translate(&event(SessionEventKind::TurnStart { turn: 1 }))
                .as_slice(),
            [RuntimeEventKind::TurnStarted { .. }]
        ));
        assert!(
            translator
                .translate(&event(SessionEventKind::AssistantMessage {
                    turn: 1,
                    step: 1,
                    content: "Let me look.".to_owned(),
                    reasoning: None,
                    tool_calls: None,
                    usage: None,
                }))
                .is_empty(),
            "an assistant message is held, not emitted"
        );
        let call = translator.translate(&event(SessionEventKind::ToolCall {
            turn: 1,
            call_id: heycode_core::CallId::from_raw("c1"),
            name: "read".to_owned(),
            args: serde_json::json!({"path": "x"}),
        }));
        assert!(matches!(
            call.as_slice(),
            [
                RuntimeEventKind::CommentaryDelta { text },
                RuntimeEventKind::ToolCall { .. }
            ] if text == "Let me look."
        ));
        let end = translator.translate(&event(SessionEventKind::TurnEnd {
            turn: 1,
            reason: TurnEndReason::Stop,
        }));
        assert!(matches!(
            end.as_slice(),
            [
                RuntimeEventKind::FinalMessage { text },
                RuntimeEventKind::TurnFinished { reason: RuntimeFinishReason::Stop, .. }
            ] if text.is_empty()
        ));
    }

    #[test]
    fn a_json_tool_result_is_forwarded_structured_so_clients_can_render_a_diff() {
        let mut translator = NativeEventTranslator::default();
        let event = |kind| SessionEvent {
            v: 2,
            seq: 0,
            time_ms: 0,
            kind,
        };
        let result = translator.translate(&event(SessionEventKind::ToolResult {
            call_id: heycode_core::CallId::from_raw("c1"),
            content: "{\"diff\":\"-a\\n+b\",\"message\":\"edited\"}".to_owned(),
            is_error: false,
            untrusted_content: None,
        }));
        assert!(matches!(
            result.as_slice(),
            [RuntimeEventKind::ToolResult { result, is_error: false, .. }]
                if result["diff"] == "-a\n+b" && result["message"] == "edited"
        ));
        let plain = translator.translate(&event(SessionEventKind::ToolResult {
            call_id: heycode_core::CallId::from_raw("c2"),
            content: "3 files".to_owned(),
            is_error: false,
            untrusted_content: None,
        }));
        assert!(matches!(
            plain.as_slice(),
            [RuntimeEventKind::ToolResult { result, .. }] if result == "3 files"
        ));
    }
}

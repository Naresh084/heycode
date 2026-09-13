//! R05/R08 adapter from delegated `AgentRuntime` sessions to O01 subagents.
//!
//! The runtime still owns its loop/process/protocol. This adapter owns only the
//! ephemeral product semantics shared by Codex and Claude: a fresh durable heycode
//! child record, one runtime session, normalized event consumption, parent
//! approval callbacks, exact cancellation, quiescent close and one final text
//! result. Fork/continuation are explicitly unsupported.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use tokio_util::sync::CancellationToken;

use heycode_llm::CapabilitySupport;
use heycode_runtime::{
    AgentRuntime, AgentRuntimeKind, RuntimeError, RuntimeErrorCode, RuntimeEventKind,
    RuntimeEventNormalizer, RuntimeFinishReason, RuntimeInput, RuntimePermissionDecision,
    RuntimePermissionResponse, RuntimeStart,
};
use heycode_session::{
    Session, SessionCreationMetadata, SessionEventKind, SessionSource, TurnEndReason,
};
use heycode_tools::{ToolCallInput, Verdict};

use crate::approval::ApprovalPolicy;
use crate::subagent_provider::{
    SubagentCapabilities, SubagentContinuation, SubagentError, SubagentErrorCode, SubagentId,
    SubagentProvider, SubagentProviderDescriptor, SubagentRegistry, SubagentRequest, SubagentSeed,
    SubagentStarted,
};

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

/// Static plugin/provider binding for one delegated runtime.
#[derive(Clone)]
pub struct RuntimeSubagentConfig {
    plugin_id: &'static str,
    runtime_id: &'static str,
    provider_id: &'static str,
    display_name: &'static str,
    sessions_root: PathBuf,
    workspace: PathBuf,
    max_depth: u32,
}

impl RuntimeSubagentConfig {
    /// Bind one static runtime/plugin identity to durable child storage.
    #[must_use]
    pub const fn new(
        plugin_id: &'static str,
        runtime_id: &'static str,
        provider_id: &'static str,
        display_name: &'static str,
        sessions_root: PathBuf,
        workspace: PathBuf,
        max_depth: u32,
    ) -> Self {
        Self {
            plugin_id,
            runtime_id,
            provider_id,
            display_name,
            sessions_root,
            workspace,
            max_depth,
        }
    }
}

/// Register one delegated runtime as a fresh one-shot subagent provider.
#[must_use]
pub fn runtime_subagent_plugin(config: RuntimeSubagentConfig) -> Box<dyn heycode_core::Plugin> {
    struct RuntimeSubagentPlugin(RuntimeSubagentConfig);

    impl heycode_core::Plugin for RuntimeSubagentPlugin {
        fn name(&self) -> &'static str {
            self.0.plugin_id
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.0.plugin_id,
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SubagentProvider,
                self.0.provider_id,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_runtime::SERVICE_RUNTIMES,
                crate::SERVICE_SUBAGENTS,
                crate::SERVICE_APPROVAL,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let runtimes = context
                .get::<heycode_runtime::AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
                .ok_or_else(|| heycode_core::CoreError::other("runtime registry missing"))?;
            let runtime = runtimes
                .get(self.0.runtime_id)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?
                .ok_or_else(|| heycode_core::CoreError::other("delegated runtime missing"))?;
            if runtime.descriptor().kind() != AgentRuntimeKind::Delegated
                || runtime.descriptor().capabilities().permissions != CapabilitySupport::Supported
            {
                return Err(heycode_core::CoreError::other(
                    "delegated runtime cannot prove permission callbacks",
                ));
            }
            let approval = context
                .get::<crate::plugin::ApprovalHandle>(crate::SERVICE_APPROVAL)
                .ok_or_else(|| heycode_core::CoreError::other("approval policy missing"))?;
            let registry = context
                .get::<SubagentRegistry>(crate::SERVICE_SUBAGENTS)
                .ok_or_else(|| heycode_core::CoreError::other("subagent registry missing"))?;
            let provider = Arc::new(
                RuntimeSubagentProvider::new(
                    runtime,
                    approval.0.clone(),
                    self.0.provider_id,
                    self.0.display_name,
                    self.0.sessions_root.clone(),
                    self.0.workspace.clone(),
                    self.0.max_depth,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?
                .with_parent_workspace(),
            );
            let registration = registry
                .register_owned(provider)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || drop(registration));
            Ok(())
        }
    }

    Box::new(RuntimeSubagentPlugin(config))
}

/// Fresh one-shot delegated runtime provider shared by Codex and Claude.
pub struct RuntimeSubagentProvider {
    runtime: Arc<dyn AgentRuntime>,
    approval: Arc<dyn ApprovalPolicy>,
    descriptor: SubagentProviderDescriptor,
    sessions_root: PathBuf,
    workspace: PathBuf,
    inherit_parent_workspace: bool,
    max_depth: u32,
}

impl RuntimeSubagentProvider {
    /// Ordinary delegated providers select the actual native parent's cwd at
    /// admission. Managed-worktree adapters deliberately keep their fixed path.
    fn with_parent_workspace(mut self) -> Self {
        self.inherit_parent_workspace = true;
        self
    }

    /// Validate and bind one runtime provider.
    ///
    /// # Errors
    /// Invalid provider metadata or a non-absolute workspace.
    pub fn new(
        runtime: Arc<dyn AgentRuntime>,
        approval: Arc<dyn ApprovalPolicy>,
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        sessions_root: PathBuf,
        workspace: PathBuf,
        max_depth: u32,
    ) -> Result<Self, SubagentError> {
        if !workspace.is_absolute() {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "delegated subagent workspace must be absolute",
            ));
        }
        let descriptor = SubagentProviderDescriptor::new(
            provider_id,
            display_name,
            SubagentCapabilities {
                fork: CapabilitySupport::Unsupported,
                continuation: CapabilitySupport::Unsupported,
                interrupt: CapabilitySupport::Supported,
            },
        )
        .map_err(|error| SubagentError::new(SubagentErrorCode::Failed, error.to_string()))?;
        Ok(Self {
            runtime,
            approval,
            descriptor,
            sessions_root,
            workspace,
            inherit_parent_workspace: false,
            max_depth,
        })
    }

    async fn run(
        &self,
        request: &SubagentRequest,
        cancellation: &CancellationToken,
        log: &mut Session,
        workspace: &std::path::Path,
    ) -> Result<TurnCapture, SubagentError> {
        let start = RuntimeStart::new(log.id().clone(), workspace)
            .map_err(|_| SubagentError::new(SubagentErrorCode::Refused, "runtime start invalid"))?
            .with_ephemeral();
        let session = self
            .runtime
            .start(start, cancellation.clone())
            .await
            .map_err(runtime_subagent_error)?;
        if let Err(error) = log.append(SessionEventKind::RuntimeLinked {
            runtime: self.runtime.descriptor().id().as_str().to_owned(),
            runtime_session_id: session.id().as_str().to_owned(),
        }) {
            let _ = session.close(CancellationToken::new()).await;
            return Err(SubagentError::new(
                SubagentErrorCode::Failed,
                error.to_string(),
            ));
        }
        let mut events = session.subscribe();
        let turn = match session
            .send(
                RuntimeInput::new(request.prompt()).map_err(|_| {
                    SubagentError::new(SubagentErrorCode::Refused, "runtime input invalid")
                })?,
                cancellation.clone(),
            )
            .await
        {
            Ok(turn) => turn,
            Err(error) => {
                let _ = session.close(CancellationToken::new()).await;
                return Err(runtime_subagent_error(error));
            }
        };
        let capture = self
            .capture_turn(&session, &mut events, &turn, cancellation, log)
            .await;
        let close = session.close(CancellationToken::new()).await;
        match (capture, close) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(runtime_subagent_error(error)),
            (Ok(capture), Ok(())) => Ok(capture),
        }
    }

    async fn capture_turn(
        &self,
        session: &Arc<dyn heycode_runtime::RuntimeSession>,
        events: &mut heycode_runtime::RuntimeEventStream,
        expected_turn: &heycode_runtime::RuntimeTurnId,
        cancellation: &CancellationToken,
        log: &mut Session,
    ) -> Result<TurnCapture, SubagentError> {
        let mut final_text = None;
        let mut reasoning = String::new();
        let mut usage = None;
        let mut normalizer = RuntimeEventNormalizer::new();
        loop {
            let event = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    let _ = session.cancel(CancellationToken::new()).await;
                    return Err(SubagentError::new(
                        SubagentErrorCode::Cancelled,
                        "delegated subagent cancelled",
                    ));
                }
                event = events.next() => event,
            };
            let raw = event
                .ok_or_else(|| {
                    SubagentError::new(
                        SubagentErrorCode::Failed,
                        "delegated runtime event stream ended before settlement",
                    )
                })?
                .map_err(runtime_subagent_error)?;
            let event = normalizer.push(raw).map_err(|violation| {
                SubagentError::new(
                    SubagentErrorCode::Failed,
                    format!("delegated runtime event rejected: {violation}"),
                )
            })?;
            match event.kind() {
                RuntimeEventKind::ContextBudgetChanged { .. } => {}
                RuntimeEventKind::SessionReady => {}
                RuntimeEventKind::TurnStarted { turn } if turn == expected_turn => {}
                RuntimeEventKind::TurnStarted { .. } => {
                    return Err(SubagentError::new(
                        SubagentErrorCode::Failed,
                        "delegated runtime returned a mismatched turn",
                    ));
                }
                RuntimeEventKind::CommentaryDelta { text } => {
                    append_chunk(log, Some(text.clone()), None)?;
                }
                RuntimeEventKind::ReasoningDelta { text } => {
                    push_bounded(&mut reasoning, text)?;
                    append_chunk(log, None, Some(text.clone()))?;
                }
                RuntimeEventKind::FinalMessage { text } => {
                    final_text = Some(text.clone());
                }
                RuntimeEventKind::ToolCall { .. } | RuntimeEventKind::ToolResult { .. } => {
                    // Runtime-owned actions stay in the delegated runtime. The
                    // normalizer still checks their exact correlation; they are
                    // not mislabeled as heycode client-tool calls in JSONL.
                }
                RuntimeEventKind::PermissionRequested {
                    request_id,
                    action,
                    detail,
                } => {
                    let verdict = self
                        .approval
                        .decide_cancellable(
                            &ToolCallInput {
                                name: action.clone(),
                                args: serde_json::json!({"detail": detail}),
                            },
                            cancellation.clone(),
                        )
                        .await;
                    if cancellation.is_cancelled() {
                        let _ = session.cancel(CancellationToken::new()).await;
                        return Err(SubagentError::new(
                            SubagentErrorCode::Cancelled,
                            "delegated subagent cancelled during permission",
                        ));
                    }
                    let decision = match verdict {
                        Verdict::Allow => RuntimePermissionDecision::AllowOnce,
                        Verdict::Deny { .. } => RuntimePermissionDecision::Deny,
                    };
                    session
                        .respond_permission(
                            RuntimePermissionResponse::new(request_id.clone(), decision),
                            cancellation.clone(),
                        )
                        .await
                        .map_err(runtime_subagent_error)?;
                }
                RuntimeEventKind::QuestionRequested { .. } => {
                    let _ = session.cancel(CancellationToken::new()).await;
                    return Err(SubagentError::new(
                        SubagentErrorCode::Unsupported,
                        "delegated subagent requires an unsupported interactive answer",
                    ));
                }
                RuntimeEventKind::Usage {
                    usage: reported, ..
                } => usage = Some(*reported),
                RuntimeEventKind::TurnFinished { turn, reason } if turn == expected_turn => {
                    return Ok(TurnCapture {
                        final_text,
                        reasoning: (!reasoning.is_empty()).then_some(reasoning),
                        usage,
                        reason: *reason,
                    });
                }
                RuntimeEventKind::TurnFinished { .. } => {
                    return Err(SubagentError::new(
                        SubagentErrorCode::Failed,
                        "delegated runtime settled a mismatched turn",
                    ));
                }
                RuntimeEventKind::Notice { message, .. } => {
                    append_chunk(log, Some(message.clone()), None)?;
                }
            }
        }
    }
}

#[async_trait]
impl SubagentProvider for RuntimeSubagentProvider {
    async fn readiness(
        &self,
        cancellation: CancellationToken,
    ) -> Result<crate::subagent_provider::SubagentReadiness, SubagentError> {
        runtime_readiness(&self.runtime, cancellation).await
    }
    fn descriptor(&self) -> &SubagentProviderDescriptor {
        &self.descriptor
    }

    async fn start(
        &self,
        request: SubagentRequest,
        cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        if request.seed != SubagentSeed::Fresh
            || request.continuation != SubagentContinuation::OneShot
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Unsupported,
                "delegated runtime subagent supports fresh one-shot work only",
            ));
        }
        if request.authority().depth() >= self.max_depth {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                format!("subagent depth limit {} reached", self.max_depth),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(SubagentError::new(
                SubagentErrorCode::Cancelled,
                "delegated subagent cancelled before start",
            ));
        }
        match self.readiness(cancellation.clone()).await? {
            crate::subagent_provider::SubagentReadiness::NeedsAuthentication => {
                return Err(SubagentError::new(
                    SubagentErrorCode::Refused,
                    "external runtime requires authentication; complete its login/setup before delegation",
                ));
            }
            crate::subagent_provider::SubagentReadiness::Unavailable => {
                return Err(SubagentError::new(
                    SubagentErrorCode::Unsupported,
                    "external runtime is unavailable; install/configure it or select native",
                ));
            }
            _ => {}
        }
        let workspace = if self.inherit_parent_workspace {
            request
                .task
                .as_ref()
                .and_then(|record| record.parent.lock().ok()?.clone())
                .or_else(crate::subagent::scoped_agent)
                .map_or_else(|| self.workspace.clone(), |parent| parent.cwd())
        } else {
            self.workspace.clone()
        };
        let metadata = SessionCreationMetadata::new(
            Some(workspace.clone()),
            Some(self.runtime.descriptor().id().as_str().to_owned()),
            SessionSource::Delegated,
        )
        .map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "child session metadata invalid")
        })?;
        let mut log =
            Session::create_with_metadata(&self.sessions_root, metadata).map_err(|_| {
                SubagentError::new(
                    SubagentErrorCode::Failed,
                    "child session could not be created",
                )
            })?;
        let id = SubagentId::new(log.id().as_str())
            .map_err(|error| SubagentError::new(SubagentErrorCode::Failed, error.to_string()))?;
        if let Some(record) = &request.task {
            record
                .update(|row| {
                    row.session_id = Some(log.id().to_string());
                    row.workspace = Some(workspace.display().to_string());
                })
                .map_err(|error| {
                    SubagentError::new(SubagentErrorCode::Failed, error.to_string())
                })?;
        }
        open_turn(&mut log, request.prompt())?;
        let capture = self
            .run(&request, &cancellation, &mut log, &workspace)
            .await;
        match capture {
            Ok(capture) => {
                let reason = capture.reason;
                close_turn(&mut log, capture.as_message(), turn_end_reason(reason))?;
                match reason {
                    RuntimeFinishReason::Stop | RuntimeFinishReason::Limit => {
                        let text = capture.final_text.ok_or_else(|| {
                            SubagentError::new(
                                SubagentErrorCode::Failed,
                                "delegated runtime settled without a final message",
                            )
                        })?;
                        Ok(SubagentStarted {
                            id,
                            text,
                            handle: None,
                        })
                    }
                    RuntimeFinishReason::Cancelled => Err(SubagentError::new(
                        SubagentErrorCode::Cancelled,
                        "delegated runtime cancelled the turn",
                    )),
                    RuntimeFinishReason::Error => Err(SubagentError::new(
                        SubagentErrorCode::Failed,
                        "delegated runtime failed the turn",
                    )),
                }
            }
            Err(error) => {
                let reason = if error.code() == SubagentErrorCode::Cancelled {
                    TurnEndReason::Aborted
                } else {
                    TurnEndReason::Error
                };
                close_turn(&mut log, None, reason)?;
                Err(error)
            }
        }
    }
}

struct TurnCapture {
    final_text: Option<String>,
    reasoning: Option<String>,
    usage: Option<heycode_core::TokenUsage>,
    reason: RuntimeFinishReason,
}

impl TurnCapture {
    fn as_message(&self) -> Option<SessionEventKind> {
        self.final_text
            .as_ref()
            .map(|text| SessionEventKind::AssistantMessage {
                turn: 0,
                step: 0,
                content: text.clone(),
                reasoning: self.reasoning.clone(),
                tool_calls: None,
                usage: self.usage,
            })
    }
}

fn open_turn(log: &mut Session, prompt: &str) -> Result<(), SubagentError> {
    for event in [
        SessionEventKind::UserMessage {
            text: prompt.to_owned(),
        },
        SessionEventKind::TurnStart { turn: 0 },
        SessionEventKind::StepStart { turn: 0, step: 0 },
    ] {
        log.append(event).map_err(session_error)?;
    }
    Ok(())
}

fn close_turn(
    log: &mut Session,
    message: Option<SessionEventKind>,
    reason: TurnEndReason,
) -> Result<(), SubagentError> {
    if let Some(message) = message {
        log.append(message).map_err(session_error)?;
    }
    for event in [
        SessionEventKind::StepEnd { turn: 0, step: 0 },
        SessionEventKind::TurnEnd { turn: 0, reason },
    ] {
        log.append(event).map_err(session_error)?;
    }
    Ok(())
}

fn append_chunk(
    log: &mut Session,
    text: Option<String>,
    reasoning: Option<String>,
) -> Result<(), SubagentError> {
    log.append(SessionEventKind::AssistantChunk {
        turn: 0,
        step: 0,
        text,
        reasoning,
    })
    .map(|_| ())
    .map_err(session_error)
}

fn push_bounded(target: &mut String, delta: &str) -> Result<(), SubagentError> {
    if target.len().saturating_add(delta.len()) > MAX_CAPTURE_BYTES {
        return Err(SubagentError::new(
            SubagentErrorCode::Failed,
            "delegated runtime output exceeded the capture limit",
        ));
    }
    target.push_str(delta);
    Ok(())
}

const fn turn_end_reason(reason: RuntimeFinishReason) -> TurnEndReason {
    match reason {
        RuntimeFinishReason::Stop => TurnEndReason::Stop,
        RuntimeFinishReason::Limit => TurnEndReason::MaxTokens,
        RuntimeFinishReason::Cancelled => TurnEndReason::Aborted,
        RuntimeFinishReason::Error => TurnEndReason::Error,
    }
}

fn runtime_subagent_error(error: RuntimeError) -> SubagentError {
    let code = match error.code() {
        RuntimeErrorCode::Cancelled => SubagentErrorCode::Cancelled,
        RuntimeErrorCode::Unsupported => SubagentErrorCode::Unsupported,
        RuntimeErrorCode::Conflict | RuntimeErrorCode::InvalidRequest => SubagentErrorCode::Refused,
        RuntimeErrorCode::Unavailable
        | RuntimeErrorCode::Unauthorized
        | RuntimeErrorCode::NotFound
        | RuntimeErrorCode::Protocol
        | RuntimeErrorCode::Closed
        | RuntimeErrorCode::Internal => SubagentErrorCode::Failed,
    };
    SubagentError::new(code, error.message())
}

fn session_error(error: heycode_session::AppendError) -> SubagentError {
    SubagentError::new(SubagentErrorCode::Failed, error.to_string())
}

pub(crate) async fn runtime_readiness(
    runtime: &Arc<dyn AgentRuntime>,
    cancellation: CancellationToken,
) -> Result<crate::subagent_provider::SubagentReadiness, SubagentError> {
    use crate::subagent_provider::SubagentReadiness;
    use heycode_runtime::AccountStatus;
    let account = match runtime.account(cancellation).await {
        Ok(account) => account,
        Err(error) if error.code() == heycode_runtime::RuntimeErrorCode::Unsupported => {
            return Ok(SubagentReadiness::Unknown);
        }
        Err(error) => return Err(runtime_subagent_error(error)),
    };
    Ok(match account.status() {
        AccountStatus::Connected | AccountStatus::NotRequired => SubagentReadiness::Ready,
        AccountStatus::Disconnected | AccountStatus::Expired => {
            SubagentReadiness::NeedsAuthentication
        }
        AccountStatus::Unavailable => SubagentReadiness::Unavailable,
        AccountStatus::Unknown => SubagentReadiness::Unknown,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use futures::{StreamExt as _, stream};

    use super::*;

    struct SequencedApproval(AtomicUsize);

    #[async_trait]
    impl ApprovalPolicy for SequencedApproval {
        async fn decide(&self, _call: &ToolCallInput) -> Verdict {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Verdict::Allow
            } else {
                Verdict::Deny {
                    reason: "fixture denial".to_owned(),
                }
            }
        }
    }

    struct FakeSession {
        id: heycode_runtime::RuntimeSessionId,
        runtime_id: heycode_runtime::AgentRuntimeId,
        capabilities: heycode_runtime::RuntimeCapabilities,
        events: Mutex<Option<Vec<heycode_runtime::RuntimeEvent>>>,
        pending_after_events: bool,
        sent: tokio::sync::Notify,
        permissions: Mutex<Vec<RuntimePermissionDecision>>,
        cancelled: AtomicBool,
        closed: AtomicBool,
    }

    impl FakeSession {
        fn new(events: Vec<heycode_runtime::RuntimeEvent>, pending_after_events: bool) -> Self {
            Self {
                id: heycode_runtime::RuntimeSessionId::new("runtime-session").unwrap(),
                runtime_id: heycode_runtime::AgentRuntimeId::new("fixture-runtime").unwrap(),
                capabilities: delegated_capabilities(),
                events: Mutex::new(Some(events)),
                pending_after_events,
                sent: tokio::sync::Notify::new(),
                permissions: Mutex::new(Vec::new()),
                cancelled: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl heycode_runtime::RuntimeSession for FakeSession {
        fn id(&self) -> &heycode_runtime::RuntimeSessionId {
            &self.id
        }

        fn runtime_id(&self) -> &heycode_runtime::AgentRuntimeId {
            &self.runtime_id
        }

        fn capabilities(&self) -> &heycode_runtime::RuntimeCapabilities {
            &self.capabilities
        }

        fn subscribe(&self) -> heycode_runtime::RuntimeEventStream {
            let events = self.events.lock().unwrap().take().unwrap_or_default();
            let finite = stream::iter(events.into_iter().map(Ok));
            if self.pending_after_events {
                Box::pin(finite.chain(stream::pending()))
            } else {
                Box::pin(finite)
            }
        }

        async fn send(
            &self,
            _input: RuntimeInput,
            _cancellation: CancellationToken,
        ) -> Result<heycode_runtime::RuntimeTurnId, RuntimeError> {
            self.sent.notify_one();
            heycode_runtime::RuntimeTurnId::new("turn-1").map_err(|_| RuntimeError::protocol())
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
            self.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn respond_permission(
            &self,
            response: RuntimePermissionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            self.permissions.lock().unwrap().push(response.decision());
            Ok(())
        }

        async fn respond_question(
            &self,
            _response: heycode_runtime::RuntimeQuestionResponse,
            _cancellation: CancellationToken,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn compact(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<heycode_runtime::RuntimeCompactOutcome, RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn close(&self, _cancellation: CancellationToken) -> Result<(), RuntimeError> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeRuntime {
        descriptor: heycode_runtime::AgentRuntimeDescriptor,
        session: Arc<FakeSession>,
    }

    #[async_trait]
    impl AgentRuntime for FakeRuntime {
        fn descriptor(&self) -> &heycode_runtime::AgentRuntimeDescriptor {
            &self.descriptor
        }

        async fn account(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<heycode_runtime::AccountState, RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn models(
            &self,
            _cancellation: CancellationToken,
        ) -> Result<heycode_llm::CatalogSnapshot, RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn start(
            &self,
            _request: RuntimeStart,
            _cancellation: CancellationToken,
        ) -> Result<Arc<dyn heycode_runtime::RuntimeSession>, RuntimeError> {
            Ok(self.session.clone())
        }

        async fn resume(
            &self,
            _request: heycode_runtime::RuntimeResume,
            _cancellation: CancellationToken,
        ) -> Result<Arc<dyn heycode_runtime::RuntimeSession>, RuntimeError> {
            Err(RuntimeError::unsupported())
        }

        async fn fork(
            &self,
            _request: heycode_runtime::RuntimeFork,
            _cancellation: CancellationToken,
        ) -> Result<Arc<dyn heycode_runtime::RuntimeSession>, RuntimeError> {
            Err(RuntimeError::unsupported())
        }
    }

    fn delegated_capabilities() -> heycode_runtime::RuntimeCapabilities {
        heycode_runtime::RuntimeCapabilities {
            models: CapabilitySupport::Unsupported,
            resume: CapabilitySupport::Unsupported,
            fork: CapabilitySupport::Unsupported,
            steer: CapabilitySupport::Unsupported,
            follow_up: CapabilitySupport::Unsupported,
            permissions: CapabilitySupport::Supported,
            questions: CapabilitySupport::Supported,
            compaction: CapabilitySupport::Unsupported,
        }
    }

    fn runtime(session: Arc<FakeSession>) -> Arc<dyn AgentRuntime> {
        Arc::new(FakeRuntime {
            descriptor: heycode_runtime::AgentRuntimeDescriptor::new(
                "fixture-runtime",
                "Fixture runtime",
                AgentRuntimeKind::Delegated,
                delegated_capabilities(),
            )
            .unwrap(),
            session,
        })
    }

    fn request() -> SubagentRequest {
        SubagentRequest::new(
            "delegated test",
            "inspect the change",
            SubagentSeed::Fresh,
            SubagentContinuation::OneShot,
            0,
        )
        .unwrap()
    }

    fn successful_events() -> Vec<heycode_runtime::RuntimeEvent> {
        let turn = heycode_runtime::RuntimeTurnId::new("turn-1").unwrap();
        vec![
            heycode_runtime::RuntimeEvent::new(0, RuntimeEventKind::SessionReady),
            heycode_runtime::RuntimeEvent::new(
                1,
                RuntimeEventKind::TurnStarted { turn: turn.clone() },
            ),
            heycode_runtime::RuntimeEvent::new(
                2,
                RuntimeEventKind::ReasoningDelta {
                    text: "planning".to_owned(),
                },
            ),
            heycode_runtime::RuntimeEvent::new(
                3,
                RuntimeEventKind::ToolCall {
                    call_id: heycode_core::CallId::from_raw("call-1"),
                    name: "read".to_owned(),
                    arguments: serde_json::json!({"path":"src/lib.rs"}),
                },
            ),
            heycode_runtime::RuntimeEvent::new(
                4,
                RuntimeEventKind::ToolResult {
                    call_id: heycode_core::CallId::from_raw("call-1"),
                    result: serde_json::json!({"ok":true}),
                    is_error: false,
                },
            ),
            heycode_runtime::RuntimeEvent::new(
                5,
                RuntimeEventKind::PermissionRequested {
                    request_id: heycode_runtime::RuntimeRequestId::new("permission-1").unwrap(),
                    action: "write".to_owned(),
                    detail: "update src/lib.rs".to_owned(),
                },
            ),
            heycode_runtime::RuntimeEvent::new(
                6,
                RuntimeEventKind::PermissionRequested {
                    request_id: heycode_runtime::RuntimeRequestId::new("permission-2").unwrap(),
                    action: "bash".to_owned(),
                    detail: "run formatter".to_owned(),
                },
            ),
            heycode_runtime::RuntimeEvent::new(
                7,
                RuntimeEventKind::FinalMessage {
                    text: "delegated answer".to_owned(),
                },
            ),
            heycode_runtime::RuntimeEvent::new(
                8,
                RuntimeEventKind::Usage {
                    usage: heycode_core::TokenUsage {
                        prompt_tokens: 12,
                        completion_tokens: 4,
                    },
                    context: None,
                },
            ),
            heycode_runtime::RuntimeEvent::new(
                9,
                RuntimeEventKind::TurnFinished {
                    turn,
                    reason: RuntimeFinishReason::Stop,
                },
            ),
        ]
    }

    #[tokio::test]
    async fn delegated_provider_normalizes_plan_tools_permissions_and_durable_final() {
        let root = tempfile::tempdir().unwrap();
        let session = Arc::new(FakeSession::new(successful_events(), true));
        let provider = RuntimeSubagentProvider::new(
            runtime(session.clone()),
            Arc::new(SequencedApproval(AtomicUsize::new(0))),
            "fixture",
            "Fixture",
            root.path().to_path_buf(),
            root.path().to_path_buf(),
            3,
        )
        .unwrap();
        let result = provider
            .start(request(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.text, "delegated answer");
        assert_eq!(
            *session.permissions.lock().unwrap(),
            [
                RuntimePermissionDecision::AllowOnce,
                RuntimePermissionDecision::Deny
            ]
        );
        assert!(session.closed.load(Ordering::SeqCst));
        let durable = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .find_map(|entry| Session::open(entry.path()).ok())
            .unwrap();
        assert_eq!(
            durable.runtime_link(),
            Some(("fixture-runtime", "runtime-session"))
        );
        assert!(durable.events().iter().any(|event| matches!(
            &event.kind,
            SessionEventKind::UserMessage { text } if text == "inspect the change"
        )));
        assert!(durable.events().iter().any(|event| matches!(
            &event.kind,
            SessionEventKind::AssistantMessage { content, reasoning, usage, .. }
                if content == "delegated answer"
                    && reasoning.as_deref() == Some("planning")
                    && usage == &Some(heycode_core::TokenUsage {
                        prompt_tokens: 12,
                        completion_tokens: 4,
                    })
        )));
    }

    #[tokio::test]
    async fn delegated_provider_cancellation_interrupts_and_quiescently_closes() {
        let turn = heycode_runtime::RuntimeTurnId::new("turn-1").unwrap();
        let session = Arc::new(FakeSession::new(
            vec![
                heycode_runtime::RuntimeEvent::new(0, RuntimeEventKind::SessionReady),
                heycode_runtime::RuntimeEvent::new(1, RuntimeEventKind::TurnStarted { turn }),
            ],
            true,
        ));
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(
            RuntimeSubagentProvider::new(
                runtime(session.clone()),
                Arc::new(crate::AutoApprove),
                "fixture",
                "Fixture",
                root.path().to_path_buf(),
                root.path().to_path_buf(),
                3,
            )
            .unwrap(),
        );
        let cancellation = CancellationToken::new();
        let task = {
            let provider = provider.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move { provider.start(request(), cancellation).await })
        };
        session.sent.notified().await;
        cancellation.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code(), SubagentErrorCode::Cancelled);
        assert!(session.cancelled.load(Ordering::SeqCst));
        assert!(session.closed.load(Ordering::SeqCst));
    }
}

//! Nested subagents: the native delegation provider and its model-facing tools.
//!
//! One `task` call delegates through the O01 [`SubagentProvider`] contract. The
//! native provider spawns a child [`Agent`] with its own durable session and
//! its own quiet event bus, sharing providers/tools/approval with the parent.
//! Nesting and child control are bound by a registry-minted task-local
//! [`SubagentAuthority`], so the same registered tools retain owner, depth and
//! continuation lifetime at every level.
//!
//! The provider owns *how* a child runs; [`SubagentRegistry`] owns *which*
//! provider serves a request and the lifetime of every continuable child. The
//! O02/O03 make those context and authority contracts production-complete.

use std::sync::Arc;

use async_trait::async_trait;

use heycode_core::{CoreError, CoreResult, EventBus, Plugin, ToolSpec, Waterfall};
use heycode_llm::{
    CatalogRegistry, LlmSelection, ProviderInterception, ProviderRegistry, TokenCounterRegistry,
};
use heycode_prompt::PromptRegistry;
use heycode_session::{
    ForkBoundary, Session, SessionCreationMetadata, SessionEventKind, SessionSource,
};
use heycode_tools::{PreToolDecision, Tool, ToolCtx, ToolError, ToolRegistry};

use crate::agent::{Agent, CompactionPolicy};
use crate::approval::ApprovalPolicy;
use crate::subagent_provider::{
    SubagentAuthority, SubagentCapabilities, SubagentContinuation, SubagentError,
    SubagentErrorCode, SubagentHandle, SubagentId, SubagentProvider, SubagentProviderDescriptor,
    SubagentProviderId, SubagentRegistry, SubagentRequest, SubagentSeed, SubagentStarted,
};
use crate::ui::UiEvent;
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    /// Current owner/depth/retention authority for the running task chain.
    static TASK_AUTHORITY: SubagentAuthority;
    static TASK_AGENT: Arc<Agent>;
}

pub(crate) fn scoped_agent() -> Option<Arc<Agent>> {
    TASK_AGENT.try_with(Clone::clone).ok()
}

/// Carry the actual native owner across a host-owned orchestration future.
pub(crate) async fn with_task_context<F: std::future::Future>(
    agent: Arc<Agent>,
    authority: SubagentAuthority,
    future: F,
) -> F::Output {
    TASK_AGENT
        .scope(agent, TASK_AUTHORITY.scope(authority, future))
        .await
}

pub(crate) fn current_authority(root: &SubagentAuthority) -> SubagentAuthority {
    TASK_AUTHORITY
        .try_with(Clone::clone)
        .unwrap_or_else(|_| root.clone())
}

/// Spawns child agents with durable sessions. Continuable children are owned by
/// [`SubagentRegistry`], not by the runner.
pub struct SubagentRunner {
    providers: Arc<ProviderRegistry>,
    provider_interception: Arc<ProviderInterception>,
    catalogs: Arc<CatalogRegistry>,
    compactions: Arc<crate::CompactionRegistry>,
    token_counters: Arc<TokenCounterRegistry>,
    native_tools: Arc<heycode_native_tools::NativeToolRegistry>,
    /// Parent's durable log; fork-mode seeds children from its projection.
    parent_session: Arc<std::sync::Mutex<Session>>,
    selection: LlmSelection,
    registry: std::sync::Weak<SubagentRegistry>,
    subprocess: Option<heycode_exec::SubprocessService>,
    sandbox: Option<heycode_exec::SandboxService>,
    shell: Option<heycode_exec::ShellService>,
    worktrees: tokio::sync::Mutex<
        std::collections::BTreeMap<std::path::PathBuf, Arc<crate::GitWorktreeManager>>,
    >,
    tools: Arc<ToolRegistry>,
    pre_seam: Arc<Waterfall<PreToolDecision>>,
    prompt: Arc<PromptRegistry>,
    approval: Arc<dyn ApprovalPolicy>,
    sessions_root: std::path::PathBuf,
    cwd: std::path::PathBuf,
    bus: EventBus,
    compaction: CompactionPolicy,
    max_depth: u32,
    lifecycle_hooks: crate::lifecycle_hooks::LifecycleHookSlot,
}

impl SubagentRunner {
    /// Resolve the runner from live services.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        providers: Arc<ProviderRegistry>,
        provider_interception: Arc<ProviderInterception>,
        catalogs: Arc<CatalogRegistry>,
        compactions: Arc<crate::CompactionRegistry>,
        token_counters: Arc<TokenCounterRegistry>,
        native_tools: Arc<heycode_native_tools::NativeToolRegistry>,
        selection: LlmSelection,
        tools: Arc<ToolRegistry>,
        pre_seam: Arc<Waterfall<PreToolDecision>>,
        prompt: Arc<PromptRegistry>,
        approval: Arc<dyn ApprovalPolicy>,
        parent_session: Arc<std::sync::Mutex<Session>>,
        sessions_root: std::path::PathBuf,
        cwd: std::path::PathBuf,
        bus: EventBus,
        max_depth: u32,
    ) -> Self {
        Self {
            parent_session,
            providers,
            provider_interception,
            catalogs,
            compactions,
            token_counters,
            native_tools,
            selection,
            registry: std::sync::Weak::new(),
            subprocess: None,
            sandbox: None,
            shell: None,
            worktrees: tokio::sync::Mutex::new(std::collections::BTreeMap::new()),
            tools,
            pre_seam,
            prompt,
            approval,
            sessions_root,
            cwd,
            bus,
            compaction: CompactionPolicy::default(),
            max_depth,
            lifecycle_hooks: crate::lifecycle_hooks::LifecycleHookSlot::default(),
        }
    }

    fn with_registry(mut self, registry: &Arc<SubagentRegistry>) -> Self {
        self.registry = Arc::downgrade(registry);
        self
    }

    fn with_execution(
        mut self,
        subprocess: Option<heycode_exec::SubprocessService>,
        sandbox: Option<heycode_exec::SandboxService>,
        shell: Option<heycode_exec::ShellService>,
    ) -> Self {
        self.subprocess = subprocess;
        self.sandbox = sandbox;
        self.shell = shell;
        self
    }

    async fn worktree_manager(
        &self,
        cwd: &std::path::Path,
    ) -> anyhow::Result<Arc<crate::GitWorktreeManager>> {
        let mut managers = self.worktrees.lock().await;
        if let Some(manager) = managers.get(cwd) {
            return Ok(manager.clone());
        }
        self.subprocess.as_ref().ok_or_else(|| {
            anyhow::anyhow!("native worktree isolation requires the composed subprocess service")
        })?;
        // Fixed-argv Git management is host-owned and must write both source .git
        // metadata and the lease. Model tools below retain the parent's sandbox mode.
        let subprocess = heycode_exec::SubprocessService::local_with_sandbox(
            heycode_exec::SandboxService::new(heycode_exec::SandboxMode::Off, cwd, None)?,
        );
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        cwd.hash(&mut hash);
        let root = self
            .sessions_root
            .join(format!("native-worktrees-{:016x}", hash.finish()));
        let manager = Arc::new(crate::GitWorktreeManager::new(
            subprocess,
            cwd,
            root,
            crate::WorktreeRetention::RemoveAlways,
        )?);
        managers.insert(cwd.to_path_buf(), manager.clone());
        Ok(manager)
    }

    fn with_lifecycle_hooks(
        mut self,
        lifecycle_hooks: crate::lifecycle_hooks::LifecycleHookSlot,
    ) -> Self {
        self.lifecycle_hooks = lifecycle_hooks;
        self
    }

    fn parent_for(&self, request: &SubagentRequest) -> Option<Arc<Agent>> {
        request
            .task
            .as_ref()
            .and_then(|record| record.parent.lock().ok()?.clone())
            .or_else(scoped_agent)
            .or_else(|| {
                self.registry
                    .upgrade()
                    .and_then(|registry| registry.agent_for_authority(request.authority()))
            })
    }

    async fn run_child(
        &self,
        request: &SubagentRequest,
        cancellation: CancellationToken,
    ) -> anyhow::Result<ChildOutcome> {
        let authority = request.authority();
        let continuation = request.continuation;
        if authority.depth() >= self.max_depth {
            anyhow::bail!("subagent depth limit {} reached", self.max_depth);
        }
        let parent = self.parent_for(request);
        let original_cwd = parent
            .as_ref()
            .map_or_else(|| self.cwd.clone(), |agent| agent.cwd().to_path_buf());
        let lease = if request.config().isolation == crate::ChildIsolation::Worktree {
            Some(
                self.worktree_manager(&original_cwd)
                    .await?
                    .create_from_current(cancellation.clone())
                    .await?,
            )
        } else {
            None
        };
        let cwd = lease
            .as_ref()
            .map_or_else(|| original_cwd.clone(), |lease| lease.path().to_path_buf());
        let result_path = lease.as_ref().map(|lease| lease.path().to_path_buf());
        let result = self.run_child_at(request, cancellation.clone(), cwd).await;
        match (result, lease) {
            (Ok(mut outcome), Some(lease)) if continuation == SubagentContinuation::Continuable => {
                outcome
                    .text
                    .push_str(&format!("\n\n[worktree: {}]", lease.path().display()));
                outcome.lease = Some(lease);
                Ok(outcome)
            }
            (result, Some(lease)) => {
                let terminal = if cancellation.is_cancelled() {
                    crate::WorktreeOutcome::Cancelled
                } else if result
                    .as_ref()
                    .is_ok_and(|outcome| outcome.initial_error.is_none())
                {
                    crate::WorktreeOutcome::Success
                } else {
                    crate::WorktreeOutcome::Failure
                };
                let cleanup = lease.finish(terminal, CancellationToken::new()).await;
                let location = result_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                match (result, cleanup) {
                    (Ok(mut outcome), Ok(())) => {
                        if std::path::Path::new(&location).exists() {
                            outcome
                                .text
                                .push_str(&format!("\n\n[worktree results retained: {location}]"));
                        }
                        Ok(outcome)
                    }
                    (Err(error), _) => Err(anyhow::anyhow!(
                        "{error}; worktree result location: {location}"
                    )),
                    (_, Err(error)) => Err(anyhow::anyhow!(
                        "{error}; worktree preserved at: {location}"
                    )),
                }
            }
            (result, None) => result,
        }
    }

    async fn run_child_at(
        &self,
        request: &SubagentRequest,
        cancellation: CancellationToken,
        cwd: std::path::PathBuf,
    ) -> anyhow::Result<ChildOutcome> {
        let label = request.label();
        let prompt = request.prompt();
        let fork = request.seed == SubagentSeed::ForkParent;
        let authority = request.authority();
        let continuation = request.continuation;
        let instructions = request.instructions();
        let config = request.config();
        let configuration_key = request.configuration_key();
        let depth = authority.depth();
        if depth >= self.max_depth {
            anyhow::bail!("subagent depth limit {max} reached", max = self.max_depth);
        }

        self.bus.emit(UiEvent::Status {
            verb: format!("Subagent({label}) starting…"),
        });
        let parent_agent = self.parent_for(request);
        let parent_context = parent_agent
            .as_ref()
            .map(|parent| parent.tool_execution_context());
        let parent_session = parent_agent
            .as_ref()
            .map_or(&self.parent_session, |agent| agent.session());
        let metadata = || {
            SessionCreationMetadata::new(
                Some(cwd.clone()),
                Some("native".to_owned()),
                if fork {
                    SessionSource::Fork
                } else {
                    SessionSource::Subagent
                },
            )
            .map_err(|error| anyhow::anyhow!("child session metadata invalid: {error}"))
        };
        let session = if fork {
            let parent = parent_session
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            parent
                .fork_with_metadata(
                    &self.sessions_root,
                    stable_fork_boundary(&parent),
                    metadata()?,
                )
                .map_err(|error| anyhow::anyhow!("child session fork failed: {error}"))?
        } else {
            Session::create_with_metadata(&self.sessions_root, metadata()?)
                .map_err(|error| anyhow::anyhow!("child session create failed: {error}"))?
        };

        let session_id = session.id().to_string();
        let id = SubagentId::new(
            request
                .task
                .as_ref()
                .map_or_else(|| session_id.clone(), |record| record.read().id),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let child_authority = authority.child(
            id.clone(),
            continuation == SubagentContinuation::Continuable,
        );
        let inherited_tools = parent_agent
            .as_ref()
            .map_or_else(|| self.tools.clone(), |parent| parent.child_tool_registry());
        let tools = if request.config().isolation == crate::ChildIsolation::Worktree {
            let sandbox = self
                .sandbox
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("native worktree sandbox service unavailable"))?
                .for_workspace(&cwd)?;
            let filesystem = heycode_exec::FileSystemService::local(
                heycode_exec::FileSystemPolicy::from_sandbox(sandbox.policy())?,
            )?;
            let subprocess = heycode_exec::SubprocessService::local_with_sandbox(sandbox);
            let shell = self
                .shell
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("native worktree shell service unavailable"))?
                .with_executor(subprocess);
            Arc::new(inherited_tools.for_workspace(&filesystem, &shell)?)
        } else {
            inherited_tools
        };
        // Resolve at spawn time from the actual parent, including nested turns.
        let (selection, effort) = parent_agent.as_ref().map_or_else(
            || Ok((self.selection.clone(), None)),
            |parent| parent.inference_configuration(),
        )?;
        let memory = crate::agent_memory::prepare_memory(
            config.memory,
            configuration_key,
            &self.sessions_root,
            &parent_agent
                .as_ref()
                .map_or_else(|| self.cwd.clone(), |parent| parent.cwd()),
        )?;
        let tools = crate::agent_memory::bind_memory_tools(tools, memory.as_ref())?;
        let tools = crate::session_control::child_question_tools(
            tools,
            continuation == SubagentContinuation::Continuable,
        )?;
        let instructions = match &memory {
            Some(memory) => format!("{instructions}\n\n{}", memory.context()?),
            None => instructions.to_owned(),
        };
        let mut child = Agent::new(
            Arc::new(std::sync::Mutex::new(session)),
            self.providers.clone(),
            self.provider_interception.clone(),
            self.catalogs.clone(),
            self.compactions.clone(),
            self.token_counters.clone(),
            self.native_tools.clone(),
            selection.clone(),
            tools,
            parent_context
                .as_ref()
                .map_or_else(|| self.pre_seam.clone(), |context| context.pre_seam.clone()),
            self.prompt.clone(),
            parent_context
                .as_ref()
                .map_or_else(|| self.approval.clone(), |context| context.approval.clone()),
            parent_context
                .as_ref()
                .and_then(|context| context.plan.clone()),
            false, // children never auto-title
            cwd,
            self.compaction,
            EventBus::default(),
        );
        child.subagent_budget = self
            .registry
            .upgrade()
            .map(|registry| registry.budget.clone());
        child.task_record = request.task.clone();
        if let Some(parent) = &parent_agent {
            child.inherit_workspace_guidance(parent);
        }
        child.set_inference_route(selection.provider_name, selection.model, effort);
        child.configure_custom_child(&instructions, config.clone())?;
        if !config.restricts_tools() {
            child.inherit_lifecycle_hooks(parent_agent.as_ref().map_or_else(
                || self.lifecycle_hooks.clone(),
                |parent| parent.child_lifecycle_hooks(),
            ));
        }
        let child = Arc::new(child);
        if let Some(record) = &request.task {
            record.publish_native(session_id, &child)?;
        }
        if let Some(registry) = self.registry.upgrade() {
            registry.publish_native_child(&id, &child);
        }
        let report = TASK_AGENT
            .scope(
                child.clone(),
                TASK_AUTHORITY.scope(
                    child_authority.clone(),
                    child.send_cancellable(prompt, cancellation),
                ),
            )
            .await?;

        let text = report.text.clone();
        let initial_error = native_report_result(report).err();
        self.bus.emit(UiEvent::Status {
            verb: "Forging…".to_owned(),
        });
        Ok(ChildOutcome {
            id,
            text,
            initial_error,
            agent: child,
            authority: child_authority,
            lease: None,
        })
    }
}

fn stable_fork_boundary(session: &Session) -> ForkBoundary {
    let mut open_turn_start = None;
    let mut open_turn = None;
    for event in session.events() {
        match &event.kind {
            SessionEventKind::TurnStart { turn } => {
                open_turn = Some(*turn);
                open_turn_start = Some(event.seq);
            }
            SessionEventKind::TurnEnd { turn, .. } if open_turn == Some(*turn) => {
                open_turn = None;
                open_turn_start = None;
            }
            _ => {}
        }
    }
    open_turn_start.map_or(ForkBoundary::Latest, ForkBoundary::EventCount)
}

/// Result of one child run; the agent handle stays alive so callers can
/// either drop it (oneshot) or keep it in the registry (continuable).
struct ChildOutcome {
    initial_error: Option<SubagentError>,
    lease: Option<crate::GitWorktreeLease>,
    /// Durable child session id (the task_id callers quote back).
    id: SubagentId,
    /// Final child text.
    text: String,
    /// Live handle for follow-ups.
    agent: Arc<Agent>,
    /// Authority retained by a continuable child follow-up.
    authority: SubagentAuthority,
}

/// Native delegation: a child [`Agent`] in this process.
///
/// It proves fork (durable shared-prefix child sessions), continuation (the
/// child agent stays live) and interrupt (its reusable per-turn cancellation).
pub struct NativeSubagentProvider {
    runner: Arc<SubagentRunner>,
    descriptor: SubagentProviderDescriptor,
}

impl NativeSubagentProvider {
    /// Bind the native provider to a runner.
    ///
    /// # Errors
    /// Static descriptor validation failure.
    pub fn new(runner: Arc<SubagentRunner>) -> Result<Self, SubagentError> {
        let supported = heycode_llm::CapabilitySupport::Supported;
        let descriptor = SubagentProviderDescriptor::new(
            "native",
            "Native child agent",
            SubagentCapabilities {
                fork: supported,
                continuation: supported,
                interrupt: supported,
            },
        )
        .map_err(|error| SubagentError::new(SubagentErrorCode::Failed, error.to_string()))?;
        Ok(Self { runner, descriptor })
    }
}

#[async_trait]
impl SubagentProvider for NativeSubagentProvider {
    async fn readiness(
        &self,
        cancellation: CancellationToken,
    ) -> Result<crate::subagent_provider::SubagentReadiness, SubagentError> {
        if cancellation.is_cancelled() {
            return Err(SubagentError::new(
                SubagentErrorCode::Cancelled,
                "readiness cancelled",
            ));
        }
        Ok(crate::subagent_provider::SubagentReadiness::Ready)
    }
    fn descriptor(&self) -> &SubagentProviderDescriptor {
        &self.descriptor
    }

    /// The child `Agent` is built with the parent's own `pre_seam`
    /// ([`SubagentRunner::run_child`]), so every guard layer mounted on
    /// `seam/pre_tool` binds the child's tool calls exactly as it binds the
    /// parent's.
    fn supports_configuration(&self) -> bool {
        true
    }

    fn inherits_parent_tool_guards(&self) -> bool {
        true
    }

    async fn start(
        &self,
        request: SubagentRequest,
        cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        if !self.descriptor.supports(&request) {
            return Err(SubagentError::new(
                SubagentErrorCode::Unsupported,
                "native subagents do not support the requested seed and continuation",
            ));
        }
        if cancellation.is_cancelled() {
            return Err(SubagentError::new(
                SubagentErrorCode::Cancelled,
                "subagent cancelled before start",
            ));
        }
        let outcome = self
            .runner
            .run_child(&request, cancellation)
            .await
            .map_err(|error| classify(&error))?;
        let id = outcome.id;
        let handle = match request.continuation {
            SubagentContinuation::OneShot => {
                if let Some(error) = outcome.initial_error {
                    return Err(error);
                }
                None
            }
            SubagentContinuation::Continuable => Some(Arc::new(NativeSubagentHandle {
                initial_error: outcome.initial_error,
                id: id.clone(),
                label: request.label().to_owned(),
                agent: outcome.agent,
                authority: outcome.authority,
                lease: tokio::sync::Mutex::new(outcome.lease),
            }) as Arc<dyn SubagentHandle>),
        };
        Ok(SubagentStarted {
            id,
            text: outcome.text,
            handle,
        })
    }
}

/// One live continuable native child.
struct NativeSubagentHandle {
    initial_error: Option<SubagentError>,
    lease: tokio::sync::Mutex<Option<crate::GitWorktreeLease>>,
    id: SubagentId,
    label: String,
    agent: Arc<Agent>,
    authority: SubagentAuthority,
}

#[async_trait]
impl SubagentHandle for NativeSubagentHandle {
    fn initial_run_error(&self) -> Option<SubagentError> {
        self.initial_error.clone()
    }

    fn id(&self) -> &SubagentId {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    async fn send(
        &self,
        text: &str,
        cancellation: CancellationToken,
    ) -> Result<String, SubagentError> {
        if cancellation.is_cancelled() {
            return Err(SubagentError::new(
                SubagentErrorCode::Cancelled,
                "subagent follow-up cancelled before start",
            ));
        }
        let report = TASK_AGENT
            .scope(
                self.agent.clone(),
                TASK_AUTHORITY.scope(
                    self.authority.clone(),
                    self.agent.send_cancellable(text, cancellation),
                ),
            )
            .await
            .map_err(|error| classify(&error))?;
        native_report_result(report)
    }

    fn deliver_mail(&self, id: &str, text: &str) -> Result<bool, SubagentError> {
        self.agent
            .deliver_team_mail(id, text)
            .map_err(|error| classify(&error))
    }

    async fn run_mail(&self, cancellation: CancellationToken) -> Result<(), SubagentError> {
        for _ in 0..64 {
            let Some(id) = self.agent.next_wakeable_message() else {
                return Ok(());
            };
            let result = TASK_AGENT
                .scope(
                    self.agent.clone(),
                    TASK_AUTHORITY.scope(
                        self.authority.clone(),
                        self.agent
                            .send_automatic_inbox_id_cancellable(&id, cancellation.clone()),
                    ),
                )
                .await;
            match result {
                Ok(report) if report.reason == "aborted" => {
                    return Err(SubagentError::new(
                        SubagentErrorCode::Cancelled,
                        "subagent inbox turn cancelled",
                    ));
                }
                Ok(report) => {
                    native_report_result(report)?;
                }
                Err(error) if error.downcast_ref::<crate::FollowUpError>().is_some() => {
                    return Ok(());
                }
                Err(error) => return Err(classify(&error)),
            }
        }
        Err(SubagentError::new(
            SubagentErrorCode::Refused,
            "native inbox turn budget exhausted",
        ))
    }

    async fn run_pending(
        &self,
        id: &heycode_session::InboxMessageId,
        cancellation: CancellationToken,
    ) -> Result<String, SubagentError> {
        for _ in 0..64 {
            let next = {
                let session = self
                    .agent
                    .session()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let inbox = session.inbox();
                let queue = if inbox.next_turn().iter().any(|message| message.id() == id) {
                    inbox.next_turn()
                } else if inbox.next_step().iter().any(|message| message.id() == id) {
                    inbox.next_step()
                } else {
                    return Err(SubagentError::new(
                        SubagentErrorCode::AlreadyConsumed,
                        "message already consumed",
                    ));
                };
                queue.first().map(|message| message.id().clone())
            }
            .ok_or_else(|| {
                SubagentError::new(SubagentErrorCode::Failed, "pending queue changed")
            })?;
            let result = TASK_AGENT
                .scope(
                    self.agent.clone(),
                    TASK_AUTHORITY.scope(
                        self.authority.clone(),
                        self.agent
                            .send_inbox_id_cancellable(&next, cancellation.clone()),
                    ),
                )
                .await;
            match result {
                Ok(report) if report.reason == "aborted" => {
                    return Err(SubagentError::new(
                        SubagentErrorCode::Cancelled,
                        "subagent turn cancelled",
                    ));
                }
                Ok(report) if &next == id => return native_report_result(report),
                Ok(report) => {
                    native_report_result(report)?;
                }
                Err(error) if error.downcast_ref::<crate::FollowUpError>().is_some() => {}
                Err(error) => return Err(classify(&error)),
            }
        }
        Err(SubagentError::new(
            SubagentErrorCode::Refused,
            "pending input drain budget exhausted",
        ))
    }

    fn interrupt(&self) -> bool {
        let token = self.agent.token();
        let active = token.is_turn_active();
        token.cancel();
        active
    }

    async fn close(&self, _cancellation: CancellationToken) -> Result<(), SubagentError> {
        // The registry is this child's only owner, so release is terminal.
        self.agent.shutdown_and_wait().await;
        if let Some(lease) = self.lease.lock().await.take() {
            lease
                .finish(crate::WorktreeOutcome::Success, CancellationToken::new())
                .await
                .map_err(|error| {
                    SubagentError::new(SubagentErrorCode::Failed, error.to_string())
                })?;
        }
        Ok(())
    }
}

/// A bounded/paused provider result is retained evidence, not a successful run.
fn native_report_result(report: crate::TurnReport) -> Result<String, SubagentError> {
    if report.reason == "stop" {
        return Ok(report.text);
    }
    let code = if report.reason == "aborted" {
        SubagentErrorCode::Cancelled
    } else {
        SubagentErrorCode::Failed
    };
    let message = if code == SubagentErrorCode::Cancelled {
        "subagent cancelled; partial response is retained in the child session".to_owned()
    } else {
        format!(
            "native run ended with {}; partial response is retained in the child session",
            report.reason
        )
    };
    let mut error = SubagentError::new(code, message.clone());
    if code == SubagentErrorCode::Failed {
        let mut partial = report.text;
        if partial.len() > 32 * 1024 {
            let mut end = 32 * 1024;
            while !partial.is_char_boundary(end) {
                end -= 1;
            }
            partial.truncate(end);
        }
        error = error.with_diagnostic(crate::TaskDiagnostic {
            message,
            code: Some(report.reason.to_owned()),
            stage: Some("native_turn".into()),
            partial_result: (!partial.is_empty()).then_some(partial),
            ..crate::TaskDiagnostic::default()
        });
    }
    Err(error)
}

fn classify(error: &anyhow::Error) -> SubagentError {
    if let Some(error) = error.downcast_ref::<SubagentError>() {
        return error.clone();
    }
    if let Some(llm) = error.downcast_ref::<heycode_llm::LlmError>() {
        let mut facts = vec![llm.class().as_str().to_owned()];
        if let Some(failure) = llm.provider_failure() {
            if let Some(status) = failure.status() {
                facts.push(format!("HTTP {status}"));
            }
            if let Some(code) = failure.code() {
                facts.push(code.as_str().to_owned());
            }
        } else if let heycode_llm::LlmError::Http { status, .. } = llm {
            facts.push(format!("HTTP {status}"));
        }
        return SubagentError::new(
            SubagentErrorCode::Failed,
            format!("{llm} [{}]", facts.join(", ")),
        );
    }
    let text = error.to_string();
    let code = if text.contains("depth limit") {
        SubagentErrorCode::Refused
    } else if text.contains("cancelled") {
        SubagentErrorCode::Cancelled
    } else {
        SubagentErrorCode::Failed
    };
    SubagentError::new(code, text)
}

/// The model-facing `task` tool wrapping a runner.
///
/// `mode:"continuable"` keeps the child alive; its result carries the
/// `task_id` needed by [`SendMessageTool`] / [`InterruptTaskTool`].
pub struct TaskTool {
    registry: Arc<SubagentRegistry>,
    root_authority: SubagentAuthority,
}

impl TaskTool {
    /// Bind the tool to the delegation registry.
    pub fn new(registry: Arc<SubagentRegistry>, root_authority: SubagentAuthority) -> Self {
        Self {
            registry,
            root_authority,
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn aliases(&self) -> &'static [&'static str] {
        &["task"]
    }
    fn supports_background(&self) -> bool {
        true
    }
    fn effect(&self) -> heycode_tools::ToolEffect {
        heycode_tools::ToolEffect::Orchestration
    }
    fn spec(&self) -> ToolSpec {
        let providers = self
            .registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id().as_str().to_owned())
            .collect::<Vec<_>>();
        let presets = self
            .registry
            .presets()
            .into_iter()
            .map(|preset| preset.id().as_str().to_owned())
            .collect::<Vec<_>>();
        let mut parameters = serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["label", "prompt"],
                "properties": {
                    "label": {
                        "type": "string",
                        "description": "Two-to-five word name for progress display, e.g. 'search auth bugs'"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Complete standalone instruction for the subagent"
                    },
                    "agent": {
                        "type": "string",
                        "description": "Optional declarative agent preset id from /agents",
                        "enum": presets
                    },
                    "provider": {
                        "type": "string",
                        "description": "Optional exact native/delegated subagent provider id from /agents",
                        "enum": providers
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["oneshot", "continuable", "fork", "fork-continuable"],
                        "description": "oneshot: fresh and ends after one run; continuable: fresh and retains context for send_message (default when supported); fork: inherits this conversation's context and ends after one turn; fork-continuable: inherits context and accepts follow-ups"
                    },
                    "isolation": {
                        "type": "string",
                        "enum": ["shared", "worktree"],
                        "description": "Native worktree copies current tracked/untracked changes and preserves results on finish/cancel/failure"
                    },
                    "background": {
                        "type": "boolean",
                        "default": true,
                        "description": "Default true: return the named agent identity immediately. Continue independent work or end your turn; its result arrives automatically. No inspect or wait call is needed. Set false only for a foreground dependency."
                    }
                }
        });
        // An empty `enum` is an invalid schema for every provider and reads as
        // "there are agents but none is allowed"; with no presets the property
        // is simply absent.
        if presets.is_empty()
            && let Some(properties) = parameters
                .get_mut("properties")
                .and_then(serde_json::Value::as_object_mut)
        {
            properties.remove("agent");
        }
        ToolSpec {
            name: "agent".to_owned(),
            description: "Delegate a self-contained subtask to a fresh subagent. Independent work runs in the background by default; use background=false for a blocking dependency. In `oneshot` and `continuable` modes it does \
                          NOT see this conversation; `fork` inherits it."
                .to_owned(),
            parameters,
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let label = args
            .get("label")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`label` must be a string"))?
            .to_owned();
        let prompt = args
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`prompt` must be a string"))?
            .to_owned();
        let preset = match args.get("agent") {
            None => None,
            Some(value) => {
                let id = value
                    .as_str()
                    .ok_or_else(|| ToolError::new("`agent` must be a string"))?;
                Some(self.registry.preset(id).ok_or_else(|| {
                    ToolError::new(format!("unknown agent preset `{id}` — see /agents"))
                })?)
            }
        };
        let explicit_mode = args
            .get("mode")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| ToolError::new("`mode` must be a string"))
            })
            .transpose()?;
        let authority = current_authority(&self.root_authority);
        let default_continuation = if authority.may_retain_children() {
            SubagentContinuation::Continuable
        } else {
            SubagentContinuation::OneShot
        };
        let (seed, continuation) = match explicit_mode {
            Some("continuable") => (SubagentSeed::Fresh, SubagentContinuation::Continuable),
            Some("fork_continuable" | "fork-continuable") => {
                (SubagentSeed::ForkParent, SubagentContinuation::Continuable)
            }
            Some("fork") => (SubagentSeed::ForkParent, SubagentContinuation::OneShot),
            Some("oneshot") => (SubagentSeed::Fresh, SubagentContinuation::OneShot),
            Some(_) => return Err(ToolError::new("`mode` is invalid")),
            None => preset
                .as_ref()
                .map_or((SubagentSeed::Fresh, default_continuation), |preset| {
                    (preset.seed(), preset.continuation())
                }),
        };
        if continuation == SubagentContinuation::Continuable && !authority.may_retain_children() {
            return Err(ToolError::new(
                "one-shot subagent authority cannot create a continuable child",
            ));
        }
        let mut request =
            SubagentRequest::with_authority(label, prompt, seed, continuation, authority.clone())
                .map_err(|error| ToolError::new(error.to_string()))?;
        if let Some(preset) = &preset {
            request = request
                .with_preset(preset)
                .map_err(|error| ToolError::new(error.to_string()))?;
        }
        let preset_provider = preset
            .as_ref()
            .and_then(|preset| preset.provider().cloned());
        let explicit_provider = match args.get("provider") {
            None => None,
            Some(value) => Some(
                SubagentProviderId::new(
                    value
                        .as_str()
                        .ok_or_else(|| ToolError::new("`provider` must be a string"))?,
                )
                .map_err(|error| ToolError::new(error.to_string()))?,
            ),
        };
        let provider = match (preset_provider, explicit_provider) {
            (Some(preset), Some(explicit)) if preset != explicit => {
                return Err(ToolError::new(
                    "`agent` preset and explicit `provider` select different providers",
                ));
            }
            (Some(provider), _) | (None, Some(provider)) => Some(provider),
            (None, None) => None,
        };
        if let Some(provider) = provider {
            if continuation == SubagentContinuation::Continuable
                && self.registry.descriptors().iter().any(|descriptor| {
                    descriptor.id() == &provider
                        && descriptor.capabilities().continuation
                            == heycode_llm::CapabilitySupport::Unsupported
                })
            {
                return Err(ToolError::new(
                    "this provider cannot retain conversations; choose mode=oneshot explicitly if follow-up messaging is unnecessary",
                ));
            }
            request = request.with_provider(provider);
        }
        if let Some(isolation) = args.get("isolation") {
            request = request.with_isolation(match isolation.as_str() {
                Some("shared") => crate::ChildIsolation::Shared,
                Some("worktree") => crate::ChildIsolation::Worktree,
                _ => return Err(ToolError::new("isolation must be shared or worktree")),
            });
        }
        let background = match args.get("background") {
            None => preset
                .as_ref()
                .is_none_or(|preset| preset.config().background),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| ToolError::new("`background` must be a boolean"))?,
        };
        if background {
            if cx.cancellation.is_cancelled() {
                return Err(ToolError::new("background task admission was cancelled"));
            }
            let (task_id, id) = self
                .registry
                .start_background_task(request, heycode_session::InboxDelivery::Steer)
                .map_err(|error| ToolError::new(error.to_string()))?;
            let snapshot = self
                .registry
                .task_snapshots_for(&authority)
                .into_iter()
                .find(|task| task.id == task_id.as_str());
            let label = snapshot
                .as_ref()
                .map_or_else(|| task_id.to_string(), |task| task.label.clone());
            let status = snapshot
                .as_ref()
                .map_or(serde_json::Value::String("queued".into()), |task| {
                    serde_json::json!(task.state)
                });
            return Ok(
                serde_json::json!({"agent_id":task_id.as_str(),"name":label,"status":status,
                "delivery":"automatic", "task_id":task_id.as_str(), "job_id":id.as_str()}),
            );
        }
        let started = self
            .registry
            .start(request, cx.cancellation.clone())
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        let text = match started.handle {
            Some(_) => format!(
                "{text}\n\n[task_id: {id} — continue with send_message(to=agent_id, message=...)]",
                text = started.text,
                id = started.id
            ),
            None => started.text,
        };
        Ok(serde_json::Value::String(text))
    }
}

/// `send_message {task_id, message}` — continue a live child's conversation.
pub struct SendMessageTool {
    registry: Arc<SubagentRegistry>,
    root_authority: SubagentAuthority,
}

impl SendMessageTool {
    /// Bind to the delegation registry.
    pub fn new(registry: Arc<SubagentRegistry>, root_authority: SubagentAuthority) -> Self {
        Self {
            registry,
            root_authority,
        }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn effect(&self) -> heycode_tools::ToolEffect {
        heycode_tools::ToolEffect::Orchestration
    }
    fn spec(&self) -> ToolSpec {
        let authority = current_authority(&self.root_authority);
        let roster = self.registry.message_roster(&authority);
        ToolSpec {
            name: "send_message".to_owned(),
            description: format!(
                "Send an asynchronous message to an agent by exact ID or unique name, or to parent/main. A running recipient reads it at a safe step; an idle continuable recipient resumes with retained context. Delivery is automatic: do useful work or end your turn. A message receipt is not an answer. Authorized recipients: {roster}"
            ),
            parameters: serde_json::json!({
                "type":"object", "additionalProperties":false, "required":["to","message"],
                "properties":{
                    "to":{"type":"string","description":"Exact agent ID, unique roster name, parent, or main"},
                    "message":{"type":"string","description":"The message content; sender identity is supplied by the runtime"}
                }
            }),
        }
    }
    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        if cx.cancellation.is_cancelled() {
            return Err(ToolError::new("message admission cancelled"));
        }
        let to = args
            .get("to")
            .or_else(|| args.get("task_id"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`to` must be a string"))?;
        let message = args
            .get("message")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`message` must be a string"))?;
        let authority = current_authority(&self.root_authority);
        // Historical clients explicitly used task_id with synchronous semantics.
        // The advertised API only accepts `to` and always admits asynchronously.
        if args.get("to").is_none()
            && args.get("background").and_then(serde_json::Value::as_bool) != Some(true)
            && args.get("steer").and_then(serde_json::Value::as_bool) != Some(true)
        {
            let id = SubagentId::new(to).map_err(|e| ToolError::new(e.to_string()))?;
            let child = self
                .registry
                .child_for(&authority, &id)
                .ok_or_else(|| ToolError::new("agent has no continuable handle"))?;
            return child
                .send(message, cx.cancellation.clone())
                .await
                .map(serde_json::Value::String)
                .map_err(|e| ToolError::new(e.to_string()));
        }
        self.registry
            .send_agent_message(&authority, to, message)
            .map_err(|error| ToolError::new(error.to_string()))
    }
}

/// `list_tasks {}` — live continuable children.
pub struct ListTasksTool {
    registry: Arc<SubagentRegistry>,
    root_authority: SubagentAuthority,
}

impl ListTasksTool {
    /// Bind to the delegation registry.
    pub fn new(registry: Arc<SubagentRegistry>, root_authority: SubagentAuthority) -> Self {
        Self {
            registry,
            root_authority,
        }
    }
}

#[async_trait]
impl Tool for ListTasksTool {
    fn model_replacement(&self) -> Option<&'static str> {
        Some("agent_control")
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["list_tasks"]
    }
    fn effect(&self) -> heycode_tools::ToolEffect {
        heycode_tools::ToolEffect::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_agents".to_owned(),
            description: "List admitted agent conversations with lifecycle and identity metadata. Results arrive automatically. Work items and process jobs are separate."
                .to_owned(),
            parameters: serde_json::json!({"type": "object", "additionalProperties": false}),
        }
    }

    async fn run(
        &self,
        _args: serde_json::Value,
        _cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        serde_json::to_value(serde_json::json!({"tasks": self.registry.task_snapshots_for(&current_authority(&self.root_authority)).into_iter().map(agent_status_metadata).collect::<Vec<_>>(), "budget": self.registry.budget_snapshot()}))
            .map_err(|error| ToolError::new(error.to_string()))
    }
}

/// `interrupt_task {task_id}` — cancel a child's active turn.
pub struct InterruptTaskTool {
    registry: Arc<SubagentRegistry>,
    root_authority: SubagentAuthority,
}

impl InterruptTaskTool {
    /// Bind to the delegation registry.
    pub fn new(registry: Arc<SubagentRegistry>, root_authority: SubagentAuthority) -> Self {
        Self {
            registry,
            root_authority,
        }
    }
}

#[async_trait]
impl Tool for InterruptTaskTool {
    fn model_replacement(&self) -> Option<&'static str> {
        Some("agent_control")
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "interrupt_task".to_owned(),
            description: "Control a task: interrupt, close (archive), restore, or wait for revision (up to 60 seconds).".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["task_id"],
                "properties": {
                    "task_id": {"type": "string"},
                    "action": {"enum": ["interrupt", "close", "restore", "wait"]},
                    "after_revision": {"type": "integer", "minimum": 0},
                    "timeout_ms": {"type": "integer", "minimum": 0, "maximum": 60000}
                }
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let id = args
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`task_id` must be a string"))?;
        let id = SubagentId::new(id).map_err(|error| ToolError::new(error.to_string()))?;
        let authority = current_authority(&self.root_authority);
        let ok = match args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("interrupt")
        {
            "interrupt" => self.registry.interrupt_for(&authority, &id),
            "close" => self
                .registry
                .archive_child_for(&authority, &id)
                .map_err(|e| ToolError::new(e.to_string()))?,
            "restore" => self
                .registry
                .restore_child_for(&authority, &id)
                .map_err(|e| ToolError::new(e.to_string()))?,
            "wait" => {
                let snapshot = self
                    .registry
                    .wait_task_for(
                        &authority,
                        &id,
                        args.get("after_revision")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        std::time::Duration::from_millis(
                            args.get("timeout_ms")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(10000),
                        ),
                        cx.cancellation.clone(),
                    )
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                return Ok(agent_status_metadata(snapshot));
            }
            _ => return Err(ToolError::new("invalid task action")),
        };
        Ok(serde_json::Value::String(if ok {
            "task control applied".into()
        } else {
            format!("no live task `{id}`")
        }))
    }
}

/// Canonical, action-discriminated conversation controls. Legacy tools remain
/// independently dispatchable because their schemas and result shapes differ.
struct AgentControlTool {
    registry: Arc<SubagentRegistry>,
    root_authority: SubagentAuthority,
}

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum AgentControlRequest {
    List,
    Wait {
        targets: Vec<AgentWaitTarget>,
        #[serde(default = "agent_wait_timeout")]
        timeout_ms: u64,
    },
    Send {
        agent_id: String,
        message: String,
        #[serde(default)]
        background: bool,
        #[serde(default)]
        steer: bool,
    },
    Interrupt {
        agent_id: String,
    },
    Archive {
        agent_id: String,
    },
    Restore {
        agent_id: String,
    },
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentWaitTarget {
    agent_id: String,
    #[serde(default)]
    after_revision: u64,
}

fn agent_wait_timeout() -> u64 {
    10000
}

#[async_trait]
impl Tool for AgentControlTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "agent_control".into(),
            description: "Explicit agent lifecycle control. Interrupt requests cancellation; archive hides a settled conversation; restore reopens an archived conversation. Send messages with send_message. Results arrive automatically without polling.".into(),
            parameters: serde_json::json!({
                "type":"object", "additionalProperties":false, "required":["action"],
                "properties": {
                    "action":{"type":"string","enum":["interrupt","archive","restore"]},
                    "agent_id":{"type":"string","description":"Agent conversation ID from agent or agent_control; never a job ID"},

                },
                "oneOf":[
                    {"properties":{"action":{"enum":["interrupt","archive","restore"]}},"required":["agent_id"]}
                ]
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        // Parse the complete discriminated request before obtaining control or
        // mutating anything. Missing/invalid actions never default to interrupt.
        let request: AgentControlRequest = serde_json::from_value(args)
            .map_err(|error| ToolError::new(format!("invalid agent_control request: {error}")))?;
        let authority = current_authority(&self.root_authority);
        let parse_id = |id: &str| SubagentId::new(id).map_err(|e| ToolError::new(e.to_string()));
        let (action, raw_id) = match request {
            AgentControlRequest::List => {
                return Ok(serde_json::json!({
                    "action":"list", "agents":self.registry.task_snapshots_for(&authority).into_iter().map(agent_status_metadata).collect::<Vec<_>>(), "budget":self.registry.budget_snapshot()
                }));
            }
            AgentControlRequest::Wait {
                targets,
                timeout_ms,
            } => {
                if !(1..=8).contains(&targets.len()) || timeout_ms > 60000 {
                    return Err(ToolError::new(
                        "wait requires 1..=8 targets and timeout_ms 0..=60000",
                    ));
                }
                let targets = targets
                    .into_iter()
                    .map(|target| Ok((parse_id(&target.agent_id)?, target.after_revision)))
                    .collect::<Result<Vec<_>, ToolError>>()?;
                if targets
                    .iter()
                    .map(|(id, _)| id)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != targets.len()
                {
                    return Err(ToolError::new(
                        "wait targets must contain distinct agent IDs",
                    ));
                }
                let agents = self
                    .registry
                    .wait_tasks_for(
                        &authority,
                        &targets,
                        std::time::Duration::from_millis(timeout_ms),
                        cx.cancellation.clone(),
                    )
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                let changed = agents
                    .iter()
                    .zip(&targets)
                    .any(|(agent, (_, revision))| agent.revision > *revision);
                return Ok(
                    serde_json::json!({"action":"wait","agents":agents.into_iter().map(agent_status_metadata).collect::<Vec<_>>(),"changed":changed}),
                );
            }
            AgentControlRequest::Send {
                agent_id,
                message,
                background,
                steer,
            } => {
                parse_id(&agent_id)?;
                let result = SendMessageTool::new(self.registry.clone(), self.root_authority.clone())
                    .run(serde_json::json!({"task_id":agent_id,"message":message,"background":background,"steer":steer}), cx).await?;
                return Ok(
                    serde_json::json!({"action":"send","agent_id":agent_id,"result":result}),
                );
            }
            AgentControlRequest::Interrupt { agent_id } => ("interrupt", agent_id),
            AgentControlRequest::Archive { agent_id } => ("archive", agent_id),
            AgentControlRequest::Restore { agent_id } => ("restore", agent_id),
        };
        let id = parse_id(&raw_id)?;
        let requested = match action {
            "interrupt" => self.registry.interrupt_for(&authority, &id),
            "archive" => self
                .registry
                .archive_child_for(&authority, &id)
                .map_err(|e| ToolError::new(e.to_string()))?,
            "restore" => self
                .registry
                .restore_child_for(&authority, &id)
                .map_err(|e| ToolError::new(e.to_string()))?,
            _ => return Err(ToolError::new("invalid agent control action")),
        };
        Ok(
            serde_json::json!({"action":action,"agent_id":id.as_str(),"requested":requested,
            "status":if !requested {"not_found"} else if action == "interrupt" {"requested"} else {"applied"}}),
        )
    }
}

fn agent_status_metadata(snapshot: crate::TaskSnapshot) -> serde_json::Value {
    serde_json::json!({"id":snapshot.id,"label":snapshot.label,"owner":snapshot.owner,
        "state":snapshot.state,"revision":snapshot.revision,"provider":snapshot.provider,
        "session_id":snapshot.session_id,"job_id":snapshot.job_id})
}

/// Provide the `task` tool into the already-published `"tools"` registry.
///
/// Injects: `providers`, `models`, `llm`, `tools`, `seam/pre_tool`, `prompt`,
/// `approval`, `compactions`. Must load AFTER those services exist.
pub fn subagent_plugin(sessions_root: std::path::PathBuf, max_depth: u32) -> Box<dyn Plugin> {
    subagent_plugin_with_budget(
        sessions_root,
        max_depth,
        crate::SubagentBudgetLimits::default(),
    )
}

/// Compose native delegation with explicit session-wide inference/spend guardrails.
pub fn subagent_plugin_with_budget(
    sessions_root: std::path::PathBuf,
    max_depth: u32,
    budget: crate::SubagentBudgetLimits,
) -> Box<dyn Plugin> {
    struct SubagentPlugin(std::path::PathBuf, u32, crate::SubagentBudgetLimits);
    impl Plugin for SubagentPlugin {
        fn name(&self) -> &'static str {
            "subagent"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "subagent",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Tool,
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Provider,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                "agent",
                "send_message",
                "list_agents",
                "interrupt_task",
                "agent_control",
            ]
            .into_iter()
            .map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    name,
                )
            })
            .collect()
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_SUBAGENTS]
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_session::SERVICE_SESSION,
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                heycode_llm::SERVICE_MODELS,
                heycode_llm::SERVICE_LLM,
                heycode_llm::SERVICE_TOKEN_COUNTERS,
                crate::SERVICE_COMPACTIONS,
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
                heycode_tools::SERVICE_TOOLS,
                heycode_tools::SEAM_PRE_TOOL,
                heycode_prompt::SERVICE_PROMPT,
                crate::SERVICE_APPROVAL,
            ]
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> CoreResult<()> {
            let providers = ctx
                .get::<ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
                .ok_or_else(|| CoreError::other("providers missing"))?;
            let provider_interception = ctx
                .get::<ProviderInterception>(heycode_llm::SERVICE_PROVIDER_INTERCEPTION)
                .ok_or_else(|| CoreError::other("provider interception missing"))?;
            let catalogs = ctx
                .get::<CatalogRegistry>(heycode_llm::SERVICE_MODELS)
                .ok_or_else(|| CoreError::other("models missing"))?;
            let selection = ctx
                .get::<LlmSelection>(heycode_llm::SERVICE_LLM)
                .ok_or_else(|| CoreError::other("llm missing"))?;
            let token_counters = ctx
                .get::<TokenCounterRegistry>(heycode_llm::SERVICE_TOKEN_COUNTERS)
                .ok_or_else(|| CoreError::other("token-counters missing"))?;
            let compactions = ctx
                .get::<crate::CompactionRegistry>(crate::SERVICE_COMPACTIONS)
                .ok_or_else(|| CoreError::other("compactions missing"))?;
            let native_tools = ctx
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| CoreError::other("native-tools missing"))?;
            let tools = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tools missing"))?;
            let pre_seam = ctx
                .get::<Waterfall<PreToolDecision>>(heycode_tools::SEAM_PRE_TOOL)
                .ok_or_else(|| CoreError::other("pre-tool seam missing"))?;
            let prompt = ctx
                .get::<PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
                .ok_or_else(|| CoreError::other("prompt missing"))?;
            let approval = ctx
                .get::<crate::plugin::ApprovalHandle>(crate::SERVICE_APPROVAL)
                .ok_or_else(|| CoreError::other("approval missing"))?;
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

            let parent_session = ctx
                .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session missing"))?;
            let root_owner = {
                let session = parent_session
                    .lock()
                    .map_err(|_| CoreError::other("session unavailable"))?;
                SubagentId::new(session.id().as_str())
                    .map_err(|error| CoreError::other(error.to_string()))?
            };
            ctx.provide(
                crate::SERVICE_SUBAGENTS,
                "subagent",
                SubagentRegistry::with_budget(self.2),
            )?;
            let registry = ctx
                .get::<SubagentRegistry>(crate::SERVICE_SUBAGENTS)
                .ok_or_else(|| CoreError::other("subagent registry missing"))?;
            for preset in crate::builtin_native_presets()
                .map_err(|error| CoreError::other(error.to_string()))?
            {
                ctx.contribute(
                    heycode_core::ContributionKind::AgentPresetFallback,
                    preset.id().as_str().to_owned(),
                )?;
                let registration = registry
                    .register_fallback_preset_owned(preset)
                    .map_err(|error| CoreError::other(error.to_string()))?;
                ctx.effect(move || drop(registration));
            }
            let runner = Arc::new(
                SubagentRunner::new(
                    providers,
                    provider_interception,
                    catalogs,
                    compactions,
                    token_counters,
                    native_tools,
                    (*selection).clone(),
                    tools.clone(),
                    pre_seam,
                    prompt,
                    approval.0.clone(),
                    parent_session,
                    self.0.clone(),
                    cwd,
                    ctx.events.clone(),
                    self.1,
                )
                .with_lifecycle_hooks(registry.lifecycle_hook_slot())
                .with_registry(&registry)
                .with_execution(
                    ctx.get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                        .map(|service| (*service).clone()),
                    ctx.get::<heycode_exec::SandboxService>(heycode_exec::SERVICE_SANDBOX)
                        .map(|service| (*service).clone()),
                    ctx.get::<heycode_exec::ShellService>(heycode_exec::SERVICE_SHELL)
                        .map(|service| (*service).clone()),
                ),
            );
            let root_authority = registry.root_authority(root_owner);
            let provider = Arc::new(
                NativeSubagentProvider::new(runner)
                    .map_err(|error| CoreError::other(error.to_string()))?,
            );
            registry
                .register(provider)
                .map_err(|error| CoreError::other(error.to_string()))?;
            // Plan mode gates delegation to providers that do not inherit this
            // agent's tool guards. Whichever of `plan`/`subagent` applies
            // second owns the wiring, so the gate is installed exactly once in
            // either composition order; the shipped order applies `plan` last.
            if let Some(plan) = ctx.get::<crate::plan::PlanHandle>(crate::SERVICE_PLAN) {
                registry
                    .attach_delegation_gate(plan.0.clone())
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            // One effect owns both the provider rows and every continuable
            // child, so shutdown cannot leave an orphaned child agent holding a
            // durable session.
            let disposable = registry.clone();
            ctx.effect(move || disposable.dispose());
            for tool in [
                Arc::new(TaskTool::new(registry.clone(), root_authority.clone())) as Arc<dyn Tool>,
                Arc::new(SendMessageTool::new(
                    registry.clone(),
                    root_authority.clone(),
                )),
                Arc::new(ListTasksTool::new(registry.clone(), root_authority.clone())),
                Arc::new(InterruptTaskTool::new(
                    registry.clone(),
                    root_authority.clone(),
                )),
                Arc::new(AgentControlTool {
                    registry,
                    root_authority,
                }),
            ] {
                tools
                    .register_shared(tool)
                    .map_err(|e| CoreError::other(e.to_string()))?;
            }
            Ok(())
        }
    }
    Box::new(SubagentPlugin(sessions_root, max_depth, budget))
}

/// Attach the already-composed Agent/job owner to the subagent registry.
///
/// Kept as a separate effect plugin because `subagent` must publish the task
/// tools before `agent`, while the background bridge necessarily consumes the
/// Agent and its job service after both exist.
#[must_use]
pub fn subagent_jobs_plugin() -> Box<dyn Plugin> {
    struct SubagentJobsPlugin;

    impl Plugin for SubagentJobsPlugin {
        fn name(&self) -> &'static str {
            "subagent-jobs"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_SUBAGENTS,
                crate::SERVICE_AGENT,
                crate::SERVICE_JOBS,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> CoreResult<()> {
            let registry = context
                .get::<SubagentRegistry>(crate::SERVICE_SUBAGENTS)
                .ok_or_else(|| CoreError::other("subagent registry missing"))?;
            let agent = context
                .get::<Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| CoreError::other("agent missing"))?;
            let jobs = context
                .get::<Arc<crate::JobRegistry>>(crate::SERVICE_JOBS)
                .ok_or_else(|| CoreError::other("job registry missing"))?;
            let history_root = agent
                .session()
                .lock()
                .map_err(|_| CoreError::other("session unavailable"))?
                .path()
                .parent()
                .ok_or_else(|| CoreError::other("session directory missing"))?
                .join("tasks");
            registry
                .attach_task_history(history_root)
                .map_err(|error| CoreError::other(error.to_string()))?;
            registry
                .attach_job_host(context, &agent, (*jobs).clone())
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(SubagentJobsPlugin)
}

/// Current child execution authority, absent for the root conversation.
pub(crate) fn scoped_authority() -> Option<SubagentAuthority> {
    TASK_AUTHORITY.try_with(Clone::clone).ok()
}

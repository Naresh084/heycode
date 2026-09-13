//! Native owner binding and race-free workspace transition admission.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use async_trait::async_trait;

use super::Agent;
use crate::workspace_transition::{
    WorkspaceSnapshot, WorkspaceTransitionError, WorkspaceTransitionGuard,
    WorkspaceTransitionOrigin, WorkspaceTransitionPermit, WorkspaceTransitionService,
};

tokio::task_local! {
    /// Minted only around an actual native foreground worktree tool barrier.
    pub(super) static MODEL_BARRIER_AGENT: usize;
}

pub(super) struct WorkspaceBinding {
    service: Arc<WorkspaceTransitionService>,
    user_home: Option<PathBuf>,
    trusted: bool,
    quiescence: Arc<NativeWorkspaceGuard>,
}

/// Additional composition-specific scope owners (MCP, LSP, extensions, skills).
pub type WorkspaceCompositionCheck = Arc<
    dyn Fn(WorkspaceTransitionOrigin) -> anyhow::Result<Box<dyn WorkspaceTransitionPermit>>
        + Send
        + Sync,
>;

impl Agent {
    /// Shared authority owner for host Consumers that pin auxiliary activity.
    pub fn workspace_service(&self) -> Option<Arc<WorkspaceTransitionService>> {
        self.workspace
            .lock()
            .ok()?
            .as_ref()
            .map(|binding| binding.service.clone())
    }

    /// Bind the production authority owner once, after job/subagent/terminal
    /// registries exist. New turns and all registry admissions are fenced.
    // Composition binds independent authority owners together exactly once.
    #[allow(clippy::too_many_arguments)]
    pub fn install_workspace(
        self: &Arc<Self>,
        service: Arc<WorkspaceTransitionService>,
        subagents: Arc<crate::SubagentRegistry>,
        terminals: heycode_exec::TerminalService,
        user_home: Option<PathBuf>,
        trusted: bool,
        composition_check: WorkspaceCompositionCheck,
        recomposition_check: Arc<
            dyn Fn() -> anyhow::Result<Box<dyn WorkspaceTransitionPermit>> + Send + Sync,
        >,
        close_protocol: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<()> {
        let mut slot = self
            .workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("workspace binding unavailable"))?;
        anyhow::ensure!(slot.is_none(), "workspace is already bound");
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("job owner unavailable"))?
            .clone()
            .ok_or_else(|| anyhow::anyhow!("workspace requires the job registry"))?;
        let quiescence = Arc::new(NativeWorkspaceGuard {
            agent: Arc::downgrade(self),
            jobs,
            subagents,
            terminals,
            composition_check,
            recomposition_check,
            close_protocol,
        });
        service.install_guard(quiescence.clone())?;
        *slot = Some(WorkspaceBinding {
            service,
            user_home,
            trusted,
            quiescence,
        });
        Ok(())
    }

    /// Hold this human-only quiescence permit until the old front end exits and
    /// composition teardown begins. Project providers may be refreshed by the
    /// new composition, so their static workspace restrictions do not apply.
    pub async fn acquire_recomposition_permit(&self) -> anyhow::Result<RecompositionPermit> {
        let (quiescence, service) = {
            let slot = self
                .workspace
                .lock()
                .map_err(|_| anyhow::anyhow!("workspace owner unavailable"))?;
            let binding = slot.as_ref().ok_or_else(|| {
                anyhow::anyhow!("recomposition requires the production workspace owner")
            })?;
            (binding.quiescence.clone(), binding.service.clone())
        };
        let permits = quiescence.acquire_recomposition()?;
        let activity = service.pause_activity()?;
        Ok(RecompositionPermit {
            _permits: Box::new((permits, activity)),
            quiescence,
            service,
        })
    }

    /// The authoritative current-session scope, separate from startup metadata.
    pub fn workspace_snapshot(&self) -> anyhow::Result<Option<WorkspaceSnapshot>> {
        let slot = self
            .workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("workspace binding unavailable"))?;
        slot.as_ref()
            .map(|binding| binding.service.snapshot().map_err(anyhow::Error::from))
            .transpose()
    }

    /// Current trusted project/user instruction sources; root expansion does
    /// not turn new project files into trusted guidance.
    pub fn current_instruction_sources(
        &self,
    ) -> Option<heycode_prompt::instructions::InstructionSources> {
        if let Some(sources) = &self.inherited_instruction_sources {
            return Some(sources.clone());
        }
        let slot = match self.workspace.lock() {
            Ok(slot) => slot,
            Err(_) => return Some(heycode_prompt::instructions::InstructionSources::default()),
        };
        slot.as_ref().map(|binding| {
            binding
                .service
                .instruction_sources(binding.user_home.clone(), binding.trusted)
                .unwrap_or_else(|_| heycode_prompt::instructions::InstructionSources {
                    user_home: binding.user_home.clone(),
                    workspace: None,
                })
        })
    }

    /// A child pins guidance from its actual spawn directory; it cannot acquire
    /// new parent roots or sources by resuming an older conversation.
    pub(crate) fn inherit_workspace_guidance(&mut self, parent: &Agent) {
        self.inherited_instruction_sources = parent.current_instruction_sources().map(|sources| {
            heycode_prompt::instructions::InstructionSources {
                user_home: sources.user_home,
                workspace: sources.workspace.map(|_| self.cwd.clone()),
            }
        });
    }
}

struct NativeWorkspaceGuard {
    agent: Weak<Agent>,
    jobs: Arc<crate::JobRegistry>,
    subagents: Arc<crate::SubagentRegistry>,
    terminals: heycode_exec::TerminalService,
    composition_check: WorkspaceCompositionCheck,
    recomposition_check:
        Arc<dyn Fn() -> anyhow::Result<Box<dyn WorkspaceTransitionPermit>> + Send + Sync>,
    close_protocol: Arc<dyn Fn() + Send + Sync>,
}

/// A real quiescence fence. Drop to abandon; `begin_shutdown` permanently
/// closes the old owners before releasing the turn gate for Context teardown.
pub struct RecompositionPermit {
    _permits: Box<dyn WorkspaceTransitionPermit>,
    quiescence: Arc<NativeWorkspaceGuard>,
    service: Arc<WorkspaceTransitionService>,
}
impl RecompositionPermit {
    /// Irreversible final restart/handoff boundary; does not await the turn gate
    /// it currently holds, so subsequent `shutdown_and_wait` cannot deadlock.
    pub fn begin_shutdown(self) {
        if let Some(agent) = self.quiescence.agent.upgrade() {
            agent.cancel.shutdown();
        }
        self.quiescence.jobs.dispose();
        self.quiescence.subagents.close_workspace_admission();
        self.quiescence.terminals.close();
        self.service.dispose();
        (self.quiescence.close_protocol)();
    }
}

impl NativeWorkspaceGuard {
    fn acquire_recomposition(&self) -> anyhow::Result<Box<dyn WorkspaceTransitionPermit>> {
        let agent = self
            .agent
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("agent owner unavailable"))?;
        let turn = agent
            .turn_gate
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("Recomposition requires an idle foreground turn"))?;
        let jobs = self.jobs.pause_workspace().map_err(|_| {
            anyhow::anyhow!("Active or queued jobs must settle before recomposition")
        })?;
        let children = self
            .subagents
            .pause_workspace()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let terminals = self.terminals.pause_workspace().map_err(|_| {
            anyhow::anyhow!("Open or retiring terminals must close before recomposition")
        })?;
        let protocol = (self.recomposition_check)()?;
        Ok(Box::new((turn, jobs, children, terminals, protocol)))
    }
}

#[async_trait]
impl WorkspaceTransitionGuard for NativeWorkspaceGuard {
    async fn acquire(
        &self,
        origin: WorkspaceTransitionOrigin,
    ) -> Result<Box<dyn WorkspaceTransitionPermit>, WorkspaceTransitionError> {
        let agent = self
            .agent
            .upgrade()
            .ok_or_else(|| error("Native workspace owner is unavailable"))?;
        let turn = match origin {
            WorkspaceTransitionOrigin::HumanCommand => {
                Some(agent.turn_gate.clone().try_lock_owned().map_err(|_| {
                    error("A foreground turn owns the workspace; wait for it to finish")
                })?)
            }
            WorkspaceTransitionOrigin::ModelTool => {
                if MODEL_BARRIER_AGENT
                    .try_with(|id| *id == Arc::as_ptr(&agent) as usize)
                    .unwrap_or(false)
                    && agent.cancel.is_turn_active()
                {
                    None
                } else {
                    return Err(error(
                        "Worktree tools require this session's native foreground tool barrier; child, workflow and delegated callbacks cannot change parent authority",
                    ));
                }
            }
        };
        let runtime = agent
            .session
            .lock()
            .map_err(|_| error("Session owner unavailable"))?
            .runtime_link()
            .map(|(runtime, _)| runtime.to_owned());
        if runtime
            .as_deref()
            .is_some_and(|runtime| runtime != "native")
        {
            return Err(error(
                "A delegated runtime owns this session; reopen a native session before changing workspace",
            ));
        }
        let jobs = self.jobs.pause_workspace().map_err(|_| error("Active or queued jobs retain workspace authority; stop them and wait for settlement"))?;
        let children = self
            .subagents
            .pause_workspace()
            .map_err(|value| error(&value.to_string()))?;
        let terminals = self.terminals.pause_workspace().map_err(|_| error("Open, opening or retiring terminals retain workspace authority; close them before changing workspace"))?;
        let composition =
            (self.composition_check)(origin).map_err(|value| error(&value.to_string()))?;
        Ok(Box::new((turn, jobs, children, terminals, composition)))
    }
}
fn error(message: &str) -> WorkspaceTransitionError {
    WorkspaceTransitionError::new(message)
}

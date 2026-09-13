//! Plan mode: a durable read-only collaboration state with real enforcement.
//!
//! State lives in the session log as `plan/mode` events (last-wins fold) and
//! is enforced by local/delegated tool guards, provider request policy, hook
//! execution policy, prompt context and a dedicated explicit human review.
//! The core review path is mounted by [`plan_plugin`]:
//! 1. a guard layer on `seam/pre_tool` that admits only read-only tools and
//!    denies everything else,
//! 2. a [`crate::DelegationGate`] on the subagent registry, because a
//!    delegated child is an agent this seam cannot reach,
//! 3. a prompt section telling the model what plan mode means,
//! 4. the `exit_plan_mode` tool, whose typed review is independent of tool approval.

use std::sync::Arc;

use async_trait::async_trait;

use heycode_core::{CoreError, CoreResult, Plugin, ToolSpec, Waterfall};
use heycode_session::{Session, SessionEventKind};
use heycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry, Verdict};

use crate::agent::Agent;
use crate::approval::ApprovalPolicy;
use crate::commands::Command;
use crate::ui::UiEvent;
use crate::{CommandArgument, CommandDescriptor, CommandSource, CommandTiming};

/// Shared plan-mode state: durable log + in-process fold.
pub struct PlanMode {
    session: Arc<std::sync::Mutex<Session>>,
    state: std::sync::Mutex<PlanState>,
    bus: heycode_core::EventBus,
    jobs: std::sync::Mutex<Option<Arc<crate::JobRegistry>>>,
    terminal: std::sync::Mutex<Option<heycode_exec::TerminalService>>,
    review_gate: tokio::sync::Mutex<()>,
    hooks: std::sync::Mutex<Option<Arc<heycode_hooks::HookService>>>,
}

struct PlanState {
    active: bool,
    pending: Option<bool>,
    review: Option<PlanReviewRecord>,
    review_cancellation: tokio_util::sync::CancellationToken,
}

/// An explicit human decision. Ordinary tool grants cannot construct a plan acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReviewDecision {
    /// Implement with conversation-scoped exact edit grants.
    AcceptedEdits,
    /// Implement and ask for every action requiring approval.
    DefaultPermissions,
    /// Continue planning with the user's retained feedback.
    StayInPlan {
        /// Revision guidance from the reviewer.
        feedback: String,
    },
}
impl PlanReviewDecision {
    /// Dismissal always keeps mutation blocking active.
    #[must_use]
    pub fn dismissed() -> Self {
        Self::StayInPlan {
            feedback: "Plan review dismissed; remain in Plan".into(),
        }
    }
}

/// Last durable proposal and its review, available after process resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewRecord {
    /// Complete proposed document.
    pub plan: String,
    /// Pending, accepted_edits, default or stay_in_plan.
    pub decision: String,
    /// Human feedback, retained with the proposal.
    pub feedback: String,
}

/// Outcome of one human/review plan-mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSelection {
    /// Idle selection appended `plan/mode` immediately.
    Committed,
    /// Mid-turn selection waits for the next accepted pre-step boundary.
    Queued,
    /// A pending selection was reversed to the already-committed state.
    Cancelled,
    /// The requested target already matches committed or pending state.
    Noop,
}

impl PlanMode {
    /// Fold current state from the log (resume-safe).
    pub fn from_log(session: &Arc<std::sync::Mutex<Session>>, bus: heycode_core::EventBus) -> Self {
        let mut active = false;
        let mut review = None;
        for event in session.lock().unwrap_or_else(|e| e.into_inner()).events() {
            match &event.kind {
                SessionEventKind::PlanMode { active: value } => active = *value,
                SessionEventKind::PlanReview {
                    plan,
                    decision,
                    feedback,
                } => {
                    active = !matches!(decision.as_str(), "accepted_edits" | "default");
                    review = Some(PlanReviewRecord {
                        plan: plan.clone(),
                        decision: decision.clone(),
                        feedback: feedback.clone(),
                    });
                }
                _ => {}
            }
        }
        Self {
            session: session.clone(),
            state: std::sync::Mutex::new(PlanState {
                active,
                pending: None,
                review,
                review_cancellation: tokio_util::sync::CancellationToken::new(),
            }),
            bus,
            jobs: std::sync::Mutex::new(None),
            terminal: std::sync::Mutex::new(None),
            review_gate: tokio::sync::Mutex::new(()),
            hooks: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn switch_policy_if_not_planning(
        &self,
        update: impl FnOnce() -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let state = self.state.lock().map_err(|_| "Plan state unavailable")?;
        if state.active || state.pending == Some(true) {
            return Err(
                "Remain in Plan until the full plan is explicitly accepted via exit_plan_mode",
            );
        }
        update()
    }

    /// A user command may leave Plan without accepting a model's proposal.
    /// Commit the mode event and policy together while the Plan guard is locked.
    pub(crate) fn switch_policy_by_user(
        &self,
        update: impl FnOnce(&mut (dyn FnMut() -> Result<(), String> + Send)) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "Plan state unavailable")?;
        let leaving = state.active || state.pending.is_some();
        update(&mut || {
            if leaving {
                let mut session = self.session.lock().map_err(|_| "Session unavailable")?;
                session
                    .append(SessionEventKind::PlanMode { active: false })
                    .map_err(|error| error.to_string())?;
                session.flush().map_err(|error| error.to_string())?;
            }
            Ok(())
        })?;
        if leaving {
            state.active = false;
            state.pending = None;
            state.review_cancellation.cancel();
            state.review_cancellation = tokio_util::sync::CancellationToken::new();
        }
        Ok(())
    }

    /// Explicit `/plan off` returns to Default without recording plan acceptance.
    pub fn leave_by_user(&self, approval: &dyn ApprovalPolicy) -> Result<(), String> {
        let target = crate::ApprovalPolicyKind::Ask;
        self.switch_policy_by_user(|commit| approval.commit_plan_transition(target, commit))?;
        self.bus
            .emit(UiEvent::PermissionModeChanged { mode: target });
        Ok(())
    }

    /// Last complete proposal, including pending or rejected reviews after resume.
    #[must_use]
    pub fn review(&self) -> Option<PlanReviewRecord> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.review.clone())
    }

    /// Check the same Plan authority before prompting and again at execution.
    pub(crate) fn tool_refusal(&self, name: &str, args: &serde_json::Value) -> Option<String> {
        (self.active() && !is_read_only_call(name, args)).then(|| {
            "plan mode is active — present your plan via exit_plan_mode; \
             the user must approve before you may modify anything"
                .to_owned()
        })
    }

    pub(crate) fn attach_jobs(&self, jobs: Arc<crate::JobRegistry>) {
        *self.jobs.lock().unwrap_or_else(|e| e.into_inner()) = Some(jobs);
    }

    async fn settle_activity(&self) -> Result<(), heycode_session::AppendError> {
        let jobs = self.jobs.lock().ok().and_then(|slot| slot.clone());
        let terminals = self.terminal.lock().ok().and_then(|slot| slot.clone());
        let session_id = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .id()
            .to_string();
        let hooks = self.hooks.lock().ok().and_then(|slot| slot.clone());
        let settle = async {
            if let Some(hooks) = hooks {
                hooks.wait_for_idle().await;
            }
            if let Some(jobs) = jobs {
                let _cancelled = jobs.cancel_all();
                for job in jobs.list() {
                    jobs.wait_for_task_exit(&job.id)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            if let Some(terminals) = terminals {
                for owner in ["world".to_owned(), format!("session:{session_id}")] {
                    let owner =
                        heycode_exec::TerminalOwner::new(owner).map_err(|e| e.to_string())?;
                    for terminal in terminals.list(&owner).await {
                        terminals
                            .kill(&owner, terminal.id())
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            Ok::<(), String>(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), settle).await
            .map_err(|_| heycode_session::AppendError::InvalidEvent { message: "Plan entry is still blocked on active execution; stop the running work and retry /plan on".into() })?
            .map_err(|message| heycode_session::AppendError::InvalidEvent { message })
    }

    /// Whether plan mode currently blocks mutating tools.
    #[must_use]
    pub fn active(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.active || state.pending == Some(true))
            .unwrap_or(true)
    }

    /// Mid-turn target waiting for the next pre-step, when one differs from
    /// the durable state.
    #[must_use]
    pub fn pending(&self) -> Option<bool> {
        self.state.lock().ok().and_then(|state| state.pending)
    }

    /// Switch plan mode: durable event FIRST, in-process fold after.
    ///
    /// # Errors
    /// Log append failures.
    pub async fn set(&self, active: bool) -> Result<PlanSelection, heycode_session::AppendError> {
        if active
            && self
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .runtime_link()
                .is_some_and(|(runtime, _)| runtime != "native")
        {
            return Err(heycode_session::AppendError::InvalidEvent { message: "Plan requires the native heycode runtime; this delegated runtime cannot prove read-only enforcement".into() });
        }
        if !active && self.active() {
            return Err(heycode_session::AppendError::InvalidEvent {
                message: "Plan exit requires explicit full-plan review via exit_plan_mode".into(),
            });
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.active == active && state.pending.is_none() {
                return Ok(PlanSelection::Noop);
            }
            // Persist entry before waiting: crash/restart must also remain read-only.
            self.session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .append(SessionEventKind::PlanMode { active: true })?;
            state.pending = Some(active);
        }
        let turn_open = has_open_turn(
            self.session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .events(),
        );
        if turn_open {
            self.bus.emit(UiEvent::Info { text: "Entering Plan: new mutations blocked; waiting for the current tool batch and background work to settle".into() });
            return Ok(PlanSelection::Queued);
        }
        self.commit_pending().await
    }

    pub(crate) async fn commit_pending(
        &self,
    ) -> Result<PlanSelection, heycode_session::AppendError> {
        if self.pending().is_none() {
            return Ok(PlanSelection::Noop);
        }
        self.settle_activity().await?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(active) = state.pending else {
            return Ok(PlanSelection::Noop);
        };
        self.commit_locked(&mut state, active)?;
        drop(state);
        self.announce(active);
        Ok(PlanSelection::Committed)
    }

    fn record_review(&self, plan: &str, decision: &str, feedback: &str) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "Plan state unavailable")?;
        if !state.active || state.pending.is_some() {
            return Err("Plan mode changed before this review could be recorded".into());
        }
        self.session
            .lock()
            .map_err(|_| "Session unavailable")?
            .append(SessionEventKind::PlanReview {
                plan: plan.into(),
                decision: decision.into(),
                feedback: feedback.into(),
            })
            .map_err(|e| e.to_string())?;
        state.review = Some(PlanReviewRecord {
            plan: plan.into(),
            decision: decision.into(),
            feedback: feedback.into(),
        });
        Ok(())
    }

    fn accept(
        &self,
        plan: &str,
        target: crate::ApprovalPolicyKind,
        approval: &dyn ApprovalPolicy,
    ) -> Result<(), String> {
        // Hold Plan authority throughout policy validation, durable commit, and live swap.
        let mut state = self.state.lock().map_err(|_| "Plan state unavailable")?;
        if !state.active || state.pending.is_some() {
            return Err("Plan entry must settle before review acceptance".into());
        }
        let decision = if target == crate::ApprovalPolicyKind::AcceptedEdits {
            "accepted_edits"
        } else {
            "default"
        };
        let record = PlanReviewRecord {
            plan: plan.into(),
            decision: decision.into(),
            feedback: String::new(),
        };
        approval.commit_plan_transition(target, &mut || {
            self.session
                .lock()
                .map_err(|_| "Session unavailable")?
                .append(SessionEventKind::PlanReview {
                    plan: record.plan.clone(),
                    decision: record.decision.clone(),
                    feedback: record.feedback.clone(),
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        state.review = Some(record);
        state.active = false;
        state.pending = None;
        drop(state);
        self.announce(false);
        self.bus
            .emit(UiEvent::PermissionModeChanged { mode: target });
        Ok(())
    }

    fn commit_locked(
        &self,
        state: &mut PlanState,
        active: bool,
    ) -> Result<(), heycode_session::AppendError> {
        {
            let mut session = self
                .session
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            session.append(SessionEventKind::PlanMode { active })?;
            session.flush()?;
        }
        state.active = active;
        state.pending = None;
        Ok(())
    }

    fn announce(&self, active: bool) {
        if active {
            self.bus.emit(UiEvent::PermissionModeChanged {
                mode: crate::ApprovalPolicyKind::Plan,
            });
        }
        self.bus.emit(UiEvent::Info {
            text: if active {
                "plan mode ON — mutations are blocked until the plan is approved".to_owned()
            } else {
                "plan mode OFF".to_owned()
            },
        });
    }
}

struct PlanHookGate(std::sync::Weak<PlanMode>);
impl heycode_hooks::HookExecutionGate for PlanHookGate {
    fn permits_execution(&self) -> bool {
        self.0.upgrade().is_some_and(|plan| !plan.active())
    }
    fn attach_service(&self, service: Arc<heycode_hooks::HookService>) {
        if let Some(plan) = self.0.upgrade() {
            *plan.hooks.lock().unwrap_or_else(|e| e.into_inner()) = Some(service);
        }
    }
}

/// Plan mode reaches past its own `seam/pre_tool` guard here.
///
/// The guard binds tool calls made by THIS agent. A delegated subagent is a
/// separate agent — routinely a separate process — whose tool calls never
/// touch this waterfall, so `task {provider: "claude", prompt: "write it"}`
/// would otherwise mutate the workspace with plan mode on. The registry
/// refuses the delegation instead, before any child exists.
impl crate::DelegationGate for PlanMode {
    fn refuse_unguarded_delegation(&self, provider: &crate::SubagentProviderId) -> Option<String> {
        self.active().then(|| {
            format!(
                "plan mode is active — `{provider}` runs an agent that does not inherit \
                 plan mode's tool guards, so it cannot be delegated to while planning; \
                 use the native subagent to research, or present your plan via \
                 exit_plan_mode",
                provider = provider.as_str()
            )
        })
    }
}

fn has_open_turn(events: &[heycode_session::SessionEvent]) -> bool {
    let mut open = std::collections::BTreeSet::new();
    for event in events {
        match &event.kind {
            SessionEventKind::TurnStart { turn } => {
                open.insert(*turn);
            }
            SessionEventKind::TurnEnd { turn, .. } => {
                open.remove(turn);
            }
            _ => {}
        }
    }
    !open.is_empty()
}

/// The only tools plan mode admits: the ones that observe and never mutate.
///
/// Plan mode is DEFAULT-DENY. It has to be: `ToolSpec`
/// ([`heycode_core::ToolSpec`]) carries no mutation metadata, so the guard cannot
/// ask a tool what it does — it can only recognise the ones it has been told
/// are safe. A deny-list gets this backwards, because every tool composed
/// after the guard shipped (`background_shell`, `background_terminal`,
/// `schedule_create`, `workflow`, `team`, `goal`, every `mcp__*` tool) is then
/// exempt by default, and `background_shell` is a plain alias for the `bash`
/// the guard already denies.
///
/// Membership rules:
/// - `exit_plan_mode` MUST stay here or plan mode becomes unexitable.
/// - `task` is admitted only because admission is not the whole story for it.
///   A NATIVE subagent inherits this exact `seam/pre_tool` waterfall (the
///   child `Agent` is built with the parent's `pre_seam`), so a research child
///   is bound by the same default-deny. A DELEGATED subagent
///   (`subagent-codex`, `subagent-claude`, the worktree providers) is an
///   external agent process that inherits nothing here and answers to the
///   approval policy alone, so [`PlanMode`] additionally registers as the
///   subagent registry's [`DelegationGate`](crate::DelegationGate) and refuses
///   to start one at all while plan mode is on. Admitting `task` without that
///   gate installed would reopen the `background_shell` class of escape.
/// - Work tools edit the session planning board, without changing project files.
///
/// Anything absent — including a tool a third-party plugin adds tomorrow — is
/// denied while plan mode is on.
const READ_ONLY_TOOLS: &[&str] = &[
    "enter_plan_mode",
    "list_mcp_resources",
    "read_mcp_resource",
    "wait_for_mcp_servers",
    "exit_plan_mode",
    "glob",
    "grep",
    "list_jobs",
    "list_tasks",
    "list_agents",
    "load_skill",
    "lsp",
    "lsp_definition",
    "lsp_diagnostics",
    "lsp_references",
    "lsp_servers",
    "read",
    "read_many",
    "schedule_list",
    "task",
    "agent",
    "terminal_list",
    "terminal_read",
    "task_create",
    "task_get",
    "task_list",
    "task_update",
    "tool_search",
    "ask_user_question",
    "ask_user_question_async",
    "understand_image",
    "web_fetch",
    "web_search",
];

/// Whether plan mode admits `name`. See [`READ_ONLY_TOOLS`].
fn is_read_only_tool(name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&name)
}

/// Canonical controls admit only explicitly read-only actions. Unknown and
/// omitted actions remain default-denied, including provider-hosted routes.
fn is_read_only_call(name: &str, args: &serde_json::Value) -> bool {
    match name {
        "agent_control" => matches!(
            args.get("action").and_then(serde_json::Value::as_str),
            Some("list" | "wait")
        ),
        "job_control" => matches!(
            args.get("action").and_then(serde_json::Value::as_str),
            Some("list" | "output")
        ),
        _ => is_read_only_tool(name),
    }
}

/// Guard layer denying mutating tools while plan mode is active.
struct PlanGuard {
    plan: Arc<PlanMode>,
}

#[async_trait]
impl heycode_core::Layer<heycode_tools::PreToolDecision> for PlanGuard {
    async fn handle(
        &self,
        input: &mut heycode_tools::PreToolDecision,
        mut next: heycode_core::Next<'_, heycode_tools::PreToolDecision>,
    ) -> anyhow::Result<()> {
        if let Some(reason) = self
            .plan
            .tool_refusal(input.call.name.as_str(), &input.call.args)
        {
            input.verdict = Verdict::Deny { reason };
            // Deliberate short-circuit: no downstream layer may resurrect this.
            return Ok(());
        }
        next.run(input).await
    }
}

// Provider-executed tools do not cross seam/pre_tool. Apply the same deny-by-default
// rule at the shared provider request boundary (also inherited by native children).
struct PlanProviderGuard {
    plan: Arc<PlanMode>,
}
#[async_trait]
impl heycode_core::Layer<heycode_llm::ProviderRequestDecision> for PlanProviderGuard {
    async fn handle(
        &self,
        input: &mut heycode_llm::ProviderRequestDecision,
        mut next: heycode_core::Next<'_, heycode_llm::ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        next.run(input).await?;
        if self.plan.active()
            && input
                .draft()
                .native_tool_routes
                .iter()
                .any(|route| !is_read_only_tool(route.logical()))
        {
            input.reject(
                heycode_llm::ProviderInterceptionCode::new("plan-mode-mutating-provider-tool")
                    .map_err(|_| anyhow::anyhow!("invalid Plan policy code"))?,
            );
        }
        Ok(())
    }
}

/// The model-facing review gate: propose a plan, then await an explicit human decision.
pub struct ExitPlanTool {
    plan: Arc<PlanMode>,
    approval: Arc<dyn ApprovalPolicy>,
}

#[async_trait]
impl Tool for ExitPlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exit_plan_mode".to_owned(),
            description: "Present your plan for review when in plan mode. The user \
                          approves (you may then implement) or rejects (keep planning). \
                          The plan must be a full detailed Markdown document starting with a '# ' heading: objective, proposed changes, affected areas, implementation steps, assumptions, risks and validation. Review offers Accepted edits, Default permissions, or remain in Plan with feedback."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["plan"],
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The complete plan in markdown, starting with '# '"
                    }
                }
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        if !self.plan.active() {
            return Err(ToolError::new("plan mode is not active"));
        }
        let plan = args
            .get("plan")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`plan` must be a string"))?;
        if !plan.trim_start().starts_with("# ") {
            return Err(ToolError::new(
                "the plan must be markdown starting with a '# ' heading",
            ));
        }
        let _review = tokio::select! {
            biased;
            () = cx.cancellation.cancelled() => return Err(ToolError::new("Plan review cancelled")),
            review = self.plan.review_gate.lock() => review,
        };
        if !self.plan.active() {
            return Err(ToolError::new("plan mode is not active"));
        }
        let mode_cancellation = self
            .plan
            .state
            .lock()
            .map_err(|_| ToolError::new("Plan state unavailable"))?
            .review_cancellation
            .clone();
        let feedback = self
            .plan
            .review()
            .map_or_else(String::new, |review| review.feedback);
        self.plan
            .record_review(plan, "pending", &feedback)
            .map_err(ToolError::new)?;
        let decision = tokio::select! {
            biased;
            () = mode_cancellation.cancelled() => return Ok(serde_json::json!({
                "status": "mode_changed", "note": "The user left Plan mode manually; follow the current permissions."
            })),
            decision = self.approval.review_plan(plan, cx.cancellation.clone()) => decision,
        };
        let decision = if cx.cancellation.is_cancelled() {
            PlanReviewDecision::dismissed()
        } else {
            decision
        };
        let target = match decision {
            PlanReviewDecision::AcceptedEdits => crate::ApprovalPolicyKind::AcceptedEdits,
            PlanReviewDecision::DefaultPermissions => crate::ApprovalPolicyKind::Ask,
            PlanReviewDecision::StayInPlan { feedback } => {
                self.plan
                    .record_review(plan, "stay_in_plan", &feedback)
                    .map_err(ToolError::new)?;
                return Ok(
                    serde_json::json!({ "status": "stay_in_plan", "feedback": feedback, "note": "Remain read-only, revise the full plan and present it again" }),
                );
            }
        };
        self.plan
            .accept(plan, target, self.approval.as_ref())
            .map_err(ToolError::new)?;
        Ok(
            serde_json::json!({ "status": "approved", "permissions": target.as_str(), "note": "Plan accepted; implementation may continue under the selected policy" }),
        )
    }
}

struct PlanCommand {
    plan: Arc<PlanMode>,
    approval: Arc<dyn ApprovalPolicy>,
    descriptor: CommandDescriptor,
}

#[async_trait]
impl Command for PlanCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let arg = args.trim();
        match arg {
            "" | "on" => {
                self.plan.set(true).await?;
                // The mode change is otherwise only visible in the footer,
                // which says nothing about which command changed it.
                agent.ui().emit(crate::UiEvent::Info {
                    text: "Enabled plan mode".to_owned(),
                });
            }
            "off" => {
                self.plan
                    .leave_by_user(self.approval.as_ref())
                    .map_err(anyhow::Error::msg)?;
                agent.ui().emit(crate::UiEvent::Info {
                    text: "Disabled plan mode".to_owned(),
                });
            }
            "review" => {
                let review = self.plan.review().ok_or_else(|| {
                    anyhow::anyhow!("No saved plan to review; ask the agent to present a full plan")
                })?;
                ExitPlanTool {
                    plan: self.plan.clone(),
                    approval: self.approval.clone(),
                }
                .run(
                    serde_json::json!({ "plan": review.plan }),
                    &ToolCtx {
                        cwd: agent.cwd().to_path_buf(),
                        cancellation: tokio_util::sync::CancellationToken::new(),
                    },
                )
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            }
            note => {
                self.plan.set(true).await?;
                agent.send(note).await?;
            }
        }
        Ok(())
    }
}

/// Mount plan mode: service, guard, prompt section, review tool, command.
///
/// Injects: `session`, `tools`, `seam/pre_tool`, `prompt`, `approval`,
/// `commands`.
pub fn plan_plugin() -> Box<dyn Plugin> {
    struct PlanPlugin;
    impl Plugin for PlanPlugin {
        fn name(&self) -> &'static str {
            "plan"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "plan",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Tool,
                    heycode_core::PluginContributionKind::PromptSection,
                    heycode_core::PluginContributionKind::Command,
                    heycode_core::PluginContributionKind::Waterfall,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                (heycode_core::ContributionKind::Tool, "enter_plan_mode"),
                (heycode_core::ContributionKind::Tool, "exit_plan_mode"),
                (heycode_core::ContributionKind::PromptSection, "plan-mode"),
                (heycode_core::ContributionKind::Command, "plan"),
                (
                    heycode_core::ContributionKind::InterceptionLayer,
                    "seam/pre_tool:plan-guard",
                ),
            ]
            .into_iter()
            .map(|(kind, name)| heycode_core::PluginContributionSpec::new(kind, name))
            .collect()
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_PLAN,
                heycode_hooks::SERVICE_HOOK_EXECUTION_GATE,
            ]
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_session::SERVICE_SESSION,
                heycode_tools::SERVICE_TOOLS,
                heycode_tools::SEAM_PRE_TOOL,
                heycode_prompt::SERVICE_PROMPT,
                crate::SERVICE_APPROVAL,
                crate::SERVICE_COMMANDS,
            ]
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> CoreResult<()> {
            let session = ctx
                .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session missing"))?;
            let tools = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tools missing"))?;
            let seam = ctx
                .get::<Waterfall<heycode_tools::PreToolDecision>>(heycode_tools::SEAM_PRE_TOOL)
                .ok_or_else(|| CoreError::other("pre-tool seam missing"))?;
            let prompt = ctx
                .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
                .ok_or_else(|| CoreError::other("prompt missing"))?;
            let approval = ctx
                .get::<crate::plugin::ApprovalHandle>(crate::SERVICE_APPROVAL)
                .ok_or_else(|| CoreError::other("approval missing"))?;
            let commands = ctx
                .get::<crate::CommandRegistry>(crate::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("commands missing"))?;

            // Resume-safe fold BEFORE any UI exists.
            let plan = Arc::new(PlanMode::from_log(&session, ctx.events.clone()));
            let hook_gate: Arc<dyn heycode_hooks::HookExecutionGate> =
                Arc::new(PlanHookGate(Arc::downgrade(&plan)));
            if let Some(hooks) = ctx.get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
            {
                hooks
                    .attach_execution_gate(ctx, hook_gate.clone())
                    .map_err(CoreError::other)?;
                hook_gate.attach_service(hooks);
            }
            ctx.provide(
                heycode_hooks::SERVICE_HOOK_EXECUTION_GATE,
                "plan",
                heycode_hooks::HookExecutionGateHandle(hook_gate),
            )?;

            if let Some(terminals) =
                ctx.get::<heycode_exec::TerminalService>(heycode_exec::SERVICE_TERMINAL)
            {
                *plan.terminal.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((*terminals).clone());
            }
            if let Some(review) = plan.review() {
                let target = match review.decision.as_str() {
                    "accepted_edits" => Some(crate::ApprovalPolicyKind::AcceptedEdits),
                    "default" => Some(crate::ApprovalPolicyKind::Ask),
                    _ => None,
                };
                if let Some(target) = target {
                    // Restoring to an unavailable policy fails before tools can execute.
                    approval
                        .0
                        .commit_plan_transition(target, &mut || Ok(()))
                        .map_err(CoreError::other)?;
                }
            }
            if let Some(switch) =
                ctx.get::<crate::ApprovalSwitchHandle>(crate::SERVICE_APPROVAL_SWITCH)
            {
                switch.0.attach_plan(&plan);
            }
            if let Some(interception) = ctx.get::<heycode_llm::ProviderInterception>(
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
            ) {
                interception.register_request(ctx, PlanProviderGuard { plan: plan.clone() });
            }
            seam.push_effect(ctx, PlanGuard { plan: plan.clone() });

            // The seam guard cannot reach a child agent that never runs
            // through it, so plan mode also gates delegation itself. Whichever
            // of `plan`/`subagent` applies second owns this wiring; the
            // shipped composition applies `plan` last, so this is the live
            // path and `subagent_plugin` covers the reverse order.
            if let Some(subagents) = ctx.get::<crate::SubagentRegistry>(crate::SERVICE_SUBAGENTS) {
                subagents
                    .attach_delegation_gate(plan.clone())
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }

            let prompt_plan = plan.clone();
            prompt
                .section_shared("plan-mode", 40, move |_cx| {
                    if prompt_plan.active() {
                        let mut instruction = "PLAN MODE ACTIVE: You MUST NOT modify files or run mutating shell commands. Research, read, and reason; then call exit_plan_mode with the full detailed Markdown plan: objective, proposed changes, affected areas, implementation steps, assumptions, risks and validation. Only explicit human plan acceptance selects implementation permissions.".to_owned();
                        if let Some(review) = prompt_plan.review() {
                            instruction.push_str(&format!("\nSaved review state: {}\nReviewer feedback: {}\nPrevious full proposal:\n{}", review.decision, review.feedback, review.plan));
                        }
                        instruction
                    } else {
                        String::new()
                    }
                })
                .map_err(CoreError::other)?;

            let entry_registration = tools
                .register_owned(Arc::new(crate::EnterPlanModeTool::new(plan.clone())))
                .map_err(|e| CoreError::other(e.to_string()))?;
            ctx.effect(move || drop(entry_registration));
            let tool_registration = tools
                .register_owned(Arc::new(ExitPlanTool {
                    plan: plan.clone(),
                    approval: approval.0.clone(),
                }))
                .map_err(|e| CoreError::other(e.to_string()))?;
            ctx.effect(move || drop(tool_registration));
            let intent = CommandArgument::optional(
                "intent",
                "Use `on`, `review` to reopen the saved full plan, or a planning note",
            )
            .map_err(|error| CoreError::other(error.to_string()))?
            .variadic();
            let source = CommandSource::from_plugin("plan")
                .map_err(|error| CoreError::other(error.to_string()))?;
            let descriptor = CommandDescriptor::new(
                "plan",
                "Show or change plan mode",
                vec![intent],
                CommandTiming::Queued,
                source,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(PlanCommand {
                        plan: plan.clone(),
                        approval: approval.0.clone(),
                        descriptor,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.provide(crate::SERVICE_PLAN, "plan", PlanHandle(plan))
        }
    }
    Box::new(PlanPlugin)
}

/// Newtype so `Arc<PlanMode>` rides the type-erased service map.
pub struct PlanHandle(pub Arc<PlanMode>);

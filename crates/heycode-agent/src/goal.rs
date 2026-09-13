//! O10 — revisioned same-session goals and bounded continuation rounds.

use std::sync::{Arc, Mutex};

use heycode_core::{CoreError, CoreResult, Plugin, ToolSpec};
use heycode_session::{
    GoalBlockReason, GoalChange, GoalId, GoalOperation, GoalPhase, GoalRef, GoalSnapshot, GoalView,
    InboxDelivery, InboxSource, Session, SessionEventKind, project_goal,
};
use heycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry};

use crate::{
    Agent, AgentIdle, Command, CommandArgument, CommandDescriptor, CommandRegistry, CommandSource,
    CommandTiming, UiEvent,
};

/// Whether this process may schedule another round for the durable active goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalActivation {
    /// Explicit create/resume authority remains live.
    Armed,
    /// Resume/fork/start never silently restores continuation authority.
    Disarmed,
}

/// Deployment-owned round and consecutive-wake defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GoalPolicy {
    default_max_rounds: u32,
    max_wakes_per_activation: u32,
}

impl GoalPolicy {
    /// Validate goal defaults.
    ///
    /// # Errors
    /// Zero disables either bound; explicit bounds must not exceed one million.
    pub fn new(default_max_rounds: u32, max_wakes_per_activation: u32) -> Result<Self, GoalError> {
        if default_max_rounds > 1_000_000 || max_wakes_per_activation > 1_000_000 {
            return Err(GoalError::new(GoalErrorCode::InvalidRounds));
        }
        Ok(Self {
            default_max_rounds,
            max_wakes_per_activation,
        })
    }
}

/// Stable goal-domain failure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalErrorCode {
    /// No current goal exists.
    NotFound,
    /// A non-complete goal already owns the session.
    AlreadyExists,
    /// Expected id/revision no longer names current state.
    StaleRevision,
    /// Objective text is invalid.
    InvalidObjective,
    /// Round cap is invalid or already exhausted.
    InvalidRounds,
    /// Requested lifecycle edge is not allowed.
    InvalidTransition,
    /// Durable replay or append/flush failed.
    Persistence,
    /// Runtime state is poisoned or disposed.
    Unavailable,
}

impl GoalErrorCode {
    /// Stable machine identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "goal_not_found",
            Self::AlreadyExists => "goal_already_exists",
            Self::StaleRevision => "goal_stale_revision",
            Self::InvalidObjective => "goal_invalid_objective",
            Self::InvalidRounds => "goal_invalid_rounds",
            Self::InvalidTransition => "goal_invalid_transition",
            Self::Persistence => "goal_persistence_failed",
            Self::Unavailable => "goal_unavailable",
        }
    }
}

/// Body-free goal error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{code}")]
pub struct GoalError {
    code: &'static str,
    class: GoalErrorCode,
}

impl GoalError {
    const fn new(class: GoalErrorCode) -> Self {
        Self {
            code: class.as_str(),
            class,
        }
    }

    /// Stable failure class.
    #[must_use]
    pub const fn code(&self) -> GoalErrorCode {
        self.class
    }
}

/// Detached current goal view with process-local activation evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalLiveView {
    durable: GoalView,
    activation: GoalActivation,
}

impl GoalLiveView {
    /// Latest durable full snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &GoalSnapshot {
        self.durable.snapshot()
    }

    /// Highest admitted continuation round.
    #[must_use]
    pub const fn rounds_started(&self) -> u32 {
        self.durable.rounds_started()
    }

    /// Process-local continuation authority.
    #[must_use]
    pub const fn activation(&self) -> GoalActivation {
        self.activation
    }
}

struct GoalRuntimeState {
    armed_ref: Option<GoalRef>,
    wakes_remaining: u32,
    driving: bool,
    closed: bool,
}

/// Revisioned durable goal service and same-session round driver.
pub struct GoalService {
    session: Arc<std::sync::Mutex<Session>>,
    agent: Arc<Agent>,
    policy: GoalPolicy,
    runtime: Mutex<GoalRuntimeState>,
}

impl std::fmt::Debug for GoalService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let activation = self
            .runtime
            .lock()
            .map(|state| state.armed_ref.is_some())
            .unwrap_or(false);
        formatter
            .debug_struct("GoalService")
            .field("armed", &activation)
            .finish_non_exhaustive()
    }
}

impl GoalService {
    /// Bind one live Agent. Durable state is replayed immediately but activation
    /// always starts disarmed, including resume and fork.
    ///
    /// # Errors
    /// A malformed goal/inbox history fails before service publication.
    pub fn from_session(
        session: Arc<std::sync::Mutex<Session>>,
        agent: Arc<Agent>,
        policy: GoalPolicy,
    ) -> Result<Self, GoalError> {
        {
            let session = session
                .lock()
                .map_err(|_| GoalError::new(GoalErrorCode::Unavailable))?;
            project_goal(session.events())
                .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?;
        }
        Ok(Self {
            session,
            agent,
            policy,
            runtime: Mutex::new(GoalRuntimeState {
                armed_ref: None,
                wakes_remaining: 0,
                driving: false,
                closed: false,
            }),
        })
    }

    /// Current detached state.
    ///
    /// # Errors
    /// Malformed durable history or poisoned runtime state.
    pub fn get(&self) -> Result<Option<GoalLiveView>, GoalError> {
        let goal = self.project()?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| GoalError::new(GoalErrorCode::Unavailable))?;
        sync_runtime(&mut runtime, goal.current());
        Ok(goal.current().cloned().map(|durable| GoalLiveView {
            activation: activation_for(&runtime, durable.snapshot()),
            durable,
        }))
    }

    /// Create and arm one active goal at revision one.
    ///
    /// # Errors
    /// Invalid definition, conflicting current goal, or persistence failure.
    pub fn create(
        &self,
        objective: impl Into<String>,
        max_rounds: Option<u32>,
    ) -> Result<GoalLiveView, GoalError> {
        let objective = objective.into();
        let max_rounds = max_rounds.unwrap_or(self.policy.default_max_rounds);
        let mut runtime = self.lock_runtime()?;
        let current = self.project()?;
        if current
            .current()
            .is_some_and(|goal| goal.snapshot().phase() != GoalPhase::Complete)
        {
            return Err(GoalError::new(GoalErrorCode::AlreadyExists));
        }
        let id = GoalId::generate();
        let snapshot = GoalSnapshot::new(id, 1, objective, GoalPhase::Active, None, max_rounds)
            .map_err(|_| classify_definition(max_rounds))?;
        let now = now_ms();
        self.append_change(GoalChange::snapshot(
            GoalOperation::Create,
            snapshot.clone(),
            0,
            now,
            now,
        ))?;
        runtime.armed_ref = Some(snapshot.reference());
        runtime.wakes_remaining = self.policy.max_wakes_per_activation;
        drop(runtime);
        self.drive_round()?;
        self.required_view()
    }

    /// Edit objective and/or max rounds under an exact CAS reference.
    ///
    /// # Errors
    /// Stale reference, empty edit, exhausted/invalid cap, or persistence failure.
    pub fn edit(
        &self,
        expected: &GoalRef,
        objective: Option<String>,
        max_rounds: Option<u32>,
    ) -> Result<GoalLiveView, GoalError> {
        if objective.is_none() && max_rounds.is_none() {
            return Err(GoalError::new(GoalErrorCode::InvalidObjective));
        }
        self.update(
            expected,
            GoalOperation::Edit,
            |current| {
                let next_rounds = max_rounds.unwrap_or(current.snapshot().max_rounds());
                if next_rounds < current.rounds_started() {
                    return Err(GoalError::new(GoalErrorCode::InvalidRounds));
                }
                GoalSnapshot::new(
                    current.snapshot().id().clone(),
                    current.snapshot().revision().saturating_add(1),
                    objective.unwrap_or_else(|| current.snapshot().objective().to_owned()),
                    current.snapshot().phase(),
                    current.snapshot().blocked_reason().cloned(),
                    next_rounds,
                )
                .map_err(|_| classify_definition(next_rounds))
            },
            ActivationEffect::Retain,
        )
    }

    /// Pause one active goal and disarm continuation.
    ///
    /// # Errors
    /// Stale reference, invalid transition, or persistence failure.
    pub fn pause(&self, expected: &GoalRef) -> Result<GoalLiveView, GoalError> {
        self.transition(expected, GoalOperation::Pause, GoalPhase::Paused, None)
    }

    /// Resume a paused/blocked goal, or explicitly rearm a disarmed active goal.
    ///
    /// # Errors
    /// Stale reference, exhausted cap, invalid transition, or persistence failure.
    pub fn resume(&self, expected: &GoalRef) -> Result<GoalLiveView, GoalError> {
        self.update(
            expected,
            GoalOperation::Resume,
            |current| {
                if current.snapshot().phase() == GoalPhase::Complete
                    || (current.snapshot().max_rounds() != 0
                        && current.rounds_started() >= current.snapshot().max_rounds())
                {
                    return Err(GoalError::new(GoalErrorCode::InvalidTransition));
                }
                clone_with_phase(current, GoalPhase::Active, None)
            },
            ActivationEffect::Arm,
        )
    }

    /// Mark an unfinished goal complete and disarm it.
    ///
    /// # Errors
    /// Stale reference, invalid transition, or persistence failure.
    pub fn complete(&self, expected: &GoalRef) -> Result<GoalLiveView, GoalError> {
        self.transition(expected, GoalOperation::Complete, GoalPhase::Complete, None)
    }

    /// Mark an active goal blocked and disarm it.
    ///
    /// # Errors
    /// Stale reference, invalid reason/transition, or persistence failure.
    pub fn block(
        &self,
        expected: &GoalRef,
        reason: GoalBlockReason,
    ) -> Result<GoalLiveView, GoalError> {
        self.transition(
            expected,
            GoalOperation::Block,
            GoalPhase::Blocked,
            Some(reason),
        )
    }

    /// Clear the current goal with a revisioned tombstone.
    ///
    /// # Errors
    /// Stale reference or persistence failure.
    pub fn clear(&self, expected: &GoalRef) -> Result<GoalRef, GoalError> {
        let mut runtime = self.lock_runtime()?;
        let current = self.required_current(expected)?;
        self.cancel_pending_rounds(current.snapshot().id())?;
        let cleared = GoalRef::new(
            current.snapshot().id().clone(),
            current.snapshot().revision().saturating_add(1),
        )
        .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?;
        self.append_change(GoalChange::clear(
            cleared.clone(),
            now_ms().max(current.updated_at_ms()),
        ))?;
        runtime.armed_ref = None;
        runtime.wakes_remaining = 0;
        Ok(cleared)
    }

    /// Remove only process-local continuation authority.
    pub fn disarm(&self) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.armed_ref = None;
            runtime.wakes_remaining = 0;
        }
    }

    /// Stop new rounds during plugin teardown.
    pub fn close(&self) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.closed = true;
            runtime.armed_ref = None;
            runtime.wakes_remaining = 0;
        }
    }

    fn transition(
        &self,
        expected: &GoalRef,
        action: GoalOperation,
        phase: GoalPhase,
        reason: Option<GoalBlockReason>,
    ) -> Result<GoalLiveView, GoalError> {
        self.update(
            expected,
            action,
            |current| clone_with_phase(current, phase, reason),
            ActivationEffect::Disarm,
        )
    }

    fn update<F>(
        &self,
        expected: &GoalRef,
        action: GoalOperation,
        build: F,
        activation: ActivationEffect,
    ) -> Result<GoalLiveView, GoalError>
    where
        F: FnOnce(&GoalView) -> Result<GoalSnapshot, GoalError>,
    {
        let mut runtime = self.lock_runtime()?;
        let current = self.required_current(expected)?;
        let was_armed = activation_for(&runtime, current.snapshot()) == GoalActivation::Armed;
        if action == GoalOperation::Resume
            && was_armed
            && current.snapshot().phase() == GoalPhase::Active
        {
            return Err(GoalError::new(GoalErrorCode::InvalidTransition));
        }
        self.cancel_pending_rounds(current.snapshot().id())?;
        let next = build(&current)?;
        let updated = now_ms().max(current.updated_at_ms());
        self.append_change(GoalChange::snapshot(
            action,
            next.clone(),
            current.rounds_started(),
            current.created_at_ms(),
            updated,
        ))?;
        match activation {
            ActivationEffect::Arm => {
                runtime.armed_ref = Some(next.reference());
                runtime.wakes_remaining = self.policy.max_wakes_per_activation;
            }
            ActivationEffect::Disarm => {
                runtime.armed_ref = None;
                runtime.wakes_remaining = 0;
            }
            ActivationEffect::Retain if was_armed => {
                runtime.armed_ref = Some(next.reference());
            }
            ActivationEffect::Retain => {}
        }
        drop(runtime);
        self.drive_round()?;
        self.required_view()
    }

    fn required_current(&self, expected: &GoalRef) -> Result<GoalView, GoalError> {
        let projected = self.project()?;
        let current = projected
            .current()
            .cloned()
            .ok_or_else(|| GoalError::new(GoalErrorCode::NotFound))?;
        if current.snapshot().id() != expected.id()
            || current.snapshot().revision() != expected.revision()
        {
            return Err(GoalError::new(GoalErrorCode::StaleRevision));
        }
        Ok(current)
    }

    fn required_view(&self) -> Result<GoalLiveView, GoalError> {
        self.get()?
            .ok_or_else(|| GoalError::new(GoalErrorCode::NotFound))
    }

    fn append_change(&self, change: GoalChange) -> Result<(), GoalError> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| GoalError::new(GoalErrorCode::Unavailable))?;
        let mut candidate = session.events().to_vec();
        candidate.push(heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: u64::try_from(candidate.len())
                .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?,
            time_ms: now_ms(),
            kind: SessionEventKind::GoalChange {
                change: Box::new(change.clone()),
            },
        });
        project_goal(&candidate).map_err(|_| GoalError::new(GoalErrorCode::InvalidTransition))?;
        session
            .append(SessionEventKind::GoalChange {
                change: Box::new(change),
            })
            .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?;
        session
            .flush()
            .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?;
        Ok(())
    }

    fn cancel_pending_rounds(&self, goal_id: &GoalId) -> Result<(), GoalError> {
        loop {
            let pending = {
                let session = self
                    .session
                    .lock()
                    .map_err(|_| GoalError::new(GoalErrorCode::Unavailable))?;
                session
                    .inbox()
                    .next_turn()
                    .iter()
                    .chain(session.inbox().next_step())
                    .find_map(|message| match message.source() {
                        InboxSource::Goal { goal_id: id, .. } if id == goal_id => {
                            Some(message.id().clone())
                        }
                        _ => None,
                    })
            };
            let Some(id) = pending else { return Ok(()) };
            self.agent
                .cancel_inbox(&id)
                .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?;
        }
    }

    fn project(&self) -> Result<heycode_session::GoalProjection, GoalError> {
        let session = self
            .session
            .lock()
            .map_err(|_| GoalError::new(GoalErrorCode::Unavailable))?;
        project_goal(session.events()).map_err(|_| GoalError::new(GoalErrorCode::Persistence))
    }

    fn lock_runtime(&self) -> Result<std::sync::MutexGuard<'_, GoalRuntimeState>, GoalError> {
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| GoalError::new(GoalErrorCode::Unavailable))?;
        if runtime.closed {
            Err(GoalError::new(GoalErrorCode::Unavailable))
        } else {
            Ok(runtime)
        }
    }

    fn drive_round(&self) -> Result<(), GoalError> {
        let (snapshot, round) = {
            let mut runtime = self.lock_runtime()?;
            if runtime.driving || self.agent.token().is_turn_active() {
                return Ok(());
            }
            let projected = self.project()?;
            sync_runtime(&mut runtime, projected.current());
            let Some(current) = projected.current() else {
                return Ok(());
            };
            if activation_for(&runtime, current.snapshot()) != GoalActivation::Armed
                || current.snapshot().phase() != GoalPhase::Active
                || (current.snapshot().max_rounds() != 0
                    && current.rounds_started() >= current.snapshot().max_rounds())
            {
                return Ok(());
            }
            if self.policy.max_wakes_per_activation != 0 && runtime.wakes_remaining == 0 {
                runtime.armed_ref = None;
                return Ok(());
            }
            {
                let session = self
                    .session
                    .lock()
                    .map_err(|_| GoalError::new(GoalErrorCode::Unavailable))?;
                if !session.inbox().next_turn().is_empty()
                    || !session.inbox().next_step().is_empty()
                {
                    return Ok(());
                }
                session
                    .flush()
                    .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?;
            }
            runtime.driving = true;
            (
                current.snapshot().clone(),
                current.rounds_started().saturating_add(1),
            )
        };

        let objective = serde_json::to_string(snapshot.objective())
            .map_err(|_| GoalError::new(GoalErrorCode::Persistence))?;
        let text = format!(
            "<goal_round>\nobjective_json: {objective}\nround: {round}/{}\nContinue from durable session state. Complete or block the goal only with evidence.\n</goal_round>",
            round_limit_label(snapshot.max_rounds())
        );
        let queued = self.agent.submit_inbox_with_source(
            InboxDelivery::FollowUp,
            text,
            InboxSource::Goal {
                goal_id: snapshot.id().clone(),
                revision: snapshot.revision(),
                round,
            },
        );
        let mut runtime = self.lock_runtime()?;
        runtime.driving = false;
        match queued {
            Ok(_) => {
                runtime.wakes_remaining = runtime.wakes_remaining.saturating_sub(1);
                Ok(())
            }
            Err(_) => {
                runtime.armed_ref = None;
                runtime.wakes_remaining = 0;
                Err(GoalError::new(GoalErrorCode::Persistence))
            }
        }
    }

    fn on_agent_idle(&self) {
        let aborted = self.session.lock().ok().and_then(|session| {
            session
                .events()
                .iter()
                .rev()
                .find_map(|event| match event.kind {
                    SessionEventKind::TurnEnd { reason, .. } => Some(reason),
                    _ => None,
                })
        }) == Some(heycode_session::TurnEndReason::Aborted);
        if aborted || self.drive_round().is_err() {
            self.disarm();
        }
    }
}

#[derive(Clone, Copy)]
enum ActivationEffect {
    Arm,
    Disarm,
    Retain,
}

fn activation_for(runtime: &GoalRuntimeState, snapshot: &GoalSnapshot) -> GoalActivation {
    if runtime.armed_ref.as_ref() == Some(&snapshot.reference()) {
        GoalActivation::Armed
    } else {
        GoalActivation::Disarmed
    }
}

fn sync_runtime(runtime: &mut GoalRuntimeState, current: Option<&GoalView>) {
    if runtime
        .armed_ref
        .as_ref()
        .is_some_and(|armed| current.is_none_or(|goal| goal.snapshot().reference() != *armed))
    {
        runtime.armed_ref = None;
        runtime.wakes_remaining = 0;
    }
}

fn clone_with_phase(
    current: &GoalView,
    phase: GoalPhase,
    reason: Option<GoalBlockReason>,
) -> Result<GoalSnapshot, GoalError> {
    GoalSnapshot::new(
        current.snapshot().id().clone(),
        current.snapshot().revision().saturating_add(1),
        current.snapshot().objective(),
        phase,
        reason,
        current.snapshot().max_rounds(),
    )
    .map_err(|_| GoalError::new(GoalErrorCode::InvalidTransition))
}

fn classify_definition(max_rounds: u32) -> GoalError {
    if max_rounds > 1_000_000 {
        GoalError::new(GoalErrorCode::InvalidRounds)
    } else {
        GoalError::new(GoalErrorCode::InvalidObjective)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

struct GoalTool {
    goals: Arc<GoalService>,
}

struct GoalCommand {
    goals: Arc<GoalService>,
    descriptor: CommandDescriptor,
}

#[async_trait::async_trait]
impl Command for GoalCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let args = args.trim();
        let text = if args.is_empty() {
            self.goals
                .get()?
                .map_or_else(|| "no current goal".to_owned(), render_goal_text)
        } else {
            let (action, rest) = args
                .split_once(char::is_whitespace)
                .map_or((args, ""), |pair| pair);
            match action {
                "create" => {
                    let view = self.goals.create(rest.trim(), None)?;
                    render_goal_text(view)
                }
                "edit" => {
                    let current = required_command_goal(&self.goals)?;
                    let view = self.goals.edit(
                        &current.snapshot().reference(),
                        Some(rest.trim().to_owned()),
                        None,
                    )?;
                    render_goal_text(view)
                }
                "pause" if rest.trim().is_empty() => {
                    let current = required_command_goal(&self.goals)?;
                    render_goal_text(self.goals.pause(&current.snapshot().reference())?)
                }
                "resume" => {
                    let current = required_command_goal(&self.goals)?;
                    let view = self.goals.resume(&current.snapshot().reference())?;
                    let message = rest.trim();
                    if !message.is_empty() {
                        agent.send(message).await?;
                    }
                    render_goal_text(view)
                }
                "complete" if rest.trim().is_empty() => {
                    let current = required_command_goal(&self.goals)?;
                    render_goal_text(self.goals.complete(&current.snapshot().reference())?)
                }
                "clear" if rest.trim().is_empty() => {
                    let current = required_command_goal(&self.goals)?;
                    let cleared = self.goals.clear(&current.snapshot().reference())?;
                    format!(
                        "cleared goal {} revision {}",
                        cleared.id(),
                        cleared.revision()
                    )
                }
                "block" => {
                    let (code, message) = rest
                        .trim()
                        .split_once(char::is_whitespace)
                        .ok_or_else(|| anyhow::anyhow!("usage: /goal block <code> <message>"))?;
                    let reason = GoalBlockReason::new(code, message)?;
                    let current = required_command_goal(&self.goals)?;
                    render_goal_text(self.goals.block(&current.snapshot().reference(), reason)?)
                }
                _ => {
                    // A plain suffix is the convenient create form when no
                    // current goal exists; explicit verbs remain unambiguous.
                    if self.goals.get()?.is_none() {
                        render_goal_text(self.goals.create(args, None)?)
                    } else {
                        anyhow::bail!(
                            "usage: /goal [create|edit|pause|resume|complete|block|clear]"
                        );
                    }
                }
            }
        };
        agent.ui().emit(UiEvent::Info { text });
        Ok(())
    }
}

fn required_command_goal(goals: &GoalService) -> anyhow::Result<GoalLiveView> {
    goals
        .get()?
        .ok_or_else(|| anyhow::anyhow!("no current goal"))
}

fn render_goal_text(view: GoalLiveView) -> String {
    format!(
        "goal {} revision {} · {} · round {}/{} · {}\n{}",
        view.snapshot().id(),
        view.snapshot().revision(),
        match view.snapshot().phase() {
            GoalPhase::Active => "active",
            GoalPhase::Paused => "paused",
            GoalPhase::Blocked => "blocked",
            GoalPhase::Complete => "complete",
        },
        view.rounds_started(),
        round_limit_label(view.snapshot().max_rounds()),
        match view.activation() {
            GoalActivation::Armed => "armed",
            GoalActivation::Disarmed => "disarmed",
        },
        view.snapshot().objective()
    )
}

#[async_trait::async_trait]
impl Tool for GoalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "goal".to_owned(),
            description: "Read or mutate the current durable goal. Every mutation except create requires the exact current goal_id and revision.".to_owned(),
            parameters: serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "required":["action"],
                "properties":{
                    "action":{"enum":["get","create","edit","pause","resume","complete","block","clear"]},
                    "goal_id":{"type":"string"},
                    "revision":{"type":"integer","minimum":1},
                    "objective":{"type":"string"},
                    "max_rounds":{"type":"integer","minimum":0},
                    "block_code":{"type":"string"},
                    "block_message":{"type":"string"}
                }
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        _cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let action = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`action` must be a string"))?;
        let value = match action {
            "get" => self.goals.get().map(|view| view.map(render_goal)),
            "create" => {
                let objective = string_arg(&args, "objective")?;
                let max_rounds = optional_u32(&args, "max_rounds")?;
                self.goals
                    .create(objective, max_rounds)
                    .map(|view| Some(render_goal(view)))
            }
            "edit" => {
                let reference = goal_ref(&args)?;
                let objective = args
                    .get("objective")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                let max_rounds = optional_u32(&args, "max_rounds")?;
                self.goals
                    .edit(&reference, objective, max_rounds)
                    .map(|view| Some(render_goal(view)))
            }
            "pause" => self
                .goals
                .pause(&goal_ref(&args)?)
                .map(|view| Some(render_goal(view))),
            "resume" => self
                .goals
                .resume(&goal_ref(&args)?)
                .map(|view| Some(render_goal(view))),
            "complete" => self
                .goals
                .complete(&goal_ref(&args)?)
                .map(|view| Some(render_goal(view))),
            "block" => {
                let reason = GoalBlockReason::new(
                    string_arg(&args, "block_code")?,
                    string_arg(&args, "block_message")?,
                )
                .map_err(|_| ToolError::new("goal blocked reason is invalid"))?;
                self.goals
                    .block(&goal_ref(&args)?, reason)
                    .map(|view| Some(render_goal(view)))
            }
            "clear" => self.goals.clear(&goal_ref(&args)?).map(|reference| {
                Some(serde_json::json!({
                    "cleared":true,
                    "goal_id":reference.id().as_str(),
                    "revision":reference.revision()
                }))
            }),
            _ => return Err(ToolError::new("unknown goal action")),
        }
        .map_err(|error| ToolError::new(error.code().as_str()))?;
        Ok(value.unwrap_or(serde_json::Value::Null))
    }
}

fn goal_ref(args: &serde_json::Value) -> Result<GoalRef, ToolError> {
    let id = GoalId::new(string_arg(args, "goal_id")?)
        .map_err(|_| ToolError::new("`goal_id` is invalid"))?;
    let revision = args
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| ToolError::new("`revision` must be a positive integer"))?;
    GoalRef::new(id, revision).map_err(|_| ToolError::new("goal ref is invalid"))
}

fn string_arg<'a>(args: &'a serde_json::Value, name: &str) -> Result<&'a str, ToolError> {
    args.get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ToolError::new(format!("`{name}` must be a string")))
}

fn optional_u32(args: &serde_json::Value, name: &str) -> Result<Option<u32>, ToolError> {
    args.get(name)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| ToolError::new(format!("`{name}` must be a positive integer")))
        })
        .transpose()
}

fn render_goal(view: GoalLiveView) -> serde_json::Value {
    serde_json::json!({
        "goal_id":view.snapshot().id().as_str(),
        "revision":view.snapshot().revision(),
        "objective":view.snapshot().objective(),
        "phase":match view.snapshot().phase() {
            GoalPhase::Active => "active",
            GoalPhase::Paused => "paused",
            GoalPhase::Blocked => "blocked",
            GoalPhase::Complete => "complete",
        },
        "rounds_started":view.rounds_started(),
        "max_rounds":view.snapshot().max_rounds(),
        "activation":match view.activation() {
            GoalActivation::Armed => "armed",
            GoalActivation::Disarmed => "disarmed",
        },
        "blocked_reason":view.snapshot().blocked_reason().map(|reason| serde_json::json!({
            "code":reason.code(),
            "message":reason.message()
        }))
    })
}

/// Publish the durable goal service, round driver, and model-facing `goal` tool.
#[must_use]
pub fn goal_plugin(policy: GoalPolicy) -> Box<dyn Plugin> {
    struct GoalPlugin(GoalPolicy);

    impl Plugin for GoalPlugin {
        fn name(&self) -> &'static str {
            "goals"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "goals",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Tool,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    "goal",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "goal",
                ),
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_AGENT,
                heycode_session::SERVICE_SESSION,
                heycode_tools::SERVICE_TOOLS,
                crate::SERVICE_COMMANDS,
            ]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_GOALS]
        }

        fn apply(&self, ctx: &mut heycode_core::Context) -> CoreResult<()> {
            let agent = ctx
                .get::<Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| CoreError::other("agent service missing"))?;
            let session = ctx
                .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session service missing"))?;
            let tools = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tool registry missing"))?;
            let commands = ctx
                .get::<CommandRegistry>(crate::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("command registry missing"))?;
            let service = GoalService::from_session(session, agent.clone(), self.0)
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.provide(crate::SERVICE_GOALS, "goals", service)?;
            let service = ctx
                .get::<GoalService>(crate::SERVICE_GOALS)
                .ok_or_else(|| CoreError::other("goal service missing"))?;
            let disposable = service.clone();
            ctx.effect(move || disposable.close());
            let registration = tools
                .register_owned(Arc::new(GoalTool {
                    goals: service.clone(),
                }))
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.effect(move || drop(registration));
            let argument = CommandArgument::optional(
                "operation",
                "Goal verb and values; `resume` may include a model-visible message",
            )
            .map_err(|error| CoreError::other(error.to_string()))?
            .variadic();
            let descriptor = CommandDescriptor::new(
                "goal",
                "Create, inspect, edit, pause, resume, complete, block, or clear the durable goal",
                vec![argument],
                CommandTiming::Queued,
                CommandSource::from_plugin("goals")
                    .map_err(|error| CoreError::other(error.to_string()))?,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(GoalCommand {
                        goals: service.clone(),
                        descriptor,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let listener = service.clone();
            agent.ui().on_effect::<AgentIdle>(ctx, move |_| {
                listener.on_agent_idle();
            });
            Ok(())
        }
    }

    Box::new(GoalPlugin(policy))
}

fn round_limit_label(limit: u32) -> String {
    if limit == 0 {
        "unlimited".into()
    } else {
        limit.to_string()
    }
}

//! A09 explicit, restart-safe turn-loop budgets.
//!
//! The layer owns no mutable counter. Before every step it re-projects the
//! current turn from durable session events: completed steps, reported token
//! usage, dispatched tool calls and the turn-start timestamp. Recomposition or
//! process restart therefore observes the same consumed budget.
//!
//! Exact stop-code durability is bounded by the existing session vocabulary.
//! Token exhaustion maps to `turn/end max_tokens`; step, elapsed, tool and
//! unreported-usage stops map to `turn/end error`. [`LoopStopReason`] remains a
//! typed live/reconstructable fact, but this crate does not claim its exact code
//! is in JSONL until the session owner adds a budget-stop field/kind.

use std::sync::Arc;
use std::sync::{Mutex, Weak};

use heycode_core::{Layer, Next};
use heycode_session::{Session, SessionEvent, SessionEventKind, TurnEndReason};

const MAX_TIMER_MILLIS: u128 = 24 * 60 * 60 * 1_000;

/// Settings namespace for the default restart-applied loop policy.
pub const LOOP_BUDGET_SETTINGS_NAMESPACE: &str = "loop-budget";

/// Policy for a step whose previous request omitted usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopUnknownUsagePolicy {
    /// Stop because the token budget cannot be proven.
    RequireReported,
    /// Continue using the reported-token lower bound.
    AllowLowerBound,
}

/// Explicit limits for one user turn's model/tool loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopBudgetPolicy {
    max_steps_per_turn: u32,
    max_tokens_per_turn: u64,
    max_elapsed_ms: u64,
    max_tool_calls_per_turn: u32,
    unknown_usage: LoopUnknownUsagePolicy,
}

impl LoopBudgetPolicy {
    /// Validate one complete budget.
    ///
    /// `max_steps_per_turn` is the max-turn-loop bound: every provider request,
    /// including native pause/continue iterations, consumes one step.
    ///
    /// # Errors
    /// Zero disables a limit. Explicit elapsed budgets above 24 hours fail.
    pub fn new(
        max_steps_per_turn: u32,
        max_tokens_per_turn: u64,
        max_elapsed_per_turn: std::time::Duration,
        max_tool_calls_per_turn: u32,
    ) -> Result<Self, LoopBudgetError> {
        let elapsed = max_elapsed_per_turn.as_millis();
        if elapsed > MAX_TIMER_MILLIS {
            return Err(LoopBudgetError::InvalidPolicy);
        }
        let max_elapsed_ms = u64::try_from(elapsed).map_err(|_| LoopBudgetError::InvalidPolicy)?;
        Ok(Self {
            max_steps_per_turn,
            max_tokens_per_turn,
            max_elapsed_ms,
            max_tool_calls_per_turn,
            unknown_usage: LoopUnknownUsagePolicy::RequireReported,
        })
    }

    /// No cumulative execution limits are configured.
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        self.max_steps_per_turn == 0
            && self.max_tokens_per_turn == 0
            && self.max_elapsed_ms == 0
            && self.max_tool_calls_per_turn == 0
    }

    /// Choose how omitted provider usage affects the token budget.
    #[must_use]
    pub const fn with_unknown_usage(mut self, policy: LoopUnknownUsagePolicy) -> Self {
        self.unknown_usage = policy;
        self
    }

    /// Maximum provider requests/steps in one turn.
    #[must_use]
    pub const fn max_steps_per_turn(&self) -> u32 {
        self.max_steps_per_turn
    }

    /// Maximum reported prompt-plus-completion tokens in one turn.
    #[must_use]
    pub const fn max_tokens_per_turn(&self) -> u64 {
        self.max_tokens_per_turn
    }

    /// Maximum wall-clock duration in milliseconds.
    #[must_use]
    pub const fn max_elapsed_ms(&self) -> u64 {
        self.max_elapsed_ms
    }

    /// Maximum client tool dispatches in one turn.
    #[must_use]
    pub const fn max_tool_calls_per_turn(&self) -> u32 {
        self.max_tool_calls_per_turn
    }

    /// Omitted usage policy.
    #[must_use]
    pub const fn unknown_usage(&self) -> LoopUnknownUsagePolicy {
        self.unknown_usage
    }

    /// Resolve one complete Settings value into a policy.
    ///
    /// # Errors
    /// Missing/unknown/mistyped/out-of-range fields fail rather than inheriting
    /// a hidden execution default.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, LoopBudgetError> {
        let object = value.as_object().ok_or(LoopBudgetError::InvalidPolicy)?;
        if object.len() != 5
            || object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "max_steps_per_turn"
                        | "max_tokens_per_turn"
                        | "max_elapsed_ms"
                        | "max_tool_calls_per_turn"
                        | "unknown_usage"
                )
            })
        {
            return Err(LoopBudgetError::InvalidPolicy);
        }
        let max_steps = u32::try_from(required_u64(object, "max_steps_per_turn")?)
            .map_err(|_| LoopBudgetError::InvalidPolicy)?;
        let max_tokens = required_u64(object, "max_tokens_per_turn")?;
        let max_elapsed = required_u64(object, "max_elapsed_ms")?;
        let max_tools = u32::try_from(required_u64(object, "max_tool_calls_per_turn")?)
            .map_err(|_| LoopBudgetError::InvalidPolicy)?;
        let unknown_usage = match object
            .get("unknown_usage")
            .and_then(serde_json::Value::as_str)
        {
            Some("require-reported") => LoopUnknownUsagePolicy::RequireReported,
            Some("allow-lower-bound") => LoopUnknownUsagePolicy::AllowLowerBound,
            _ => return Err(LoopBudgetError::InvalidPolicy),
        };
        Ok(Self::new(
            max_steps,
            max_tokens,
            std::time::Duration::from_millis(max_elapsed),
            max_tools,
        )?
        .with_unknown_usage(unknown_usage))
    }
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<u64, LoopBudgetError> {
    object
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or(LoopBudgetError::InvalidPolicy)
}

/// Build the explicit default loop-budget Settings contract.
///
/// # Errors
/// Static namespace/schema/default construction failure.
pub fn loop_budget_settings_definition()
-> Result<heycode_settings::SettingsDefinition, heycode_settings::SettingsError> {
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "properties":{
                "max_steps_per_turn":{"type":"integer","minimum":0,"maximum":4294967295_u64},
                "max_tokens_per_turn":{"type":"integer","minimum":0},
                "max_elapsed_ms":{"type":"integer","minimum":0,"maximum":86400000},
                "max_tool_calls_per_turn":{"type":"integer","minimum":0,"maximum":4294967295_u64},
                "unknown_usage":{"type":"string","enum":["require-reported","allow-lower-bound"]}
            }
        }),
        serde_json::json!({
            "max_steps_per_turn":0,
            "max_tokens_per_turn":0,
            "max_elapsed_ms":0,
            "max_tool_calls_per_turn":0,
            "unknown_usage":"allow-lower-bound"
        }),
        |value| {
            LoopBudgetPolicy::from_value(value)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?;
    Ok(heycode_settings::SettingsDefinition::new(
        heycode_settings::SettingsNamespace::new(LOOP_BUDGET_SETTINGS_NAMESPACE)?,
        schema,
    )
    .with_applies(heycode_settings::SettingsApplies::Restart))
}

/// Durable facts consumed by a loop-budget decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopBudgetState {
    turn: u64,
    completed_steps: u32,
    reported_tokens: u64,
    unreported_steps: u32,
    tool_calls: u32,
    elapsed_ms: Option<u64>,
}

impl LoopBudgetState {
    /// Reconstruct one turn from the durable event log.
    ///
    /// # Errors
    /// Missing/duplicate turn start, token/count overflow, or an event sequence
    /// that cannot be attributed safely fails rather than resetting a counter.
    pub fn project(
        events: &[SessionEvent],
        turn: u64,
        at_ms: u64,
    ) -> Result<Self, LoopBudgetError> {
        let mut saw_start = false;
        let mut start_ms = None;
        let mut completed_steps = 0u32;
        let mut reported_tokens = 0u64;
        let mut unreported_steps = 0u32;
        let mut tool_calls = 0u32;
        for event in events {
            match &event.kind {
                SessionEventKind::TurnStart { turn: candidate } if *candidate == turn => {
                    if saw_start {
                        return Err(LoopBudgetError::DuplicateTurnStart);
                    }
                    saw_start = true;
                    start_ms = u64::try_from(event.time_ms).ok();
                }
                SessionEventKind::StepEnd {
                    turn: candidate, ..
                } if *candidate == turn => {
                    completed_steps = completed_steps
                        .checked_add(1)
                        .ok_or(LoopBudgetError::CountOverflow)?;
                }
                SessionEventKind::AssistantMessage {
                    turn: candidate,
                    usage,
                    ..
                } if *candidate == turn => match usage {
                    Some(usage) => {
                        let step_tokens = usage
                            .prompt_tokens
                            .checked_add(usage.completion_tokens)
                            .ok_or(LoopBudgetError::TokenOverflow)?;
                        reported_tokens = reported_tokens
                            .checked_add(step_tokens)
                            .ok_or(LoopBudgetError::TokenOverflow)?;
                    }
                    None => {
                        unreported_steps = unreported_steps
                            .checked_add(1)
                            .ok_or(LoopBudgetError::CountOverflow)?;
                    }
                },
                SessionEventKind::ToolCall {
                    turn: candidate, ..
                } if *candidate == turn => {
                    tool_calls = tool_calls
                        .checked_add(1)
                        .ok_or(LoopBudgetError::CountOverflow)?;
                }
                _ => {}
            }
        }
        let start_ms = start_ms.ok_or(LoopBudgetError::MissingTurnStart)?;
        Ok(Self {
            turn,
            completed_steps,
            reported_tokens,
            unreported_steps,
            tool_calls,
            elapsed_ms: at_ms.checked_sub(start_ms),
        })
    }

    /// First deterministic reason the next step is outside policy.
    #[must_use]
    pub fn exhaustion(&self, policy: &LoopBudgetPolicy, next_step: u32) -> Option<LoopStopReason> {
        if policy.max_steps_per_turn != 0 && next_step > policy.max_steps_per_turn {
            return Some(LoopStopReason::MaxStepsPerTurn);
        }
        if policy.max_tokens_per_turn != 0 && self.reported_tokens >= policy.max_tokens_per_turn {
            return Some(LoopStopReason::MaxTokensPerTurn);
        }
        if policy.max_tokens_per_turn != 0
            && self.unreported_steps != 0
            && policy.unknown_usage == LoopUnknownUsagePolicy::RequireReported
        {
            return Some(LoopStopReason::UnreportedTokenUsage);
        }
        if policy.max_elapsed_ms != 0 {
            match self.elapsed_ms {
                Some(elapsed) if elapsed >= policy.max_elapsed_ms => {
                    return Some(LoopStopReason::MaxElapsedPerTurn);
                }
                None => return Some(LoopStopReason::ClockUnavailable),
                Some(_) => {}
            }
        }
        if policy.max_tool_calls_per_turn != 0 && self.tool_calls >= policy.max_tool_calls_per_turn
        {
            return Some(LoopStopReason::MaxToolCallsPerTurn);
        }
        None
    }

    /// Turn identity projected.
    #[must_use]
    pub const fn turn(&self) -> u64 {
        self.turn
    }

    /// Durably completed steps.
    #[must_use]
    pub const fn completed_steps(&self) -> u32 {
        self.completed_steps
    }

    /// Sum of reported prompt and completion tokens.
    #[must_use]
    pub const fn reported_tokens(&self) -> u64 {
        self.reported_tokens
    }

    /// Assistant steps whose provider reported no usage.
    #[must_use]
    pub const fn unreported_steps(&self) -> u32 {
        self.unreported_steps
    }

    /// Durably dispatched client tool calls.
    #[must_use]
    pub const fn tool_calls(&self) -> u32 {
        self.tool_calls
    }

    /// Elapsed duration when the comparison clock did not move backward.
    #[must_use]
    pub const fn elapsed_ms(&self) -> Option<u64> {
        self.elapsed_ms
    }
}

/// Why the loop budget stopped before another request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStopReason {
    /// Next request exceeds the step/turn-loop cap.
    MaxStepsPerTurn,
    /// Reported tokens reached the cap.
    MaxTokensPerTurn,
    /// Provider omitted usage under strict policy.
    UnreportedTokenUsage,
    /// Wall-clock duration reached the cap.
    MaxElapsedPerTurn,
    /// Durable client dispatch count reached the cap.
    MaxToolCallsPerTurn,
    /// Wall clock was unavailable or moved behind turn start.
    ClockUnavailable,
}

impl LoopStopReason {
    /// Stable machine code included in the failure text.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MaxStepsPerTurn => "max_steps_per_turn",
            Self::MaxTokensPerTurn => "max_tokens_per_turn",
            Self::UnreportedTokenUsage => "unreported_token_usage",
            Self::MaxElapsedPerTurn => "max_elapsed_per_turn",
            Self::MaxToolCallsPerTurn => "max_tool_calls_per_turn",
            Self::ClockUnavailable => "clock_unavailable",
        }
    }

    /// Exact durable session reason for this budget stop.
    #[must_use]
    pub const fn durable_reason(self) -> TurnEndReason {
        match self {
            Self::MaxTokensPerTurn => TurnEndReason::MaxTokens,
            Self::MaxStepsPerTurn => TurnEndReason::MaxSteps,
            Self::UnreportedTokenUsage => TurnEndReason::UnreportedTokenUsage,
            Self::MaxElapsedPerTurn => TurnEndReason::MaxElapsed,
            Self::MaxToolCallsPerTurn => TurnEndReason::MaxToolCalls,
            Self::ClockUnavailable => TurnEndReason::ClockUnavailable,
        }
    }

    fn message(self) -> String {
        format!("loop budget exhausted: {}", self.code())
    }
}

/// Clock boundary used by elapsed-turn policy.
pub trait LoopClock: Send + Sync {
    /// Current Unix epoch milliseconds, absent when the clock is unusable.
    fn now_ms(&self) -> Option<u64>;
}

/// Production wall-clock implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLoopClock;

impl LoopClock for SystemLoopClock {
    fn now_ms(&self) -> Option<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok())
    }
}

/// Pre-step enforcement layer over one durable Session.
pub struct LoopBudgetLayer {
    session: Arc<std::sync::Mutex<Session>>,
    policy: LoopBudgetPolicy,
    clock: Arc<dyn LoopClock>,
}

pub(crate) type LoopBudgetOwnerSlot = Mutex<Option<Arc<()>>>;

pub(crate) struct LoopBudgetRegistration {
    pub(crate) slot: Weak<LoopBudgetOwnerSlot>,
    pub(crate) token: Arc<()>,
}

impl Drop for LoopBudgetRegistration {
    fn drop(&mut self) {
        let Some(slot) = self.slot.upgrade() else {
            return;
        };
        let Ok(mut owner) = slot.lock() else {
            return;
        };
        if owner
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.token))
        {
            *owner = None;
        }
    }
}

impl LoopBudgetLayer {
    /// Bind policy and clock to the exact session this Agent owns.
    #[must_use]
    pub fn new(
        session: Arc<std::sync::Mutex<Session>>,
        policy: LoopBudgetPolicy,
        clock: Arc<dyn LoopClock>,
    ) -> Self {
        Self {
            session,
            policy,
            clock,
        }
    }
}

#[async_trait::async_trait]
impl Layer<crate::PreStepDecision> for LoopBudgetLayer {
    async fn handle(
        &self,
        input: &mut crate::PreStepDecision,
        mut next: Next<'_, crate::PreStepDecision>,
    ) -> anyhow::Result<()> {
        // Turn/caller cancellation owns settlement. This layer neither spends
        // a budget nor changes the stop classification when that token wins.
        if input.cancellation.is_cancelled() || self.policy.is_unlimited() {
            return next.run(input).await;
        }
        let reason = match self.clock.now_ms() {
            None => Some(LoopStopReason::ClockUnavailable),
            Some(now) => {
                let session = self
                    .session
                    .lock()
                    .map_err(|_| LoopBudgetError::SessionUnavailable)?;
                LoopBudgetState::project(session.events(), input.turn, now)?
                    .exhaustion(&self.policy, input.step)
            }
        };
        if let Some(reason) = reason {
            input.verdict = crate::StepVerdict::StopTurnDurably {
                reason: reason.message(),
                durable_reason: reason.durable_reason(),
            };
            return Ok(());
        }
        next.run(input).await
    }
}

/// Mount the effect-owned budget layer after the Agent plugin.
#[must_use]
pub fn loop_budget_plugin(policy: LoopBudgetPolicy) -> Box<dyn heycode_core::Plugin> {
    struct LoopBudgetPlugin(LoopBudgetPolicy);

    impl heycode_core::Plugin for LoopBudgetPlugin {
        fn name(&self) -> &'static str {
            "loop-budget"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Waterfall],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::InterceptionLayer,
                "agent/pre-step:loop-budget",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_AGENT]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let agent = context
                .get::<crate::Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service type mismatch"))?;
            install_budget(context, &agent, self.0)
        }
    }

    Box::new(LoopBudgetPlugin(policy))
}

/// Mount the default restart-applied Settings-backed loop budget.
#[must_use]
pub fn settings_loop_budget_plugin() -> Box<dyn heycode_core::Plugin> {
    struct SettingsLoopBudgetPlugin;

    impl heycode_core::Plugin for SettingsLoopBudgetPlugin {
        fn name(&self) -> &'static str {
            "loop-budget-settings"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Waterfall,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::SettingsNamespace,
                    LOOP_BUDGET_SETTINGS_NAMESPACE,
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::InterceptionLayer,
                    "agent/pre-step:loop-budget",
                ),
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS, crate::SERVICE_AGENT]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| heycode_core::CoreError::other("settings service type mismatch"))?;
            let agent = context
                .get::<crate::Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service type mismatch"))?;
            let snapshot = settings
                .register(
                    context,
                    loop_budget_settings_definition()
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let policy = LoopBudgetPolicy::from_value(snapshot.resolved())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            install_budget(context, &agent, policy)
        }
    }

    Box::new(SettingsLoopBudgetPlugin)
}

fn install_budget(
    context: &heycode_core::Context,
    agent: &Arc<crate::Agent>,
    policy: LoopBudgetPolicy,
) -> heycode_core::CoreResult<()> {
    let registration = agent
        .install_loop_budget_owner()
        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
    // Register this first so LIFO removes the seam layer before the marker
    // that suppresses the legacy hardcoded fallback.
    context.effect(move || drop(registration));
    agent.pre_step_seam().push_effect(
        context,
        LoopBudgetLayer::new(agent.session().clone(), policy, Arc::new(SystemLoopClock)),
    );
    Ok(())
}

/// Loop budget configuration/projection failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoopBudgetError {
    /// Policy used zero or an unsupported duration.
    #[error("loop budget policy is invalid")]
    InvalidPolicy,
    /// Current turn has no durable start.
    #[error("loop budget turn has no durable start")]
    MissingTurnStart,
    /// Same turn id was started twice.
    #[error("loop budget turn has duplicate starts")]
    DuplicateTurnStart,
    /// Token arithmetic overflowed.
    #[error("loop budget token accounting overflowed")]
    TokenOverflow,
    /// Step/tool/unreported count overflowed.
    #[error("loop budget count overflowed")]
    CountOverflow,
    /// Session mutex is unavailable.
    #[error("loop budget session state is unavailable")]
    SessionUnavailable,
    /// Another budget plugin already owns this Agent.
    #[error("a loop budget is already installed")]
    AlreadyInstalled,
    /// Agent-local budget owner marker is unavailable.
    #[error("loop budget owner state is unavailable")]
    OwnerUnavailable,
}

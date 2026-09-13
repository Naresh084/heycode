//! Interception seams around the turn loop (A02).
//!
//! Three decision points in [`crate::Agent`]'s step loop are open to layers,
//! each an ordered `Waterfall` of around-middleware:
//!
//! * **pre-step** — before a step is durably announced. A layer may stop the
//!   turn outright.
//! * **request** — after the request is assembled, before it is dispatched. A
//!   layer may edit the request, or declare it stale after changing durable
//!   history.
//! * **request-error** — the single point at which a failed request closes its
//!   turn. A layer may revise the failure text before it is announced.
//!
//! Every seam is around-middleware: a layer that does not short-circuit MUST
//! end in `next.run(input).await` (GOTCHAS #16). Each seam runs at exactly one
//! call site; a seam invoked from two places would let two different decisions
//! claim to be the same one.
//!
//! The chains are owned by the `Agent` rather than published as services
//! because a step belongs to one turn loop: a subagent child runs its own
//! steps and must not inherit its parent's step budget. Layers mount through
//! [`crate::Agent::pre_step_seam`] and its siblings after the agent is
//! published, exactly like the late-registration path in GOTCHAS #3.

use tokio_util::sync::CancellationToken;

/// What a pre-step layer decided about the step that is about to begin.
#[derive(Debug, Clone)]
pub enum StepVerdict {
    /// Run the step. The default; layers that agree call `next`.
    Proceed,
    /// Do not run this step and end the turn. The layer must return without
    /// calling `next`. Nothing is announced for the step that never ran: no
    /// `step/start` reaches the log, the turn closes as `turn/end error`, and
    /// `reason` is what the caller and the UI receive.
    StopTurn {
        /// Why the turn stopped. Becomes the turn's failure text.
        reason: String,
    },
    /// Stop before this step and choose one existing durable turn-end class.
    /// Budget plugins use this to preserve `max_tokens`; other exact budget
    /// codes remain in the safe failure text until the session vocabulary owns
    /// them explicitly.
    StopTurnDurably {
        /// Safe actionable failure text returned to the caller/UI.
        reason: String,
        /// Existing session-level terminal class.
        durable_reason: heycode_session::TurnEndReason,
    },
}

/// The decision flowing through the pre-step seam.
///
/// Runs once per step, before the step is durably announced and before any
/// queued steer/inject input is spliced in.
#[derive(Debug, Clone)]
pub struct PreStepDecision {
    /// 1-based turn this step belongs to.
    pub turn: u64,
    /// 1-based step number within the turn.
    pub step: u32,
    /// Operation token for this seam run. A layer that parks MUST race it;
    /// the agent cancels it when either the turn or the caller is cancelled,
    /// so the turn keeps one cancellation owner.
    pub cancellation: CancellationToken,
    /// Current verdict; starts as [`StepVerdict::Proceed`].
    pub verdict: StepVerdict,
}

/// What a request layer decided about the assembled request.
#[derive(Debug, Clone)]
pub enum RequestVerdict {
    /// Dispatch the request as it stands, including any layer edits.
    Dispatch,
    /// Do not dispatch: durable history changed under this request, so it no
    /// longer matches the log. The layer must return without calling `next`,
    /// because later layers would otherwise inspect a request that is already
    /// stale. The agent rebuilds the request from the log exactly once and
    /// dispatches that; the seam is not re-run for the same step.
    Rebuild {
        /// Why the request became stale. Diagnostics only.
        reason: String,
    },
}

/// The decision flowing through the request seam.
///
/// Runs once per step, after the request is assembled from the durable log and
/// before it reaches a provider.
pub struct RequestDecision {
    /// 1-based turn this request belongs to.
    pub turn: u64,
    /// 1-based step number within the turn.
    pub step: u32,
    /// The assembled request. Layers may edit it in place; edits made on a
    /// delegating path survive to dispatch.
    pub request: heycode_llm::ChatRequest,
    /// Operation token for this seam run. A layer that parks — a summarizer,
    /// an approval — MUST race it; the agent cancels it when either the turn
    /// or the caller is cancelled.
    pub cancellation: CancellationToken,
    /// Current verdict; starts as [`RequestVerdict::Dispatch`].
    pub verdict: RequestVerdict,
}

/// Which part of a model request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestErrorStage {
    /// A pre-step layer stopped the turn before a request was built.
    PreStep,
    /// Routing, catalog resolution or adapter preparation failed before
    /// anything was dispatched.
    Prepare,
    /// The dispatched stream failed, or its events could not be absorbed.
    Stream,
    /// The completed stream broke a turn-loop invariant.
    Invariant,
}

impl RequestErrorStage {
    /// Stable diagnostic name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PreStep => "pre-step",
            Self::Prepare => "prepare",
            Self::Stream => "stream",
            Self::Invariant => "invariant",
        }
    }

    /// Whether this failure occurs after `step/start` and before `step/end`.
    pub(crate) const fn has_open_step(self) -> bool {
        matches!(self, Self::Prepare | Self::Stream)
    }
}

/// The decision flowing through the request-error seam.
///
/// Runs once per failed request, at the single point where a failure closes
/// its turn. The turn always ends `turn/end error`; what the chain decides is
/// the failure text. A layer that revises [`Self::message`] and delegates
/// leaves it open to later layers; a layer that revises it and returns without
/// calling `next` finalizes it — later layers do not run and cannot revise it.
/// The surviving message is announced as `UiEvent::Error` and returned to the
/// caller.
///
/// Prepare/stream failures close their already-started step before the turn;
/// pre-step/invariant failures have no open step at this point. This seam is
/// not cancellable: closing a failed turn must finish even when the turn was
/// cancelled, so layers here must not park.
#[derive(Debug, Clone)]
pub struct RequestErrorDecision {
    /// 1-based turn that failed.
    pub turn: u64,
    /// 1-based step that failed.
    pub step: u32,
    /// Which part of the request failed.
    pub stage: RequestErrorStage,
    /// Safe failure text, seeded from the underlying error.
    pub message: String,
}

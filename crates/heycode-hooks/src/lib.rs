//! O08 hook service: typed lifecycle points backed by host commands.
//!
//! A hook is arbitrary code someone else wrote, running on the user's machine,
//! at a moment heycode chose. Three of the four things this row asks for exist
//! because of that — a **timeout**, so a hung hook cannot hang heycode; a **trust**
//! gate, so a project-supplied hook cannot run in a workspace nobody vouched
//! for; and **disposal**, so a plugin's hook leaves when the plugin does.
//!
//! The fourth, the **pre/post lifecycle**, is what makes the other three
//! meaningful: a `Pre` hook can refuse the operation it precedes, and a `Post`
//! hook cannot. That asymmetry is in the types, because a post hook that could
//! veto would be vetoing something that already happened.
//!
//! O09 adds the other handler kinds — prompt, subagent and MCP tool — behind
//! [`HookHandler`], and the lifecycle points where they are useful. Every one
//! of them enters through the same [`HookService::run_with`] path, so the time
//! budget, the trust gate, the ordering rule and the phase's refusal policy are
//! applied once and cannot be re-decided per kind. See [`handler`] for the
//! answer/fault vocabulary and [`providers`] for the three maps onto it.

mod handler;
mod providers;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt as _;
use heycode_core::Context;
use heycode_exec::{ProcessExit, ShellRequest, ShellService};
use heycode_trust::WorkspaceTrustDecision;

pub use handler::{
    CommittedHookContribution, HookAction, HookAnswer, HookBridgeFault, HookContribution,
    HookContributionEvent, HookContributionProvenance, HookDecision, HookDurableEventBridge,
    HookHandler, HookHandlerKind, HookInvocation, HookPayload,
};
pub use providers::{
    McpHookResult, McpToolHookCaller, McpToolHookHandler, PromptHookHandler, PromptHookRunner,
    SubagentHookFailure, SubagentHookHandler, SubagentHookLauncher,
};

/// Service key for the hook registry.
pub const SERVICE_HOOKS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("hooks");

/// Longest a single hook may run before it is abandoned.
///
/// A hook is someone else's code on the critical path of an operation the user
/// is waiting for. The budget is deliberately small and deliberately not
/// configurable per hook: a hook author cannot extend their own leash.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest output heycode will read back from a hook.
pub const HOOK_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

/// Where in an operation a hook runs.
///
/// Closed on purpose: a new lifecycle point must break every consumer that
/// decides per point, rather than being absorbed by a `_` arm that silently
/// gives it the wrong policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookPhase {
    /// Runs before the operation, and may refuse it.
    Pre,
    /// Runs after the operation. Cannot refuse: the operation already happened.
    Post,
}

impl HookPhase {
    /// Whether a hook at this phase can refuse the operation.
    #[must_use]
    pub const fn can_refuse(self) -> bool {
        matches!(self, Self::Pre)
    }

    /// Stable diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Post => "post",
        }
    }
}

/// The operations heycode offers hook points around.
///
/// Closed for the same reason as [`HookPhase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum HookEvent {
    /// A model-callable tool is about to run, or has run.
    ToolUse,
    /// A turn is starting, or has settled.
    Turn,
    /// A session is opening, or closing.
    Session,
    /// A user prompt is being submitted, or has been accepted.
    UserPrompt,
    /// A subagent is starting, or has stopped.
    Subagent,
    /// An MCP server is about to be used, or its state has changed.
    McpServer,
}

impl HookEvent {
    /// Every event, for a surface that must enumerate them.
    pub const ALL: [Self; 6] = [
        Self::ToolUse,
        Self::Turn,
        Self::Session,
        Self::UserPrompt,
        Self::Subagent,
        Self::McpServer,
    ];

    /// Stable diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolUse => "tool-use",
            Self::Turn => "turn",
            Self::Session => "session",
            Self::UserPrompt => "user-prompt",
            Self::Subagent => "subagent",
            Self::McpServer => "mcp-server",
        }
    }
}

/// One registered hook.
#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    /// Owning plugin or scope, for attribution in diagnostics.
    pub owner: String,
    /// Which lifecycle point this runs at.
    pub phase: HookPhase,
    /// Which operation it surrounds.
    pub event: HookEvent,
    /// What to do when it fires, and therefore which provider executes it.
    pub action: HookAction,
    /// Whether the hook came from project-scoped configuration.
    ///
    /// A project hook is code from the checkout, so it is exactly what K12's
    /// trust gate exists for. A user-scoped hook is the user's own.
    pub project_scoped: bool,
}

/// Why a hook did not run, or did not succeed.
///
/// Closed, and no variant carries hook output: a hook is third-party code whose
/// stdout is attacker-influenced whenever the workspace is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HookFault {
    /// The workspace is not trusted and the hook is project-scoped.
    UntrustedWorkspace,
    /// The hook exceeded [`HOOK_TIMEOUT`].
    TimedOut,
    /// The hook ran and failed: a non-zero exit, or a handler reporting that
    /// the work it was asked to do did not succeed.
    Failed,
    /// The hook could not be started at all — an unusable command, a missing
    /// binary, or a gate that stopped the handler before it reached a decision.
    Unlaunchable,
    /// The caller cancelled before the hook settled.
    Cancelled,
    /// The hook's action names a handler kind nothing has registered.
    ///
    /// Distinct from [`Self::Unlaunchable`] because the remedy is different: an
    /// unlaunchable hook is broken, an unavailable one is configured for a
    /// capability this composition does not have.
    HandlerUnavailable,
    /// A handler refused where refusal is not available to it.
    ///
    /// Either the phase cannot refuse — a `Post` hook has nothing left to veto
    /// — or the handler kind may never refuse. The refusal is not honoured and
    /// is not laundered into an allowance: it is reported as its own class so a
    /// misconfigured guard is visible rather than quietly inert.
    RefusalNotPermitted,
}

impl std::fmt::Display for HookFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UntrustedWorkspace => "the workspace is not trusted, so project hooks stay inert",
            Self::TimedOut => "the hook exceeded its time budget",
            Self::Failed => "the hook ran and failed",
            Self::Unlaunchable => "the hook could not be launched",
            Self::Cancelled => "the operation was cancelled before the hook settled",
            Self::HandlerUnavailable => "no handler is registered for this hook's action",
            Self::RefusalNotPermitted => "the hook refused where refusal is not available to it",
        })
    }
}

impl std::error::Error for HookFault {}

/// What one hook concluded.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HookOutcome {
    /// The hook ran and allowed the operation.
    Allowed {
        /// Text the hook contributed to the model's context, if any, already
        /// carrying the provenance the service — not the handler — assigned it.
        contribution: Option<HookContribution>,
    },
    /// A `Pre` hook refused the operation.
    Refused {
        /// The hook's owner, so a user can tell who blocked them.
        owner: String,
    },
    /// The hook did not complete. The operation is **not** refused by a fault:
    /// a broken hook must not become an outage, so only a deliberate refusal
    /// from a hook entitled to refuse refuses.
    Faulted {
        /// The hook's owner.
        owner: String,
        /// Why, as a closed class.
        fault: HookFault,
    },
}

impl HookOutcome {
    /// The hook allowed the operation and contributed nothing.
    #[must_use]
    pub const fn allowed() -> Self {
        Self::Allowed { contribution: None }
    }

    /// Whether the surrounded operation may proceed.
    #[must_use]
    pub const fn proceeds(&self) -> bool {
        !matches!(self, Self::Refused { .. })
    }

    /// Why the hook did not complete, if it did not.
    ///
    /// A fault proceeds, so `proceeds()` alone cannot tell a hook that allowed
    /// the operation from one that never ran. This is the accessor that can:
    /// callers that must not treat a broken guard as a passed guard read it.
    #[must_use]
    pub const fn fault(&self) -> Option<HookFault> {
        match self {
            Self::Faulted { fault, .. } => Some(*fault),
            Self::Allowed { .. } | Self::Refused { .. } => None,
        }
    }

    /// The text the hook contributed, if it allowed and produced any.
    #[must_use]
    pub const fn contribution(&self) -> Option<&HookContribution> {
        match self {
            Self::Allowed { contribution } => contribution.as_ref(),
            Self::Refused { .. } | Self::Faulted { .. } => None,
        }
    }

    /// Consume this outcome and return its pending contribution, if any.
    #[must_use]
    pub fn into_contribution(self) -> Option<HookContribution> {
        match self {
            Self::Allowed { contribution } => contribution,
            Self::Refused { .. } | Self::Faulted { .. } => None,
        }
    }
}

/// Optional ambient execution policy (for example a read-only Plan session).
/// Hooks run outside ordinary tool approval and must honor this gate themselves.
pub trait HookExecutionGate: Send + Sync {
    /// Whether any configured hook may start now. False is a refusal, never a grant.
    fn permits_execution(&self) -> bool;
    /// Bind the actual hook owner so an ambient transition can await in-flight work.
    fn attach_service(&self, _service: Arc<HookService>) {}
}

/// Optional shared ambient gate, independent of plugin composition order.
pub struct HookExecutionGateHandle(pub Arc<dyn HookExecutionGate>);
/// Ambient hook execution gate provided by a session policy plugin.
pub const SERVICE_HOOK_EXECUTION_GATE: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("hook-execution-gate");

/// Registered hooks and handlers, and the policy for running them.
pub struct HookService {
    shell: Arc<ShellService>,
    hooks: Arc<Mutex<Vec<Arc<Hook>>>>,
    handlers: Arc<Mutex<Vec<Arc<dyn HookHandler>>>>,
    trust: WorkspaceTrustDecision,
    execution_gate: Arc<Mutex<Option<Arc<dyn HookExecutionGate>>>>,
    active: std::sync::atomic::AtomicUsize,
    idle: tokio::sync::Notify,
}

struct HookRunLease<'a>(&'a HookService);
impl Drop for HookRunLease<'_> {
    fn drop(&mut self) {
        self.0
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0.idle.notify_waiters();
    }
}

impl std::fmt::Debug for HookService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookService")
            .field("trust", &self.trust)
            .field("registered", &self.len())
            .field("handlers", &self.handler_kinds())
            .finish_non_exhaustive()
    }
}

impl HookService {
    /// A service over a shell backend and one workspace trust decision.
    ///
    /// The command handler is built in, because the shell arrives here. The
    /// prompt, subagent and MCP handlers are registered by whoever owns those
    /// capabilities; until one is, a hook naming it faults with
    /// [`HookFault::HandlerUnavailable`] rather than being treated as absent.
    #[must_use]
    pub fn new(shell: Arc<ShellService>, trust: WorkspaceTrustDecision) -> Self {
        Self {
            shell,
            hooks: Arc::new(Mutex::new(Vec::new())),
            handlers: Arc::new(Mutex::new(Vec::new())),
            trust,
            execution_gate: Arc::new(Mutex::new(None)),
            active: std::sync::atomic::AtomicUsize::new(0),
            idle: tokio::sync::Notify::new(),
        }
    }

    /// Attach a single context-owned execution policy without weakening an existing one.
    pub fn attach_execution_gate(
        &self,
        context: &Context,
        gate: Arc<dyn HookExecutionGate>,
    ) -> Result<(), &'static str> {
        let mut slot = self
            .execution_gate
            .lock()
            .map_err(|_| "hook execution gate unavailable")?;
        if let Some(current) = slot.as_ref() {
            return if Arc::ptr_eq(current, &gate) {
                Ok(())
            } else {
                Err("hook execution gate already attached")
            };
        }
        *slot = Some(gate.clone());
        drop(slot);
        let binding = Arc::downgrade(&self.execution_gate);
        context.effect(move || {
            if let Some(binding) = binding.upgrade()
                && let Ok(mut slot) = binding.lock()
                && slot
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &gate))
            {
                *slot = None;
            }
        });
        Ok(())
    }

    /// Wait until every handler which began before an ambient gate closed has settled.
    pub async fn wait_for_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.active.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    /// Number of registered hooks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hooks.lock().map_or(0, |hooks| hooks.len())
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Register a hook owned by one plugin context effect.
    ///
    /// Rollback or shutdown removes this exact hook and no other. There is
    /// deliberately no non-effect registration: a hook is executable code, and
    /// one that outlives its owner is code running for a plugin that is gone.
    pub fn register(&self, context: &Context, hook: Hook) {
        let registration = self.register_owned(hook);
        context.effect(move || drop(registration));
    }

    /// Register one hook and return its exact ownership handle.
    ///
    /// This lower-level form is for aggregate declarative activation, whose
    /// bridge owns the context effect. Ordinary hook plugins should use
    /// [`Self::register`].
    #[must_use]
    pub fn register_owned(&self, hook: Hook) -> HookRegistration {
        let hook = Arc::new(hook);
        if let Ok(mut hooks) = self.hooks.lock() {
            hooks.push(Arc::clone(&hook));
        }
        HookRegistration {
            registry: Arc::downgrade(&self.hooks),
            hook,
            active: true,
        }
    }

    /// Register a handler owned by one plugin context effect.
    ///
    /// Effect-only for the same reason as [`Self::register`]: a handler is a
    /// live call-out into another subsystem, and one that outlives its owner is
    /// a call into a subsystem that is gone.
    ///
    /// Registering a second handler for a kind **shadows** the first, and
    /// disposing the second restores it. Effects unwind LIFO, so the stack the
    /// registry keeps and the order the disposers run in agree by construction.
    pub fn register_handler(&self, context: &Context, handler: Arc<dyn HookHandler>) {
        if let Ok(mut handlers) = self.handlers.lock() {
            handlers.push(Arc::clone(&handler));
        }
        let registry = Arc::downgrade(&self.handlers);
        context.effect(move || {
            let Some(registry) = registry.upgrade() else {
                return;
            };
            let Ok(mut handlers) = registry.lock() else {
                return;
            };
            handlers.retain(|registered| !Arc::ptr_eq(registered, &handler));
        });
    }

    /// The kinds a registered handler currently serves, in registration order.
    #[must_use]
    pub fn handler_kinds(&self) -> Vec<HookHandlerKind> {
        self.handlers.lock().map_or_else(
            |_| Vec::new(),
            |handlers| handlers.iter().map(|handler| handler.kind()).collect(),
        )
    }

    /// The handler that would execute an action of this kind, if any.
    fn handler_for(&self, kind: HookHandlerKind) -> Option<Arc<dyn HookHandler>> {
        self.handlers
            .lock()
            .ok()?
            .iter()
            .rev()
            .find(|handler| handler.kind() == kind)
            .map(Arc::clone)
    }

    /// Hooks registered for one phase and event, in registration order.
    #[must_use]
    pub fn matching(&self, phase: HookPhase, event: HookEvent) -> Vec<Arc<Hook>> {
        self.hooks.lock().map_or_else(
            |_| Vec::new(),
            |hooks| {
                hooks
                    .iter()
                    .filter(|hook| hook.phase == phase && hook.event == event)
                    .cloned()
                    .collect()
            },
        )
    }

    /// Whether a hook is allowed to run at all under the current trust state.
    ///
    /// `Unknown` is not trusted. K12's rule is that project executable
    /// contributions stay inert until an affirmative decision exists, and the
    /// absence of a decision is not an affirmative one.
    #[must_use]
    pub const fn permits(&self, hook: &Hook) -> bool {
        !hook.project_scoped || matches!(self.trust, WorkspaceTrustDecision::Trusted)
    }

    /// Run every hook for one phase and event, in order, with no content.
    pub async fn run(
        &self,
        phase: HookPhase,
        event: HookEvent,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<HookOutcome> {
        self.run_with(phase, event, &HookPayload::empty(), cancellation)
            .await
    }

    /// Run every hook for one phase and event, in order, over one payload.
    ///
    /// **Ordering.** Hooks run sequentially in registration order, never
    /// concurrently, and the order does not depend on handler kind: a hook
    /// registered first runs first whether it is a command or a subagent. The
    /// order survives disposal of an earlier hook, because removal preserves
    /// the relative order of the rest.
    ///
    /// **Refusal.** The first refusal stops the remaining hooks, because a
    /// refused operation must not keep running the hooks that were meant to
    /// observe it. A fault stops nothing: a broken hook must not silently
    /// suppress the ones after it.
    ///
    /// **Payload.** Typed handlers receive the payload projected through its
    /// boundary, so a hook that reads untrusted content reads it labelled. The
    /// command handler receives no payload at all — passing external content
    /// into a shell command is its own design, and O08's shell contract does
    /// not carry one.
    pub async fn run_with(
        &self,
        phase: HookPhase,
        event: HookEvent,
        payload: &HookPayload,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<HookOutcome> {
        let mut outcomes = Vec::new();
        for hook in self.matching(phase, event) {
            let outcome = self.run_one(&hook, payload, cancellation.clone()).await;
            let refused = matches!(outcome, HookOutcome::Refused { .. });
            outcomes.push(outcome);
            if refused {
                break;
            }
        }
        outcomes
    }

    async fn run_one(
        &self,
        hook: &Hook,
        payload: &HookPayload,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> HookOutcome {
        self.active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _lease = HookRunLease(self);
        let owner = hook.owner.clone();
        let gate = match self.execution_gate.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => return HookOutcome::Refused { owner },
        };
        if gate.as_ref().is_some_and(|gate| !gate.permits_execution()) {
            return HookOutcome::Refused { owner };
        }
        // Both gates precede every dispatch path, so no handler kind can be
        // reached in an untrusted workspace or after cancellation.
        if !self.permits(hook) {
            return HookOutcome::Faulted {
                owner,
                fault: HookFault::UntrustedWorkspace,
            };
        }
        if cancellation.is_cancelled() {
            return HookOutcome::Faulted {
                owner,
                fault: HookFault::Cancelled,
            };
        }
        match &hook.action {
            HookAction::Command(command) => self.run_command(hook, command, cancellation).await,
            action => self.run_handler(hook, action, payload, cancellation).await,
        }
    }

    /// Execute one typed handler and apply the phase and kind refusal policy.
    async fn run_handler(
        &self,
        hook: &Hook,
        action: &HookAction,
        payload: &HookPayload,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> HookOutcome {
        let owner = hook.owner.clone();
        let kind = action.kind();
        let Some(handler) = self.handler_for(kind) else {
            return HookOutcome::Faulted {
                owner,
                fault: HookFault::HandlerUnavailable,
            };
        };
        let invocation = HookInvocation::new(
            owner.clone(),
            hook.phase,
            hook.event,
            action.clone(),
            payload.render_for_model(),
        );
        // The budget is applied here, by the service, for exactly the reason
        // O08 gives the command path its own: a handler author cannot extend
        // their own leash. A handler that never returns is abandoned, and the
        // elapse is classified as a timeout rather than a generic failure.
        let invocation =
            std::panic::AssertUnwindSafe(handler.invoke(invocation, cancellation.clone()))
                .catch_unwind();
        let answer = match tokio::time::timeout(HOOK_TIMEOUT, invocation).await {
            Err(_elapsed) => {
                return HookOutcome::Faulted {
                    owner,
                    fault: HookFault::TimedOut,
                };
            }
            // A handler's fault is its own class, carried through unchanged: a
            // hook that did not run must never read as one that allowed.
            Ok(Err(_panic)) => {
                return HookOutcome::Faulted {
                    owner,
                    fault: HookFault::Failed,
                };
            }
            Ok(Ok(Err(fault))) => return HookOutcome::Faulted { owner, fault },
            Ok(Ok(Ok(answer))) => answer,
        };
        let (decision, text) = answer.into_parts();
        match decision {
            HookDecision::Refuse if hook.phase.can_refuse() && kind.may_refuse() => {
                HookOutcome::Refused { owner }
            }
            // The refusal is dropped, and saying so is the point: reporting it
            // as `Allowed` would be indistinguishable from a handler that
            // actually allowed, and honouring it would let a `Post` hook veto
            // something that already happened.
            HookDecision::Refuse => HookOutcome::Faulted {
                owner,
                fault: HookFault::RefusalNotPermitted,
            },
            // Provenance is assigned here and nowhere else. A handler hands
            // back plain text; the label comes from the kind's intrinsic source
            // if it has one, and otherwise from whatever the payload carried.
            // A handler therefore cannot strip, weaken, or invent a label.
            HookDecision::Allow => HookOutcome::Allowed {
                contribution: text.map(|text| {
                    HookContribution::new(
                        text,
                        HookContributionProvenance::new(
                            owner,
                            hook.phase,
                            hook.event,
                            kind,
                            kind.intrinsic_boundary().or_else(|| payload.boundary()),
                        ),
                    )
                }),
            },
        }
    }

    /// Execute one host command. O08's path, unchanged.
    async fn run_command(
        &self,
        hook: &Hook,
        command: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> HookOutcome {
        let owner = hook.owner.clone();
        let request = ShellRequest::new(command.to_owned())
            .and_then(|request| request.with_timeout(HOOK_TIMEOUT))
            .and_then(|request| request.with_output_limit_bytes(HOOK_OUTPUT_LIMIT_BYTES));
        let Ok(request) = request else {
            return HookOutcome::Faulted {
                owner,
                fault: HookFault::Unlaunchable,
            };
        };
        let Ok(spec) = self.shell.resolve(request) else {
            return HookOutcome::Faulted {
                owner,
                fault: HookFault::Unlaunchable,
            };
        };
        let observer = cancellation.clone();
        let result = self.shell.execute(spec, cancellation).await;
        let cancellation_observed = observer.is_cancelled();
        match result {
            // The exit is a typed value, so nothing here classifies by parsing
            // an error string — a timeout arrives as `ProcessExit::TimedOut`,
            // not as a message that happens to contain the word.
            Ok(output) => match output.exit() {
                ProcessExit::Exited { code: 0 } => HookOutcome::allowed(),
                // A non-zero exit from a `Pre` hook is a deliberate refusal;
                // from a `Post` hook there is nothing left to refuse, so it is
                // only a fault. The phase decides, not the exit code.
                ProcessExit::Exited { .. } if hook.phase.can_refuse() => {
                    HookOutcome::Refused { owner }
                }
                ProcessExit::Exited { .. } | ProcessExit::Signalled { .. } => {
                    HookOutcome::Faulted {
                        owner,
                        fault: HookFault::Failed,
                    }
                }
                ProcessExit::TimedOut | ProcessExit::InactivityTimedOut => HookOutcome::Faulted {
                    owner,
                    fault: HookFault::TimedOut,
                },
                // An exit shape this build does not recognize is a fault, never
                // an allowance: the safe direction is to not have run.
                _ => HookOutcome::Faulted {
                    owner,
                    fault: HookFault::Failed,
                },
            },
            Err(_) if cancellation_observed => HookOutcome::Faulted {
                owner,
                fault: HookFault::Cancelled,
            },
            // A launch failure carries no hook output into the fault: a hook's
            // stderr is third-party text whenever the workspace is untrusted.
            Err(_) => HookOutcome::Faulted {
                owner,
                fault: HookFault::Unlaunchable,
            },
        }
    }
}

/// Exact ownership handle for one hook contribution.
pub struct HookRegistration {
    registry: std::sync::Weak<Mutex<Vec<Arc<Hook>>>>,
    hook: Arc<Hook>,
    active: bool,
}

impl Drop for HookRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let Ok(mut hooks) = registry.lock() else {
            return;
        };
        hooks.retain(|registered| !Arc::ptr_eq(registered, &self.hook));
    }
}

/// Publish the hook registry.
#[must_use]
pub fn hooks_plugin(trust: WorkspaceTrustDecision) -> Box<dyn heycode_core::Plugin> {
    struct HooksPlugin(WorkspaceTrustDecision);

    impl heycode_core::Plugin for HooksPlugin {
        fn name(&self) -> &'static str {
            "hooks"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_exec::SERVICE_SHELL]
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_HOOKS]
        }
        fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
            let shell = context
                .get::<ShellService>(heycode_exec::SERVICE_SHELL)
                .ok_or_else(|| heycode_core::CoreError::other("shell missing"))?;
            context.provide(SERVICE_HOOKS, self.name(), HookService::new(shell, self.0))?;
            if let Some(gate) = context.get::<HookExecutionGateHandle>(SERVICE_HOOK_EXECUTION_GATE)
            {
                let service = context
                    .get::<HookService>(SERVICE_HOOKS)
                    .ok_or_else(|| heycode_core::CoreError::other("hooks missing"))?;
                service
                    .attach_execution_gate(context, gate.0.clone())
                    .map_err(heycode_core::CoreError::other)?;
                gate.0.attach_service(service);
            }
            Ok(())
        }
    }

    Box::new(HooksPlugin(trust))
}

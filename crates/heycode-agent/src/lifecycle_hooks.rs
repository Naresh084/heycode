//! Dependency-neutral ports for product hook attachment at Agent-owned call sites.

use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use futures::FutureExt as _;
use tokio_util::sync::CancellationToken;

/// Hook phase at an Agent-owned lifecycle point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleHookPhase {
    /// Before the operation commits.
    Pre,
    /// After the operation commits or settles.
    Post,
}

/// Agent-owned lifecycle point available to the O09 product adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleHookEvent {
    /// User prompt admission into durable model history.
    UserPrompt,
    /// Subagent delegation start/settlement.
    Subagent,
}

/// One bounded lifecycle invocation.
#[derive(Clone, PartialEq, Eq)]
pub struct LifecycleHookRequest {
    phase: LifecycleHookPhase,
    event: LifecycleHookEvent,
    payload: String,
}

impl LifecycleHookRequest {
    /// Construct one adapter invocation.
    #[must_use]
    pub fn new(phase: LifecycleHookPhase, event: LifecycleHookEvent, payload: String) -> Self {
        Self {
            phase,
            event,
            payload,
        }
    }

    /// Lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> LifecycleHookPhase {
        self.phase
    }

    /// Lifecycle event.
    #[must_use]
    pub const fn event(&self) -> LifecycleHookEvent {
        self.event
    }

    /// Exact operation payload. Implementations must keep it out of Debug and
    /// diagnostics and apply their own configured hook-input bound.
    #[must_use]
    pub fn payload(&self) -> &str {
        &self.payload
    }
}

impl std::fmt::Debug for LifecycleHookRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LifecycleHookRequest")
            .field("phase", &self.phase)
            .field("event", &self.event)
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

/// Whether the surrounded operation may continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleHookDecision {
    /// Continue after every applicable hook settled.
    Proceed,
    /// One entitled pre-hook deliberately refused.
    Refuse,
}

/// Body-free aggregate of one lifecycle hook run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleHookReport {
    decision: LifecycleHookDecision,
    faults: u32,
}

impl LifecycleHookReport {
    /// Successful pass with no hook faults.
    #[must_use]
    pub const fn proceed() -> Self {
        Self {
            decision: LifecycleHookDecision::Proceed,
            faults: 0,
        }
    }

    /// Deliberate pre-hook refusal with any preceding fault count.
    #[must_use]
    pub const fn refuse(faults: u32) -> Self {
        Self {
            decision: LifecycleHookDecision::Refuse,
            faults,
        }
    }

    /// Proceed while preserving how many handlers faulted or could not commit.
    #[must_use]
    pub const fn proceed_with_faults(faults: u32) -> Self {
        Self {
            decision: LifecycleHookDecision::Proceed,
            faults,
        }
    }

    /// Effective decision.
    #[must_use]
    pub const fn decision(self) -> LifecycleHookDecision {
        self.decision
    }

    /// Number of non-vetoing hook faults.
    #[must_use]
    pub const fn faults(self) -> u32 {
        self.faults
    }
}

/// Product adapter invoked at Agent-owned lifecycle points.
///
/// The adapter owns HookService dispatch and durable contribution commit. It
/// returns no text: the Agent obtains successful contributions only by
/// rebuilding model input from the session log.
#[async_trait]
pub trait LifecycleHookPort: Send + Sync {
    /// Run every matching hook under one caller cancellation token.
    async fn run(
        &self,
        request: LifecycleHookRequest,
        cancellation: CancellationToken,
    ) -> LifecycleHookReport;
}

type HookBinding = (Arc<dyn LifecycleHookPort>, Arc<()>);

/// One effect-ownable lifecycle-hook attachment slot.
#[derive(Clone, Default)]
pub(crate) struct LifecycleHookSlot {
    binding: Arc<Mutex<Option<HookBinding>>>,
}

impl LifecycleHookSlot {
    pub(crate) fn install(
        &self,
        context: &heycode_core::Context,
        port: Arc<dyn LifecycleHookPort>,
    ) -> Result<(), LifecycleHookAttachmentError> {
        let token = Arc::new(());
        let mut binding = self
            .binding
            .lock()
            .map_err(|_| LifecycleHookAttachmentError::Unavailable)?;
        if binding.is_some() {
            return Err(LifecycleHookAttachmentError::AlreadyAttached);
        }
        *binding = Some((port, token.clone()));
        drop(binding);
        let slot: Weak<Mutex<Option<HookBinding>>> = Arc::downgrade(&self.binding);
        context.effect(move || {
            let Some(slot) = slot.upgrade() else {
                return;
            };
            let Ok(mut binding) = slot.lock() else {
                return;
            };
            if binding
                .as_ref()
                .is_some_and(|(_, current)| Arc::ptr_eq(current, &token))
            {
                *binding = None;
            }
        });
        Ok(())
    }

    pub(crate) async fn run(
        &self,
        request: LifecycleHookRequest,
        cancellation: CancellationToken,
    ) -> LifecycleHookReport {
        let port = self
            .binding
            .lock()
            .ok()
            .and_then(|binding| binding.as_ref().map(|(port, _)| Arc::clone(port)));
        let Some(port) = port else {
            return LifecycleHookReport::proceed();
        };
        let invocation =
            std::panic::AssertUnwindSafe(port.run(request, cancellation)).catch_unwind();
        invocation
            .await
            .unwrap_or_else(|_| LifecycleHookReport::proceed_with_faults(1))
    }
}

/// Lifecycle hook adapter attachment failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleHookAttachmentError {
    /// One adapter already owns the slot.
    #[error("lifecycle hook adapter is already attached")]
    AlreadyAttached,
    /// The attachment state is unavailable.
    #[error("lifecycle hook adapter is unavailable")]
    Unavailable,
}

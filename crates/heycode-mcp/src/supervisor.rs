//! Bounded reconnect supervision above a transport.
//!
//! A dead transport must not turn into a spin. One supervisor owns one
//! server's recovery: a single retry task, a single cancellation token and a
//! single attempt budget taken from the definition's [`McpReconnectPolicy`].
//! Attempts are separated by a doubling backoff, the budget is exhaustible and
//! exhaustion is terminal, so a crash loop ends in a classified failure instead
//! of retrying forever.
//!
//! Recovery swaps exactly once. A failed attempt never touches the live tool
//! rows, so the previous generation stays whole while the transport is down;
//! only a successful reconnect reaches [`McpToolGenerationOwner::refresh`],
//! whose all-or-nothing swap advances the registry generation at one commit
//! point. Concurrent `recover()` calls join the episode in flight rather than
//! starting a second one, so a crash storm still produces one generation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::channel::{
    McpChannelError, McpRequestChannel, McpServerHandshake, McpSiblingContributions,
};
use crate::generation::McpToolGenerationOwner;
use crate::registry::{
    McpConnectionPublisher, McpFailureCode, McpGenerationRetention, McpReconnectPolicy,
};

/// One re-established transport ready to serve a fresh connection generation.
pub struct McpConnectionAttempt {
    pub(crate) channel: Arc<dyn McpRequestChannel>,
    pub(crate) handshake: McpServerHandshake,
    pub(crate) siblings: McpSiblingContributions,
}

impl McpConnectionAttempt {
    /// Bind a reconnected channel to the `initialize` result it negotiated.
    ///
    /// The sibling listings default to unwalked, which is correct only for a
    /// server advertising neither resources nor prompts. A transport that
    /// re-handshakes a server advertising either must re-walk them and record
    /// the counts with [`Self::with_siblings`]; otherwise publication refuses
    /// rather than reporting the advertised listing as empty.
    #[must_use]
    pub fn new(channel: Arc<dyn McpRequestChannel>, handshake: McpServerHandshake) -> Self {
        Self {
            channel,
            handshake,
            siblings: McpSiblingContributions::none(),
        }
    }

    /// Record what this attempt's resource and prompt walks found.
    #[must_use]
    pub const fn with_siblings(mut self, siblings: McpSiblingContributions) -> Self {
        self.siblings = siblings;
        self
    }
}

/// Re-establishes one server's transport for the reconnect supervisor.
///
/// Implementations own their own spawn/handshake and retire whatever the
/// previous connection held; the supervisor only decides whether and when to
/// ask for another one.
#[async_trait]
pub trait McpReconnect: Send + Sync {
    /// Re-establish the transport and complete its `initialize` handshake.
    ///
    /// # Errors
    /// Transport, timeout, cancellation, JSON-RPC and protocol failures, all
    /// without server text.
    async fn connect(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<McpConnectionAttempt, McpChannelError>;
}

/// Whether a [`McpReconnectSupervisor::recover`] call started an episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRecoveryAdmission {
    /// A bounded episode started and the supervisor owns its retry task.
    Started,
    /// An episode is already in flight; this call started nothing.
    AlreadyRunning,
    /// The definition's policy forbids automatic reconnect.
    Disabled,
    /// The attempt budget was spent; re-arming needs a new supervisor.
    Exhausted,
    /// The supervisor was disposed or its token cancelled.
    ShutDown,
    /// No Tokio runtime can own the retry task, or the state is unusable.
    Unavailable,
}

/// How one bounded recovery episode ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRecoveryOutcome {
    /// A complete generation replaced the live one at the numbered commit.
    Recovered {
        /// Connect attempts spent in this episode.
        attempts: u32,
        /// Registry generation number published by the swap.
        generation: u64,
    },
    /// Every attempt in the budget failed and the generation was removed.
    Exhausted {
        /// Connect attempts spent in this episode.
        attempts: u32,
    },
    /// Cancellation won before the episode could commit anything.
    Cancelled {
        /// Connect attempts spent before cancellation.
        attempts: u32,
    },
}

/// Terminal and non-terminal supervisor phases.
///
/// `Exhausted` and `ShutDown` are terminal: an outer Consumer that retried a
/// spent supervisor would recreate exactly the crash loop the budget exists to
/// bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Running,
    Exhausted,
    ShutDown,
}

struct SupervisorState {
    phase: Phase,
    attempts: u32,
    handle: Option<tokio::task::JoinHandle<()>>,
    outcome: Option<McpRecoveryOutcome>,
}

struct SupervisorInner {
    owner: Arc<McpToolGenerationOwner>,
    publisher: McpConnectionPublisher,
    connector: Arc<dyn McpReconnect>,
    policy: McpReconnectPolicy,
    token: CancellationToken,
    /// Never held across an `await`; the retry task locks it only to record.
    state: Mutex<SupervisorState>,
}

/// The single lifecycle owner of one server's bounded reconnect attempts.
///
/// Dropping or disposing the supervisor cancels its token and aborts its retry
/// task, so no attempt can outlive the context that owns it.
pub struct McpReconnectSupervisor {
    inner: Arc<SupervisorInner>,
}

impl McpReconnectSupervisor {
    /// Bind one generation owner and its publisher to a reconnect strategy.
    ///
    /// The supervisor's token is a child of `parent`, so cancelling the
    /// owning context's token stops every pending attempt.
    #[must_use]
    pub fn new(
        owner: Arc<McpToolGenerationOwner>,
        publisher: McpConnectionPublisher,
        connector: Arc<dyn McpReconnect>,
        policy: McpReconnectPolicy,
        parent: &CancellationToken,
    ) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                owner,
                publisher,
                connector,
                policy,
                token: parent.child_token(),
                state: Mutex::new(SupervisorState {
                    phase: Phase::Idle,
                    attempts: 0,
                    handle: None,
                    outcome: None,
                }),
            }),
        }
    }

    /// Start one bounded recovery episode, or report why none started.
    ///
    /// Admission is single-flight: while an episode runs, further calls report
    /// [`McpRecoveryAdmission::AlreadyRunning`] and start nothing, so a burst of
    /// failure reports cannot produce a burst of generations.
    pub fn recover(&self) -> McpRecoveryAdmission {
        if !self.inner.policy.enabled() {
            return McpRecoveryAdmission::Disabled;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return McpRecoveryAdmission::Unavailable;
        };
        let Ok(mut state) = self.inner.state.lock() else {
            return McpRecoveryAdmission::Unavailable;
        };
        match state.phase {
            Phase::Running => return McpRecoveryAdmission::AlreadyRunning,
            Phase::Exhausted => return McpRecoveryAdmission::Exhausted,
            Phase::ShutDown => return McpRecoveryAdmission::ShutDown,
            Phase::Idle => {}
        }
        if self.inner.token.is_cancelled() {
            state.phase = Phase::ShutDown;
            return McpRecoveryAdmission::ShutDown;
        }
        state.phase = Phase::Running;
        state.attempts = 0;
        state.outcome = None;
        // A settled episode's handle may still sit here; abort before replacing
        // so no task is ever left without an owner.
        if let Some(previous) = state.handle.take() {
            previous.abort();
        }
        let inner = Arc::clone(&self.inner);
        state.handle = Some(runtime.spawn(async move { inner.run_episode().await }));
        McpRecoveryAdmission::Started
    }

    /// Await the owned retry task and report how the episode ended.
    ///
    /// An aborted task reports [`McpRecoveryOutcome::Cancelled`] with the
    /// attempts it actually spent.
    pub async fn settle(&self) -> Option<McpRecoveryOutcome> {
        let handle = self.inner.state.lock().ok()?.handle.take();
        if let Some(handle) = handle
            && handle.await.is_err()
        {
            let mut state = self.inner.state.lock().ok()?;
            state.phase = Phase::ShutDown;
            if state.outcome.is_none() {
                let attempts = state.attempts;
                state.outcome = Some(McpRecoveryOutcome::Cancelled { attempts });
            }
        }
        self.outcome()
    }

    /// How the last settled episode ended.
    #[must_use]
    pub fn outcome(&self) -> Option<McpRecoveryOutcome> {
        self.inner.state.lock().ok().and_then(|state| state.outcome)
    }

    /// Cancel the token and abort the retry task. The exact disposer.
    ///
    /// Runtime-free, idempotent and non-blocking, because a context disposer
    /// cannot await. The aborted handle stays owned until [`Self::settle`]
    /// takes it.
    pub fn shutdown(&self) {
        self.inner.token.cancel();
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        state.phase = Phase::ShutDown;
        if let Some(handle) = state.handle.as_ref() {
            handle.abort();
        }
    }
}

impl Drop for McpReconnectSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl SupervisorInner {
    async fn run_episode(self: Arc<Self>) {
        let max_attempts = self.policy.max_attempts();
        let mut attempts: u32 = 0;
        let outcome = loop {
            let attempt = attempts.saturating_add(1);
            if !self
                .announce_and_wait(
                    attempt,
                    max_attempts,
                    self.policy.delay_before_attempt(attempt),
                )
                .await
            {
                break McpRecoveryOutcome::Cancelled { attempts };
            }
            attempts = attempt;
            self.note_attempt(attempts);
            match self.attempt_once().await {
                Ok(generation) => {
                    break McpRecoveryOutcome::Recovered {
                        attempts,
                        generation,
                    };
                }
                Err(_) if self.token.is_cancelled() => {
                    break McpRecoveryOutcome::Cancelled { attempts };
                }
                Err(_) => {}
            }
            if attempts >= max_attempts {
                break self.exhaust(attempts).await;
            }
        };
        self.settle_episode(outcome);
    }

    /// Publish the upcoming attempt, then wait out its backoff.
    ///
    /// Returns `false` when cancellation won, in which case nothing was
    /// published: cancellation is not a commit point.
    async fn announce_and_wait(&self, attempt: u32, max_attempts: u32, delay: Duration) -> bool {
        if self.token.is_cancelled() {
            return false;
        }
        let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        let _published = self.publisher.report_reconnecting(
            attempt,
            max_attempts,
            crate::unix_time_ms().saturating_add(delay_ms),
        );
        if delay.is_zero() {
            return !self.token.is_cancelled();
        }
        tokio::select! {
            () = self.token.cancelled() => false,
            () = tokio::time::sleep(delay) => true,
        }
    }

    async fn attempt_once(&self) -> Result<u64, McpChannelError> {
        let attempt = self.connector.connect(&self.token).await?;
        let generation = self
            .owner
            .refresh(
                attempt.channel,
                &attempt.handshake,
                attempt.siblings,
                &self.token,
            )
            .await?;
        Ok(generation.number())
    }

    /// Retire the rows first, then stop claiming the generation.
    ///
    /// `Remove` states that no retained generation remains, so leaving the
    /// last-good rows model-visible would make the snapshot lie about tools
    /// that no live transport can serve.
    async fn exhaust(&self, attempts: u32) -> McpRecoveryOutcome {
        self.owner.retire().await;
        let _published = self.publisher.report_failure(
            McpFailureCode::ReconnectExhausted,
            crate::unix_time_ms(),
            McpGenerationRetention::Remove,
        );
        McpRecoveryOutcome::Exhausted { attempts }
    }

    fn note_attempt(&self, attempts: u32) {
        if let Ok(mut state) = self.state.lock() {
            state.attempts = attempts;
        }
    }

    fn settle_episode(&self, outcome: McpRecoveryOutcome) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.phase = match outcome {
            McpRecoveryOutcome::Recovered { .. } => Phase::Idle,
            McpRecoveryOutcome::Exhausted { .. } => Phase::Exhausted,
            McpRecoveryOutcome::Cancelled { .. } => Phase::ShutDown,
        };
        state.outcome = Some(outcome);
    }
}

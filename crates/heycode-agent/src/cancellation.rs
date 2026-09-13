//! Reusable per-turn cancellation ownership.

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

struct ActiveTurn {
    identity: Arc<()>,
    token: CancellationToken,
}

struct CancellationState {
    shutdown: CancellationToken,
    active: Mutex<Option<ActiveTurn>>,
}

/// Cloneable control handle that cancels only the current turn while keeping
/// later turns usable. Context shutdown is a separate terminal operation.
#[derive(Clone)]
pub struct AgentCancellation {
    state: Arc<CancellationState>,
}

impl AgentCancellation {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(CancellationState {
                shutdown: CancellationToken::new(),
                active: Mutex::new(None),
            }),
        }
    }

    /// Cancel the active turn, if one exists. Cancelling while idle is a no-op
    /// and cannot poison the next turn.
    pub fn cancel(&self) {
        let token = self
            .state
            .active
            .lock()
            .ok()
            .and_then(|active| active.as_ref().map(|turn| turn.token.clone()));
        if let Some(token) = token {
            token.cancel();
        }
    }

    /// Permanently stop this Agent and cancel its active turn. Used only by
    /// owning context teardown.
    pub fn shutdown(&self) {
        self.state.shutdown.cancel();
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.state.shutdown.clone()
    }

    /// Whether the owner shut down or the current turn was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.shutdown.is_cancelled()
            || self.state.active.lock().map_or(true, |active| {
                active
                    .as_ref()
                    .is_some_and(|turn| turn.token.is_cancelled())
            })
    }

    /// Whether a turn currently owns this Agent.
    ///
    /// This is the authoritative liveness fact, not an inference from UI or
    /// spinner state: the lease exists exactly between `begin_turn` and the
    /// drop of its `TurnCancellation`.
    #[must_use]
    pub fn is_turn_active(&self) -> bool {
        self.state
            .active
            .lock()
            .is_ok_and(|active| active.is_some())
    }

    /// Whether context teardown permanently closed the Agent.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.state.shutdown.is_cancelled()
    }

    pub(crate) fn begin_turn(&self) -> Result<TurnCancellation, TurnCancellationError> {
        if self.state.shutdown.is_cancelled() {
            return Err(TurnCancellationError::Shutdown);
        }
        let mut active = self
            .state
            .active
            .lock()
            .map_err(|_| TurnCancellationError::Unavailable)?;
        if active.is_some() {
            return Err(TurnCancellationError::Concurrent);
        }
        let token = self.state.shutdown.child_token();
        let identity = Arc::new(());
        *active = Some(ActiveTurn {
            identity: identity.clone(),
            token: token.clone(),
        });
        Ok(TurnCancellation {
            state: self.state.clone(),
            identity,
            token,
        })
    }
}

pub(crate) struct TurnCancellation {
    state: Arc<CancellationState>,
    identity: Arc<()>,
    token: CancellationToken,
}

impl TurnCancellation {
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for TurnCancellation {
    fn drop(&mut self) {
        let Ok(mut active) = self.state.active.lock() else {
            return;
        };
        let remove = active
            .as_ref()
            .is_some_and(|turn| Arc::ptr_eq(&turn.identity, &self.identity));
        if remove {
            *active = None;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TurnCancellationError {
    #[error("agent is shut down")]
    Shutdown,
    #[error("another agent turn is already active")]
    Concurrent,
    #[error("agent cancellation state is unavailable")]
    Unavailable,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_one_lease_does_not_cancel_the_next_or_idle_state() {
        let cancellation = AgentCancellation::new();
        cancellation.cancel();
        assert!(!cancellation.is_cancelled());
        let first = cancellation.begin_turn().unwrap();
        cancellation.cancel();
        assert!(first.token().is_cancelled());
        drop(first);
        assert!(!cancellation.is_cancelled());
        let second = cancellation.begin_turn().unwrap();
        assert!(!second.token().is_cancelled());
    }

    #[test]
    fn shutdown_cancels_active_and_rejects_future_turns() {
        let cancellation = AgentCancellation::new();
        let active = cancellation.begin_turn().unwrap();
        cancellation.shutdown();
        assert!(active.token().is_cancelled());
        drop(active);
        assert!(cancellation.is_shutdown());
        assert!(matches!(
            cancellation.begin_turn(),
            Err(TurnCancellationError::Shutdown)
        ));
    }
}

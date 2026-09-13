//! C14 execution of explicit opaque-state provider-switch resolutions.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::{Agent, CompactionError, PortableCompaction};

/// Explicit decision when a route crosses opaque native state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaqueStateResolution {
    /// Create a later portable summary before changing route.
    PortableRecompact,
    /// Create a durable child immediately before the native checkpoint.
    ForkBeforeCheckpoint,
    /// Leave session and routing state unchanged.
    Cancel,
}

/// Result of preparing one provider switch.
pub enum ProviderSwitchPreparation {
    /// No opaque barrier remains; the routing owner may commit its selection.
    Ready,
    /// The explicit cancel option made no durable change.
    Cancelled,
    /// A pre-checkpoint child committed; the current session/route remains
    /// unchanged until a session-lifecycle Consumer opens the child.
    Forked {
        /// Durable child session identity.
        session_id: heycode_core::SessionId,
        /// Child directory for the existing resume boundary.
        session_dir: PathBuf,
        /// Exact parent prefix inherited before native settlement.
        fork_event_count: u64,
    },
}

impl std::fmt::Debug for ProviderSwitchPreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready => formatter.write_str("Ready"),
            Self::Cancelled => formatter.write_str("Cancelled"),
            Self::Forked {
                session_id,
                fork_event_count,
                ..
            } => formatter
                .debug_struct("Forked")
                .field("session_id", session_id)
                .field("fork_event_count", fork_event_count)
                .finish_non_exhaustive(),
        }
    }
}

/// Opaque-state provider-switch preparation failure.
#[derive(Debug, thiserror::Error)]
pub enum ProviderSwitchError {
    /// Durable projection is malformed.
    #[error("provider switch compaction projection is invalid")]
    Projection,
    /// Portable compaction could not settle.
    #[error(transparent)]
    Compaction(#[from] CompactionError),
    /// Parent root or shared-prefix child creation failed.
    #[error("provider switch fork could not be created")]
    Fork,
    /// Session state could not be read.
    #[error("provider switch session is unavailable")]
    Unavailable,
    /// Portable settlement did not supersede the native checkpoint.
    #[error("portable compaction did not resolve the opaque checkpoint")]
    StillOpaque,
}

impl Agent {
    /// Inspect whether a target provider/model crosses the winning native
    /// compaction checkpoint.
    ///
    /// # Errors
    /// Malformed durable projection or unavailable session state.
    pub fn provider_switch_barrier(
        &self,
        target_provider: &str,
        target_model: &str,
    ) -> Result<Option<heycode_session::OpaqueCompactionBarrier>, ProviderSwitchError> {
        let session = self
            .session()
            .lock()
            .map_err(|_| ProviderSwitchError::Unavailable)?;
        heycode_session::opaque_compaction_barrier(session.events(), target_provider, target_model)
            .map_err(|_| ProviderSwitchError::Projection)
    }

    /// Execute one explicit opaque-state resolution before a routing owner
    /// commits a provider/model change.
    ///
    /// `Ready` authorizes only the caller's already-validated target; this API
    /// does not mutate routing Settings. `Forked` commits a resumable child but
    /// intentionally leaves the current session and route unchanged.
    ///
    /// # Errors
    /// Projection, compaction, cancellation, root derivation, or fork failure.
    pub async fn prepare_provider_switch(
        &self,
        target_provider: &str,
        target_model: &str,
        resolution: OpaqueStateResolution,
        cancellation: CancellationToken,
    ) -> Result<ProviderSwitchPreparation, ProviderSwitchError> {
        if resolution == OpaqueStateResolution::Cancel {
            return Ok(ProviderSwitchPreparation::Cancelled);
        }
        let Some(_initial) = self.provider_switch_barrier(target_provider, target_model)? else {
            return Ok(ProviderSwitchPreparation::Ready);
        };
        match resolution {
            OpaqueStateResolution::Cancel => Ok(ProviderSwitchPreparation::Cancelled),
            OpaqueStateResolution::PortableRecompact => {
                self.compact(PortableCompaction::ID, 1, cancellation)
                    .await?;
                if self
                    .provider_switch_barrier(target_provider, target_model)?
                    .is_some()
                {
                    return Err(ProviderSwitchError::StillOpaque);
                }
                Ok(ProviderSwitchPreparation::Ready)
            }
            OpaqueStateResolution::ForkBeforeCheckpoint => {
                if cancellation.is_cancelled() {
                    return Err(ProviderSwitchError::Compaction(CompactionError::Cancelled));
                }
                let session = self
                    .session()
                    .lock()
                    .map_err(|_| ProviderSwitchError::Unavailable)?;
                let Some(barrier) = heycode_session::opaque_compaction_barrier(
                    session.events(),
                    target_provider,
                    target_model,
                )
                .map_err(|_| ProviderSwitchError::Projection)?
                else {
                    return Ok(ProviderSwitchPreparation::Ready);
                };
                let session_dir = session.path().parent().ok_or(ProviderSwitchError::Fork)?;
                let sessions_root = session_dir.parent().ok_or(ProviderSwitchError::Fork)?;
                let child = session
                    .fork(
                        sessions_root,
                        heycode_session::ForkBoundary::EventCount(barrier.fork_event_count()),
                    )
                    .map_err(|_| ProviderSwitchError::Fork)?;
                let child_dir = child
                    .path()
                    .parent()
                    .ok_or(ProviderSwitchError::Fork)?
                    .to_path_buf();
                let session_id = child.id().clone();
                drop(child);
                Ok(ProviderSwitchPreparation::Forked {
                    session_id,
                    session_dir: child_dir,
                    fork_event_count: barrier.fork_event_count(),
                })
            }
        }
    }
}

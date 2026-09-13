//! Route-switch barrier derived from durable native compaction state.

use crate::{ProjectionError, SessionEvent, SessionEventKind, project_requests};

/// Exact opaque checkpoint that requires an explicit route-switch decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueCompactionBarrier {
    provider: String,
    model: String,
    protocol: heycode_core::ProviderProtocol,
    replaced_upto_seq: u64,
    settlement_seq: u64,
}

impl OpaqueCompactionBarrier {
    /// Provider that owns the opaque checkpoint.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Canonical model that owns the opaque checkpoint.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Exact wire protocol required to replay the checkpoint.
    #[must_use]
    pub const fn protocol(&self) -> heycode_core::ProviderProtocol {
        self.protocol
    }

    /// Highest event sequence represented by the checkpoint.
    #[must_use]
    pub const fn replaced_upto_seq(&self) -> u64 {
        self.replaced_upto_seq
    }

    /// Native settlement event sequence.
    #[must_use]
    pub const fn settlement_seq(&self) -> u64 {
        self.settlement_seq
    }

    /// Prefix event count for a fork immediately before native settlement.
    #[must_use]
    pub const fn fork_event_count(&self) -> u64 {
        self.settlement_seq
    }
}

/// Decide whether a proposed provider/model route crosses the winning opaque
/// compaction checkpoint.
///
/// Portable settlements never create a barrier. Equal replaced boundaries are
/// last-write-wins, allowing a later portable recompact to supersede a native
/// marker without rewriting history.
///
/// # Errors
/// Invalid target identity or any malformed durable request/compaction event.
pub fn opaque_compaction_barrier(
    events: &[SessionEvent],
    target_provider: &str,
    target_model: &str,
) -> Result<Option<OpaqueCompactionBarrier>, ProjectionError> {
    if target_provider.is_empty() || target_provider.trim() != target_provider {
        return Err(ProjectionError::InvalidRoute { field: "provider" });
    }
    if target_model.is_empty() || target_model.trim() != target_model {
        return Err(ProjectionError::InvalidRoute { field: "model" });
    }
    let _validated = project_requests(events)?;
    let winner = events.iter().fold(None, |best, event| {
        let replaced = match &event.kind {
            SessionEventKind::CompactionApplied {
                replaced_upto_seq, ..
            }
            | SessionEventKind::NativeCompactionApplied {
                replaced_upto_seq, ..
            } => *replaced_upto_seq,
            _ => return best,
        };
        match best {
            Some((best_replaced, _)) if best_replaced > replaced => best,
            _ => Some((replaced, event)),
        }
    });
    let Some((replaced_upto_seq, event)) = winner else {
        return Ok(None);
    };
    let SessionEventKind::NativeCompactionApplied { items, .. } = &event.kind else {
        return Ok(None);
    };
    let Some(route) = items.first() else {
        return Err(ProjectionError::InvalidCompaction { seq: event.seq });
    };
    if route.provider() == target_provider && route.model() == target_model {
        return Ok(None);
    }
    Ok(Some(OpaqueCompactionBarrier {
        provider: route.provider().to_owned(),
        model: route.model().to_owned(),
        protocol: route.protocol(),
        replaced_upto_seq,
        settlement_seq: event.seq,
    }))
}

//! Provider-native compaction transport boundary.
//!
//! The provider owns how an exact resolved call becomes an opaque checkpoint.
//! It does not own durable session mutation: the Agent validates this value and
//! commits the corresponding compaction event at one local commit point.

use std::future::Future;
use std::pin::Pin;

use heycode_core::{ProviderProtocol, ProviderStateItem, TokenUsage};

use crate::ResolvedCall;

/// Maximum opaque state items one native checkpoint may contribute.
const MAX_CHECKPOINT_ITEMS: usize = 256;

/// Future returned by one provider-native compaction operation.
pub type NativeCompactionFuture<'a> = Pin<
    Box<dyn Future<Output = Result<NativeCompactionCheckpoint, NativeCompactionError>> + Send + 'a>,
>;

/// Provider-owned transport operation for one resolved native-compaction call.
pub trait NativeCompactionAdapter: Send + Sync {
    /// Consume one exact resolved call and return its opaque continuation.
    ///
    /// The caller owns `cancellation`. Implementations must settle transport
    /// before returning and must never mutate a heycode session directly.
    fn compact(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> NativeCompactionFuture<'_>;
}

/// Validated provider-owned continuation produced by native compaction.
#[derive(Clone, PartialEq)]
pub struct NativeCompactionCheckpoint {
    provider: String,
    model: String,
    protocol: ProviderProtocol,
    items: Vec<ProviderStateItem>,
    usage: Option<TokenUsage>,
}

impl NativeCompactionCheckpoint {
    /// Admit one nonempty, bounded exact-route checkpoint.
    ///
    /// # Errors
    /// Empty/oversized state, invalid items, an unknown protocol, or mixed
    /// provider/model/protocol identity is rejected.
    pub fn new(
        items: Vec<ProviderStateItem>,
        usage: Option<TokenUsage>,
    ) -> Result<Self, NativeCompactionError> {
        let first = items
            .first()
            .ok_or(NativeCompactionError::InvalidCheckpoint)?;
        if items.len() > MAX_CHECKPOINT_ITEMS
            || first.protocol() == ProviderProtocol::Unknown
            || items.iter().any(|item| {
                item.validate().is_err()
                    || item.provider() != first.provider()
                    || item.model() != first.model()
                    || item.protocol() != first.protocol()
            })
        {
            return Err(NativeCompactionError::InvalidCheckpoint);
        }
        Ok(Self {
            provider: first.provider().to_owned(),
            model: first.model().to_owned(),
            protocol: first.protocol(),
            items,
            usage,
        })
    }

    /// Owning provider id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Canonical model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Exact provider protocol.
    #[must_use]
    pub const fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    /// Ordered opaque continuation items.
    #[must_use]
    pub fn items(&self) -> &[ProviderStateItem] {
        &self.items
    }

    /// Provider-reported usage, when normalized exactly.
    #[must_use]
    pub const fn usage(&self) -> Option<TokenUsage> {
        self.usage
    }

    /// Consume the checkpoint into durable fields.
    #[must_use]
    pub fn into_parts(self) -> (Vec<ProviderStateItem>, Option<TokenUsage>) {
        (self.items, self.usage)
    }
}

impl std::fmt::Debug for NativeCompactionCheckpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeCompactionCheckpoint")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            .field("item_count", &self.items.len())
            .field("has_usage", &self.usage.is_some())
            .finish()
    }
}

/// Stable body-free native-compaction failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NativeCompactionError {
    /// This adapter has no native compaction operation.
    #[error("native compaction is unsupported")]
    Unsupported,
    /// Caller cancellation settled the operation.
    #[error("native compaction was cancelled")]
    Cancelled,
    /// Transport failed without exposing provider or request content.
    #[error("native compaction transport failed")]
    Transport,
    /// Provider rejected an otherwise valid operation.
    #[error("native compaction request was rejected")]
    Rejected,
    /// Returned checkpoint was missing, malformed, oversized, or cross-route.
    #[error("native compaction checkpoint is invalid")]
    InvalidCheckpoint,
}

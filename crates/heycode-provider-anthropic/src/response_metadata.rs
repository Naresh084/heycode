//! Successful-response metadata retained outside provider-controlled text.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::{AnthropicCacheUsage, AnthropicContextEditReport};

const MAX_RETAINED_RESPONSES: usize = 256;

/// Validated provider-specific metadata published only after stream success.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicResponseMetadata {
    context_editing: Option<AnthropicContextEditReport>,
    cache_usage: Option<AnthropicCacheUsage>,
}

impl AnthropicResponseMetadata {
    pub(crate) const fn new(
        context_editing: Option<AnthropicContextEditReport>,
        cache_usage: Option<AnthropicCacheUsage>,
    ) -> Self {
        Self {
            context_editing,
            cache_usage,
        }
    }

    /// Applied context-editing facts when that policy was requested.
    #[must_use]
    pub const fn context_editing(&self) -> Option<&AnthropicContextEditReport> {
        self.context_editing.as_ref()
    }

    /// Detailed prompt-cache usage when those counters were reported.
    #[must_use]
    pub const fn cache_usage(&self) -> Option<AnthropicCacheUsage> {
        self.cache_usage
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.context_editing.is_none() && self.cache_usage.is_none()
    }
}

impl std::fmt::Debug for AnthropicResponseMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicResponseMetadata")
            .field("context_editing", &self.context_editing)
            .field("cache_usage", &self.cache_usage)
            .finish()
    }
}

#[derive(Clone, Default)]
pub(crate) struct ResponseMetadataLedger {
    inner: Arc<Mutex<LedgerState>>,
}

#[derive(Default)]
struct LedgerState {
    order: VecDeque<String>,
    rows: BTreeMap<String, AnthropicResponseMetadata>,
}

impl ResponseMetadataLedger {
    pub(crate) fn insert(&self, response_id: String, metadata: AnthropicResponseMetadata) {
        if metadata.is_empty() {
            return;
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.rows.insert(response_id.clone(), metadata).is_none() {
            state.order.push_back(response_id);
        }
        while state.rows.len() > MAX_RETAINED_RESPONSES {
            let Some(oldest) = state.order.pop_front() else {
                break;
            };
            state.rows.remove(&oldest);
        }
    }

    pub(crate) fn take(&self, response_id: &str) -> Option<AnthropicResponseMetadata> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let row = state.rows.remove(response_id);
        if row.is_some() {
            state.order.retain(|candidate| candidate != response_id);
        }
        row
    }
}

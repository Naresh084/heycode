//! Shared model-specific request budget for admission, compaction and UI.

use serde::{Deserialize, Serialize};

/// Strength of a context measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextConfidence {
    /// All contributors counted exactly.
    Exact,
    /// All contributors counted, with some estimates.
    Estimated,
    /// Missing contributors make the count a lower bound.
    AtLeast,
}

/// Authority for the effective capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextLimitSource {
    /// Active model's published limit.
    Model,
    /// Explicit user limit smaller than the model capacity.
    ConfiguredCap,
    /// Explicit user fallback for an unknown model capacity.
    ConfiguredFallback,
    /// No capacity evidence is available.
    Unknown,
}

/// Current automatic compaction activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextActivity {
    /// Request measured and ready for pressure evaluation.
    Ready,
    /// A compaction operation is in progress.
    Compacting,
    /// The attempted compaction failed.
    Failed,
}

/// One request's budget. This is never cumulative billing usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Exact selected provider.
    pub provider: String,
    /// Exact selected model.
    pub model: String,
    /// Counted input tokens (a floor when confidence is `AtLeast`).
    pub used: u64,
    /// Measurement evidence.
    pub confidence: ContextConfidence,
    /// Effective context capacity; unknown is distinct from zero.
    pub window: Option<u64>,
    /// Capacity evidence.
    pub limit_source: ContextLimitSource,
    /// Reserved response tokens; conservative when the provider has no default.
    pub output_reserve: u64,
    /// Safety allowance for estimation and request growth.
    pub safety_margin: u64,
    /// Maximum input allocation after reserves.
    pub usable_input: Option<u64>,
    /// Input count at which automatic compaction is attempted.
    pub compact_at: Option<u64>,
    /// Whether the owning agent has enabled automatic compaction.
    pub auto_compact: bool,
    /// Compaction lifecycle.
    pub activity: ContextActivity,
    /// Count immediately before the latest successful automatic compaction.
    pub before_compaction: Option<u64>,
    /// Count immediately after the latest successful automatic compaction.
    pub after_compaction: Option<u64>,
    /// True when newly generated content has been added to the request estimate.
    pub projected: bool,
}

impl ContextBudget {
    /// Known pressure warrants one automatic compaction attempt.
    #[must_use]
    pub fn should_compact(&self) -> bool {
        self.auto_compact && self.compact_at.is_some_and(|limit| self.used >= limit)
    }

    /// Even the counted input exceeds the usable budget.
    #[must_use]
    pub fn exceeds_usable_input(&self) -> bool {
        self.usable_input.is_some_and(|limit| self.used > limit)
    }

    /// Remaining input allocation; an upper bound for incomplete measurements.
    #[must_use]
    pub fn remaining(&self) -> Option<u64> {
        self.usable_input
            .map(|limit| limit.saturating_sub(self.used))
    }
}

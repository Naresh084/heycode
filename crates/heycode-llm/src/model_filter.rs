//! Explicit unknown-safe capability and lifecycle catalog filters.

use crate::{CapabilitySupport, CatalogSnapshot, ModelDescriptor, ModelLifecycleStatus};

/// Required evidence for one tri-state model capability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CapabilityFilter {
    /// Do not constrain this capability.
    #[default]
    Any,
    /// Match only explicit support.
    Supported,
    /// Match only explicit lack of support.
    Unsupported,
    /// Match only rows with no trustworthy evidence.
    Unknown,
}

/// Effective lifecycle phase constraint at the query instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelLifecycleFilter {
    /// Include every phase, including retired rows.
    #[default]
    Any,
    /// Include every phase except effectively retired rows.
    Selectable,
    /// Include only stable rows.
    Stable,
    /// Include only preview rows.
    Preview,
    /// Include only deprecated rows that have not reached retirement.
    Deprecated,
    /// Include explicitly retired rows and rows past their deadline.
    Retired,
    /// Include only rows with unknown lifecycle evidence.
    Unknown,
}

/// Model picker constraints for the currently normalized capability set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelFilter {
    /// Tool/function-calling evidence.
    pub tools: CapabilityFilter,
    /// Image-input evidence.
    pub image_input: CapabilityFilter,
    /// Reasoning/thinking evidence.
    pub reasoning: CapabilityFilter,
    /// Effective lifecycle constraint.
    pub lifecycle: ModelLifecycleFilter,
}

impl ModelFilter {
    /// Unconstrained filter, including retired rows.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// No capability constraint, excluding effectively retired rows.
    #[must_use]
    pub fn selectable() -> Self {
        Self {
            lifecycle: ModelLifecycleFilter::Selectable,
            ..Self::default()
        }
    }
}

impl CatalogSnapshot {
    /// Borrow model rows matching every filter in deterministic catalog order.
    /// Lifecycle constraints are evaluated at explicit Unix milliseconds.
    #[must_use]
    pub fn filter_models<'a>(
        &'a self,
        filter: &ModelFilter,
        at_ms: u64,
    ) -> Vec<&'a ModelDescriptor> {
        self.models
            .iter()
            .filter(|model| {
                capability_matches(filter.tools, model.capabilities.tools)
                    && capability_matches(filter.image_input, model.capabilities.image_input)
                    && capability_matches(filter.reasoning, model.capabilities.reasoning)
                    && lifecycle_matches(filter.lifecycle, model.lifecycle.effective_status(at_ms))
            })
            .collect()
    }
}

fn capability_matches(filter: CapabilityFilter, support: CapabilitySupport) -> bool {
    match filter {
        CapabilityFilter::Any => true,
        CapabilityFilter::Supported => support == CapabilitySupport::Supported,
        CapabilityFilter::Unsupported => support == CapabilitySupport::Unsupported,
        CapabilityFilter::Unknown => support == CapabilitySupport::Unknown,
    }
}

fn lifecycle_matches(filter: ModelLifecycleFilter, status: ModelLifecycleStatus) -> bool {
    match filter {
        ModelLifecycleFilter::Any => true,
        ModelLifecycleFilter::Selectable => status != ModelLifecycleStatus::Retired,
        ModelLifecycleFilter::Stable => status == ModelLifecycleStatus::Stable,
        ModelLifecycleFilter::Preview => status == ModelLifecycleStatus::Preview,
        ModelLifecycleFilter::Deprecated => status == ModelLifecycleStatus::Deprecated,
        ModelLifecycleFilter::Retired => status == ModelLifecycleStatus::Retired,
        ModelLifecycleFilter::Unknown => status == ModelLifecycleStatus::Unknown,
    }
}

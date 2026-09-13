//! Provider/model descriptors and conservative capability semantics.

use heycode_core::ProviderProtocol;

use crate::{ModelPerformance, ModelPricing};

/// Tri-state capability evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySupport {
    /// Provider/model explicitly supports the capability.
    Supported,
    /// Provider/model explicitly does not support the capability.
    Unsupported,
    /// No trustworthy evidence is available yet.
    Unknown,
}

impl CapabilitySupport {
    /// Preserve unknown rather than coercing it to false.
    #[must_use]
    pub const fn as_bool(self) -> Option<bool> {
        match self {
            Self::Supported => Some(true),
            Self::Unsupported => Some(false),
            Self::Unknown => None,
        }
    }

    /// True only for explicit support.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Model capability snapshot. Every field is explicit tri-state evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// Function/tool calling.
    pub tools: CapabilitySupport,
    /// Reasoning/thinking state.
    pub reasoning: CapabilitySupport,
    /// Image input.
    pub image_input: CapabilitySupport,
    /// Native document/file input.
    pub document_input: CapabilitySupport,
    /// Schema-constrained structured output.
    pub structured_output: CapabilitySupport,
    /// Provider-hosted web search/fetch.
    pub native_web: CapabilitySupport,
    /// Provider-native compaction/context editing.
    pub native_compaction: CapabilitySupport,
    /// Prompt-prefix caching.
    pub prompt_cache: CapabilitySupport,
}

impl ModelCapabilities {
    /// Conservative snapshot for an undiscovered model.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            tools: CapabilitySupport::Unknown,
            reasoning: CapabilitySupport::Unknown,
            image_input: CapabilitySupport::Unknown,
            document_input: CapabilitySupport::Unknown,
            structured_output: CapabilitySupport::Unknown,
            native_web: CapabilitySupport::Unknown,
            native_compaction: CapabilitySupport::Unknown,
            prompt_cache: CapabilitySupport::Unknown,
        }
    }
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self::unknown()
    }
}

/// Provider-evidenced lifecycle phase for a model id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelLifecycleStatus {
    /// The source did not publish trustworthy lifecycle evidence.
    Unknown,
    /// Generally available and not known to be retiring.
    Stable,
    /// Preview/experimental availability with weaker continuity guarantees.
    Preview,
    /// Still selectable, but replacement should be planned.
    Deprecated,
    /// No longer selectable for a new request.
    Retired,
}

/// Lifecycle evidence and provider-recommended replacement ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLifecycle {
    /// Provider-evidenced phase.
    pub status: ModelLifecycleStatus,
    /// Exact retirement instant in Unix milliseconds when known.
    pub retirement_at_ms: Option<u64>,
    /// Provider-recommended replacement ids, in provider preference order.
    pub replacement_ids: Vec<String>,
}

impl ModelLifecycle {
    /// No trustworthy lifecycle evidence.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            status: ModelLifecycleStatus::Unknown,
            retirement_at_ms: None,
            replacement_ids: Vec::new(),
        }
    }

    /// Stable lifecycle evidence.
    #[must_use]
    pub fn stable() -> Self {
        Self {
            status: ModelLifecycleStatus::Stable,
            retirement_at_ms: None,
            replacement_ids: Vec::new(),
        }
    }

    /// Preview lifecycle evidence.
    #[must_use]
    pub fn preview() -> Self {
        Self {
            status: ModelLifecycleStatus::Preview,
            retirement_at_ms: None,
            replacement_ids: Vec::new(),
        }
    }

    /// Deprecated lifecycle evidence with an optional retirement instant and
    /// ordered provider recommendations.
    #[must_use]
    pub fn deprecated(retirement_at_ms: Option<u64>, replacement_ids: Vec<String>) -> Self {
        Self {
            status: ModelLifecycleStatus::Deprecated,
            retirement_at_ms,
            replacement_ids,
        }
    }

    /// Retired lifecycle evidence with an optional historical instant and
    /// ordered provider recommendations.
    #[must_use]
    pub fn retired(retirement_at_ms: Option<u64>, replacement_ids: Vec<String>) -> Self {
        Self {
            status: ModelLifecycleStatus::Retired,
            retirement_at_ms,
            replacement_ids,
        }
    }

    /// Lifecycle phase effective at the explicit Unix-millisecond instant.
    /// A published retirement deadline is authoritative even if the cached
    /// phase has not refreshed from Deprecated to Retired yet.
    #[must_use]
    pub fn effective_status(&self, at_ms: u64) -> ModelLifecycleStatus {
        if self.status == ModelLifecycleStatus::Retired
            || self
                .retirement_at_ms
                .is_some_and(|deadline| at_ms >= deadline)
        {
            ModelLifecycleStatus::Retired
        } else {
            self.status
        }
    }

    /// Whether this lifecycle permits selecting the model at `at_ms`.
    #[must_use]
    pub fn is_selectable(&self, at_ms: u64) -> bool {
        self.effective_status(at_ms) != ModelLifecycleStatus::Retired
    }
}

impl Default for ModelLifecycle {
    fn default() -> Self {
        Self::unknown()
    }
}

/// Why a source's published reasoning metadata cannot be retained verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ModelReasoningError {
    /// An effort id is blank, padded, overlong or control-bearing.
    #[error("published reasoning effort id is blank, padded, overlong or control-bearing")]
    InvalidEffort,
    /// The published vocabulary repeats an effort id.
    #[error("published reasoning vocabulary repeats an effort id")]
    DuplicateEffort,
    /// The published default is outside the published vocabulary.
    #[error("published reasoning default is not one of the published efforts")]
    DefaultOutsideVocabulary,
    /// A default effort was published without the vocabulary it belongs to.
    #[error("published reasoning default has no published effort vocabulary")]
    DefaultWithoutVocabulary,
}

/// Reasoning evidence a source published for one exact model.
///
/// Effort ids are retained in published order and are never renamed, sorted,
/// case-folded or merged with another route's vocabulary. An empty vocabulary
/// means the source published reasoning state without naming the efforts it
/// accepts; that is unknown, never the union of some other model's values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReasoningMetadata {
    efforts: Vec<String>,
    default_effort: Option<String>,
    default_enabled: Option<bool>,
    mandatory: Option<bool>,
}

impl ModelReasoningMetadata {
    /// Retain one source-published reasoning block verbatim.
    ///
    /// # Errors
    /// Malformed, duplicated or internally contradictory published metadata
    /// fails loudly instead of being trimmed into a usable shape.
    pub fn published(
        efforts: Vec<String>,
        default_effort: Option<String>,
        default_enabled: Option<bool>,
        mandatory: Option<bool>,
    ) -> Result<Self, ModelReasoningError> {
        let mut seen = std::collections::BTreeSet::new();
        for effort in &efforts {
            if !safe_effort(effort) {
                return Err(ModelReasoningError::InvalidEffort);
            }
            if !seen.insert(effort.as_str()) {
                return Err(ModelReasoningError::DuplicateEffort);
            }
        }
        if let Some(default) = default_effort.as_deref() {
            if !safe_effort(default) {
                return Err(ModelReasoningError::InvalidEffort);
            }
            if efforts.is_empty() {
                return Err(ModelReasoningError::DefaultWithoutVocabulary);
            }
            if !seen.contains(default) {
                return Err(ModelReasoningError::DefaultOutsideVocabulary);
            }
        }
        Ok(Self {
            efforts,
            default_effort,
            default_enabled,
            mandatory,
        })
    }

    /// Published effort ids in published presentation order. Empty when the
    /// source named no vocabulary for this model.
    #[must_use]
    pub fn efforts(&self) -> &[String] {
        &self.efforts
    }

    /// Published default effort, when the source named one.
    #[must_use]
    pub fn default_effort(&self) -> Option<&str> {
        self.default_effort.as_deref()
    }

    /// Whether the source reports reasoning on by default for this model.
    #[must_use]
    pub const fn default_enabled(&self) -> Option<bool> {
        self.default_enabled
    }

    /// Whether the source reports reasoning as impossible to disable.
    #[must_use]
    pub const fn mandatory(&self) -> Option<bool> {
        self.mandatory
    }

    /// Whether this model's own effort vocabulary is known.
    #[must_use]
    pub const fn names_efforts(&self) -> bool {
        !self.efforts.is_empty()
    }
}

fn safe_effort(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= 128
        && !value.chars().any(char::is_control)
}

/// Safe provider catalog identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    /// Stable provider id.
    pub id: String,
    /// Human display name.
    pub display_name: String,
    /// Supported/unknown protocol families.
    pub protocols: Vec<ProviderProtocol>,
}

/// Safe model catalog row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDescriptor {
    /// Provider-native model id.
    pub id: String,
    /// Human display name.
    pub display_name: String,
    /// Provider-recognized aliases that resolve to this canonical id.
    pub aliases: Vec<String>,
    /// Provider-published creation time in Unix milliseconds, when known.
    pub created_at_ms: Option<u64>,
    /// Input context window when known.
    pub context_window: Option<u64>,
    /// Maximum output tokens when known.
    pub max_output_tokens: Option<u64>,
    /// Availability/deprecation/retirement evidence.
    pub lifecycle: ModelLifecycle,
    /// Explicit tri-state capability snapshot.
    pub capabilities: ModelCapabilities,
    /// Source-published reasoning evidence for this exact model, retained
    /// verbatim. `None` means the source published none; it never licenses
    /// another model's or route's effort vocabulary.
    pub reasoning: Option<ModelReasoningMetadata>,
    /// Normalized published pricing evidence. Empty when the provider
    /// publishes no prices; an absent component is unknown, never free.
    pub pricing: ModelPricing,
    /// Advisory observed performance evidence. Never a guarantee and never an
    /// input to capability, lifecycle or request resolution.
    pub performance: ModelPerformance,
}

impl ModelDescriptor {
    /// Conservative descriptor for an id absent from a live catalog.
    #[must_use]
    pub fn unknown(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            display_name: id.clone(),
            id,
            aliases: Vec::new(),
            created_at_ms: None,
            context_window: None,
            max_output_tokens: None,
            lifecycle: ModelLifecycle::unknown(),
            capabilities: ModelCapabilities::unknown(),
            reasoning: None,
            pricing: ModelPricing::unknown(),
            performance: ModelPerformance::unknown(),
        }
    }
}

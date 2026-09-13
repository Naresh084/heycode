//! Explicit schema-v1 wire mapping independent from runtime Rust layout.

use heycode_llm::{
    CapabilitySupport, CatalogSnapshot, ModelCapabilities, ModelDescriptor, ModelLifecycle,
    ModelLifecycleStatus, ModelMetadataProvenance, ModelPerformance, ModelPricing,
    ModelReasoningMetadata, PriceComponent, PriceCurrency, ProviderDescriptor, ProviderProtocol,
    TokenPrice, TokenPriceUnit,
};
use serde::{Deserialize, Serialize};

use crate::FILE_SCHEMA_VERSION;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireDocument {
    pub(crate) schema_version: u32,
    pub(crate) generations: Vec<WireSnapshot>,
}

impl WireDocument {
    pub(crate) fn from_snapshots(
        snapshots: &[std::sync::Arc<CatalogSnapshot>],
    ) -> Result<Self, String> {
        let mut generations: Vec<_> = snapshots
            .iter()
            .map(|snapshot| WireSnapshot::from_snapshot(snapshot.as_ref()))
            .collect::<Result<_, _>>()?;
        generations.sort_by(|left, right| left.provider.id.cmp(&right.provider.id));
        Ok(Self {
            schema_version: FILE_SCHEMA_VERSION,
            generations,
        })
    }

    /// Convert a decoded document back into runtime generations.
    ///
    /// # Errors
    /// An unknown currency, unit or price component fails rather than being
    /// downgraded to unknown, so a newer file can never be read as a model with
    /// silently missing price evidence.
    pub(crate) fn into_snapshots(self) -> Result<Vec<CatalogSnapshot>, String> {
        let schema_version = self.schema_version;
        self.generations
            .into_iter()
            .map(|snapshot| snapshot.into_snapshot(schema_version))
            .collect()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireSnapshot {
    provider: WireProvider,
    models: Vec<WireModel>,
    revision: u64,
    fetched_at_ms: u64,
}

impl WireSnapshot {
    fn from_snapshot(snapshot: &CatalogSnapshot) -> Result<Self, String> {
        let mut models: Vec<_> = snapshot.models.iter().map(WireModel::from).collect();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self {
            provider: WireProvider::from_descriptor(&snapshot.provider)?,
            models,
            revision: snapshot.revision,
            fetched_at_ms: snapshot.fetched_at_ms,
        })
    }

    fn into_snapshot(self, schema_version: u32) -> Result<CatalogSnapshot, String> {
        Ok(CatalogSnapshot {
            provider: self.provider.into_descriptor(),
            models: self
                .models
                .into_iter()
                .map(|model| model.into_descriptor(schema_version))
                .collect::<Result<_, _>>()?,
            revision: self.revision,
            fetched_at_ms: self.fetched_at_ms,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProvider {
    id: String,
    display_name: String,
    protocols: Vec<WireProtocol>,
}

impl WireProvider {
    fn from_descriptor(provider: &ProviderDescriptor) -> Result<Self, String> {
        Ok(Self {
            id: provider.id.clone(),
            display_name: provider.display_name.clone(),
            protocols: provider
                .protocols
                .iter()
                .map(WireProtocol::from_protocol)
                .collect::<Result<_, _>>()?,
        })
    }

    fn into_descriptor(self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: self.id,
            display_name: self.display_name,
            protocols: self
                .protocols
                .into_iter()
                .map(WireProtocol::into_protocol)
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireModel {
    id: String,
    display_name: String,
    aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at_ms: Option<u64>,
    context_window: Option<u64>,
    max_output_tokens: Option<u64>,
    lifecycle: WireLifecycle,
    capabilities: WireCapabilities,
    /// Source-published reasoning evidence, retained verbatim. Absent when the
    /// source published none, which never licenses a route-wide vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning: Option<WireReasoning>,
    /// Absent in a generation whose provider published no price evidence.
    /// Serialized only when present so an unknown price is never written as a
    /// zero that a reader could mistake for free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pricing: Option<WirePricing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    performance: Option<WirePerformance>,
}

impl From<&ModelDescriptor> for WireModel {
    fn from(model: &ModelDescriptor) -> Self {
        Self {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            aliases: model.aliases.clone(),
            created_at_ms: model.created_at_ms,
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            lifecycle: WireLifecycle::from(&model.lifecycle),
            capabilities: WireCapabilities::from(&model.capabilities),
            reasoning: model.reasoning.as_ref().map(WireReasoning::from),
            pricing: WirePricing::from_pricing(&model.pricing),
            performance: WirePerformance::from_performance(&model.performance),
        }
    }
}

impl WireModel {
    fn into_descriptor(self, schema_version: u32) -> Result<ModelDescriptor, String> {
        Ok(ModelDescriptor {
            id: self.id,
            display_name: self.display_name,
            aliases: self.aliases,
            created_at_ms: self.created_at_ms,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            lifecycle: self.lifecycle.into_lifecycle(),
            capabilities: self.capabilities.into_capabilities(),
            pricing: match self.pricing {
                Some(pricing) => pricing.into_pricing(schema_version)?,
                None => ModelPricing::unknown(),
            },
            performance: match self.performance {
                Some(performance) => performance.into_performance(schema_version)?,
                None => ModelPerformance::unknown(),
            },
            reasoning: self
                .reasoning
                .map(WireReasoning::into_reasoning)
                .transpose()?,
        })
    }
}

/// Published reasoning evidence for one model, retained in published order.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReasoning {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mandatory: Option<bool>,
}

impl From<&ModelReasoningMetadata> for WireReasoning {
    fn from(reasoning: &ModelReasoningMetadata) -> Self {
        Self {
            efforts: reasoning.efforts().to_vec(),
            default_effort: reasoning.default_effort().map(str::to_owned),
            default_enabled: reasoning.default_enabled(),
            mandatory: reasoning.mandatory(),
        }
    }
}

impl WireReasoning {
    /// Rebuild published reasoning evidence, failing rather than repairing a
    /// file whose vocabulary and default disagree.
    fn into_reasoning(self) -> Result<ModelReasoningMetadata, String> {
        ModelReasoningMetadata::published(
            self.efforts,
            self.default_effort,
            self.default_enabled,
            self.mandatory,
        )
        .map_err(|error| error.to_string())
    }
}

/// Published token prices, retained in the exact currency and unit they were
/// fetched in rather than converted.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePricing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<WireProvenance>,
    currency: String,
    unit: String,
    /// Component name to exact pico-unit amount, in stable component order.
    components: Vec<WirePriceComponent>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePriceComponent {
    component: String,
    pico_units: u64,
}

impl WirePricing {
    fn from_pricing(pricing: &ModelPricing) -> Option<Self> {
        let currency = pricing.currency()?;
        let unit = pricing.unit()?;
        Some(Self {
            provenance: Some(WireProvenance::from_runtime(pricing.provenance()?)),
            currency: currency.code().to_owned(),
            unit: unit.name().to_owned(),
            components: pricing
                .components()
                .map(|(component, price)| WirePriceComponent {
                    component: component.name().to_owned(),
                    pico_units: price.pico_units(),
                })
                .collect(),
        })
    }

    fn into_pricing(self, schema_version: u32) -> Result<ModelPricing, String> {
        // An unrecognized currency, unit or component is a newer file, and
        // reading it as "no price" would silently misreport cost.
        let currency = PriceCurrency::parse(&self.currency)
            .map_err(|_| format!("unsupported price currency `{}`", self.currency))?;
        let unit = TokenPriceUnit::parse(&self.unit)
            .map_err(|_| format!("unsupported price unit `{}`", self.unit))?;
        let provenance = match (schema_version, self.provenance) {
            (1, _) => ModelMetadataProvenance::new("catalog-cache:v1-validation", 1)
                .map_err(|_| "invalid legacy price provenance".to_owned())?,
            (_, Some(provenance)) => provenance.into_runtime()?,
            (_, None) => return Err("model pricing has no provenance".to_owned()),
        };
        let mut components = self.components.into_iter();
        let Some(first) = components.next() else {
            return Err("model pricing has no components".to_owned());
        };
        let first_component = PriceComponent::parse(&first.component)
            .map_err(|_| format!("unsupported price component `{}`", first.component))?;
        let first_price = TokenPrice::from_pico_units(currency, unit, first.pico_units)
            .map_err(|_| "price amount is out of range".to_owned())?;
        let mut pricing = ModelPricing::captured(provenance, first_component, first_price);
        for row in components {
            let component = PriceComponent::parse(&row.component)
                .map_err(|_| format!("unsupported price component `{}`", row.component))?;
            let price = TokenPrice::from_pico_units(currency, unit, row.pico_units)
                .map_err(|_| "price amount is out of range".to_owned())?;
            pricing = pricing
                .with(component, price)
                .map_err(|_| format!("duplicate price component `{}`", component.name()))?;
        }
        if schema_version == 1 {
            Ok(ModelPricing::unknown())
        } else {
            Ok(pricing)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProvenance {
    source: String,
    captured_at_ms: u64,
}

impl WireProvenance {
    fn from_runtime(provenance: &ModelMetadataProvenance) -> Self {
        Self {
            source: provenance.source().to_owned(),
            captured_at_ms: provenance.captured_at_ms(),
        }
    }

    fn into_runtime(self) -> Result<ModelMetadataProvenance, String> {
        ModelMetadataProvenance::new(self.source, self.captured_at_ms)
            .map_err(|_| "invalid model metadata provenance".to_owned())
    }
}

/// Advisory observed performance. Never affects resolution.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePerformance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<WireProvenance>,
    /// Schema-v1 compatibility field. Schema v2 writes provenance instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_token_latency_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_throughput_milli_tokens_per_second: Option<u32>,
}

impl WirePerformance {
    fn from_performance(performance: &ModelPerformance) -> Option<Self> {
        Some(Self {
            provenance: Some(WireProvenance::from_runtime(performance.provenance()?)),
            observed_at_ms: None,
            first_token_latency_ms: performance.first_token_latency_ms(),
            output_throughput_milli_tokens_per_second: performance
                .output_throughput_milli_tokens_per_second(),
        })
    }

    fn into_performance(self, schema_version: u32) -> Result<ModelPerformance, String> {
        let provenance = match (schema_version, self.provenance, self.observed_at_ms) {
            (1, _, Some(observed_at_ms)) => {
                ModelMetadataProvenance::new("catalog-cache:v1-validation", observed_at_ms)
                    .map_err(|_| "invalid legacy performance provenance".to_owned())?
            }
            (1, _, None) => return Err("legacy performance has no observation instant".to_owned()),
            (_, Some(provenance), None) => provenance.into_runtime()?,
            (_, _, _) => return Err("model performance has invalid provenance".to_owned()),
        };
        let performance = ModelPerformance::observed(
            provenance,
            self.first_token_latency_ms,
            self.output_throughput_milli_tokens_per_second,
        )
        .map_err(|_| "invalid observed model performance".to_owned())?;
        if schema_version == 1 {
            Ok(ModelPerformance::unknown())
        } else {
            Ok(performance)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLifecycle {
    status: WireLifecycleStatus,
    retirement_at_ms: Option<u64>,
    replacement_ids: Vec<String>,
}

impl From<&ModelLifecycle> for WireLifecycle {
    fn from(lifecycle: &ModelLifecycle) -> Self {
        Self {
            status: WireLifecycleStatus::from(lifecycle.status),
            retirement_at_ms: lifecycle.retirement_at_ms,
            replacement_ids: lifecycle.replacement_ids.clone(),
        }
    }
}

impl WireLifecycle {
    fn into_lifecycle(self) -> ModelLifecycle {
        ModelLifecycle {
            status: self.status.into_status(),
            retirement_at_ms: self.retirement_at_ms,
            replacement_ids: self.replacement_ids,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCapabilities {
    tools: WireSupport,
    reasoning: WireSupport,
    image_input: WireSupport,
    #[serde(default = "unknown_support")]
    document_input: WireSupport,
    structured_output: WireSupport,
    native_web: WireSupport,
    native_compaction: WireSupport,
    prompt_cache: WireSupport,
}

impl From<&ModelCapabilities> for WireCapabilities {
    fn from(capabilities: &ModelCapabilities) -> Self {
        Self {
            tools: WireSupport::from(capabilities.tools),
            reasoning: WireSupport::from(capabilities.reasoning),
            image_input: WireSupport::from(capabilities.image_input),
            document_input: WireSupport::from(capabilities.document_input),
            structured_output: WireSupport::from(capabilities.structured_output),
            native_web: WireSupport::from(capabilities.native_web),
            native_compaction: WireSupport::from(capabilities.native_compaction),
            prompt_cache: WireSupport::from(capabilities.prompt_cache),
        }
    }
}

impl WireCapabilities {
    fn into_capabilities(self) -> ModelCapabilities {
        ModelCapabilities {
            tools: self.tools.into_support(),
            reasoning: self.reasoning.into_support(),
            image_input: self.image_input.into_support(),
            document_input: self.document_input.into_support(),
            structured_output: self.structured_output.into_support(),
            native_web: self.native_web.into_support(),
            native_compaction: self.native_compaction.into_support(),
            prompt_cache: self.prompt_cache.into_support(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireSupport {
    Supported,
    Unsupported,
    Unknown,
}

const fn unknown_support() -> WireSupport {
    WireSupport::Unknown
}

impl From<CapabilitySupport> for WireSupport {
    fn from(support: CapabilitySupport) -> Self {
        match support {
            CapabilitySupport::Supported => Self::Supported,
            CapabilitySupport::Unsupported => Self::Unsupported,
            CapabilitySupport::Unknown => Self::Unknown,
        }
    }
}

impl WireSupport {
    fn into_support(self) -> CapabilitySupport {
        match self {
            Self::Supported => CapabilitySupport::Supported,
            Self::Unsupported => CapabilitySupport::Unsupported,
            Self::Unknown => CapabilitySupport::Unknown,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireLifecycleStatus {
    Unknown,
    Stable,
    Preview,
    Deprecated,
    Retired,
}

impl From<ModelLifecycleStatus> for WireLifecycleStatus {
    fn from(status: ModelLifecycleStatus) -> Self {
        match status {
            ModelLifecycleStatus::Unknown => Self::Unknown,
            ModelLifecycleStatus::Stable => Self::Stable,
            ModelLifecycleStatus::Preview => Self::Preview,
            ModelLifecycleStatus::Deprecated => Self::Deprecated,
            ModelLifecycleStatus::Retired => Self::Retired,
        }
    }
}

impl WireLifecycleStatus {
    fn into_status(self) -> ModelLifecycleStatus {
        match self {
            Self::Unknown => ModelLifecycleStatus::Unknown,
            Self::Stable => ModelLifecycleStatus::Stable,
            Self::Preview => ModelLifecycleStatus::Preview,
            Self::Deprecated => ModelLifecycleStatus::Deprecated,
            Self::Retired => ModelLifecycleStatus::Retired,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireProtocol {
    Unknown,
    OpenAiChatCompletions,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerateContent,
    BedrockConverse,
    DelegatedAgent,
}

impl WireProtocol {
    fn from_protocol(protocol: &ProviderProtocol) -> Result<Self, String> {
        match protocol {
            ProviderProtocol::Unknown => Ok(Self::Unknown),
            ProviderProtocol::OpenAiChatCompletions => Ok(Self::OpenAiChatCompletions),
            ProviderProtocol::OpenAiResponses => Ok(Self::OpenAiResponses),
            ProviderProtocol::AnthropicMessages => Ok(Self::AnthropicMessages),
            ProviderProtocol::GeminiGenerateContent => Ok(Self::GeminiGenerateContent),
            ProviderProtocol::BedrockConverse => Ok(Self::BedrockConverse),
            ProviderProtocol::DelegatedAgent => Ok(Self::DelegatedAgent),
            _ => Err("provider protocol is unsupported by catalog cache schema v1".to_owned()),
        }
    }

    fn into_protocol(self) -> ProviderProtocol {
        match self {
            Self::Unknown => ProviderProtocol::Unknown,
            Self::OpenAiChatCompletions => ProviderProtocol::OpenAiChatCompletions,
            Self::OpenAiResponses => ProviderProtocol::OpenAiResponses,
            Self::AnthropicMessages => ProviderProtocol::AnthropicMessages,
            Self::GeminiGenerateContent => ProviderProtocol::GeminiGenerateContent,
            Self::BedrockConverse => ProviderProtocol::BedrockConverse,
            Self::DelegatedAgent => ProviderProtocol::DelegatedAgent,
        }
    }
}

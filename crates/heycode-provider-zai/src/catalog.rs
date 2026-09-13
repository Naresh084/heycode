//! Maintained GLM catalog normalized from Z.ai's published API evidence.
//!
//! Z.ai publishes **no model-list endpoint**: its official OpenAPI document
//! declares `/paas/v4/chat/completions` and eleven siblings, and none of them
//! lists models. This source is therefore *maintained* rather than live, and
//! its evidence is that same OpenAPI document — a machine-readable primary
//! source rather than a prose table.
//!
//! Three rules keep the rows honest:
//!
//! - **Limits** come from the specification's own bounds. `max_tokens`
//!   declares `maximum: 131072`, which fixes `K` at 1024 for every per-family
//!   output cap it states. No context window appears anywhere in the
//!   specification, so a context window is recorded only where Z.ai states an
//!   exact number in its own configuration guidance, and is `None` otherwise —
//!   a rounded "200K" does not decide between 200,000 and 204,800.
//! - **Capabilities** are `Supported` or `Unsupported` only where Z.ai states
//!   the restriction; a field the specification simply omits stays `Unknown`
//!   and is never promoted.
//! - **Retirement** is unannounced, which is not the same as absent. Z.ai
//!   publishes no shutdown date, deprecation notice or replacement list for
//!   any model, so every row is `ModelLifecycle::unknown()` and none claims
//!   `Stable`.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ProviderProtocol,
};
use heycode_llm::{
    CapabilitySupport, CatalogFetchError, CatalogRegistry, ModelCapabilities, ModelCatalog,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, ProviderDescriptor,
    SERVICE_MODELS,
};
use tokio_util::sync::CancellationToken;

use crate::{ZaiPlan, ZaiPlanKind};

/// Which request schema a model is callable through.
///
/// Z.ai splits `POST /paas/v4/chat/completions` into a `Text Model` and a
/// `Vision Model` request schema. That split is the restriction: image input
/// and `response_format` follow directly from which schema names the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZaiModality {
    /// Listed in the `ChatCompletionTextRequest` model enum.
    Text,
    /// Listed in the `ChatCompletionVisionRequest` model enum.
    Vision,
}

/// One documented model row, transcribed from the OpenAPI specification.
struct ZaiModelFacts {
    id: &'static str,
    display_name: &'static str,
    modality: ZaiModality,
    max_output_tokens: u64,
    context_window: Option<u64>,
    tools: CapabilitySupport,
    reasoning: CapabilitySupport,
}

use CapabilitySupport::{Supported, Unknown, Unsupported};
use ZaiModality::{Text, Vision};

/// Exact one-million context window Z.ai instructs tools to configure.
///
/// Sources: <https://docs.z.ai/devpack/latest-model> ("Set Context Window Size
/// to 1000000" for GLM-5.3 and GLM-5.3-Flash) and
/// <https://docs.z.ai/devpack/tool/others> ("`glm-5.2` is `1000000`"). Decimal,
/// not 2^20 — Z.ai writes the digits out.
const ZAI_ONE_MILLION_CONTEXT: u64 = 1_000_000;

/// Every model Z.ai's chat-completions schema names, in specification order.
///
/// Output caps come from the `max_tokens` descriptions: the GLM-5.3/5.2/5.1/5,
/// GLM-4.7 and GLM-4.6 series cap at 128K, the GLM-4.5 series at 96K,
/// GLM-4-32B-0414-128K at 16K, GLM-5.3-Flash at 128K, the GLM-4.6V series at
/// 32K, the GLM-4.5V series at 16K and AutoGLM-Phone-Multilingual at 4K.
const ZAI_MODELS: &[ZaiModelFacts] = &[
    text("glm-5.3", "GLM-5.3", 131_072, Some(ZAI_ONE_MILLION_CONTEXT)),
    text("glm-5.2", "GLM-5.2", 131_072, Some(ZAI_ONE_MILLION_CONTEXT)),
    text("glm-5.1", "GLM-5.1", 131_072, None),
    text("glm-5", "GLM-5", 131_072, None),
    text("glm-4.7", "GLM-4.7", 131_072, None),
    text("glm-4.7-flash", "GLM-4.7-Flash", 131_072, None),
    text("glm-4.7-flashx", "GLM-4.7-FlashX", 131_072, None),
    text("glm-4.6", "GLM-4.6", 131_072, None),
    text("glm-4.5", "GLM-4.5", 98_304, None),
    text("glm-4.5-air", "GLM-4.5-Air", 98_304, None),
    text("glm-4.5-x", "GLM-4.5-X", 98_304, None),
    text("glm-4.5-airx", "GLM-4.5-AirX", 98_304, None),
    text("glm-4.5-flash", "GLM-4.5-Flash", 98_304, None),
    // `thinking` is "Only supported by GLM-4.5 series and higher models", and
    // this 0414 release is below that line — a stated exclusion, not a gap.
    ZaiModelFacts {
        id: "glm-4-32b-0414-128k",
        display_name: "GLM-4-32B-0414-128K",
        modality: Text,
        max_output_tokens: 16_384,
        context_window: None,
        tools: Supported,
        reasoning: Unsupported,
    },
    vision(
        "glm-5.3-flash",
        "GLM-5.3-Flash",
        131_072,
        Some(ZAI_ONE_MILLION_CONTEXT),
        Supported,
        Supported,
    ),
    vision("glm-4.6v", "GLM-4.6V", 32_768, None, Supported, Supported),
    // Vision tools are "Only supported by GLM-5.3-Flash, the GLM-4.6V series,
    // and autoglm-phone-multilingual". This model is outside the GLM-4.5-and-
    // higher numbering that `thinking` names, so its reasoning is unstated.
    vision(
        "autoglm-phone-multilingual",
        "AutoGLM-Phone-Multilingual",
        4_096,
        None,
        Supported,
        Unknown,
    ),
    vision(
        "glm-4.6v-flash",
        "GLM-4.6V-Flash",
        32_768,
        None,
        Supported,
        Supported,
    ),
    vision(
        "glm-4.6v-flashx",
        "GLM-4.6V-FlashX",
        32_768,
        None,
        Supported,
        Supported,
    ),
    // Excluded from the list of vision models that accept `tools`, while
    // `thinking` says GLM-4.5V "will think compulsorily".
    vision("glm-4.5v", "GLM-4.5V", 16_384, None, Unsupported, Supported),
];

const fn text(
    id: &'static str,
    display_name: &'static str,
    max_output_tokens: u64,
    context_window: Option<u64>,
) -> ZaiModelFacts {
    ZaiModelFacts {
        id,
        display_name,
        modality: Text,
        max_output_tokens,
        context_window,
        tools: Supported,
        reasoning: Supported,
    }
}

const fn vision(
    id: &'static str,
    display_name: &'static str,
    max_output_tokens: u64,
    context_window: Option<u64>,
    tools: CapabilitySupport,
    reasoning: CapabilitySupport,
) -> ZaiModelFacts {
    ZaiModelFacts {
        id,
        display_name,
        modality: Vision,
        max_output_tokens,
        context_window,
        tools,
        reasoning,
    }
}

/// Maintained model catalog for one Z.ai plan.
///
/// The rows are identical for both plans on purpose: Z.ai publishes one
/// chat-completions model schema and no per-plan model list, so a plan changes
/// the endpoint and the entitlement, not the published model facts. Omitting a
/// model from one plan's generation would assert it is unavailable there, and
/// that is a claim Z.ai's documentation does not support.
pub struct ZaiCatalog<P: ZaiPlanKind> {
    plan: std::marker::PhantomData<P>,
}

impl<P: ZaiPlanKind> Default for ZaiCatalog<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: ZaiPlanKind> ZaiCatalog<P> {
    /// A source over the documented Z.ai model set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            plan: std::marker::PhantomData,
        }
    }

    /// Plan whose registry name this source publishes under.
    #[must_use]
    pub fn plan(&self) -> ZaiPlan {
        P::PLAN
    }

    /// Number of documented rows this source publishes.
    #[must_use]
    pub fn model_count(&self) -> usize {
        ZAI_MODELS.len()
    }
}

#[async_trait]
impl<P: ZaiPlanKind + Send + Sync + 'static> ModelCatalog for ZaiCatalog<P> {
    fn provider(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: P::REGISTRY_NAME.to_owned(),
            display_name: P::DISPLAY_NAME.to_owned(),
            // The rows come from the Chat Completions schema, so that is the
            // protocol this evidence describes.
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        }
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        Ok(ZAI_MODELS.iter().map(descriptor).collect())
    }
}

/// The maintained row for one documented model id.
///
/// `None` for an id Z.ai publishes no evidence for. A caller that needs a
/// placeholder builds a conservative unknown descriptor itself rather than
/// receiving an invented row from here.
pub(crate) fn zai_model_descriptor(id: &str) -> Option<ModelDescriptor> {
    ZAI_MODELS
        .iter()
        .find(|facts| facts.id == id)
        .map(descriptor)
}

fn descriptor(facts: &ZaiModelFacts) -> ModelDescriptor {
    ModelDescriptor {
        id: facts.id.to_owned(),
        display_name: facts.display_name.to_owned(),
        // Z.ai publishes no alias field. `glm-5.3-flash[1m]` is a Claude Code
        // configuration form, not a catalog alias.
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: facts.context_window,
        max_output_tokens: Some(facts.max_output_tokens),
        // Unannounced, not absent: no Z.ai page publishes a shutdown date,
        // deprecation notice or replacement id for any model.
        lifecycle: ModelLifecycle::unknown(),
        capabilities: capabilities(facts),
        // Z.ai publishes prices on a documentation page, not through any API
        // endpoint. A docs table is not API evidence, so no price is recorded
        // rather than a transcribed one presented as published data.
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn capabilities(facts: &ZaiModelFacts) -> ModelCapabilities {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = facts.tools;
    capabilities.reasoning = facts.reasoning;
    // The Text/Vision request split is the stated restriction for both of
    // these: only the vision schema takes image content, and `response_format`
    // is documented as "Only text models support this field".
    match facts.modality {
        Text => {
            capabilities.image_input = Unsupported;
            capabilities.structured_output = Supported;
        }
        Vision => {
            capabilities.image_input = Supported;
            capabilities.structured_output = Unsupported;
        }
    }
    // document_input, native_web, native_compaction and prompt_cache have no
    // per-model evidence in the specification and stay Unknown. The text tool
    // union does admit a provider-hosted `web_search` tool, but that is
    // endpoint evidence rather than a per-model claim; PZA04 owns it.
    capabilities
}

/// Register this plan's maintained catalog into the shared model registry.
#[must_use]
pub fn zai_catalog_plugin<P: ZaiPlanKind + Send + Sync + 'static>() -> Box<dyn Plugin> {
    struct ZaiCatalogPlugin<P: ZaiPlanKind>(std::marker::PhantomData<P>);

    impl<P: ZaiPlanKind + Send + Sync + 'static> Plugin for ZaiCatalogPlugin<P> {
        fn name(&self) -> &'static str {
            P::CATALOG_PLUGIN_NAME
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            vec![PluginContributionSpec::new(
                ContributionKind::ModelCatalog,
                P::REGISTRY_NAME,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MODELS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            models
                .register(context, Arc::new(ZaiCatalog::<P>::new()))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(ZaiCatalogPlugin::<P>(std::marker::PhantomData))
}

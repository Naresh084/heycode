//! Provider-owned maintained Vertex model catalogs.
//!
//! These sources answer only "which exact model metadata does the provider
//! currently publish?" They do not inspect ADC, call Model Garden, or claim
//! that the configured project has access. The shared catalog registry labels
//! a successful source execution as live;
//! [`MaintainedVertexCatalog::account_access`] remains the separate account
//! verdict and is always Unknown for this source class.
//!
//! Google Cloud's `publishers.models.list` is a Model Garden directory, not an
//! account-entitlement API. Claude on Google Cloud explicitly does not expose
//! the Models API. A credentialed discovery implementation would therefore
//! fabricate a stronger claim for both products, so the maintained exact rows
//! are the complete honest source until an official access API exists.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization_gcp::GcpHealth;
use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginContributionSpec, PluginDescriptor,
    ServiceKey,
};
use heycode_llm::{
    CapabilitySupport, CatalogFetchError, CatalogRegistry, ModelCapabilities, ModelCatalog,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, ProviderDescriptor,
    ProviderProtocol, SERVICE_MODELS,
};
use tokio_util::sync::CancellationToken;

use crate::{GOOGLE_GEMINI_3_7_FLASH, GOOGLE_VERTEX_PROVIDER};

/// Primary model card for the maintained Vertex Gemini row.
pub const GOOGLE_VERTEX_MODEL_SOURCE: &str =
    "https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-7-flash";

/// Primary model card for the maintained Claude-on-Vertex row.
pub const GOOGLE_CLAUDE_VERTEX_MODEL_SOURCE: &str = "https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/partner-models/claude/sonnet-5";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaintainedVertexProduct {
    Gemini,
    Claude,
}

/// One credential-blind maintained model source for a Vertex product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintainedVertexCatalog {
    product: MaintainedVertexProduct,
}

impl MaintainedVertexCatalog {
    /// Maintained Gemini 3.7 Flash facts for the Vertex Gemini route.
    #[must_use]
    pub const fn vertex_gemini() -> Self {
        Self {
            product: MaintainedVertexProduct::Gemini,
        }
    }

    /// Maintained Claude Sonnet 5 facts for the Claude-on-Vertex route.
    #[must_use]
    pub const fn claude_vertex() -> Self {
        Self {
            product: MaintainedVertexProduct::Claude,
        }
    }

    /// Official primary source for this exact maintained row.
    #[must_use]
    pub const fn source_url(self) -> &'static str {
        match self.product {
            MaintainedVertexProduct::Gemini => GOOGLE_VERTEX_MODEL_SOURCE,
            MaintainedVertexProduct::Claude => GOOGLE_CLAUDE_VERTEX_MODEL_SOURCE,
        }
    }

    /// Account access evidence held by this credential-blind source.
    #[must_use]
    pub const fn account_access(self) -> GcpHealth {
        GcpHealth::Unknown
    }

    fn provider_descriptor(self) -> ProviderDescriptor {
        match self.product {
            MaintainedVertexProduct::Gemini => vertex_gemini_provider_descriptor(),
            MaintainedVertexProduct::Claude => crate::claude_vertex::provider_descriptor(),
        }
    }

    fn model_descriptor(self) -> ModelDescriptor {
        match self.product {
            MaintainedVertexProduct::Gemini => vertex_gemini_model_descriptor(),
            MaintainedVertexProduct::Claude => crate::claude_vertex::sonnet_five_descriptor(),
        }
    }
}

#[async_trait]
impl ModelCatalog for MaintainedVertexCatalog {
    fn provider(&self) -> ProviderDescriptor {
        self.provider_descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        Ok(vec![self.model_descriptor()])
    }
}

pub(crate) fn vertex_gemini_provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: GOOGLE_VERTEX_PROVIDER.to_owned(),
        display_name: "Vertex Gemini".to_owned(),
        protocols: vec![ProviderProtocol::GeminiGenerateContent],
    }
}

pub(crate) fn vertex_gemini_model_descriptor() -> ModelDescriptor {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    capabilities.reasoning = CapabilitySupport::Supported;
    capabilities.image_input = CapabilitySupport::Supported;
    capabilities.structured_output = CapabilitySupport::Supported;
    capabilities.native_web = CapabilitySupport::Supported;
    capabilities.prompt_cache = CapabilitySupport::Supported;
    ModelDescriptor {
        id: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
        display_name: "Gemini 3.7 Flash".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(65_536),
        lifecycle: ModelLifecycle::stable(),
        capabilities,
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn maintained_catalog_plugin(
    name: &'static str,
    source: MaintainedVertexCatalog,
) -> Box<dyn Plugin> {
    struct MaintainedVertexCatalogPlugin {
        name: &'static str,
        source: MaintainedVertexCatalog,
    }

    impl Plugin for MaintainedVertexCatalogPlugin {
        fn name(&self) -> &'static str {
            self.name
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
                heycode_core::ContributionKind::ModelCatalog,
                self.source.provider_descriptor().id,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            &[SERVICE_MODELS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            models
                .register(context, Arc::new(self.source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(MaintainedVertexCatalogPlugin { name, source })
}

/// Register the maintained Vertex Gemini catalog as a Context effect.
#[must_use]
pub fn maintained_vertex_gemini_catalog_plugin() -> Box<dyn Plugin> {
    maintained_catalog_plugin(
        "catalog-google-vertex",
        MaintainedVertexCatalog::vertex_gemini(),
    )
}

/// Register the maintained Claude-on-Vertex catalog as a Context effect.
#[must_use]
pub fn maintained_claude_vertex_catalog_plugin() -> Box<dyn Plugin> {
    maintained_catalog_plugin(
        "catalog-google-claude-vertex",
        MaintainedVertexCatalog::claude_vertex(),
    )
}

//! OpenRouter product profile and authorization contribution.

use std::sync::Arc;

use heycode_authorization::{
    AuthorizationFlowFailure, AuthorizationService, SERVICE_AUTHORIZATION,
};
use heycode_authorization_api_key::{
    ApiKeyAuthorizationFlow, ApiKeyFlowConfig, ApiKeyValidator, HttpApiKeyValidator, SecretPrompt,
};
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::CredentialQuery;

mod catalog;
mod request_transforms;
mod transforms;

pub use catalog::{
    OPENROUTER_GLM_5_3_FLASH, OpenRouterCatalog, OpenRouterCatalogConfig, openrouter_catalog_plugin,
};
pub use request_transforms::{OpenRouterRequestTransforms, openrouter_request_transforms_plugin};
pub use transforms::{
    OPENROUTER_TRANSFORM_OPTION_KIND, OPENROUTER_TRANSFORM_WIRE_FIELD, OpenRouterPagePrice,
    OpenRouterPdfEngine, OpenRouterTransform, OpenRouterTransformActivation,
    OpenRouterTransformCost, OpenRouterTransformEffect, OpenRouterTransformPolicy,
    OpenRouterTransformPolicyError, OpenRouterTransformRequest, OpenRouterTransformRequestContext,
};

/// Stable OpenRouter authorization-flow id.
pub const OPENROUTER_FLOW_ID: &str = "openrouter-api-key";
/// Exact provider-native implementation id for OpenRouter web search.
pub const OPENROUTER_WEB_SEARCH_IMPLEMENTATION: &str = "openrouter:web_search";

/// Provider-owned OpenRouter authorization plugin configuration.
#[derive(Clone)]
pub struct OpenRouterPluginConfig {
    query: CredentialQuery,
    prompt: Arc<dyn SecretPrompt>,
    validator: Arc<dyn ApiKeyValidator>,
}

impl OpenRouterPluginConfig {
    /// Build from one exact credential query and replaceable interaction/
    /// validation providers.
    #[must_use]
    pub fn new(
        query: CredentialQuery,
        prompt: Arc<dyn SecretPrompt>,
        validator: Arc<dyn ApiKeyValidator>,
    ) -> Self {
        Self {
            query,
            prompt,
            validator,
        }
    }

    /// Build the official OpenRouter validation plan.
    ///
    /// # Errors
    /// Fixed endpoint/client construction failures retain the safe host class.
    pub fn official(
        query: CredentialQuery,
        prompt: Arc<dyn SecretPrompt>,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        Ok(Self::new(
            query,
            prompt,
            Arc::new(HttpApiKeyValidator::openrouter(required_model)?),
        ))
    }

    /// [`Self::official`] whose validation probe goes to `base_url`
    /// (`llm.base_url`: a proxy or gateway) instead of the official host.
    ///
    /// # Errors
    /// Endpoint/client construction failures retain the safe host class.
    pub fn at_base_url(
        query: CredentialQuery,
        prompt: Arc<dyn SecretPrompt>,
        base_url: &str,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        Ok(Self::new(
            query,
            prompt,
            Arc::new(HttpApiKeyValidator::openrouter_at(
                base_url,
                required_model,
            )?),
        ))
    }

    fn flow(&self) -> Result<ApiKeyAuthorizationFlow, CoreError> {
        let id = heycode_authorization::AuthorizationFlowId::new(OPENROUTER_FLOW_ID)
            .map_err(|error| CoreError::other(error.to_string()))?;
        Ok(ApiKeyAuthorizationFlow::new(
            ApiKeyFlowConfig {
                id,
                label: "OpenRouter API key".to_owned(),
                query: self.query.clone(),
                prompt: "Paste your OpenRouter API key".to_owned(),
            },
            self.prompt.clone(),
            self.validator.clone(),
        ))
    }
}

/// Safe OpenRouter identity/default metadata shared by setup and routing.
#[must_use]
pub fn openrouter_profile() -> heycode_llm::ProviderProfile {
    heycode_llm::OpenRouterProvider::setup_profile()
}

/// Register OpenRouter's provider-executed web-search candidate.
#[must_use]
pub fn openrouter_native_tools_plugin() -> Box<dyn Plugin> {
    struct OpenRouterNativeToolsPlugin;

    impl Plugin for OpenRouterNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-openrouter"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::NativeTool,
                OPENROUTER_WEB_SEARCH_IMPLEMENTATION,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_native_tools::SERVICE_NATIVE_TOOLS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = context
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| CoreError::other("native-tools service type mismatch"))?;
            let implementation = heycode_native_tools::NativeToolImplementation::new(
                "web_search",
                OPENROUTER_WEB_SEARCH_IMPLEMENTATION,
                heycode_core::NativeToolImplementationKind::Provider,
                Some(heycode_llm::OpenRouterProvider::NAME.to_owned()),
                100,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            registry
                .register(context, implementation)
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(OpenRouterNativeToolsPlugin)
}

/// Register the provider-owned OpenRouter authorization contribution.
#[must_use]
pub fn openrouter_plugin(config: OpenRouterPluginConfig) -> Box<dyn Plugin> {
    struct OpenRouterPlugin(OpenRouterPluginConfig);

    impl Plugin for OpenRouterPlugin {
        fn name(&self) -> &'static str {
            "provider-openrouter"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::AuthorizationFlow,
                OPENROUTER_FLOW_ID,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AUTHORIZATION]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let authorization = context
                .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
                .ok_or_else(|| CoreError::MissingService(SERVICE_AUTHORIZATION.to_string()))?;
            authorization
                .register(context, Arc::new(self.0.flow()?))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(OpenRouterPlugin(config))
}

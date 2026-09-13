//! N05 OpenRouter request-transform registry contribution.

use std::sync::Arc;

use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_llm::{
    RequestDraft, RequestTransformCost, RequestTransformDescriptor, RequestTransformEffect,
    RequestTransformError, RequestTransformId, RequestTransformProvider, RequestTransformRegistry,
    RequestTransformRequest,
};

use crate::{
    OpenRouterTransform, OpenRouterTransformCost, OpenRouterTransformEffect,
    OpenRouterTransformPolicy, OpenRouterTransformRequest, OpenRouterTransformRequestContext,
};

/// Provider-owned complete OpenRouter request transform policy.
#[derive(Debug, Clone, Copy)]
pub struct OpenRouterRequestTransforms {
    policy: OpenRouterTransformPolicy,
}

impl OpenRouterRequestTransforms {
    /// Bind one explicit OpenRouter policy.
    #[must_use]
    pub const fn new(policy: OpenRouterTransformPolicy) -> Self {
        Self { policy }
    }
}

impl RequestTransformProvider for OpenRouterRequestTransforms {
    fn provider(&self) -> &str {
        heycode_llm::OpenRouterProvider::NAME
    }

    fn descriptors(&self) -> Result<Vec<RequestTransformDescriptor>, RequestTransformError> {
        self.policy
            .activations()
            .into_iter()
            .map(|activation| {
                let transform = activation.transform();
                RequestTransformDescriptor::new(
                    RequestTransformId::new(format!(
                        "{}:{}",
                        heycode_llm::OpenRouterProvider::NAME,
                        transform.id()
                    ))?,
                    heycode_llm::OpenRouterProvider::NAME,
                    effect(transform.effect()),
                    request(activation.request()),
                    activation.effective(),
                    activation.cost().map(cost).transpose()?,
                )
            })
            .collect()
    }

    fn provider_option(
        &self,
        draft: &RequestDraft,
    ) -> Result<heycode_core::ProviderRequestOption, RequestTransformError> {
        if draft.provider != heycode_llm::OpenRouterProvider::NAME {
            return Err(RequestTransformError::InvalidProviderOption);
        }
        self.policy
            .provider_option(OpenRouterTransformRequestContext::new(
                true,
                draft.structured_output.is_some(),
            ))
            .map_err(|_| RequestTransformError::IncompatibleRequest)
    }
}

fn effect(value: OpenRouterTransformEffect) -> RequestTransformEffect {
    match value {
        OpenRouterTransformEffect::RewritesPromptAndRoute => {
            RequestTransformEffect::RewritePromptAndRoute
        }
        OpenRouterTransformEffect::RewritesResponse => RequestTransformEffect::RewriteResponse,
        OpenRouterTransformEffect::ParsesDocuments => RequestTransformEffect::ParseDocuments,
    }
}

fn request(value: OpenRouterTransformRequest) -> RequestTransformRequest {
    match value {
        OpenRouterTransformRequest::Disabled => RequestTransformRequest::Disabled,
        OpenRouterTransformRequest::Enabled => RequestTransformRequest::Enabled,
    }
}

fn cost(value: OpenRouterTransformCost) -> Result<RequestTransformCost, RequestTransformError> {
    match value {
        OpenRouterTransformCost::Unknown => Ok(RequestTransformCost::Unknown),
        OpenRouterTransformCost::DocumentedFree => Ok(RequestTransformCost::DocumentedFree),
        OpenRouterTransformCost::UpstreamInputTokens => {
            Ok(RequestTransformCost::UpstreamInputTokens)
        }
        OpenRouterTransformCost::PerThousandPages(price) => {
            RequestTransformCost::published_per_thousand_pages(
                price.currency(),
                price.pico_units_per_thousand_pages(),
            )
        }
    }
}

/// Register OpenRouter's complete transform policy into the N05 registry.
#[must_use]
pub fn openrouter_request_transforms_plugin(policy: OpenRouterTransformPolicy) -> Box<dyn Plugin> {
    struct OpenRouterTransformsPlugin(OpenRouterTransformPolicy);

    impl Plugin for OpenRouterTransformsPlugin {
        fn name(&self) -> &'static str {
            "request-transforms-openrouter"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            OpenRouterTransform::ALL
                .into_iter()
                .map(|transform| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::RequestTransform,
                        format!(
                            "{}:{}",
                            heycode_llm::OpenRouterProvider::NAME,
                            transform.id()
                        ),
                    )
                })
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_llm::SERVICE_REQUEST_TRANSFORMS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = context
                .get::<RequestTransformRegistry>(heycode_llm::SERVICE_REQUEST_TRANSFORMS)
                .ok_or_else(|| CoreError::other("request transform registry missing"))?;
            registry
                .register(context, Arc::new(OpenRouterRequestTransforms::new(self.0)))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(OpenRouterTransformsPlugin(policy))
}

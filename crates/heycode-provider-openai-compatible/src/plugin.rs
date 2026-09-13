//! Effect-owned activation of one saved custom OpenAI-compatible route.

use std::sync::Arc;

use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ServiceKey,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpService, SERVICE_HTTP};
use heycode_llm::{Provider, ProviderRegistry, RouteCredential, SERVICE_PROVIDERS};

use crate::{
    CUSTOM_OPENAI_PROVIDER, CustomOpenAiEndpoint, CustomOpenAiModel, CustomOpenAiProvider,
};

/// Safe inference contribution configuration failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CustomOpenAiInferencePluginError {
    /// An optional bearer binding used a semantic kind other than api-key.
    #[error("custom server bearer credential must be an api-key")]
    InvalidCredentialKind,
}

/// Complete saved custom-server route.
#[derive(Clone)]
pub struct CustomOpenAiInferencePluginConfig {
    endpoint: CustomOpenAiEndpoint,
    model: CustomOpenAiModel,
    credential: Option<CredentialQuery>,
}

impl CustomOpenAiInferencePluginConfig {
    /// Bind one selected model and optional operation-time bearer credential.
    ///
    /// # Errors
    /// A credential kind other than `api-key` is refused.
    pub fn new(
        endpoint: CustomOpenAiEndpoint,
        model: CustomOpenAiModel,
        credential: Option<CredentialQuery>,
    ) -> Result<Self, CustomOpenAiInferencePluginError> {
        if credential
            .as_ref()
            .is_some_and(|credential| credential.kind.as_str() != "api-key")
        {
            return Err(CustomOpenAiInferencePluginError::InvalidCredentialKind);
        }
        Ok(Self {
            endpoint,
            model,
            credential,
        })
    }
}

/// Register the active route in the shared provider registry.
#[must_use]
pub fn custom_openai_inference_plugin(
    config: CustomOpenAiInferencePluginConfig,
) -> Box<dyn Plugin> {
    struct CustomInferencePlugin(CustomOpenAiInferencePluginConfig);

    impl Plugin for CustomInferencePlugin {
        fn name(&self) -> &'static str {
            "inference-custom-openai"
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
                ContributionKind::InferenceProvider,
                CUSTOM_OPENAI_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            if self.0.credential.is_some() {
                &[SERVICE_PROVIDERS, SERVICE_HTTP, SERVICE_CREDENTIALS]
            } else {
                &[SERVICE_PROVIDERS, SERVICE_HTTP]
            }
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let providers = context
                .get::<ProviderRegistry>(SERVICE_PROVIDERS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_PROVIDERS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credential = match self.0.credential.as_ref() {
                Some(query) => {
                    let credentials = context
                        .get::<CredentialsService>(SERVICE_CREDENTIALS)
                        .ok_or_else(|| {
                            CoreError::MissingService(SERVICE_CREDENTIALS.to_string())
                        })?;
                    Some(RouteCredential::registry(
                        credentials.as_ref().clone(),
                        query.clone(),
                    ))
                }
                None => None,
            };
            let provider: Arc<dyn Provider> = Arc::new(
                CustomOpenAiProvider::new(
                    http.as_ref().clone(),
                    self.0.endpoint.clone(),
                    self.0.model.clone(),
                    credential,
                )
                .map_err(|_| CoreError::other("custom OpenAI-compatible route is invalid"))?,
            );
            let registration = providers
                .register_owned(provider)
                .map_err(CoreError::DuplicatePlugin)?;
            context.effect(move || drop(registration));
            Ok(())
        }
    }

    Box::new(CustomInferencePlugin(config))
}

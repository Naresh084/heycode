//! Effect-owned activation of one saved Azure OpenAI deployment.

use std::sync::Arc;

use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ServiceKey,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpService, SERVICE_HTTP};
use heycode_llm::{Provider, ProviderRegistry, RouteCredential, SERVICE_PROVIDERS};

use crate::{AZURE_OPENAI_PROVIDER, AzureDeploymentName, AzureOpenAiProvider, AzureResourceName};

/// Safe Azure inference contribution configuration failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AzureInferencePluginError {
    /// Azure v1 API-key route was bound to a different semantic credential.
    #[error("Azure OpenAI inference requires an api-key credential query")]
    InvalidCredentialKind,
}

/// Complete saved coordinates and credential route for one Azure deployment.
#[derive(Clone)]
pub struct AzureInferencePluginConfig {
    resource: AzureResourceName,
    deployment: AzureDeploymentName,
    credential: CredentialQuery,
}

impl AzureInferencePluginConfig {
    /// Bind one API-key-authenticated Azure v1 route.
    ///
    /// # Errors
    /// A credential query with a kind other than `api-key` is refused.
    pub fn api_key(
        resource: AzureResourceName,
        deployment: AzureDeploymentName,
        credential: CredentialQuery,
    ) -> Result<Self, AzureInferencePluginError> {
        if credential.kind.as_str() != "api-key" {
            return Err(AzureInferencePluginError::InvalidCredentialKind);
        }
        Ok(Self {
            resource,
            deployment,
            credential,
        })
    }
}

/// Register the exact active Azure OpenAI inference route as a Context effect.
#[must_use]
pub fn azure_openai_inference_plugin(config: AzureInferencePluginConfig) -> Box<dyn Plugin> {
    struct AzureInferencePlugin(AzureInferencePluginConfig);

    impl Plugin for AzureInferencePlugin {
        fn name(&self) -> &'static str {
            "inference-azure-openai"
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
                AZURE_OPENAI_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            &[SERVICE_PROVIDERS, SERVICE_HTTP, SERVICE_CREDENTIALS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let providers = context
                .get::<ProviderRegistry>(SERVICE_PROVIDERS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_PROVIDERS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            let provider: Arc<dyn Provider> = Arc::new(
                AzureOpenAiProvider::new(
                    http.as_ref().clone(),
                    self.0.resource.clone(),
                    self.0.deployment.clone(),
                    self.0.credential.reference.as_str(),
                    RouteCredential::registry(
                        credentials.as_ref().clone(),
                        self.0.credential.clone(),
                    ),
                )
                .map_err(|_| CoreError::other("Azure OpenAI inference route is invalid"))?,
            );
            let registration = providers
                .register_owned(provider)
                .map_err(CoreError::DuplicatePlugin)?;
            context.effect(move || drop(registration));
            Ok(())
        }
    }

    Box::new(AzureInferencePlugin(config))
}

use crate::{CompatibleCatalog, CompatibleSpec};
use heycode_authorization::{AuthorizationFlowFailure, AuthorizationFlowId};
use heycode_authorization_api_key::{
    ApiKeyAuthorizationFlow, ApiKeyFlowConfig, HttpApiKeyValidator, SecretPrompt,
};
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ServiceKey,
};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialsService, SERVICE_CREDENTIALS,
};
use heycode_http::{HttpService, SERVICE_HTTP};
use heycode_llm::{CatalogRegistry, RouteCredential, SERVICE_MODELS};
use std::sync::Arc;

fn query(reference: &str) -> Result<CredentialQuery, CoreError> {
    Ok(CredentialQuery::new(
        CredentialReference::new(reference)
            .map_err(|_| CoreError::other("invalid compatible credential reference"))?,
        CredentialKind::new("api-key").map_err(|_| CoreError::other("invalid credential kind"))?,
    ))
}

/// Construct provider-owned masked authorization flows against explicit catalog endpoints.
///
/// # Errors
/// Invalid credential references, flow identities or endpoints fail before registration.
pub fn compatible_authorization_flows(
    spec: CompatibleSpec,
    models_url: &str,
    reference: &str,
    prompt: Arc<dyn SecretPrompt>,
) -> Result<Vec<ApiKeyAuthorizationFlow>, AuthorizationFlowFailure> {
    let failure = || {
        AuthorizationFlowFailure::new(
            "configuration",
            "compatible authorization configuration is invalid",
        )
    };
    Ok(vec![ApiKeyAuthorizationFlow::new(
        ApiKeyFlowConfig {
            id: AuthorizationFlowId::new(format!("{}-api-key", spec.id)).map_err(|_| failure())?,
            label: format!("{} API key", spec.name),
            query: query(reference).map_err(|_| failure())?,
            prompt: format!("Paste your {} API key", spec.name),
        },
        prompt,
        Arc::new(HttpApiKeyValidator::new(models_url.into(), None, None)?),
    )])
}

/// Register a group of exact compatible catalog bindings as one lifecycle effect owner.
#[must_use]
pub fn compatible_catalog_plugin(
    bindings: Vec<(CompatibleSpec, String, String)>,
) -> Box<dyn Plugin> {
    struct CatalogPlugin(Vec<(CompatibleSpec, String, String)>);
    impl Plugin for CatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-compatible"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }
        fn inventory(&self) -> Vec<PluginContributionSpec> {
            self.0
                .iter()
                .map(|(spec, _, _)| {
                    PluginContributionSpec::new(ContributionKind::ModelCatalog, spec.id)
                })
                .collect()
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[SERVICE_MODELS, SERVICE_HTTP, SERVICE_CREDENTIALS]
        }
        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            for (spec, url, reference) in &self.0 {
                let source = CompatibleCatalog::new(
                    *spec,
                    http.as_ref().clone(),
                    url,
                    RouteCredential::registry(credentials.as_ref().clone(), query(reference)?),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
                models
                    .register(context, Arc::new(source))
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            Ok(())
        }
    }
    Box::new(CatalogPlugin(bindings))
}

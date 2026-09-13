use std::sync::Arc;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, ServiceKey, compose};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialsService, SERVICE_CREDENTIALS,
};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SERVICE_HTTP, SseEventStream};
use heycode_llm::{
    CatalogRegistry, LlmSelection, ProviderRegistry, SERVICE_MODELS, SERVICE_PROVIDERS, llm_plugin,
    model_catalog_plugin,
};
use heycode_provider_azure::{
    AZURE_OPENAI_PROVIDER, AzureDeploymentName, AzureInferencePluginConfig,
    AzureOpenAiCatalogConfig, AzureResourceName, azure_openai_catalog_plugin,
    azure_openai_inference_plugin,
};
use tokio_util::sync::CancellationToken;

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct HttpPlugin;

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-azure-http"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(
            SERVICE_HTTP,
            self.name(),
            HttpService::new(Arc::new(DeadTransport)),
        )
    }
}

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-azure-credentials"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

fn query(kind: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("AZURE_OPENAI_API_KEY").unwrap(),
        CredentialKind::new(kind).unwrap(),
    )
}

#[test]
fn inference_and_catalog_rows_are_declared_registered_and_effect_owned() {
    let inference = AzureInferencePluginConfig::api_key(
        AzureResourceName::new("team-agent").unwrap(),
        AzureDeploymentName::new("prod-gpt").unwrap(),
        query("api-key"),
    )
    .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(HttpPlugin),
        Box::new(CredentialsPlugin),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: AZURE_OPENAI_PROVIDER.to_owned(),
                model: "prod-gpt".to_owned(),
            },
            Vec::new(),
        ),
        azure_openai_catalog_plugin(
            AzureOpenAiCatalogConfig::api_key(query("api-key")).with_connection(
                AzureResourceName::new("team-agent").unwrap(),
                AzureDeploymentName::new("prod-gpt").unwrap(),
            ),
        ),
        azure_openai_inference_plugin(inference),
    ];
    let mut context = compose(&plugins).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    assert_eq!(providers.names(), [AZURE_OPENAI_PROVIDER]);
    assert_eq!(
        models
            .descriptors()
            .unwrap()
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>(),
        [AZURE_OPENAI_PROVIDER]
    );
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for (plugin, kind) in [
        (
            "catalog-azure-openai",
            heycode_core::ContributionKind::ModelCatalog,
        ),
        (
            "inference-azure-openai",
            heycode_core::ContributionKind::InferenceProvider,
        ),
    ] {
        assert!(inventory.contributions.iter().any(|row| {
            row.plugin == plugin && row.kind == kind && row.name == AZURE_OPENAI_PROVIDER
        }));
    }

    context.shutdown();
    assert!(providers.names().is_empty());
    assert!(models.descriptors().unwrap().is_empty());
}

#[test]
fn inference_plugin_rejects_non_api_key_credential_semantics() {
    assert!(
        AzureInferencePluginConfig::api_key(
            AzureResourceName::new("team-agent").unwrap(),
            AzureDeploymentName::new("prod-gpt").unwrap(),
            query("oauth-token"),
        )
        .is_err()
    );
}

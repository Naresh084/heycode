use std::sync::Arc;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, ServiceKey, compose};
use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SERVICE_HTTP, SseEventStream};
use heycode_llm::{
    CatalogRegistry, LlmSelection, ProviderRegistry, SERVICE_MODELS, SERVICE_PROVIDERS, llm_plugin,
    model_catalog_plugin,
};
use heycode_provider_openai_compatible::{
    CUSTOM_OPENAI_PROVIDER, CustomOpenAiCatalogConfig, CustomOpenAiEndpoint,
    CustomOpenAiInferencePluginConfig, CustomOpenAiModel, custom_openai_catalog_plugin,
    custom_openai_inference_plugin,
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
        "test-custom-http"
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

fn query(kind: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("HEYCODE_ENDPOINT_TEST").unwrap(),
        CredentialKind::new(kind).unwrap(),
    )
}

#[test]
fn no_auth_inference_and_catalog_are_declared_registered_and_effect_owned() {
    let endpoint = CustomOpenAiEndpoint::new("http://localhost:8000/v1").unwrap();
    let inference = CustomOpenAiInferencePluginConfig::new(
        endpoint.clone(),
        CustomOpenAiModel::new("local-model").unwrap(),
        None,
    )
    .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(HttpPlugin),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: CUSTOM_OPENAI_PROVIDER.to_owned(),
                model: "local-model".to_owned(),
            },
            Vec::new(),
        ),
        custom_openai_catalog_plugin(
            CustomOpenAiCatalogConfig::discovery().with_connection(endpoint, None),
        ),
        custom_openai_inference_plugin(inference),
    ];
    let mut context = compose(&plugins).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    assert_eq!(providers.names(), [CUSTOM_OPENAI_PROVIDER]);
    assert_eq!(
        models
            .descriptors()
            .unwrap()
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>(),
        [CUSTOM_OPENAI_PROVIDER]
    );
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for (plugin, kind) in [
        (
            "catalog-custom-openai",
            heycode_core::ContributionKind::ModelCatalog,
        ),
        (
            "inference-custom-openai",
            heycode_core::ContributionKind::InferenceProvider,
        ),
    ] {
        assert!(inventory.contributions.iter().any(|row| {
            row.plugin == plugin && row.kind == kind && row.name == CUSTOM_OPENAI_PROVIDER
        }));
    }

    context.shutdown();
    assert!(providers.names().is_empty());
    assert!(models.descriptors().unwrap().is_empty());
}

#[test]
fn inference_plugin_rejects_non_api_key_credential_semantics() {
    assert!(
        CustomOpenAiInferencePluginConfig::new(
            CustomOpenAiEndpoint::new("http://localhost:8000/v1").unwrap(),
            CustomOpenAiModel::new("local-model").unwrap(),
            Some(query("oauth-token")),
        )
        .is_err()
    );
}

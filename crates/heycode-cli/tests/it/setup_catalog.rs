//! B09 setup choices come from provider/model services, never a binary table.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use heycode_cli::{
    SetupCatalog, SetupModelSource, SetupWorldOptions, compose_setup_world, resolve_setup_model,
    resolve_setup_provider,
};
use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    CatalogFetchError, CatalogRegistry, LlmSelection, ModelCapabilities, ModelCatalog,
    ModelDescriptor, ModelLifecycle, ModelLifecycleStatus, ProviderDescriptor, ProviderProtocol,
    SERVICE_MODELS, llm_plugin, model_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

struct AlphaCatalog;

#[async_trait]
impl ModelCatalog for AlphaCatalog {
    fn provider(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "alpha".to_owned(),
            display_name: "Alpha Cloud".to_owned(),
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        }
    }

    async fn fetch(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        Ok(vec![
            model("m-2", ModelLifecycleStatus::Stable),
            model("m-retired", ModelLifecycleStatus::Retired),
            model("m-1", ModelLifecycleStatus::Preview),
        ])
    }
}

fn model(id: &str, status: ModelLifecycleStatus) -> ModelDescriptor {
    ModelDescriptor {
        pricing: heycode_llm::ModelPricing::unknown(),
        performance: heycode_llm::ModelPerformance::unknown(),
        id: id.to_owned(),
        display_name: format!("Model {id}"),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(128_000),
        max_output_tokens: Some(8_192),
        lifecycle: ModelLifecycle {
            status,
            retirement_at_ms: None,
            replacement_ids: Vec::new(),
        },
        capabilities: ModelCapabilities::unknown(),
        reasoning: None,
    }
}

struct AlphaCatalogPlugin;

impl Plugin for AlphaCatalogPlugin {
    fn name(&self) -> &'static str {
        "alpha-catalog"
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_MODELS]
    }

    fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
        let catalogs = context
            .get::<CatalogRegistry>(SERVICE_MODELS)
            .ok_or_else(|| CoreError::other("models missing"))?;
        catalogs
            .register(context, Arc::new(AlphaCatalog))
            .map_err(|error| CoreError::other(error.to_string()))
    }
}

#[tokio::test]
async fn providers_and_models_are_projected_from_services_with_visible_fallback() {
    let providers: Vec<Arc<dyn heycode_llm::Provider>> = vec![
        Arc::new(FakeProvider::named("zeta", "zeta-default", Vec::new())),
        Arc::new(FakeProvider::named("alpha", "m-2", Vec::new())),
    ];
    let plugins: Vec<Box<dyn Plugin>> = vec![
        model_catalog_plugin(std::time::Duration::from_secs(60)),
        Box::new(AlphaCatalogPlugin),
        llm_plugin(
            LlmSelection {
                provider_name: "alpha".to_owned(),
                model: "m-2".to_owned(),
            },
            providers,
        ),
    ];
    let mut context = compose(&plugins).unwrap();
    let setup = SetupCatalog::from_context(&context).unwrap();

    let provider_rows = setup.providers();
    assert_eq!(
        provider_rows
            .iter()
            .map(|row| (row.id.as_str(), row.default_model.as_str()))
            .collect::<Vec<_>>(),
        [("alpha", "m-2"), ("zeta", "zeta-default")]
    );
    assert!(provider_rows[0].has_catalog);
    assert!(!provider_rows[1].has_catalog);
    assert_eq!(
        resolve_setup_provider(&provider_rows, "1").unwrap().id,
        "alpha"
    );
    assert_eq!(
        resolve_setup_provider(&provider_rows, "zeta").unwrap().id,
        "zeta"
    );
    assert!(resolve_setup_provider(&provider_rows, "3").is_err());

    let alpha = setup
        .models("alpha", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(alpha.source, SetupModelSource::LiveCatalog);
    assert_eq!(alpha.recommended, "m-2");
    assert_eq!(
        alpha
            .models
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["m-1", "m-2"]
    );
    assert!(!alpha.custom_allowed);
    assert!(alpha.warning.is_none());
    assert_eq!(resolve_setup_model(&alpha, "1").unwrap(), "m-1");
    assert_eq!(resolve_setup_model(&alpha, "m-2").unwrap(), "m-2");
    assert!(resolve_setup_model(&alpha, "made-up").is_err());

    let zeta = setup
        .models("zeta", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(zeta.source, SetupModelSource::ProviderDefault);
    assert_eq!(zeta.recommended, "zeta-default");
    assert!(zeta.custom_allowed);
    assert!(
        zeta.warning
            .as_deref()
            .unwrap()
            .contains("no model catalog")
    );
    assert_eq!(
        resolve_setup_model(&zeta, "custom/model").unwrap(),
        "custom/model"
    );
    context.shutdown();
}

#[tokio::test]
async fn real_setup_world_discovers_builtins_without_sessions_or_live_provider_construction() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = compose_setup_world(SetupWorldOptions {
        settings_user_path: dir.path().join("settings.toml"),
        credentials_root: dir.path().join("home"),
        catalog_cache_path: dir.path().join("cache/models.json"),
    })
    .unwrap();
    let providers = world.catalog().providers();
    assert_eq!(
        providers
            .iter()
            .map(|row| {
                (
                    row.id.as_str(),
                    row.default_model.as_str(),
                    row.credential_reference.as_deref(),
                )
            })
            .collect::<Vec<_>>(),
        [
            (
                "anthropic",
                heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5,
                Some(heycode_provider_anthropic::ANTHROPIC_API_KEY_REFERENCE),
            ),
            (
                "deepseek",
                heycode_llm::DeepSeekProvider::DEFAULT_MODEL,
                Some(heycode_llm::DeepSeekProvider::API_KEY_ENV),
            ),
            (
                "fireworks",
                "accounts/fireworks/models/kimi-k2-instruct-0905",
                Some("FIREWORKS_API_KEY"),
            ),
            (
                "google",
                heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH,
                Some(heycode_provider_google::GOOGLE_API_KEY_REFERENCE),
            ),
            ("groq", "openai/gpt-oss-120b", Some("GROQ_API_KEY"),),
            ("mistral", "mistral-small-latest", Some("MISTRAL_API_KEY"),),
            (
                "openai",
                heycode_provider_openai::OPENAI_GPT_5_6_SOL,
                Some(heycode_provider_openai::OPENAI_API_KEY_REFERENCE),
            ),
            (
                "openrouter",
                heycode_llm::OpenRouterProvider::DEFAULT_MODEL,
                Some(heycode_llm::OpenRouterProvider::API_KEY_ENV),
            ),
            (
                "together",
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                Some("TOGETHER_API_KEY"),
            ),
            ("xai", "grok-4.6", Some("XAI_API_KEY")),
        ]
    );
    assert!(providers.iter().all(|provider| provider.has_catalog));
    assert!(!dir.path().join("sessions").exists());
    assert!(!dir.path().join("settings.toml").exists());
    assert!(!dir.path().join("home/credentials.toml").exists());
    world.shutdown();
}

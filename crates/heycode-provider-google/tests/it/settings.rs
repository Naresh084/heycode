//! PGCP05/PGCP06 restart-applied policy and inference-config construction.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_authorization_gcp::{GcpHostPlatform, GcpMetadataPolicy, GcpProfileRequest};
use heycode_core::{Plugin, ServiceKey, compose};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialsService, SERVICE_CREDENTIALS,
};
use heycode_http::{HttpService, SERVICE_HTTP};
use heycode_llm::{
    CapabilitySupport, LlmSelection, ModelDescriptor, ProviderOptionContext, ProviderRegistry,
    RequestedCapability, ResolveError, SERVICE_PROVIDERS, llm_plugin,
};
use heycode_provider_google::{
    GOOGLE_CODE_EXECUTION_OPTION_KIND, GOOGLE_CONTEXT_CACHE_OPTION_KIND,
    GOOGLE_EXTERNAL_GROUNDING_OPTION_KIND, GOOGLE_GEMINI_3_7_FLASH,
    GOOGLE_INFERENCE_SETTINGS_NAMESPACE, GOOGLE_SEARCH_OPTION_KIND, GOOGLE_VERTEX_PROVIDER,
    GoogleGeminiModelEvidence, GoogleInferenceSettingsError, google_developer_config_from_settings,
    google_developer_settings_plugin, google_inference_plugin, google_inference_settings_plugin,
    google_lazy_vertex_config_from_settings, google_vertex_config_from_settings,
    resolve_google_inference_settings,
};
use heycode_settings::{
    SERVICE_SETTINGS, SettingsApplies, SettingsDocuments, SettingsNamespace, SettingsService,
    settings_plugin,
};

struct TestHttpPlugin(HttpService);

impl Plugin for TestHttpPlugin {
    fn name(&self) -> &'static str {
        "test-google-settings-http"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
        context.provide(SERVICE_HTTP, self.name(), self.0.clone())
    }
}

struct TestCredentialsPlugin;

impl Plugin for TestCredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-google-settings-credentials"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

fn api_key_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("GEMINI_API_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn oauth_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("gcp:cloud-platform-access-token").unwrap(),
        CredentialKind::new("oauth-token").unwrap(),
    )
}

fn descriptor(
    provider_tools: CapabilitySupport,
    native_web: CapabilitySupport,
    prompt_cache: CapabilitySupport,
) -> ModelDescriptor {
    let mut descriptor = ModelDescriptor::unknown(GOOGLE_GEMINI_3_7_FLASH);
    descriptor.capabilities.tools = provider_tools;
    descriptor.capabilities.native_web = native_web;
    descriptor.capabilities.prompt_cache = prompt_cache;
    descriptor
}

fn evidence(model: ModelDescriptor) -> GoogleGeminiModelEvidence {
    GoogleGeminiModelEvidence::new(GOOGLE_GEMINI_3_7_FLASH, vec![model]).unwrap()
}

fn settings_documents(value: serde_json::Value) -> SettingsDocuments {
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            SettingsNamespace::new(GOOGLE_INFERENCE_SETTINGS_NAMESPACE).unwrap(),
            value,
        )
        .unwrap();
    documents
}

fn resolve_settings(documents: SettingsDocuments) -> (heycode_core::Context, Arc<SettingsService>) {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(documents),
        google_inference_settings_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    let settings = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    (context, settings)
}

fn provider_plugins(
    provider: &str,
    config: heycode_provider_google::GoogleInferencePluginConfig,
) -> Vec<Box<dyn Plugin>> {
    let (http, _) = super::support::http(Vec::new());
    vec![
        Box::new(TestHttpPlugin(http)),
        Box::new(TestCredentialsPlugin),
        heycode_native_tools::native_tools_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: provider.to_owned(),
                model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
            },
            Vec::new(),
        ),
        google_inference_plugin(config),
    ]
}

#[test]
fn default_policy_is_restart_applied_disabled_and_effect_owned() {
    let (mut context, settings) = resolve_settings(SettingsDocuments::new());
    let namespace = SettingsNamespace::new(GOOGLE_INFERENCE_SETTINGS_NAMESPACE).unwrap();
    let snapshot = settings.get(&namespace).unwrap().unwrap();
    assert_eq!(snapshot.applies(), SettingsApplies::Restart);
    assert!(snapshot.wire_exposed());

    let resolved = resolve_google_inference_settings(&settings).unwrap();
    let config = resolved
        .developer_config(
            api_key_query(),
            evidence(descriptor(
                CapabilitySupport::Unknown,
                CapabilitySupport::Unknown,
                CapabilitySupport::Unknown,
            )),
        )
        .unwrap();
    let plugin = google_inference_plugin(config);
    assert_eq!(plugin.name(), "inference-google-gemini");
    assert_eq!(plugin.inventory().len(), 1);

    context.shutdown();
    assert!(settings.get(&namespace).unwrap().is_none());
}

#[test]
fn developer_search_code_and_explicit_cache_map_to_exact_request_options() {
    let documents = settings_documents(serde_json::json!({
        "developer": {
            "google_search": {
                "mode":"web",
                "models":[GOOGLE_GEMINI_3_7_FLASH]
            },
            "code_execution": {
                "mode":"enabled",
                "models":[GOOGLE_GEMINI_3_7_FLASH]
            },
            "cache": {
                "mode":"explicit",
                "resource":"cachedContents/cache-1"
            }
        }
    }));
    let (mut settings_context, settings) = resolve_settings(documents);
    let model = descriptor(
        CapabilitySupport::Supported,
        CapabilitySupport::Supported,
        CapabilitySupport::Supported,
    );
    let config =
        google_developer_config_from_settings(&settings, api_key_query(), evidence(model.clone()))
            .unwrap();
    settings_context.shutdown();

    let plugins = provider_plugins("google", config);
    let mut context = compose(&plugins).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let native = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = native.resolve("google").unwrap();
    assert_eq!(routes.len(), 2);
    let provider = providers.get("google").unwrap();
    let options = provider
        .request_options_for(ProviderOptionContext::new(&model, &routes))
        .unwrap();
    assert_eq!(
        options
            .iter()
            .map(|option| option.kind())
            .collect::<Vec<_>>(),
        vec![
            GOOGLE_CODE_EXECUTION_OPTION_KIND,
            GOOGLE_CONTEXT_CACHE_OPTION_KIND,
            GOOGLE_SEARCH_OPTION_KIND,
        ]
    );
    assert_eq!(
        options
            .iter()
            .find(|option| option.kind() == GOOGLE_CONTEXT_CACHE_OPTION_KIND)
            .unwrap()
            .data(),
        &serde_json::json!({"cachedContent":"cachedContents/cache-1"})
    );
    context.shutdown();
}

#[test]
fn settings_backed_developer_plugin_publishes_only_configured_candidates() {
    let documents = settings_documents(serde_json::json!({
        "developer": {
            "google_search": {
                "mode":"web",
                "models":[GOOGLE_GEMINI_3_7_FLASH]
            },
            "code_execution": {
                "mode":"enabled",
                "models":[GOOGLE_GEMINI_3_7_FLASH]
            }
        }
    }));
    let (http, _) = super::support::http(Vec::new());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(documents),
        google_inference_settings_plugin(),
        Box::new(TestHttpPlugin(http)),
        Box::new(TestCredentialsPlugin),
        heycode_native_tools::native_tools_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "google".to_owned(),
                model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
            },
            Vec::new(),
        ),
        google_developer_settings_plugin(
            api_key_query(),
            evidence(descriptor(
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
                CapabilitySupport::Unknown,
            )),
        )
        .unwrap(),
    ];
    let mut context = compose(&plugins).unwrap();
    let routes = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap()
        .resolve("google")
        .unwrap();
    assert_eq!(
        routes
            .iter()
            .map(|route| route.implementation())
            .collect::<Vec<_>>(),
        ["google:code_execution", "google:google_search"]
    );
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for name in ["google:code_execution", "google:google_search"] {
        assert!(inventory.contributions.iter().any(|row| {
            row.plugin == "inference-google-gemini"
                && row.kind == heycode_core::ContributionKind::NativeTool
                && row.name == name
        }));
    }
    context.shutdown();
}

#[test]
fn vertex_external_grounding_preserves_only_a_secret_reference_and_exact_cache_resource() {
    let secret_reference = "projects/vertex-fixture/secrets/search-api/versions/latest";
    let endpoint = "https://search.example.test/query";
    let documents = settings_documents(serde_json::json!({
        "vertex": {
            "external_grounding": {
                "mode":"simple-search",
                "models":[GOOGLE_GEMINI_3_7_FLASH],
                "endpoint":endpoint,
                "auth":{
                    "mode":"secret-manager-api-key",
                    "secret_version":secret_reference,
                    "name":"x-api-key",
                    "location":"header"
                }
            },
            "cache": {
                "mode":"explicit",
                "resource":"projects/vertex-fixture/locations/global/cachedContents/cache-1"
            }
        }
    }));
    let (mut settings_context, settings) = resolve_settings(documents);
    let snapshot = settings
        .get(&SettingsNamespace::new(GOOGLE_INFERENCE_SETTINGS_NAMESPACE).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.wire_projection().unwrap().resolved()["vertex"]["external_grounding"]["auth"]["secret_version"],
        secret_reference
    );
    let resolved = resolve_google_inference_settings(&settings).unwrap();
    let rendered = format!("{resolved:?}");
    assert!(!rendered.contains(secret_reference));
    assert!(!rendered.contains(endpoint));

    let model = descriptor(
        CapabilitySupport::Supported,
        CapabilitySupport::Unknown,
        CapabilitySupport::Supported,
    );
    let config = google_vertex_config_from_settings(
        &settings,
        "https://aiplatform.googleapis.test/v1/projects/vertex-fixture/locations/global/publishers/google",
        oauth_query(),
        evidence(model.clone()),
    )
    .unwrap();
    let lazy = google_lazy_vertex_config_from_settings(
        &settings,
        GcpProfileRequest {
            project: Some("vertex-fixture".to_owned()),
            location: Some("global".to_owned()),
            platform: GcpHostPlatform::Unix,
            metadata: GcpMetadataPolicy::Disabled,
        },
        oauth_query(),
    )
    .unwrap();
    assert_eq!(
        google_inference_plugin(lazy).name(),
        "inference-google-vertex"
    );
    settings_context.shutdown();

    let plugins = provider_plugins(GOOGLE_VERTEX_PROVIDER, config);
    let mut context = compose(&plugins).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let native = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = native.resolve(GOOGLE_VERTEX_PROVIDER).unwrap();
    assert_eq!(routes.len(), 1);
    let options = providers
        .get(GOOGLE_VERTEX_PROVIDER)
        .unwrap()
        .request_options_for(ProviderOptionContext::new(&model, &routes))
        .unwrap();
    assert_eq!(options.len(), 2);
    let grounding = options
        .iter()
        .find(|option| option.kind() == GOOGLE_EXTERNAL_GROUNDING_OPTION_KIND)
        .unwrap();
    assert_eq!(
        grounding.data()["tool"]["retrieval"]["externalApi"]["authConfig"]["apiKeyConfig"]["apiKeySecret"],
        secret_reference
    );
    assert!(!format!("{grounding:?}").contains(secret_reference));
    assert_eq!(
        options
            .iter()
            .find(|option| option.kind() == GOOGLE_CONTEXT_CACHE_OPTION_KIND)
            .unwrap()
            .data()["cachedContent"],
        "projects/vertex-fixture/locations/global/cachedContents/cache-1"
    );
    context.shutdown();
}

#[test]
fn implicit_cache_keeps_unknown_model_evidence_unproven() {
    let documents = settings_documents(serde_json::json!({
        "developer": {
            "cache":{"mode":"implicit","resource":""}
        }
    }));
    let (mut settings_context, settings) = resolve_settings(documents);
    let model = descriptor(
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
        CapabilitySupport::Unknown,
    );
    let config =
        google_developer_config_from_settings(&settings, api_key_query(), evidence(model.clone()))
            .unwrap();
    settings_context.shutdown();
    let plugins = provider_plugins("google", config);
    let mut context = compose(&plugins).unwrap();
    let provider = context
        .get::<ProviderRegistry>(SERVICE_PROVIDERS)
        .unwrap()
        .get("google")
        .unwrap();
    assert_eq!(
        provider
            .describe_model(GOOGLE_GEMINI_3_7_FLASH)
            .capabilities
            .prompt_cache,
        CapabilitySupport::Unknown
    );
    assert!(matches!(
        provider.request_options_for(ProviderOptionContext::new(&model, &[])),
        Err(ResolveError::Unproven {
            capability: RequestedCapability::PromptCache,
            ..
        })
    ));
    context.shutdown();
}

#[test]
fn cross_product_cache_or_literal_api_key_fields_fail_without_echo() {
    let canary = "sk-proj-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    for user in [
        serde_json::json!({
            "developer":{"cache":{
                "mode":"explicit",
                "resource":"projects/vertex-fixture/locations/global/cachedContents/cache-1"
            }}
        }),
        serde_json::json!({
            "vertex":{"external_grounding":{
                "mode":"simple-search",
                "models":[GOOGLE_GEMINI_3_7_FLASH],
                "endpoint":"https://search.example.test/query",
                "api_key":canary
            }}
        }),
    ] {
        let plugins: Vec<Box<dyn Plugin>> = vec![
            settings_plugin(settings_documents(user)),
            google_inference_settings_plugin(),
        ];
        let error = compose(&plugins).err().expect("invalid policy must fail");
        assert!(!error.to_string().contains(canary));
        assert!(!format!("{error:?}").contains(canary));
    }

    assert_eq!(
        resolve_google_inference_settings(&SettingsService::new(SettingsDocuments::new()))
            .unwrap_err(),
        GoogleInferenceSettingsError::Unavailable
    );
}

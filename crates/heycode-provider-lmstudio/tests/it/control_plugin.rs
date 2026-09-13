//! PLM04 Settings/command/catalog product integration.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_http::SERVICE_HTTP;
use heycode_llm::{CatalogRegistry, SERVICE_MODELS, model_catalog_plugin};
use heycode_provider_lmstudio::{
    LM_STUDIO_CONTROL_SETTINGS_NAMESPACE, LM_STUDIO_PROVIDER, LmStudioCatalog, LmStudioConfig,
    LmStudioControlOperations, LmStudioControlPreferences, LmStudioProductControlError,
    LmStudioProductControlReceipt, SERVICE_LM_STUDIO_MODEL_CONTROL, SERVICE_LM_STUDIO_MODELS,
    lmstudio_catalog_plugin, lmstudio_control_plugin, lmstudio_control_settings_definition,
    lmstudio_plugin,
};
use heycode_settings::{SettingsDocuments, SettingsNamespace, SettingsService, settings_plugin};
use tokio_util::sync::CancellationToken;

use super::support::{Outcome, ScriptedTransport, service};

const MODEL: &str = "openai/gpt-oss-20b";
const INSTANCE: &str = "gpt-oss-explicit";
const DOWNLOADED: &str = r#"{"models":[{
  "type":"llm","key":"openai/gpt-oss-20b","display_name":"GPT OSS 20B",
  "max_context_length":131072,"loaded_instances":[],
  "capabilities":{"trained_for_tool_use":true,"vision":false}
}]}"#;
const LOADED: &str = r#"{"models":[{
  "type":"llm","key":"openai/gpt-oss-20b","display_name":"GPT OSS 20B",
  "max_context_length":131072,
  "loaded_instances":[{"id":"gpt-oss-explicit","config":{"context_length":16384}}],
  "capabilities":{"trained_for_tool_use":true,"vision":false}
}]}"#;
const LOAD_RECEIPT: &str = r#"{
  "type":"llm","instance_id":"gpt-oss-explicit","load_time_seconds":1.5,
  "status":"loaded","load_config":{
    "context_length":16384,"eval_batch_size":512,"flash_attention":true,
    "num_experts":4,"offload_kv_cache_to_gpu":false
  }
}"#;

struct HttpPlugin(Arc<ScriptedTransport>);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), service(&self.0))
    }
}

fn world(transport: Arc<ScriptedTransport>, documents: SettingsDocuments) -> heycode_core::Context {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(documents),
        Box::new(HttpPlugin(transport)),
        model_catalog_plugin(Duration::from_secs(60)),
        lmstudio_plugin(LmStudioConfig::local()),
        lmstudio_catalog_plugin(LmStudioConfig::local()),
        heycode_agent::commands_plugin(),
        lmstudio_control_plugin(),
    ];
    compose(&plugins).unwrap()
}

fn operations(context: &Context, cancellation: CancellationToken) -> LmStudioControlOperations {
    LmStudioControlOperations::new(
        context
            .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
            .unwrap(),
        context.get(SERVICE_LM_STUDIO_MODEL_CONTROL).unwrap(),
        context
            .get::<LmStudioCatalog>(SERVICE_LM_STUDIO_MODELS)
            .unwrap(),
        context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap(),
        cancellation,
    )
}

#[test]
fn settings_modes_explicitly_separate_server_defaults_from_sent_values() {
    let definition = lmstudio_control_settings_definition().unwrap();
    let context = compose(&[settings_plugin(SettingsDocuments::new())]).unwrap();
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings.register(&context, definition).unwrap();
    let defaults = LmStudioControlPreferences::from_value(snapshot.resolved()).unwrap();
    assert_eq!(defaults.load_settings().context_length(), None);
    assert_eq!(defaults.load_settings().flash_attention(), None);

    let explicit = LmStudioControlPreferences::from_value(&serde_json::json!({
        "context_length_mode":"explicit","context_length":16384,
        "eval_batch_size_mode":"explicit","eval_batch_size":512,
        "flash_attention":"enabled",
        "num_experts_mode":"explicit","num_experts":4,
        "offload_kv_cache_to_gpu":"disabled"
    }))
    .unwrap();
    assert_eq!(explicit.load_settings().context_length(), Some(16_384));
    assert_eq!(explicit.load_settings().eval_batch_size(), Some(512));
    assert_eq!(explicit.load_settings().flash_attention(), Some(true));
    assert_eq!(explicit.load_settings().num_experts(), Some(4));
    assert_eq!(
        explicit.load_settings().offload_kv_cache_to_gpu(),
        Some(false)
    );
    assert_eq!(
        LmStudioControlPreferences::from_value(&serde_json::json!({
            "context_length_mode":"explicit","context_length":0,
            "eval_batch_size_mode":"server-default","eval_batch_size":512,
            "flash_attention":"server-default",
            "num_experts_mode":"server-default","num_experts":1,
            "offload_kv_cache_to_gpu":"server-default"
        })),
        Err(LmStudioProductControlError::InvalidSettings)
    );
}

#[test]
fn plugin_registers_effect_owned_settings_and_queued_command() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Refused));
    let mut context = world(transport, SettingsDocuments::new());
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let namespace = SettingsNamespace::new(LM_STUDIO_CONTROL_SETTINGS_NAMESPACE).unwrap();
    assert!(settings.get(&namespace).unwrap().is_some());
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let command = commands.get("lmstudio").unwrap().unwrap();
    assert_eq!(
        command.descriptor().timing(),
        heycode_agent::CommandTiming::Queued
    );
    assert_eq!(command.descriptor().source().plugin(), "lmstudio-control");
    assert_eq!(
        command.descriptor().synopsis(),
        "/lmstudio <operation> <target>"
    );

    context.shutdown();
    assert!(settings.get(&namespace).unwrap().is_none());
    assert!(commands.get("lmstudio").unwrap().is_none());
    assert!(!command.availability().is_available());
}

#[tokio::test]
async fn explicit_load_reads_live_settings_verifies_readback_and_refreshes_shared_catalog() {
    let list_url = format!(
        "{}/api/v1/models",
        heycode_provider_lmstudio::LM_STUDIO_DEFAULT_BASE_URL
    );
    let load_url = format!(
        "{}/api/v1/models/load",
        heycode_provider_lmstudio::LM_STUDIO_DEFAULT_BASE_URL
    );
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Refused)
            .on_sequence(
                list_url,
                [
                    Outcome::Json(DOWNLOADED),
                    Outcome::Json(LOADED),
                    Outcome::Json(LOADED),
                ],
            )
            .on_url(load_url, Outcome::Json(LOAD_RECEIPT)),
    );
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            SettingsNamespace::new(LM_STUDIO_CONTROL_SETTINGS_NAMESPACE).unwrap(),
            serde_json::json!({
                "context_length_mode":"explicit","context_length":16384,
                "eval_batch_size_mode":"explicit","eval_batch_size":512,
                "flash_attention":"enabled",
                "num_experts_mode":"explicit","num_experts":4,
                "offload_kv_cache_to_gpu":"disabled"
            }),
        )
        .unwrap();
    let context = world(transport.clone(), documents);

    let receipt = operations(&context, CancellationToken::new())
        .load(MODEL)
        .await
        .unwrap();
    assert_eq!(
        receipt,
        LmStudioProductControlReceipt::Loaded {
            instance_id: INSTANCE.to_owned(),
            catalog_revision: 1,
        }
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1].url,
        format!(
            "{}/api/v1/models/load",
            heycode_provider_lmstudio::LM_STUDIO_DEFAULT_BASE_URL
        )
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[1].body).unwrap(),
        serde_json::json!({
            "model":MODEL,"context_length":16384,"eval_batch_size":512,
            "flash_attention":true,"num_experts":4,
            "offload_kv_cache_to_gpu":false,"echo_load_config":true
        })
    );
    assert!(
        context
            .get::<CatalogRegistry>(SERVICE_MODELS)
            .unwrap()
            .cached(LM_STUDIO_PROVIDER)
            .unwrap()
            .models
            .iter()
            .any(|model| model.id == INSTANCE && model.context_window == Some(16384))
    );
}

#[tokio::test]
async fn unload_requires_an_observed_exact_instance_and_confirms_its_removal() {
    let list_url = format!(
        "{}/api/v1/models",
        heycode_provider_lmstudio::LM_STUDIO_DEFAULT_BASE_URL
    );
    let unload_url = format!(
        "{}/api/v1/models/unload",
        heycode_provider_lmstudio::LM_STUDIO_DEFAULT_BASE_URL
    );
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Refused)
            .on_sequence(
                list_url,
                [
                    Outcome::Json(LOADED),
                    Outcome::Json(DOWNLOADED),
                    Outcome::Json(DOWNLOADED),
                ],
            )
            .on_url(
                unload_url,
                Outcome::Json(r#"{"instance_id":"gpt-oss-explicit"}"#),
            ),
    );
    let context = world(transport.clone(), SettingsDocuments::new());
    let receipt = operations(&context, CancellationToken::new())
        .unload(INSTANCE)
        .await
        .unwrap();
    assert_eq!(
        receipt,
        LmStudioProductControlReceipt::Unloaded {
            instance_id: INSTANCE.to_owned(),
            catalog_revision: 1,
        }
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[1].body).unwrap(),
        serde_json::json!({"instance_id":INSTANCE})
    );
}

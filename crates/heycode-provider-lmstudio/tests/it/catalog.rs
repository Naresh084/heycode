//! Model list, local state and capability mapping. Every case drives an
//! injected transport; none uses the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, ProviderProtocol, compose};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::SERVICE_HTTP;
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogRefreshMode, CatalogRegistry, ModelCatalog,
    SERVICE_MODELS,
};
use heycode_provider_lmstudio::{
    LmStudioCatalog, LmStudioConfig, LmStudioModelKind, LmStudioModelRecord, LmStudioModelState,
    LmStudioSurface, SERVICE_LM_STUDIO_MODELS, lmstudio_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

use super::support::{self, Outcome, ScriptedTransport};

/// The complete documented `GET /api/v1/models` example, verbatim.
/// <https://lmstudio.ai/docs/developer/rest/list>
const DOCUMENTED_LIBRARY: &str = r#"{
  "models": [
    {
      "type": "llm",
      "publisher": "google",
      "key": "google/gemma-4-26b-a4b",
      "display_name": "Gemma 4 26B A4B",
      "architecture": "gemma4",
      "quantization": { "name": "Q4_K_M", "bits_per_weight": 4 },
      "size_bytes": 17990911801,
      "params_string": "26B-A4B",
      "loaded_instances": [
        {
          "id": "google/gemma-4-26b-a4b",
          "config": {
            "context_length": 4096,
            "eval_batch_size": 512,
            "parallel": 4,
            "flash_attention": true,
            "num_experts": 8,
            "offload_kv_cache_to_gpu": true
          }
        }
      ],
      "max_context_length": 262144,
      "format": "gguf",
      "capabilities": {
        "vision": true,
        "trained_for_tool_use": true,
        "reasoning": { "allowed_options": ["off", "on"], "default": "on" }
      },
      "description": null,
      "variants": ["google/gemma-4-26b-a4b@q4_k_m"],
      "selected_variant": "google/gemma-4-26b-a4b@q4_k_m"
    },
    {
      "type": "llm",
      "publisher": "deepseek",
      "key": "deepseek-r1",
      "display_name": "DeepSeek R1",
      "architecture": "deepseek",
      "quantization": { "name": "Q4_K_M", "bits_per_weight": 4 },
      "size_bytes": 40492610355,
      "params_string": "671B",
      "loaded_instances": [],
      "max_context_length": 131072,
      "format": "gguf",
      "capabilities": {
        "vision": false,
        "trained_for_tool_use": true,
        "reasoning": { "allowed_options": ["on"], "default": "on" }
      },
      "description": null
    },
    {
      "type": "embedding",
      "publisher": "gaianet",
      "key": "text-embedding-nomic-embed-text-v1.5-embedding",
      "display_name": "Nomic Embed Text v1.5",
      "quantization": { "name": "F16", "bits_per_weight": 16 },
      "size_bytes": 274290560,
      "params_string": null,
      "loaded_instances": [],
      "max_context_length": 2048,
      "format": "gguf"
    }
  ]
}"#;

fn library(body: &'static str) -> Arc<ScriptedTransport> {
    Arc::new(
        ScriptedTransport::new(Outcome::Response(404, Some("application/json"), "{}"))
            .on(LmStudioSurface::NativeRestV1, Outcome::Json(body)),
    )
}

fn catalog(transport: &Arc<ScriptedTransport>) -> LmStudioCatalog {
    LmStudioCatalog::new(support::service(transport), None, LmStudioConfig::local())
}

async fn records(body: &'static str) -> Vec<LmStudioModelRecord> {
    catalog(&library(body))
        .list_models(CancellationToken::new())
        .await
        .unwrap()
}

fn find<'a>(records: &'a [LmStudioModelRecord], key: &str) -> &'a LmStudioModelRecord {
    records
        .iter()
        .find(|record| record.key == key)
        .unwrap_or_else(|| panic!("no record for {key}"))
}

#[tokio::test]
async fn the_documented_library_yields_one_record_per_published_model() {
    let records = records(DOCUMENTED_LIBRARY).await;
    let keys: Vec<&str> = records.iter().map(|record| record.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "google/gemma-4-26b-a4b",
            "deepseek-r1",
            "text-embedding-nomic-embed-text-v1.5-embedding",
        ]
    );
}

#[tokio::test]
async fn a_model_with_a_loaded_instance_reports_loaded() {
    let records = records(DOCUMENTED_LIBRARY).await;
    let loaded = find(&records, "google/gemma-4-26b-a4b");
    assert_eq!(loaded.state(), LmStudioModelState::Loaded);
    assert!(loaded.is_loaded());
    assert_eq!(loaded.loaded_instances.len(), 1);
    assert_eq!(loaded.loaded_instances[0].id, "google/gemma-4-26b-a4b");
}

#[tokio::test]
async fn a_model_with_no_loaded_instance_reports_downloaded_not_missing() {
    let records = records(DOCUMENTED_LIBRARY).await;
    let downloaded = find(&records, "deepseek-r1");
    assert_eq!(downloaded.state(), LmStudioModelState::Downloaded);
    assert!(!downloaded.is_loaded());
    assert!(
        downloaded.is_chat_model(),
        "a downloaded model is routable: LM Studio loads it on demand"
    );
}

#[tokio::test]
async fn a_loaded_instance_context_never_replaces_the_model_context_window() {
    // The documented example loads a 262,144-token model at 4,096.
    let records = records(DOCUMENTED_LIBRARY).await;
    let loaded = find(&records, "google/gemma-4-26b-a4b");
    assert_eq!(loaded.max_context_length, Some(262_144));
    assert_eq!(loaded.loaded_instances[0].context_length, Some(4_096));
    assert_eq!(loaded.descriptor().context_window, Some(262_144));
}

#[tokio::test]
async fn an_explicit_tool_training_flag_is_evidence_in_both_directions() {
    let records = records(
        r#"{"models":[
            {"type":"llm","key":"trained","capabilities":{"trained_for_tool_use":true}},
            {"type":"llm","key":"not-trained","capabilities":{"trained_for_tool_use":false}}
        ]}"#,
    )
    .await;
    assert_eq!(
        find(&records, "trained").tool_trained,
        CapabilitySupport::Supported
    );
    assert_eq!(
        find(&records, "not-trained").tool_trained,
        CapabilitySupport::Unsupported
    );
}

#[tokio::test]
async fn an_absent_capabilities_object_leaves_tool_training_unknown() {
    let records = records(r#"{"models":[{"type":"embedding","key":"embed"}]}"#).await;
    let record = find(&records, "embed");
    assert_eq!(record.tool_trained, CapabilitySupport::Unknown);
    assert_eq!(record.vision, CapabilitySupport::Unknown);
    assert_eq!(record.reasoning, CapabilitySupport::Unknown);
}

#[tokio::test]
async fn an_absent_capability_key_is_unknown_rather_than_unsupported() {
    let records =
        records(r#"{"models":[{"type":"llm","key":"partial","capabilities":{"vision":true}}]}"#)
            .await;
    let record = find(&records, "partial");
    assert_eq!(record.vision, CapabilitySupport::Supported);
    assert_eq!(
        record.tool_trained,
        CapabilitySupport::Unknown,
        "an omitted key is not a published denial"
    );
}

#[tokio::test]
async fn a_reasoning_control_is_an_object_not_a_boolean() {
    // A boolean deserialize would reject every reasoning-capable model.
    let records = records(DOCUMENTED_LIBRARY).await;
    let record = find(&records, "google/gemma-4-26b-a4b");
    assert_eq!(record.reasoning, CapabilitySupport::Supported);
    assert_eq!(record.reasoning_options, vec!["off", "on"]);
    assert_eq!(
        find(&records, "deepseek-r1").reasoning_options,
        vec!["on"],
        "an always-on model publishes a single option"
    );
}

#[tokio::test]
async fn capability_mapping_reaches_exactly_the_three_published_components() {
    let records = records(DOCUMENTED_LIBRARY).await;
    let capabilities = find(&records, "google/gemma-4-26b-a4b").capabilities();
    assert_eq!(capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(capabilities.image_input, CapabilitySupport::Supported);
    assert_eq!(capabilities.reasoning, CapabilitySupport::Supported);
    for unpublished in [
        capabilities.document_input,
        capabilities.structured_output,
        capabilities.native_web,
        capabilities.native_compaction,
        capabilities.prompt_cache,
    ] {
        assert_eq!(
            unpublished,
            CapabilitySupport::Unknown,
            "LM Studio publishes no evidence for this component"
        );
    }
}

#[tokio::test]
async fn an_embedding_model_never_reaches_the_routing_catalog() {
    let transport = library(DOCUMENTED_LIBRARY);
    let ids: Vec<String> = catalog(&transport)
        .fetch(CancellationToken::new())
        .await
        .unwrap()
        .into_iter()
        .map(|descriptor| descriptor.id)
        .collect();
    assert_eq!(ids, vec!["google/gemma-4-26b-a4b", "deepseek-r1"]);
}

#[tokio::test]
async fn an_embedding_model_stays_visible_in_the_local_record_list() {
    let records = records(DOCUMENTED_LIBRARY).await;
    let embedding = find(&records, "text-embedding-nomic-embed-text-v1.5-embedding");
    assert_eq!(embedding.kind, LmStudioModelKind::Embedding);
    assert!(!embedding.is_chat_model());
    assert_eq!(embedding.state(), LmStudioModelState::Downloaded);
}

#[tokio::test]
async fn an_unrecognized_model_type_is_retained_but_never_routed() {
    let records = records(r#"{"models":[{"type":"vlm","key":"legacy-vision"}]}"#).await;
    let record = find(&records, "legacy-vision");
    assert_eq!(
        record.kind,
        LmStudioModelKind::Unrecognized("vlm".to_owned()),
        "a type this crate does not know is kept verbatim, not dropped"
    );
    assert!(!record.is_chat_model());
}

#[tokio::test]
async fn a_blank_or_duplicate_model_key_rejects_the_whole_generation() {
    for body in [
        r#"{"models":[{"type":"llm","key":"ok"},{"type":"llm","key":"  "}]}"#,
        r#"{"models":[{"type":"llm","key":"same"},{"type":"llm","key":"same"}]}"#,
    ] {
        let error = catalog(&library(body))
            .list_models(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    }
}

#[tokio::test]
async fn an_absent_display_name_falls_back_to_the_model_key() {
    let records = records(r#"{"models":[{"type":"llm","key":"bare"}]}"#).await;
    assert_eq!(find(&records, "bare").display_name, "bare");
}

#[tokio::test]
async fn a_body_that_is_not_the_documented_envelope_is_an_invalid_response() {
    for body in [
        r#"{"object":"list","data":[]}"#,
        r#"{"models":{"not":"an array"}}"#,
        "not json at all",
    ] {
        let error = catalog(&library(body))
            .list_models(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    }
}

#[tokio::test]
async fn a_documented_body_under_a_non_json_content_type_is_rejected() {
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Response(404, Some("application/json"), "{}")).on(
            LmStudioSurface::NativeRestV1,
            Outcome::Response(200, Some("text/html"), DOCUMENTED_LIBRARY),
        ),
    );
    let error = catalog(&transport)
        .list_models(CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn a_rejected_model_list_is_classified_unauthorized() {
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Response(404, Some("application/json"), "{}")).on(
            LmStudioSurface::NativeRestV1,
            Outcome::Response(401, Some("application/json"), "{}"),
        ),
    );
    let error = catalog(&transport)
        .list_models(CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unauthorized);
}

#[tokio::test]
async fn a_stopped_server_is_a_network_failure_not_an_empty_library() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Refused));
    let error = catalog(&transport)
        .list_models(CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind(),
        CatalogFailureKind::Network,
        "an unreachable server must never look like a library with no models"
    );
}

#[tokio::test]
async fn a_hung_server_bounds_the_model_list_instead_of_stalling_the_refresh() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Hang));
    let config = LmStudioConfig::local()
        .with_catalog_timeout(Duration::from_millis(50))
        .unwrap();
    let started = std::time::Instant::now();
    let error = match tokio::time::timeout(
        Duration::from_secs(5),
        LmStudioCatalog::new(support::service(&transport), None, config)
            .list_models(CancellationToken::new()),
    )
    .await
    {
        Ok(result) => result.unwrap_err(),
        Err(_) => panic!("the model list never settled: the read is unbounded"),
    };
    assert_eq!(error.kind(), CatalogFailureKind::Network);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_cancelled_read_never_sends_a_request() {
    let transport = library(DOCUMENTED_LIBRARY);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = catalog(&transport)
        .list_models(cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Cancelled);
    assert!(transport.requests().is_empty());
}

#[tokio::test]
async fn the_model_list_is_read_from_the_documented_native_v1_path() {
    let transport = library(DOCUMENTED_LIBRARY);
    catalog(&transport)
        .list_models(CancellationToken::new())
        .await
        .unwrap();
    let urls: Vec<String> = transport
        .requests()
        .into_iter()
        .map(|request| request.url)
        .collect();
    assert_eq!(urls, vec![support::url(LmStudioSurface::NativeRestV1)]);
}

#[tokio::test]
async fn a_configured_bearer_token_is_sent_and_never_reaches_debug_output() {
    let transport = library(DOCUMENTED_LIBRARY);
    let (_context, credentials) = support::credentials(Some("lmstudio-secret-token"));
    let catalog = LmStudioCatalog::new(
        support::service(&transport),
        Some(credentials),
        LmStudioConfig::local().with_bearer_token(support::query()),
    );
    let records = catalog.list_models(CancellationToken::new()).await.unwrap();
    assert_eq!(
        transport.requests()[0].authorization.as_deref(),
        Some("Bearer lmstudio-secret-token")
    );
    let rendered = format!("{catalog:?} {records:?}");
    assert!(!rendered.contains("lmstudio-secret-token"), "{rendered}");
}

#[tokio::test]
async fn the_catalog_source_claims_no_protocol_it_did_not_observe() {
    // The model list proves the native REST surface, not an inference protocol.
    let descriptor = catalog(&library(DOCUMENTED_LIBRARY)).provider();
    assert_eq!(descriptor.id, "lmstudio");
    assert_eq!(descriptor.protocols, vec![ProviderProtocol::Unknown]);
}

#[tokio::test]
async fn a_local_model_publishes_no_price_output_cap_or_lifecycle() {
    let records = records(DOCUMENTED_LIBRARY).await;
    let descriptor = find(&records, "deepseek-r1").descriptor();
    assert_eq!(descriptor.max_output_tokens, None);
    assert!(descriptor.pricing.is_unknown());
    assert!(descriptor.aliases.is_empty());
}

struct HttpPlugin(Arc<ScriptedTransport>);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), support::service(&self.0))
    }
}

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

fn world(transport: Arc<ScriptedTransport>, config: LmStudioConfig) -> Vec<Box<dyn Plugin>> {
    vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(transport)),
        Box::new(CredentialsPlugin),
        lmstudio_catalog_plugin(config),
    ]
}

#[tokio::test]
async fn the_plugin_registers_the_catalog_as_an_effect_that_shutdown_removes() {
    let plugins = world(library(DOCUMENTED_LIBRARY), LmStudioConfig::local());
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            "lmstudio",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let ids: Vec<&str> = view
        .snapshot
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["google/gemma-4-26b-a4b"],
        "the shared inference picker offers loaded instances; downloaded records remain in the native service"
    );

    context.shutdown();
    assert!(
        models
            .refresh(
                "lmstudio",
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_plugin_publishes_the_local_records_under_their_own_service_key() {
    let plugins = world(library(DOCUMENTED_LIBRARY), LmStudioConfig::local());
    let context = compose(&plugins).unwrap();
    // The bare value, not an `Arc<Arc<_>>` (GOTCHAS #27).
    let catalog = context
        .get::<LmStudioCatalog>(SERVICE_LM_STUDIO_MODELS)
        .unwrap();
    let records = catalog.list_models(CancellationToken::new()).await.unwrap();
    assert_eq!(records.len(), 3, "the service exposes embeddings too");
    assert!(records.iter().any(LmStudioModelRecord::is_loaded));
}

#[test]
fn the_catalog_plugin_declares_its_catalog_row_and_service() {
    let plugin = lmstudio_catalog_plugin(LmStudioConfig::local());
    assert_eq!(plugin.name(), "catalog-lmstudio");
    assert_eq!(plugin.descriptor().id, plugin.name());
    assert_eq!(plugin.provides(), &[SERVICE_LM_STUDIO_MODELS]);
    let inventory = plugin.inventory();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].name, "lmstudio");
}

#[test]
fn the_default_posture_catalog_plugin_injects_no_credentials() {
    let plugin = lmstudio_catalog_plugin(LmStudioConfig::local());
    assert_eq!(plugin.inject(), &[SERVICE_MODELS, SERVICE_HTTP]);
    let authenticated =
        lmstudio_catalog_plugin(LmStudioConfig::local().with_bearer_token(support::query()));
    assert_eq!(
        authenticated.inject(),
        &[SERVICE_MODELS, SERVICE_HTTP, SERVICE_CREDENTIALS]
    );
}

#[test]
fn a_model_list_budget_outside_its_bounds_is_rejected_not_clamped() {
    assert!(
        LmStudioConfig::local()
            .with_catalog_timeout(Duration::ZERO)
            .is_err()
    );
    assert!(
        LmStudioConfig::local()
            .with_catalog_timeout(Duration::from_secs(3600))
            .is_err()
    );
}

#[tokio::test]
async fn endpoint_discovery_does_not_replace_shared_cache() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Json(DOCUMENTED_LIBRARY)));
    let context = compose(&world(
        transport.clone(),
        LmStudioConfig::local().with_bearer_token(support::query()),
    ))
    .unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let snapshot = models
        .probe_endpoint(
            "lmstudio",
            "http://localhost:2234",
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!snapshot.models.is_empty());
    assert!(
        models.cached("lmstudio").is_err(),
        "draft discovery must not overwrite the active catalog"
    );
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.url.starts_with("http://localhost:2234/"))
    );
}

#[tokio::test]
async fn endpoint_key_is_explicit_and_does_not_leak_to_the_next_discovery() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Json(DOCUMENTED_LIBRARY)));
    let context = compose(&world(transport.clone(), LmStudioConfig::local())).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let credential = heycode_credentials::CredentialSecret::new("fixture-local-key");
    models
        .probe_endpoint_with_credential(
            "lmstudio",
            "http://localhost:2234",
            Some(&credential),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    models
        .probe_endpoint(
            "lmstudio",
            "http://localhost:3234",
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let requests = transport.requests();
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer fixture-local-key")
    );
    assert_eq!(requests[1].authorization, None);
    assert!(requests[1].url.starts_with("http://localhost:3234/"));
    assert!(models.cached("lmstudio").is_err());
}

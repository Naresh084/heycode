//! Versioned owner-only catalog persistence contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_catalog_file::{
    FILE_SCHEMA_VERSION, FileCatalogConfig, FileCatalogPersistence, MIN_FILE_SCHEMA_VERSION,
    file_catalog_persistence_plugin,
};
use heycode_llm::{
    CapabilitySupport, CatalogPersistence, CatalogRegistry, CatalogSnapshot, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelMetadataProvenance, ProviderDescriptor, ProviderProtocol,
    SERVICE_MODELS, model_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

fn sample_pricing() -> heycode_llm::ModelPricing {
    heycode_llm::ModelPricing::captured(
        ModelMetadataProvenance::new("https://openrouter.ai/api/v1/models", 1_787_752_741_000)
            .unwrap(),
        heycode_llm::PriceComponent::Input,
        heycode_llm::TokenPrice::parse_decimal(
            heycode_llm::PriceCurrency::Usd,
            heycode_llm::TokenPriceUnit::PerToken,
            "0.000000075",
        )
        .unwrap(),
    )
    .with(
        heycode_llm::PriceComponent::Output,
        heycode_llm::TokenPrice::parse_decimal(
            heycode_llm::PriceCurrency::Usd,
            heycode_llm::TokenPriceUnit::PerToken,
            "0.00000025",
        )
        .unwrap(),
    )
    .unwrap()
}

fn sample_performance() -> heycode_llm::ModelPerformance {
    heycode_llm::ModelPerformance::observed(
        ModelMetadataProvenance::new("benchmark:catalog-file-fixture", 1_787_752_742_000).unwrap(),
        Some(420),
        Some(83_500),
    )
    .unwrap()
}

fn snapshot(revision: u64, fetched_at_ms: u64) -> Arc<CatalogSnapshot> {
    Arc::new(CatalogSnapshot {
        provider: ProviderDescriptor {
            id: "openrouter".to_owned(),
            display_name: "OpenRouter".to_owned(),
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        },
        models: vec![ModelDescriptor {
            pricing: sample_pricing(),
            performance: sample_performance(),
            id: "stealth/ox-alpha".to_owned(),
            display_name: "Ox Alpha".to_owned(),
            aliases: vec!["stealth/ox".to_owned()],
            created_at_ms: None,
            context_window: Some(1_048_576),
            max_output_tokens: Some(131_072),
            lifecycle: ModelLifecycle::preview(),
            capabilities: ModelCapabilities {
                tools: CapabilitySupport::Supported,
                reasoning: CapabilitySupport::Supported,
                image_input: CapabilitySupport::Supported,
                document_input: CapabilitySupport::Supported,
                structured_output: CapabilitySupport::Supported,
                native_web: CapabilitySupport::Unknown,
                native_compaction: CapabilitySupport::Unknown,
                prompt_cache: CapabilitySupport::Unknown,
            },
            reasoning: None,
        }],
        revision,
        fetched_at_ms,
    })
}

#[test]
fn current_schema_round_trips_complete_generation_not_user_selection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache").join("models.json");
    let store = FileCatalogPersistence::open(FileCatalogConfig::new(&path)).unwrap();
    store
        .save(&[snapshot(7, 1_777_777)], &CancellationToken::new())
        .unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(loaded, [snapshot(7, 1_777_777).as_ref().clone()]);
    let raw = std::fs::read_to_string(&path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["schema_version"], FILE_SCHEMA_VERSION);
    assert_eq!(json["generations"][0]["revision"], 7);
    assert_eq!(json["generations"][0]["fetched_at_ms"], 1_777_777);
    assert_eq!(
        json["generations"][0]["models"][0]["id"],
        "stealth/ox-alpha"
    );
    assert!(json.get("selection").is_none());
    assert!(json.get("provider").is_none());
    assert!(json.get("model").is_none());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn schema_v1_restores_rows_but_drops_advisory_facts_whose_source_was_never_stored() {
    assert_eq!(MIN_FILE_SCHEMA_VERSION, 1);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let store = FileCatalogPersistence::open(FileCatalogConfig::new(&path)).unwrap();
    store
        .save(&[snapshot(7, 1_700_000_000_000)], &CancellationToken::new())
        .unwrap();
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    legacy["schema_version"] = serde_json::json!(1);
    let model = &mut legacy["generations"][0]["models"][0];
    model["pricing"]
        .as_object_mut()
        .unwrap()
        .remove("provenance");
    let performance = model["performance"].as_object_mut().unwrap();
    let provenance = performance.remove("provenance").unwrap();
    performance.insert(
        "observed_at_ms".to_owned(),
        provenance["captured_at_ms"].clone(),
    );
    std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let restored = store.load().unwrap();
    assert_eq!(restored[0].revision, 7);
    assert_eq!(restored[0].models[0].id, "stealth/ox-alpha");
    assert!(restored[0].models[0].pricing.is_unknown());
    assert!(restored[0].models[0].performance.is_unknown());
}

#[test]
fn current_schema_refuses_advisory_metadata_missing_its_own_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let store = FileCatalogPersistence::open(FileCatalogConfig::new(&path)).unwrap();
    store
        .save(&[snapshot(7, 1_700_000_000_000)], &CancellationToken::new())
        .unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

    for field in ["pricing", "performance"] {
        let mut broken = original.clone();
        broken["generations"][0]["models"][0][field]
            .as_object_mut()
            .unwrap()
            .remove("provenance");
        std::fs::write(&path, serde_json::to_vec_pretty(&broken).unwrap()).unwrap();
        let error = store
            .load()
            .expect_err("schema-v2 advisory metadata must carry provenance");
        assert!(error.to_string().contains("provenance"), "{error}");
    }
}

#[test]
fn cancelled_replacement_keeps_exact_last_good_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let store = FileCatalogPersistence::open(FileCatalogConfig::new(&path)).unwrap();
    store
        .save(&[snapshot(1, 1_000)], &CancellationToken::new())
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert!(store.save(&[snapshot(2, 2_000)], &cancellation).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(store.load().unwrap()[0].revision, 1);
}

#[test]
fn newer_schema_fails_loud_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let raw = format!(
        "{{\"schema_version\":{},\"generations\":[],\"future_field\":true}}\n",
        FILE_SCHEMA_VERSION + 1
    );
    std::fs::write(&path, &raw).unwrap();

    let error = match FileCatalogPersistence::open(FileCatalogConfig::new(&path)) {
        Ok(_) => panic!("expected newer-schema failure"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("newer schema"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
}

#[test]
fn file_persistence_mounts_as_a_disposable_models_provider() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = vec![
        model_catalog_plugin(std::time::Duration::from_secs(300)),
        file_catalog_persistence_plugin(FileCatalogConfig::new(dir.path().join("models.json"))),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    assert!(registry.has_persistence().unwrap());
    context.shutdown();
    assert!(!registry.has_persistence().unwrap());
}

#[cfg(unix)]
#[test]
fn symbolic_link_cache_path_is_refused() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.json");
    std::fs::write(&target, "{}\n").unwrap();
    let link = dir.path().join("models.json");
    symlink(&target, &link).unwrap();

    let error = match FileCatalogPersistence::open(FileCatalogConfig::new(&link)) {
        Ok(_) => panic!("expected symlink refusal"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("symbolic link"));
}

#[test]
fn pricing_and_performance_round_trip_and_unknown_wire_values_fail_loud() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache").join("models.json");
    let store = FileCatalogPersistence::open(FileCatalogConfig::new(&path)).unwrap();
    store
        .save(&[snapshot(7, 1_700_000_000_000)], &CancellationToken::new())
        .unwrap();

    let restored = store.load().unwrap();
    let model = &restored[0].models[0];
    assert_eq!(model.pricing, sample_pricing(), "exact price round trip");
    assert_eq!(model.performance, sample_performance());

    // Prices persist in the currency and unit they were published in, as exact
    // integers — never as a float that could drift.
    let text = std::fs::read_to_string(&path).unwrap();
    let document: serde_json::Value = serde_json::from_str(&text).unwrap();
    let pricing = &document["generations"][0]["models"][0]["pricing"];
    assert_eq!(pricing["currency"], "USD");
    assert_eq!(pricing["unit"], "per_token");
    assert_eq!(
        pricing["provenance"]["source"],
        "https://openrouter.ai/api/v1/models"
    );
    assert_eq!(
        pricing["provenance"]["captured_at_ms"],
        1_787_752_741_000_u64
    );
    assert_eq!(pricing["components"][0]["component"], "input");
    assert_eq!(pricing["components"][0]["pico_units"], 75_000);
    let performance = &document["generations"][0]["models"][0]["performance"];
    assert_eq!(
        performance["provenance"]["source"],
        "benchmark:catalog-file-fixture"
    );
    assert_eq!(
        performance["provenance"]["captured_at_ms"],
        1_787_752_742_000_u64
    );

    // A newer file naming an unrepresented component must fail rather than be
    // downgraded to "no price", which would silently misreport cost.
    for (pointer, replacement) in [
        (
            "/generations/0/models/0/pricing/currency",
            serde_json::json!("XTS"),
        ),
        (
            "/generations/0/models/0/pricing/unit",
            serde_json::json!("per_billion_tokens"),
        ),
        (
            "/generations/0/models/0/pricing/components/0/component",
            serde_json::json!("audio"),
        ),
    ] {
        let mut mutated = document.clone();
        *mutated.pointer_mut(pointer).unwrap() = replacement;
        std::fs::write(&path, serde_json::to_string(&mutated).unwrap()).unwrap();
        let error = store
            .load()
            .expect_err("an unrepresented wire value must fail loud");
        let rendered = error.to_string();
        assert!(
            !rendered.contains("panicked"),
            "failure must stay a typed parse error: {rendered}"
        );
    }
}

#[test]
fn creation_dates_round_trip_and_older_caches_without_dates_still_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let store = FileCatalogPersistence::open(FileCatalogConfig::new(&path)).unwrap();
    let mut generation = snapshot(1, 1000).as_ref().clone();
    generation.models[0].created_at_ms = Some(1_780_272_000_000);
    store
        .save(&[Arc::new(generation.clone())], &CancellationToken::new())
        .unwrap();
    assert_eq!(store.load().unwrap(), [generation]);
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["generations"][0]["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("created_at_ms");
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(store.load().unwrap()[0].models[0].created_at_ms, None);
}

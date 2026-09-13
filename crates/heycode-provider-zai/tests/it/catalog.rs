//! PZA02 maintained GLM catalog: limits, capabilities and retirement.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, ProviderProtocol, compose};
use heycode_llm::{
    CapabilitySupport, CatalogError, CatalogRefreshMode, CatalogRegistry, ModelCatalog,
    ModelDescriptor, ModelLifecycle, ModelLifecycleStatus, ModelPerformance, ModelPricing,
    SERVICE_MODELS,
};
use heycode_provider_zai::{Coding, General, ZaiCatalog, ZaiPlan, zai_catalog_plugin};
use tokio_util::sync::CancellationToken;

async fn rows() -> BTreeMap<String, ModelDescriptor> {
    ZaiCatalog::<General>::new()
        .fetch(CancellationToken::new())
        .await
        .unwrap()
        .into_iter()
        .map(|descriptor| (descriptor.id.clone(), descriptor))
        .collect()
}

#[tokio::test]
async fn the_catalog_publishes_every_model_the_chat_schema_names() {
    let rows = rows().await;
    let ids: Vec<&str> = rows.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        vec![
            "autoglm-phone-multilingual",
            "glm-4-32b-0414-128k",
            "glm-4.5",
            "glm-4.5-air",
            "glm-4.5-airx",
            "glm-4.5-flash",
            "glm-4.5-x",
            "glm-4.5v",
            "glm-4.6",
            "glm-4.6v",
            "glm-4.6v-flash",
            "glm-4.6v-flashx",
            "glm-4.7",
            "glm-4.7-flash",
            "glm-4.7-flashx",
            "glm-5",
            "glm-5.1",
            "glm-5.2",
            "glm-5.3",
            "glm-5.3-flash",
        ]
    );
}

#[tokio::test]
async fn output_caps_follow_the_documented_per_family_maximums() {
    let rows = rows().await;
    for (id, expected) in [
        ("glm-5.3", 131_072),
        ("glm-5.3-flash", 131_072),
        ("glm-4.7-flashx", 131_072),
        ("glm-4.6", 131_072),
        ("glm-4.5", 98_304),
        ("glm-4.5-airx", 98_304),
        ("glm-4-32b-0414-128k", 16_384),
        ("glm-4.6v", 32_768),
        ("glm-4.6v-flash", 32_768),
        ("glm-4.5v", 16_384),
        ("autoglm-phone-multilingual", 4_096),
    ] {
        assert_eq!(
            rows[id].max_output_tokens,
            Some(expected),
            "{id} output cap"
        );
    }
}

#[tokio::test]
async fn only_models_with_an_exactly_published_context_window_declare_one() {
    let rows = rows().await;
    let declared: Vec<(&str, u64)> = rows
        .values()
        .filter_map(|row| row.context_window.map(|window| (row.id.as_str(), window)))
        .collect();
    assert_eq!(
        declared,
        vec![
            ("glm-5.2", 1_000_000),
            ("glm-5.3", 1_000_000),
            ("glm-5.3-flash", 1_000_000),
        ],
        "a rounded 200K does not decide between 200000 and 204800"
    );
}

#[tokio::test]
async fn no_model_claims_a_lifecycle_z_ai_never_published() {
    let rows = rows().await;
    for row in rows.values() {
        assert_eq!(row.lifecycle, ModelLifecycle::unknown(), "{}", row.id);
    }
}

#[tokio::test]
async fn an_unannounced_retirement_never_becomes_a_retirement_or_a_stable_claim() {
    let rows = rows().await;
    // Far past the newest model's release, and past any plausible deadline.
    let far_future_ms = 4_102_444_800_000;
    for row in rows.values() {
        assert_eq!(
            row.lifecycle.effective_status(far_future_ms),
            ModelLifecycleStatus::Unknown,
            "{} must stay unannounced, not resolve to retired or stable",
            row.id
        );
        assert!(row.lifecycle.is_selectable(far_future_ms), "{}", row.id);
        assert_eq!(row.lifecycle.retirement_at_ms, None, "{}", row.id);
        assert!(row.lifecycle.replacement_ids.is_empty(), "{}", row.id);
    }
}

#[tokio::test]
async fn text_models_reject_images_and_accept_structured_output() {
    let rows = rows().await;
    for id in ["glm-5.3", "glm-4.6", "glm-4.5-air", "glm-4-32b-0414-128k"] {
        assert_eq!(
            rows[id].capabilities.image_input,
            CapabilitySupport::Unsupported,
            "{id}"
        );
        assert_eq!(
            rows[id].capabilities.structured_output,
            CapabilitySupport::Supported,
            "{id}"
        );
    }
}

#[tokio::test]
async fn vision_models_accept_images_and_are_denied_structured_output() {
    let rows = rows().await;
    for id in [
        "glm-5.3-flash",
        "glm-4.6v",
        "glm-4.5v",
        "autoglm-phone-multilingual",
    ] {
        assert_eq!(
            rows[id].capabilities.image_input,
            CapabilitySupport::Supported,
            "{id}"
        );
        assert_eq!(
            rows[id].capabilities.structured_output,
            CapabilitySupport::Unsupported,
            "{id} — response_format is documented as text-models-only"
        );
    }
}

#[tokio::test]
async fn a_stated_tool_exclusion_is_unsupported_rather_than_unknown() {
    let rows = rows().await;
    assert_eq!(
        rows["glm-4.5v"].capabilities.tools,
        CapabilitySupport::Unsupported,
        "GLM-4.5V is absent from the list of vision models that accept tools"
    );
    assert_eq!(
        rows["glm-4.6v"].capabilities.tools,
        CapabilitySupport::Supported
    );
    assert_eq!(
        rows["glm-5.3"].capabilities.tools,
        CapabilitySupport::Supported
    );
}

#[tokio::test]
async fn a_model_below_the_thinking_floor_is_unsupported_rather_than_unknown() {
    let rows = rows().await;
    assert_eq!(
        rows["glm-4-32b-0414-128k"].capabilities.reasoning,
        CapabilitySupport::Unsupported,
        "thinking is documented as GLM-4.5-series-and-higher only"
    );
    assert_eq!(
        rows["glm-4.5"].capabilities.reasoning,
        CapabilitySupport::Supported
    );
}

#[tokio::test]
async fn a_model_outside_the_stated_families_keeps_unknown_reasoning() {
    let rows = rows().await;
    assert_eq!(
        rows["autoglm-phone-multilingual"].capabilities.reasoning,
        CapabilitySupport::Unknown,
        "outside the GLM-4.5-and-higher numbering; neither supported nor excluded"
    );
}

#[tokio::test]
async fn capabilities_without_evidence_stay_unknown_for_every_row() {
    let rows = rows().await;
    for row in rows.values() {
        for (name, value) in [
            ("document_input", row.capabilities.document_input),
            ("native_web", row.capabilities.native_web),
            ("native_compaction", row.capabilities.native_compaction),
            ("prompt_cache", row.capabilities.prompt_cache),
        ] {
            assert_eq!(
                value,
                CapabilitySupport::Unknown,
                "{} {name} has no per-model evidence and must not be promoted",
                row.id
            );
        }
    }
}

#[tokio::test]
async fn no_row_invents_pricing_performance_or_aliases() {
    let rows = rows().await;
    for row in rows.values() {
        assert_eq!(row.pricing, ModelPricing::unknown(), "{}", row.id);
        assert_eq!(row.performance, ModelPerformance::unknown(), "{}", row.id);
        assert!(row.aliases.is_empty(), "{}", row.id);
    }
}

#[tokio::test]
async fn both_plans_normalize_the_same_rows_under_different_identities() {
    let general = ZaiCatalog::<General>::new();
    let coding = ZaiCatalog::<Coding>::new();
    assert_eq!(general.plan(), ZaiPlan::General);
    assert_eq!(coding.plan(), ZaiPlan::Coding);
    assert_eq!(
        general.fetch(CancellationToken::new()).await.unwrap(),
        coding.fetch(CancellationToken::new()).await.unwrap(),
        "Z.ai publishes one model schema; a plan changes entitlement, not model facts"
    );
    assert_ne!(general.provider().id, coding.provider().id);
}

#[tokio::test]
async fn the_provider_descriptor_matches_the_plans_registry_name() {
    for (descriptor, expected) in [
        (ZaiCatalog::<General>::new().provider(), "zai"),
        (ZaiCatalog::<Coding>::new().provider(), "zai-coding"),
    ] {
        assert_eq!(descriptor.id, expected);
        assert_eq!(
            descriptor.protocols,
            vec![ProviderProtocol::OpenAiChatCompletions]
        );
    }
}

#[tokio::test]
async fn a_cancelled_refresh_fails_instead_of_publishing_a_generation() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = ZaiCatalog::<General>::new()
        .fetch(cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), heycode_llm::CatalogFailureKind::Cancelled);
}

struct ModelsOnly;

impl Plugin for ModelsOnly {
    fn name(&self) -> &'static str {
        "test-models"
    }

    fn apply(&self, _context: &mut Context) -> Result<(), CoreError> {
        Ok(())
    }
}

#[tokio::test]
async fn the_plugin_registers_the_catalog_as_an_effect_and_shutdown_removes_it() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(ModelsOnly),
        zai_catalog_plugin::<General>(),
    ];
    let mut context = compose(&plugins).unwrap();

    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(
        inventory.contributions.iter().any(|row| {
            row.plugin == "catalog-zai"
                && row.kind == heycode_core::ContributionKind::ModelCatalog
                && row.name == "zai"
        }),
        "the catalog contribution must be declared in inventory"
    );

    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let generation = models
        .refresh("zai", CatalogRefreshMode::Force, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(generation.snapshot.models.len(), 20);
    assert_eq!(generation.snapshot.provider.id, "zai");

    context.shutdown();
    assert!(matches!(
        models
            .refresh("zai", CatalogRefreshMode::Force, CancellationToken::new())
            .await,
        Err(CatalogError::UnknownCatalog { .. })
    ));
}

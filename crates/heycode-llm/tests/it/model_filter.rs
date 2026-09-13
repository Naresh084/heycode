//! Capability and lifecycle-aware catalog filter contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::{
    CapabilityFilter, CapabilitySupport, CatalogSnapshot, ModelCapabilities, ModelDescriptor,
    ModelFilter, ModelLifecycle, ModelLifecycleFilter, ModelPerformance, ModelPricing,
    ProviderDescriptor, ProviderProtocol,
};

fn model(
    id: &str,
    lifecycle: ModelLifecycle,
    tools: CapabilitySupport,
    image_input: CapabilitySupport,
    reasoning: CapabilitySupport,
) -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: id.to_owned(),
        display_name: id.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle,
        capabilities: ModelCapabilities {
            tools,
            reasoning,
            image_input,
            ..ModelCapabilities::unknown()
        },
        reasoning: None,
    }
}

fn snapshot() -> CatalogSnapshot {
    CatalogSnapshot {
        provider: ProviderDescriptor {
            id: "provider".to_owned(),
            display_name: "Provider".to_owned(),
            protocols: vec![ProviderProtocol::Unknown],
        },
        models: vec![
            model(
                "a-stable-tools",
                ModelLifecycle::stable(),
                CapabilitySupport::Supported,
                CapabilitySupport::Unsupported,
                CapabilitySupport::Supported,
            ),
            model(
                "b-preview-image",
                ModelLifecycle::preview(),
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
                CapabilitySupport::Unknown,
            ),
            model(
                "c-unknown",
                ModelLifecycle::unknown(),
                CapabilitySupport::Unknown,
                CapabilitySupport::Unknown,
                CapabilitySupport::Unknown,
            ),
            model(
                "d-deprecated",
                ModelLifecycle::deprecated(Some(5_000), Vec::new()),
                CapabilitySupport::Unsupported,
                CapabilitySupport::Unsupported,
                CapabilitySupport::Unsupported,
            ),
            model(
                "e-retired",
                ModelLifecycle::retired(None, Vec::new()),
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
            ),
        ],
        revision: 1,
        fetched_at_ms: 1_000,
    }
}

fn ids(models: Vec<&ModelDescriptor>) -> Vec<&str> {
    models.into_iter().map(|model| model.id.as_str()).collect()
}

#[test]
fn supported_unsupported_and_unknown_are_three_distinct_filters() {
    let snapshot = snapshot();
    let mut filter = ModelFilter::all();
    filter.tools = CapabilityFilter::Supported;
    assert_eq!(
        ids(snapshot.filter_models(&filter, 2_000)),
        ["a-stable-tools", "b-preview-image", "e-retired"]
    );

    filter.tools = CapabilityFilter::Unsupported;
    assert_eq!(
        ids(snapshot.filter_models(&filter, 2_000)),
        ["d-deprecated"]
    );

    filter.tools = CapabilityFilter::Unknown;
    assert_eq!(ids(snapshot.filter_models(&filter, 2_000)), ["c-unknown"]);
}

#[test]
fn capability_filters_compose_without_treating_unknown_as_support() {
    let filter = ModelFilter {
        tools: CapabilityFilter::Supported,
        image_input: CapabilityFilter::Supported,
        reasoning: CapabilityFilter::Any,
        lifecycle: ModelLifecycleFilter::Selectable,
    };
    assert_eq!(
        ids(snapshot().filter_models(&filter, 2_000)),
        ["b-preview-image"]
    );
}

#[test]
fn stable_and_selectable_lifecycle_filters_are_explicitly_different() {
    let snapshot = snapshot();
    let stable = ModelFilter {
        lifecycle: ModelLifecycleFilter::Stable,
        ..ModelFilter::all()
    };
    assert_eq!(
        ids(snapshot.filter_models(&stable, 2_000)),
        ["a-stable-tools"]
    );

    let selectable = ModelFilter::selectable();
    assert_eq!(
        ids(snapshot.filter_models(&selectable, 2_000)),
        [
            "a-stable-tools",
            "b-preview-image",
            "c-unknown",
            "d-deprecated",
        ]
    );
}

#[test]
fn retirement_deadline_changes_filter_result_at_the_exact_boundary() {
    let snapshot = snapshot();
    let deprecated = ModelFilter {
        lifecycle: ModelLifecycleFilter::Deprecated,
        ..ModelFilter::all()
    };
    assert_eq!(
        ids(snapshot.filter_models(&deprecated, 4_999)),
        ["d-deprecated"]
    );
    assert!(snapshot.filter_models(&deprecated, 5_000).is_empty());

    let retired = ModelFilter {
        lifecycle: ModelLifecycleFilter::Retired,
        ..ModelFilter::all()
    };
    assert_eq!(
        ids(snapshot.filter_models(&retired, 5_000)),
        ["d-deprecated", "e-retired"]
    );
}

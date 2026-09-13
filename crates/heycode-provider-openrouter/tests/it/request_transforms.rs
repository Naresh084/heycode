//! N05 provider-owned OpenRouter transform registry contribution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_llm::{
    CallPurpose, CapabilitySupport, InputModality, RequestDraft, RequestTransformCost,
    RequestTransformEffect, RequestTransformRegistry, RequestTransformRequest,
    SERVICE_REQUEST_TRANSFORMS, llm_plugin, request_transforms_plugin,
};
use heycode_provider_openrouter::{
    OpenRouterTransformPolicy, openrouter_request_transforms_plugin,
};

fn draft(option: heycode_core::ProviderRequestOption) -> RequestDraft {
    RequestDraft {
        provider: "openrouter".to_owned(),
        model: "z-ai/glm-5.3-flash".to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: None,
        inputs: vec![heycode_llm::InferenceInput::Message(
            heycode_llm::ChatMessage::user("hello"),
        )],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: vec![option],
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    }
}

#[test]
fn all_disabled_policy_registers_three_explicit_unknown_effective_rows() {
    let selection = heycode_llm::LlmSelection {
        provider_name: "fake".to_owned(),
        model: "fake/model".to_owned(),
    };
    let providers: Vec<Arc<dyn heycode_llm::Provider>> = vec![Arc::new(
        heycode_llm::testing::FakeProvider::named("fake", "fake/model", Vec::new()),
    )];
    let policy = OpenRouterTransformPolicy::all_disabled();
    let plugins = [
        llm_plugin(selection, providers),
        request_transforms_plugin(),
        openrouter_request_transforms_plugin(policy),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context
        .get::<RequestTransformRegistry>(SERVICE_REQUEST_TRANSFORMS)
        .unwrap();
    let rows = registry.descriptors().unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id().as_str()).collect::<Vec<_>>(),
        [
            "openrouter:context-compression",
            "openrouter:file-parser",
            "openrouter:response-healing",
        ]
    );
    assert_eq!(
        rows.iter().map(|row| row.effect()).collect::<Vec<_>>(),
        [
            RequestTransformEffect::RewritePromptAndRoute,
            RequestTransformEffect::ParseDocuments,
            RequestTransformEffect::RewriteResponse,
        ]
    );
    assert!(rows.iter().all(|row| {
        row.request() == RequestTransformRequest::Disabled
            && row.effective() == CapabilitySupport::Unknown
            && row.cost().is_none()
    }));

    let option = policy
        .provider_option(
            heycode_provider_openrouter::OpenRouterTransformRequestContext::new(true, false),
        )
        .unwrap();
    let mut request = draft(option);
    registry.apply(&mut request).unwrap();
    assert_eq!(request.provider_options.len(), 1);
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for name in [
        "openrouter:context-compression",
        "openrouter:file-parser",
        "openrouter:response-healing",
    ] {
        assert!(inventory.contributions.iter().any(|row| {
            row.plugin == "request-transforms-openrouter"
                && row.kind == heycode_core::ContributionKind::RequestTransform
                && row.name == name
        }));
    }

    context.shutdown();
    assert!(registry.descriptors().unwrap().is_empty());
}

#[test]
fn enabled_rows_preserve_effect_and_nonzero_cost_evidence() {
    let policy = OpenRouterTransformPolicy::all_disabled()
        .with_context_compression()
        .with_document_parsing(heycode_provider_openrouter::OpenRouterPdfEngine::MistralOcr);
    let provider = heycode_provider_openrouter::OpenRouterRequestTransforms::new(policy);
    let rows = heycode_llm::RequestTransformProvider::descriptors(&provider).unwrap();
    let compression = rows
        .iter()
        .find(|row| row.id().as_str().ends_with("context-compression"))
        .unwrap();
    assert_eq!(compression.request(), RequestTransformRequest::Enabled);
    assert_eq!(compression.cost(), Some(RequestTransformCost::Unknown));
    let parser = rows
        .iter()
        .find(|row| row.id().as_str().ends_with("file-parser"))
        .unwrap();
    assert!(matches!(
        parser.cost(),
        Some(RequestTransformCost::PublishedPerThousandPages(price))
            if price.pico_units() == 2_000_000_000_000
    ));
    assert!(
        rows.iter()
            .all(|row| row.effective() == CapabilitySupport::Unknown)
    );
}

//! N05 effect-owned request-transform registry and P10 application layer.

use std::sync::Arc;

use heycode_core::{Context, ProviderProtocol};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, InputModality,
    PriceCurrency, ProviderRequestContext, RequestDraft, RequestTransformCost,
    RequestTransformDescriptor, RequestTransformEffect, RequestTransformError, RequestTransformId,
    RequestTransformProvider, RequestTransformRegistry, RequestTransformRequest,
    SERVICE_PROVIDER_INTERCEPTION, SERVICE_REQUEST_TRANSFORMS, llm_plugin,
    request_transforms_plugin,
};
use tokio_util::sync::CancellationToken;

fn descriptor(provider: &str, id: &str) -> RequestTransformDescriptor {
    RequestTransformDescriptor::new(
        RequestTransformId::new(id).unwrap(),
        provider,
        RequestTransformEffect::RewritePromptAndRoute,
        RequestTransformRequest::Enabled,
        CapabilitySupport::Unknown,
        Some(
            RequestTransformCost::published_per_thousand_pages(
                PriceCurrency::Usd,
                2_000_000_000_000,
            )
            .unwrap(),
        ),
    )
    .unwrap()
}

struct FixtureTransform {
    provider: &'static str,
    kind: &'static str,
    data: serde_json::Value,
    descriptors: Vec<RequestTransformDescriptor>,
}

impl RequestTransformProvider for FixtureTransform {
    fn provider(&self) -> &str {
        self.provider
    }

    fn descriptors(&self) -> Result<Vec<RequestTransformDescriptor>, RequestTransformError> {
        Ok(self.descriptors.clone())
    }

    fn provider_option(
        &self,
        _draft: &RequestDraft,
    ) -> Result<heycode_core::ProviderRequestOption, RequestTransformError> {
        heycode_core::ProviderRequestOption::new(self.provider, self.kind, self.data.clone())
            .map_err(|_| RequestTransformError::InvalidProviderOption)
    }
}

fn fixture(provider: &'static str, id: &'static str) -> Arc<dyn RequestTransformProvider> {
    Arc::new(FixtureTransform {
        provider,
        kind: "transforms",
        data: serde_json::json!({"plugins":[{"id":"fixture","enabled":true}]}),
        descriptors: vec![descriptor(provider, id)],
    })
}

fn draft(provider: &str) -> RequestDraft {
    RequestDraft {
        provider: provider.to_owned(),
        model: "provider/model".to_owned(),
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
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    }
}

#[test]
fn registry_is_sorted_unique_and_effect_owned() {
    let mut context = Context::new();
    let registry = RequestTransformRegistry::default();
    registry
        .register(&context, fixture("zeta", "zeta:compress"))
        .unwrap();
    registry
        .register(&context, fixture("alpha", "alpha:compress"))
        .unwrap();
    assert_eq!(
        registry
            .descriptors()
            .unwrap()
            .iter()
            .map(|row| row.id().as_str())
            .collect::<Vec<_>>(),
        ["alpha:compress", "zeta:compress"]
    );
    assert!(matches!(
        registry.register(&context, fixture("alpha", "alpha:other")),
        Err(RequestTransformError::DuplicateProvider { .. })
    ));

    context.shutdown();
    assert!(registry.descriptors().unwrap().is_empty());
}

#[test]
fn apply_inserts_once_and_refuses_a_conflicting_durable_option() {
    let context = Context::new();
    let registry = RequestTransformRegistry::default();
    registry
        .register(&context, fixture("provider", "provider:fixture"))
        .unwrap();
    let mut request = draft("provider");
    registry.apply(&mut request).unwrap();
    registry.apply(&mut request).unwrap();
    assert_eq!(request.provider_options.len(), 1);

    request.provider_options = vec![
        heycode_core::ProviderRequestOption::new(
            "provider",
            "transforms",
            serde_json::json!({"plugins":[]}),
        )
        .unwrap(),
    ];
    assert!(matches!(
        registry.apply(&mut request),
        Err(RequestTransformError::ConflictingProviderOption { .. })
    ));
}

#[tokio::test]
async fn composed_p10_layer_applies_late_registered_transform_before_return() {
    let selection = heycode_llm::LlmSelection {
        provider_name: "fake".to_owned(),
        model: "provider/model".to_owned(),
    };
    let providers: Vec<Arc<dyn heycode_llm::Provider>> = vec![Arc::new(
        heycode_llm::testing::FakeProvider::named("fake", "provider/model", Vec::new()),
    )];
    let plugins = [
        llm_plugin(selection, providers),
        request_transforms_plugin(),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let registry = context
        .get::<RequestTransformRegistry>(SERVICE_REQUEST_TRANSFORMS)
        .unwrap();
    registry
        .register(&context, fixture("fake", "fake:fixture"))
        .unwrap();
    let interception = context
        .get::<heycode_llm::ProviderInterception>(SERVICE_PROVIDER_INTERCEPTION)
        .unwrap();
    let request = interception
        .intercept_request(
            ProviderRequestContext::new(
                "fake",
                "provider/model",
                CallPurpose::Conversation,
                AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
            )
            .unwrap(),
            draft("fake"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(request.provider_options.len(), 1);
    assert_eq!(request.provider_options[0].kind(), "transforms");
    assert_eq!(
        context.owner_of(SERVICE_REQUEST_TRANSFORMS),
        Some("request-transforms")
    );
    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "request-transforms"
            && row.kind == heycode_core::ContributionKind::InterceptionLayer
            && row.name == "provider/request:transforms"
    }));
}

#[test]
fn descriptor_and_cost_boundaries_reject_untruthful_values() {
    assert!(RequestTransformId::new("").is_err());
    assert!(RequestTransformId::new("not spaced").is_err());
    assert!(
        RequestTransformDescriptor::new(
            RequestTransformId::new("provider:transform").unwrap(),
            "bad provider",
            RequestTransformEffect::RewriteResponse,
            RequestTransformRequest::Disabled,
            CapabilitySupport::Supported,
            Some(RequestTransformCost::DocumentedFree),
        )
        .is_err()
    );
    assert!(RequestTransformCost::published_per_thousand_pages(PriceCurrency::Usd, 0).is_err());
    assert_ne!(
        RequestTransformCost::Unknown,
        RequestTransformCost::DocumentedFree
    );
    assert_ne!(
        ProviderProtocol::Unknown,
        ProviderProtocol::OpenAiChatCompletions
    );
}

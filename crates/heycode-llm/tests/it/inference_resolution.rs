//! Explicit request-draft to one-shot resolved-call contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt as _;
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatDocument,
    ChatImage, ChatMessage, InferenceAdapter, InferenceEvent, InferenceInput, InferenceStream,
    InferenceTarget, InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle,
    ModelPerformance, ModelPricing, NativeFeature, ProviderDescriptor, ProviderProtocol,
    ReasoningEffortId, ReasoningEffortOptions, RequestDraft, RequestedCapability, ResolveError,
    ResolveSpec, ResolvedCall, ToolSpec, resolve_request,
};

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "provider".to_owned(),
        display_name: "Provider".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

fn model(capabilities: ModelCapabilities) -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "provider/model".to_owned(),
        display_name: "Model".to_owned(),
        aliases: vec!["provider/model-alias".to_owned()],
        created_at_ms: None,
        context_window: Some(16_384),
        max_output_tokens: Some(2_048),
        lifecycle: ModelLifecycle::stable(),
        capabilities,
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "provider".to_owned(),
        model: "provider/model-alias".to_owned(),
        catalog_revision: Some(7),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: Some("system".to_owned()),
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: Some(0.2),
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    }
}

fn spec() -> ResolveSpec {
    ResolveSpec {
        protocol: ProviderProtocol::OpenAiChatCompletions,
        target: InferenceTarget::Http {
            base_url: "https://example.test/v1".to_owned(),
        },
        authentication: AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
        default_max_output_tokens: Some(1_024),
        reasoning_efforts: vec![
            ReasoningEffortId::new("low").unwrap(),
            ReasoningEffortId::new("high").unwrap(),
        ],
        default_reasoning_effort: Some(ReasoningEffortId::new("low").unwrap()),
    }
}

fn tool() -> ToolSpec {
    ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    }
}

fn image() -> ChatImage {
    ChatImage::new(
        heycode_core::AttachmentMediaType::new("image/png").unwrap(),
        vec![1],
    )
    .unwrap()
}

fn document() -> ChatDocument {
    ChatDocument::new(
        heycode_core::AttachmentMediaType::new("application/pdf").unwrap(),
        "guide.pdf",
        b"%PDF-".to_vec(),
    )
    .unwrap()
}

struct TestAdapter {
    dispatches: AtomicUsize,
    descriptor: ProviderDescriptor,
    spec: ResolveSpec,
}

impl TestAdapter {
    fn new(spec: ResolveSpec) -> Self {
        Self {
            dispatches: AtomicUsize::new(0),
            descriptor: provider(),
            spec,
        }
    }
}

impl InferenceAdapter for TestAdapter {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.spec.authentication.clone()
    }

    fn reasoning_effort_options(
        &self,
        model: &ModelDescriptor,
    ) -> Result<Option<ReasoningEffortOptions>, ResolveError> {
        ReasoningEffortOptions::for_model(
            model,
            self.spec.reasoning_efforts.clone(),
            self.spec.default_reasoning_effort.clone(),
        )
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        resolve_request(&self.descriptor, draft, model, &self.spec)
    }

    fn stream(&self, _call: ResolvedCall) -> InferenceStream {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter([Ok(InferenceEvent::Finish(
            heycode_llm::FinishReason::Stop,
        ))]))
    }
}

#[test]
fn adapter_exposes_exact_reasoning_effort_order_and_default_for_supported_model() {
    let adapter = TestAdapter::new(spec());
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.reasoning = CapabilitySupport::Supported;

    let options = adapter
        .reasoning_effort_options(&model(capabilities))
        .unwrap()
        .expect("reasoning-capable model exposes adapter-owned choices");

    assert_eq!(
        options
            .choices()
            .iter()
            .map(ReasoningEffortId::as_str)
            .collect::<Vec<_>>(),
        ["low", "high"]
    );
    assert_eq!(
        options.default().map(ReasoningEffortId::as_str),
        Some("low")
    );
}

#[test]
fn effort_options_distinguish_unsupported_models_and_invalid_adapter_metadata() {
    let adapter = TestAdapter::new(spec());
    assert_eq!(
        adapter
            .reasoning_effort_options(&model(ModelCapabilities::unknown()))
            .unwrap(),
        None
    );

    let duplicate = ReasoningEffortId::new("same").unwrap();
    assert!(matches!(
        ReasoningEffortOptions::new(
            vec![duplicate.clone(), duplicate],
            Some(ReasoningEffortId::new("same").unwrap())
        ),
        Err(ResolveError::InvalidAdapter {
            field: "reasoning_efforts",
            ..
        })
    ));
}

#[test]
fn unsupported_and_unknown_tool_capability_fail_before_transport() {
    for (support, expected_unproven) in [
        (CapabilitySupport::Unsupported, false),
        (CapabilitySupport::Unknown, true),
    ] {
        let adapter = TestAdapter::new(spec());
        let mut request = draft();
        request.tools.push(tool());
        let mut capabilities = ModelCapabilities::unknown();
        capabilities.tools = support;
        let error = adapter.resolve(request, &model(capabilities)).unwrap_err();
        assert!(match error {
            ResolveError::Unsupported { capability, .. } => {
                !expected_unproven && capability == RequestedCapability::Tools
            }
            ResolveError::Unproven { capability, .. } => {
                expected_unproven && capability == RequestedCapability::Tools
            }
            _ => false,
        });
        assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn image_reasoning_structured_and_native_requests_each_require_explicit_support() {
    let cases = [
        RequestedCapability::ImageInput,
        RequestedCapability::DocumentInput,
        RequestedCapability::Reasoning,
        RequestedCapability::StructuredOutput,
        RequestedCapability::NativeWeb,
        RequestedCapability::NativeCompaction,
        RequestedCapability::PromptCache,
    ];
    for capability in cases {
        let adapter = TestAdapter::new(spec());
        let mut request = draft();
        match capability {
            RequestedCapability::ImageInput => {
                request.inputs = vec![InferenceInput::Message(ChatMessage::user_with_images(
                    "hello",
                    vec![image()],
                ))];
                request.input_modalities.push(InputModality::Image);
            }
            RequestedCapability::DocumentInput => {
                request.inputs = vec![InferenceInput::Message(ChatMessage::user_with_media(
                    "hello",
                    Vec::new(),
                    vec![document()],
                ))];
                request.input_modalities.push(InputModality::Document);
            }
            RequestedCapability::Reasoning => {
                request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
            }
            RequestedCapability::StructuredOutput => {
                request.structured_output = Some(serde_json::json!({"type": "object"}));
            }
            RequestedCapability::NativeWeb => request.native_features.push(NativeFeature::Web),
            RequestedCapability::NativeCompaction => {
                request.native_features.push(NativeFeature::Compaction);
            }
            RequestedCapability::PromptCache => {
                request.native_features.push(NativeFeature::PromptCache);
            }
            RequestedCapability::Tools => unreachable!(),
        }
        let error = adapter
            .resolve(request, &model(ModelCapabilities::unknown()))
            .unwrap_err();
        assert!(matches!(
            error,
            ResolveError::Unproven { capability: found, .. } if found == capability
        ));
        assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn unsupported_reasoning_effort_is_rejected_without_clamping() {
    let adapter = TestAdapter::new(spec());
    let mut request = draft();
    request.reasoning_effort = Some(ReasoningEffortId::new("ultra").unwrap());
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.reasoning = CapabilitySupport::Supported;

    let error = adapter.resolve(request, &model(capabilities)).unwrap_err();
    assert!(matches!(
        error,
        ResolveError::UnsupportedReasoningEffort { requested, available }
            if requested.as_str() == "ultra"
                && available.iter().map(ReasoningEffortId::as_str).collect::<Vec<_>>()
                    == ["low", "high"]
    ));
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn valid_resolution_materializes_defaults_and_dispatches_exactly_once_by_ownership() {
    let adapter = TestAdapter::new(spec());
    let mut request = draft();
    request.tools.push(tool());
    request.inputs = vec![InferenceInput::Message(ChatMessage::user_with_images(
        "hello",
        vec![image()],
    ))];
    request.input_modalities.push(InputModality::Image);
    request.structured_output = Some(serde_json::json!({"type": "object"}));
    request.native_features = vec![
        NativeFeature::Web,
        NativeFeature::Compaction,
        NativeFeature::PromptCache,
    ];
    let capabilities = ModelCapabilities {
        tools: CapabilitySupport::Supported,
        reasoning: CapabilitySupport::Supported,
        image_input: CapabilitySupport::Supported,
        document_input: CapabilitySupport::Supported,
        structured_output: CapabilitySupport::Supported,
        native_web: CapabilitySupport::Supported,
        native_compaction: CapabilitySupport::Supported,
        prompt_cache: CapabilitySupport::Supported,
    };

    let call = adapter.resolve(request, &model(capabilities)).unwrap();
    assert_eq!(call.provider(), "provider");
    assert_eq!(call.model(), "provider/model");
    assert_eq!(call.catalog_revision(), Some(7));
    assert_eq!(call.max_output_tokens(), Some(1_024));
    assert_eq!(call.reasoning_effort().unwrap().as_str(), "low");
    assert!(call.defaults().max_output_tokens);
    assert!(call.defaults().reasoning_effort);
    assert_eq!(call.protocol(), ProviderProtocol::OpenAiChatCompletions);
    assert_eq!(call.tools().len(), 1);
    assert_eq!(call.purpose(), CallPurpose::Conversation);

    let chunks: Vec<_> = adapter.stream(call).collect().await;
    assert_eq!(chunks.len(), 1);
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 1);
}

#[test]
fn invalid_limits_routes_and_retirement_fail_during_resolution() {
    let adapter = TestAdapter::new(spec());
    let mut too_large = draft();
    too_large.max_output_tokens = Some(4_096);
    assert!(matches!(
        adapter.resolve(too_large, &model(ModelCapabilities::unknown())),
        Err(ResolveError::OutputLimitExceeded {
            requested: 4_096,
            maximum: 2_048
        })
    ));

    let mut wrong_provider = draft();
    wrong_provider.provider = "other".to_owned();
    assert!(matches!(
        adapter.resolve(wrong_provider, &model(ModelCapabilities::unknown())),
        Err(ResolveError::ProviderMismatch { .. })
    ));

    let mut retired = model(ModelCapabilities::unknown());
    retired.lifecycle = ModelLifecycle::retired(Some(1_000), Vec::new());
    assert!(matches!(
        adapter.resolve(draft(), &retired),
        Err(ResolveError::RetiredModel { .. })
    ));
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);

    let mut invalid_default = spec();
    invalid_default.default_max_output_tokens = Some(4_096);
    assert!(matches!(
        TestAdapter::new(invalid_default).resolve(draft(), &model(ModelCapabilities::unknown())),
        Err(ResolveError::InvalidAdapter {
            field: "default_max_output_tokens",
            ..
        })
    ));

    let mut wrong_protocol = spec();
    wrong_protocol.protocol = ProviderProtocol::OpenAiResponses;
    assert!(matches!(
        TestAdapter::new(wrong_protocol).resolve(draft(), &model(ModelCapabilities::unknown())),
        Err(ResolveError::ProtocolUnsupported { .. })
    ));
}

#[test]
fn malformed_tool_schema_and_target_are_rejected_at_the_boundary() {
    let mut request = draft();
    let mut malformed = tool();
    malformed.parameters = serde_json::json!([]);
    request.tools.push(malformed);
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    assert!(matches!(
        TestAdapter::new(spec()).resolve(request, &model(capabilities)),
        Err(ResolveError::InvalidRequest { field: "tools", .. })
    ));

    let mut invalid_spec = spec();
    invalid_spec.target = InferenceTarget::Http {
        base_url: "file:///tmp/model".to_owned(),
    };
    assert!(matches!(
        TestAdapter::new(invalid_spec).resolve(draft(), &model(ModelCapabilities::unknown())),
        Err(ResolveError::InvalidAdapter {
            field: "target",
            ..
        })
    ));

    let mut no_input = draft();
    no_input.input_modalities.clear();
    assert!(matches!(
        TestAdapter::new(spec()).resolve(no_input, &model(ModelCapabilities::unknown())),
        Err(ResolveError::InvalidRequest {
            field: "input_modalities",
            ..
        })
    ));

    let mut invalid_reasoning = spec();
    invalid_reasoning.default_reasoning_effort = Some(ReasoningEffortId::new("ultra").unwrap());
    assert!(matches!(
        TestAdapter::new(invalid_reasoning).resolve(draft(), &model(ModelCapabilities::unknown())),
        Err(ResolveError::InvalidAdapter {
            field: "default_reasoning_effort",
            ..
        })
    ));
}

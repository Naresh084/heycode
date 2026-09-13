//! Provider/model descriptor and tri-state capability contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::{
    CallPurpose, CapabilitySupport, InferenceAdapter, InferenceInput, InputModality,
    ModelCapabilities, ModelDescriptor, ModelLifecycle, Provider, ProviderDescriptor, ProviderInfo,
    ProviderOptionContext, ProviderProtocol, RequestDraft,
};

#[test]
fn capability_support_never_collapses_unknown_into_false() {
    assert_eq!(CapabilitySupport::Supported.as_bool(), Some(true));
    assert_eq!(CapabilitySupport::Unsupported.as_bool(), Some(false));
    assert_eq!(CapabilitySupport::Unknown.as_bool(), None);
    assert!(!CapabilitySupport::Unknown.is_supported());
}

#[test]
fn unknown_model_descriptor_is_conservative_and_keeps_identity() {
    let descriptor = ModelDescriptor::unknown("provider/model");
    assert_eq!(descriptor.id, "provider/model");
    assert_eq!(descriptor.display_name, "provider/model");
    assert!(descriptor.aliases.is_empty());
    assert_eq!(descriptor.context_window, None);
    assert_eq!(descriptor.max_output_tokens, None);
    assert_eq!(descriptor.lifecycle, ModelLifecycle::unknown());
    assert_eq!(descriptor.capabilities, ModelCapabilities::unknown());
}

struct MinimalProvider;

impl Provider for MinimalProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "minimal".to_owned(),
            default_model: "minimal/default".to_owned(),
        }
    }

    fn stream(&self, _request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
        Box::pin(futures::stream::empty())
    }
}

#[test]
fn provider_trait_defaults_project_identity_without_inventing_capabilities() {
    let provider = MinimalProvider;
    assert_eq!(
        provider.descriptor(),
        ProviderDescriptor {
            id: "minimal".to_owned(),
            display_name: "minimal".to_owned(),
            protocols: vec![ProviderProtocol::Unknown],
        }
    );
    assert_eq!(
        provider.describe_model("minimal/other"),
        ModelDescriptor::unknown("minimal/other")
    );
}

#[test]
fn request_specific_options_receive_the_exact_selected_model_and_native_routes() {
    struct ContextProvider;

    impl Provider for ContextProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "context-provider".to_owned(),
                default_model: "context-provider/default".to_owned(),
            }
        }

        fn request_options_for(
            &self,
            context: ProviderOptionContext<'_>,
        ) -> Result<Vec<heycode_core::ProviderRequestOption>, heycode_llm::ResolveError> {
            let route = context.native_tool_routes().first().ok_or_else(|| {
                heycode_llm::ResolveError::InvalidRequest {
                    field: "native_tool_routes",
                    message: "one selected route is required".to_owned(),
                }
            })?;
            heycode_core::ProviderRequestOption::new(
                "context-provider",
                "selection",
                serde_json::json!({
                    "model":context.model().id,
                    "logical":route.logical(),
                    "implementation":route.implementation(),
                }),
            )
            .map(|option| vec![option])
            .map_err(|_| heycode_llm::ResolveError::InvalidRequest {
                field: "provider_options",
                message: "selection option is invalid".to_owned(),
            })
        }

        fn stream(&self, _request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
            Box::pin(futures::stream::empty())
        }
    }

    let model = ModelDescriptor::unknown("context-provider/selected");
    let route = heycode_core::NativeToolRoute::new(
        "web_search",
        "context-provider:web",
        heycode_core::NativeToolImplementationKind::Provider,
        Some("context-provider".to_owned()),
    )
    .unwrap();
    let provider = ContextProvider;
    let options = provider
        .request_options_for(ProviderOptionContext::new(
            &model,
            std::slice::from_ref(&route),
        ))
        .unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].data()["model"], model.id);
    assert_eq!(options[0].data()["logical"], "web_search");
    assert_eq!(options[0].data()["implementation"], "context-provider:web");
}

#[test]
fn request_specific_option_default_preserves_static_provider_policy() {
    struct StaticProvider;

    impl Provider for StaticProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "static-provider".to_owned(),
                default_model: "static-provider/default".to_owned(),
            }
        }

        fn request_options(&self) -> Vec<heycode_core::ProviderRequestOption> {
            vec![
                heycode_core::ProviderRequestOption::new(
                    "static-provider",
                    "policy",
                    serde_json::json!({"enabled":false}),
                )
                .unwrap(),
            ]
        }

        fn stream(&self, _request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
            Box::pin(futures::stream::empty())
        }
    }

    let provider = StaticProvider;
    let model = ModelDescriptor::unknown("static-provider/selected");
    assert_eq!(
        provider
            .request_options_for(ProviderOptionContext::new(&model, &[]))
            .unwrap(),
        provider.request_options()
    );
}

#[test]
fn shipped_adapters_declare_protocol_but_not_model_capability_guesses() {
    let deepseek = heycode_llm::DeepSeekProvider::from_key("test", None).unwrap();
    let openrouter = heycode_llm::OpenRouterProvider::from_key(
        "test",
        None,
        super::openrouter_transform_options(),
    )
    .unwrap();
    assert_eq!(
        Provider::descriptor(&deepseek).protocols,
        [ProviderProtocol::OpenAiChatCompletions]
    );
    assert_eq!(
        Provider::descriptor(&openrouter).protocols,
        [ProviderProtocol::OpenAiChatCompletions]
    );
    assert_eq!(
        openrouter
            .describe_model("unknown/model")
            .capabilities
            .tools,
        CapabilitySupport::Unknown
    );
}

#[test]
fn shipped_branded_providers_also_expose_conservative_new_inference_contract() {
    let adapters: Vec<Box<dyn InferenceAdapter>> = vec![
        Box::new(heycode_llm::DeepSeekProvider::from_key("test", None).unwrap()),
        Box::new(
            heycode_llm::OpenRouterProvider::from_key(
                "test",
                None,
                super::openrouter_transform_options(),
            )
            .unwrap(),
        ),
    ];
    for adapter in adapters {
        let descriptor = adapter.descriptor();
        let model_id = format!("{}/unlisted", descriptor.id);
        let model = ModelDescriptor::unknown(&model_id);
        let draft = RequestDraft {
            provider: descriptor.id.clone(),
            model: model_id,
            catalog_revision: None,
            catalog_fetched_at_ms: None,
            effective_at_ms: 1,
            system: None,
            inputs: vec![InferenceInput::Message(heycode_llm::ChatMessage::user(
                "hi",
            ))],
            tools: Vec::new(),
            input_modalities: vec![InputModality::Text],
            reasoning_effort: None,
            structured_output: None,
            native_features: Vec::new(),
            native_tool_routes: Vec::new(),
            provider_options: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            purpose: CallPurpose::Evaluation,
        };
        let resolved = adapter.resolve(draft, &model).unwrap();
        assert_eq!(resolved.protocol(), ProviderProtocol::OpenAiChatCompletions);
        assert_eq!(resolved.model(), model.id);
    }
}

//! Provider-owned zero-configuration hosted-tool product policy.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::{ContributionKind, ProviderProtocol};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEventStream};
use heycode_llm::{
    CapabilitySupport, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, Provider, ProviderOptionContext,
};
use heycode_provider_openai::{
    OPENAI_BRIDGE_COMPLETE_HOSTED_TOOL_KINDS, OPENAI_GPT_5_6_SOL, OpenAiHostedToolKind,
    OpenAiProvider, configure_openai_bridge_complete_hosted_tools,
    openai_bridge_complete_native_tools_plugin,
};
use tokio_util::sync::CancellationToken;

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: OPENAI_GPT_5_6_SOL.to_owned(),
        display_name: OPENAI_GPT_5_6_SOL.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            native_web: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

#[test]
fn product_policy_contract_matches_candidate_plugin_and_disposes() {
    assert_eq!(
        OPENAI_BRIDGE_COMPLETE_HOSTED_TOOL_KINDS,
        [
            OpenAiHostedToolKind::WebSearch,
            OpenAiHostedToolKind::CodeInterpreter,
            OpenAiHostedToolKind::HostedShell,
        ]
    );

    let declaration = openai_bridge_complete_native_tools_plugin();
    let declared = declaration
        .inventory()
        .into_iter()
        .filter(|row| row.kind == ContributionKind::NativeTool)
        .map(|row| row.name)
        .collect::<Vec<_>>();
    assert_eq!(
        declared,
        vec![
            "openai:web_search",
            "openai:code_interpreter",
            "openai:hosted_shell",
        ]
    );

    let provider = OpenAiProvider::new(
        HttpService::new(Arc::new(DeadTransport)),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let provider = configure_openai_bridge_complete_hosted_tools(provider).unwrap();

    let plugins = vec![
        heycode_native_tools::native_tools_plugin(),
        openai_bridge_complete_native_tools_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = registry.resolve("openai").unwrap();
    assert_eq!(
        routes
            .iter()
            .map(|route| route.implementation())
            .collect::<Vec<_>>(),
        vec![
            "openai:code_interpreter",
            "openai:hosted_shell",
            "openai:web_search",
        ]
    );
    let options =
        Provider::request_options_for(&provider, ProviderOptionContext::new(&model(), &routes))
            .unwrap();
    let hosted = options
        .iter()
        .find(|option| option.kind() == "hosted-tools")
        .expect("the matching product policy must materialize one option");
    assert_eq!(hosted.provider(), "openai");
    assert_eq!(
        hosted.data()["definitions"],
        serde_json::json!([
            {"type":"web_search"},
            {"type":"code_interpreter","container":{"type":"auto"}},
            {
                "type":"shell",
                "allowed_callers":["direct"],
                "environment":{
                    "type":"container_auto",
                    "network_policy":{"type":"disabled"}
                }
            }
        ])
    );
    assert_eq!(Provider::descriptor(&provider).protocols.len(), 2);
    assert!(
        Provider::descriptor(&provider)
            .protocols
            .contains(&ProviderProtocol::OpenAiResponses)
    );

    context.shutdown();
    assert!(registry.resolve("openai").unwrap().is_empty());
}

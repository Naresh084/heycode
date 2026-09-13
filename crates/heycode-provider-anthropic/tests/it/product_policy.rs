//! Provider-owned default-model atomic server-tool product policy.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::ContributionKind;
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEventStream};
use heycode_llm::{
    CapabilitySupport, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, Provider, ProviderOptionContext,
};
use heycode_provider_anthropic::{
    ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_DEFAULT_ATOMIC_SERVER_TOOL_KINDS, AnthropicProvider,
    AnthropicServerToolKind, anthropic_default_native_tools_plugin,
    configure_anthropic_default_server_tools,
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
        id: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        display_name: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
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
        ANTHROPIC_DEFAULT_ATOMIC_SERVER_TOOL_KINDS,
        [
            AnthropicServerToolKind::WebSearch,
            AnthropicServerToolKind::CodeExecution,
        ]
    );

    let declaration = anthropic_default_native_tools_plugin();
    let declared = declaration
        .inventory()
        .into_iter()
        .filter(|row| row.kind == ContributionKind::NativeTool)
        .map(|row| row.name)
        .collect::<Vec<_>>();
    assert_eq!(
        declared,
        vec!["anthropic:web_search", "anthropic:code_execution"]
    );

    let provider = AnthropicProvider::new(
        HttpService::new(Arc::new(DeadTransport)),
        "test-key",
        Some(ANTHROPIC_CLAUDE_OPUS_5.to_owned()),
    )
    .unwrap();
    let provider = configure_anthropic_default_server_tools(provider).unwrap();

    let plugins = vec![
        heycode_native_tools::native_tools_plugin(),
        anthropic_default_native_tools_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = registry.resolve("anthropic").unwrap();
    assert_eq!(
        routes
            .iter()
            .map(|route| route.implementation())
            .collect::<Vec<_>>(),
        vec!["anthropic:code_execution", "anthropic:web_search"]
    );
    let options =
        Provider::request_options_for(&provider, ProviderOptionContext::new(&model(), &routes))
            .unwrap();
    let server_tools = options
        .iter()
        .find(|option| option.kind() == "server-tools")
        .expect("the matching atomic policy must materialize one option");
    assert_eq!(server_tools.provider(), "anthropic");
    assert_eq!(
        server_tools.data()["tools"],
        serde_json::json!([
            {"type":"web_search_20250305","name":"web_search","max_uses":5},
            {"type":"code_execution_20260521","name":"code_execution"}
        ])
    );

    context.shutdown();
    assert!(registry.resolve("anthropic").unwrap().is_empty());
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod anthropic_protocol;
mod bedrock_protocol;
mod catalog_persistence;
mod catalog_registry;
mod chat_protocol;
mod context_projection;
mod credential_resolution;
mod deepseek_thinking;
mod descriptors;
mod experimental_audio;
mod gemini_protocol;
mod inference_resolution;
mod mock_server;
mod model_filter;
mod model_lifecycle;
mod model_pricing;
mod native_compaction;
mod openrouter_routing;
mod provider_conformance;
mod provider_interception;
mod request_transforms;
mod responses_protocol;
mod retry_adapters;
mod retry_policy;
mod shared_transport;
mod token_counting;
mod token_envelope_measurement;

fn openrouter_transform_options() -> Vec<heycode_core::ProviderRequestOption> {
    vec![
        heycode_core::ProviderRequestOption::new(
            heycode_llm::OpenRouterProvider::NAME,
            "transforms",
            serde_json::json!({
                "plugins":[
                    {"id":"context-compression","enabled":false},
                    {"id":"file-parser","enabled":false},
                    {"id":"response-healing","enabled":false}
                ]
            }),
        )
        .expect("the shared test transform policy is valid"),
    ]
}

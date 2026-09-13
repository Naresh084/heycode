//! PAN06 automatic prompt caching and detailed usage visibility.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceInput, InputModality, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, Provider, RequestDraft,
    RequestedCapability, ResolveError,
};
use heycode_provider_anthropic::{
    ANTHROPIC_CLAUDE_OPUS_5, AnthropicCacheActivity, AnthropicPromptCachePolicy,
    AnthropicPromptCacheTtl, AnthropicProvider,
};
use tokio_util::sync::CancellationToken;

type RecordedRequest = (serde_json::Value, Vec<(String, String)>);

#[derive(Default)]
struct RecordingTransport {
    requests: Mutex<Vec<RecordedRequest>>,
}

impl HttpTransport for RecordingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        let body = serde_json::from_slice(request.body().expect("Messages body")).unwrap();
        let headers = request
            .headers()
            .iter()
            .map(|header| (header.name().to_owned(), header.value().to_owned()))
            .collect();
        self.requests.lock().unwrap().push((body, headers));
        Box::pin(futures::stream::iter(script().into_iter().map(Ok)))
    }
}

fn event(name: &str, data: serde_json::Value) -> SseEvent {
    SseEvent {
        event: name.to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    }
}

fn script() -> Vec<SseEvent> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start",
                "message":{"id":"msg_cache","type":"message","role":"assistant",
                    "model":ANTHROPIC_CLAUDE_OPUS_5,"content":[],
                    "stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":8,"cache_creation_input_tokens":5120,
                        "cache_read_input_tokens":0,
                        "cache_creation":{"ephemeral_5m_input_tokens":0,
                            "ephemeral_1h_input_tokens":5120},"output_tokens":0}}
            }),
        ),
        event(
            "message_delta",
            serde_json::json!({"type":"message_delta",
                "delta":{"stop_reason":"end_turn","stop_sequence":null},
                "usage":{"output_tokens":1}}),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        display_name: "Claude Opus 5".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_000_000),
        max_output_tokens: Some(128_000),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            reasoning: CapabilitySupport::Supported,
            prompt_cache: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft(provider: &AnthropicProvider) -> RequestDraft {
    RequestDraft {
        provider: "anthropic".to_owned(),
        model: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: Some("stable system prefix".to_owned()),
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Provider::request_options(provider),
        temperature: None,
        max_output_tokens: Some(1_024),
        purpose: CallPurpose::Conversation,
    }
}

#[tokio::test]
async fn automatic_cache_intent_reaches_the_wire_and_detailed_usage_is_visible() {
    let transport = Arc::new(RecordingTransport::default());
    let provider = AnthropicProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(ANTHROPIC_CLAUDE_OPUS_5.to_owned()),
    )
    .unwrap()
    .with_prompt_caching(AnthropicPromptCachePolicy::automatic(
        AnthropicPromptCacheTtl::OneHour,
    ))
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter.resolve(draft(&provider), &model()).unwrap();
    assert_eq!(call.provider_options().len(), 1);
    assert_eq!(call.provider_options()[0].kind(), "prompt-cache");
    assert!(
        call.native_features()
            .contains(&heycode_llm::NativeFeature::PromptCache)
    );
    let mut stream = adapter.stream(call);
    let mut durable = None;
    while let Some(event) = stream.next().await {
        if let heycode_llm::InferenceEvent::ResponseMetadata(metadata) = event.unwrap() {
            durable = Some(metadata);
        }
    }
    let durable = durable.expect("neutral detailed cache usage is emitted");
    let durable_cache = durable.cache_usage().unwrap();
    assert_eq!(durable_cache.input_tokens(), 5128);
    assert_eq!(durable_cache.cache_read_tokens(), 0);
    assert_eq!(durable_cache.cache_write_tokens(), 5120);
    assert_eq!(durable_cache.cache_write_1h_tokens(), Some(5120));

    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].0["cache_control"],
        serde_json::json!({"type":"ephemeral","ttl":"1h"})
    );
    drop(requests);

    let metadata = provider
        .take_response_metadata("msg_cache")
        .expect("successful response metadata is recorded at terminal finish");
    let usage = metadata.cache_usage().expect("cache usage is present");
    assert_eq!(usage.uncached_input_tokens(), 8);
    assert_eq!(usage.cache_creation_input_tokens(), 5120);
    assert_eq!(usage.cache_read_input_tokens(), 0);
    assert_eq!(usage.cache_creation_1h_input_tokens(), Some(5120));
    assert_eq!(usage.total_input_tokens(), 5128);
    assert_eq!(usage.output_tokens(), 1);
    assert_eq!(usage.activity(), AnthropicCacheActivity::Write);
}

#[test]
fn automatic_cache_refuses_unproven_model_evidence_before_transport() {
    let transport = Arc::new(RecordingTransport::default());
    let provider = AnthropicProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(ANTHROPIC_CLAUDE_OPUS_5.to_owned()),
    )
    .unwrap()
    .with_prompt_caching(AnthropicPromptCachePolicy::automatic(
        AnthropicPromptCacheTtl::FiveMinutes,
    ))
    .unwrap();
    let mut unproven = model();
    unproven.capabilities.prompt_cache = CapabilitySupport::Unknown;
    assert!(matches!(
        Provider::inference_adapter(&provider)
            .unwrap()
            .resolve(draft(&provider), &unproven),
        Err(ResolveError::Unproven {
            capability: RequestedCapability::PromptCache,
            ..
        })
    ));
    assert!(transport.requests.lock().unwrap().is_empty());
}

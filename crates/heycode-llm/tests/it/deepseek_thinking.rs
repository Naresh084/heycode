//! DeepSeek V4 thinking toggle, effort and sampling compatibility.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, ChatToolCall, DeepSeekProvider, FinishReason,
    InferenceAdapter, InferenceEvent, InferenceInput, InputModality, LlmError, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, ProviderProtocol,
    ProviderStateItem, ProviderStateKind, ReasoningEffortId, RequestDraft, ResolveError, ToolSpec,
};
use tokio_util::sync::CancellationToken;

struct CaptureTransport {
    bodies: Mutex<Vec<serde_json::Value>>,
    events: Mutex<Option<Vec<Result<SseEvent, heycode_http::TransportError>>>>,
}

impl CaptureTransport {
    fn empty() -> Self {
        Self {
            bodies: Mutex::new(Vec::new()),
            events: Mutex::new(Some(Vec::new())),
        }
    }

    fn scripted(events: Vec<Result<SseEvent, heycode_http::TransportError>>) -> Self {
        Self {
            bodies: Mutex::new(Vec::new()),
            events: Mutex::new(Some(events)),
        }
    }
}

impl HttpTransport for CaptureTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().unwrap_or_default()).unwrap());
        Box::pin(futures::stream::iter(
            self.events.lock().unwrap().take().unwrap_or_default(),
        ))
    }
}

fn provider() -> (DeepSeekProvider, Arc<CaptureTransport>) {
    provider_with_transport(CaptureTransport::empty())
}

fn provider_with_transport(capture: CaptureTransport) -> (DeepSeekProvider, Arc<CaptureTransport>) {
    let transport = Arc::new(capture);
    let provider = DeepSeekProvider::from_key_with_transport(
        "test-key",
        Some(DeepSeekProvider::DEFAULT_MODEL.to_owned()),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    (provider, transport)
}

fn chat_state(data: serde_json::Value) -> ProviderStateItem {
    ProviderStateItem::new(
        DeepSeekProvider::NAME,
        DeepSeekProvider::DEFAULT_MODEL,
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        data,
    )
    .unwrap()
}

fn sse(data: serde_json::Value) -> Result<SseEvent, heycode_http::TransportError> {
    Ok(SseEvent {
        event: "message".to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    })
}

fn done() -> Result<SseEvent, heycode_http::TransportError> {
    Ok(SseEvent {
        event: "message".to_owned(),
        data: "[DONE]".to_owned(),
        id: None,
        retry_ms: None,
    })
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: DeepSeekProvider::DEFAULT_MODEL.to_owned(),
        display_name: "DeepSeek V4 Flash".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(393_216),
        lifecycle: ModelLifecycle::preview(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            image_input: CapabilitySupport::Unknown,
            document_input: CapabilitySupport::Unknown,
            structured_output: CapabilitySupport::Unknown,
            native_web: CapabilitySupport::Unknown,
            native_compaction: CapabilitySupport::Unknown,
            prompt_cache: CapabilitySupport::Supported,
        },
        reasoning: None,
    }
}

fn draft(effort: Option<&str>) -> RequestDraft {
    RequestDraft {
        provider: DeepSeekProvider::NAME.to_owned(),
        model: DeepSeekProvider::DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: effort.map(|effort| ReasoningEffortId::new(effort).unwrap()),
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: Some(0.7),
        max_output_tokens: Some(4_096),
        purpose: CallPurpose::Conversation,
    }
}

fn dispatch_body(
    request: RequestDraft,
) -> (
    serde_json::Value,
    heycode_llm::ResolvedDefaults,
    Option<f32>,
) {
    let (provider, transport) = provider();
    let call = provider.resolve(request, &model()).unwrap();
    let defaults = call.defaults();
    let temperature = call.temperature();
    drop(provider.stream(call));
    let body = transport.bodies.lock().unwrap().remove(0);
    (body, defaults, temperature)
}

#[test]
fn omitted_toggle_defaults_to_enabled_high_and_removes_temperature() {
    let (body, defaults, temperature) = dispatch_body(draft(None));
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["reasoning_effort"], "high");
    assert!(body.get("temperature").is_none());
    assert_eq!(temperature, None);
    assert!(defaults.reasoning_effort);
}

#[test]
fn exact_high_and_max_efforts_map_to_deepseek_wire_values() {
    for effort in ["high", "max"] {
        let (body, defaults, temperature) = dispatch_body(draft(Some(effort)));
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], effort);
        assert!(body.get("temperature").is_none());
        assert_eq!(temperature, None);
        assert!(!defaults.reasoning_effort);
    }
}

#[test]
fn none_disables_thinking_omits_effort_and_keeps_sampling() {
    let (body, defaults, temperature) = dispatch_body(draft(Some("none")));
    assert_eq!(body["thinking"]["type"], "disabled");
    assert!(body.get("reasoning_effort").is_none());
    assert!((body["temperature"].as_f64().unwrap() - 0.7).abs() < 0.000_001);
    assert_eq!(temperature, Some(0.7));
    assert!(!defaults.reasoning_effort);
}

#[test]
fn unadvertised_effort_fails_before_transport_and_thinking_tools_omit_controls() {
    let (provider, transport) = provider();
    let error = provider.resolve(draft(Some("low")), &model()).unwrap_err();
    assert!(matches!(
        error,
        ResolveError::UnsupportedReasoningEffort { available, .. }
            if available.iter().map(ReasoningEffortId::as_str).collect::<Vec<_>>()
                == ["none", "high", "max"]
    ));
    assert!(transport.bodies.lock().unwrap().is_empty());

    let mut request = draft(Some("high"));
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    let call = provider.resolve(request, &model()).unwrap();
    drop(provider.stream(call));
    let body = transport.bodies.lock().unwrap().remove(0);
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("parallel_tool_calls").is_none());
}

#[test]
fn thinking_tool_continuation_replays_complete_reasoning_state() {
    let (provider, transport) = provider();
    let mut request = draft(Some("high"));
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("use a tool")),
        InferenceInput::ProviderState(chat_state(serde_json::json!({
            "role":"assistant",
            "content":null,
            "reasoning_content":"complete private reasoning",
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{\"path\":\"a\"}"}
            }]
        }))),
        InferenceInput::Message(ChatMessage::tool("call_1", "contents")),
    ];
    let call = provider.resolve(request, &model()).unwrap();
    drop(provider.stream(call));
    let body = transport.bodies.lock().unwrap().remove(0);
    assert_eq!(
        body["messages"][1]["reasoning_content"],
        "complete private reasoning"
    );
    assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
}

#[test]
fn missing_thinking_tool_reasoning_fails_before_transport() {
    let (provider, transport) = provider();
    let mut missing = draft(Some("high"));
    missing
        .inputs
        .push(InferenceInput::ProviderState(chat_state(
            serde_json::json!({
                "role":"assistant","content":null,
                "tool_calls":[{
                    "id":"call_1","type":"function",
                    "function":{"name":"read","arguments":"{}"}
                }]
            }),
        )));
    assert!(matches!(
        provider.resolve(missing, &model()),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));

    let mut neutral = draft(Some("high"));
    neutral.inputs.push(InferenceInput::Message(
        ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "call_2".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            }],
        ),
    ));
    assert!(matches!(
        provider.resolve(neutral, &model()),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));
    assert!(transport.bodies.lock().unwrap().is_empty());

    let mut disabled = draft(Some("none"));
    disabled
        .inputs
        .push(InferenceInput::ProviderState(chat_state(
            serde_json::json!({
                "role":"assistant","content":"non-thinking",
                "tool_calls":[{
                    "id":"call_3","type":"function",
                    "function":{"name":"read","arguments":"{}"}
                }]
            }),
        )));
    assert!(provider.resolve(disabled, &model()).is_ok());
}

#[tokio::test]
async fn thinking_tool_response_without_reasoning_fails_without_state_or_finish() {
    let events = vec![
        sse(serde_json::json!({
            "id":"chat_missing_reasoning",
            "choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_missing_reasoning",
            "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
        })),
        done(),
    ];
    let (provider, _transport) = provider_with_transport(CaptureTransport::scripted(events));
    let call = provider.resolve(draft(Some("high")), &model()).unwrap();
    let events = provider.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().any(|event| matches!(
        event,
        Err(LlmError::InvalidResponse(message)) if message.contains("reasoning_content")
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(_)) | Ok(InferenceEvent::Finish(FinishReason::ToolCalls))
    )));
}

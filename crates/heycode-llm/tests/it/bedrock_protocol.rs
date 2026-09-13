//! P07 Bedrock Converse protocol conformance.
//!
//! Fixtures are built by an independent event-stream encoder that follows the
//! Amazon Event Stream Specification directly
//! (<https://smithy.io/2.0/aws/amazon-eventstream.html>), so the decoder under
//! test is never checked against itself. No test touches the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt as _;
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream, TransportError,
};
use heycode_llm::{
    BedrockConverseAdapter, BedrockConverseConfig, CallPurpose, CapabilitySupport, ChatMessage,
    ChatToolCall, FinishReason, InferenceAdapter, InferenceEvent, InferenceInput, InputModality,
    LlmError, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing,
    NativeFeature, ProviderDescriptor, ProviderErrorClass, ProviderProtocol, ProviderStateItem,
    ProviderStateKind, ReasoningEffortId, RequestDraft, ResolveError, ResolvedCall, RetrySpec,
    StreamItemKind, ToolSpec,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Independent `application/vnd.amazon.eventstream` encoder
// ---------------------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut state = 0xFFFF_FFFF_u32;
    for byte in bytes {
        state ^= u32::from(*byte);
        for _ in 0..8 {
            state = if state & 1 == 1 {
                0xEDB8_8320 ^ (state >> 1)
            } else {
                state >> 1
            };
        }
    }
    state ^ 0xFFFF_FFFF
}

/// `uint32 total_length`, `uint32 headers_length`, `uint32 prelude_crc`,
/// headers, payload, `uint32 message_crc`.
fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(u8::try_from(name.len()).unwrap());
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7); // string
        header_bytes.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let total = u32::try_from(16 + header_bytes.len() + payload.len()).unwrap();
    let mut message = Vec::new();
    message.extend_from_slice(&total.to_be_bytes());
    message.extend_from_slice(&u32::try_from(header_bytes.len()).unwrap().to_be_bytes());
    message.extend_from_slice(&crc32(&message).to_be_bytes());
    message.extend_from_slice(&header_bytes);
    message.extend_from_slice(payload);
    let crc = crc32(&message);
    message.extend_from_slice(&crc.to_be_bytes());
    message
}

fn event(event_type: &str, payload: serde_json::Value) -> Vec<u8> {
    frame(
        &[
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ],
        payload.to_string().as_bytes(),
    )
}

fn exception(exception_type: &str, payload: serde_json::Value) -> Vec<u8> {
    frame(
        &[
            (":message-type", "exception"),
            (":exception-type", exception_type),
            (":content-type", "application/json"),
        ],
        payload.to_string().as_bytes(),
    )
}

fn body(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.iter().flatten().copied().collect()
}

fn message_start() -> Vec<u8> {
    event("messageStart", serde_json::json!({"role": "assistant"}))
}

fn text_delta(index: u32, text: &str) -> Vec<u8> {
    event(
        "contentBlockDelta",
        serde_json::json!({"contentBlockIndex": index, "delta": {"text": text}}),
    )
}

fn block_stop(index: u32) -> Vec<u8> {
    event(
        "contentBlockStop",
        serde_json::json!({"contentBlockIndex": index}),
    )
}

fn message_stop(reason: &str) -> Vec<u8> {
    event("messageStop", serde_json::json!({"stopReason": reason}))
}

fn metadata(usage: serde_json::Value) -> Vec<u8> {
    event(
        "metadata",
        serde_json::json!({"usage": usage, "metrics": {"latencyMs": 12}}),
    )
}

fn plain_usage() -> serde_json::Value {
    serde_json::json!({"inputTokens": 11, "outputTokens": 3, "totalTokens": 14})
}

// ---------------------------------------------------------------------------
// Injected transport
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Captured {
    url: String,
    headers: Vec<(String, String)>,
    body: serde_json::Value,
}

struct ScriptedTransport {
    response: Mutex<Option<Result<HttpResponse, TransportError>>>,
    captured: Mutex<Option<Captured>>,
    calls: AtomicUsize,
}

impl ScriptedTransport {
    fn ok(body: Vec<u8>) -> Self {
        Self::responding(Ok(HttpResponse {
            status: 200,
            content_type: Some("application/vnd.amazon.eventstream".to_owned()),
            headers: BTreeMap::from([("x-amzn-requestid".to_owned(), "req-0123456789".to_owned())]),
            body,
        }))
    }

    fn responding(response: Result<HttpResponse, TransportError>) -> Self {
        Self {
            response: Mutex::new(Some(response)),
            captured: Mutex::new(None),
            calls: AtomicUsize::new(0),
        }
    }

    fn captured(&self) -> Captured {
        self.captured.lock().unwrap().clone().unwrap()
    }
}

impl HttpTransport for ScriptedTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.captured.lock().unwrap() = Some(Captured {
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: serde_json::from_slice(request.body().unwrap_or_default()).unwrap_or_default(),
        });
        if cancellation.is_cancelled() {
            return Box::pin(async { Err(TransportError::Cancelled) });
        }
        let response = self.response.lock().unwrap().take();
        Box::pin(async move {
            response.unwrap_or(Err(TransportError::Network {
                message: "script exhausted".to_owned(),
            }))
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::once(async {
            Err(TransportError::InvalidRequest {
                field: "transport",
                message: "Converse never uses SSE".to_owned(),
            })
        }))
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "bedrock-fixture".to_owned(),
        display_name: "Bedrock Fixture".to_owned(),
        protocols: vec![ProviderProtocol::BedrockConverse],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "us.anthropic.claude-fixture".to_owned(),
        display_name: "Claude Fixture on Bedrock".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(200_000),
        max_output_tokens: Some(8_192),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            image_input: CapabilitySupport::Supported,
            document_input: CapabilitySupport::Supported,
            structured_output: CapabilitySupport::Supported,
            native_web: CapabilitySupport::Supported,
            native_compaction: CapabilitySupport::Supported,
            prompt_cache: CapabilitySupport::Supported,
        },
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "bedrock-fixture".to_owned(),
        model: "us.anthropic.claude-fixture".to_owned(),
        catalog_revision: Some(3),
        catalog_fetched_at_ms: Some(3_000),
        effective_at_ms: 4_000,
        system: Some("Follow the repository law.".to_owned()),
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
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

fn tool() -> ToolSpec {
    ToolSpec {
        name: "top_song".to_owned(),
        description: "Get the most popular song played on a radio station.".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"sign": {"type": "string"}},
            "required": ["sign"],
        }),
    }
}

fn config() -> BedrockConverseConfig {
    BedrockConverseConfig::with_api_key(
        provider(),
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        "bedrock-api-key",
    )
    .with_retry_spec(RetrySpec::no_retry())
}

fn adapter(transport: &Arc<ScriptedTransport>) -> BedrockConverseAdapter {
    BedrockConverseAdapter::new(config(), HttpService::new(transport.clone())).unwrap()
}

fn resolved(adapter: &BedrockConverseAdapter, draft: RequestDraft) -> ResolvedCall {
    adapter.resolve(draft, &model()).unwrap()
}

async fn drain(adapter: &BedrockConverseAdapter, call: ResolvedCall) -> Vec<InferenceEvent> {
    collect(adapter, call)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

async fn collect(
    adapter: &BedrockConverseAdapter,
    call: ResolvedCall,
) -> Vec<Result<InferenceEvent, LlmError>> {
    adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect::<Vec<_>>()
        .await
}

async fn failure(adapter: &BedrockConverseAdapter, call: ResolvedCall) -> LlmError {
    collect(adapter, call)
        .await
        .into_iter()
        .find_map(Result::err)
        .expect("stream must fail")
}

fn tool_call_arguments(events: &[InferenceEvent], index: u32) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            InferenceEvent::ToolCallDelta {
                output_index,
                arguments_delta,
                ..
            } if *output_index == index => Some(arguments_delta.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_targets_the_converse_stream_path_with_the_authorized_bearer_credential() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    drain(&adapter, call).await;

    let captured = transport.captured();
    assert_eq!(
        captured.url,
        "https://bedrock-runtime.us-east-1.amazonaws.com/model/us.anthropic.claude-fixture/converse-stream"
    );
    assert!(captured.headers.contains(&(
        "authorization".to_owned(),
        "Bearer bedrock-api-key".to_owned()
    )));
    assert!(
        captured
            .headers
            .contains(&("content-type".to_owned(), "application/json".to_owned()))
    );
    assert!(captured.headers.contains(&(
        "accept".to_owned(),
        "application/vnd.amazon.eventstream".to_owned()
    )));
}

#[tokio::test]
async fn request_body_carries_system_slot_messages_and_inference_config() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.temperature = Some(0.25);
    draft.max_output_tokens = Some(512);
    let call = resolved(&adapter, draft);
    drain(&adapter, call).await;

    assert_eq!(
        transport.captured().body,
        serde_json::json!({
            "messages": [{"role": "user", "content": [{"text": "hello"}]}],
            "system": [{"text": "Follow the repository law."}],
            "inferenceConfig": {"maxTokens": 512, "temperature": 0.25},
        })
    );
}

#[tokio::test]
async fn request_body_maps_client_tools_into_tool_config_with_automatic_choice() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    drain(&adapter, call).await;

    assert_eq!(
        transport.captured().body["toolConfig"],
        serde_json::json!({
            "tools": [{
                "toolSpec": {
                    "name": "top_song",
                    "description": "Get the most popular song played on a radio station.",
                    "inputSchema": {"json": {
                        "type": "object",
                        "properties": {"sign": {"type": "string"}},
                        "required": ["sign"],
                    }},
                }
            }],
            "toolChoice": {"auto": {}},
        })
    );
}

#[tokio::test]
async fn runtime_metadata_serializes_cache_points_guardrail_and_route_for_the_selected_model() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    draft.provider_options = vec![
        heycode_core::ProviderRequestOption::new(
            "bedrock-fixture",
            "runtime-metadata",
            serde_json::json!({
                "route": {
                    "source_region":"us-east-1",
                    "target_kind":"cross_region_inference_profile",
                    "cross_region_scope":"geographic"
                },
                "cache_points":[
                    {"placement":"tools","cachePoint":{"type":"default","ttl":"1h"}},
                    {"placement":"system","cachePoint":{"type":"default"}},
                    {"placement":"latest_user_message","cachePoint":{"type":"default"}}
                ],
                "guardrailConfig": {
                    "guardrailIdentifier":"grabc123",
                    "guardrailVersion":"7",
                    "trace":"enabled",
                    "streamProcessingMode":"async"
                }
            }),
        )
        .unwrap(),
    ];
    let call = resolved(&adapter, draft);
    drain(&adapter, call).await;

    let request = transport.captured().body;
    assert_eq!(
        request["messages"][0]["content"][1]["cachePoint"]["type"],
        "default"
    );
    assert_eq!(request["system"][1]["cachePoint"]["type"], "default");
    assert_eq!(request["toolConfig"]["tools"][1]["cachePoint"]["ttl"], "1h");
    assert_eq!(
        request["guardrailConfig"],
        serde_json::json!({
            "guardrailIdentifier":"grabc123",
            "guardrailVersion":"7",
            "trace":"enabled",
            "streamProcessingMode":"async"
        })
    );
}

#[test]
fn runtime_metadata_requires_the_owning_provider_and_a_present_cache_plane() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[])));
    let adapter = adapter(&transport);
    let route = serde_json::json!({
        "source_region":"us-east-1",
        "target_kind":"cross_region_inference_profile",
        "cross_region_scope":"geographic"
    });

    let mut foreign = draft();
    foreign.provider_options = vec![
        heycode_core::ProviderRequestOption::new(
            "another-provider",
            "runtime-metadata",
            serde_json::json!({"route":route}),
        )
        .unwrap(),
    ];
    assert!(matches!(
        adapter.resolve(foreign, &model()),
        Err(ResolveError::InvalidRequest {
            field: "provider_options",
            ..
        })
    ));

    let mut missing_tools = draft();
    missing_tools.provider_options = vec![
        heycode_core::ProviderRequestOption::new(
            "bedrock-fixture",
            "runtime-metadata",
            serde_json::json!({
                "route":route,
                "cache_points":[{
                    "placement":"tools","cachePoint":{"type":"default"}
                }]
            }),
        )
        .unwrap(),
    ];
    assert!(matches!(
        adapter.resolve(missing_tools, &model()),
        Err(ResolveError::InvalidRequest {
            field: "provider_options",
            ..
        })
    ));
}

#[tokio::test]
async fn request_body_batches_parallel_tool_results_into_one_user_turn() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    draft.inputs = vec![
        InferenceInput::Message(ChatMessage::user("two stations please")),
        InferenceInput::Message(ChatMessage {
            role: heycode_llm::Role::Assistant,
            content: String::new(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: Some(vec![
                ChatToolCall {
                    id: "call-1".to_owned(),
                    name: "top_song".to_owned(),
                    arguments: r#"{"sign":"WZPZ"}"#.to_owned(),
                },
                ChatToolCall {
                    id: "call-2".to_owned(),
                    name: "top_song".to_owned(),
                    arguments: r#"{"sign":"WKRP"}"#.to_owned(),
                },
            ]),
            tool_call_id: None,
            tool_result_is_error: None,
        }),
        InferenceInput::Message(ChatMessage {
            role: heycode_llm::Role::Tool,
            content: "Song A".to_owned(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: None,
            tool_call_id: Some("call-1".to_owned()),
            tool_result_is_error: None,
        }),
        InferenceInput::Message(ChatMessage {
            role: heycode_llm::Role::Tool,
            content: "station offline".to_owned(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: None,
            tool_call_id: Some("call-2".to_owned()),
            tool_result_is_error: Some(true),
        }),
    ];
    let call = resolved(&adapter, draft);
    drain(&adapter, call).await;

    let messages = transport.captured().body["messages"].clone();
    assert_eq!(messages.as_array().map(Vec::len), Some(3));
    assert_eq!(
        messages[1],
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"toolUse": {"toolUseId": "call-1", "name": "top_song", "input": {"sign": "WZPZ"}}},
                {"toolUse": {"toolUseId": "call-2", "name": "top_song", "input": {"sign": "WKRP"}}},
            ],
        })
    );
    assert_eq!(
        messages[2],
        serde_json::json!({
            "role": "user",
            "content": [
                {"toolResult": {"toolUseId": "call-1", "content": [{"text": "Song A"}]}},
                {"toolResult": {
                    "toolUseId": "call-2",
                    "content": [{"text": "station offline"}],
                    "status": "error",
                }},
            ],
        })
    );
}

// ---------------------------------------------------------------------------
// Stream normalization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn text_stream_normalizes_lifecycle_deltas_usage_and_finish() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        text_delta(0, "He"),
        text_delta(0, "llo"),
        block_stop(0),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let state = ProviderStateItem::new(
        "bedrock-fixture",
        "us.anthropic.claude-fixture",
        ProviderProtocol::BedrockConverse,
        ProviderStateKind::BedrockConverseMessage,
        serde_json::json!({"role":"assistant","content":[{"text":"Hello"}]}),
    )
    .unwrap();

    assert_eq!(
        drain(&adapter, call).await,
        vec![
            InferenceEvent::ResponseStarted {
                response_id: "req-0123456789".to_owned()
            },
            InferenceEvent::ItemStarted {
                output_index: 0,
                item_id: "content/0".to_owned(),
                kind: StreamItemKind::Message,
            },
            InferenceEvent::TextDelta("He".to_owned()),
            InferenceEvent::TextDelta("llo".to_owned()),
            InferenceEvent::ItemFinished {
                output_index: 0,
                item_id: "content/0".to_owned(),
                kind: StreamItemKind::Message,
            },
            InferenceEvent::ResponseFinished {
                response_id: "req-0123456789".to_owned(),
                status: "end_turn".to_owned(),
            },
            InferenceEvent::ProviderState(state),
            InferenceEvent::Usage(heycode_llm::TokenUsage {
                prompt_tokens: 11,
                completion_tokens: 3,
            }),
            InferenceEvent::Finish(FinishReason::Stop),
        ]
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn parallel_tool_use_yields_two_complete_tool_calls_and_a_tool_finish() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 0,
                "start": {"toolUse": {"toolUseId": "call-1", "name": "top_song"}},
            }),
        ),
        event(
            "contentBlockDelta",
            serde_json::json!({
                "contentBlockIndex": 0,
                "delta": {"toolUse": {"input": "{\"sign\":"}},
            }),
        ),
        event(
            "contentBlockDelta",
            serde_json::json!({
                "contentBlockIndex": 0,
                "delta": {"toolUse": {"input": "\"WZPZ\"}"}},
            }),
        ),
        block_stop(0),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 1,
                "start": {"toolUse": {"toolUseId": "call-2", "name": "top_song"}},
            }),
        ),
        event(
            "contentBlockDelta",
            serde_json::json!({
                "contentBlockIndex": 1,
                "delta": {"toolUse": {"input": "{\"sign\":\"WKRP\"}"}},
            }),
        ),
        block_stop(1),
        message_stop("tool_use"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    let events = drain(&adapter, call).await;

    assert_eq!(tool_call_arguments(&events, 0), r#"{"sign":"WZPZ"}"#);
    assert_eq!(tool_call_arguments(&events, 1), r#"{"sign":"WKRP"}"#);
    assert!(events.contains(&InferenceEvent::ToolCallDelta {
        output_index: 0,
        id: Some(heycode_core::CallId::from_raw("call-1")),
        name: Some("top_song".to_owned()),
        arguments_delta: "{\"sign\":".to_owned(),
    }));
    assert!(events.contains(&InferenceEvent::ItemFinished {
        output_index: 1,
        item_id: "call-2".to_owned(),
        kind: StreamItemKind::FunctionCall,
    }));
    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::ToolCalls))
    );
}

#[tokio::test]
async fn tool_call_with_no_input_deltas_still_yields_a_complete_empty_object() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 0,
                "start": {"toolUse": {"toolUseId": "call-1", "name": "top_song"}},
            }),
        ),
        block_stop(0),
        message_stop("tool_use"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    let events = drain(&adapter, call).await;

    assert!(events.contains(&InferenceEvent::ToolCallDelta {
        output_index: 0,
        id: Some(heycode_core::CallId::from_raw("call-1")),
        name: Some("top_song".to_owned()),
        arguments_delta: "{}".to_owned(),
    }));
    assert_eq!(tool_call_arguments(&events, 0), "{}");
}

/// One otherwise-complete tool-use stream around a single injected defect, so
/// a negative test cannot pass merely because the stream ran out of events.
fn tool_stream(start: serde_json::Value, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut frames = vec![
        message_start(),
        event(
            "contentBlockStart",
            serde_json::json!({"contentBlockIndex": 0, "start": start}),
        ),
    ];
    frames.extend(extra.iter().cloned());
    frames.extend([
        block_stop(0),
        message_stop("tool_use"),
        metadata(plain_usage()),
    ]);
    body(&frames)
}

#[tokio::test]
async fn an_otherwise_complete_tool_stream_is_the_baseline_for_the_defect_tests() {
    let transport = Arc::new(ScriptedTransport::ok(tool_stream(
        serde_json::json!({"toolUse": {"toolUseId": "call-1", "name": "top_song"}}),
        &[],
    )));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    assert_eq!(
        drain(&adapter, call).await.last(),
        Some(&InferenceEvent::Finish(FinishReason::ToolCalls))
    );
}

#[tokio::test]
async fn a_repeated_tool_use_id_fails_the_stream() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 0,
                "start": {"toolUse": {"toolUseId": "call-1", "name": "top_song"}},
            }),
        ),
        block_stop(0),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 1,
                "start": {"toolUse": {"toolUseId": "call-1", "name": "top_song"}},
            }),
        ),
        block_stop(1),
        message_stop("tool_use"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn a_malformed_tool_use_id_fails_the_stream() {
    let transport = Arc::new(ScriptedTransport::ok(tool_stream(
        serde_json::json!({"toolUse": {"toolUseId": "call 1/../x", "name": "top_song"}}),
        &[],
    )));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn overlapping_content_blocks_fail_the_stream() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 0,
                "start": {"toolUse": {"toolUseId": "call-1", "name": "top_song"}},
            }),
        ),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 1,
                "start": {"toolUse": {"toolUseId": "call-2", "name": "top_song"}},
            }),
        ),
        block_stop(1),
        message_stop("tool_use"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn a_message_start_that_is_not_the_assistant_fails() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        event("messageStart", serde_json::json!({"role": "user"})),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn an_unknown_event_type_fails_instead_of_being_ignored() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        event("someFutureEvent", serde_json::json!({})),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn tool_use_stop_reason_without_any_tool_block_fails() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        text_delta(0, "hi"),
        block_stop(0),
        message_stop("tool_use"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn opaque_reasoning_state_publishes_complete_lossless_bedrock_state() {
    for opaque in [
        serde_json::json!({"signature":"opaque-signature"}),
        serde_json::json!({"redactedContent":"opaque-redacted-content"}),
    ] {
        let transport = Arc::new(ScriptedTransport::ok(body(&[
            message_start(),
            event(
                "contentBlockDelta",
                serde_json::json!({
                    "contentBlockIndex": 0,
                    "delta": {"reasoningContent": {"text": "thinking"}},
                }),
            ),
            event(
                "contentBlockDelta",
                serde_json::json!({
                    "contentBlockIndex": 0,
                    "delta": {"reasoningContent": opaque},
                }),
            ),
            block_stop(0),
            message_stop("end_turn"),
            metadata(plain_usage()),
        ])));
        let adapter = adapter(&transport);
        let call = resolved(&adapter, draft());
        let events = collect(&adapter, call).await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        let state = events
            .iter()
            .find_map(|event| match event {
                Ok(InferenceEvent::ProviderState(state)) => Some(state),
                _ => None,
            })
            .expect("Converse must publish replay state");
        assert_eq!(state.kind(), ProviderStateKind::BedrockConverseMessage);
        assert_eq!(
            state.data()["content"][0]["reasoningContent"]["text"],
            "thinking"
        );
        assert_eq!(
            state.data()["content"][0]["reasoningContent"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        let debug = format!("{events:?}");
        assert!(!debug.contains("opaque-signature"), "{debug}");
        assert!(!debug.contains("opaque-redacted-content"), "{debug}");
    }
}

#[tokio::test]
async fn content_block_indexes_must_strictly_increase() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        text_delta(1, "first"),
        block_stop(1),
        text_delta(1, "again"),
        block_stop(1),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn response_without_a_request_id_omits_response_lifecycle_events() {
    let transport = Arc::new(ScriptedTransport::responding(Ok(HttpResponse {
        status: 200,
        content_type: Some("application/vnd.amazon.eventstream".to_owned()),
        headers: BTreeMap::new(),
        body: body(&[
            message_start(),
            text_delta(0, "hi"),
            block_stop(0),
            message_stop("end_turn"),
            metadata(plain_usage()),
        ]),
    })));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let events = drain(&adapter, call).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, InferenceEvent::ResponseStarted { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, InferenceEvent::ResponseFinished { .. }))
    );
    assert_eq!(
        events.first(),
        Some(&InferenceEvent::ItemStarted {
            output_index: 0,
            item_id: "content/0".to_owned(),
            kind: StreamItemKind::Message,
        })
    );
}

#[tokio::test]
async fn unadvertised_tool_name_fails_the_stream() {
    let transport = Arc::new(ScriptedTransport::ok(tool_stream(
        serde_json::json!({"toolUse": {"toolUseId": "call-1", "name": "not_advertised"}}),
        &[],
    )));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn server_tool_use_start_fails_because_the_route_advertises_none() {
    let transport = Arc::new(ScriptedTransport::ok(tool_stream(
        serde_json::json!({"toolUse": {
            "toolUseId": "call-1",
            "name": "top_song",
            "type": "server_tool_use",
        }}),
        &[],
    )));
    let adapter = adapter(&transport);
    let mut draft = draft();
    draft.tools = vec![tool()];
    let call = resolved(&adapter, draft);
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn unknown_stop_reason_fails_instead_of_degrading_to_stop() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("some_future_reason"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn content_filtered_stop_reason_is_a_body_free_provider_failure() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("content_filtered"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let LlmError::Provider(failure) = failure(&adapter, call).await else {
        panic!("content_filtered must be a provider failure");
    };
    assert_eq!(failure.class(), ProviderErrorClass::InvalidRequest);
    assert_eq!(
        failure.code().map(heycode_llm::ProviderErrorCode::as_str),
        Some("content_filtered")
    );
}

#[tokio::test]
async fn max_tokens_stop_reason_maps_to_a_length_finish() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        text_delta(0, "truncated"),
        block_stop(0),
        message_stop("max_tokens"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert_eq!(
        drain(&adapter, call).await.last(),
        Some(&InferenceEvent::Finish(FinishReason::Length))
    );
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn present_cache_counters_are_added_to_prompt_usage() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(serde_json::json!({
            "inputTokens": 11,
            "outputTokens": 3,
            "totalTokens": 1_034,
            "cacheReadInputTokens": 1_000,
            "cacheWriteInputTokens": 20,
        })),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(drain(&adapter, call).await.contains(&InferenceEvent::Usage(
        heycode_llm::TokenUsage {
            prompt_tokens: 1_031,
            completion_tokens: 3,
        }
    )));
}

#[tokio::test]
async fn cache_counters_and_ttl_details_emit_neutral_response_metadata_without_double_counting() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(serde_json::json!({
            "inputTokens": 11,
            "outputTokens": 3,
            "totalTokens": 1_034,
            "cacheReadInputTokens": 1_000,
            "cacheWriteInputTokens": 20,
            "cacheDetails": [{"inputTokens": 20, "ttl": "1h"}]
        })),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let events = drain(&adapter, call).await;
    let metadata = events
        .iter()
        .find_map(|event| match event {
            InferenceEvent::ResponseMetadata(metadata) => Some(metadata),
            _ => None,
        })
        .expect("detailed cache metadata");
    let cache = metadata.cache_usage().expect("cache usage");
    assert_eq!(cache.input_tokens(), 1_031);
    assert_eq!(cache.uncached_input_tokens(), Some(11));
    assert_eq!(cache.cache_read_tokens(), 1_000);
    assert_eq!(cache.cache_write_tokens(), 20);
    assert_eq!(cache.cache_write_5m_tokens(), Some(0));
    assert_eq!(cache.cache_write_1h_tokens(), Some(20));
    assert_eq!(
        events
            .iter()
            .position(|event| matches!(event, InferenceEvent::ResponseMetadata(_))),
        events
            .iter()
            .position(|event| matches!(event, InferenceEvent::Usage(_)))
            .map(|i| i - 1)
    );
}

#[tokio::test]
async fn absent_cache_fields_emit_no_detailed_cache_fact_while_explicit_zero_remains_reported() {
    for (usage, expect_metadata) in [
        (plain_usage(), false),
        (
            serde_json::json!({
                "inputTokens":11,
                "outputTokens":3,
                "totalTokens":14,
                "cacheReadInputTokens":0,
                "cacheWriteInputTokens":0
            }),
            true,
        ),
    ] {
        let transport = Arc::new(ScriptedTransport::ok(body(&[
            message_start(),
            message_stop("end_turn"),
            metadata(usage),
        ])));
        let adapter = adapter(&transport);
        let events = drain(&adapter, resolved(&adapter, draft())).await;
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, InferenceEvent::ResponseMetadata(_))),
            expect_metadata
        );
    }
}

#[tokio::test]
async fn absent_cache_counters_leave_prompt_usage_at_the_reported_input_tokens() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(drain(&adapter, call).await.contains(&InferenceEvent::Usage(
        heycode_llm::TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 3,
        }
    )));
}

#[tokio::test]
async fn usage_missing_input_tokens_fails_instead_of_reporting_zero() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(serde_json::json!({"outputTokens": 3, "totalTokens": 3})),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn usage_missing_output_tokens_fails_instead_of_reporting_zero() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(serde_json::json!({"inputTokens": 11, "totalTokens": 11})),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn usage_missing_total_tokens_fails() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(serde_json::json!({"inputTokens": 11, "outputTokens": 3})),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn a_wrongly_typed_cache_counter_fails_rather_than_being_ignored() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(serde_json::json!({
            "inputTokens": 11,
            "outputTokens": 3,
            "totalTokens": 14,
            "cacheReadInputTokens": "many",
        })),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn stream_ending_before_metadata_fails_instead_of_synthesizing_usage() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        text_delta(0, "hi"),
        block_stop(0),
        message_stop("end_turn"),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let events = collect(&adapter, call).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Usage(_))))
    );
    assert!(matches!(
        events.last(),
        Some(Err(LlmError::InvalidResponse(_)))
    ));
}

#[tokio::test]
async fn metadata_before_message_stop_fails() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

// ---------------------------------------------------------------------------
// Framing and transport failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn truncated_event_stream_body_fails_after_its_complete_messages() {
    let mut wire = body(&[message_start(), text_delta(0, "hi")]);
    wire.truncate(wire.len() - 3);
    let transport = Arc::new(ScriptedTransport::ok(wire));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let events = collect(&adapter, call).await;

    assert_eq!(
        events.first().and_then(|event| event.as_ref().ok()),
        Some(&InferenceEvent::ResponseStarted {
            response_id: "req-0123456789".to_owned()
        })
    );
    assert!(matches!(
        events.last(),
        Some(Err(LlmError::InvalidResponse(_)))
    ));
}

#[tokio::test]
async fn corrupted_message_checksum_fails_the_stream() {
    let mut wire = body(&[message_start()]);
    let last = wire.len() - 1;
    wire[last] ^= 0xFF;
    let transport = Arc::new(ScriptedTransport::ok(wire));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn a_response_that_is_not_an_event_stream_fails_before_framing() {
    // The body is a perfectly good event stream, so only the declared media
    // type can reject this response.
    let transport = Arc::new(ScriptedTransport::responding(Ok(HttpResponse {
        status: 200,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: body(&[
            message_start(),
            message_stop("end_turn"),
            metadata(plain_usage()),
        ]),
    })));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert!(matches!(
        failure(&adapter, call).await,
        LlmError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn a_response_without_a_declared_media_type_is_still_decoded() {
    let transport = Arc::new(ScriptedTransport::responding(Ok(HttpResponse {
        status: 200,
        content_type: None,
        headers: BTreeMap::new(),
        body: body(&[
            message_start(),
            message_stop("end_turn"),
            metadata(plain_usage()),
        ]),
    })));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    assert_eq!(
        drain(&adapter, call).await.last(),
        Some(&InferenceEvent::Finish(FinishReason::Stop))
    );
}

#[tokio::test]
async fn trailing_bytes_after_the_terminal_finish_add_nothing_to_the_stream() {
    let mut wire = body(&[
        message_start(),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ]);
    wire.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let transport = Arc::new(ScriptedTransport::ok(wire));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let events = collect(&adapter, call).await;

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        events.last().and_then(|event| event.as_ref().ok()),
        Some(&InferenceEvent::Finish(FinishReason::Stop))
    );
}

#[tokio::test]
async fn throttling_exception_frame_becomes_a_rate_limited_provider_failure() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        exception(
            "throttlingException",
            serde_json::json!({"message": "Too many tokens for account 1234"}),
        ),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let error = failure(&adapter, call).await;
    let LlmError::Provider(provider_failure) = &error else {
        panic!("modeled exceptions must be provider failures");
    };
    assert_eq!(provider_failure.class(), ProviderErrorClass::RateLimited);
    assert_eq!(
        provider_failure
            .code()
            .map(heycode_llm::ProviderErrorCode::as_str),
        Some("throttlingException")
    );
    assert!(!format!("{error:?} {error}").contains("1234"));
}

#[tokio::test]
async fn validation_exception_frame_is_not_retried_as_a_server_failure() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        exception("validationException", serde_json::json!({"message": "bad"})),
    ])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let LlmError::Provider(provider_failure) = failure(&adapter, call).await else {
        panic!("modeled exceptions must be provider failures");
    };
    assert_eq!(provider_failure.class(), ProviderErrorClass::InvalidRequest);
}

#[tokio::test]
async fn unmodeled_error_frame_retains_only_its_code() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[frame(
        &[
            (":message-type", "error"),
            (":error-code", "InternalError"),
            (
                ":error-message",
                "An internal server error occurred for tenant zeta",
            ),
        ],
        b"",
    )])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let error = failure(&adapter, call).await;
    let LlmError::Provider(provider_failure) = &error else {
        panic!("unmodeled errors must be provider failures");
    };
    assert_eq!(provider_failure.class(), ProviderErrorClass::Server);
    assert_eq!(
        provider_failure
            .code()
            .map(heycode_llm::ProviderErrorCode::as_str),
        Some("InternalError")
    );
    assert!(!format!("{error:?} {error}").contains("zeta"));
}

#[tokio::test]
async fn non_success_status_classifies_without_leaking_the_response_body() {
    let transport = Arc::new(ScriptedTransport::responding(Ok(HttpResponse {
        status: 429,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::from([("retry-after".to_owned(), "7".to_owned())]),
        body: br#"{"message":"tenant omega exceeded quota"}"#.to_vec(),
    })));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let error = failure(&adapter, call).await;
    let LlmError::Provider(provider_failure) = &error else {
        panic!("HTTP failures must classify into a provider failure");
    };
    assert_eq!(provider_failure.class(), ProviderErrorClass::RateLimited);
    assert_eq!(provider_failure.status(), Some(429));
    assert_eq!(
        provider_failure.retry_after(),
        Some(heycode_http::HttpRetryAfter::Delay(
            std::time::Duration::from_secs(7)
        ))
    );
    assert!(!format!("{error:?} {error}").contains("omega"));
}

#[tokio::test]
async fn a_cancelled_caller_never_reaches_the_transport() {
    let transport = Arc::new(ScriptedTransport::ok(body(&[message_start()])));
    let adapter = adapter(&transport);
    let call = resolved(&adapter, draft());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let events = adapter
        .stream_cancellable(call, cancellation)
        .collect::<Vec<_>>()
        .await;

    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        events.as_slice(),
        [Err(LlmError::Provider(failure))] if failure.class() == ProviderErrorClass::Cancelled
    ));
}

// ---------------------------------------------------------------------------
// Resolution: unsupported choices fail before any transport is constructed
// ---------------------------------------------------------------------------

fn resolve_error(mutate: impl FnOnce(&mut RequestDraft)) -> ResolveError {
    let transport = Arc::new(ScriptedTransport::ok(Vec::new()));
    let adapter = adapter(&transport);
    let mut draft = draft();
    mutate(&mut draft);
    let error = adapter
        .resolve(draft, &model())
        .expect_err("resolution must fail");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    error
}

#[tokio::test]
async fn resolve_rejects_a_request_with_no_messages() {
    let error = resolve_error(|draft| draft.inputs.clear());
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "inputs",
            ..
        }
    ));
}

#[tokio::test]
async fn resolve_rejects_image_input_before_transport() {
    // A genuine image on a model whose descriptor proves `image_input`, so the
    // shared modality cross-check passes and only the Converse dialect
    // boundary can reject it.
    let error = resolve_error(|draft| {
        let image = heycode_llm::ChatImage::new(
            heycode_core::AttachmentMediaType::new("image/png").unwrap(),
            vec![1, 2, 3],
        )
        .unwrap();
        draft.inputs = vec![InferenceInput::Message(ChatMessage::user_with_images(
            "describe this",
            vec![image],
        ))];
        draft.input_modalities = vec![InputModality::Text, InputModality::Image];
    });
    assert!(
        matches!(
            &error,
            ResolveError::InvalidRequest {
                field: "input_modalities",
                message,
            } if message.contains("image/document input dialects")
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn exact_bedrock_provider_state_replays_verbatim_and_wrong_route_fails() {
    let prior = ProviderStateItem::new(
        "bedrock-fixture",
        "us.anthropic.claude-fixture",
        ProviderProtocol::BedrockConverse,
        ProviderStateKind::BedrockConverseMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{"reasoningContent":{"text":"summary","signature":"opaque"}},{"text":"answer"}]
        }),
    )
    .unwrap();
    let transport = Arc::new(ScriptedTransport::ok(body(&[
        message_start(),
        text_delta(0, "continued"),
        block_stop(0),
        message_stop("end_turn"),
        metadata(plain_usage()),
    ])));
    let adapter = adapter(&transport);
    let mut request = draft();
    request
        .inputs
        .push(InferenceInput::ProviderState(prior.clone()));
    let call = resolved(&adapter, request);
    let _events = drain(&adapter, call).await;
    assert_eq!(transport.captured().body["messages"][1], *prior.data());

    let error = resolve_error(|draft| {
        draft.inputs.push(InferenceInput::ProviderState(
            ProviderStateItem::new(
                "other-bedrock",
                "us.anthropic.claude-fixture",
                ProviderProtocol::BedrockConverse,
                ProviderStateKind::BedrockConverseMessage,
                serde_json::json!({"role":"assistant","content":[{"text":"a"}]}),
            )
            .unwrap(),
        ));
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        }
    ));
}

#[tokio::test]
async fn resolve_rejects_reasoning_effort_because_converse_has_no_effort_field() {
    let error = resolve_error(|draft| {
        draft.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    });
    assert!(matches!(
        error,
        ResolveError::UnsupportedReasoningEffort { .. }
    ));
}

#[tokio::test]
async fn resolve_rejects_structured_output_before_transport() {
    let error = resolve_error(|draft| {
        draft.structured_output = Some(serde_json::json!({"type": "object"}));
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "structured_output",
            ..
        }
    ));
}

#[tokio::test]
async fn resolve_rejects_native_features_before_transport() {
    let error = resolve_error(|draft| {
        draft.native_features = vec![NativeFeature::Web];
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "native_features",
            ..
        }
    ));
}

#[test]
fn resolve_retains_client_and_mcp_tool_routes_for_local_dispatch() {
    for kind in [
        heycode_core::NativeToolImplementationKind::Client,
        heycode_core::NativeToolImplementationKind::Mcp,
    ] {
        let transport = Arc::new(ScriptedTransport::ok(Vec::new()));
        let adapter = adapter(&transport);
        let mut draft = draft();
        draft.tools = vec![tool()];
        let route =
            heycode_core::NativeToolRoute::new("top_song", "local:top_song", kind, None).unwrap();
        draft.native_tool_routes = vec![route.clone()];
        let call = adapter.resolve(draft, &model()).unwrap();
        assert_eq!(call.native_tool_routes(), &[route]);
        assert_eq!(call.tools(), &[tool()]);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn resolve_rejects_provider_hosted_routes_rather_than_dropping_them() {
    let error = resolve_error(|draft| {
        draft.native_tool_routes = vec![
            heycode_core::NativeToolRoute::new(
                "web_search",
                "provider:bedrock-fixture:web_search",
                heycode_core::NativeToolImplementationKind::Provider,
                Some("bedrock-fixture".into()),
            )
            .unwrap(),
        ];
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "native_tool_routes",
            ..
        }
    ));
}

#[tokio::test]
async fn resolve_rejects_temperature_outside_the_documented_range() {
    let error = resolve_error(|draft| draft.temperature = Some(1.5));
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "temperature",
            ..
        }
    ));
}

#[tokio::test]
async fn resolve_rejects_a_tool_call_without_its_immediate_result() {
    let error = resolve_error(|draft| {
        draft.tools = vec![tool()];
        draft.inputs.push(InferenceInput::Message(ChatMessage {
            role: heycode_llm::Role::Assistant,
            content: String::new(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: Some(vec![ChatToolCall {
                id: "call-1".to_owned(),
                name: "top_song".to_owned(),
                arguments: "{}".to_owned(),
            }]),
            tool_call_id: None,
            tool_result_is_error: None,
        }));
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "inputs",
            ..
        }
    ));
}

#[tokio::test]
async fn resolve_rejects_replaying_a_tool_call_the_request_no_longer_advertises() {
    let error = resolve_error(|draft| {
        draft.inputs.push(InferenceInput::Message(ChatMessage {
            role: heycode_llm::Role::Assistant,
            content: String::new(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: Some(vec![ChatToolCall {
                id: "call-1".to_owned(),
                name: "top_song".to_owned(),
                arguments: "{}".to_owned(),
            }]),
            tool_call_id: None,
            tool_result_is_error: None,
        }));
        draft.inputs.push(InferenceInput::Message(ChatMessage {
            role: heycode_llm::Role::Tool,
            content: "ok".to_owned(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: None,
            tool_call_id: Some("call-1".to_owned()),
            tool_result_is_error: None,
        }));
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "inputs",
            ..
        }
    ));
}

#[tokio::test]
async fn resolve_rejects_a_tool_name_outside_the_documented_pattern() {
    // ToolSpecification.name is `[a-zA-Z0-9_-]+`, so a dotted or spaced name
    // the service would reject fails here instead.
    let error = resolve_error(|draft| {
        draft.tools = vec![ToolSpec {
            name: "web.search tool".to_owned(),
            description: "dotted and spaced".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }];
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest { field: "tools", .. }
    ));
}

#[tokio::test]
async fn resolve_rejects_provider_options_it_has_no_dialect_for() {
    let error = resolve_error(|draft| {
        draft.provider_options = vec![
            heycode_core::ProviderRequestOption::new(
                "bedrock-fixture",
                "routing",
                serde_json::json!({"order": ["a"]}),
            )
            .unwrap(),
        ];
    });
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "provider_options",
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

fn construction_error(config: BedrockConverseConfig) -> LlmError {
    let transport = Arc::new(ScriptedTransport::ok(Vec::new()));
    match BedrockConverseAdapter::new(config, HttpService::new(transport)) {
        Ok(_) => panic!("adapter construction must fail"),
        Err(error) => error,
    }
}

#[test]
fn construction_rejects_a_provider_that_does_not_declare_bedrock_converse() {
    let mut provider = provider();
    provider.protocols = vec![ProviderProtocol::AnthropicMessages];
    let error = construction_error(BedrockConverseConfig::with_api_key(
        provider,
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        "key",
    ));
    assert!(matches!(error, LlmError::InvalidResponse(_)));
}

#[test]
fn construction_rejects_a_blank_credential_as_an_authentication_failure() {
    let error = construction_error(BedrockConverseConfig::with_api_key(
        provider(),
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        "   ",
    ));
    assert_eq!(error.class(), ProviderErrorClass::Authentication);
}

#[test]
fn construction_rejects_an_extra_header_that_would_replace_authorization() {
    let error = construction_error(config().with_extra_headers(vec![(
        "Authorization".to_owned(),
        "Bearer other".to_owned(),
    )]));
    assert!(matches!(error, LlmError::InvalidResponse(_)));
}

#[test]
fn descriptor_reports_the_configured_provider_route() {
    let transport = Arc::new(ScriptedTransport::ok(Vec::new()));
    assert_eq!(adapter(&transport).descriptor(), provider());
}

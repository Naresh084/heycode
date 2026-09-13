//! Anthropic Messages request, block, thinking, tool, pause and state contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::{CallId, ProviderRequestOption};
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::testing::{
    ConformanceFixtureMetadata, ConformanceSourceKind, SseConformanceFixture, run_sse_conformance,
};
use heycode_llm::{
    AdapterOwnedAuth, AnthropicAuthWire, AnthropicMessagesAdapter, AnthropicMessagesConfig,
    AnthropicMessagesDialect, AnthropicServerToolNormalizationFault, AnthropicServerToolPlan,
    AnthropicServerToolResultNormalizer, AnthropicServerToolRoute, AnthropicThinkingDisplay,
    AnthropicThinkingMode, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatMessage,
    ChatToolCall, FinishReason, InferenceAdapter, InferenceEvent, InferenceInput, InputModality,
    ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing,
    NativeFeature, ProviderDescriptor, ProviderProtocol, ProviderStateItem, ProviderStateKind,
    ReasoningEffortId, RequestDraft, ResolveSpec, StreamItemKind, ToolSpec,
};
use tokio_util::sync::CancellationToken;

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "anthropic-fixture".to_owned(),
        display_name: "Anthropic Fixture".to_owned(),
        protocols: vec![ProviderProtocol::AnthropicMessages],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "claude-fixture".to_owned(),
        display_name: "Claude Fixture".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(200_000),
        max_output_tokens: Some(16_384),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            image_input: CapabilitySupport::Unsupported,
            document_input: CapabilitySupport::Unsupported,
            structured_output: CapabilitySupport::Unsupported,
            native_web: CapabilitySupport::Supported,
            native_compaction: CapabilitySupport::Unsupported,
            prompt_cache: CapabilitySupport::Unsupported,
        },
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "anthropic-fixture".to_owned(),
        model: "claude-fixture".to_owned(),
        catalog_revision: Some(7),
        catalog_fetched_at_ms: Some(7_000),
        effective_at_ms: 8_000,
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

fn event(event: &str, data: serde_json::Value) -> Result<SseEvent, heycode_http::TransportError> {
    Ok(SseEvent {
        event: event.to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    })
}

#[derive(Debug, Clone)]
struct Captured {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct ScriptedTransport {
    events: Mutex<Option<Vec<Result<SseEvent, heycode_http::TransportError>>>>,
    captured: Mutex<Option<Captured>>,
}

impl ScriptedTransport {
    fn new(events: Vec<Result<SseEvent, heycode_http::TransportError>>) -> Self {
        Self {
            events: Mutex::new(Some(events)),
            captured: Mutex::new(None),
        }
    }
}

impl HttpTransport for ScriptedTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        *self.captured.lock().unwrap() = Some(Captured {
            url: request.url().to_string(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().unwrap_or_default().to_vec(),
        });
        Box::pin(futures::stream::iter(
            self.events.lock().unwrap().take().unwrap(),
        ))
    }
}

fn base_config() -> AnthropicMessagesConfig {
    AnthropicMessagesConfig::with_key(provider(), "https://api.anthropic.test/v1", "test-key")
        .with_default_max_output_tokens(Some(4_096))
        .with_retry_spec(heycode_llm::RetrySpec::no_retry())
        .with_thinking(
            vec![
                (
                    ReasoningEffortId::new("none").unwrap(),
                    AnthropicThinkingMode::disabled(),
                ),
                (
                    ReasoningEffortId::new("high").unwrap(),
                    AnthropicThinkingMode::enabled(2_048, Some(AnthropicThinkingDisplay::Omitted))
                        .with_wire_effort("high"),
                ),
            ],
            Some(ReasoningEffortId::new("none").unwrap()),
        )
        .with_server_tool(
            NativeFeature::Web,
            serde_json::json!({
                    "type":"web_search_20250305",
                    "name":"web_search",
                "max_uses":3
            }),
        )
        .with_server_tool(
            NativeFeature::Web,
            serde_json::json!({
                "type":"web_fetch_20250910",
                "name":"web_fetch",
                "max_uses":2
            }),
        )
        .with_extra_headers(vec![(
            "anthropic-beta".to_owned(),
            "interleaved-thinking-2025-05-14".to_owned(),
        )])
}

struct FixtureAnthropicResultNormalizer {
    id: &'static str,
}

impl FixtureAnthropicResultNormalizer {
    const fn new(id: &'static str) -> Self {
        Self { id }
    }
}

impl AnthropicServerToolResultNormalizer for FixtureAnthropicResultNormalizer {
    fn id(&self) -> &'static str {
        self.id
    }

    fn normalize(
        &self,
        block: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<heycode_core::ServerToolResult, AnthropicServerToolNormalizationFault> {
        let call_id = block
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
            .ok_or(AnthropicServerToolNormalizationFault::InvalidResult)?;
        if block.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
            return heycode_core::ServerToolResult::error(
                CallId::from_raw(call_id),
                "provider_reported_error",
            )
            .map_err(|_| AnthropicServerToolNormalizationFault::InvalidResult);
        }
        let output_count = match block.get("content") {
            Some(serde_json::Value::Array(items)) => Some(
                u32::try_from(items.len())
                    .map_err(|_| AnthropicServerToolNormalizationFault::InvalidResult)?,
            ),
            Some(_) => Some(1),
            None => return Err(AnthropicServerToolNormalizationFault::InvalidResult),
        };
        heycode_core::ServerToolResult::success(CallId::from_raw(call_id), output_count, Vec::new())
            .map_err(|_| AnthropicServerToolNormalizationFault::InvalidResult)
    }
}

fn provider_server_tool_plan() -> AnthropicServerToolPlan {
    AnthropicServerToolPlan::new(
        "server-tools",
        vec![
            serde_json::json!({
                "type":"code_execution_20260521",
                "name":"code_execution"
            }),
            serde_json::json!({"type":"mcp_toolset"}),
        ],
        vec![
            "code-execution-2026-05-21".to_owned(),
            "mcp-client-2025-11-20".to_owned(),
        ],
        vec![(
            "mcp_servers".to_owned(),
            serde_json::json!([{
                "type":"url","name":"fixture","url":"https://mcp.example.test"
            }]),
        )],
        vec![
            AnthropicServerToolRoute::named(
                "code_execution",
                "bash_code_execution",
                vec!["bash_code_execution_tool_result".to_owned()],
                Some("code_execution_requests".to_owned()),
            )
            .unwrap()
            .with_result_normalizer(Arc::new(FixtureAnthropicResultNormalizer::new(
                "fixture-code-bash-v1",
            )))
            .unwrap(),
            AnthropicServerToolRoute::named(
                "code_execution",
                "text_editor_code_execution",
                vec!["text_editor_code_execution_tool_result".to_owned()],
                Some("code_execution_requests".to_owned()),
            )
            .unwrap()
            .with_result_normalizer(Arc::new(FixtureAnthropicResultNormalizer::new(
                "fixture-code-editor-v1",
            )))
            .unwrap(),
            AnthropicServerToolRoute::mcp()
                .unwrap()
                .with_result_normalizer(Arc::new(FixtureAnthropicResultNormalizer::new(
                    "fixture-mcp-v1",
                )))
                .unwrap(),
        ],
    )
    .unwrap()
}

fn make_adapter(
    events: Vec<Result<SseEvent, heycode_http::TransportError>>,
) -> (AnthropicMessagesAdapter, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::new(events));
    let adapter = AnthropicMessagesAdapter::new(
        base_config(),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    (adapter, transport)
}

fn terminal_text_events(id: &str) -> Vec<Result<SseEvent, heycode_http::TransportError>> {
    terminal_text_events_for_model(id, "claude-fixture")
}

fn terminal_text_events_for_model(
    id: &str,
    model: &str,
) -> Vec<Result<SseEvent, heycode_http::TransportError>> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start",
                "message":{
                    "id":id,"type":"message","role":"assistant","model":model,
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":3,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"text","text":""}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"ok"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta",
                "delta":{"stop_reason":"end_turn","stop_sequence":null},
                "usage":{"output_tokens":2}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn continued_server_result_events(
    id: &str,
    tool_use_id: &str,
) -> Vec<Result<SseEvent, heycode_http::TransportError>> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start",
                "message":{
                    "id":id,"type":"message","role":"assistant","model":"claude-fixture",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":3,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{
                    "type":"web_search_tool_result",
                    "tool_use_id":tool_use_id,
                    "content":[]
                }
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":1,
                "content_block":{"type":"text","text":""}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":1,
                "delta":{"type":"text_delta","text":"ok"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":1}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta",
                "delta":{"stop_reason":"end_turn","stop_sequence":null},
                "usage":{"output_tokens":2}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

#[tokio::test]
async fn request_replays_exact_blocks_and_serializes_client_server_tools_thinking_and_headers() {
    let (adapter, transport) = make_adapter(terminal_text_events("msg_request"));
    let prior = ProviderStateItem::new(
        "anthropic-fixture",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[
                {"type":"thinking","thinking":"summary","signature":"opaque-signature"},
                {"type":"redacted_thinking","data":"opaque-redacted"},
                {"type":"tool_use","id":"toolu_prior","name":"read","input":{"path":"old"}},
                {"type":"server_tool_use","id":"srvtoolu_prior","name":"web_search","input":{"query":"old"}},
                {"type":"web_search_tool_result","tool_use_id":"srvtoolu_prior","content":{"type":"web_search_tool_result_error","error_code":"unavailable"}}
            ]
        }),
    )
    .unwrap();
    let continued = ProviderStateItem::new(
        "anthropic-fixture",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{"type":"text","text":"continued"}]
        }),
    )
    .unwrap();
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("first")),
        InferenceInput::ProviderState(prior.clone()),
        InferenceInput::Message(ChatMessage::tool("toolu_prior", "old contents")),
        InferenceInput::ProviderState(continued),
        InferenceInput::Message(ChatMessage::user("next")),
    ];
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
    });
    request.native_features.push(NativeFeature::Web);
    request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    request.temperature = Some(0.2);
    request.max_output_tokens = Some(4_096);
    let call = adapter.resolve(request, &model()).unwrap();
    assert_eq!(
        call.temperature(),
        None,
        "thinking strips temperature at resolution"
    );
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured.url, "https://api.anthropic.test/v1/messages");
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| { name == "x-api-key" && value == "test-key" })
    );
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| { name == "anthropic-version" && value == "2023-06-01" })
    );
    assert!(captured.headers.iter().any(|(name, value)| {
        name == "anthropic-beta" && value == "interleaved-thinking-2025-05-14"
    }));
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["model"], "claude-fixture");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 4_096);
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["thinking"]["budget_tokens"], 2_048);
    assert_eq!(body["thinking"]["display"], "omitted");
    assert_eq!(body["output_config"]["effort"], "high");
    assert!(body.get("temperature").is_none());
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["tools"][1]["type"], "web_search_20250305");
    assert_eq!(body["tools"][2]["type"], "web_fetch_20250910");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[1], prior.data().clone());
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_prior");
    assert!(
        !String::from_utf8(captured.body)
            .unwrap()
            .contains("test-key")
    );
}

#[tokio::test]
async fn parallel_client_results_serialize_as_one_immediate_user_message() {
    let (adapter, transport) = make_adapter(terminal_text_events("msg_parallel_results"));
    let prior = ProviderStateItem::new(
        "anthropic-fixture",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[
                {"type":"tool_use","id":"toolu_read","name":"read","input":{"path":"a"}},
                {"type":"tool_use","id":"toolu_grep","name":"grep","input":{"query":"b"}}
            ]
        }),
    )
    .unwrap();
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::ProviderState(prior),
        InferenceInput::Message(ChatMessage::tool("toolu_read", "contents")),
        InferenceInput::Message(ChatMessage::tool_result("toolu_grep", "failed", true)),
    ];
    request.tools = vec![
        ToolSpec {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        },
        ToolSpec {
            name: "grep".to_owned(),
            description: "Search files".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        },
    ];
    let call = adapter.resolve(request, &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");
    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_read");
    assert_eq!(messages[2]["content"][1]["tool_use_id"], "toolu_grep");
    assert!(messages[2]["content"][0].get("is_error").is_none());
    assert_eq!(messages[2]["content"][1]["is_error"], true);
}

#[tokio::test]
async fn stream_normalizes_interleaved_thinking_client_and_server_tools_losslessly() {
    let events = vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start","message":{
                    "id":"msg_blocks","type":"message","role":"assistant","model":"claude-fixture",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{
                        "input_tokens":12,"cache_creation_input_tokens":3,
                        "cache_read_input_tokens":4,"output_tokens":1
                    }
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"thinking","thinking":"","signature":""}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"thinking_delta","thinking":"think one"}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"signature_delta","signature":"signature-one"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"path\":"}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"\"a\"}"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":1}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":2,
                "content_block":{"type":"thinking","thinking":"","signature":""}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":2,
                "delta":{"type":"thinking_delta","thinking":"think two"}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":2,
                "delta":{"type":"signature_delta","signature":"signature-two"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":2}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":3,
                "content_block":{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{}}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":3,
                "delta":{"type":"input_json_delta","partial_json":"{\"query\":\"rust\"}"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":3}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":4,
                "content_block":{
                    "type":"web_search_tool_result","tool_use_id":"srvtoolu_1",
                    "content":[{"type":"web_search_result","title":"Rust","url":"https://example.test","encrypted_content":"opaque"}]
                }
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":4}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":5,
                "content_block":{"type":"text","text":"","citations":[]}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":5,
                "delta":{"type":"text_delta","text":"answer"}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":5,
                "delta":{"type":"citations_delta","citation":{
                    "type":"web_search_result_location","cited_text":"Rust","encrypted_index":"idx",
                    "title":"Rust","url":"https://example.test"
                }}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":5}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
                "usage":{
                    "input_tokens":20,"cache_creation_input_tokens":3,
                    "cache_read_input_tokens":5,"output_tokens":31,
                    "server_tool_use":{"web_search_requests":1}
                }
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ];
    let (adapter, _) = make_adapter(events);
    let mut request = draft();
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    request.native_features.push(NativeFeature::Web);
    let call = adapter.resolve(request, &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");
    assert!(
        matches!(&events[0], Ok(InferenceEvent::ResponseStarted { response_id }) if response_id == "msg_blocks")
    );
    assert!(events.iter().any(|item| matches!(
        item,
        Ok(InferenceEvent::ReasoningDelta(text)) if text == "think one"
    )));
    assert!(events.iter().any(|item| matches!(
        item,
        Ok(InferenceEvent::ToolCallDelta {
            output_index:1,id:Some(id),name:Some(name),arguments_delta
        }) if id == &CallId::from_raw("toolu_1") && name == "read" && arguments_delta == "{\"path\":"
    )));
    assert_eq!(
        events
            .iter()
            .filter(|item| matches!(item, Ok(InferenceEvent::ToolCallDelta { .. })))
            .count(),
        2,
        "server tool input must not become a client-executable call"
    );
    let server_call = events
        .iter()
        .position(|item| {
            matches!(item, Ok(InferenceEvent::ServerToolCall { call, .. })
            if call.id() == &CallId::from_raw("srvtoolu_1")
                && call.logical() == "web_search"
                && call.input()["query"] == "rust")
        })
        .expect("normalized server call");
    let server_result = events
        .iter()
        .position(|item| {
            matches!(item, Ok(InferenceEvent::ServerToolResult { result, .. })
            if result.call_id() == &CallId::from_raw("srvtoolu_1")
                && result.output_count() == Some(1)
                && result.sources()[0].url() == "https://example.test")
        })
        .expect("normalized server result");
    let citation = events
        .iter()
        .position(|item| {
            matches!(item, Ok(InferenceEvent::Citation { citation, .. })
            if citation.url() == "https://example.test"
                && citation.title() == Some("Rust")
                && citation.cited_text() == Some("Rust"))
        })
        .expect("normalized citation");
    assert!(server_call < server_result && server_result < citation);
    let state = events
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.kind(), ProviderStateKind::AnthropicMessage);
    assert_eq!(state.data()["content"][0]["signature"], "signature-one");
    assert_eq!(state.data()["content"][1]["input"]["path"], "a");
    assert_eq!(state.data()["content"][2]["thinking"], "think two");
    assert_eq!(state.data()["content"][3]["input"]["query"], "rust");
    assert_eq!(
        state.data()["content"][4]["content"][0]["encrypted_content"],
        "opaque"
    );
    assert_eq!(
        state.data()["content"][5]["citations"][0]["encrypted_index"],
        "idx"
    );
    assert!(matches!(events.as_slice(), [..,
        Ok(InferenceEvent::ResponseFinished { status, .. }),
        Ok(InferenceEvent::ServerToolUsage(server_usage)),
        Ok(InferenceEvent::Usage(usage)),
        Ok(InferenceEvent::Finish(FinishReason::ToolCalls))
    ] if status == "tool_use"
        && server_usage.logical() == "web_search"
        && server_usage.requests() == 1
        && usage.prompt_tokens == 28
        && usage.completion_tokens == 31));
}

#[tokio::test]
async fn orphan_server_result_and_unsafe_citation_fail_without_state_or_finish() {
    let orphan_result = vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start","message":{
                    "id":"msg_orphan","type":"message","role":"assistant","model":"claude-fixture",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":1,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_missing","content":[]}
            }),
        ),
    ];
    let unsafe_citation = vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start","message":{
                    "id":"msg_citation","type":"message","role":"assistant","model":"claude-fixture",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":1,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"text","text":"","citations":[]}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"citations_delta","citation":{
                    "type":"web_search_result_location","cited_text":"unsafe",
                    "encrypted_index":"opaque","title":"Unsafe","url":"javascript:alert(1)"
                }}
            }),
        ),
    ];

    for events in [orphan_result, unsafe_citation] {
        let (adapter, _) = make_adapter(events);
        let mut request = draft();
        request.native_features.push(NativeFeature::Web);
        let call = adapter.resolve(request, &model()).unwrap();
        let output = adapter.stream(call).collect::<Vec<_>>().await;
        assert!(output.iter().any(Result::is_err), "{output:#?}");
        assert!(!output.iter().any(|event| matches!(
            event,
            Ok(InferenceEvent::ProviderState(_) | InferenceEvent::Finish(_))
        )));
    }
}

#[tokio::test]
async fn pause_turn_is_distinct_and_retains_the_exact_pending_server_tool_message() {
    let events = vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start","message":{
                    "id":"msg_pause","type":"message","role":"assistant","model":"claude-fixture",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":9,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"server_tool_use","id":"srvtoolu_wait","name":"web_search","input":{}}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"query\":\"latest\"}"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta","delta":{"stop_reason":"pause_turn","stop_sequence":null},
                "usage":{"output_tokens":8}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ];
    let (adapter, _) = make_adapter(events);
    let mut request = draft();
    request.native_features.push(NativeFeature::Web);
    let call = adapter.resolve(request, &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    let state = events
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.data()["content"][0]["id"], "srvtoolu_wait");
    assert_eq!(state.data()["content"][0]["input"]["query"], "latest");
    assert!(matches!(events.as_slice(), [..,
        Ok(InferenceEvent::ResponseFinished { status, .. }),
        Ok(InferenceEvent::Usage(_)),
        Ok(InferenceEvent::Finish(FinishReason::Pause))
    ] if status == "pause_turn"));

    let paused_state = (*state).clone();
    let (continuation, transport) = make_adapter(continued_server_result_events(
        "msg_continued",
        "srvtoolu_wait",
    ));
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("search")),
        InferenceInput::ProviderState(paused_state.clone()),
    ];
    request.native_features.push(NativeFeature::Web);
    let call = continuation.resolve(request, &model()).unwrap();
    let continued = continuation.stream(call).collect::<Vec<_>>().await;
    assert!(continued.iter().all(Result::is_ok), "{continued:#?}");
    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["messages"][1], paused_state.data().clone());
}

#[tokio::test]
async fn configured_server_tool_plan_normalizes_code_mcp_usage_and_exact_state() {
    let events = vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start","message":{
                    "id":"msg_plan","type":"message","role":"assistant","model":"claude-fixture",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":5,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"server_tool_use","id":"srv_code_1",
                    "name":"bash_code_execution","input":{"code":"pwd"}}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":1,
                "content_block":{"type":"bash_code_execution_tool_result",
                    "tool_use_id":"srv_code_1","content":{
                        "type":"bash_code_execution_result","stdout":"private output"
                    }}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":1}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":2,
                "content_block":{"type":"mcp_tool_use","id":"mcptoolu_1",
                    "name":"remote_lookup","server_name":"fixture","input":{"id":"1"}}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":2}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":3,
                "content_block":{"type":"mcp_tool_result","tool_use_id":"mcptoolu_1",
                    "is_error":false,"content":[{"type":"text","text":"private MCP output"}]}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":3}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                "usage":{"output_tokens":7,
                    "server_tool_use":{"code_execution_requests":1}}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ];
    let transport = Arc::new(ScriptedTransport::new(events));
    let plan = provider_server_tool_plan();
    let option = plan.provider_option("anthropic-fixture").unwrap();
    let config = base_config()
        .with_server_tool_plan(plan)
        .with_externally_consumed_provider_option_kind("context-edit");
    let adapter =
        AnthropicMessagesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap();
    let mut request = draft();
    request.provider_options.push(
        ProviderRequestOption::new(
            "anthropic-fixture",
            "context-edit",
            serde_json::json!({"strategy":"clear_tool_uses"}),
        )
        .unwrap(),
    );
    request.provider_options.push(option);
    let call = adapter.resolve(request, &model()).unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:#?}");
    assert!(output.iter().any(|event| matches!(event,
        Ok(InferenceEvent::ServerToolCall { call, .. })
            if call.id() == &CallId::from_raw("srv_code_1")
                && call.logical() == "code_execution"
                && call.provider_name() == "bash_code_execution"
                && call.input()["code"] == "pwd"
    )));
    assert!(output.iter().any(|event| matches!(event,
        Ok(InferenceEvent::ServerToolCall { call, .. })
            if call.id() == &CallId::from_raw("mcptoolu_1")
                && call.logical() == "remote_mcp"
                && call.provider_name() == "remote_lookup"
    )));
    assert_eq!(
        output
            .iter()
            .filter(|event| matches!(event, Ok(InferenceEvent::ServerToolResult { .. })))
            .count(),
        2
    );
    assert!(output.iter().any(|event| matches!(event,
        Ok(InferenceEvent::ServerToolUsage(usage))
            if usage.logical() == "code_execution" && usage.requests() == 1
    )));
    let state = output
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        state.data()["content"][1]["content"]["stdout"],
        "private output"
    );
    assert_eq!(
        state.data()["content"][3]["content"][0]["text"],
        "private MCP output"
    );
    assert!(matches!(
        output.as_slice(),
        [
            ..,
            Ok(InferenceEvent::ServerToolUsage(_)),
            Ok(InferenceEvent::Usage(_)),
            Ok(InferenceEvent::Finish(FinishReason::Stop))
        ]
    ));

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["tools"][0]["type"], "code_execution_20260521");
    assert_eq!(body["tools"][1]["type"], "mcp_toolset");
    assert_eq!(body["mcp_servers"][0]["url"], "https://mcp.example.test");
    let beta = captured
        .headers
        .iter()
        .find(|(name, _)| name == "anthropic-beta")
        .map(|(_, value)| value.as_str())
        .unwrap();
    assert!(beta.contains("interleaved-thinking-2025-05-14"));
    assert!(beta.contains("code-execution-2026-05-21"));
    assert!(beta.contains("mcp-client-2025-11-20"));
}

#[test]
fn undeclared_messages_provider_option_fails_before_transport() {
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));
    let adapter = AnthropicMessagesAdapter::new(
        base_config(),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let mut request = draft();
    request.provider_options.push(
        ProviderRequestOption::new(
            "anthropic-fixture",
            "unowned-option",
            serde_json::json!({"enabled":true}),
        )
        .unwrap(),
    );
    assert!(adapter.resolve(request, &model()).is_err());
    assert!(transport.captured.lock().unwrap().is_none());
}

#[tokio::test]
async fn completed_server_history_keeps_replay_beta_without_reauthorizing_tools() {
    let transport = Arc::new(ScriptedTransport::new(terminal_text_events(
        "msg_after_history",
    )));
    let plan = provider_server_tool_plan();
    let adapter = AnthropicMessagesAdapter::new(
        base_config().with_server_tool_plan(plan),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let completed = ProviderStateItem::new(
        "anthropic-fixture",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[
                {"type":"server_tool_use","id":"srv_done","name":"bash_code_execution",
                    "input":{"code":"pwd"}},
                {"type":"bash_code_execution_tool_result","tool_use_id":"srv_done",
                    "content":{"type":"bash_code_execution_result","stdout":"private"}}
            ]
        }),
    )
    .unwrap();
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("run")),
        InferenceInput::ProviderState(completed.clone()),
        InferenceInput::Message(ChatMessage::user("continue")),
    ];
    let call = adapter.resolve(request, &model()).unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:#?}");
    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["messages"][1], completed.data().clone());
    assert!(body.get("tools").is_none());
    assert!(body.get("mcp_servers").is_none());
    let beta = captured
        .headers
        .iter()
        .find(|(name, _)| name == "anthropic-beta")
        .map(|(_, value)| value.as_str())
        .unwrap();
    assert!(beta.contains("code-execution-2026-05-21"));

    let pending = ProviderStateItem::new(
        "anthropic-fixture",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{"type":"server_tool_use","id":"srv_pending",
                "name":"bash_code_execution","input":{"code":"pwd"}}]
        }),
    )
    .unwrap();
    let mut request = draft();
    request.inputs.push(InferenceInput::ProviderState(pending));
    assert!(adapter.resolve(request, &model()).is_err());
}

#[tokio::test]
async fn iteration_pause_without_a_pending_server_call_still_retains_exact_state() {
    let events = vec![
        terminal_text_events("msg_empty_pause")[0].clone(),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta","delta":{"stop_reason":"pause_turn","stop_sequence":null},
                "usage":{"output_tokens":1}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ];
    let (adapter, _) = make_adapter(events);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:#?}");
    assert!(output.iter().any(|event| matches!(event,
        Ok(InferenceEvent::ProviderState(state)) if state.data()["content"] == serde_json::json!([])
    )));
    assert!(matches!(
        output.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Pause)))
    ));
}

#[tokio::test]
async fn empty_object_client_tool_input_settles_to_complete_json_arguments() {
    let events = vec![
        terminal_text_events("msg_empty_tool")[0].clone(),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"toolu_empty","name":"status","input":{}}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":""}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
                "usage":{"output_tokens":2}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ];
    let (adapter, _) = make_adapter(events);
    let mut request = draft();
    request.tools.push(ToolSpec {
        name: "status".to_owned(),
        description: "Read status".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    let call = adapter.resolve(request, &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    let arguments = events
        .iter()
        .filter_map(|event| match event {
            Ok(InferenceEvent::ToolCallDelta {
                arguments_delta, ..
            }) => Some(arguments_delta.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(arguments, "{}");
    assert!(matches!(
        events.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::ToolCalls)))
    ));
}

#[tokio::test]
async fn programmatic_tool_container_is_preserved_in_state_and_rebound_on_continuation() {
    let events = vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start","message":{
                    "id":"msg_container","type":"message","role":"assistant","model":"claude-fixture",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "container":{"id":"container_1","expires_at":"2026-08-25T01:02:03Z"},
                    "usage":{"input_tokens":5,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{
                    "type":"tool_use","id":"toolu_programmatic","name":"read","input":{},
                    "caller":{"type":"code_execution_20260120","tool_id":"srvtoolu_code"}
                }
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\"}"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
                "usage":{"output_tokens":7}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ];
    let (adapter, _) = make_adapter(events);
    let mut request = draft();
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    let call = adapter.resolve(request, &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    let state = events
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some((*state).clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.data()["container"]["id"], "container_1");
    assert_eq!(
        state.data()["content"][0]["caller"]["tool_id"],
        "srvtoolu_code"
    );

    let (continuation, transport) = make_adapter(terminal_text_events("msg_after_container"));
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("read")),
        InferenceInput::ProviderState(state.clone()),
        InferenceInput::Message(ChatMessage::tool("toolu_programmatic", "contents")),
    ];
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    let call = continuation.resolve(request, &model()).unwrap();
    let continued = continuation.stream(call).collect::<Vec<_>>().await;
    assert!(continued.iter().all(Result::is_ok), "{continued:#?}");
    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["container"], "container_1");
    assert_eq!(body["messages"][1]["role"], "assistant");
    assert_eq!(body["messages"][1]["content"], state.data()["content"]);
    assert!(body["messages"][1].get("container").is_none());
}

#[tokio::test]
async fn unknown_complete_blocks_and_ping_are_forward_compatible_and_lossless() {
    let events = vec![
        terminal_text_events("msg_future")[0].clone(),
        event("ping", serde_json::json!({"type":"ping"})),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"future_block","opaque":{"kept":true}}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":1,
                "content_block":{"type":"compaction","content":""}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":1,
                "delta":{"type":"compaction_delta","content":"summary"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":1}),
        ),
        event(
            "future_message_event",
            serde_json::json!({"type":"future_message_event","opaque":true}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                "usage":{"output_tokens":2}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ];
    let (adapter, _) = make_adapter(events);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    let state = events
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.data()["content"][0]["type"], "future_block");
    assert_eq!(state.data()["content"][0]["opaque"]["kept"], true);
    assert_eq!(state.data()["content"][1]["content"], "summary");
    assert!(matches!(
        events.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
    ));
}

#[tokio::test]
async fn malformed_lifecycle_block_delta_thinking_tool_and_terminal_shapes_fail_closed() {
    let scripts = vec![
        vec![event("message_start", serde_json::json!({"type":"ping"}))],
        vec![event(
            "provider-secret-canary",
            serde_json::json!({"type":"ping"}),
        )],
        vec![
            terminal_text_events("msg_gap")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":1,
                    "content_block":{"type":"text","text":""}
                }),
            ),
        ],
        vec![
            terminal_text_events("msg_unadvertised_client")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"tool_use","id":"toolu_unknown","name":"write","input":{}}
                }),
            ),
        ],
        vec![
            terminal_text_events("msg_unadvertised_server")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"server_tool_use","id":"srvtoolu_unknown","name":"code_execution","input":{}}
                }),
            ),
        ],
        vec![
            terminal_text_events("msg_thinking")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"thinking","thinking":"","signature":""}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"thinking_delta","thinking":"unsigned"}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
        ],
        vec![
            terminal_text_events("msg_private_delta")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"text","text":""}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"provider-secret-canary","text":"private"}
                }),
            ),
        ],
        vec![
            terminal_text_events("msg_json")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"tool_use","id":"toolu_bad","name":"read","input":{}}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"input_json_delta","partial_json":"[1]"}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
        ],
        vec![
            terminal_text_events("msg_tool_reason")[0].clone(),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
                    "usage":{"output_tokens":1}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            terminal_text_events("msg_private_reason")[0].clone(),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta",
                    "delta":{"stop_reason":"provider-secret-canary","stop_sequence":null},
                    "usage":{"output_tokens":1}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            terminal_text_events("msg_pause_client")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"tool_use","id":"toolu_wait","name":"read","input":{}}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"pause_turn","stop_sequence":null},
                    "usage":{"output_tokens":1}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            terminal_text_events("msg_unresolved_server")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"server_tool_use","id":"srvtoolu_open","name":"web_search","input":{}}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"input_json_delta","partial_json":"{\"query\":\"open\"}"}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"output_tokens":2}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        terminal_text_events_for_model("msg_wrong_model", "claude-other"),
        vec![
            terminal_text_events("msg_after_delta")[0].clone(),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":null,"stop_sequence":null},
                    "usage":{"output_tokens":1}
                }),
            ),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"text","text":""}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"text_delta","text":"late"}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"output_tokens":2}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            terminal_text_events("msg_thinking_after_signature")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"thinking","thinking":"","signature":""}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"signature_delta","signature":"opaque"}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"thinking_delta","thinking":"late thinking"}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"output_tokens":2}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            terminal_text_events("msg_compaction_missing_delta")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"compaction","content":""}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"output_tokens":2}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            terminal_text_events("msg_compaction_duplicate_delta")[0].clone(),
            event(
                "content_block_start",
                serde_json::json!({
                    "type":"content_block_start","index":0,
                    "content_block":{"type":"compaction","content":""}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"compaction_delta","content":"one"}
                }),
            ),
            event(
                "content_block_delta",
                serde_json::json!({
                    "type":"content_block_delta","index":0,
                    "delta":{"type":"compaction_delta","content":"two"}
                }),
            ),
            event(
                "content_block_stop",
                serde_json::json!({"type":"content_block_stop","index":0}),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"output_tokens":2}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            event(
                "message_start",
                serde_json::json!({
                    "type":"message_start","message":{
                        "id":"msg_cache_decrease","type":"message","role":"assistant",
                        "model":"claude-fixture","content":[],"stop_reason":null,"stop_sequence":null,
                        "usage":{
                            "input_tokens":5,"cache_creation_input_tokens":4,
                            "cache_read_input_tokens":2,"output_tokens":1
                        }
                    }
                }),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"cache_creation_input_tokens":3,"output_tokens":2}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![
            event(
                "message_start",
                serde_json::json!({
                    "type":"message_start","message":{
                        "id":"msg_usage_overflow","type":"message","role":"assistant",
                        "model":"claude-fixture","content":[],"stop_reason":null,"stop_sequence":null,
                        "usage":{
                            "input_tokens":18446744073709551615u64,
                            "cache_creation_input_tokens":1,"cache_read_input_tokens":0,
                            "output_tokens":1
                        }
                    }
                }),
            ),
            event(
                "message_delta",
                serde_json::json!({
                    "type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"output_tokens":2}
                }),
            ),
            event("message_stop", serde_json::json!({"type":"message_stop"})),
        ],
        vec![terminal_text_events("msg_eof")[0].clone()],
        vec![event(
            "error",
            serde_json::json!({
                "type":"error","error":{"type":"provider-secret-canary","message":"secret body"}
            }),
        )],
    ];
    for script in scripts {
        let (adapter, _) = make_adapter(script);
        let mut request = draft();
        request.tools.push(ToolSpec {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        });
        request.native_features.push(NativeFeature::Web);
        let call = adapter.resolve(request, &model()).unwrap();
        let events = adapter.stream(call).collect::<Vec<_>>().await;
        assert!(events.iter().any(Result::is_err), "{events:#?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(InferenceEvent::ProviderState(_))))
        );
        let diagnostic = format!("{events:?}");
        for canary in ["secret body", "provider-secret-canary"] {
            assert!(!diagnostic.contains(canary), "{diagnostic}");
        }
    }
}

#[tokio::test]
async fn message_start_requires_both_terminal_fields_to_be_null() {
    let mut script = terminal_text_events("msg_bad_start_sequence");
    let start = script[0].as_mut().unwrap();
    let mut payload: serde_json::Value = serde_json::from_str(&start.data).unwrap();
    payload["message"]["stop_sequence"] = serde_json::json!("unexpected");
    start.data = payload.to_string();

    let (adapter, _) = make_adapter(script);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().any(Result::is_err), "{events:#?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
    );
}

#[test]
fn resolution_rejects_unrepresented_or_malformed_inputs_before_transport() {
    let (adapter, transport) = make_adapter(Vec::new());
    let mut system = draft();
    system
        .inputs
        .push(InferenceInput::Message(ChatMessage::system("late")));
    assert!(adapter.resolve(system, &model()).is_err());

    let mut bad_tool = draft();
    bad_tool.inputs.push(InferenceInput::Message(
        ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "toolu_bad".to_owned(),
                name: "read".to_owned(),
                arguments: "[]".to_owned(),
            }],
        ),
    ));
    assert!(adapter.resolve(bad_tool, &model()).is_err());

    let mut missing_result = draft();
    missing_result.inputs.push(InferenceInput::Message(
        ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "toolu_pending".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            }],
        ),
    ));
    assert!(adapter.resolve(missing_result, &model()).is_err());

    let mut orphan_result = draft();
    orphan_result
        .inputs
        .push(InferenceInput::Message(ChatMessage::tool(
            "toolu_orphan",
            "result",
        )));
    assert!(adapter.resolve(orphan_result, &model()).is_err());

    let programmatic_without_container = ProviderStateItem::new(
        "anthropic-fixture",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{
                "type":"tool_use","id":"toolu_programmatic","name":"read","input":{},
                "caller":{"type":"code_execution_20260120","tool_id":"srvtoolu_code"}
            }]
        }),
    )
    .unwrap();
    let mut missing_container = draft();
    missing_container.inputs = vec![
        InferenceInput::Message(ChatMessage::user("read")),
        InferenceInput::ProviderState(programmatic_without_container),
        InferenceInput::Message(ChatMessage::tool("toolu_programmatic", "contents")),
    ];
    missing_container.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    assert!(adapter.resolve(missing_container, &model()).is_err());

    let wrong_state = ProviderStateItem::new(
        "other",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({"role":"assistant","content":[]}),
    )
    .unwrap();
    let mut wrong = draft();
    wrong
        .inputs
        .push(InferenceInput::ProviderState(wrong_state));
    assert!(adapter.resolve(wrong, &model()).is_err());

    let mut structured = draft();
    structured.structured_output = Some(serde_json::json!({"type":"object"}));
    let mut structured_model = model();
    structured_model.capabilities.structured_output = CapabilitySupport::Supported;
    assert!(adapter.resolve(structured, &structured_model).is_err());

    for feature in [NativeFeature::Compaction, NativeFeature::PromptCache] {
        let mut request = draft();
        request.native_features.push(feature);
        let mut supported = model();
        match feature {
            NativeFeature::Compaction => {
                supported.capabilities.native_compaction = CapabilitySupport::Supported
            }
            NativeFeature::PromptCache => {
                supported.capabilities.prompt_cache = CapabilitySupport::Supported
            }
            NativeFeature::Web => unreachable!(),
        }
        assert!(adapter.resolve(request, &supported).is_err());
    }

    let no_web_config =
        AnthropicMessagesConfig::with_key(provider(), "https://api.anthropic.test/v1", "key")
            .with_default_max_output_tokens(Some(1_024));
    let no_web = AnthropicMessagesAdapter::new(
        no_web_config,
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let mut web = draft();
    web.native_features.push(NativeFeature::Web);
    assert!(no_web.resolve(web, &model()).is_err());

    let pending_server_state = ProviderStateItem::new(
        "anthropic-fixture",
        "claude-fixture",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{
                "type":"server_tool_use","id":"srvtoolu_pending",
                "name":"web_search","input":{"query":"latest"}
            }]
        }),
    )
    .unwrap();
    let mut missing_definition = draft();
    missing_definition
        .inputs
        .push(InferenceInput::ProviderState(pending_server_state));
    assert!(no_web.resolve(missing_definition, &model()).is_err());
    assert!(transport.captured.lock().unwrap().is_none());
}

#[test]
fn manual_thinking_budget_requires_explicit_interleaved_route_evidence() {
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));
    let high = ReasoningEffortId::new("high").unwrap();
    let config =
        AnthropicMessagesConfig::with_key(provider(), "https://api.anthropic.test/v1", "key")
            .with_default_max_output_tokens(Some(2_048))
            .with_thinking(
                vec![(high.clone(), AnthropicThinkingMode::enabled(4_096, None))],
                None,
            );
    let adapter =
        AnthropicMessagesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap();
    let mut request = draft();
    request.reasoning_effort = Some(high.clone());
    assert!(adapter.resolve(request, &model()).is_err());

    let config =
        AnthropicMessagesConfig::with_key(provider(), "https://api.anthropic.test/v1", "key")
            .with_default_max_output_tokens(Some(2_048))
            .with_thinking(
                vec![(
                    high.clone(),
                    AnthropicThinkingMode::enabled(4_096, None).with_interleaved_budget(),
                )],
                None,
            );
    let adapter =
        AnthropicMessagesAdapter::new(config, heycode_http::HttpService::new(transport)).unwrap();
    let mut request = draft();
    request.reasoning_effort = Some(high);
    assert!(adapter.resolve(request, &model()).is_ok());
}

#[test]
fn config_is_redacted_and_declares_exact_protocol_auth_and_bearer_variant() {
    let config = base_config();
    let spec: ResolveSpec = config.resolve_spec();
    assert_eq!(spec.protocol, ProviderProtocol::AnthropicMessages);
    assert_eq!(
        spec.authentication,
        AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new())
    );
    assert!(!format!("{config:?}").contains("test-key"));

    let bearer = AnthropicMessagesConfig::with_key(
        provider(),
        "https://compatible.test/anthropic/v1",
        "bearer-secret",
    )
    .with_auth_wire(AnthropicAuthWire::Bearer)
    .with_anthropic_version(None)
    .with_default_max_output_tokens(Some(1_024));
    assert!(!format!("{bearer:?}").contains("bearer-secret"));
}

#[tokio::test]
async fn bearer_route_omits_native_version_header_and_uses_adaptive_output_config_effort() {
    let transport = Arc::new(ScriptedTransport::new(terminal_text_events("msg_bearer")));
    let medium = ReasoningEffortId::new("medium").unwrap();
    let config = AnthropicMessagesConfig::with_key(
        provider(),
        "https://compatible.test/anthropic/v1",
        "bearer-secret",
    )
    .with_auth_wire(AnthropicAuthWire::Bearer)
    .with_anthropic_version(None)
    .with_default_max_output_tokens(Some(2_048))
    .with_thinking(
        vec![(
            medium.clone(),
            AnthropicThinkingMode::adaptive(Some(AnthropicThinkingDisplay::Summarized))
                .with_wire_effort("medium"),
        )],
        None,
    );
    let adapter =
        AnthropicMessagesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap();
    let mut request = draft();
    request.reasoning_effort = Some(medium);
    let call = adapter.resolve(request, &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");
    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| { name == "authorization" && value == "Bearer bearer-secret" })
    );
    assert!(
        !captured
            .headers
            .iter()
            .any(|(name, _)| { name == "x-api-key" || name == "anthropic-version" })
    );
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["thinking"]["display"], "summarized");
    assert_eq!(body["output_config"]["effort"], "medium");
}

#[tokio::test]
async fn exact_model_endpoint_dialect_moves_model_and_version_without_forking_the_parser() {
    let transport = Arc::new(ScriptedTransport::new(terminal_text_events("msg_vertex")));
    let dialect = AnthropicMessagesDialect::exact_model_endpoint(
        "https://aiplatform.googleapis.test/v1/projects/p/locations/global/publishers/anthropic/models/claude-fixture:streamRawPredict",
        "claude-fixture",
    )
    .with_body_field(
        "anthropic_version",
        serde_json::json!("vertex-2023-10-16"),
    );
    let config = AnthropicMessagesConfig::with_key(
        provider(),
        "https://unused-native-base.test/v1",
        "oauth-token",
    )
    .with_auth_wire(AnthropicAuthWire::Bearer)
    .with_anthropic_version(None)
    .with_dialect(dialect)
    .with_default_max_output_tokens(Some(2_048))
    .with_retry_spec(heycode_llm::RetrySpec::no_retry());
    let adapter =
        AnthropicMessagesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap();

    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert_eq!(
        captured.url,
        "https://aiplatform.googleapis.test/v1/projects/p/locations/global/publishers/anthropic/models/claude-fixture:streamRawPredict"
    );
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| { name == "authorization" && value == "Bearer oauth-token" })
    );
    assert!(
        captured
            .headers
            .iter()
            .all(|(name, _)| name != "anthropic-version")
    );
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert!(body.get("model").is_none());
    assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
}

#[test]
fn malformed_route_metadata_fails_at_adapter_construction_without_secret_echo() {
    let high = ReasoningEffortId::new("high").unwrap();
    let wrong_provider = ProviderDescriptor {
        id: "anthropic-fixture".to_owned(),
        display_name: "Wrong Protocol".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    };
    let configs = vec![
        AnthropicMessagesConfig::with_key(
            wrong_provider,
            "https://api.anthropic.test/v1",
            "secret-sentinel",
        ),
        AnthropicMessagesConfig::with_key(
            provider(),
            "https://api.anthropic.test/v1",
            "secret-sentinel",
        )
        .with_thinking(
            vec![
                (high.clone(), AnthropicThinkingMode::adaptive(None)),
                (high.clone(), AnthropicThinkingMode::adaptive(None)),
            ],
            None,
        ),
        AnthropicMessagesConfig::with_key(
            provider(),
            "https://api.anthropic.test/v1",
            "secret-sentinel",
        )
        .with_thinking(Vec::new(), Some(high.clone())),
        AnthropicMessagesConfig::with_key(
            provider(),
            "https://api.anthropic.test/v1",
            "secret-sentinel",
        )
        .with_thinking(
            vec![(high, AnthropicThinkingMode::enabled(1_023, None))],
            None,
        ),
        AnthropicMessagesConfig::with_key(
            provider(),
            "https://api.anthropic.test/v1",
            "secret-sentinel",
        )
        .with_extra_headers(vec![("x-api-key".to_owned(), "override".to_owned())]),
        AnthropicMessagesConfig::with_key(
            provider(),
            "https://api.anthropic.test/v1",
            "secret-sentinel\nleak",
        ),
        AnthropicMessagesConfig::with_key(
            provider(),
            "https://api.anthropic.test/v1",
            "secret-sentinel",
        )
        .with_default_max_output_tokens(Some(0)),
    ];
    for config in configs {
        let transport = Arc::new(ScriptedTransport::new(Vec::new()));
        let error = match AnthropicMessagesAdapter::new(
            config,
            heycode_http::HttpService::new(transport),
        ) {
            Ok(_) => panic!("malformed Messages route must fail construction"),
            Err(error) => error,
        };
        assert!(!error.to_string().contains("secret-sentinel"), "{error}");
    }
}

#[tokio::test]
async fn raw_sse_fragmentation_is_protocol_invariant() {
    let wire = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_frag\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-fixture\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hé\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let metadata = ConformanceFixtureMetadata::new(
        "anthropic",
        ConformanceSourceKind::Synthetic,
        "https://platform.claude.com/docs/en/api/messages",
        "messages-2023-06-01",
        1_788_048_000_000,
    )
    .unwrap();
    let cases = SseConformanceFixture::new("anthropic-terminal", metadata, wire.as_bytes())
        .unwrap()
        .fragmentation_cases();
    let runs = run_sse_conformance(&cases, |http| async move {
        let adapter = AnthropicMessagesAdapter::new(base_config(), http).unwrap();
        let call = adapter.resolve(draft(), &model()).unwrap();
        adapter.stream(call).collect::<Vec<_>>().await
    })
    .await;
    let expected = runs[0]
        .output
        .iter()
        .map(|event| event.as_ref().unwrap().clone())
        .collect::<Vec<_>>();
    assert!(matches!(expected.as_slice(), [
        InferenceEvent::ResponseStarted { .. },
        InferenceEvent::ItemStarted { kind: StreamItemKind::Message, .. },
        InferenceEvent::TextDelta(text),
        InferenceEvent::ItemFinished { kind: StreamItemKind::Message, .. },
        InferenceEvent::ProviderState(_),
        InferenceEvent::ResponseFinished { .. },
        InferenceEvent::Usage(_),
        InferenceEvent::Finish(FinishReason::Stop),
    ] if text == "hé"));
    for run in runs {
        assert_eq!(run.transport_calls, 1, "case {}", run.case);
        assert_eq!(
            run.output
                .into_iter()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            expected,
            "case {} drifted",
            run.case
        );
    }
}

//! OpenAI Responses request, phases, state, tools, usage and terminal contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::{CallId, ProviderRequestOption};
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatMessage,
    ChatToolCall, FinishReason, InferenceAdapter, InferenceEvent, InferenceInput, InputModality,
    ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing,
    NativeFeature, OpenAiResponsesAdapter, OpenAiResponsesConfig, ProviderDescriptor,
    ProviderProtocol, ProviderStateItem, ProviderStateKind, ReasoningEffortId, RequestDraft,
    ResolveSpec, ResponsesContinuation, ResponsesServerToolDefinition, ResponsesServerToolFault,
    ResponsesServerToolNormalization, ResponsesServerToolNormalizer, ResponsesServerToolPlan,
    StreamItemKind, ToolSpec,
};
use tokio_util::sync::CancellationToken;

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "openai".to_owned(),
        display_name: "OpenAI".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiResponses],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "gpt-test".to_owned(),
        display_name: "GPT Test".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(128_000),
        max_output_tokens: Some(16_384),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            image_input: CapabilitySupport::Unsupported,
            document_input: CapabilitySupport::Unsupported,
            structured_output: CapabilitySupport::Supported,
            native_web: CapabilitySupport::Supported,
            native_compaction: CapabilitySupport::Unsupported,
            prompt_cache: CapabilitySupport::Unsupported,
        },
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "openai".to_owned(),
        model: "gpt-test".to_owned(),
        catalog_revision: Some(4),
        catalog_fetched_at_ms: Some(2_000),
        effective_at_ms: 2_000,
        system: Some("Follow instructions.".to_owned()),
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

fn make_adapter(
    events: Vec<Result<SseEvent, heycode_http::TransportError>>,
) -> (OpenAiResponsesAdapter, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::new(events));
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "test-key")
            .with_reasoning(
                vec![
                    ReasoningEffortId::new("low").unwrap(),
                    ReasoningEffortId::new("high").unwrap(),
                ],
                Some(ReasoningEffortId::new("low").unwrap()),
            )
            .with_default_max_output_tokens(Some(4_096))
            .with_retry_spec(heycode_llm::RetrySpec::no_retry());
    (
        OpenAiResponsesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap(),
        transport,
    )
}

fn terminal_event(sequence_number: u64) -> Result<SseEvent, heycode_http::TransportError> {
    event(
        "response.completed",
        serde_json::json!({
            "type": "response.completed",
            "sequence_number": sequence_number,
            "response": {
                "id": "resp_1",
                "status": "completed",
                "output": [],
                "usage": {"input_tokens": 3, "output_tokens": 2}
            }
        }),
    )
}

struct FixtureResponsesServerToolNormalizer {
    output_item_type: &'static str,
}

impl FixtureResponsesServerToolNormalizer {
    const fn new(output_item_type: &'static str) -> Self {
        Self { output_item_type }
    }
}

impl ResponsesServerToolNormalizer for FixtureResponsesServerToolNormalizer {
    fn output_item_type(&self) -> &'static str {
        self.output_item_type
    }

    fn normalize(
        &self,
        state: &ProviderStateItem,
    ) -> Result<ResponsesServerToolNormalization, ResponsesServerToolFault> {
        let item = state
            .data()
            .as_object()
            .ok_or(ResponsesServerToolFault::InvalidItem)?;
        if item.get("type").and_then(serde_json::Value::as_str) != Some(self.output_item_type) {
            return Err(ResponsesServerToolFault::InvalidItem);
        }
        match self.output_item_type {
            "web_search_call" => {
                let id = item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ResponsesServerToolFault::InvalidItem)?;
                let action = item
                    .get("action")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or(ResponsesServerToolFault::InvalidItem)?;
                let call = heycode_core::ServerToolCall::new(
                    CallId::from_raw(id),
                    "web_search",
                    "web_search",
                    action,
                )
                .map_err(|_| ResponsesServerToolFault::InvalidItem)?;
                let result =
                    heycode_core::ServerToolResult::success(CallId::from_raw(id), None, Vec::new())
                        .map_err(|_| ResponsesServerToolFault::InvalidItem)?;
                ResponsesServerToolNormalization::new(Some(call), Some(result), Vec::new())
            }
            "computer_call" => Ok(ResponsesServerToolNormalization::state_only()),
            _ => Err(ResponsesServerToolFault::InvalidItem),
        }
    }
}

fn fixture_responses_server_tool_plan() -> (
    ResponsesServerToolPlan,
    ResponsesServerToolDefinition,
    ResponsesServerToolDefinition,
) {
    let web = ResponsesServerToolDefinition::new(
        serde_json::json!({"type":"web_search"}),
        vec![Arc::new(FixtureResponsesServerToolNormalizer::new(
            "web_search_call",
        ))],
    )
    .unwrap()
    .with_required_native_feature(NativeFeature::Web);
    let computer = ResponsesServerToolDefinition::new(
        serde_json::json!({"type":"computer"}),
        vec![Arc::new(FixtureResponsesServerToolNormalizer::new(
            "computer_call",
        ))],
    )
    .unwrap();
    let plan =
        ResponsesServerToolPlan::new("hosted-tools", vec![web.clone(), computer.clone()]).unwrap();
    (plan, web, computer)
}

#[tokio::test]
async fn request_serializes_state_messages_tools_reasoning_schema_and_web_without_secrets() {
    let (adapter, transport) = make_adapter(vec![terminal_event(0)]);
    let state = ProviderStateItem::new(
        "openai",
        "gpt-test",
        ProviderProtocol::OpenAiResponses,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({
            "id": "rs_1",
            "type": "reasoning",
            "encrypted_content": "opaque",
            "summary": [],
        }),
    )
    .unwrap();
    let mut request = draft();
    request
        .inputs
        .insert(0, InferenceInput::ProviderState(state));
    request.inputs.push(InferenceInput::Message(
        ChatMessage::assistant_with_tool_calls(
            "checking",
            vec![ChatToolCall {
                id: "call_1".to_owned(),
                name: "read".to_owned(),
                arguments: "{\"path\":\"a\"}".to_owned(),
            }],
        ),
    ));
    request
        .inputs
        .push(InferenceInput::Message(ChatMessage::tool(
            "call_1", "contents",
        )));
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    request.structured_output = Some(serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    }));
    request.native_features.push(NativeFeature::Web);
    request.temperature = Some(0.3);
    request.max_output_tokens = Some(2_048);
    let call = adapter.resolve(request, &model()).unwrap();
    let _events = adapter.stream(call).collect::<Vec<_>>().await;

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured.url, "https://api.openai.test/v1/responses");
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "authorization" && value == "Bearer test-key")
    );
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["instructions"], "Follow instructions.");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["max_output_tokens"], 2_048);
    assert!((body["temperature"].as_f64().unwrap() - 0.3).abs() < 0.000_001);
    assert_eq!(body["metadata"]["heycode_purpose"], "conversation");
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][1]["type"], "web_search");
    assert_eq!(body["input"][0]["type"], "reasoning");
    assert_eq!(body["input"][0]["encrypted_content"], "opaque");
    assert!(body["input"].as_array().unwrap().iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_1"
            && item["output"] == "contents"
    }));
    assert!(
        !String::from_utf8(captured.body)
            .unwrap()
            .contains("test-key")
    );
}

#[tokio::test]
async fn configured_hosted_tools_normalize_safe_events_citations_and_exact_replay_state() {
    let events = vec![
        event(
            "response.created",
            serde_json::json!({
                "type":"response.created","sequence_number":0,
                "response":{"id":"resp_hosted","status":"in_progress"}
            }),
        ),
        event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added","sequence_number":1,"output_index":0,
                "item":{"id":"ws_1","type":"web_search_call","status":"in_progress",
                    "action":{"type":"search","query":"rust"}}
            }),
        ),
        event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done","sequence_number":2,"output_index":0,
                "item":{"id":"ws_1","type":"web_search_call","status":"completed",
                    "action":{"type":"search","query":"rust"}}
            }),
        ),
        event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added","sequence_number":3,"output_index":1,
                "item":{"id":"msg_1","type":"message","role":"assistant","status":"in_progress","content":[]}
            }),
        ),
        event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done","sequence_number":4,"output_index":1,
                "item":{"id":"msg_1","type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":"Rust docs","annotations":[{
                        "type":"url_citation","start_index":0,"end_index":4,
                        "url":"https://www.rust-lang.org/","title":"Rust"
                    }]}]}
            }),
        ),
        event(
            "response.completed",
            serde_json::json!({
                "type":"response.completed","sequence_number":5,
                "response":{"id":"resp_hosted","status":"completed","output":[],
                    "usage":{"input_tokens":9,"output_tokens":2}}
            }),
        ),
    ];
    let transport = Arc::new(ScriptedTransport::new(events));
    let (plan, web, _) = fixture_responses_server_tool_plan();
    let option = plan
        .provider_option("openai", std::slice::from_ref(&web))
        .unwrap();
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "test-key")
            .with_default_max_output_tokens(Some(4_096))
            .with_retry_spec(heycode_llm::RetrySpec::no_retry())
            .with_server_tool_plan(plan);
    let adapter =
        OpenAiResponsesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap();
    let mut request = draft();
    request.native_features.push(NativeFeature::Web);
    request.provider_options.push(option);
    let call = adapter.resolve(request, &model()).unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:#?}");

    let server_call = output
        .iter()
        .position(|event| {
            matches!(event,
                Ok(InferenceEvent::ServerToolCall { output_index:0, call })
                    if call.id() == &CallId::from_raw("ws_1")
                        && call.logical() == "web_search"
                        && call.input()["query"] == "rust"
            )
        })
        .expect("normalized hosted call");
    let server_result = output
        .iter()
        .position(|event| {
            matches!(event,
                Ok(InferenceEvent::ServerToolResult { output_index:0, result })
                    if result.call_id() == &CallId::from_raw("ws_1")
            )
        })
        .expect("normalized hosted result");
    let citation = output
        .iter()
        .position(|event| {
            matches!(event,
                Ok(InferenceEvent::Citation { output_index:1, citation })
                    if citation.url() == "https://www.rust-lang.org/"
                        && citation.title() == Some("Rust")
                        && citation.start_index() == Some(0)
                        && citation.end_index() == Some(4)
            )
        })
        .expect("normalized completed-message citation");
    assert!(server_call < server_result && server_result < citation);
    let states = output
        .iter()
        .filter_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].data()["action"]["query"], "rust");
    assert_eq!(
        states[1].data()["content"][0]["annotations"][0]["url"],
        "https://www.rust-lang.org/"
    );
    assert!(matches!(
        output.as_slice(),
        [
            ..,
            Ok(InferenceEvent::ResponseFinished { .. }),
            Ok(InferenceEvent::Usage(_)),
            Ok(InferenceEvent::Finish(FinishReason::Stop))
        ]
    ));

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["tools"], serde_json::json!([{"type":"web_search"}]));
}

#[test]
fn hosted_tool_definition_selection_is_exact_and_native_web_stays_capability_gated() {
    let (plan, web, computer) = fixture_responses_server_tool_plan();
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "test-key")
            .with_server_tool_plan(plan.clone());
    let adapter = OpenAiResponsesAdapter::new(
        config,
        heycode_http::HttpService::new(Arc::new(ScriptedTransport::new(Vec::new()))),
    )
    .unwrap();

    let mut missing_feature = draft();
    missing_feature.provider_options.push(
        plan.provider_option("openai", std::slice::from_ref(&web))
            .unwrap(),
    );
    assert!(adapter.resolve(missing_feature, &model()).is_err());

    let mut missing_definition = draft();
    missing_definition.native_features.push(NativeFeature::Web);
    missing_definition.provider_options.push(
        plan.provider_option("openai", std::slice::from_ref(&computer))
            .unwrap(),
    );
    assert!(adapter.resolve(missing_definition, &model()).is_err());

    let mut unproven = model();
    unproven.capabilities.native_web = CapabilitySupport::Unknown;
    let mut request = draft();
    request.native_features.push(NativeFeature::Web);
    request
        .provider_options
        .push(plan.provider_option("openai", &[web]).unwrap());
    assert!(adapter.resolve(request, &unproven).is_err());

    let forged = ProviderRequestOption::new(
        "openai",
        "hosted-tools",
        serde_json::json!({"definitions":[{"type":"web_search","unreviewed":true}]}),
    )
    .unwrap();
    let mut request = draft();
    request.native_features.push(NativeFeature::Web);
    request.provider_options.push(forged);
    assert!(adapter.resolve(request, &model()).is_err());
}

#[tokio::test]
async fn stream_normalizes_phases_text_reasoning_tool_state_usage_and_finish_order() {
    let events = vec![
        event(
            "response.created",
            serde_json::json!({
                "type":"response.created", "sequence_number":0,
                "response":{"id":"resp_1","status":"in_progress"}
            }),
        ),
        event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added", "sequence_number":1,
                "output_index":0,
                "item":{"id":"rs_1","type":"reasoning","summary":[]}
            }),
        ),
        event(
            "response.reasoning_summary_text.delta",
            serde_json::json!({
                "type":"response.reasoning_summary_text.delta", "sequence_number":2,
                "output_index":0,"item_id":"rs_1","delta":"summary"
            }),
        ),
        event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done", "sequence_number":3,
                "output_index":0,
                "item":{"id":"rs_1","type":"reasoning","encrypted_content":"opaque","summary":[]}
            }),
        ),
        event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added", "sequence_number":4,
                "output_index":1,
                "item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"read","arguments":""}
            }),
        ),
        event(
            "response.function_call_arguments.delta",
            serde_json::json!({
                "type":"response.function_call_arguments.delta", "sequence_number":5,
                "output_index":1,"item_id":"fc_1","delta":"{\"path\""
            }),
        ),
        event(
            "response.function_call_arguments.delta",
            serde_json::json!({
                "type":"response.function_call_arguments.delta", "sequence_number":6,
                "output_index":1,"item_id":"fc_1","delta":":\"a\"}"
            }),
        ),
        event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done", "sequence_number":7,
                "output_index":1,
                "item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"read","arguments":"{\"path\":\"a\"}","status":"completed"}
            }),
        ),
        event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added", "sequence_number":8,
                "output_index":2,
                "item":{"id":"msg_1","type":"message","role":"assistant","content":[]}
            }),
        ),
        event(
            "response.output_text.delta",
            serde_json::json!({
                "type":"response.output_text.delta", "sequence_number":9,
                "output_index":2,"item_id":"msg_1","content_index":0,"delta":"done"
            }),
        ),
        event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done", "sequence_number":10,
                "output_index":2,
                "item":{"id":"msg_1","type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done","annotations":[]}]}
            }),
        ),
        terminal_event(11),
    ];
    let (adapter, _transport) = make_adapter(events);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;

    assert!(matches!(
        &events[0],
        Ok(InferenceEvent::ResponseStarted { response_id }) if response_id == "resp_1"
    ));
    assert!(matches!(
        &events[1],
        Ok(InferenceEvent::ItemStarted {
            output_index: 0,
            kind: StreamItemKind::Reasoning,
            ..
        })
    ));
    assert!(matches!(&events[2], Ok(InferenceEvent::ReasoningDelta(text)) if text == "summary"));
    assert!(
        matches!(&events[4], Ok(InferenceEvent::ProviderState(state))
        if state.kind() == ProviderStateKind::ResponseOutputItem
            && state.data()["encrypted_content"] == "opaque")
    );
    assert!(matches!(
        &events[6],
        Ok(InferenceEvent::ToolCallDelta {
            output_index: 1,
            id: Some(id),
            name: Some(name),
            arguments_delta,
        }) if id == &CallId::from_raw("call_1")
            && name == "read"
            && arguments_delta == "{\"path\""
    ));
    assert!(matches!(
        &events[7],
        Ok(InferenceEvent::ToolCallDelta {
            output_index: 1,
            id: None,
            name: None,
            arguments_delta,
        }) if arguments_delta == ":\"a\"}"
    ));
    assert!(matches!(&events[11], Ok(InferenceEvent::TextDelta(text)) if text == "done"));
    assert!(
        matches!(&events[13], Ok(InferenceEvent::ProviderState(state))
        if state.data()["phase"] == "final_answer")
    );
    assert!(
        matches!(&events[14], Ok(InferenceEvent::ResponseFinished { response_id, status })
        if response_id == "resp_1" && status == "completed")
    );
    assert!(matches!(&events[15], Ok(InferenceEvent::Usage(usage))
        if usage.prompt_tokens == 3 && usage.completion_tokens == 2));
    assert!(matches!(
        &events[16],
        Ok(InferenceEvent::Finish(FinishReason::ToolCalls))
    ));
    assert_eq!(events.len(), 17, "events: {events:#?}");
}

#[tokio::test]
async fn out_of_order_sequence_and_terminal_failure_are_errors_not_fake_finish() {
    let (adapter, _) = make_adapter(vec![
        event(
            "response.created",
            serde_json::json!({
                "type":"response.created","sequence_number":2,
                "response":{"id":"resp_1","status":"in_progress"}
            }),
        ),
        event(
            "response.output_text.delta",
            serde_json::json!({
                "type":"response.output_text.delta","sequence_number":1,
                "output_index":0,"item_id":"msg_1","content_index":0,"delta":"late"
            }),
        ),
    ]);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(matches!(
        &events[0],
        Ok(InferenceEvent::ResponseStarted { .. })
    ));
    assert!(
        matches!(&events[1], Err(heycode_llm::LlmError::InvalidResponse(message))
        if message.contains("sequence"))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
    );

    let (adapter, _) = make_adapter(vec![event(
        "response.failed",
        serde_json::json!({
            "type":"response.failed","sequence_number":0,
            "response":{"id":"resp_2","status":"failed","error":{"code":"server_error","message":"failed"}}
        }),
    )]);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    let error = events[0].as_ref().unwrap_err();
    assert_eq!(error.class(), heycode_llm::ProviderErrorClass::Server);
    assert_eq!(
        error
            .provider_failure()
            .and_then(|failure| failure.code())
            .map(heycode_llm::ProviderErrorCode::as_str),
        Some("server_error")
    );
    assert!(!format!("{error:?} {error}").contains("server_error"));
}

#[tokio::test]
async fn max_output_incomplete_maps_to_length_with_usage_before_finish() {
    let (adapter, _) = make_adapter(vec![event(
        "response.incomplete",
        serde_json::json!({
            "type":"response.incomplete","sequence_number":0,
            "response":{
                "id":"resp_limit","status":"incomplete","output":[],
                "incomplete_details":{"reason":"max_output_tokens"},
                "usage":{"input_tokens":9,"output_tokens":4}
            }
        }),
    )]);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(matches!(
        &events[..],
        [
            Ok(InferenceEvent::ResponseFinished { status, .. }),
            Ok(InferenceEvent::Usage(usage)),
            Ok(InferenceEvent::Finish(FinishReason::Length)),
        ] if status == "incomplete"
            && usage.prompt_tokens == 9
            && usage.completion_tokens == 4
    ));
}

#[tokio::test]
async fn eof_before_terminal_event_is_an_error() {
    let (adapter, _) = make_adapter(vec![event(
        "response.created",
        serde_json::json!({
            "type":"response.created","sequence_number":0,
            "response":{"id":"resp_open","status":"in_progress"}
        }),
    )]);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(matches!(
        &events[0],
        Ok(InferenceEvent::ResponseStarted { .. })
    ));
    assert!(
        matches!(&events[1], Err(heycode_llm::LlmError::InvalidResponse(message))
        if message.contains("before a terminal"))
    );
    assert_eq!(events.len(), 2);
}

#[test]
fn responses_adapter_refuses_unrepresented_image_and_unimplemented_native_features() {
    let (adapter, _) = make_adapter(Vec::new());
    let mut image = draft();
    image.input_modalities.push(InputModality::Image);
    let mut image_model = model();
    image_model.capabilities.image_input = CapabilitySupport::Supported;
    assert!(adapter.resolve(image, &image_model).is_err());

    for feature in [NativeFeature::Compaction, NativeFeature::PromptCache] {
        let mut request = draft();
        request.native_features.push(feature);
        let mut supported = model();
        match feature {
            NativeFeature::Compaction => {
                supported.capabilities.native_compaction = CapabilitySupport::Supported;
            }
            NativeFeature::PromptCache => {
                supported.capabilities.prompt_cache = CapabilitySupport::Supported;
            }
            NativeFeature::Web => unreachable!(),
        }
        assert!(adapter.resolve(request, &supported).is_err());
    }
}

#[tokio::test]
async fn configured_prompt_cache_option_projects_every_member_without_dropping_siblings() {
    let transport = Arc::new(ScriptedTransport::new(vec![event(
        "response.completed",
        serde_json::json!({
            "type":"response.completed","sequence_number":0,
            "response":{"id":"resp_cache","status":"completed","output":[],
                "usage":{
                    "input_tokens":9,
                    "input_tokens_details":{"cached_tokens":4,"cache_write_tokens":2},
                    "output_tokens":1,
                    "output_tokens_details":{"reasoning_tokens":1},
                    "total_tokens":10
                }}
        }),
    )]));
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "test-key")
            .with_provider_request_option_members(
                "prompt-cache",
                vec![
                    ("prompt_cache_key".to_owned(), "prompt_cache_key".to_owned()),
                    (
                        "prompt_cache_options".to_owned(),
                        "prompt_cache_options".to_owned(),
                    ),
                ],
            );
    let adapter =
        OpenAiResponsesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap();
    let option = ProviderRequestOption::new(
        "openai",
        "prompt-cache",
        serde_json::json!({
            "prompt_cache_key":"session-cache-01",
            "prompt_cache_options":{"mode":"explicit","ttl":"30m"}
        }),
    )
    .unwrap();
    let mut request = draft();
    request.native_features = vec![NativeFeature::PromptCache];
    request.provider_options = vec![option.clone()];
    let mut supported = model();
    supported.capabilities.prompt_cache = CapabilitySupport::Supported;
    let call = adapter.resolve(request, &supported).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok));
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ResponseMetadata(metadata))
            if metadata.cache_usage().is_some_and(|usage|
                usage.cache_read_tokens() == 4 && usage.cache_write_tokens() == 2)
    )));
    let captured = transport.captured.lock().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.as_ref().unwrap().body).unwrap();
    assert_eq!(body["prompt_cache_key"], "session-cache-01");
    assert_eq!(
        body["prompt_cache_options"],
        serde_json::json!({"mode":"explicit","ttl":"30m"})
    );

    for malformed in [
        serde_json::json!({"prompt_cache_key":"session-cache-01"}),
        serde_json::json!({
            "prompt_cache_key":"session-cache-01",
            "prompt_cache_options":{"mode":"explicit","ttl":"30m"},
            "ignored":"must fail"
        }),
    ] {
        let mut request = draft();
        request.native_features = vec![NativeFeature::PromptCache];
        request.provider_options =
            vec![ProviderRequestOption::new("openai", "prompt-cache", malformed).unwrap()];
        assert!(adapter.resolve(request, &supported).is_err());
    }
}

#[test]
fn response_config_uses_responses_protocol_and_secret_free_auth_marker() {
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "secret");
    let spec: ResolveSpec = config.resolve_spec();
    assert_eq!(spec.protocol, ProviderProtocol::OpenAiResponses);
    assert_eq!(
        spec.authentication,
        AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new())
    );
    assert!(!format!("{config:?}").contains("secret"));

    for invalid in [
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "key")
            .with_provider_request_option_members(
                "bad",
                vec![("member".to_owned(), "model".to_owned())],
            ),
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "key")
            .with_provider_request_option_members(
                "bad",
                vec![
                    ("one".to_owned(), "same".to_owned()),
                    ("two".to_owned(), "same".to_owned()),
                ],
            ),
    ] {
        assert!(
            OpenAiResponsesAdapter::new(
                invalid,
                heycode_http::HttpService::new(Arc::new(ScriptedTransport::new(Vec::new()))),
            )
            .is_err()
        );
    }
}

// ---------------------------------------------------------------------------
// POA02 — stateless tool-loop item preservation.
//
// The Responses API states the requirement itself. On the `include` parameter:
// "`reasoning.encrypted_content`: Includes an encrypted version of reasoning
// tokens in reasoning item outputs. This enables reasoning items to be used in
// multi-turn conversations when using the Responses API statelessly (like when
// the `store` parameter is set to `false` …)". On `MessagePhase`: "For models
// like `gpt-5.3-codex` and beyond, when sending follow-up requests, preserve
// and resend phase on all assistant messages — dropping it can degrade
// performance." A degraded request that still succeeds is the failure this
// section exists to prevent, so every miss is a pre-dispatch error.
// ---------------------------------------------------------------------------

fn state(data: serde_json::Value) -> InferenceInput {
    InferenceInput::ProviderState(
        ProviderStateItem::new(
            "openai",
            "gpt-test",
            ProviderProtocol::OpenAiResponses,
            ProviderStateKind::ResponseOutputItem,
            data,
        )
        .unwrap(),
    )
}

fn reasoning_item() -> serde_json::Value {
    serde_json::json!({
        "type": "reasoning",
        "id": "rs_1",
        "encrypted_content": "gAAAAABpM0Yj-encrypted",
        "summary": []
    })
}

fn function_call_item() -> serde_json::Value {
    serde_json::json!({
        "type": "function_call",
        "id": "fc_1",
        "call_id": "call_1",
        "name": "lookup",
        "arguments": "{\"q\":\"x\"}",
        "status": "completed"
    })
}

fn assistant_message_item(phase: Option<&str>) -> serde_json::Value {
    let mut item = serde_json::json!({
        "type": "message",
        "id": "msg_1",
        "role": "assistant",
        "status": "completed",
        "content": [{"type": "output_text", "text": "done"}]
    });
    if let Some(phase) = phase {
        item["phase"] = serde_json::json!(phase);
    }
    item
}

fn tool_result() -> InferenceInput {
    InferenceInput::Message(ChatMessage::tool("call_1", "{\"ok\":true}"))
}

fn continuation_adapter(requirement: ResponsesContinuation) -> OpenAiResponsesAdapter {
    let transport = Arc::new(ScriptedTransport::new(vec![terminal_event(0)]));
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "test-key")
            .with_retry_spec(heycode_llm::RetrySpec::no_retry())
            .with_continuation(requirement);
    OpenAiResponsesAdapter::new(config, heycode_http::HttpService::new(transport)).unwrap()
}

/// The positive acceptance: a stateless tool loop replays the required items,
/// and they reach the wire byte-for-byte as the API returned them.
#[tokio::test]
async fn a_stateless_tool_loop_replays_required_items_unchanged() {
    let transport = Arc::new(ScriptedTransport::new(vec![terminal_event(0)]));
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "test-key")
            .with_retry_spec(heycode_llm::RetrySpec::no_retry())
            .with_continuation(ResponsesContinuation::ReasoningAndPhase);
    let adapter =
        OpenAiResponsesAdapter::new(config, heycode_http::HttpService::new(transport.clone()))
            .unwrap();

    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("look it up")),
        state(reasoning_item()),
        state(function_call_item()),
        tool_result(),
        state(reasoning_item()),
        state(assistant_message_item(Some("final_answer"))),
    ];
    let call = adapter.resolve(request, &model()).unwrap();
    let mut stream = adapter.stream(call);
    while stream.next().await.is_some() {}

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    let input = body["input"].as_array().unwrap();
    // Stateless replay requires `store:false` plus the encrypted reasoning
    // include; both are what make the replayed items usable at all.
    assert_eq!(body["store"], serde_json::json!(false));
    assert_eq!(
        body["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
    // Every retained item is replayed at its exact position, unchanged.
    assert_eq!(input[1], reasoning_item());
    assert_eq!(input[2], function_call_item());
    assert_eq!(input[4], reasoning_item());
    assert_eq!(input[5], assistant_message_item(Some("final_answer")));
    // `phase`, `encrypted_content` and `call_id` specifically survive.
    assert_eq!(input[5]["phase"], serde_json::json!("final_answer"));
    assert_eq!(
        input[1]["encrypted_content"],
        serde_json::json!("gAAAAABpM0Yj-encrypted")
    );
    assert_eq!(input[2]["call_id"], serde_json::json!("call_1"));
}

/// The neutral assistant path cannot carry a reasoning item or a phase, so a
/// route that requires them must refuse it rather than silently degrade.
#[test]
fn a_neutral_assistant_tool_call_turn_fails_before_dispatch() {
    let neutral = || {
        let mut request = draft();
        request.inputs = vec![
            InferenceInput::Message(ChatMessage::user("look it up")),
            InferenceInput::Message(ChatMessage {
                role: heycode_llm::Role::Assistant,
                content: String::new(),
                images: Vec::new(),
                documents: Vec::new(),
                tool_calls: Some(vec![ChatToolCall {
                    id: "call_1".to_owned(),
                    name: "lookup".to_owned(),
                    arguments: "{}".to_owned(),
                }]),
                tool_call_id: None,
                tool_result_is_error: None,
            }),
            tool_result(),
        ];
        request
    };
    for requirement in [
        ResponsesContinuation::ReasoningItems,
        ResponsesContinuation::ReasoningAndPhase,
    ] {
        let error = continuation_adapter(requirement)
            .resolve(neutral(), &model())
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("provider_state"),
            "{requirement:?} must refuse the neutral assistant fallback"
        );
    }
    // A route with no requirement keeps the compatibility path.
    assert!(
        continuation_adapter(ResponsesContinuation::None)
            .resolve(neutral(), &model())
            .is_ok()
    );
}

/// A replayed tool call whose turn lost its reasoning item is the exact
/// regression the encrypted-content include exists to prevent.
#[test]
fn a_replayed_tool_call_without_its_reasoning_item_fails_before_dispatch() {
    let cases: Vec<(&str, Vec<InferenceInput>)> = vec![
        (
            "no reasoning item at all",
            vec![
                InferenceInput::Message(ChatMessage::user("go")),
                state(function_call_item()),
            ],
        ),
        (
            "reasoning item stripped of its encrypted content",
            vec![
                InferenceInput::Message(ChatMessage::user("go")),
                state(serde_json::json!({"type":"reasoning","id":"rs_1","summary":[]})),
                state(function_call_item()),
            ],
        ),
        (
            "reasoning item with empty encrypted content",
            vec![
                InferenceInput::Message(ChatMessage::user("go")),
                state(serde_json::json!({"type":"reasoning","id":"rs_1","encrypted_content":""})),
                state(function_call_item()),
            ],
        ),
        (
            "a later turn reuses an earlier turn's reasoning",
            vec![
                InferenceInput::Message(ChatMessage::user("go")),
                state(reasoning_item()),
                state(function_call_item()),
                tool_result(),
                state(function_call_item()),
            ],
        ),
    ];
    for (label, inputs) in cases {
        let mut request = draft();
        request.inputs = inputs;
        let error = continuation_adapter(ResponsesContinuation::ReasoningItems)
            .resolve(request, &model())
            .expect_err(label);
        assert!(format!("{error:?}").contains("provider_state"), "{label}");
    }
}

/// `phase` is a separate requirement: OpenAI names it only for newer models,
/// so a route that does not require it must not reject a phase-free replay.
#[test]
fn a_replayed_assistant_message_without_phase_fails_only_under_the_phase_requirement() {
    let phaseless = || {
        let mut request = draft();
        request.inputs = vec![
            InferenceInput::Message(ChatMessage::user("go")),
            state(reasoning_item()),
            state(assistant_message_item(None)),
        ];
        request
    };
    let error = continuation_adapter(ResponsesContinuation::ReasoningAndPhase)
        .resolve(phaseless(), &model())
        .unwrap_err();
    assert!(format!("{error:?}").contains("provider_state"));

    assert!(
        continuation_adapter(ResponsesContinuation::ReasoningItems)
            .resolve(phaseless(), &model())
            .is_ok()
    );
    // Either documented phase value satisfies the requirement.
    for phase in ["commentary", "final_answer"] {
        let mut request = draft();
        request.inputs = vec![
            InferenceInput::Message(ChatMessage::user("go")),
            state(reasoning_item()),
            state(assistant_message_item(Some(phase))),
        ];
        assert!(
            continuation_adapter(ResponsesContinuation::ReasoningAndPhase)
                .resolve(request, &model())
                .is_ok(),
            "`{phase}` must be accepted"
        );
    }
}

fn terminal_event_with_usage(
    usage: serde_json::Value,
) -> Result<SseEvent, heycode_http::TransportError> {
    event(
        "response.completed",
        serde_json::json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "status": "completed",
                "output": [],
                "usage": usage
            }
        }),
    )
}

fn prompt_cache_adapter(
    events: Vec<Result<SseEvent, heycode_http::TransportError>>,
) -> (OpenAiResponsesAdapter, RequestDraft, ModelDescriptor) {
    let transport = Arc::new(ScriptedTransport::new(events));
    let config =
        OpenAiResponsesConfig::with_key(provider(), "https://api.openai.test/v1", "test-key")
            .with_provider_request_option_members(
                "prompt-cache",
                vec![("prompt_cache_key".to_owned(), "prompt_cache_key".to_owned())],
            );
    let adapter = OpenAiResponsesAdapter::new(config, heycode_http::HttpService::new(transport))
        .expect("adapter");
    let mut request = draft();
    request.native_features = vec![NativeFeature::PromptCache];
    request.provider_options = vec![
        ProviderRequestOption::new(
            "openai",
            "prompt-cache",
            serde_json::json!({"prompt_cache_key":"session-cache-01"}),
        )
        .expect("option"),
    ];
    let mut supported = model();
    supported.capabilities.prompt_cache = CapabilitySupport::Supported;
    (adapter, request, supported)
}

#[tokio::test]
async fn responses_usage_without_a_cache_write_counter_still_finishes_the_turn() {
    let (adapter, _) = make_adapter(vec![terminal_event_with_usage(serde_json::json!({
        "input_tokens": 3,
        "input_tokens_details": {"cached_tokens": 1},
        "output_tokens": 2,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": 5
    }))]);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ResponseMetadata(metadata))
            if metadata.cache_usage().is_some_and(|usage|
                usage.cache_read_tokens() == 1 && usage.cache_write_tokens() == 0)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::Usage(usage))
            if usage.prompt_tokens == 3 && usage.completion_tokens == 2
    )));
    assert!(matches!(
        events.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
    ));
}

#[tokio::test]
async fn responses_usage_totals_without_details_emit_no_response_metadata() {
    let (adapter, _) = make_adapter(vec![terminal_event_with_usage(serde_json::json!({
        "input_tokens": 3,
        "output_tokens": 2,
        "total_tokens": 5
    }))]);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::ResponseMetadata(_))))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::Usage(usage))
            if usage.prompt_tokens == 3 && usage.completion_tokens == 2
    )));
    assert!(matches!(
        events.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
    ));
}

#[tokio::test]
async fn responses_usage_enrichment_degrades_only_when_no_cache_was_negotiated() {
    let inconsistent = serde_json::json!({
        "input_tokens": 3,
        "input_tokens_details": {"cached_tokens": 1, "cache_write_tokens": 0},
        "output_tokens": 2,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": 11
    });

    let (adapter, _) = make_adapter(vec![terminal_event_with_usage(inconsistent.clone())]);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::ResponseMetadata(_))))
    );
    assert!(matches!(
        events.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
    ));

    for usage in [
        inconsistent,
        serde_json::json!({
            "input_tokens": 3,
            "input_tokens_details": {"cached_tokens": 1},
            "output_tokens": 2,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 5
        }),
    ] {
        let (adapter, request, supported) =
            prompt_cache_adapter(vec![terminal_event_with_usage(usage)]);
        let call = adapter.resolve(request, &supported).unwrap();
        let events = adapter.stream(call).collect::<Vec<_>>().await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Err(heycode_llm::LlmError::InvalidResponse(_)))),
            "a negotiated prompt-cache call must fail loud: {events:?}"
        );
    }
}

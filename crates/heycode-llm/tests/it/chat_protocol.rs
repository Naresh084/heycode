//! OpenAI Chat Completions normalized request, replay and stream contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::CallId;
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatMessage,
    ChatReasoningWire, ChatThinkingConfig, FinishReason, InferenceAdapter, InferenceEvent,
    InferenceInput, InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle,
    ModelPerformance, ModelPricing, NativeFeature, OpenAiChatCompletionsAdapter,
    OpenAiChatCompletionsConfig, ProviderDescriptor, ProviderProtocol, ProviderRequestOption,
    ProviderStateItem, ProviderStateKind, ReasoningEffortId, RequestDraft, ResolveSpec,
    StreamItemKind, ToolSpec,
};
use tokio_util::sync::CancellationToken;

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "router".to_owned(),
        display_name: "Router".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "router/model".to_owned(),
        display_name: "Model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(128_000),
        max_output_tokens: Some(8_192),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            image_input: CapabilitySupport::Unsupported,
            document_input: CapabilitySupport::Unsupported,
            structured_output: CapabilitySupport::Unsupported,
            native_web: CapabilitySupport::Unsupported,
            native_compaction: CapabilitySupport::Unsupported,
            prompt_cache: CapabilitySupport::Unsupported,
        },
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "router".to_owned(),
        model: "router/model".to_owned(),
        catalog_revision: Some(2),
        catalog_fetched_at_ms: Some(2_000),
        effective_at_ms: 3_000,
        system: Some("system".to_owned()),
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

#[derive(Clone)]
struct Captured {
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
) -> (OpenAiChatCompletionsAdapter, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::new(events));
    let config =
        OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "test-key")
            .with_reasoning(
                vec![
                    ReasoningEffortId::new("low").unwrap(),
                    ReasoningEffortId::new("high").unwrap(),
                ],
                Some(ReasoningEffortId::new("low").unwrap()),
                ChatReasoningWire::ObjectEffort,
            )
            .with_default_max_output_tokens(Some(4_096))
            .with_retry_spec(heycode_llm::RetrySpec::no_retry())
            .with_extra_headers(vec![("x-title".to_owned(), "heycode".to_owned())]);
    (
        OpenAiChatCompletionsAdapter::new(
            config,
            heycode_http::HttpService::new(transport.clone()),
        )
        .unwrap(),
        transport,
    )
}

#[tokio::test]
async fn request_preserves_ordered_assistant_state_and_serializes_tools_reasoning_and_controls() {
    let (adapter, transport) = make_adapter(vec![
        sse(serde_json::json!({
            "id":"chat_req","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}
        })),
        done(),
    ]);
    let state = ProviderStateItem::new(
        "router",
        "router/model",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant",
            "content":null,
            "reasoning_content":"prior reasoning",
            "tool_calls":[{
                "id":"prior_call","type":"function",
                "function":{"name":"read","arguments":"{\"path\":\"old\"}"}
            }]
        }),
    )
    .unwrap();
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("first")),
        InferenceInput::ProviderState(state),
        InferenceInput::Message(ChatMessage::tool("prior_call", "old contents")),
        InferenceInput::Message(ChatMessage::user("next")),
    ];
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    request.temperature = Some(0.4);
    request.max_output_tokens = Some(2_048);
    let call = adapter.resolve(request, &model()).unwrap();
    let _events = adapter.stream(call).collect::<Vec<_>>().await;

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "authorization" && value == "Bearer test-key")
    );
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "x-title" && value == "heycode")
    );
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["model"], "router/model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["max_tokens"], 2_048);
    assert!((body["temperature"].as_f64().unwrap() - 0.4).abs() < 0.000_001);
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["content"], "first");
    assert_eq!(messages[2]["reasoning_content"], "prior reasoning");
    assert_eq!(messages[3]["tool_call_id"], "prior_call");
    assert_eq!(messages[4]["content"], "next");
    assert!(
        !String::from_utf8(captured.body)
            .unwrap()
            .contains("test-key")
    );
}

#[tokio::test]
async fn stream_normalizes_reasoning_text_interleaved_multi_tools_state_usage_and_finish() {
    let events = vec![
        sse(serde_json::json!({
            "id":"chat_1","model":"router/model",
            "choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"think "},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_1","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_1","choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"call_0","type":"function","function":{"name":"read","arguments":"{\"p\""}},
                {"index":1,"id":"call_1","type":"function","function":{"name":"grep","arguments":"{\"q\""}}
            ]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_1","choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":":\"a\"}"}},
                {"index":1,"function":{"arguments":":\"b\"}"}}
            ]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
        })),
        sse(serde_json::json!({
            "id":"chat_1","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":6}
        })),
        done(),
    ];
    let (adapter, _) = make_adapter(events);
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;

    assert!(
        matches!(&events[0], Ok(InferenceEvent::ResponseStarted { response_id }) if response_id == "chat_1")
    );
    assert!(matches!(
        &events[1],
        Ok(InferenceEvent::ItemStarted {
            kind: StreamItemKind::Message,
            ..
        })
    ));
    assert!(matches!(&events[2], Ok(InferenceEvent::ReasoningDelta(text)) if text == "think "));
    assert!(matches!(&events[3], Ok(InferenceEvent::TextDelta(text)) if text == "hello"));
    assert!(matches!(&events[4], Ok(InferenceEvent::ToolCallDelta {
        output_index:0,id:Some(id),name:Some(name),arguments_delta
    }) if id == &CallId::from_raw("call_0") && name == "read" && arguments_delta == "{\"p\""));
    assert!(matches!(&events[5], Ok(InferenceEvent::ToolCallDelta {
        output_index:1,id:Some(id),name:Some(name),arguments_delta
    }) if id == &CallId::from_raw("call_1") && name == "grep" && arguments_delta == "{\"q\""));
    assert!(matches!(
        &events[6],
        Ok(InferenceEvent::ToolCallDelta {
            output_index: 0,
            id: None,
            name: None,
            ..
        })
    ));
    assert!(matches!(
        &events[7],
        Ok(InferenceEvent::ToolCallDelta {
            output_index: 1,
            id: None,
            name: None,
            ..
        })
    ));
    assert!(
        matches!(&events[9], Ok(InferenceEvent::ProviderState(state))
        if state.kind() == ProviderStateKind::ChatAssistantMessage
            && state.data()["reasoning_content"] == "think "
            && state.data()["content"] == "hello"
            && state.data()["tool_calls"][0]["function"]["arguments"] == "{\"p\":\"a\"}"
            && state.data()["tool_calls"][1]["function"]["arguments"] == "{\"q\":\"b\"}")
    );
    assert!(
        matches!(&events[10], Ok(InferenceEvent::ResponseFinished { status, .. }) if status == "completed")
    );
    assert!(matches!(&events[11], Ok(InferenceEvent::Usage(usage))
        if usage.prompt_tokens == 12 && usage.completion_tokens == 6));
    assert!(matches!(
        &events[12],
        Ok(InferenceEvent::Finish(FinishReason::ToolCalls))
    ));
    assert_eq!(events.len(), 13, "events: {events:#?}");
}

#[tokio::test]
async fn malformed_choice_tool_identity_late_delta_and_unfinished_eof_fail() {
    let scripts = vec![
        vec![
            sse(serde_json::json!({
                "id":"chat_bad","choices":[{"index":1,"delta":{"content":"wrong"},"finish_reason":"stop"}]
            })),
            done(),
        ],
        vec![
            sse(serde_json::json!({
                "id":"chat_bad","choices":[{"index":0,"delta":{"tool_calls":[
                    {"index":0,"id":"call_a","function":{"name":"read","arguments":"{"}}
                ]},"finish_reason":null}]
            })),
            sse(serde_json::json!({
                "id":"chat_bad","choices":[{"index":0,"delta":{"tool_calls":[
                    {"index":0,"id":"call_b","function":{"arguments":"}"}}
                ]},"finish_reason":null}]
            })),
        ],
        vec![
            sse(serde_json::json!({
                "id":"chat_bad","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
            })),
            sse(serde_json::json!({
                "id":"chat_bad","choices":[{"index":0,"delta":{"content":"late"},"finish_reason":null}]
            })),
        ],
        vec![sse(serde_json::json!({
            "id":"chat_bad","choices":[{"index":0,"delta":{"content":"open"},"finish_reason":null}]
        }))],
    ];
    for script in scripts {
        let (adapter, _) = make_adapter(script);
        let call = adapter.resolve(draft(), &model()).unwrap();
        let events = adapter.stream(call).collect::<Vec<_>>().await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Err(heycode_llm::LlmError::InvalidResponse(_))))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
        );
    }
}

#[test]
fn adapter_rejects_unrepresented_modalities_structured_output_and_native_features() {
    let (adapter, _) = make_adapter(Vec::new());
    let mut image = draft();
    image.input_modalities.push(InputModality::Image);
    let mut image_model = model();
    image_model.capabilities.image_input = CapabilitySupport::Supported;
    assert!(adapter.resolve(image, &image_model).is_err());

    let mut structured = draft();
    structured.structured_output = Some(serde_json::json!({"type":"object"}));
    let mut structured_model = model();
    structured_model.capabilities.structured_output = CapabilitySupport::Supported;
    assert!(adapter.resolve(structured, &structured_model).is_err());

    for feature in [
        NativeFeature::Web,
        NativeFeature::Compaction,
        NativeFeature::PromptCache,
    ] {
        let mut request = draft();
        request.native_features.push(feature);
        let mut supported = model();
        match feature {
            NativeFeature::Web => supported.capabilities.native_web = CapabilitySupport::Supported,
            NativeFeature::Compaction => {
                supported.capabilities.native_compaction = CapabilitySupport::Supported;
            }
            NativeFeature::PromptCache => {
                supported.capabilities.prompt_cache = CapabilitySupport::Supported;
            }
        }
        assert!(adapter.resolve(request, &supported).is_err());
    }
}

#[test]
fn chat_config_is_redacted_and_exposes_exact_protocol_spec() {
    let config =
        OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "secret");
    let spec: ResolveSpec = config.resolve_spec();
    assert_eq!(spec.protocol, ProviderProtocol::OpenAiChatCompletions);
    assert_eq!(
        spec.authentication,
        AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new())
    );
    assert!(!format!("{config:?}").contains("secret"));
}

#[tokio::test]
async fn unauthenticated_chat_route_sends_no_authorization_header() {
    let transport = Arc::new(ScriptedTransport::new(vec![
        sse(serde_json::json!({
            "id":"chat_local","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
        })),
        done(),
    ]));
    let config =
        OpenAiChatCompletionsConfig::without_authentication(provider(), "http://localhost:8000/v1")
            .with_retry_spec(heycode_llm::RetrySpec::no_retry());
    assert_eq!(
        config.resolve_spec().authentication,
        AuthenticationBinding::None
    );
    let adapter = OpenAiChatCompletionsAdapter::new(
        config,
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();

    let call = adapter.resolve(draft(), &model()).unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:#?}");
    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert!(
        captured
            .headers
            .iter()
            .all(|(name, _)| name != "authorization")
    );
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "content-type" && value == "application/json")
    );
}

#[tokio::test]
async fn multiple_provider_options_map_whole_objects_and_exact_members_to_the_wire() {
    let transport = Arc::new(ScriptedTransport::new(vec![
        sse(serde_json::json!({
            "id":"chat_options","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
        })),
        done(),
    ]));
    let config =
        OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "secret")
            .with_provider_request_option("routing", "provider")
            .with_provider_request_option_member("transforms", "plugins", "plugins");
    let adapter = OpenAiChatCompletionsAdapter::new(
        config,
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let mut request = draft();
    request.provider_options = vec![
        ProviderRequestOption::new(
            "router",
            "routing",
            serde_json::json!({"allow_fallbacks":false}),
        )
        .unwrap(),
        ProviderRequestOption::new(
            "router",
            "transforms",
            serde_json::json!({
                "plugins":[{"id":"context-compression","enabled":false}]
            }),
        )
        .unwrap(),
    ];

    let call = adapter.resolve(request, &model()).unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:#?}");
    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(
        body["provider"],
        serde_json::json!({"allow_fallbacks":false})
    );
    assert_eq!(
        body["plugins"],
        serde_json::json!([{"id":"context-compression","enabled":false}])
    );
}

#[test]
fn member_projection_refuses_missing_or_silently_dropped_option_data() {
    let config =
        OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "secret")
            .with_provider_request_option_member("transforms", "plugins", "plugins");
    let adapter = OpenAiChatCompletionsAdapter::new(
        config,
        heycode_http::HttpService::new(Arc::new(ScriptedTransport::new(Vec::new()))),
    )
    .unwrap();
    for data in [
        serde_json::json!({"other":[]}),
        serde_json::json!({"plugins":[],"unprojected":true}),
    ] {
        let mut request = draft();
        request.provider_options =
            vec![ProviderRequestOption::new("router", "transforms", data).unwrap()];
        assert!(
            adapter.resolve(request, &model()).is_err(),
            "member projection must not invent or drop option data"
        );
    }
}

#[test]
fn chat_thinking_config_rejects_incomplete_effort_maps() {
    let none = ReasoningEffortId::new("none").unwrap();
    let high = ReasoningEffortId::new("high").unwrap();
    let config =
        OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "secret")
            .with_reasoning(
                vec![none.clone(), high.clone()],
                Some(high),
                ChatReasoningWire::ScalarEffort,
            )
            .with_thinking(ChatThinkingConfig::object_type(none, Vec::new()));
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));
    let error = match OpenAiChatCompletionsAdapter::new(
        config,
        heycode_http::HttpService::new(transport),
    ) {
        Ok(_) => panic!("incomplete thinking map must fail adapter construction"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("does not cover"), "{error}");
}

#[tokio::test]
async fn configured_server_web_tool_reaches_wire_and_url_citations_normalize_losslessly() {
    let transport = Arc::new(ScriptedTransport::new(vec![
        sse(serde_json::json!({
            "id":"chat_web","choices":[{"index":0,"delta":{
                "content":"Rust source",
                "annotations":[{
                    "type":"url_citation",
                    "url_citation":{
                        "url":"https://example.test/rust",
                        "title":"Rust",
                        "content":"bounded excerpt",
                        "start_index":0,
                        "end_index":11
                    }
                }]
            },"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_web","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
            "usage":{
                "prompt_tokens":10,"completion_tokens":4,
                "server_tool_use":{"web_search_requests":1}
            }
        })),
        done(),
    ]));
    let config =
        OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "test-key")
            .with_server_tool(
                NativeFeature::Web,
                serde_json::json!({
                    "type":"openrouter:web_search",
                    "parameters":{
                        "engine":"auto",
                        "max_results":5,
                        "max_uses":3,
                        "max_total_results":15
                    }
                }),
            )
            .with_max_server_tool_calls(5)
            .with_url_citations()
            .with_retry_spec(heycode_llm::RetrySpec::standard());
    let adapter = OpenAiChatCompletionsAdapter::new(
        config,
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let mut request = draft();
    request.native_features.push(NativeFeature::Web);
    request.tools.push(ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    });
    let mut supported = model();
    supported.capabilities.native_web = CapabilitySupport::Supported;
    let call = adapter.resolve(request, &supported).unwrap();
    assert_eq!(call.retry_spec().safety(), heycode_llm::RetrySafety::Never);
    let events = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::Citation { output_index:0, citation })
            if citation.url() == "https://example.test/rust"
                && citation.title() == Some("Rust")
                && citation.cited_text() == Some("bounded excerpt")
                && citation.start_index() == Some(0)
                && citation.end_index() == Some(11)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(state))
            if state.data()["annotations"][0]["type"] == "url_citation"
    )));

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert_eq!(body["tools"][1]["type"], "openrouter:web_search");
    assert_eq!(body["tools"][1]["parameters"]["engine"], "auto");
    assert_eq!(body["max_tool_calls"], 5);
}

#[tokio::test]
async fn usage_only_frame_may_repeat_the_same_finish_and_accepts_both_web_usage_aliases() {
    for server_usage in [
        serde_json::json!({"server_tool_use":{"web_search_requests":1}}),
        serde_json::json!({"server_tool_use_details":{"web_search_requests":1}}),
        serde_json::json!({
            "server_tool_use":{"web_search_requests":1},
            "server_tool_use_details":{"web_search_requests":1}
        }),
        serde_json::json!({
            "server_tool_use":{"web_search_requests":1},
            "server_tool_use_details":{"tool_calls_requested":1,"tool_calls_executed":1}
        }),
    ] {
        let mut usage = serde_json::json!({"prompt_tokens":4,"completion_tokens":2});
        for (key, value) in server_usage.as_object().unwrap() {
            usage[key] = value.clone();
        }
        let transport = Arc::new(ScriptedTransport::new(vec![
            sse(serde_json::json!({
                "id":"chat_usage_repeat",
                "choices":[{"index":0,"delta":{"content":"done"},"finish_reason":null}]
            })),
            sse(serde_json::json!({
                "id":"chat_usage_repeat",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
            })),
            sse(serde_json::json!({
                "id":"chat_usage_repeat",
                "choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}],
                "usage":usage
            })),
            done(),
        ]));
        let config =
            OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "test-key")
                .with_server_tool(
                    NativeFeature::Web,
                    serde_json::json!({"type":"openrouter:web_search"}),
                )
                .with_max_server_tool_calls(5)
                .with_url_citations();
        let adapter =
            OpenAiChatCompletionsAdapter::new(config, heycode_http::HttpService::new(transport))
                .unwrap();
        let mut request = draft();
        request.native_features.push(NativeFeature::Web);
        let mut supported = model();
        supported.capabilities.native_web = CapabilitySupport::Supported;
        let call = adapter.resolve(request, &supported).unwrap();
        let output = adapter.stream(call).collect::<Vec<_>>().await;
        assert!(output.iter().all(Result::is_ok), "{output:#?}");
        assert!(matches!(output.as_slice(), [..,
            Ok(InferenceEvent::ServerToolUsage(usage)),
            Ok(InferenceEvent::Usage(_)),
            Ok(InferenceEvent::Finish(FinishReason::Stop))
        ] if usage.requests() == 1));
    }
}

#[tokio::test]
async fn conflicting_web_usage_aliases_and_non_usage_finish_repeats_fail_closed() {
    let scripts = [
        vec![
            sse(serde_json::json!({
                "id":"chat_conflict","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
            })),
            sse(serde_json::json!({
                "id":"chat_conflict","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,
                    "server_tool_use":{"web_search_requests":1},
                    "server_tool_use_details":{"web_search_requests":2}}
            })),
        ],
        vec![
            sse(serde_json::json!({
                "id":"chat_duplicate","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
            })),
            sse(serde_json::json!({
                "id":"chat_duplicate","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
            })),
        ],
    ];
    for script in scripts {
        let transport = Arc::new(ScriptedTransport::new(script));
        let config =
            OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "test-key")
                .with_server_tool(
                    NativeFeature::Web,
                    serde_json::json!({"type":"openrouter:web_search"}),
                )
                .with_max_server_tool_calls(5)
                .with_url_citations();
        let adapter =
            OpenAiChatCompletionsAdapter::new(config, heycode_http::HttpService::new(transport))
                .unwrap();
        let mut request = draft();
        request.native_features.push(NativeFeature::Web);
        let mut supported = model();
        supported.capabilities.native_web = CapabilitySupport::Supported;
        let call = adapter.resolve(request, &supported).unwrap();
        let output = adapter.stream(call).collect::<Vec<_>>().await;
        assert!(output.iter().any(Result::is_err), "{output:#?}");
        assert!(!output.iter().any(|event| matches!(
            event,
            Ok(InferenceEvent::ProviderState(_) | InferenceEvent::Finish(_))
        )));
    }
}

#[tokio::test]
async fn unconfigured_or_unsafe_chat_citations_fail_before_successful_settlement() {
    let events = vec![
        sse(serde_json::json!({
            "id":"chat_bad_citation","choices":[{"index":0,"delta":{
                "annotations":[{
                    "type":"url_citation",
                    "url_citation":{"url":"javascript:alert(1)","title":"unsafe"}
                }]
            },"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_bad_citation","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
        })),
        done(),
    ];
    for configured in [false, true] {
        let transport = Arc::new(ScriptedTransport::new(events.clone()));
        let mut config =
            OpenAiChatCompletionsConfig::with_key(provider(), "https://router.test/v1", "test-key");
        if configured {
            config = config
                .with_server_tool(
                    NativeFeature::Web,
                    serde_json::json!({"type":"openrouter:web_search"}),
                )
                .with_max_server_tool_calls(5)
                .with_url_citations();
        }
        let adapter =
            OpenAiChatCompletionsAdapter::new(config, heycode_http::HttpService::new(transport))
                .unwrap();
        let mut request = draft();
        if configured {
            request.native_features.push(NativeFeature::Web);
        }
        let mut supported = model();
        supported.capabilities.native_web = CapabilitySupport::Supported;
        let call = adapter.resolve(request, &supported).unwrap();
        let output = adapter.stream(call).collect::<Vec<_>>().await;
        assert!(output.iter().any(Result::is_err), "{output:#?}");
        assert!(!output.iter().any(|event| matches!(
            event,
            Ok(InferenceEvent::ProviderState(_) | InferenceEvent::Finish(_))
        )));
    }
}

#[tokio::test]
async fn chat_cache_metadata_preserves_unknown_writes_and_rejects_impossible_reads() {
    for (read, write, valid) in [(80, Some(10), true), (80, None, true), (101, None, false)] {
        let mut usage = serde_json::json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":read},"completion_tokens_details":{"reasoning_tokens":5}});
        if let Some(write) = write {
            usage["prompt_tokens_details"]["cache_write_tokens"] = write.into();
        }
        let (adapter, _) = make_adapter(vec![
            sse(
                serde_json::json!({"id":"cache_1","choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]}),
            ),
            sse(serde_json::json!({"id":"cache_1","choices":[],"usage":usage})),
            done(),
        ]);
        let call = adapter.resolve(draft(), &model()).unwrap();
        let events = adapter.stream(call).collect::<Vec<_>>().await;
        if valid {
            assert!(events.iter().all(Result::is_ok));
            let metadata = events
                .iter()
                .find_map(|event| match event {
                    Ok(InferenceEvent::ResponseMetadata(value)) => Some(value),
                    _ => None,
                })
                .unwrap();
            let cache = metadata.cache_usage().unwrap();
            assert_eq!(cache.cache_read_tokens(), 80);
            assert_eq!(cache.reported_cache_write_tokens(), write);
            assert_eq!(cache.reasoning_tokens(), Some(5));
            let wire = serde_json::to_value(metadata).unwrap();
            let restored: heycode_core::ProviderResponseMetadata =
                serde_json::from_value(wire).unwrap();
            restored.validate().unwrap();
            assert_eq!(&restored, metadata);
        } else {
            assert!(events.iter().any(Result::is_err));
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
            );
        }
    }
}

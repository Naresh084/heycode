//! PZA03 multi-step tool fixtures: GLM thinking + function-call continuation.
//!
//! The failure these fixtures exist to catch is a replay that round-trips the
//! function calls and silently drops the thinking channel. Every replay
//! assertion therefore compares the whole assistant message for equality with
//! the value the response produced, not a field at a time: a distilled,
//! reordered or re-encoded `reasoning_content` fails the same way a missing one
//! does, which is what Z.ai's "complete, unmodified and correctly ordered"
//! requirement actually says.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, ChatToolCall, FinishReason, InferenceAdapter,
    InferenceEvent, InferenceInput, InputModality, LlmError, ModelDescriptor, Provider,
    ProviderProtocol, ProviderStateItem, ProviderStateKind, ReasoningEffortId, RequestDraft,
    ResolveError, ToolSpec,
};
use heycode_provider_zai::{
    Coding, General, ZAI_ALWAYS_THINKING_MODELS, ZAI_CODING_CHAT_BASE_URL,
    ZAI_DEFAULT_REASONING_EFFORT, ZAI_GENERAL_BASE_URL, ZAI_GLM_5_3, ZAI_REASONING_EFFORT_MODELS,
    ZAI_REASONING_EFFORTS, ZaiAuthHeaderEvidence, ZaiInference, ZaiPlan, ZaiPreservedThinking,
    zai_thinking_reports,
};

/// Z.ai documents GLM-4.6 as deciding for itself whether to think, so a
/// thinking-free tool turn from it is a valid turn and not a lost one.
const OPTIONAL_THINKING_MODEL: &str = "glm-4.6";
/// Z.ai says this model thinks compulsorily, but does not document
/// `reasoning_effort` for it.
const ALWAYS_THINKING_WITHOUT_EFFORT: &str = "glm-4.7";
use futures::StreamExt as _;

const GENERAL_PROVIDER: &str = "zai";
const CODING_PROVIDER: &str = "zai-coding";

/// One request the route actually built, kept as non-secret text.
struct Captured {
    url: String,
    authorization: Option<String>,
    body: serde_json::Value,
}

/// Records every request and answers each with the next scripted SSE script.
struct CaptureTransport {
    captured: Mutex<Vec<Captured>>,
    scripts: Mutex<VecDeque<Vec<Result<SseEvent, heycode_http::TransportError>>>>,
}

impl CaptureTransport {
    fn empty() -> Self {
        Self {
            captured: Mutex::new(Vec::new()),
            scripts: Mutex::new(VecDeque::new()),
        }
    }

    fn scripted(scripts: Vec<Vec<Result<SseEvent, heycode_http::TransportError>>>) -> Self {
        Self {
            captured: Mutex::new(Vec::new()),
            scripts: Mutex::new(scripts.into()),
        }
    }

    fn body(&self, index: usize) -> serde_json::Value {
        self.captured.lock().unwrap()[index].body.clone()
    }

    fn request_count(&self) -> usize {
        self.captured.lock().unwrap().len()
    }
}

impl HttpTransport for CaptureTransport {
    fn sse(
        &self,
        request: HttpSseRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> SseEventStream {
        self.captured.lock().unwrap().push(Captured {
            url: request.url().to_owned(),
            authorization: request
                .headers()
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("authorization"))
                .map(|header| header.value().to_owned()),
            body: serde_json::from_slice(request.body().unwrap_or_default()).unwrap(),
        });
        Box::pin(futures::stream::iter(
            self.scripts.lock().unwrap().pop_front().unwrap_or_default(),
        ))
    }
}

fn general(transport: CaptureTransport) -> (ZaiInference<General>, Arc<CaptureTransport>) {
    let transport = Arc::new(transport);
    let route = ZaiInference::<General>::new(HttpService::new(transport.clone()), "test-key", None)
        .unwrap();
    (route, transport)
}

fn coding(transport: CaptureTransport) -> (ZaiInference<Coding>, Arc<CaptureTransport>) {
    let transport = Arc::new(transport);
    let route =
        ZaiInference::<Coding>::new(HttpService::new(transport.clone()), "test-key", None).unwrap();
    (route, transport)
}

fn model(route: &ZaiInference<General>) -> ModelDescriptor {
    route.describe_model(ZAI_GLM_5_3)
}

fn read_tool() -> ToolSpec {
    ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    }
}

fn draft(provider: &str, inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        provider: provider.to_owned(),
        model: ZAI_GLM_5_3.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: None,
        inputs,
        tools: vec![read_tool()],
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: Some(0.7),
        max_output_tokens: Some(4_096),
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

/// One GLM turn that thinks, then calls `read`, streamed the way Z.ai streams
/// it: `delta.reasoning_content` fragments first, then `delta.tool_calls`
/// fragments whose `function.arguments` arrive in pieces.
fn thinking_tool_turn(
    response_id: &str,
    reasoning: [&str; 2],
    call_id: &str,
    arguments: [&str; 2],
) -> Vec<Result<SseEvent, heycode_http::TransportError>> {
    vec![
        sse(serde_json::json!({
            "id": response_id,
            "choices":[{"index":0,"delta":{"reasoning_content":reasoning[0]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id": response_id,
            "choices":[{"index":0,"delta":{"reasoning_content":reasoning[1]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id": response_id,
            "choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":call_id,"type":"function",
                "function":{"name":"read","arguments":arguments[0]}
            }]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id": response_id,
            "choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"function":{"arguments":arguments[1]}
            }]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id": response_id,
            "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
        })),
        done(),
    ]
}

async fn assistant_state(
    route: &ZaiInference<General>,
    inputs: Vec<InferenceInput>,
) -> ProviderStateItem {
    let call = route
        .resolve(draft(GENERAL_PROVIDER, inputs), &model(route))
        .unwrap();
    let events = InferenceAdapter::stream(route, call)
        .collect::<Vec<_>>()
        .await;
    events
        .into_iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state),
            _ => None,
        })
        .expect("a completed GLM turn publishes replayable assistant state")
}

#[tokio::test]
async fn a_thinking_tool_turn_publishes_its_reasoning_and_calls_as_one_chat_assistant_state() {
    let (route, _transport) = general(CaptureTransport::scripted(vec![thinking_tool_turn(
        "chat_step_1",
        ["I should read ", "the file."],
        "call_1",
        ["{\"path\":", "\"a\"}"],
    )]));
    let state = assistant_state(
        &route,
        vec![InferenceInput::Message(ChatMessage::user("read a"))],
    )
    .await;

    assert_eq!(state.provider(), GENERAL_PROVIDER);
    assert_eq!(state.model(), ZAI_GLM_5_3);
    assert_eq!(state.protocol(), ProviderProtocol::OpenAiChatCompletions);
    assert_eq!(state.kind(), ProviderStateKind::ChatAssistantMessage);
    assert_eq!(
        *state.data(),
        serde_json::json!({
            "role":"assistant",
            "content":null,
            "reasoning_content":"I should read the file.",
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{\"path\":\"a\"}"}
            }]
        })
    );
}

#[tokio::test]
async fn the_next_step_replays_that_assistant_message_verbatim_before_the_tool_result() {
    let (route, transport) = general(CaptureTransport::scripted(vec![thinking_tool_turn(
        "chat_step_1",
        ["I should read ", "the file."],
        "call_1",
        ["{\"path\":", "\"a\"}"],
    )]));
    let state = assistant_state(
        &route,
        vec![InferenceInput::Message(ChatMessage::user("read a"))],
    )
    .await;

    let call = route
        .resolve(
            draft(
                GENERAL_PROVIDER,
                vec![
                    InferenceInput::Message(ChatMessage::user("read a")),
                    InferenceInput::ProviderState(state.clone()),
                    InferenceInput::Message(ChatMessage::tool("call_1", "file contents")),
                ],
            ),
            &model(&route),
        )
        .unwrap();
    drop(InferenceAdapter::stream(&route, call));

    let body = transport.body(1);
    assert_eq!(body["messages"][0]["role"], "user");
    // Whole-message equality: a replay that kept the tool calls and distilled,
    // truncated or re-encoded the thinking fails here.
    assert_eq!(body["messages"][1], *state.data());
    assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
    assert_eq!(body["messages"][2]["content"], "file contents");
}

#[tokio::test]
async fn a_two_round_tool_loop_replays_both_thinking_blocks_in_the_order_glm_produced_them() {
    let (route, transport) = general(CaptureTransport::scripted(vec![
        thinking_tool_turn(
            "chat_step_1",
            ["First I list ", "the directory."],
            "call_1",
            ["{\"path\":", "\".\"}"],
        ),
        thinking_tool_turn(
            "chat_step_2",
            ["Now I read ", "the file it named."],
            "call_2",
            ["{\"path\":", "\"a\"}"],
        ),
    ]));

    let first = assistant_state(
        &route,
        vec![InferenceInput::Message(ChatMessage::user("read a"))],
    )
    .await;
    let after_first = vec![
        InferenceInput::Message(ChatMessage::user("read a")),
        InferenceInput::ProviderState(first.clone()),
        InferenceInput::Message(ChatMessage::tool("call_1", "a")),
    ];
    let second = assistant_state(&route, after_first.clone()).await;

    let mut third = after_first;
    third.push(InferenceInput::ProviderState(second.clone()));
    third.push(InferenceInput::Message(ChatMessage::tool(
        "call_2",
        "file contents",
    )));
    let call = route
        .resolve(draft(GENERAL_PROVIDER, third), &model(&route))
        .unwrap();
    drop(InferenceAdapter::stream(&route, call));

    let body = transport.body(2);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[1], *first.data());
    assert_eq!(messages[3], *second.data());
    // Z.ai requires the consecutive reasoning blocks to match the sequence the
    // model generated, so the order of the two turns is part of the contract.
    assert_eq!(
        messages
            .iter()
            .filter_map(|message| message["reasoning_content"].as_str())
            .collect::<Vec<_>>(),
        [
            "First I list the directory.",
            "Now I read the file it named."
        ]
    );
}

#[test]
fn a_replayed_tool_call_turn_without_its_thinking_fails_before_transport() {
    let (route, transport) = general(CaptureTransport::empty());
    let stripped = ProviderStateItem::new(
        GENERAL_PROVIDER,
        ZAI_GLM_5_3,
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant","content":null,
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]
        }),
    )
    .unwrap();
    assert!(matches!(
        route.resolve(
            draft(
                GENERAL_PROVIDER,
                vec![InferenceInput::ProviderState(stripped)]
            ),
            &model(&route)
        ),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn an_always_thinking_older_model_rejects_stripped_state_without_sending_effort() {
    let (route, transport) = general(CaptureTransport::empty());
    let descriptor = route.describe_model(ALWAYS_THINKING_WITHOUT_EFFORT);
    let stripped = ProviderStateItem::new(
        GENERAL_PROVIDER,
        ALWAYS_THINKING_WITHOUT_EFFORT,
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant","content":null,
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]
        }),
    )
    .unwrap();
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::ProviderState(stripped)],
    );
    request.model = ALWAYS_THINKING_WITHOUT_EFFORT.to_owned();
    assert!(matches!(
        route.resolve(request, &descriptor),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn an_empty_thinking_string_is_not_accepted_as_replayed_thinking() {
    let (route, transport) = general(CaptureTransport::empty());
    let blank = ProviderStateItem::new(
        GENERAL_PROVIDER,
        ZAI_GLM_5_3,
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant","content":null,"reasoning_content":"",
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]
        }),
    )
    .unwrap();
    assert!(matches!(
        route.resolve(
            draft(GENERAL_PROVIDER, vec![InferenceInput::ProviderState(blank)]),
            &model(&route)
        ),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn a_neutral_assistant_tool_call_message_cannot_stand_in_for_provider_state() {
    let (route, transport) = general(CaptureTransport::empty());
    let distilled = ChatMessage::assistant_with_tool_calls(
        "",
        vec![ChatToolCall {
            id: "call_1".to_owned(),
            name: "read".to_owned(),
            arguments: "{}".to_owned(),
        }],
    );
    assert!(matches!(
        route.resolve(
            draft(GENERAL_PROVIDER, vec![InferenceInput::Message(distilled)]),
            &model(&route)
        ),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));
    assert_eq!(transport.request_count(), 0);
}

#[tokio::test]
async fn a_tool_call_response_that_omitted_its_thinking_yields_no_state_and_no_finish() {
    let (route, _transport) = general(CaptureTransport::scripted(vec![vec![
        sse(serde_json::json!({
            "id":"chat_no_thinking",
            "choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_no_thinking",
            "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
        })),
        done(),
    ]]));
    let call = route
        .resolve(
            draft(
                GENERAL_PROVIDER,
                vec![InferenceInput::Message(ChatMessage::user("read a"))],
            ),
            &model(&route),
        )
        .unwrap();
    let events = InferenceAdapter::stream(&route, call)
        .collect::<Vec<_>>()
        .await;

    assert!(events.iter().any(|event| matches!(
        event,
        Err(LlmError::InvalidResponse(message)) if message.contains("reasoning_content")
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(_)) | Ok(InferenceEvent::Finish(FinishReason::ToolCalls))
    )));
}

#[tokio::test]
async fn an_always_thinking_older_model_rejects_lossy_output_without_effort() {
    let (route, transport) = general(CaptureTransport::scripted(vec![vec![
        sse(serde_json::json!({
            "id":"chat_no_thinking",
            "choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_no_thinking",
            "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
        })),
        done(),
    ]]));
    let descriptor = route.describe_model(ALWAYS_THINKING_WITHOUT_EFFORT);
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("read a"))],
    );
    request.model = ALWAYS_THINKING_WITHOUT_EFFORT.to_owned();
    let call = route.resolve(request, &descriptor).unwrap();
    let events = InferenceAdapter::stream(&route, call)
        .collect::<Vec<_>>()
        .await;

    assert!(events.iter().any(|event| matches!(
        event,
        Err(LlmError::InvalidResponse(message)) if message.contains("reasoning_content")
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(_)) | Ok(InferenceEvent::Finish(FinishReason::ToolCalls))
    )));
    assert!(transport.body(0).get("reasoning_effort").is_none());
}

#[tokio::test]
async fn an_always_thinking_older_model_replays_valid_state_verbatim_without_effort() {
    let (route, transport) = general(CaptureTransport::scripted(vec![thinking_tool_turn(
        "chat_older_step_1",
        ["Inspect ", "the file."],
        "call_older_1",
        ["{\"path\":", "\"a\"}"],
    )]));
    let descriptor = route.describe_model(ALWAYS_THINKING_WITHOUT_EFFORT);
    let mut first_request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("read a"))],
    );
    first_request.model = ALWAYS_THINKING_WITHOUT_EFFORT.to_owned();
    let call = route.resolve(first_request, &descriptor).unwrap();
    let events = InferenceAdapter::stream(&route, call)
        .collect::<Vec<_>>()
        .await;
    let state = events
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state.clone()),
            _ => None,
        })
        .expect("valid older-model thinking state is retained");
    assert!(events.iter().all(Result::is_ok));

    let mut second_request = draft(
        GENERAL_PROVIDER,
        vec![
            InferenceInput::Message(ChatMessage::user("read a")),
            InferenceInput::ProviderState(state.clone()),
            InferenceInput::Message(ChatMessage::tool("call_older_1", "contents")),
        ],
    );
    second_request.model = ALWAYS_THINKING_WITHOUT_EFFORT.to_owned();
    let call = route.resolve(second_request, &descriptor).unwrap();
    drop(InferenceAdapter::stream(&route, call));

    assert!(transport.body(0).get("reasoning_effort").is_none());
    assert!(transport.body(1).get("reasoning_effort").is_none());
    assert_eq!(transport.body(1)["messages"][1], *state.data());
}

#[tokio::test]
async fn a_model_that_cannot_think_is_sent_no_effort_field_at_all() {
    // PZA02 records GLM-4-32B-0414-128K's reasoning as Unsupported because
    // Z.ai states `thinking` needs GLM-4.5 or higher. A model that cannot think
    // must not be asked how hard to think.
    let (route, transport) = general(CaptureTransport::empty());
    let non_thinking = route.describe_model("glm-4-32b-0414-128k");
    assert_eq!(
        non_thinking.capabilities.reasoning,
        CapabilitySupport::Unsupported
    );
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("read a"))],
    );
    request.model = "glm-4-32b-0414-128k".to_owned();
    let call = route.resolve(request, &non_thinking).unwrap();
    drop(InferenceAdapter::stream(&route, call));

    let body = transport.body(0);
    assert_eq!(body["model"], "glm-4-32b-0414-128k");
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("thinking").is_none());
}

#[test]
fn an_older_hybrid_model_accepts_thinking_free_state_and_gets_no_unpublished_effort() {
    let (route, transport) = general(CaptureTransport::empty());
    assert!(ZaiInference::<General>::requires_replayed_thinking(
        ZAI_GLM_5_3
    ));
    assert!(!ZaiInference::<General>::requires_replayed_thinking(
        OPTIONAL_THINKING_MODEL
    ));
    assert_eq!(
        ZAI_ALWAYS_THINKING_MODELS,
        ["glm-5.3", "glm-5.3-flash", "glm-4.7", "glm-4.5v"]
    );

    let optional = route.describe_model(OPTIONAL_THINKING_MODEL);
    assert_eq!(
        optional.capabilities.reasoning,
        CapabilitySupport::Supported
    );
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::ProviderState(
            ProviderStateItem::new(
                GENERAL_PROVIDER,
                OPTIONAL_THINKING_MODEL,
                ProviderProtocol::OpenAiChatCompletions,
                ProviderStateKind::ChatAssistantMessage,
                serde_json::json!({
                    "role":"assistant","content":null,
                    "tool_calls":[{
                        "id":"call_1","type":"function",
                        "function":{"name":"read","arguments":"{}"}
                    }]
                }),
            )
            .unwrap(),
        )],
    );
    request.model = OPTIONAL_THINKING_MODEL.to_owned();
    // `a_replayed_tool_call_turn_without_its_thinking_fails_before_transport`
    // rejects the identical state on GLM-5.3: the model id is the only
    // difference, and Z.ai's own `thinking.type` note is the reason.
    let call = route.resolve(request, &optional).unwrap();
    drop(InferenceAdapter::stream(&route, call));
    let body = transport.body(0);
    assert!(
        body.get("reasoning_effort").is_none(),
        "Z.ai documents reasoning_effort only for GLM-5.2 and above"
    );
}

#[tokio::test]
async fn a_thinking_free_tool_response_from_a_model_z_ai_lets_decide_still_publishes_state() {
    let thinking_free = vec![
        sse(serde_json::json!({
            "id":"chat_no_thinking",
            "choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]},"finish_reason":null}]
        })),
        sse(serde_json::json!({
            "id":"chat_no_thinking",
            "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
        })),
        done(),
    ];
    let (route, _transport) = general(CaptureTransport::scripted(vec![thinking_free]));
    let optional = route.describe_model(OPTIONAL_THINKING_MODEL);
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("read a"))],
    );
    request.model = OPTIONAL_THINKING_MODEL.to_owned();
    let call = route.resolve(request, &optional).unwrap();
    let events = InferenceAdapter::stream(&route, call)
        .collect::<Vec<_>>()
        .await;

    let state = events
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state),
            _ => None,
        })
        .expect("a model Z.ai lets decide may finish a tool turn without thinking");
    assert_eq!(state.model(), OPTIONAL_THINKING_MODEL);
    assert!(state.data().get("reasoning_content").is_none());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish(FinishReason::ToolCalls))))
    );
    assert!(events.iter().all(Result::is_ok));
}

#[tokio::test]
async fn the_request_carries_effort_and_explicit_preserved_thinking() {
    let (route, transport) = general(CaptureTransport::empty());
    let call = route
        .resolve(
            draft(
                GENERAL_PROVIDER,
                vec![InferenceInput::Message(ChatMessage::user("hello"))],
            ),
            &model(&route),
        )
        .unwrap();
    drop(InferenceAdapter::stream(&route, call));

    let body = transport.body(0);
    // Literals, not the crate's own constants: an assertion against
    // `ZAI_DEFAULT_REASONING_EFFORT` would agree with any value that constant
    // was changed to, and Z.ai's published default is the fact under test.
    assert_eq!(ZAI_DEFAULT_REASONING_EFFORT, "max");
    assert_eq!(body["reasoning_effort"], "max");
    assert_eq!(
        body["thinking"],
        serde_json::json!({"type":"enabled", "clear_thinking":false})
    );
    // No Z.ai page couples thinking to sampling, so DeepSeek's rule is not
    // borrowed: the caller's temperature reaches the wire.
    assert!((body["temperature"].as_f64().unwrap() - 0.7).abs() < 0.000_001);
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert_eq!(body["model"], ZAI_GLM_5_3);
}

#[test]
fn only_the_efforts_z_ai_documents_for_the_default_model_are_offered() {
    let (route, transport) = general(CaptureTransport::empty());
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("hello"))],
    );
    // `medium` is in the endpoint's parameter enum but is documented for no
    // named model, so this route must not offer it.
    request.reasoning_effort = Some(ReasoningEffortId::new("medium").unwrap());
    assert_eq!(ZAI_REASONING_EFFORTS, ["low", "high", "max"]);
    assert!(matches!(
        route.resolve(request, &model(&route)),
        Err(ResolveError::UnsupportedReasoningEffort { available, .. })
            if available.iter().map(ReasoningEffortId::as_str).collect::<Vec<_>>()
                == ["low", "high", "max"]
    ));
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn reasoning_effort_is_scoped_to_the_models_z_ai_documents_for_it() {
    assert_eq!(
        ZAI_REASONING_EFFORT_MODELS,
        ["glm-5.2", "glm-5.3", "glm-5.3-flash"]
    );

    let (route, transport) = general(CaptureTransport::empty());
    let descriptor = route.describe_model("glm-5.2");
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("hello"))],
    );
    request.model = "glm-5.2".to_owned();
    let call = route.resolve(request, &descriptor).unwrap();
    drop(InferenceAdapter::stream(&route, call));
    assert_eq!(transport.body(0)["reasoning_effort"], "max");

    let descriptor = route.describe_model("glm-5.1");
    let mut request = draft(
        GENERAL_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("hello"))],
    );
    request.model = "glm-5.1".to_owned();
    let call = route.resolve(request, &descriptor).unwrap();
    drop(InferenceAdapter::stream(&route, call));
    assert!(transport.body(1).get("reasoning_effort").is_none());
}

#[tokio::test]
async fn each_plan_dispatches_to_its_own_documented_chat_endpoint() {
    let (general_route, general_transport) = general(CaptureTransport::empty());
    let call = general_route
        .resolve(
            draft(
                GENERAL_PROVIDER,
                vec![InferenceInput::Message(ChatMessage::user("hello"))],
            ),
            &model(&general_route),
        )
        .unwrap();
    drop(InferenceAdapter::stream(&general_route, call));

    let (coding_route, coding_transport) = coding(CaptureTransport::empty());
    let mut request = draft(
        CODING_PROVIDER,
        vec![InferenceInput::Message(ChatMessage::user("hello"))],
    );
    request.provider = CODING_PROVIDER.to_owned();
    let call = coding_route
        .resolve(request, &coding_route.describe_model(ZAI_GLM_5_3))
        .unwrap();
    drop(InferenceAdapter::stream(&coding_route, call));

    let general_url = general_transport.captured.lock().unwrap()[0].url.clone();
    let coding_url = coding_transport.captured.lock().unwrap()[0].url.clone();
    assert_eq!(
        general_url,
        format!("{ZAI_GENERAL_BASE_URL}/chat/completions")
    );
    assert_eq!(
        coding_url,
        format!("{ZAI_CODING_CHAT_BASE_URL}/chat/completions")
    );
    assert_ne!(general_url, coding_url);
    assert_eq!(
        general_transport.captured.lock().unwrap()[0].authorization,
        Some("Bearer test-key".to_owned())
    );
}

#[test]
fn one_plans_assistant_state_cannot_continue_the_other_plans_route() {
    let (coding_route, transport) = coding(CaptureTransport::empty());
    let general_state = ProviderStateItem::new(
        GENERAL_PROVIDER,
        ZAI_GLM_5_3,
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant","content":null,"reasoning_content":"general-plan thinking",
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]
        }),
    )
    .unwrap();
    let mut request = draft(
        CODING_PROVIDER,
        vec![InferenceInput::ProviderState(general_state)],
    );
    request.provider = CODING_PROVIDER.to_owned();
    assert!(matches!(
        coding_route.resolve(request, &coding_route.describe_model(ZAI_GLM_5_3)),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn both_configured_routes_explicitly_preserve_thinking_without_changing_auth_evidence() {
    let [general_report, coding_report] = zai_thinking_reports().unwrap();

    assert_eq!(general_report.plan, ZaiPlan::General);
    assert_eq!(general_report.endpoint, ZAI_GENERAL_BASE_URL);
    assert_eq!(
        general_report.preserved_thinking,
        ZaiPreservedThinking::ExplicitPreservation
    );
    assert!(general_report.preserved_thinking.survives_replay());
    assert_eq!(
        general_report.auth_header,
        ZaiAuthHeaderEvidence::Documented
    );
    assert!(general_report.summary().contains("explicit-preservation"));
    assert!(!general_report.summary().contains("unverified"));

    assert_eq!(coding_report.plan, ZaiPlan::Coding);
    assert_eq!(coding_report.endpoint, ZAI_CODING_CHAT_BASE_URL);
    assert_eq!(
        coding_report.preserved_thinking,
        ZaiPreservedThinking::ExplicitPreservation
    );
    assert!(coding_report.preserved_thinking.survives_replay());
    assert_eq!(
        coding_report.auth_header,
        ZaiAuthHeaderEvidence::Undocumented
    );
    assert!(!coding_report.summary().contains("clear_thinking=false"));
    assert!(coding_report.summary().contains("unverified"));

    let (route, _transport) = coding(CaptureTransport::empty());
    assert_eq!(route.thinking_report(), coding_report);
    assert_eq!(route.plan(), ZaiPlan::Coding);
    assert_eq!(route.endpoint(), ZAI_CODING_CHAT_BASE_URL);
}

#[test]
fn the_route_publishes_maintained_model_facts_and_invents_none_for_unlisted_ids() {
    let (route, _transport) = general(CaptureTransport::empty());
    let flagship = route.describe_model(ZAI_GLM_5_3);
    assert_eq!(
        flagship.capabilities.reasoning,
        CapabilitySupport::Supported
    );
    assert_eq!(flagship.capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(flagship.max_output_tokens, Some(131_072));

    let invented = route.describe_model("glm-9-imaginary");
    assert_eq!(invented.id, "glm-9-imaginary");
    assert_eq!(invented.capabilities.reasoning, CapabilitySupport::Unknown);
    assert_eq!(invented.max_output_tokens, None);
}

#[test]
fn the_route_publishes_its_plan_identity_and_credential_reference() {
    let (general_route, _general_transport) = general(CaptureTransport::empty());
    let (coding_route, _coding_transport) = coding(CaptureTransport::empty());

    assert_eq!(general_route.info().name, GENERAL_PROVIDER);
    assert_eq!(general_route.info().default_model, ZAI_GLM_5_3);
    assert_eq!(
        Provider::credential_reference(&general_route),
        Some("ZAI_API_KEY")
    );
    assert_eq!(
        Provider::descriptor(&general_route).protocols,
        vec![ProviderProtocol::OpenAiChatCompletions]
    );

    assert_eq!(coding_route.info().name, CODING_PROVIDER);
    assert_eq!(
        Provider::credential_reference(&coding_route),
        Some("ZAI_CODING_API_KEY")
    );
    assert!(Provider::inference_adapter(&coding_route).is_some());
}

#[tokio::test]
async fn the_legacy_chat_path_refuses_to_bypass_the_thinking_replay_contract() {
    let (route, transport) = general(CaptureTransport::empty());
    let events = Provider::stream(
        &route,
        heycode_llm::ChatRequest {
            model: ZAI_GLM_5_3.to_owned(),
            messages: vec![ChatMessage::user("hello")],
            tools: None,
            temperature: None,
            max_tokens: None,
        },
    )
    .collect::<Vec<_>>()
    .await;
    assert!(matches!(
        events.first(),
        Some(Err(LlmError::InvalidResponse(_)))
    ));
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn the_routes_debug_rendering_cannot_print_a_key() {
    let (route, _transport) = general(CaptureTransport::empty());
    let rendered = format!("{route:?}");
    assert!(!rendered.contains("test-key"));
    assert!(rendered.contains(ZAI_GENERAL_BASE_URL));
}

#[test]
fn rotating_credentials_keep_the_exact_reference_and_reject_a_foreign_plan() {
    use heycode_credentials::{
        CredentialKind, CredentialQuery, CredentialReference, CredentialsService,
    };
    use heycode_llm::{AuthenticationBinding, CredentialHandle, RouteCredential};
    let credential = RouteCredential::registry(
        CredentialsService::new(),
        CredentialQuery::new(
            CredentialReference::new("CUSTOM_GLM_KEY").unwrap(),
            CredentialKind::new("api-key").unwrap(),
        ),
    );
    let route = ZaiInference::<General>::with_credential(
        HttpService::new(Arc::new(CaptureTransport::empty())),
        credential,
        None,
    )
    .unwrap();
    let expected =
        AuthenticationBinding::Credential(CredentialHandle::new("CUSTOM_GLM_KEY").unwrap());
    assert_eq!(route.authentication_binding(), expected);
    assert_eq!(
        route
            .resolve(
                draft(
                    GENERAL_PROVIDER,
                    vec![InferenceInput::Message(ChatMessage::user("hello"))]
                ),
                &model(&route)
            )
            .unwrap()
            .authentication(),
        &expected
    );
    assert_eq!(route.credential_reference(), Some("CUSTOM_GLM_KEY"));
    let foreign = RouteCredential::registry(
        CredentialsService::new(),
        CredentialQuery::new(
            CredentialReference::new("ZAI_CODING_API_KEY").unwrap(),
            CredentialKind::new("api-key").unwrap(),
        ),
    );
    assert!(
        ZaiInference::<General>::with_credential(
            HttpService::new(Arc::new(CaptureTransport::empty())),
            foreign,
            None
        )
        .is_err()
    );
}

#[test]
fn invalid_temperature_fails_before_transport() {
    let (route, transport) = general(CaptureTransport::empty());
    for temperature in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        let mut request = draft(
            GENERAL_PROVIDER,
            vec![InferenceInput::Message(ChatMessage::user("hello"))],
        );
        request.temperature = Some(temperature);
        assert!(route.resolve(request, &model(&route)).is_err());
    }
    assert_eq!(transport.request_count(), 0);
}

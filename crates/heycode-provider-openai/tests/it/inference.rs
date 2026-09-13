//! POA02 provider-owned OpenAI inference wiring.
//!
//! The catalog and auth plugins alone advertise no way to dispatch. This is
//! the seam that makes the Responses route reachable, and that chooses the
//! retained-item requirement the adapter enforces on replay.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::ProviderProtocol;
use heycode_http::{
    HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream, TransportError,
};
use heycode_llm::{
    CallPurpose, ChatMessage, ChatRequest, ChatToolCall, InferenceInput, InputModality,
    ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, Provider,
    ProviderStateItem, ProviderStateKind, RequestDraft, ResponsesContinuation,
};
use heycode_provider_openai::{OPENAI_API_KEY_REFERENCE, OPENAI_GPT_5_6_SOL, OpenAiProvider};
use tokio_util::sync::CancellationToken;

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

#[derive(Default)]
struct UrlRecordingTransport(std::sync::Mutex<Option<String>>);

impl HttpTransport for UrlRecordingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        *self.0.lock().unwrap() = Some(request.url().to_owned());
        Box::pin(futures::stream::empty())
    }
}

fn provider() -> OpenAiProvider {
    OpenAiProvider::new(
        HttpService::new(Arc::new(DeadTransport)),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap()
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
            tools: heycode_llm::CapabilitySupport::Supported,
            reasoning: heycode_llm::CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft(inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        provider: "openai".to_owned(),
        model: OPENAI_GPT_5_6_SOL.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1_000,
        system: None,
        inputs,
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

fn state(data: serde_json::Value) -> InferenceInput {
    InferenceInput::ProviderState(
        ProviderStateItem::new(
            "openai",
            OPENAI_GPT_5_6_SOL,
            ProviderProtocol::OpenAiResponses,
            ProviderStateKind::ResponseOutputItem,
            data,
        )
        .unwrap(),
    )
}

/// A refusal is only useful if it names the requirement that fired: a bare
/// `is_err()` would pass for a replay rejected for some unrelated reason and
/// would still read as if it proved the contract.
#[track_caller]
fn assert_refused(
    outcome: Result<heycode_llm::ResolvedCall, heycode_llm::ResolveError>,
    reason: &str,
) {
    // `expect_err` would demand `Debug` on `ResolvedCall`, which the
    // resolution boundary deliberately does not implement.
    let Err(error) = outcome else {
        panic!("this replay must be refused");
    };
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("provider_state"),
        "refusal must name the `provider_state` field: {rendered}"
    );
    assert!(
        rendered.contains(reason),
        "refusal must name `{reason}`: {rendered}"
    );
}

#[test]
fn provider_identity_matches_the_profile_and_advertises_the_responses_adapter() {
    let provider = provider();
    let info = provider.info();
    assert_eq!(info.name, "openai");
    assert_eq!(info.default_model, OPENAI_GPT_5_6_SOL);
    assert_eq!(
        provider.credential_reference(),
        Some(OPENAI_API_KEY_REFERENCE)
    );
    assert_eq!(
        Provider::descriptor(&provider).protocols,
        vec![
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions
        ]
    );
    assert!(Provider::inference_adapter(&provider).is_some());
    // No header spelling was verifiable from a primary source, so the route
    // declares none rather than publishing a guess.
    assert!(provider.rate_limit_headers().is_empty());

    // The catalog is account-scoped, so a route is routinely built for an id
    // that is not the flagship. A configured id is authoritative; only its
    // absence falls back to the provider default.
    let configured = OpenAiProvider::new(
        HttpService::new(Arc::new(DeadTransport)),
        "test-key",
        Some("gpt-5.3-codex".to_owned()),
    )
    .unwrap();
    assert_eq!(configured.info().default_model, "gpt-5.3-codex");
    let unconfigured =
        OpenAiProvider::new(HttpService::new(Arc::new(DeadTransport)), "test-key", None).unwrap();
    assert_eq!(unconfigured.info().default_model, OPENAI_GPT_5_6_SOL);
}

#[test]
fn advertised_inference_descriptor_is_route_exact_while_provider_stays_multi_protocol() {
    let provider = provider();
    assert_eq!(
        Provider::descriptor(&provider).protocols,
        vec![
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions,
        ]
    );

    let adapter = Provider::inference_adapter(&provider).unwrap();
    assert_eq!(
        adapter.descriptor().protocols,
        vec![ProviderProtocol::OpenAiResponses]
    );
    let call = adapter
        .resolve(
            draft(vec![InferenceInput::Message(ChatMessage::user("go"))]),
            &model(),
        )
        .unwrap();
    assert_eq!(call.protocol(), ProviderProtocol::OpenAiResponses);
}

#[tokio::test]
async fn the_legacy_chat_path_fails_loud_once_an_adapter_is_advertised() {
    // AGENTS §5: once `inference_adapter` is Some, no legacy fallback is
    // permitted. A silent legacy dispatch would bypass every replay contract
    // this row exists to enforce.
    let provider = provider();
    let mut stream = provider.stream(ChatRequest {
        model: OPENAI_GPT_5_6_SOL.to_owned(),
        messages: vec![ChatMessage::user("hi")],
        tools: None,
        temperature: None,
        max_tokens: None,
    });
    let first = stream.next().await.expect("legacy path must yield an item");
    assert!(first.is_err(), "legacy dispatch must not silently succeed");
    assert!(stream.next().await.is_none());
}

#[test]
fn the_official_route_requires_preserved_reasoning_and_phase_on_replay() {
    // The provider profile chooses the dialect; this pins that OpenAI's route
    // selects the strictest documented requirement rather than leaving the
    // adapter's permissive default in place.
    let provider = provider();
    let adapter = Provider::inference_adapter(&provider).unwrap();

    let dropped_reasoning = draft(vec![
        InferenceInput::Message(ChatMessage::user("go")),
        state(serde_json::json!({
            "type":"function_call","id":"fc_1","call_id":"call_1",
            "name":"lookup","arguments":"{}"
        })),
    ]);
    assert_refused(
        adapter.resolve(dropped_reasoning, &model()),
        "missing its turn's encrypted reasoning item",
    );

    let dropped_phase = draft(vec![
        InferenceInput::Message(ChatMessage::user("go")),
        state(serde_json::json!({
            "type":"reasoning","id":"rs_1","encrypted_content":"gAAAA-x"
        })),
        state(serde_json::json!({
            "type":"message","id":"msg_1","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":"done"}]
        })),
    ]);
    assert_refused(
        adapter.resolve(dropped_phase, &model()),
        "commentary or final_answer",
    );

    let neutral = draft(vec![
        InferenceInput::Message(ChatMessage::user("go")),
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
    ]);
    assert_refused(
        adapter.resolve(neutral, &model()),
        "exact Responses output items",
    );

    let complete = draft(vec![
        InferenceInput::Message(ChatMessage::user("go")),
        state(serde_json::json!({
            "type":"reasoning","id":"rs_1","encrypted_content":"gAAAA-x"
        })),
        state(serde_json::json!({
            "type":"function_call","id":"fc_1","call_id":"call_1",
            "name":"lookup","arguments":"{}"
        })),
    ]);
    assert!(adapter.resolve(complete, &model()).is_ok());
}

#[test]
fn the_route_requirement_is_the_strictest_documented_level() {
    assert_eq!(
        OpenAiProvider::CONTINUATION,
        ResponsesContinuation::ReasoningAndPhase
    );
}

#[test]
fn the_phase_preserving_route_refuses_mutated_or_neutral_assistant_state() {
    let provider = provider();
    let adapter = Provider::inference_adapter(&provider).unwrap();

    assert_refused(
        adapter.resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("go")),
                state(serde_json::json!({
                    "type":"message","id":"msg_1","role":"assistant",
                    "status":"completed","phase":"invented",
                    "content":[{"type":"output_text","text":"done"}]
                })),
            ]),
            &model(),
        ),
        "commentary or final_answer",
    );

    assert_refused(
        adapter.resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("go")),
                InferenceInput::Message(ChatMessage::assistant("done")),
            ]),
            &model(),
        ),
        "exact Responses output items",
    );

    assert_refused(
        adapter.resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("go")),
                state(serde_json::json!({
                    "type":"reasoning","id":"rs_1","encrypted_content":""
                })),
            ]),
            &model(),
        ),
        "non-empty encrypted_content",
    );

    assert!(
        adapter
            .resolve(
                draft(vec![
                    InferenceInput::Message(ChatMessage::user("go")),
                    state(serde_json::json!({
                        "type":"message","id":"msg_1","role":"assistant",
                        "status":"completed","phase":"final_answer",
                        "content":[{"type":"output_text","text":"done"}]
                    })),
                ]),
                &model(),
            )
            .is_ok()
    );
}

#[tokio::test]
async fn the_official_route_dispatches_to_the_documented_responses_endpoint() {
    // The provider owns the origin; the adapter owns the `/responses` path.
    // A wrong join here would only surface as a 404 at runtime.
    let transport = Arc::new(UrlRecordingTransport::default());
    let provider = OpenAiProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            draft(vec![InferenceInput::Message(ChatMessage::user("go"))]),
            &model(),
        )
        .unwrap();
    let mut stream = adapter.stream(call);
    while stream.next().await.is_some() {}
    assert_eq!(
        transport.0.lock().unwrap().as_deref(),
        Some("https://api.openai.com/v1/responses")
    );
}

// ---------------------------------------------------------------------------
// The stateless loop itself.
//
// Every continuation test above — and every one in `heycode-llm` — hand-builds the
// items it replays, so none of them can see a field the *parser* dropped. These
// drive a real loop: the items replayed are the exact `ProviderStateItem`s the
// stream published, so a distilled or re-derived state fails here.
// ---------------------------------------------------------------------------

/// One route driven across several turns. Each request pops the next scripted
/// response and records its exact JSON body, so the turns share one provider
/// the way a real tool loop does.
struct TurnTransport {
    scripts: Mutex<VecDeque<Vec<Result<SseEvent, TransportError>>>>,
    bodies: Mutex<Vec<serde_json::Value>>,
}

impl TurnTransport {
    fn scripted(scripts: Vec<Vec<Result<SseEvent, TransportError>>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            bodies: Mutex::new(Vec::new()),
        })
    }

    fn body(&self, turn: usize) -> serde_json::Value {
        self.bodies.lock().unwrap()[turn].clone()
    }
}

impl HttpTransport for TurnTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        let body: serde_json::Value = serde_json::from_slice(request.body().unwrap_or_default())
            .expect("request body must be JSON");
        self.bodies.lock().unwrap().push(body);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("an unscripted request reached the transport");
        Box::pin(futures::stream::iter(script))
    }
}

fn sse_event(name: &str, data: serde_json::Value) -> Result<SseEvent, TransportError> {
    Ok(SseEvent {
        event: name.to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    })
}

/// A completed reasoning item exactly as `/v1/responses` publishes it under
/// `include: ["reasoning.encrypted_content"]`: the encrypted blob is opaque and
/// only survives if it is replayed untouched.
fn reasoning_item(id: &str, encrypted: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "type": "reasoning",
        "summary": [{"type": "summary_text", "text": "check the catalog"}],
        "content": [],
        "encrypted_content": encrypted,
        "status": "completed",
    })
}

fn function_call_item() -> serde_json::Value {
    serde_json::json!({
        "id": "fc_1",
        "type": "function_call",
        "call_id": "call_1",
        "name": "lookup",
        "arguments": "{\"q\":\"heycode\"}",
        "status": "completed",
    })
}

fn message_item(id: &str, phase: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "phase": phase,
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

/// Turn one: the model reasons, then calls a client tool.
fn reasoning_then_tool_call_turn() -> Vec<Result<SseEvent, TransportError>> {
    vec![
        sse_event(
            "response.created",
            serde_json::json!({
                "type":"response.created","sequence_number":0,
                "response":{"id":"resp_1","status":"in_progress"}
            }),
        ),
        sse_event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added","sequence_number":1,"output_index":0,
                "item":{"id":"rs_1","type":"reasoning","summary":[]}
            }),
        ),
        sse_event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done","sequence_number":2,"output_index":0,
                "item":reasoning_item("rs_1","gAAAAABpM0Yj-turn-one-opaque-state")
            }),
        ),
        sse_event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added","sequence_number":3,"output_index":1,
                "item":{"id":"fc_1","type":"function_call","call_id":"call_1",
                        "name":"lookup","arguments":""}
            }),
        ),
        sse_event(
            "response.function_call_arguments.delta",
            serde_json::json!({
                "type":"response.function_call_arguments.delta","sequence_number":4,
                "output_index":1,"item_id":"fc_1","delta":"{\"q\":\"heycode\"}"
            }),
        ),
        sse_event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done","sequence_number":5,"output_index":1,
                "item":function_call_item()
            }),
        ),
        sse_event(
            "response.completed",
            serde_json::json!({
                "type":"response.completed","sequence_number":6,
                "response":{"id":"resp_1","status":"completed","output":[],
                            "usage":{"input_tokens":11,"output_tokens":7}}
            }),
        ),
    ]
}

/// Turn two: the model reasons again, emits a `commentary` preamble and then a
/// `final_answer`. Both phases are documented values and both must survive.
fn commentary_then_final_answer_turn() -> Vec<Result<SseEvent, TransportError>> {
    vec![
        sse_event(
            "response.created",
            serde_json::json!({
                "type":"response.created","sequence_number":0,
                "response":{"id":"resp_2","status":"in_progress"}
            }),
        ),
        sse_event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added","sequence_number":1,"output_index":0,
                "item":{"id":"rs_2","type":"reasoning","summary":[]}
            }),
        ),
        sse_event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done","sequence_number":2,"output_index":0,
                "item":reasoning_item("rs_2","gAAAAABpM0Yj-turn-two-opaque-state")
            }),
        ),
        sse_event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added","sequence_number":3,"output_index":1,
                "item":{"id":"msg_pre","type":"message","role":"assistant",
                        "status":"in_progress","content":[]}
            }),
        ),
        sse_event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done","sequence_number":4,"output_index":1,
                "item":message_item("msg_pre","commentary","Looking that up now.")
            }),
        ),
        sse_event(
            "response.output_item.added",
            serde_json::json!({
                "type":"response.output_item.added","sequence_number":5,"output_index":2,
                "item":{"id":"msg_final","type":"message","role":"assistant",
                        "status":"in_progress","content":[]}
            }),
        ),
        sse_event(
            "response.output_item.done",
            serde_json::json!({
                "type":"response.output_item.done","sequence_number":6,"output_index":2,
                "item":message_item("msg_final","final_answer","heycode is a terminal coding agent.")
            }),
        ),
        sse_event(
            "response.completed",
            serde_json::json!({
                "type":"response.completed","sequence_number":7,
                "response":{"id":"resp_2","status":"completed","output":[],
                            "usage":{"input_tokens":29,"output_tokens":13}}
            }),
        ),
    ]
}

/// A turn that only has to reach the wire; its output is never replayed.
fn empty_turn() -> Vec<Result<SseEvent, TransportError>> {
    vec![sse_event(
        "response.completed",
        serde_json::json!({
            "type":"response.completed","sequence_number":0,
            "response":{"id":"resp_3","status":"completed","output":[],
                        "usage":{"input_tokens":41,"output_tokens":0}}
        }),
    )]
}

/// Dispatch one turn and return the continuation state the stream published, in
/// stream order. A stream error fails the turn rather than yielding fewer items.
async fn published_state(
    provider: &OpenAiProvider,
    inputs: Vec<InferenceInput>,
) -> Vec<ProviderStateItem> {
    let adapter = Provider::inference_adapter(provider).unwrap();
    let call = adapter
        .resolve(draft(inputs), &model())
        .expect("turn must resolve");
    let mut stream = adapter.stream(call);
    let mut published = Vec::new();
    while let Some(event) = stream.next().await {
        if let heycode_llm::InferenceEvent::ProviderState(state) =
            event.expect("turn must not fail")
        {
            published.push(state);
        }
    }
    published
}

fn replay(state: &ProviderStateItem) -> InferenceInput {
    InferenceInput::ProviderState(state.clone())
}

/// POA02's acceptance, driven end to end: a stateless tool loop replays the
/// required items, and every one of them reaches the wire as the value the
/// stream published — same keys, same order, same opaque blobs.
#[tokio::test]
async fn a_stateless_tool_loop_replays_every_published_item_unchanged() {
    let transport = TurnTransport::scripted(vec![
        reasoning_then_tool_call_turn(),
        commentary_then_final_answer_turn(),
        empty_turn(),
    ]);
    let route = OpenAiProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();

    // Turn one: ask, and receive a reasoning item plus a tool call.
    let opening = vec![InferenceInput::Message(ChatMessage::user(
        "what is heycode?",
    ))];
    let first = published_state(&route, opening.clone()).await;
    assert_eq!(
        first.len(),
        2,
        "reasoning and function call are both retained"
    );

    // Turn two: replay both, append the tool result.
    let mut history = opening;
    history.push(replay(&first[0]));
    history.push(replay(&first[1]));
    history.push(InferenceInput::Message(ChatMessage::tool(
        "call_1",
        "{\"answer\":\"a terminal coding agent\"}",
    )));
    let second = published_state(&route, history.clone()).await;
    assert_eq!(second.len(), 3, "reasoning, commentary and final answer");

    // Turn three: replay the whole history and ask again.
    history.extend(second.iter().map(replay));
    history.push(InferenceInput::Message(ChatMessage::user(
        "and in one word?",
    )));
    let _ = published_state(&route, history).await;

    // `store: false` is what makes replay load-bearing: nothing is retained
    // server-side, so an item the caller drops is simply gone.
    let body = transport.body(2);
    assert_eq!(body["store"], serde_json::json!(false));
    // Still sent, and still accepted: the reasoning guide now calls this the
    // "legacy" `include` value and says the API "doesn't require it", not that
    // it is rejected.
    assert_eq!(
        body["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );

    let input = body["input"].as_array().unwrap();
    assert_eq!(
        input.len(),
        8,
        "no replayed item is dropped, merged or reordered"
    );
    assert_eq!(
        input[0],
        serde_json::json!({
            "type":"message","role":"user",
            "content":[{"type":"input_text","text":"what is heycode?"}]
        })
    );
    assert_eq!(input[1], *first[0].data(), "turn-one reasoning item");
    assert_eq!(input[2], *first[1].data(), "turn-one function call");
    assert_eq!(
        input[3],
        serde_json::json!({
            "type":"function_call_output","call_id":"call_1",
            "output":"{\"answer\":\"a terminal coding agent\"}"
        })
    );
    assert_eq!(input[4], *second[0].data(), "turn-two reasoning item");
    assert_eq!(input[5], *second[1].data(), "commentary message");
    assert_eq!(input[6], *second[2].data(), "final answer message");
    assert_eq!(
        input[7],
        serde_json::json!({
            "type":"message","role":"user",
            "content":[{"type":"input_text","text":"and in one word?"}]
        })
    );

    // The three fields the row exists for, named explicitly so a regression
    // that keeps the item but empties one of them still fails.
    assert_eq!(
        input[1]["encrypted_content"],
        serde_json::json!("gAAAAABpM0Yj-turn-one-opaque-state")
    );
    assert_eq!(
        input[4]["encrypted_content"],
        serde_json::json!("gAAAAABpM0Yj-turn-two-opaque-state")
    );
    assert_eq!(input[2]["call_id"], serde_json::json!("call_1"));
    assert_eq!(input[5]["phase"], serde_json::json!("commentary"));
    assert_eq!(input[6]["phase"], serde_json::json!("final_answer"));
}

/// The published state is the wire item itself, not a normalized projection of
/// it. `phase` and `encrypted_content` are the two fields with no neutral
/// representation anywhere in heycode, so a distilling parser loses them here and
/// nothing downstream would ever notice.
///
/// It must also come from the completed item: the specification says the
/// `encrypted_content` in `response.output_item.added` "may be incomplete" and
/// that this "is especially important when `store` is `false`". Every scripted
/// `.added` item below carries no `encrypted_content` at all, so a parser that
/// published the opening item would fail here rather than on the live service.
#[tokio::test]
async fn published_state_is_the_wire_item_itself_under_this_route_identity() {
    let transport = TurnTransport::scripted(vec![commentary_then_final_answer_turn()]);
    let route = OpenAiProvider::new(
        HttpService::new(transport),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();

    let published = published_state(
        &route,
        vec![InferenceInput::Message(ChatMessage::user("go"))],
    )
    .await;

    let replayed: Vec<serde_json::Value> =
        published.iter().map(|state| state.data().clone()).collect();
    assert_eq!(
        replayed,
        vec![
            reasoning_item("rs_2", "gAAAAABpM0Yj-turn-two-opaque-state"),
            message_item("msg_pre", "commentary", "Looking that up now."),
            message_item(
                "msg_final",
                "final_answer",
                "heycode is a terminal coding agent."
            ),
        ],
        "every key of every completed output item is retained verbatim"
    );
    assert!(
        published
            .iter()
            .all(|state| state.data().get("status") == Some(&serde_json::json!("completed"))),
        "only completed output items become continuation state"
    );
    for state in &published {
        // State is only replayable on the route that produced it: a mismatched
        // provider, model or protocol is refused before transport.
        assert_eq!(state.provider(), "openai");
        assert_eq!(state.model(), OPENAI_GPT_5_6_SOL);
        assert_eq!(state.protocol(), ProviderProtocol::OpenAiResponses);
        assert_eq!(state.kind(), ProviderStateKind::ResponseOutputItem);
        assert_eq!(state.schema_version(), 1);
    }
}

/// State published by one model is not replayable on another: the encrypted
/// reasoning blob is opaque and model-bound, so a silent cross-model replay
/// would be rejected by the service, not by us.
#[test]
fn state_published_by_another_model_never_reaches_this_route() {
    let transport = TurnTransport::scripted(vec![reasoning_then_tool_call_turn()]);
    let route = OpenAiProvider::new(
        HttpService::new(transport),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&route).unwrap();

    let foreign = InferenceInput::ProviderState(
        ProviderStateItem::new(
            "openai",
            "gpt-5.3-codex",
            ProviderProtocol::OpenAiResponses,
            ProviderStateKind::ResponseOutputItem,
            reasoning_item("rs_1", "gAAAAABpM0Yj-turn-one-opaque-state"),
        )
        .unwrap(),
    );
    let error = adapter
        .resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("go")),
                foreign,
                InferenceInput::ProviderState(
                    ProviderStateItem::new(
                        "openai",
                        OPENAI_GPT_5_6_SOL,
                        ProviderProtocol::OpenAiResponses,
                        ProviderStateKind::ResponseOutputItem,
                        function_call_item(),
                    )
                    .unwrap(),
                ),
            ]),
            &model(),
        )
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("provider_state"),
        "foreign state must be refused on the `provider_state` field: {error:?}"
    );
}

//! PAN02 provider-owned Anthropic route and thinking/interleaved tool state.
//!
//! PAN01 shipped auth, catalog and token counting but nothing that could
//! dispatch. These cases pin the route itself and the one property the row
//! exists for: a thinking turn waiting on tool results replays its own thinking
//! blocks, signatures byte-identical, or the request never leaves the process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::{
    NativeToolImplementationKind, NativeToolRoute, ProviderProtocol, ServerToolOutcome,
};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, ChatRequest, ChatToolCall, FinishReason,
    InferenceEvent, InferenceInput, InputModality, ModelCapabilities, ModelDescriptor,
    ModelLifecycle, ModelPerformance, ModelPricing, NativeFeature, Provider, ProviderOptionContext,
    ProviderStateItem, ProviderStateKind, ReasoningEffortId, RequestDraft, ResolveError,
    ResolvedCall, Role, ToolSpec,
};
use heycode_provider_anthropic::{
    ANTHROPIC_API_KEY_REFERENCE, ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_DEFAULT_REASONING_EFFORT,
    ANTHROPIC_REASONING_EFFORTS, ANTHROPIC_VERSION, AnthropicContextEditingPolicy,
    AnthropicPromptCachePolicy, AnthropicPromptCacheTtl, AnthropicProvider,
    AnthropicServerToolDefinition, AnthropicServerToolKind, AnthropicServerToolPlan,
    AnthropicThinkingClear, AnthropicThinkingContinuation, AnthropicThinkingKeep,
    INTERLEAVED_THINKING_BETA,
};
use tokio_util::sync::CancellationToken;

/// A real Claude signature is a long opaque base64-ish blob. Using one with
/// padding, slashes and mixed case here means a test would fail if any layer
/// re-encoded, trimmed or normalized it on the way back out.
const SIGNATURE: &str = "EosnCkYICxIMMb3LzNrMu/qX+Y0aDHl0aGlua2luZw==+/9jZQ";

#[derive(Debug, Clone)]
struct Captured {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Records every dispatched request and serves one scripted SSE stream per
/// call, so a refusal can be distinguished from a request that was sent and
/// merely failed later.
#[derive(Default)]
struct RecordingTransport {
    scripts: Mutex<Vec<Vec<SseEvent>>>,
    captured: Mutex<Vec<Captured>>,
}

impl RecordingTransport {
    fn with_scripts(scripts: Vec<Vec<SseEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts),
            captured: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }

    fn last_body(&self) -> serde_json::Value {
        let captured = self.captured.lock().unwrap();
        let last = captured.last().expect("a request must have been sent");
        serde_json::from_slice(&last.body).expect("request body must be JSON")
    }
}

impl HttpTransport for RecordingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.captured.lock().unwrap().push(Captured {
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().unwrap_or_default().to_vec(),
        });
        let mut scripts = self.scripts.lock().unwrap();
        let script = if scripts.is_empty() {
            Vec::new()
        } else {
            scripts.remove(0)
        };
        Box::pin(futures::stream::iter(script.into_iter().map(Ok)))
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

/// One complete thinking-plus-tool-use turn exactly as the Messages stream
/// delivers it: the signature arrives as a `signature_delta` "just before the
/// `content_block_stop` event".
fn thinking_tool_script() -> Vec<SseEvent> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start",
                "message":{
                    "id":"msg_01",
                    "type":"message",
                    "role":"assistant",
                    "model":ANTHROPIC_CLAUDE_OPUS_5,
                    "content":[],
                    "stop_reason":serde_json::Value::Null,
                    "stop_sequence":serde_json::Value::Null,
                    "usage":{"input_tokens":11,"output_tokens":0}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{"type":"thinking","thinking":"","signature":""}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta",
                "index":0,
                "delta":{"type":"thinking_delta","thinking":"weigh the options"}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta",
                "index":0,
                "delta":{"type":"signature_delta","signature":SIGNATURE}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":1,
                "content_block":{"type":"tool_use","id":"toolu_01","name":"lookup","input":{}}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta",
                "index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"q\":\"paris\"}"}
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
                "delta":{"stop_reason":"tool_use","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":42}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn route(transport: Arc<RecordingTransport>) -> AnthropicProvider {
    AnthropicProvider::new(HttpService::new(transport), "test-key", None).unwrap()
}

fn dead_route() -> AnthropicProvider {
    route(RecordingTransport::with_scripts(Vec::new()))
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        display_name: "Claude Opus 5".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_000_000),
        max_output_tokens: Some(64_000),
        lifecycle: ModelLifecycle::stable(),
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

fn lookup_tool() -> ToolSpec {
    ToolSpec {
        name: "lookup".to_owned(),
        description: "Look one term up.".to_owned(),
        parameters: serde_json::json!({"type":"object","properties":{}}),
    }
}

fn draft(inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        provider: "anthropic".to_owned(),
        model: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1_000,
        system: None,
        inputs,
        tools: vec![lookup_tool()],
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        // The Messages API requires `max_tokens` and publishes no default, so
        // this route inherits none and every caller supplies the cap.
        max_output_tokens: Some(8_192),
        purpose: CallPurpose::Conversation,
    }
}

fn provider_route(kind: AnthropicServerToolKind) -> NativeToolRoute {
    NativeToolRoute::new(
        kind.as_str(),
        format!("anthropic:{}", kind.as_str()),
        NativeToolImplementationKind::Provider,
        Some("anthropic".to_owned()),
    )
    .unwrap()
}

fn apply_server_tool_selection(
    provider: &AnthropicProvider,
    request: &mut RequestDraft,
    descriptor: &ModelDescriptor,
    kinds: &[AnthropicServerToolKind],
) -> Result<(), ResolveError> {
    request.native_tool_routes = kinds.iter().copied().map(provider_route).collect();
    if kinds.iter().any(|kind| {
        matches!(
            kind,
            AnthropicServerToolKind::WebSearch | AnthropicServerToolKind::WebFetch
        )
    }) {
        request.native_features.push(NativeFeature::Web);
    }
    request.provider_options = Provider::request_options_for(
        provider,
        ProviderOptionContext::new(descriptor, &request.native_tool_routes),
    )?;
    Ok(())
}

fn state_for_model(model: &str, content: serde_json::Value) -> InferenceInput {
    InferenceInput::ProviderState(
        ProviderStateItem::new(
            "anthropic",
            model,
            ProviderProtocol::AnthropicMessages,
            ProviderStateKind::AnthropicMessage,
            serde_json::json!({"role":"assistant","content":content}),
        )
        .unwrap(),
    )
}

fn state(content: serde_json::Value) -> InferenceInput {
    state_for_model(ANTHROPIC_CLAUDE_OPUS_5, content)
}

fn thinking_block(signature: &str) -> serde_json::Value {
    serde_json::json!({"type":"thinking","thinking":"weigh the options","signature":signature})
}

fn tool_use_block(id: &str) -> serde_json::Value {
    serde_json::json!({"type":"tool_use","id":id,"name":"lookup","input":{"q":"paris"}})
}

fn neutral_tool_call(id: &str) -> InferenceInput {
    InferenceInput::Message(ChatMessage {
        role: Role::Assistant,
        content: String::new(),
        images: Vec::new(),
        documents: Vec::new(),
        tool_calls: Some(vec![ChatToolCall {
            id: id.to_owned(),
            name: "lookup".to_owned(),
            arguments: "{\"q\":\"paris\"}".to_owned(),
        }]),
        tool_call_id: None,
        tool_result_is_error: None,
    })
}

fn tool_result(id: &str) -> InferenceInput {
    InferenceInput::Message(ChatMessage {
        role: Role::Tool,
        content: "20C, sunny".to_owned(),
        images: Vec::new(),
        documents: Vec::new(),
        tool_calls: None,
        tool_call_id: Some(id.to_owned()),
        tool_result_is_error: Some(false),
    })
}

/// A refusal is only evidence if it names the requirement that fired. A bare
/// `is_err()` would also pass for a replay rejected for an unrelated reason and
/// would still read as though it proved this row's contract.
#[track_caller]
fn assert_refused(outcome: Result<ResolvedCall, ResolveError>, reason: &str) {
    let error = outcome.expect_err("this replay must be refused");
    let ResolveError::InvalidRequest { field, message } = &error else {
        panic!("expected an invalid-request refusal, got {error:?}");
    };
    assert_eq!(*field, "provider_state", "refusal must name provider_state");
    assert!(
        message.contains(reason),
        "refusal must name `{reason}`: {message}"
    );
}

#[test]
fn the_route_publishes_the_provider_identity_and_its_messages_adapter() {
    let provider = dead_route();
    let info = provider.info();
    assert_eq!(info.name, "anthropic");
    assert_eq!(info.default_model, ANTHROPIC_CLAUDE_OPUS_5);
    assert_eq!(
        provider.credential_reference(),
        Some(ANTHROPIC_API_KEY_REFERENCE)
    );
    assert_eq!(
        Provider::descriptor(&provider).protocols,
        vec![ProviderProtocol::AnthropicMessages]
    );
    assert!(Provider::inference_adapter(&provider).is_some());
    // No `anthropic-ratelimit-*` spelling was proven by this row, so the route
    // declares none rather than making `/usage` display a guess.
    assert!(provider.rate_limit_headers().is_empty());
}

#[test]
fn request_specific_routes_cannot_silently_enable_the_whole_server_tool_plan() {
    let unconfigured = dead_route();
    let unconfigured_route = provider_route(AnthropicServerToolKind::WebSearch);
    assert!(
        Provider::request_options_for(
            &unconfigured,
            ProviderOptionContext::new(&model(), &[unconfigured_route]),
        )
        .is_err(),
        "a provider route without its provider-owned plan must fail loud"
    );

    let plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::web_search(),
        AnthropicServerToolDefinition::code_execution(),
    ])
    .unwrap();
    let provider = dead_route().with_server_tools(plan).unwrap();

    let no_native =
        Provider::request_options_for(&provider, ProviderOptionContext::new(&model(), &[]))
            .unwrap();
    assert!(
        no_native
            .iter()
            .all(|option| option.kind() != "server-tools"),
        "a configured provider plan is not selected request authority"
    );

    let web = provider_route(AnthropicServerToolKind::WebSearch);
    assert!(
        Provider::request_options_for(&provider, ProviderOptionContext::new(&model(), &[web]),)
            .is_err(),
        "the current shared plan is all-or-none and must refuse a partial route set"
    );

    let routes = [
        provider_route(AnthropicServerToolKind::WebSearch),
        provider_route(AnthropicServerToolKind::CodeExecution),
    ];
    let selected =
        Provider::request_options_for(&provider, ProviderOptionContext::new(&model(), &routes))
            .unwrap();
    assert_eq!(
        selected
            .iter()
            .filter(|option| option.kind() == "server-tools")
            .count(),
        1
    );
}

#[tokio::test]
async fn the_legacy_chat_path_fails_loud_once_the_adapter_is_advertised() {
    // AGENTS §5: a silent legacy dispatch would bypass every thinking replay
    // contract this row enforces in `resolve`.
    let provider = dead_route();
    let mut stream = Provider::stream(
        &provider,
        ChatRequest {
            model: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
            messages: vec![ChatMessage::user("hi")],
            tools: None,
            temperature: None,
            max_tokens: None,
        },
    );
    assert!(stream.next().await.expect("one item").is_err());
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn the_official_route_asks_for_adaptive_thinking_at_the_documented_endpoint() {
    // `thinking: {"type":"enabled"}` returns a 400 on Claude Opus 4.7 and
    // later, which includes this route's default model, so adaptive is the only
    // correct dialect here — and interleaving is automatic, so no beta header
    // is sent.
    let transport = RecordingTransport::with_scripts(vec![Vec::new()]);
    let provider = route(transport.clone());
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            draft(vec![InferenceInput::Message(ChatMessage::user("go"))]),
            &model(),
        )
        .unwrap();
    let mut stream = adapter.stream(call);
    while stream.next().await.is_some() {}

    let sent = transport.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, "https://api.anthropic.com/v1/messages");
    assert!(
        sent[0]
            .headers
            .iter()
            .any(|(name, value)| name == "x-api-key" && value == "test-key")
    );
    assert!(
        sent[0]
            .headers
            .iter()
            .any(|(name, value)| name == "anthropic-version" && value == ANTHROPIC_VERSION)
    );
    assert!(
        !sent[0]
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta")),
        "adaptive thinking interleaves with no beta header"
    );
    let body = transport.last_body();
    assert_eq!(body["thinking"], serde_json::json!({"type":"adaptive"}));
    assert_eq!(
        body["output_config"],
        serde_json::json!({"effort":ANTHROPIC_DEFAULT_REASONING_EFFORT})
    );
    assert_eq!(body["max_tokens"], serde_json::json!(8_192));
}

#[test]
fn every_documented_effort_level_resolves_and_only_none_disables_thinking() {
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    for effort in ANTHROPIC_REASONING_EFFORTS {
        let mut request = draft(vec![InferenceInput::Message(ChatMessage::user("go"))]);
        request.reasoning_effort = Some(ReasoningEffortId::new(effort).unwrap());
        let call = adapter
            .resolve(request, &model())
            .unwrap_or_else(|error| panic!("effort {effort} must resolve: {error:?}"));
        assert_eq!(
            call.reasoning_effort().map(ReasoningEffortId::as_str),
            Some(effort)
        );
    }
    assert_eq!(
        provider.continuation(),
        AnthropicThinkingContinuation::Adaptive
    );
}

#[test]
fn a_neutral_assistant_tool_call_cannot_continue_a_thinking_turn() {
    // A neutral assistant message has no field that can carry a thinking block.
    // Sent as-is the API does not error: it silently disables thinking for the
    // request, so only a local refusal makes the loss visible.
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    assert_refused(
        adapter.resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("go")),
                neutral_tool_call("toolu_01"),
                tool_result("toolu_01"),
            ]),
            &model(),
        ),
        "neutral assistant message cannot carry its thinking blocks",
    );
}

#[test]
fn the_same_neutral_history_is_accepted_once_the_route_stops_thinking() {
    // The requirement is conditioned on the resolved thinking mode, not on the
    // provider name or the shape of the history.
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let mut request = draft(vec![
        InferenceInput::Message(ChatMessage::user("go")),
        neutral_tool_call("toolu_01"),
        tool_result("toolu_01"),
    ]);
    request.reasoning_effort = Some(ReasoningEffortId::new("none").unwrap());
    assert!(adapter.resolve(request, &model()).is_ok());
}

#[test]
fn a_replayed_thinking_block_that_lost_its_signature_never_reaches_the_wire() {
    // The signature is what the API decrypts to verify the block was generated
    // by Claude. A re-serializer, a truncated log or a hand-built item that
    // drops it produces a block that passes every schema check and is rejected
    // live.
    let transport = RecordingTransport::with_scripts(Vec::new());
    let provider = route(transport.clone());
    let adapter = Provider::inference_adapter(&provider).unwrap();
    assert_refused(
        adapter.resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("go")),
                state(serde_json::json!([
                    thinking_block(""),
                    tool_use_block("toolu_01")
                ])),
                tool_result("toolu_01"),
            ]),
            &model(),
        ),
        "lost its opaque continuity field",
    );
    assert!(
        transport.requests().is_empty(),
        "a refused replay must send no request bytes"
    );
}

#[test]
fn an_omitted_display_thinking_block_replays_on_its_signature_alone() {
    // `display: "omitted"` is the default on this route's default model: the
    // `thinking` field comes back empty while the signature carries the
    // encrypted thinking. Requiring readable text would reject the API's own
    // default response shape.
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    assert!(
        adapter
            .resolve(
                draft(vec![
                    InferenceInput::Message(ChatMessage::user("go")),
                    state(serde_json::json!([
                        {"type":"thinking","thinking":"","signature":SIGNATURE},
                        tool_use_block("toolu_01")
                    ])),
                    tool_result("toolu_01"),
                ]),
                &model(),
            )
            .is_ok()
    );
}

#[test]
fn a_redacted_thinking_block_replays_only_while_it_keeps_its_opaque_data() {
    // Filtering on `block.type == "thinking"` alone silently drops
    // `redacted_thinking` and breaks the multi-turn protocol, so this route
    // treats both spellings as thinking that must survive replay.
    //
    // The empty-`data` case is already refused one layer down: the shared
    // adapter requires `redacted_thinking.data` to be non-empty. It does *not*
    // apply the same rule to `thinking.signature`, which is exactly the
    // asymmetry this route closes — see the signature case above.
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let history = |data: &str| {
        draft(vec![
            InferenceInput::Message(ChatMessage::user("go")),
            state(serde_json::json!([
                {"type":"redacted_thinking","data":data},
                tool_use_block("toolu_01")
            ])),
            tool_result("toolu_01"),
        ])
    };
    assert!(adapter.resolve(history("EroBCkYIC"), &model()).is_ok());
    assert_refused(adapter.resolve(history(""), &model()), "malformed");
}

#[test]
fn every_step_of_an_interleaved_tool_chain_keeps_its_own_thinking() {
    // With interleaved thinking each response in the turn carries its own
    // thinking block, so the requirement is a per-step property across the
    // whole turn rather than a check on the first step.
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let chain = |second: InferenceInput| {
        draft(vec![
            InferenceInput::Message(ChatMessage::user("go")),
            state(serde_json::json!([
                thinking_block(SIGNATURE),
                tool_use_block("toolu_01")
            ])),
            tool_result("toolu_01"),
            second,
            tool_result("toolu_02"),
        ])
    };
    assert!(
        adapter
            .resolve(
                chain(state(serde_json::json!([
                    thinking_block(SIGNATURE),
                    tool_use_block("toolu_02")
                ]))),
                &model()
            )
            .is_ok()
    );
    assert_refused(
        adapter.resolve(
            chain(state(serde_json::json!([
                thinking_block(""),
                tool_use_block("toolu_02")
            ]))),
            &model(),
        ),
        "lost its opaque continuity field",
    );
    assert_refused(
        adapter.resolve(chain(neutral_tool_call("toolu_02")), &model()),
        "neutral assistant message cannot carry its thinking blocks",
    );
}

#[test]
fn a_finished_turn_may_drop_its_thinking_once_a_new_user_message_opens_the_next() {
    // "Allowed: outside tool use, omit prior turns' thinking." Scoping the rule
    // to the current turn is what keeps a session that ever used a tool from
    // becoming unreplayable, and a tool result continues its turn rather than
    // opening a new one.
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    assert!(
        adapter
            .resolve(
                draft(vec![
                    InferenceInput::Message(ChatMessage::user("first")),
                    neutral_tool_call("toolu_01"),
                    tool_result("toolu_01"),
                    InferenceInput::Message(ChatMessage::assistant("the first answer")),
                    InferenceInput::Message(ChatMessage::user("second")),
                    state(serde_json::json!([
                        thinking_block(SIGNATURE),
                        tool_use_block("toolu_02")
                    ])),
                    tool_result("toolu_02"),
                ]),
                &model(),
            )
            .is_ok()
    );
}

#[tokio::test]
async fn a_streamed_signature_reaches_the_next_request_byte_for_byte() {
    // The end-to-end property this row exists for: what the stream delivered is
    // what the continuation sends. A re-serialized or reconstructed block would
    // still satisfy every shape check above and fail live.
    let transport = RecordingTransport::with_scripts(vec![thinking_tool_script(), Vec::new()]);
    let provider = route(transport.clone());
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            draft(vec![InferenceInput::Message(ChatMessage::user("go"))]),
            &model(),
        )
        .unwrap();
    let mut stream = adapter.stream(call);
    let mut recorded = None;
    while let Some(item) = stream.next().await {
        if let Ok(InferenceEvent::ProviderState(item)) = item {
            recorded = Some(item);
        }
    }
    let recorded = recorded.expect("a completed turn must publish provider state");
    assert_eq!(recorded.kind(), ProviderStateKind::AnthropicMessage);

    let call = adapter
        .resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("go")),
                InferenceInput::ProviderState(recorded),
                tool_result("toolu_01"),
            ]),
            &model(),
        )
        .unwrap();
    let mut stream = adapter.stream(call);
    while stream.next().await.is_some() {}

    let body = transport.last_body();
    let replayed = &body["messages"][1];
    assert_eq!(replayed["role"], serde_json::json!("assistant"));
    assert_eq!(
        replayed["content"][0],
        serde_json::json!({
            "type":"thinking",
            "thinking":"weigh the options",
            "signature":SIGNATURE
        }),
        "the thinking block must go back exactly as it arrived"
    );
    assert_eq!(replayed["content"][1]["id"], serde_json::json!("toolu_01"));
    assert_eq!(
        body["messages"][2]["content"][0]["tool_use_id"],
        serde_json::json!("toolu_01")
    );
}

fn server_message_start(id: &str) -> SseEvent {
    server_message_start_for(id, ANTHROPIC_CLAUDE_OPUS_5)
}

fn server_message_start_for(id: &str, model: &str) -> SseEvent {
    event(
        "message_start",
        serde_json::json!({
            "type":"message_start",
            "message":{
                "id":id,
                "type":"message",
                "role":"assistant",
                "model":model,
                "content":[],
                "stop_reason":serde_json::Value::Null,
                "stop_sequence":serde_json::Value::Null,
                "usage":{"input_tokens":9,"output_tokens":0}
            }
        }),
    )
}

fn pending_search_script() -> Vec<SseEvent> {
    vec![
        server_message_start("msg_search_pause"),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{
                    "type":"server_tool_use",
                    "id":"srvtoolu_exact_search",
                    "name":"web_search",
                    "input":{"query":"current protocol"}
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
                "type":"content_block_start",
                "index":1,
                "content_block":{
                    "type":"text",
                    "text":"Protocol source",
                    "citations":[{
                        "type":"web_search_result_location",
                        "url":"https://example.test/protocol",
                        "title":"Protocol",
                        "cited_text":"bounded excerpt",
                        "encrypted_index":"opaque citation index"
                    }]
                }
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
                "delta":{"stop_reason":"pause_turn","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":4}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn resumed_search_script() -> Vec<SseEvent> {
    vec![
        server_message_start("msg_search_resumed"),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{
                    "type":"web_search_tool_result",
                    "tool_use_id":"srvtoolu_exact_search",
                    "content":[{
                        "type":"web_search_result",
                        "url":"https://example.test/protocol",
                        "title":"Protocol",
                        "encrypted_content":"opaque"
                    }]
                }
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
                "delta":{"stop_reason":"end_turn","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":3}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

#[tokio::test]
async fn provider_server_tool_plan_crosses_messages_normalization_and_exact_pause_replay() {
    let transport =
        RecordingTransport::with_scripts(vec![pending_search_script(), resumed_search_script()]);
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let provider = route(transport.clone()).with_server_tools(plan).unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let descriptor = model();

    let mut first = draft(vec![InferenceInput::Message(ChatMessage::user("search"))]);
    apply_server_tool_selection(
        &provider,
        &mut first,
        &descriptor,
        &[AnthropicServerToolKind::WebSearch],
    )
    .unwrap();
    let mut stream = adapter.stream(adapter.resolve(first, &descriptor).unwrap());
    let mut exact_state = None;
    let mut normalized_call = None;
    let mut normalized_citation = None;
    let mut first_finish = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            InferenceEvent::ServerToolCall { call, .. } => normalized_call = Some(call),
            InferenceEvent::Citation { citation, .. } => normalized_citation = Some(citation),
            InferenceEvent::ProviderState(state) => exact_state = Some(state),
            InferenceEvent::Finish(finish) => first_finish = Some(finish),
            _ => {}
        }
    }
    let normalized_call = normalized_call.expect("configured parser must emit the call");
    assert_eq!(normalized_call.id().as_str(), "srvtoolu_exact_search");
    assert_eq!(normalized_call.logical(), "web_search");
    assert_eq!(first_finish, Some(FinishReason::Pause));
    let exact_state = exact_state.expect("pause must retain exact provider state");
    assert_eq!(
        exact_state.data()["content"][0]["input"]["query"],
        serde_json::json!("current protocol")
    );
    let normalized_citation = normalized_citation.expect("citation must normalize");
    assert_eq!(normalized_citation.url(), "https://example.test/protocol");
    assert_eq!(normalized_citation.cited_text(), Some("bounded excerpt"));
    assert_eq!(
        exact_state.data()["content"][1]["citations"][0]["encrypted_index"],
        serde_json::json!("opaque citation index")
    );

    let mut second = draft(vec![
        InferenceInput::Message(ChatMessage::user("search")),
        InferenceInput::ProviderState(exact_state.clone()),
    ]);
    apply_server_tool_selection(
        &provider,
        &mut second,
        &descriptor,
        &[AnthropicServerToolKind::WebSearch],
    )
    .unwrap();
    let mut stream = adapter.stream(adapter.resolve(second, &descriptor).unwrap());
    let mut normalized_result = None;
    while let Some(item) = stream.next().await {
        if let InferenceEvent::ServerToolResult { result, .. } = item.unwrap() {
            normalized_result = Some(result);
        }
    }
    let normalized_result = normalized_result.expect("resumed result must normalize");
    assert_eq!(
        normalized_result.call_id().as_str(),
        "srvtoolu_exact_search"
    );
    assert_eq!(normalized_result.outcome(), ServerToolOutcome::Success);
    assert_eq!(normalized_result.sources().len(), 1);

    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    let first_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(first_body["tools"].as_array().unwrap().iter().any(|tool| {
        tool["type"] == serde_json::json!("web_search_20250305")
            && tool["max_uses"] == serde_json::json!(5)
    }));
    let second_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(
        second_body["messages"][1]["content"],
        exact_state.data()["content"]
    );
    assert!(
        second_body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["type"] == serde_json::json!("web_search_20250305") })
    );
}

fn mcp_error_script() -> Vec<SseEvent> {
    vec![
        server_message_start("msg_mcp"),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{
                    "type":"mcp_tool_use",
                    "id":"mcptoolu_exact",
                    "name":"lookup",
                    "server_name":"docs",
                    "input":{"query":"private"}
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
                "type":"content_block_start",
                "index":1,
                "content_block":{
                    "type":"mcp_tool_result",
                    "tool_use_id":"mcptoolu_exact",
                    "is_error":true,
                    "content":[{"type":"text","text":"private remote error"}]
                }
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
                "delta":{"stop_reason":"end_turn","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":3}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

#[tokio::test]
async fn provider_mcp_extension_reaches_wire_and_generic_is_error_normalization() {
    let transport = RecordingTransport::with_scripts(vec![mcp_error_script()]);
    let plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::mcp_connector("docs", "https://mcp.example.test/sse")
            .unwrap(),
    ])
    .unwrap();
    let provider = route(transport.clone()).with_server_tools(plan).unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let descriptor = model();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user("use mcp"))]);
    apply_server_tool_selection(
        &provider,
        &mut request,
        &descriptor,
        &[AnthropicServerToolKind::McpConnector],
    )
    .unwrap();
    let mut stream = adapter.stream(adapter.resolve(request, &descriptor).unwrap());
    let mut call = None;
    let mut result = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            InferenceEvent::ServerToolCall { call: value, .. } => call = Some(value),
            InferenceEvent::ServerToolResult { result: value, .. } => result = Some(value),
            _ => {}
        }
    }
    let call = call.expect("MCP call must normalize");
    assert_eq!(call.id().as_str(), "mcptoolu_exact");
    assert_eq!(call.logical(), "remote_mcp");
    assert_eq!(call.provider_name(), "lookup");
    let result = result.expect("MCP result must normalize");
    assert_eq!(result.call_id().as_str(), "mcptoolu_exact");
    assert_eq!(result.outcome(), ServerToolOutcome::Error);
    assert_eq!(result.error_code(), Some("provider_reported_error"));

    let sent = transport.requests();
    let body: serde_json::Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body["mcp_servers"][0]["name"], serde_json::json!("docs"));
    assert!(body["mcp_servers"][0].get("authorization_token").is_none());
    assert_eq!(body["tools"][1].get("name"), None);
    assert_eq!(body["tools"][1]["type"], serde_json::json!("mcp_toolset"));
    assert!(sent[0].headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("anthropic-beta") && value == "mcp-client-2025-11-20"
    }));
}

#[tokio::test]
async fn provider_tool_search_marks_the_exact_offered_companion_deferred_on_wire() {
    let transport = RecordingTransport::with_scripts(vec![Vec::new()]);
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::tool_search_regex()])
            .unwrap()
            .with_deferred_tools(vec!["lookup".to_owned()])
            .unwrap();
    let provider = route(transport.clone()).with_server_tools(plan).unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let descriptor = model();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user(
        "find a tool",
    ))]);
    apply_server_tool_selection(
        &provider,
        &mut request,
        &descriptor,
        &[AnthropicServerToolKind::ToolSearch],
    )
    .unwrap();
    let mut stream = adapter.stream(adapter.resolve(request, &descriptor).unwrap());
    while stream.next().await.is_some() {}

    let body = transport.last_body();
    let lookup = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == serde_json::json!("lookup"))
        .expect("the existing client definition must stay offered");
    assert_eq!(lookup["defer_loading"], serde_json::json!(true));
    let search = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["type"] == serde_json::json!("tool_search_tool_regex_20251119"))
        .expect("the hosted search definition must stay non-deferred");
    assert_eq!(search.get("defer_loading"), None);
}

fn tool_search_result_script() -> Vec<SseEvent> {
    vec![
        server_message_start("msg_tool_search"),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{
                    "type":"server_tool_use",
                    "id":"srvtoolu_search_tools",
                    "name":"tool_search_tool_regex",
                    "input":{"pattern":"search"}
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
                "type":"content_block_start",
                "index":1,
                "content_block":{
                    "type":"tool_search_tool_result",
                    "tool_use_id":"srvtoolu_search_tools",
                    "content":{
                        "type":"tool_search_tool_search_result",
                        "tool_references":[
                            {"type":"tool_reference","tool_name":"lookup"},
                            {"type":"tool_reference","tool_name":"search_files"}
                        ]
                    }
                }
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
                "delta":{"stop_reason":"end_turn","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":3}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

#[tokio::test]
async fn provider_tool_search_normalizer_counts_references_not_the_wrapper_object() {
    let transport = RecordingTransport::with_scripts(vec![tool_search_result_script()]);
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::tool_search_regex()])
            .unwrap()
            .with_deferred_tools(vec!["lookup".to_owned(), "search_files".to_owned()])
            .unwrap();
    let provider = route(transport).with_server_tools(plan).unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user(
        "find tools",
    ))]);
    request.tools.push(ToolSpec {
        name: "search_files".to_owned(),
        description: "Search files.".to_owned(),
        parameters: serde_json::json!({"type":"object","properties":{}}),
    });
    let descriptor = model();
    apply_server_tool_selection(
        &provider,
        &mut request,
        &descriptor,
        &[AnthropicServerToolKind::ToolSearch],
    )
    .unwrap();
    let mut stream = adapter.stream(adapter.resolve(request, &descriptor).unwrap());
    let mut result = None;
    while let Some(item) = stream.next().await {
        if let InferenceEvent::ServerToolResult { result: value, .. } = item.unwrap() {
            result = Some(value);
        }
    }
    assert_eq!(
        result
            .expect("tool search result must normalize")
            .output_count(),
        Some(2)
    );
}

fn web_fetch_result_script(model: &str) -> Vec<SseEvent> {
    vec![
        server_message_start_for("msg_web_fetch", model),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{
                    "type":"server_tool_use",
                    "id":"srvtoolu_fetch",
                    "name":"web_fetch",
                    "input":{"url":"https://example.test/article"}
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
                "type":"content_block_start",
                "index":1,
                "content_block":{
                    "type":"web_fetch_tool_result",
                    "tool_use_id":"srvtoolu_fetch",
                    "content":{
                        "type":"web_fetch_result",
                        "url":"https://example.test/article",
                        "content":{
                            "type":"document",
                            "source":{
                                "type":"text",
                                "media_type":"text/plain",
                                "data":"private article"
                            },
                            "title":"Article",
                            "citations":{"enabled":true}
                        },
                        "retrieved_at":"2026-08-31T00:00:00Z"
                    }
                }
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
                "delta":{"stop_reason":"end_turn","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":3}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

#[tokio::test]
async fn provider_web_fetch_normalizer_retains_the_public_result_source() {
    let model_id = "claude-opus-4-8";
    let transport = RecordingTransport::with_scripts(vec![web_fetch_result_script(model_id)]);
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_fetch()]).unwrap();
    let provider = route(transport).with_server_tools(plan).unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user("fetch"))]);
    request.model = model_id.to_owned();
    let mut descriptor = model();
    descriptor.id = model_id.to_owned();
    descriptor.display_name = "Claude Opus 4.8".to_owned();
    apply_server_tool_selection(
        &provider,
        &mut request,
        &descriptor,
        &[AnthropicServerToolKind::WebFetch],
    )
    .unwrap();
    let mut stream = adapter.stream(adapter.resolve(request, &descriptor).unwrap());
    let mut result = None;
    while let Some(item) = stream.next().await {
        if let InferenceEvent::ServerToolResult { result: value, .. } = item.unwrap() {
            result = Some(value);
        }
    }
    let result = result.expect("web fetch result must normalize");
    assert_eq!(result.output_count(), Some(1));
    assert_eq!(result.sources().len(), 1);
    assert_eq!(result.sources()[0].url(), "https://example.test/article");
    assert_eq!(result.sources()[0].title(), Some("Article"));
    assert!(!format!("{result:?}").contains("private article"));
}

#[tokio::test]
async fn completed_advisor_history_keeps_beta_after_live_tool_selection_is_removed() {
    let transport = RecordingTransport::with_scripts(vec![Vec::new()]);
    let plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::advisor(ANTHROPIC_CLAUDE_OPUS_5, 3).unwrap(),
    ])
    .unwrap();
    let provider = route(transport.clone())
        .with_server_tools(plan)
        .unwrap()
        .without_server_tool_selection();
    assert!(
        Provider::request_options(&provider)
            .iter()
            .all(|option| option.kind() != "server-tools")
    );
    let history = state(serde_json::json!([
        {
            "type":"server_tool_use",
            "id":"srvtoolu_old_advisor",
            "name":"advisor",
            "input":{}
        },
        {
            "type":"advisor_tool_result",
            "tool_use_id":"srvtoolu_old_advisor",
            "content":{
                "type":"advisor_redacted_result",
                "encrypted_content":"opaque historical advisor state"
            }
        }
    ]));
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let request = draft(vec![
        InferenceInput::Message(ChatMessage::user("first")),
        history,
        InferenceInput::Message(ChatMessage::user("continue without advisor")),
    ]);
    let mut stream = adapter.stream(adapter.resolve(request, &model()).unwrap());
    while stream.next().await.is_some() {}

    let sent = transport.requests();
    assert!(sent[0].headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("anthropic-beta") && value == "advisor-tool-2026-03-01"
    }));
    let body: serde_json::Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert!(
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| { tool["type"] != serde_json::json!("advisor_20260301") })
    );
    assert_eq!(
        body["messages"][1]["content"][1]["content"]["encrypted_content"],
        serde_json::json!("opaque historical advisor state")
    );
}

#[test]
fn pending_server_call_cannot_drop_its_exact_live_selection() {
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let provider = dead_route()
        .with_server_tools(plan)
        .unwrap()
        .without_server_tool_selection();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let request = draft(vec![
        InferenceInput::Message(ChatMessage::user("search")),
        state(serde_json::json!([{
            "type":"server_tool_use",
            "id":"srvtoolu_pending",
            "name":"web_search",
            "input":{"query":"current"}
        }])),
    ]);
    assert!(matches!(
        adapter.resolve(request, &model()),
        Err(ResolveError::InvalidRequest {
            field: "provider_state",
            ..
        })
    ));
}

#[test]
fn provider_server_tool_model_gates_preserve_unsupported_and_unknown() {
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_fetch()]).unwrap();
    let provider = dead_route().with_server_tools(plan).unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user("fetch"))]);
    request.native_tool_routes = vec![provider_route(AnthropicServerToolKind::WebFetch)];
    request.native_features = vec![NativeFeature::Web];
    assert!(matches!(
        adapter.resolve(request.clone(), &model()),
        Err(ResolveError::Unsupported {
            capability: heycode_llm::RequestedCapability::Tools,
            ..
        })
    ));

    request.model = "unlisted-model".to_owned();
    let unknown = ModelDescriptor::unknown("unlisted-model");
    assert!(matches!(
        adapter.resolve(request, &unknown),
        Err(ResolveError::Unproven {
            capability: heycode_llm::RequestedCapability::Tools,
            ..
        })
    ));
}

#[tokio::test]
async fn wrapper_owned_context_cache_and_server_tool_options_coexist_fail_loud() {
    let context = AnthropicContextEditingPolicy::new(
        Some(AnthropicThinkingClear::new(AnthropicThinkingKeep::Turns(2)).unwrap()),
        None,
    )
    .unwrap();
    let server_tools =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let transport = RecordingTransport::with_scripts(vec![Vec::new()]);
    let provider = route(transport.clone())
        .with_context_editing(context)
        .unwrap()
        .with_prompt_caching(AnthropicPromptCachePolicy::automatic(
            AnthropicPromptCacheTtl::OneHour,
        ))
        .unwrap()
        .with_server_tools(server_tools)
        .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user("research"))]);
    let mut descriptor = model();
    descriptor.capabilities.prompt_cache = CapabilitySupport::Supported;
    apply_server_tool_selection(
        &provider,
        &mut request,
        &descriptor,
        &[AnthropicServerToolKind::WebSearch],
    )
    .unwrap();
    let call = adapter.resolve(request, &descriptor).unwrap();
    assert_eq!(
        call.provider_options()
            .iter()
            .map(heycode_core::ProviderRequestOption::kind)
            .collect::<Vec<_>>(),
        vec!["context-editing", "prompt-cache", "server-tools"]
    );
    let mut stream = adapter.stream(call);
    while stream.next().await.is_some() {}
    let requests = transport.requests();
    assert_eq!(requests.len(), 1, "declared wrapper options must dispatch");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["context_management"]["edits"][0]["type"],
        serde_json::json!("clear_thinking_20251015")
    );
    assert_eq!(
        body["cache_control"],
        serde_json::json!({"type":"ephemeral","ttl":"1h"})
    );
    assert!(
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["type"] == serde_json::json!("web_search_20250305") })
    );
}

#[tokio::test]
async fn the_manual_route_asks_for_interleaved_thinking_with_a_token_budget() {
    // Manual mode is the only dialect Claude Sonnet 4.5 and the earlier Claude
    // 4 lineup support, and it is the only mode where interleaving needs a beta
    // header.
    let transport = RecordingTransport::with_scripts(vec![Vec::new()]);
    let provider = AnthropicProvider::extended_thinking(
        HttpService::new(transport.clone()),
        "https://api.anthropic.com",
        "test-key",
        Some("claude-sonnet-4-5".to_owned()),
        4_096,
    )
    .unwrap();
    assert_eq!(
        provider.continuation(),
        AnthropicThinkingContinuation::Manual
    );
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user("go"))]);
    request.model = "claude-sonnet-4-5".to_owned();
    let mut manual_model = model();
    manual_model.id = "claude-sonnet-4-5".to_owned();
    let call = adapter.resolve(request, &manual_model).unwrap();
    let mut stream = adapter.stream(call);
    while stream.next().await.is_some() {}

    let sent = transport.requests();
    assert!(sent.iter().any(|request| {
        request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("anthropic-beta") && value == INTERLEAVED_THINKING_BETA
        })
    }));
    let body = transport.last_body();
    assert_eq!(
        body["thinking"],
        serde_json::json!({"type":"enabled","budget_tokens":4_096})
    );
    // Effort is unsupported on most of the manual-only lineup, so this route
    // controls depth with the budget alone.
    assert_eq!(body.get("output_config"), None);
}

#[test]
fn a_manual_route_never_defaults_to_or_explicitly_selects_adaptive_only_opus_5() {
    for model in [None, Some(ANTHROPIC_CLAUDE_OPUS_5.to_owned())] {
        let outcome = AnthropicProvider::extended_thinking(
            HttpService::new(RecordingTransport::with_scripts(Vec::new())),
            "https://api.anthropic.com",
            "test-key",
            model,
            4_096,
        );
        let Err(error) = outcome else {
            panic!("manual thinking must require an explicit compatible model")
        };
        assert!(
            error.to_string().contains("manual thinking model"),
            "{error}"
        );
    }

    let manual = AnthropicProvider::extended_thinking(
        HttpService::new(RecordingTransport::with_scripts(Vec::new())),
        "https://api.anthropic.com",
        "test-key",
        Some("claude-sonnet-4-5".to_owned()),
        4_096,
    )
    .unwrap();
    assert_refused(
        Provider::inference_adapter(&manual).unwrap().resolve(
            draft(vec![InferenceInput::Message(ChatMessage::user("go"))]),
            &model(),
        ),
        "manual thinking model",
    );
}

#[test]
fn manual_thinking_additionally_requires_the_continued_turn_to_begin_with_thinking() {
    // Adaptive mode drops this requirement, so it is enforced only where the
    // API enforces it.
    let manual = AnthropicProvider::extended_thinking(
        HttpService::new(RecordingTransport::with_scripts(Vec::new())),
        "https://api.anthropic.com",
        "test-key",
        Some("claude-sonnet-4-5".to_owned()),
        4_096,
    )
    .unwrap();
    let adaptive = dead_route();
    let text_first = |model_id: &str| {
        let mut request = draft(vec![
            InferenceInput::Message(ChatMessage::user("go")),
            state_for_model(
                model_id,
                serde_json::json!([
                    {"type":"text","text":"let me check"},
                    thinking_block(SIGNATURE),
                    tool_use_block("toolu_01")
                ]),
            ),
            tool_result("toolu_01"),
        ]);
        request.model = model_id.to_owned();
        request
    };
    let mut manual_model = model();
    manual_model.id = "claude-sonnet-4-5".to_owned();
    assert_refused(
        Provider::inference_adapter(&manual)
            .unwrap()
            .resolve(text_first("claude-sonnet-4-5"), &manual_model),
        "begin with a thinking block",
    );
    assert!(
        Provider::inference_adapter(&adaptive)
            .unwrap()
            .resolve(text_first(ANTHROPIC_CLAUDE_OPUS_5), &model())
            .is_ok(),
        "adaptive thinking places no ordering requirement on the turn"
    );
    assert!(
        Provider::inference_adapter(&manual)
            .unwrap()
            .resolve(
                {
                    let mut request = draft(vec![
                        InferenceInput::Message(ChatMessage::user("go")),
                        state_for_model(
                            "claude-sonnet-4-5",
                            serde_json::json!([
                                {"type":"redacted_thinking","data":"EroBCkYIC"},
                                tool_use_block("toolu_01")
                            ]),
                        ),
                        tool_result("toolu_01"),
                    ]);
                    request.model = "claude-sonnet-4-5".to_owned();
                    request
                },
                &manual_model,
            )
            .is_ok(),
        "a safety-redacted thinking block still opens the turn"
    );
}

#[test]
fn a_non_direct_tool_caller_in_state_withholds_pre_output_replay() {
    let provider = dead_route();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            draft(vec![
                InferenceInput::Message(ChatMessage::user("look it up")),
                InferenceInput::ProviderState(
                    ProviderStateItem::new(
                        "anthropic",
                        ANTHROPIC_CLAUDE_OPUS_5,
                        ProviderProtocol::AnthropicMessages,
                        ProviderStateKind::AnthropicMessage,
                        serde_json::json!({
                            "role":"assistant",
                            "container":{"id":"container_exact"},
                            "content":[{
                                "type":"tool_use","id":"toolu_sub","name":"lookup",
                                "input":{"q":"paris"},
                                "caller":{"type":"code_execution","tool_id":"srvtoolu_exec"}
                            }]
                        }),
                    )
                    .unwrap(),
                ),
                tool_result("toolu_sub"),
            ]),
            &model(),
        )
        .unwrap();
    assert!(call.native_features().is_empty());
    assert!(call.native_tool_routes().is_empty());
    assert_eq!(
        call.retry_spec().safety(),
        heycode_llm::RetrySafety::Never,
        "a tool call the provider issued on its own behalf is not replayable"
    );
}

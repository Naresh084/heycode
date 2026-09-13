//! P06 Gemini generateContent protocol conformance.
//!
//! Ordered parts, function calls, thought signatures and `usageMetadata` are
//! the four contracts this file pins, plus the resolve-time refusals that keep
//! an unrepresentable choice away from the transport.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::testing::{
    ConformanceFixtureMetadata, ConformanceSourceKind, SseConformanceFixture, run_sse_conformance,
};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatImage, ChatMessage, ChatToolCall, FinishReason,
    GeminiAdapter, GeminiConfig, GeminiExtensionFault, GeminiProviderOptionPlan,
    GeminiProviderOptionWire, GeminiStreamNormalizer, GeminiStreamNormalizerFactory,
    InferenceAdapter, InferenceEvent, InferenceInput, InputModality, LlmError, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, NativeFeature,
    ProviderDescriptor, ProviderProtocol, ProviderStateItem, ProviderStateKind, ReasoningEffortId,
    RequestDraft, ResolveError, ToolSpec,
};
use tokio_util::sync::CancellationToken;

const BASE_URL: &str = "https://generativelanguage.test/v1beta";

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "gemini-fixture".to_owned(),
        display_name: "Gemini Fixture".to_owned(),
        protocols: vec![ProviderProtocol::GeminiGenerateContent],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "gemini-fixture-model".to_owned(),
        display_name: "Gemini Fixture Model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(65_536),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            image_input: CapabilitySupport::Supported,
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
        provider: "gemini-fixture".to_owned(),
        model: "gemini-fixture-model".to_owned(),
        catalog_revision: Some(4),
        catalog_fetched_at_ms: Some(4_000),
        effective_at_ms: 5_000,
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

fn read_tool() -> ToolSpec {
    ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
    }
}

fn grep_tool() -> ToolSpec {
    ToolSpec {
        name: "grep".to_owned(),
        description: "Search files".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    }
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
    calls: AtomicUsize,
}

struct CodeNormalizerFactory;

impl GeminiStreamNormalizerFactory for CodeNormalizerFactory {
    fn id(&self) -> &'static str {
        "fixture-code"
    }

    fn start(&self) -> Box<dyn GeminiStreamNormalizer> {
        Box::new(CodeNormalizer { pending: None })
    }
}

struct CodeNormalizer {
    pending: Option<heycode_core::CallId>,
}

impl GeminiStreamNormalizer for CodeNormalizer {
    fn accepts_part(&self, field: &str) -> bool {
        matches!(field, "executableCode" | "codeExecutionResult")
    }

    fn observe_part(
        &mut self,
        _response_id: &str,
        output_index: u32,
        part: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        if let Some(code) = part.get("executableCode") {
            let call_id = heycode_core::CallId::from_raw(format!("code-{output_index}"));
            let call = heycode_core::ServerToolCall::new(
                call_id.clone(),
                "code_execution",
                "code_execution",
                code.clone(),
            )
            .map_err(|_| GeminiExtensionFault::InvalidResponse)?;
            self.pending = Some(call_id);
            return Ok(vec![InferenceEvent::ServerToolCall { output_index, call }]);
        }
        if part.get("codeExecutionResult").is_some() {
            let call_id = self
                .pending
                .take()
                .ok_or(GeminiExtensionFault::InvalidResponse)?;
            let result = heycode_core::ServerToolResult::success(call_id, None, Vec::new())
                .map_err(|_| GeminiExtensionFault::InvalidResponse)?;
            return Ok(vec![InferenceEvent::ServerToolResult {
                output_index,
                result,
            }]);
        }
        Ok(Vec::new())
    }

    fn finish(
        &mut self,
        _response_id: &str,
        _next_output_index: u32,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        if self.pending.is_some() {
            Err(GeminiExtensionFault::InvalidResponse)
        } else {
            Ok(Vec::new())
        }
    }
}

impl ScriptedTransport {
    fn new(events: Vec<Result<SseEvent, heycode_http::TransportError>>) -> Self {
        Self {
            events: Mutex::new(Some(events)),
            captured: Mutex::new(None),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn body(&self) -> serde_json::Value {
        let captured = self.captured.lock().unwrap().clone().unwrap();
        serde_json::from_slice(&captured.body).unwrap()
    }
}

impl HttpTransport for ScriptedTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
            self.events.lock().unwrap().take().unwrap_or_default(),
        ))
    }
}

/// Signature-optional route: the Gemini 2.5 family documents echoing a
/// `thoughtSignature` as optional, so this route can exercise ordinary parts,
/// tools and continuations.
fn optional_config() -> GeminiConfig {
    GeminiConfig::with_key(provider(), BASE_URL, "test-key")
        .with_retry_spec(heycode_llm::RetrySpec::no_retry())
        .with_optional_thought_signatures()
        .with_thinking_level(ReasoningEffortId::new("high").unwrap(), "high", true)
        .with_thinking_budget(ReasoningEffortId::new("budgeted").unwrap(), 2_048, false)
}

/// Signature-mandatory route: the Gemini 3 family rejects a continuation whose
/// `functionCall` lost its signature. This is `GeminiConfig`'s default.
fn mandatory_config() -> GeminiConfig {
    GeminiConfig::with_key(provider(), BASE_URL, "test-key")
        .with_retry_spec(heycode_llm::RetrySpec::no_retry())
}

fn code_extension() -> (
    GeminiProviderOptionPlan,
    heycode_core::ProviderRequestOption,
    heycode_core::NativeToolRoute,
) {
    let option = heycode_core::ProviderRequestOption::new(
        "gemini-fixture",
        "google-code-execution",
        serde_json::json!({"tool":{"codeExecution":{}}}),
    )
    .unwrap();
    let route = heycode_core::NativeToolRoute::new(
        "code_execution",
        "gemini-fixture:code",
        heycode_core::NativeToolImplementationKind::Provider,
        Some("gemini-fixture".to_owned()),
    )
    .unwrap();
    let plan = GeminiProviderOptionPlan::new(
        option.clone(),
        GeminiProviderOptionWire::ToolMember {
            member: "tool".to_owned(),
        },
    )
    .unwrap()
    .with_required_route(route.clone())
    .with_normalizer(Arc::new(CodeNormalizerFactory));
    (plan, option, route)
}

fn make_adapter(
    config: GeminiConfig,
    events: Vec<Result<SseEvent, heycode_http::TransportError>>,
) -> (GeminiAdapter, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::new(events));
    let adapter =
        GeminiAdapter::new(config, heycode_http::HttpService::new(transport.clone())).unwrap();
    (adapter, transport)
}

/// Gemini sends bare `data:` payloads with no `event:` name.
fn chunk(value: serde_json::Value) -> Result<SseEvent, heycode_http::TransportError> {
    Ok(SseEvent {
        event: "message".to_owned(),
        data: value.to_string(),
        id: None,
        retry_ms: None,
    })
}

fn text_response(text: &str) -> Vec<Result<SseEvent, heycode_http::TransportError>> {
    vec![chunk(serde_json::json!({
        "candidates":[{
            "content":{"role":"model","parts":[{"text":text}]},
            "finishReason":"STOP",
            "index":0
        }],
        "usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":7,"totalTokenCount":18},
        "modelVersion":"gemini-fixture-model-001",
        "responseId":"resp-1"
    }))]
}

async fn run(
    adapter: &GeminiAdapter,
    draft: RequestDraft,
) -> Vec<Result<InferenceEvent, LlmError>> {
    let call = adapter.resolve(draft, &model()).unwrap();
    adapter.stream(call).collect::<Vec<_>>().await
}

fn ok_events(events: Vec<Result<InferenceEvent, LlmError>>) -> Vec<InferenceEvent> {
    events
        .into_iter()
        .map(|event| event.expect("stream must not fail"))
        .collect()
}

fn stream_error(events: Vec<Result<InferenceEvent, LlmError>>) -> LlmError {
    events
        .into_iter()
        .find_map(Result::err)
        .expect("stream must fail")
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_targets_stream_generate_content_over_sse_with_the_api_key_header() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    run(&adapter, draft()).await;

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert_eq!(
        captured.url,
        "https://generativelanguage.test/v1beta/models/gemini-fixture-model:streamGenerateContent?alt=sse"
    );
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "x-goog-api-key" && value == "test-key")
    );
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "content-type" && value == "application/json")
    );
    assert!(
        !String::from_utf8(captured.body)
            .unwrap()
            .contains("test-key")
    );
}

#[tokio::test]
async fn selected_provider_option_crosses_tools_events_and_lossless_state_only_with_its_route() {
    let (plan, option, route) = code_extension();
    let response = vec![chunk(serde_json::json!({
        "candidates":[{
            "content":{"role":"model","parts":[
                {"executableCode":{"language":"PYTHON","code":"print(2 + 2)"}},
                {"codeExecutionResult":{"outcome":"OUTCOME_OK","output":"4"}},
                {"text":"4"}
            ]},
            "finishReason":"STOP",
            "index":0
        }],
        "usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":3},
        "responseId":"resp-code"
    }))];
    let (adapter, transport) =
        make_adapter(optional_config().with_provider_option_plan(plan), response);
    let mut request = draft();
    request.provider_options = vec![option.clone()];
    let missing_route = adapter.resolve(request.clone(), &model()).unwrap_err();
    assert!(matches!(
        missing_route,
        ResolveError::InvalidRequest {
            field: "native_tool_routes",
            ..
        }
    ));
    request.native_tool_routes = vec![route];
    let events = ok_events(run(&adapter, request).await);
    assert_eq!(transport.body()["tools"][0], option.data()["tool"]);
    assert!(events.iter().any(|event| matches!(event, InferenceEvent::ServerToolCall { call, .. } if call.logical() == "code_execution")));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, InferenceEvent::ServerToolResult { .. }))
    );
    let state = published_state(&events);
    assert_eq!(
        state.data()["parts"][0]["executableCode"]["language"],
        "PYTHON"
    );
    assert_eq!(
        state.data()["parts"][1]["codeExecutionResult"]["output"],
        "4"
    );
}

#[tokio::test]
async fn selected_extension_parts_replay_byte_semantically_on_the_next_turn() {
    let (plan, option, route) = code_extension();
    let first_response = vec![chunk(serde_json::json!({
        "candidates":[{
            "content":{"role":"model","parts":[
                {"executableCode":{"language":"PYTHON","code":"print(7)","id":"code-a"}},
                {"codeExecutionResult":{"outcome":"OUTCOME_OK","output":"7","id":"code-a"}}
            ]},
            "finishReason":"STOP","index":0
        }],
        "usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":2},
        "responseId":"resp-code-replay"
    }))];
    let (first, _) = make_adapter(
        optional_config().with_provider_option_plan(plan),
        first_response,
    );
    let mut first_request = draft();
    first_request.provider_options = vec![option];
    first_request.native_tool_routes = vec![route];
    let first_events = ok_events(run(&first, first_request).await);
    let state = published_state(&first_events).clone();

    let (plan, option, route) = code_extension();
    let (second, transport) = make_adapter(
        optional_config().with_provider_option_plan(plan),
        text_response("continued"),
    );
    let mut second_request = draft();
    second_request.inputs = vec![
        InferenceInput::ProviderState(state.clone()),
        InferenceInput::Message(ChatMessage::user("continue")),
    ];
    second_request.provider_options = vec![option];
    second_request.native_tool_routes = vec![route];
    ok_events(run(&second, second_request).await);

    assert_eq!(transport.body()["contents"][0], *state.data());
}

#[tokio::test]
async fn bearer_auth_wire_replaces_the_api_key_header() {
    let (adapter, transport) = make_adapter(
        GeminiConfig::with_bearer_token(provider(), BASE_URL, "test-key")
            .with_retry_spec(heycode_llm::RetrySpec::no_retry())
            .with_optional_thought_signatures(),
        text_response("ok"),
    );
    run(&adapter, draft()).await;

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "authorization" && value == "Bearer test-key")
    );
    assert!(
        !captured
            .headers
            .iter()
            .any(|(name, _)| name == "x-goog-api-key")
    );
}

#[tokio::test]
async fn system_prompt_serializes_as_the_top_level_system_instruction() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    run(&adapter, draft()).await;

    let body = transport.body();
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        "Follow the repository law."
    );
    assert_eq!(body["contents"].as_array().unwrap().len(), 1);
    assert_eq!(body["contents"][0]["role"], "user");
}

#[tokio::test]
async fn neutral_assistant_turns_serialize_as_the_model_content_role() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("first")),
        InferenceInput::Message(ChatMessage::assistant("prior answer")),
        InferenceInput::Message(ChatMessage::user("next")),
    ];
    run(&adapter, request).await;

    let contents = transport.body()["contents"].clone();
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[1]["parts"][0]["text"], "prior answer");
    assert_eq!(contents[2]["role"], "user");
}

#[tokio::test]
async fn tools_serialize_as_function_declarations_with_an_auto_tool_config() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    let mut request = draft();
    request.tools = vec![read_tool(), grep_tool()];
    run(&adapter, request).await;

    let body = transport.body();
    let declarations = body["tools"][0]["functionDeclarations"].clone();
    assert_eq!(declarations[0]["name"], "read");
    assert_eq!(declarations[0]["description"], "Read a file");
    assert_eq!(declarations[0]["parametersJsonSchema"]["type"], "object");
    assert_eq!(declarations[1]["name"], "grep");
    assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
}

#[tokio::test]
async fn parallel_function_results_coalesce_into_one_user_content_in_call_order() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    let mut request = draft();
    request.tools = vec![read_tool(), grep_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::Message(ChatMessage::assistant_with_tool_calls(
            "",
            vec![
                ChatToolCall {
                    id: "call-read".to_owned(),
                    name: "read".to_owned(),
                    arguments: "{\"path\":\"a\"}".to_owned(),
                },
                ChatToolCall {
                    id: "call-grep".to_owned(),
                    name: "grep".to_owned(),
                    arguments: "{}".to_owned(),
                },
            ],
        )),
        InferenceInput::Message(ChatMessage::tool("call-read", "file body")),
        InferenceInput::Message(ChatMessage::tool("call-grep", "no match")),
    ];
    run(&adapter, request).await;

    let contents = transport.body()["contents"].clone();
    assert_eq!(contents.as_array().unwrap().len(), 3);
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[1]["parts"][0]["functionCall"]["id"], "call-read");
    assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "read");
    assert_eq!(contents[1]["parts"][0]["functionCall"]["args"]["path"], "a");
    assert_eq!(contents[1]["parts"][1]["functionCall"]["id"], "call-grep");

    assert_eq!(contents[2]["role"], "user");
    let responses = contents[2]["parts"].as_array().unwrap();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["functionResponse"]["id"], "call-read");
    assert_eq!(responses[0]["functionResponse"]["name"], "read");
    assert_eq!(
        responses[0]["functionResponse"]["response"]["result"],
        "file body"
    );
    assert_eq!(responses[1]["functionResponse"]["id"], "call-grep");
    assert_eq!(responses[1]["functionResponse"]["name"], "grep");
}

#[tokio::test]
async fn a_failed_tool_result_keeps_its_durable_error_outcome() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    let mut request = draft();
    request.tools = vec![read_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::Message(ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "call-read".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            }],
        )),
        InferenceInput::Message(ChatMessage::tool_result("call-read", "denied", true)),
    ];
    run(&adapter, request).await;

    let response =
        transport.body()["contents"][2]["parts"][0]["functionResponse"]["response"].clone();
    assert_eq!(response["error"], "denied");
    assert!(response.get("result").is_none());
}

#[tokio::test]
async fn reasoning_effort_serializes_its_exact_thinking_config() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    let mut request = draft();
    request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    run(&adapter, request).await;

    let thinking = transport.body()["generationConfig"]["thinkingConfig"].clone();
    assert_eq!(thinking["thinkingLevel"], "high");
    assert_eq!(thinking["includeThoughts"], true);
    assert!(thinking.get("thinkingBudget").is_none());
}

#[tokio::test]
async fn budget_reasoning_effort_serializes_a_thinking_budget_instead_of_a_level() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    let mut request = draft();
    request.reasoning_effort = Some(ReasoningEffortId::new("budgeted").unwrap());
    run(&adapter, request).await;

    let thinking = transport.body()["generationConfig"]["thinkingConfig"].clone();
    assert_eq!(thinking["thinkingBudget"], 2_048);
    assert_eq!(thinking["includeThoughts"], false);
    assert!(thinking.get("thinkingLevel").is_none());
}

#[tokio::test]
async fn output_cap_and_temperature_serialize_into_generation_config() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    let mut request = draft();
    request.max_output_tokens = Some(1_024);
    request.temperature = Some(0.25);
    run(&adapter, request).await;

    let config = transport.body()["generationConfig"].clone();
    assert_eq!(config["maxOutputTokens"], 1_024);
    assert_eq!(config["temperature"], 0.25);
}

#[tokio::test]
async fn a_request_without_generation_settings_omits_generation_config_entirely() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("ok"));
    run(&adapter, draft()).await;

    assert!(transport.body().get("generationConfig").is_none());
    assert!(transport.body().get("tools").is_none());
    assert!(transport.body().get("toolConfig").is_none());
}

// ---------------------------------------------------------------------------
// Resolve-time refusals — every one of these must precede transport
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_signature_mandatory_route_refuses_a_function_call_continuation_before_transport() {
    let (adapter, transport) = make_adapter(mandatory_config(), Vec::new());
    let mut request = draft();
    request.tools = vec![read_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::Message(ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "call-read".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            }],
        )),
        InferenceInput::Message(ChatMessage::tool("call-read", "body")),
    ];

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, message }
            if *field == "provider_state" && message.contains("thoughtSignature")),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0, "resolve must precede transport");
}

#[tokio::test]
async fn a_signature_mandatory_route_still_offers_tools_on_a_first_turn() {
    let (adapter, transport) = make_adapter(
        mandatory_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{}},
                     "thoughtSignature":"opaque-signature"}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-first-turn"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool()];

    let events = ok_events(run(&adapter, request).await);

    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::ToolCalls))
    );
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn a_signature_optional_route_accepts_a_function_call_continuation() {
    let (adapter, _transport) = make_adapter(optional_config(), text_response("done"));
    let mut request = draft();
    request.tools = vec![read_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::Message(ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "call-read".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            }],
        )),
        InferenceInput::Message(ChatMessage::tool("call-read", "body")),
    ];

    assert!(adapter.resolve(request, &model()).is_ok());
}

#[tokio::test]
async fn a_non_gemini_provider_state_is_refused_on_a_gemini_route() {
    // The C05 desync invariant, and the reason protocol/kind is a pair rather
    // than a flag: another route's model turn is not a Gemini `Content`, and
    // serializing it would put an alien block in `contents`.
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request.inputs.push(InferenceInput::ProviderState(
        ProviderStateItem::new(
            "gemini-fixture",
            "gemini-fixture-model",
            ProviderProtocol::AnthropicMessages,
            ProviderStateKind::AnthropicMessage,
            serde_json::json!({"role":"assistant","content":[]}),
        )
        .unwrap(),
    ));

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, message }
            if *field == "provider_state" && message.contains("non-Gemini provider state")),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn structured_output_is_refused_until_its_dialect_is_configured() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request.structured_output = Some(serde_json::json!({"type":"object"}));

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. }
            if *field == "structured_output"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn native_features_are_refused_until_their_dialects_are_configured() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request.native_features = vec![NativeFeature::Web];

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. }
            if *field == "native_features"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

// ---------------------------------------------------------------------------
// PGCP04 — image input as inline data
//
// <https://generativelanguage.googleapis.com/$discovery/rest?version=v1> —
// `Part.inlineData` is a `Blob` of `mimeType` plus base64 `data`.
// ---------------------------------------------------------------------------

fn image(mime: &str, bytes: Vec<u8>) -> ChatImage {
    ChatImage::new(heycode_core::AttachmentMediaType::new(mime).unwrap(), bytes).unwrap()
}

fn image_draft(message: ChatMessage) -> RequestDraft {
    let mut request = draft();
    request.input_modalities = vec![InputModality::Text, InputModality::Image];
    request.inputs = vec![InferenceInput::Message(message)];
    request
}

#[tokio::test]
async fn a_png_image_serializes_as_inline_data_after_the_text_prompt() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("a cat"));

    run(
        &adapter,
        image_draft(ChatMessage::user_with_images(
            "describe",
            vec![image("image/png", vec![1, 2, 3])],
        )),
    )
    .await;

    // "When using a single image with text, place the text prompt before the
    // image in the input array."
    // <https://ai.google.dev/gemini-api/docs/image-understanding>
    // Anthropic orders these the other way round; this follows Gemini's own
    // guidance rather than the house precedent.
    assert_eq!(
        transport.body()["contents"][0]["parts"],
        serde_json::json!([
            {"text":"describe"},
            {"inlineData":{"mimeType":"image/png","data":"AQID"}}
        ])
    );
}

#[tokio::test]
async fn an_image_only_message_sends_no_empty_text_part() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("a cat"));

    run(
        &adapter,
        image_draft(ChatMessage::user_with_images(
            "",
            vec![image("image/jpeg", vec![255, 216, 255])],
        )),
    )
    .await;

    assert_eq!(
        transport.body()["contents"][0]["parts"],
        serde_json::json!([{"inlineData":{"mimeType":"image/jpeg","data":"/9j/"}}])
    );
}

#[tokio::test]
async fn several_images_keep_their_order_after_the_prompt() {
    let (adapter, transport) = make_adapter(optional_config(), text_response("two cats"));

    run(
        &adapter,
        image_draft(ChatMessage::user_with_images(
            "compare",
            vec![
                image("image/png", vec![1, 2, 3]),
                image("image/webp", vec![4, 5, 6]),
            ],
        )),
    )
    .await;

    let parts = transport.body()["contents"][0]["parts"].clone();
    assert_eq!(parts[0], serde_json::json!({"text":"compare"}));
    assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
    assert_eq!(parts[2]["inlineData"]["mimeType"], "image/webp");
}

#[tokio::test]
async fn a_gif_is_refused_before_transport_because_gemini_does_not_document_it() {
    // `ChatImage` admits GIF because other providers document it. Of Google's
    // three current sources only the discovery document's cross-modality
    // example list mentions it for Gemini, and the image guide's own list
    // omits it, so it is refused rather than sent on the weakest evidence.
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());

    let error = adapter
        .resolve(
            image_draft(ChatMessage::user_with_images(
                "describe",
                vec![image("image/gif", vec![71, 73, 70])],
            )),
            &model(),
        )
        .unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, message }
            if *field == "input_modalities" && message.contains("PNG, JPEG and WebP")),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn an_inline_request_over_the_documented_limit_is_refused_before_transport() {
    // "Inline image data limits your total request size (text prompts, system
    // instructions, and inline bytes) to 20MB."
    // <https://ai.google.dev/gemini-api/docs/image-understanding>
    // The bound is the whole serialized payload, so base64 expansion counts.
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let oversized = image("image/png", vec![7; 16 * 1024 * 1024]);

    let events = run(
        &adapter,
        image_draft(ChatMessage::user_with_images("describe", vec![oversized])),
    )
    .await;

    assert!(matches!(stream_error(events), LlmError::InvalidResponse(_)));
    assert_eq!(transport.calls(), 0, "the cap must precede transport");
}

#[tokio::test]
async fn a_document_modality_is_refused_until_its_dialect_is_configured() {
    // PDF rides the same `inlineData` part, but its size cap and page
    // semantics are a separate contract nothing has proven for this route.
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let document = heycode_llm::ChatDocument::new(
        heycode_core::AttachmentMediaType::new("application/pdf").unwrap(),
        "report.pdf".to_owned(),
        b"%PDF-1.7\n".to_vec(),
    )
    .unwrap();
    let mut capable = model();
    capable.capabilities.document_input = CapabilitySupport::Supported;
    let mut request = draft();
    request.input_modalities = vec![InputModality::Text, InputModality::Document];
    request.inputs = vec![InferenceInput::Message(ChatMessage::user_with_media(
        "summarize",
        Vec::new(),
        vec![document],
    ))];

    let error = adapter.resolve(request, &capable).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, message }
            if *field == "input_modalities" && message.contains("document input dialect")),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn a_system_role_input_is_refused_because_gemini_owns_a_system_slot() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request
        .inputs
        .insert(0, InferenceInput::Message(ChatMessage::system("inline")));

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. } if *field == "inputs"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn replaying_an_unadvertised_function_call_is_refused() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request.tools = vec![read_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::Message(ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "call-x".to_owned(),
                name: "delete".to_owned(),
                arguments: "{}".to_owned(),
            }],
        )),
        InferenceInput::Message(ChatMessage::tool("call-x", "done")),
    ];

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. } if *field == "tools"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn a_function_response_without_its_preceding_call_is_refused() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::Message(ChatMessage::tool("call-orphan", "body")),
    ];

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. } if *field == "inputs"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn a_function_call_without_its_immediate_response_is_refused() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request.tools = vec![read_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        InferenceInput::Message(ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "call-read".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            }],
        )),
        InferenceInput::Message(ChatMessage::user("never mind")),
    ];

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. } if *field == "inputs"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn a_model_id_that_is_not_a_bare_path_segment_is_refused() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut descriptor = model();
    descriptor.id = "models/gemini:generateContent?x=1".to_owned();
    let mut request = draft();
    request.model.clone_from(&descriptor.id);

    let error = adapter.resolve(request, &descriptor).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. } if *field == "model"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn provider_options_are_refused_because_no_gemini_dialect_is_configured() {
    let (adapter, transport) = make_adapter(optional_config(), Vec::new());
    let mut request = draft();
    request.provider_options = vec![
        heycode_core::ProviderRequestOption::new(
            "gemini-fixture",
            "routing",
            serde_json::json!({"order":["a"]}),
        )
        .unwrap(),
    ];

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. }
            if *field == "provider_options"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

// ---------------------------------------------------------------------------
// Response normalization — parts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ordered_parts_normalize_into_ordered_items_that_preserve_part_order() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"text":"planning","thought":true},
                    {"text":"answer","thought":false}
                ]},
                "finishReason":"STOP"
            }],
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":2},
            "responseId":"resp-order"
        }))],
    );

    let events = ok_events(run(&adapter, draft()).await);

    assert_eq!(
        events,
        vec![
            InferenceEvent::ResponseStarted {
                response_id: "resp-order".to_owned()
            },
            InferenceEvent::ItemStarted {
                output_index: 0,
                item_id: "resp-order/parts/0".to_owned(),
                kind: heycode_llm::StreamItemKind::Reasoning,
            },
            InferenceEvent::ReasoningDelta("planning".to_owned()),
            InferenceEvent::ItemFinished {
                output_index: 0,
                item_id: "resp-order/parts/0".to_owned(),
                kind: heycode_llm::StreamItemKind::Reasoning,
            },
            InferenceEvent::ItemStarted {
                output_index: 1,
                item_id: "resp-order/parts/1".to_owned(),
                kind: heycode_llm::StreamItemKind::Message,
            },
            InferenceEvent::TextDelta("answer".to_owned()),
            InferenceEvent::ItemFinished {
                output_index: 1,
                item_id: "resp-order/parts/1".to_owned(),
                kind: heycode_llm::StreamItemKind::Message,
            },
            // The verbatim model turn is published once, after the last item
            // closes and before the response settles.
            InferenceEvent::ProviderState(
                ProviderStateItem::new(
                    "gemini-fixture",
                    "gemini-fixture-model",
                    ProviderProtocol::GeminiGenerateContent,
                    ProviderStateKind::GeminiModelContent,
                    serde_json::json!({"role":"model","parts":[
                        {"text":"planning","thought":true},
                        {"text":"answer","thought":false}
                    ]}),
                )
                .unwrap()
            ),
            InferenceEvent::ResponseFinished {
                response_id: "resp-order".to_owned(),
                status: "STOP".to_owned(),
            },
            InferenceEvent::Usage(heycode_llm::TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 2,
            }),
            InferenceEvent::Finish(FinishReason::Stop),
        ]
    );
}

#[tokio::test]
async fn a_text_run_split_across_chunks_stays_one_normalized_item() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![
            chunk(serde_json::json!({
                "candidates":[{"content":{"role":"model","parts":[{"text":"He"}]}}],
                "responseId":"resp-run"
            })),
            chunk(serde_json::json!({
                "candidates":[{
                    "content":{"role":"model","parts":[{"text":"llo"}]},
                    "finishReason":"STOP"
                }],
                "responseId":"resp-run"
            })),
        ],
    );

    let events = ok_events(run(&adapter, draft()).await);
    let started = events
        .iter()
        .filter(|event| matches!(event, InferenceEvent::ItemStarted { .. }))
        .count();

    assert_eq!(started, 1, "one logical text item, got {events:#?}");
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                InferenceEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["He", "llo"]
    );
}

#[tokio::test]
async fn parallel_function_calls_normalize_as_distinct_ordered_items() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{"path":"a"}},
                     "thoughtSignature":"opaque-signature"},
                    {"functionCall":{"id":"fc-2","name":"grep","args":{"q":"b"}}}
                ]},
                "finishReason":"STOP"
            }],
            "usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":9},
            "responseId":"resp-parallel"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool(), grep_tool()];

    let events = ok_events(run(&adapter, request).await);
    let calls = events
        .iter()
        .filter_map(|event| match event {
            InferenceEvent::ToolCallDelta {
                output_index,
                id,
                name,
                arguments_delta,
            } => Some((
                *output_index,
                id.as_ref()
                    .map(heycode_core::CallId::as_str)
                    .unwrap_or_default(),
                name.as_deref().unwrap_or_default(),
                arguments_delta.as_str(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        calls,
        vec![
            (0, "fc-1", "read", "{\"path\":\"a\"}"),
            (1, "fc-2", "grep", "{\"q\":\"b\"}"),
        ]
    );
    assert!(matches!(
        events.last(),
        Some(InferenceEvent::Finish(FinishReason::ToolCalls))
    ));
}

#[tokio::test]
async fn a_function_call_without_args_normalizes_to_an_empty_object() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"grep"}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-noargs"
        }))],
    );
    let mut request = draft();
    request.tools = vec![grep_tool()];

    let events = ok_events(run(&adapter, request).await);

    assert!(events.iter().any(|event| matches!(
        event,
        InferenceEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta == "{}"
    )));
}

#[tokio::test]
async fn a_stop_finish_reason_with_function_calls_normalizes_to_tool_calls() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{}}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-tool-stop"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool()];

    let events = ok_events(run(&adapter, request).await);

    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::ToolCalls))
    );
}

#[tokio::test]
async fn a_max_tokens_finish_reason_normalizes_to_length() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"text":"partial"}]},
                "finishReason":"MAX_TOKENS"
            }],
            "responseId":"resp-length"
        }))],
    );

    let events = ok_events(run(&adapter, draft()).await);

    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::Length))
    );
}

#[tokio::test]
async fn a_safety_finish_reason_fails_with_a_bounded_code_and_no_provider_prose() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"text":""}]},
                "finishReason":"SAFETY",
                "safetyRatings":[{"category":"HARM_CATEGORY_DANGEROUS_CONTENT"}]
            }],
            "responseId":"resp-safety"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert_eq!(
        error.class(),
        heycode_llm::ProviderErrorClass::InvalidRequest
    );
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains("HARM_CATEGORY_DANGEROUS_CONTENT"),
        "provider prose leaked: {rendered}"
    );
}

#[tokio::test]
async fn a_response_that_never_finishes_fails_at_eof() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{"content":{"role":"model","parts":[{"text":"truncated"}]}}],
            "responseId":"resp-eof"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn a_response_calling_an_unadvertised_function_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"delete","args":{}}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-unadvertised"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool()];

    let error = stream_error(run(&adapter, request).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn a_repeated_function_call_id_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{}}},
                    {"functionCall":{"id":"fc-1","name":"read","args":{}}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-dup"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool()];

    let error = stream_error(run(&adapter, request).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn a_code_execution_part_fails_instead_of_silently_dropping_output() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"executableCode":{"language":"PYTHON","code":"print(1)"}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-code"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(
        matches!(&error, LlmError::InvalidResponse(message)
            if message.contains("no configured normalizer")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_input_only_part_in_a_response_fails_instead_of_being_treated_as_output() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"inlineData":{"mimeType":"image/png","data":"AQID"}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-input-only"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(
        matches!(&error, LlmError::InvalidResponse(message)
            if message.contains("input-only")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_part_setting_two_content_fields_fails_instead_of_dropping_one() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"text":"a","functionCall":{"id":"fc-1","name":"read","args":{}}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-two-fields"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool()];

    let error = stream_error(run(&adapter, request).await);

    assert!(
        matches!(&error, LlmError::InvalidResponse(message)
            if message.contains("more than one content field")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unrecognized_part_fails_instead_of_silently_dropping_output() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"somethingNew":{"value":1}}]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-unknown"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(
        matches!(&error, LlmError::InvalidResponse(message)
            if message.contains("no recognized content")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_multi_candidate_response_fails_rather_than_silently_choosing_one() {
    // Only the first candidate carries `finishReason` and neither carries a
    // nonzero `index`, so nothing but the candidate-count rule can reject this
    // chunk.
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[
                {"content":{"role":"model","parts":[{"text":"a"}]},"finishReason":"STOP","index":0},
                {"content":{"role":"model","parts":[{"text":"b"}]}}
            ],
            "responseId":"resp-multi"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

// ---------------------------------------------------------------------------
// Thought signatures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_mandatory_route_rejects_a_first_function_call_with_no_thought_signature() {
    let (adapter, _transport) = make_adapter(
        mandatory_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{}}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-unsigned-call"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool()];

    let error = stream_error(run(&adapter, request).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn a_mandatory_route_needs_a_signature_only_on_the_first_parallel_function_call() {
    let (adapter, _transport) = make_adapter(
        mandatory_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{}},
                     "thoughtSignature":"opaque-signature"},
                    {"functionCall":{"id":"fc-2","name":"grep","args":{}}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-parallel-signed"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool(), grep_tool()];

    let events = ok_events(run(&adapter, request).await);

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, InferenceEvent::ToolCallDelta { .. }))
            .count(),
        2,
        "{events:#?}"
    );
}

#[tokio::test]
async fn a_mandatory_route_accepts_a_text_response_with_no_thought_signature() {
    let (adapter, _transport) = make_adapter(mandatory_config(), text_response("answer"));

    let events = ok_events(run(&adapter, draft()).await);

    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::Stop))
    );
}

#[tokio::test]
async fn a_signature_only_part_is_well_formed_and_emits_no_output() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"text":"answer"},
                    {"thoughtSignature":"opaque-signature"}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-signature-only"
        }))],
    );

    let events = ok_events(run(&adapter, draft()).await);

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, InferenceEvent::ItemStarted { .. }))
            .count(),
        1,
        "a bare signature part is not its own item: {events:#?}"
    );
}

#[tokio::test]
async fn a_non_string_thought_signature_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"text":"a","thoughtSignature":42}]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-bad-signature"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

// ---------------------------------------------------------------------------
// PGCP03 — thought signatures survive as durable provider state
//
// The documented requirement is that a signature returns "in the exact part
// where it was received", and that "the first `functionCall` part in each step
// of the current turn must include its `thought_signature`" or the request
// fails with a 400.
// <https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures>
// ---------------------------------------------------------------------------

/// The one provider-state item a Gemini response publishes.
fn published_state(events: &[InferenceEvent]) -> &ProviderStateItem {
    let mut found = events.iter().filter_map(|event| match event {
        InferenceEvent::ProviderState(item) => Some(item),
        _ => None,
    });
    let item = found.next().expect("response published no provider state");
    assert!(
        found.next().is_none(),
        "one response is one model turn, so it publishes one state"
    );
    item
}

fn signed_call_response(signature: &str) -> Vec<Result<SseEvent, heycode_http::TransportError>> {
    vec![chunk(serde_json::json!({
        "candidates":[{
            "content":{"role":"model","parts":[
                {"functionCall":{"id":"fc-1","name":"read","args":{"path":"a"}},
                 "thoughtSignature":signature}
            ]},
            "finishReason":"STOP"
        }],
        "responseId":"resp-signed-call"
    }))]
}

#[tokio::test]
async fn a_function_call_turn_publishes_its_signature_in_the_exact_part_that_carried_it() {
    let (adapter, _transport) =
        make_adapter(mandatory_config(), signed_call_response("Signature-A"));
    let mut request = draft();
    request.tools = vec![read_tool()];

    let events = ok_events(run(&adapter, request).await);
    let state = published_state(&events);

    assert_eq!(state.protocol(), ProviderProtocol::GeminiGenerateContent);
    assert_eq!(state.kind(), ProviderStateKind::GeminiModelContent);
    // Byte-for-byte, including the signature's position beside its own call.
    assert_eq!(
        state.data(),
        &serde_json::json!({
            "role":"model",
            "parts":[
                {"functionCall":{"id":"fc-1","name":"read","args":{"path":"a"}},
                 "thoughtSignature":"Signature-A"}
            ]
        })
    );
}

#[tokio::test]
async fn a_signature_on_a_text_part_survives_into_provider_state() {
    // The documented streaming shape for a turn with no function call: "the
    // model may return the thought signature in a part with an empty text
    // content part". Normalization takes the text branch for such a part, so
    // only the verbatim record keeps the signature.
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"text":"answer"},
                    {"text":"","thoughtSignature":"Signature-Tail"}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-text-signature"
        }))],
    );

    let events = ok_events(run(&adapter, draft()).await);
    let state = published_state(&events);

    assert_eq!(
        state.data()["parts"],
        serde_json::json!([
            {"text":"answer"},
            {"text":"","thoughtSignature":"Signature-Tail"}
        ])
    );
}

#[tokio::test]
async fn parallel_calls_keep_the_signature_on_the_first_part_and_grow_none_on_the_others() {
    // "the thought_signature is attached only to the first functionCall part.
    // Subsequent functionCall parts in the same response will not contain a
    // signature." Inventing one for the second call would be fabricating
    // provider evidence.
    let (adapter, _transport) = make_adapter(
        mandatory_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{}},
                     "thoughtSignature":"Signature-A"},
                    {"functionCall":{"id":"fc-2","name":"grep","args":{}}}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-parallel-state"
        }))],
    );
    let mut request = draft();
    request.tools = vec![read_tool(), grep_tool()];

    let events = ok_events(run(&adapter, request).await);
    let parts = published_state(&events).data()["parts"].clone();

    assert_eq!(
        parts[0]["thoughtSignature"],
        serde_json::json!("Signature-A")
    );
    assert_eq!(parts[1].get("thoughtSignature"), None);
}

#[tokio::test]
async fn a_streamed_turn_publishes_every_chunk_s_parts_in_arrival_order() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![
            chunk(serde_json::json!({
                "candidates":[{"content":{"role":"model","parts":[{"text":"He"}]}}],
                "responseId":"resp-stream-state"
            })),
            chunk(serde_json::json!({
                "candidates":[{
                    "content":{"role":"model","parts":[
                        {"text":"llo","thoughtSignature":"Signature-Tail"}
                    ]},
                    "finishReason":"STOP"
                }],
                "responseId":"resp-stream-state"
            })),
        ],
    );

    let events = ok_events(run(&adapter, draft()).await);

    // Parts are not merged: a signature belongs to the part that carried it,
    // and coalescing two text parts would move it.
    assert_eq!(
        published_state(&events).data()["parts"],
        serde_json::json!([
            {"text":"He"},
            {"text":"llo","thoughtSignature":"Signature-Tail"}
        ])
    );
}

#[tokio::test]
async fn a_response_that_produced_no_part_publishes_no_provider_state() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{"finishReason":"STOP"}],
            "responseId":"resp-empty-turn"
        }))],
    );

    let events = ok_events(run(&adapter, draft()).await);

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, InferenceEvent::ProviderState(_))),
        "an empty model turn must not publish a content block that says nothing: {events:#?}"
    );
}

#[tokio::test]
async fn a_response_that_failed_validation_publishes_no_model_turn() {
    // The first chunk is well formed, so a parser that published state as it
    // went would already have emitted a turn by the time the second chunk
    // fails. State belongs to the settled response, not to a chunk.
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![
            chunk(serde_json::json!({
                "candidates":[{"content":{"role":"model","parts":[{"text":"kept"}]}}],
                "responseId":"resp-bad-part"
            })),
            chunk(serde_json::json!({
                "candidates":[{
                    "content":{"role":"model","parts":[{"text":"a","thoughtSignature":42}]},
                    "finishReason":"STOP"
                }],
                "responseId":"resp-bad-part"
            })),
        ],
    );

    let events = run(&adapter, draft()).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::ProviderState(_)))),
        "a failed response must publish no state: the parser goes terminal, so \
         the turn is never settled and no partial content reaches the session"
    );
    assert!(matches!(stream_error(events), LlmError::InvalidResponse(_)));
}

/// The model turn a continuation replays, as the adapter published it.
fn replayed_state(parts: serde_json::Value) -> InferenceInput {
    InferenceInput::ProviderState(
        ProviderStateItem::new(
            "gemini-fixture",
            "gemini-fixture-model",
            ProviderProtocol::GeminiGenerateContent,
            ProviderStateKind::GeminiModelContent,
            serde_json::json!({"role":"model","parts":parts}),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn replaying_a_model_turn_sends_its_content_byte_for_byte() {
    let (adapter, transport) = make_adapter(mandatory_config(), text_response("done"));
    let parts = serde_json::json!([
        {"functionCall":{"id":"fc-1","name":"read","args":{"path":"a"}},
         "thoughtSignature":"Signature-A"}
    ]);
    let mut request = draft();
    request.tools = vec![read_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        replayed_state(parts.clone()),
        InferenceInput::Message(ChatMessage::tool("fc-1", "body")),
    ];

    run(&adapter, request).await;

    let contents = transport.body()["contents"].clone();
    assert_eq!(
        contents[1],
        serde_json::json!({"role":"model","parts":parts})
    );
    // And the tool result still finds its call name, which only the replayed
    // model turn can supply once it has replaced the neutral assistant copy.
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["name"],
        serde_json::json!("read")
    );
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["id"],
        serde_json::json!("fc-1")
    );
}

#[tokio::test]
async fn a_multi_step_turn_replays_every_step_signature_in_order() {
    // Validation covers "all function calls within the current turn", so a
    // second step must carry the first step's signature as well as its own.
    let (adapter, transport) = make_adapter(mandatory_config(), text_response("done"));
    let first = serde_json::json!([
        {"functionCall":{"id":"fc-1","name":"read","args":{}},
         "thoughtSignature":"Signature-A"}
    ]);
    let second = serde_json::json!([
        {"functionCall":{"id":"fc-2","name":"grep","args":{}},
         "thoughtSignature":"Signature-B"}
    ]);
    let mut request = draft();
    request.tools = vec![read_tool(), grep_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        replayed_state(first.clone()),
        InferenceInput::Message(ChatMessage::tool("fc-1", "body")),
        replayed_state(second.clone()),
        InferenceInput::Message(ChatMessage::tool("fc-2", "hits")),
    ];

    run(&adapter, request).await;

    let contents = transport.body()["contents"].clone();
    assert_eq!(contents[1]["parts"], first);
    assert_eq!(contents[3]["parts"], second);
    assert_eq!(
        contents[4]["parts"][0]["functionResponse"]["name"],
        serde_json::json!("grep")
    );
}

#[tokio::test]
async fn a_replayed_model_turn_still_requires_its_function_declaration() {
    // The lossless path must not be the less validated one: Gemini refuses a
    // replayed call whose declaration is absent from this request.
    let (adapter, transport) = make_adapter(mandatory_config(), Vec::new());
    let mut request = draft();
    request.tools = vec![grep_tool()];
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect")),
        replayed_state(serde_json::json!([
            {"functionCall":{"id":"fc-1","name":"read","args":{}},
             "thoughtSignature":"Signature-A"}
        ])),
        InferenceInput::Message(ChatMessage::tool("fc-1", "body")),
    ];

    let error = adapter.resolve(request, &model()).unwrap_err();

    assert!(
        matches!(&error, ResolveError::InvalidRequest { field, .. } if *field == "tools"),
        "got {error:?}"
    );
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn a_signature_optional_route_accepts_a_response_with_no_signature_at_all() {
    let (adapter, _transport) = make_adapter(optional_config(), text_response("answer"));

    let events = ok_events(run(&adapter, draft()).await);

    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::Stop))
    );
}

// ---------------------------------------------------------------------------
// usageMetadata — an absent counter is unknown, never zero
// ---------------------------------------------------------------------------

fn usage_of(events: &[InferenceEvent]) -> Option<heycode_llm::TokenUsage> {
    events.iter().find_map(|event| match event {
        InferenceEvent::Usage(usage) => Some(*usage),
        _ => None,
    })
}

async fn usage_for(usage_metadata: Option<serde_json::Value>) -> Option<heycode_llm::TokenUsage> {
    let mut chunk_value = serde_json::json!({
        "candidates":[{
            "content":{"role":"model","parts":[{"text":"a"}]},
            "finishReason":"STOP"
        }],
        "responseId":"resp-usage"
    });
    if let Some(usage_metadata) = usage_metadata {
        chunk_value["usageMetadata"] = usage_metadata;
    }
    let (adapter, _transport) = make_adapter(optional_config(), vec![chunk(chunk_value)]);
    usage_of(&ok_events(run(&adapter, draft()).await))
}

#[tokio::test]
async fn absent_usage_metadata_emits_no_usage_event() {
    assert_eq!(usage_for(None).await, None);
}

#[tokio::test]
async fn an_absent_prompt_token_count_emits_no_usage_event() {
    assert_eq!(
        usage_for(Some(serde_json::json!({"candidatesTokenCount":7}))).await,
        None,
        "an absent prompt counter is unknown, never zero"
    );
}

#[tokio::test]
async fn an_absent_candidates_token_count_emits_no_usage_event() {
    assert_eq!(
        usage_for(Some(serde_json::json!({"promptTokenCount":11}))).await,
        None,
        "an absent candidate counter is unknown, never zero"
    );
}

#[tokio::test]
async fn thoughts_tokens_are_added_to_the_completion_count() {
    assert_eq!(
        usage_for(Some(serde_json::json!({
            "promptTokenCount":11,
            "candidatesTokenCount":7,
            "thoughtsTokenCount":40,
            "totalTokenCount":58
        })))
        .await,
        Some(heycode_llm::TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 47,
        })
    );
}

#[tokio::test]
async fn cached_prompt_tokens_are_not_added_to_the_prompt_count() {
    assert_eq!(
        usage_for(Some(serde_json::json!({
            "promptTokenCount":11,
            "cachedContentTokenCount":9,
            "candidatesTokenCount":7
        })))
        .await,
        Some(heycode_llm::TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 7,
        }),
        "cachedContentTokenCount counts part of promptTokenCount"
    );
}

#[tokio::test]
async fn a_non_integer_usage_counter_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"text":"a"}]},
                "finishReason":"STOP"
            }],
            "usageMetadata":{"promptTokenCount":-1,"candidatesTokenCount":7},
            "responseId":"resp-bad-usage"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn a_usage_counter_that_moves_backwards_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![
            chunk(serde_json::json!({
                "candidates":[{"content":{"role":"model","parts":[{"text":"a"}]}}],
                "usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":7},
                "responseId":"resp-regress"
            })),
            chunk(serde_json::json!({
                "candidates":[{
                    "content":{"role":"model","parts":[{"text":"b"}]},
                    "finishReason":"STOP"
                }],
                "usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":2},
                "responseId":"resp-regress"
            })),
        ],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn the_last_streamed_usage_metadata_wins() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![
            chunk(serde_json::json!({
                "candidates":[{"content":{"role":"model","parts":[{"text":"a"}]}}],
                "usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":2},
                "responseId":"resp-cumulative"
            })),
            chunk(serde_json::json!({
                "candidates":[{
                    "content":{"role":"model","parts":[{"text":"b"}]},
                    "finishReason":"STOP"
                }],
                "usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":9},
                "responseId":"resp-cumulative"
            })),
        ],
    );

    let events = ok_events(run(&adapter, draft()).await);

    assert_eq!(
        usage_of(&events),
        Some(heycode_llm::TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 9,
        })
    );
}

// ---------------------------------------------------------------------------
// Chunk envelope, secrets and raw-SSE fragmentation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_chunk_without_a_response_id_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"text":"a"}]},
                "finishReason":"STOP"
            }]
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn a_nonzero_candidate_index_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"text":"a"}]},
                "finishReason":"STOP",
                "index":1
            }],
            "responseId":"resp-index"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(
        matches!(&error, LlmError::InvalidResponse(message)
            if message.contains("candidate index")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_candidate_content_role_other_than_model_fails() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"user","parts":[{"text":"a"}]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-role"
        }))],
    );

    let error = stream_error(run(&adapter, draft()).await);

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn a_model_version_that_differs_from_the_requested_id_is_accepted() {
    let (adapter, _transport) = make_adapter(
        optional_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[{"text":"a"}]},
                "finishReason":"STOP"
            }],
            "modelVersion":"gemini-fixture-model-20260401",
            "responseId":"resp-version"
        }))],
    );

    let events = ok_events(run(&adapter, draft()).await);

    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::Stop))
    );
}

#[test]
fn config_debug_never_exposes_the_api_key() {
    let rendered = format!("{:?}", optional_config());

    assert!(!rendered.contains("test-key"), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
}

#[test]
fn a_blank_api_key_fails_construction_as_an_authentication_failure() {
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));
    let error = GeminiAdapter::new(
        GeminiConfig::with_key(provider(), BASE_URL, "   "),
        heycode_http::HttpService::new(transport),
    )
    .err()
    .expect("construction must fail");

    assert_eq!(
        error.class(),
        heycode_llm::ProviderErrorClass::Authentication
    );
}

#[test]
fn a_provider_that_does_not_declare_the_protocol_fails_construction() {
    let mut descriptor = provider();
    descriptor.protocols = vec![ProviderProtocol::OpenAiChatCompletions];
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));

    let error = GeminiAdapter::new(
        GeminiConfig::with_key(descriptor, BASE_URL, "test-key"),
        heycode_http::HttpService::new(transport),
    )
    .err()
    .expect("construction must fail");

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[test]
fn an_extra_header_that_replaces_the_api_key_header_fails_construction() {
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));

    let error = GeminiAdapter::new(
        optional_config()
            .with_extra_headers(vec![("X-Goog-Api-Key".to_owned(), "other".to_owned())]),
        heycode_http::HttpService::new(transport),
    )
    .err()
    .expect("construction must fail");

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[test]
fn a_reasoning_default_outside_the_exact_choice_list_fails_construction() {
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));

    let error = GeminiAdapter::new(
        optional_config()
            .with_default_reasoning_effort(Some(ReasoningEffortId::new("low").unwrap())),
        heycode_http::HttpService::new(transport),
    )
    .err()
    .expect("construction must fail");

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[test]
fn a_thinking_budget_below_minus_one_fails_construction() {
    let transport = Arc::new(ScriptedTransport::new(Vec::new()));

    let error = GeminiAdapter::new(
        optional_config().with_thinking_budget(ReasoningEffortId::new("bad").unwrap(), -2, false),
        heycode_http::HttpService::new(transport),
    )
    .err()
    .expect("construction must fail");

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
}

#[tokio::test]
async fn raw_sse_fragmentation_is_protocol_invariant() {
    let wire = concat!(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hé\"}]}}],\"responseId\":\"resp-frag\"}\n\n",
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"id\":\"fc-1\",\"name\":\"read\",\"args\":{}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":4},\"responseId\":\"resp-frag\"}\n\n",
    );
    let metadata = ConformanceFixtureMetadata::new(
        "google",
        ConformanceSourceKind::Synthetic,
        "https://ai.google.dev/api/generate-content",
        "gemini-v1beta",
        1_788_048_000_000,
    )
    .unwrap();
    let cases = SseConformanceFixture::new("gemini-terminal", metadata, wire.as_bytes())
        .unwrap()
        .fragmentation_cases();

    let runs = run_sse_conformance(&cases, |http| async move {
        let adapter = GeminiAdapter::new(optional_config(), http).unwrap();
        let mut request = draft();
        request.tools = vec![read_tool()];
        let call = adapter.resolve(request, &model()).unwrap();
        adapter.stream(call).collect::<Vec<_>>().await
    })
    .await;

    let expected = runs[0]
        .output
        .iter()
        .map(|event| event.as_ref().unwrap().clone())
        .collect::<Vec<_>>();
    assert!(
        matches!(expected.as_slice(), [
            InferenceEvent::ResponseStarted { .. },
            InferenceEvent::ItemStarted { kind: heycode_llm::StreamItemKind::Message, .. },
            InferenceEvent::TextDelta(text),
            InferenceEvent::ItemFinished { kind: heycode_llm::StreamItemKind::Message, .. },
            InferenceEvent::ItemStarted { kind: heycode_llm::StreamItemKind::FunctionCall, .. },
            InferenceEvent::ToolCallDelta { .. },
            InferenceEvent::ItemFinished { kind: heycode_llm::StreamItemKind::FunctionCall, .. },
            InferenceEvent::ProviderState(_),
            InferenceEvent::ResponseFinished { .. },
            InferenceEvent::Usage(_),
            InferenceEvent::Finish(FinishReason::ToolCalls),
        ] if text == "hé"),
        "{expected:#?}"
    );
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

// ---------------------------------------------------------------------------
// PGCP04 — end-to-end tool loop, and the live smoke pass
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tool_loop_carries_the_first_turn_s_state_into_the_second_request() {
    // One fixture for the whole round trip: the model asks for a tool, the
    // adapter publishes the turn it actually received, and the continuation
    // sends that turn back beside the result. Each half is pinned separately
    // elsewhere; this proves they compose.
    let (first, _) = make_adapter(
        mandatory_config(),
        vec![chunk(serde_json::json!({
            "candidates":[{
                "content":{"role":"model","parts":[
                    {"functionCall":{"id":"fc-1","name":"read","args":{"path":"a.rs"}},
                     "thoughtSignature":"Signature-A"}
                ]},
                "finishReason":"STOP"
            }],
            "responseId":"resp-loop-1"
        }))],
    );
    let mut opening = draft();
    opening.tools = vec![read_tool()];
    let events = ok_events(run(&first, opening).await);
    assert_eq!(
        events.last(),
        Some(&InferenceEvent::Finish(FinishReason::ToolCalls))
    );
    let state = published_state(&events).clone();

    let (second, transport) = make_adapter(mandatory_config(), text_response("it compiles"));
    let mut follow_up = draft();
    follow_up.tools = vec![read_tool()];
    follow_up.inputs = vec![
        InferenceInput::Message(ChatMessage::user("hello")),
        InferenceInput::ProviderState(state),
        InferenceInput::Message(ChatMessage::tool("fc-1", "fn main() {}")),
    ];
    let closing = ok_events(run(&second, follow_up).await);
    assert_eq!(
        closing.last(),
        Some(&InferenceEvent::Finish(FinishReason::Stop))
    );

    let contents = transport.body()["contents"].clone();
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(
        contents[1]["parts"][0]["thoughtSignature"],
        serde_json::json!("Signature-A")
    );
    assert_eq!(contents[2]["role"], "user");
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"],
        serde_json::json!({
            "id":"fc-1","name":"read","response":{"result":"fn main() {}"}
        })
    );
}

/// A one-pixel PNG, so the live image smoke sends a real decodable image.
const ONE_PIXEL_PNG: [u8; 69] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0, 3, 1,
    1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// The live route, or `None` when this host has no credential.
///
/// Every other test in this file drives an injected transport. These two are
/// the only paths that touch the real service, and they stay skipped unless
/// `HEYCODE_E2E=1` and `GEMINI_API_KEY` are both set — so on an ordinary run this
/// file proves wire *shape* against the documentation and proves nothing about
/// what Google accepts.
fn live_route() -> Option<(GeminiAdapter, ModelDescriptor)> {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return None;
    }
    let key = std::env::var("GEMINI_API_KEY").ok()?;
    // <https://ai.google.dev/gemini-api/docs/models> lists `gemini-3.7-flash`
    // as a current stable id; override it for a different route.
    let model_id =
        std::env::var("HEYCODE_E2E_GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.7-flash".to_owned());
    let provider = ProviderDescriptor {
        id: "google".to_owned(),
        display_name: "Google Gemini".to_owned(),
        protocols: vec![ProviderProtocol::GeminiGenerateContent],
    };
    let config = GeminiConfig::with_key(
        provider,
        "https://generativelanguage.googleapis.com/v1beta",
        key,
    )
    .with_retry_spec(heycode_llm::RetrySpec::no_retry());
    let transport = heycode_http::ReqwestHttpTransport::new().ok()?;
    let adapter = GeminiAdapter::new(
        config,
        heycode_http::HttpService::new(std::sync::Arc::new(transport)),
    )
    .ok()?;
    let mut live_model = model();
    live_model.id = model_id;
    Some((adapter, live_model))
}

#[tokio::test]
async fn live_tool_turn_returns_a_signed_function_call_when_enabled() {
    let Some((adapter, live_model)) = live_route() else {
        return;
    };
    let mut request = draft();
    request.tools = vec![read_tool()];
    request.inputs = vec![InferenceInput::Message(ChatMessage::user(
        "Read the file a.rs. Call the tool; do not answer directly.",
    ))];
    let call = adapter.resolve(request, &live_model).unwrap();
    let events = ok_events(adapter.stream(call).collect::<Vec<_>>().await);

    // A signature-mandatory route rejects an unsigned first call during
    // normalization, so reaching a published turn is itself the assertion.
    let state = published_state(&events);
    assert_eq!(state.kind(), ProviderStateKind::GeminiModelContent);
    assert!(
        state.data()["parts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part.get("functionCall").is_some()),
        "live turn requested no function: {:#?}",
        state.data()
    );
}

#[tokio::test]
async fn live_image_turn_accepts_inline_png_when_enabled() {
    let Some((adapter, live_model)) = live_route() else {
        return;
    };
    let request = image_draft(ChatMessage::user_with_images(
        "Reply with the single word: ok",
        vec![image("image/png", ONE_PIXEL_PNG.to_vec())],
    ));
    let call = adapter.resolve(request, &live_model).unwrap();
    let events = ok_events(adapter.stream(call).collect::<Vec<_>>().await);

    assert!(
        events
            .iter()
            .any(|event| matches!(event, InferenceEvent::TextDelta(_))),
        "live image turn produced no text: {events:#?}"
    );
}

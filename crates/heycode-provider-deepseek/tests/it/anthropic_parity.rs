//! Tool and reasoning parity for DeepSeek's Anthropic-format route.
//!
//! Every fixture below is a *constructed* Anthropic Messages stream, not a
//! DeepSeek capture. No DeepSeek credential exists on this host, so nothing
//! here observed DeepSeek's real bytes; what these cases pin is which shapes
//! the profile can and cannot carry, which is exactly what makes the
//! signature gap in [`heycode_provider_deepseek::DeepSeekAnthropicField::ThinkingBlockSignature`]
//! actionable rather than decorative.
//!
//! The live smoke at the end of this file is the only path that would settle
//! the constructed cases against DeepSeek itself. It is gated on an explicit
//! opt-in and a credential, and it did not run for PDS04.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::ToolSpec;
use heycode_credentials::{CredentialResolutionError, CredentialSecret};
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream, TransportError};
use heycode_llm::testing::{
    ConformanceFixtureMetadata, ConformanceSourceKind, SseConformanceFixture, run_sse_conformance,
};
use heycode_llm::{
    AuthenticationBinding, CallPurpose, CapabilitySupport, ChatMessage, ChatRequest,
    CredentialHandle, CredentialResolver, InferenceAdapter, InferenceEvent, InferenceInput,
    InputModality, LlmError, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, ProviderStateKind, ReasoningEffortId, RequestDraft, RouteCredential,
};
use heycode_provider_deepseek::{
    DEEPSEEK_V4_FLASH, DEEPSEEK_V4_MAX_OUTPUT_TOKENS, DEEPSEEK_V4_PRO, DeepSeekAnthropicAdapter,
    DeepSeekAnthropicProfile,
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Captured {
    url: String,
    headers: Vec<(String, String)>,
    body: serde_json::Value,
}

struct ScriptedTransport {
    events: Mutex<Option<Vec<Result<SseEvent, TransportError>>>>,
    captured: Mutex<Option<Captured>>,
}

struct RotatingTransport {
    scripts: Mutex<VecDeque<Vec<Result<SseEvent, TransportError>>>>,
    headers: Mutex<Vec<Vec<(String, String)>>>,
}

impl RotatingTransport {
    fn new(scripts: Vec<Vec<Result<SseEvent, TransportError>>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            headers: Mutex::new(Vec::new()),
        })
    }

    fn headers(&self) -> Vec<Vec<(String, String)>> {
        self.headers.lock().unwrap().clone()
    }
}

impl HttpTransport for RotatingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.headers.lock().unwrap().push(
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        );
        let events = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("one script must exist per operation");
        Box::pin(futures::stream::iter(events))
    }
}

struct RotatingCredential {
    route: CredentialHandle,
    calls: AtomicUsize,
}

impl CredentialResolver for RotatingCredential {
    fn route(&self) -> &CredentialHandle {
        &self.route
    }

    fn resolve(
        &self,
        route: &CredentialHandle,
    ) -> Result<CredentialSecret, CredentialResolutionError> {
        assert_eq!(route, &self.route);
        let value = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => "deepseek-operation-key-one",
            1 => "deepseek-operation-key-two",
            _ => panic!("credential must resolve exactly once per operation"),
        };
        Ok(CredentialSecret::new(value.to_owned()))
    }
}

impl ScriptedTransport {
    fn new(events: Vec<Result<SseEvent, TransportError>>) -> Self {
        Self {
            events: Mutex::new(Some(events)),
            captured: Mutex::new(None),
        }
    }

    fn captured(&self) -> Captured {
        self.captured.lock().unwrap().clone().unwrap()
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
            body: serde_json::from_slice(request.body().unwrap_or_default())
                .unwrap_or(serde_json::Value::Null),
        });
        Box::pin(futures::stream::iter(
            self.events.lock().unwrap().take().unwrap(),
        ))
    }
}

/// A V4 descriptor shaped like the one PDS01's catalog publishes.
fn v4_model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: DEEPSEEK_V4_FLASH.to_owned(),
        display_name: "DeepSeek V4 Flash".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(DEEPSEEK_V4_MAX_OUTPUT_TOKENS),
        lifecycle: ModelLifecycle::preview(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            image_input: CapabilitySupport::Unknown,
            document_input: CapabilitySupport::Unsupported,
            structured_output: CapabilitySupport::Unknown,
            native_web: CapabilitySupport::Unknown,
            native_compaction: CapabilitySupport::Unsupported,
            // Every `cache_control` row on DeepSeek's compatibility page reads
            // "Ignored", so this route cannot address prompt caching.
            prompt_cache: CapabilitySupport::Unsupported,
        },
        reasoning: None,
    }
}

fn v4_pro_model() -> ModelDescriptor {
    let mut model = v4_model();
    model.id = DEEPSEEK_V4_PRO.to_owned();
    model.display_name = "DeepSeek V4 Pro".to_owned();
    model
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "deepseek".to_owned(),
        model: DEEPSEEK_V4_FLASH.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
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
        parameters: serde_json::json!({
            "type":"object",
            "properties":{"path":{"type":"string"}},
            "required":["path"]
        }),
    }
}

fn adapter_for(
    profile: DeepSeekAnthropicProfile,
    events: Vec<Result<SseEvent, TransportError>>,
) -> (DeepSeekAnthropicAdapter, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::new(events));
    let adapter = DeepSeekAnthropicAdapter::with_key(
        profile,
        "deepseek-test-key",
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    (adapter, transport)
}

fn guarded_adapter(
    events: Vec<Result<SseEvent, TransportError>>,
) -> (DeepSeekAnthropicAdapter, Arc<ScriptedTransport>) {
    adapter_for(DeepSeekAnthropicProfile::api_key(), events)
}

fn adapter(
    events: Vec<Result<SseEvent, TransportError>>,
) -> (DeepSeekAnthropicAdapter, Arc<ScriptedTransport>) {
    adapter_for(DeepSeekAnthropicProfile::api_key(), events)
}

fn event(name: &str, data: serde_json::Value) -> Result<SseEvent, TransportError> {
    Ok(SseEvent {
        event: name.to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    })
}

fn message_start() -> Result<SseEvent, TransportError> {
    event(
        "message_start",
        serde_json::json!({
            "type":"message_start",
            "message":{
                "id":"msg_deepseek_fixture","type":"message","role":"assistant",
                "model":DEEPSEEK_V4_FLASH,"content":[],
                "stop_reason":null,"stop_sequence":null,
                "usage":{"input_tokens":11,"output_tokens":1}
            }
        }),
    )
}

fn message_stop(stop_reason: &str, output_tokens: u64) -> Vec<Result<SseEvent, TransportError>> {
    vec![
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta",
                "delta":{"stop_reason":stop_reason,"stop_sequence":null},
                "usage":{"output_tokens":output_tokens}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

/// A thinking block, optionally carrying the signature Anthropic requires and
/// DeepSeek never mentions.
fn thinking_block(signed: bool) -> Vec<Result<SseEvent, TransportError>> {
    let mut events = vec![
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
                "delta":{"type":"thinking_delta","thinking":"weigh the options"}
            }),
        ),
    ];
    if signed {
        events.push(event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":0,
                "delta":{"type":"signature_delta","signature":"EqQBCgIYAhIB"}
            }),
        ));
    }
    events.push(event(
        "content_block_stop",
        serde_json::json!({"type":"content_block_stop","index":0}),
    ));
    events
}

fn tool_use_block(index: u32) -> Vec<Result<SseEvent, TransportError>> {
    vec![
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start","index":index,
                "content_block":{
                    "type":"tool_use","id":"toolu_deepseek_1","name":"read","input":{}
                }
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":index,
                "delta":{"type":"input_json_delta","partial_json":"{\"path\":"}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta","index":index,
                "delta":{"type":"input_json_delta","partial_json":"\"AGENTS.md\"}"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":index}),
        ),
    ]
}

/// One signed thinking block followed by one tool call — the turn shape a
/// reasoning model takes mid tool loop.
fn signed_reasoning_tool_turn() -> Vec<Result<SseEvent, TransportError>> {
    let mut events = vec![message_start()];
    events.extend(thinking_block(true));
    events.extend(tool_use_block(1));
    events.extend(message_stop("tool_use", 24));
    events
}

/// The same turn with the signature omitted.
fn unsigned_reasoning_tool_turn() -> Vec<Result<SseEvent, TransportError>> {
    let mut events = vec![message_start()];
    events.extend(thinking_block(false));
    events.extend(tool_use_block(1));
    events.extend(message_stop("tool_use", 24));
    events
}

async fn drain(
    adapter: &dyn InferenceAdapter,
    request: RequestDraft,
) -> Vec<Result<InferenceEvent, LlmError>> {
    drain_with_model(adapter, request, &v4_model()).await
}

async fn drain_with_model(
    adapter: &dyn InferenceAdapter,
    request: RequestDraft,
    model: &ModelDescriptor,
) -> Vec<Result<InferenceEvent, LlmError>> {
    let call = adapter.resolve(request, model).unwrap();
    adapter.stream(call).collect::<Vec<_>>().await
}

fn provider_state(events: &[Result<InferenceEvent, LlmError>]) -> Option<serde_json::Value> {
    events.iter().find_map(|item| match item {
        Ok(InferenceEvent::ProviderState(state)) => {
            assert_eq!(state.kind(), ProviderStateKind::AnthropicMessage);
            Some(state.data().clone())
        }
        _ => None,
    })
}

fn provider_state_item(
    events: &[Result<InferenceEvent, LlmError>],
) -> Option<heycode_llm::ProviderStateItem> {
    events.iter().find_map(|item| match item {
        Ok(InferenceEvent::ProviderState(state)) => Some(state.clone()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Request parity: what the profile actually puts on the wire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_request_goes_to_the_messages_path_under_deepseeks_anthropic_base_url() {
    let (adapter, transport) = guarded_adapter(text_turn());
    drain(&adapter, draft()).await;
    assert_eq!(
        transport.captured().url,
        "https://api.deepseek.com/anthropic/v1/messages"
    );
}

#[test]
fn an_unrecognized_model_is_refused_by_the_profile_before_transport() {
    let (adapter, transport) = guarded_adapter(text_turn());
    let mut request = draft();
    request.model = "deepseek-v4-prro".to_owned();
    let mut model = v4_model();
    model.id = request.model.clone();

    let error = adapter.resolve(request, &model).unwrap_err();
    assert!(
        matches!(
            error,
            heycode_llm::ResolveError::InvalidRequest { field: "model", .. }
        ),
        "{error}"
    );
    assert!(transport.captured.lock().unwrap().is_none());
}

#[test]
fn a_documented_claude_alias_must_be_canonicalized_before_exact_dispatch() {
    let (adapter, transport) = guarded_adapter(text_turn());
    let mut request = draft();
    request.model = "claude-opus-5".to_owned();
    let mut model = v4_model();
    model.id = request.model.clone();

    let error = adapter.resolve(request, &model).unwrap_err();
    assert!(
        matches!(
            error,
            heycode_llm::ResolveError::InvalidRequest { field: "model", .. }
        ),
        "{error}"
    );
    assert!(transport.captured.lock().unwrap().is_none());
}

#[test]
fn provider_identity_stays_multi_dialect_while_the_selected_adapter_is_exact_messages() {
    let (adapter, transport) = guarded_adapter(text_turn());
    let provider = heycode_llm::Provider::descriptor(&adapter);
    assert_eq!(
        provider.protocols,
        vec![
            heycode_core::ProviderProtocol::OpenAiChatCompletions,
            heycode_core::ProviderProtocol::AnthropicMessages,
        ]
    );

    let inference = heycode_llm::InferenceAdapter::descriptor(&adapter);
    assert_eq!(
        inference.protocols,
        vec![heycode_core::ProviderProtocol::AnthropicMessages]
    );
    let call = heycode_llm::InferenceAdapter::resolve(&adapter, draft(), &v4_model()).unwrap();
    assert_eq!(
        call.protocol(),
        heycode_core::ProviderProtocol::AnthropicMessages
    );
    assert!(transport.captured.lock().unwrap().is_none());
}

#[tokio::test]
async fn the_guarded_route_is_provider_ready_and_never_falls_back_to_legacy_chat() {
    let (adapter, transport) = guarded_adapter(text_turn());
    let info = heycode_llm::Provider::info(&adapter);
    assert_eq!(info.name, "deepseek");
    assert_eq!(info.default_model, DEEPSEEK_V4_FLASH);
    assert_eq!(
        heycode_llm::Provider::credential_reference(&adapter),
        Some("DEEPSEEK_API_KEY")
    );
    assert!(heycode_llm::Provider::inference_adapter(&adapter).is_some());

    let legacy = heycode_llm::Provider::stream(
        &adapter,
        ChatRequest {
            model: DEEPSEEK_V4_FLASH.to_owned(),
            messages: vec![ChatMessage::user("must not dispatch")],
            tools: None,
            temperature: None,
            max_tokens: None,
        },
    )
    .collect::<Vec<_>>()
    .await;
    assert_eq!(legacy.len(), 1);
    assert!(legacy[0].is_err());
    assert!(transport.captured.lock().unwrap().is_none());
}

#[tokio::test]
async fn the_api_key_profile_authenticates_with_the_x_api_key_header_the_table_documents() {
    let (adapter, transport) = adapter(text_turn());
    drain(&adapter, draft()).await;
    let headers = transport.captured().headers;
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "x-api-key" && value == "deepseek-test-key"),
        "{headers:?}"
    );
    assert!(
        !headers.iter().any(|(name, _)| name == "authorization"),
        "{headers:?}"
    );
}

#[tokio::test]
async fn the_auth_token_profile_projects_the_claude_code_recipe_to_the_current_bearer_wire() {
    // DeepSeek documents ANTHROPIC_AUTH_TOKEN for Claude Code but does not
    // state this header in its compatibility table. The profile projection is
    // exact; its support evidence remains IntegrationInferred / Unknown.
    let (adapter, transport) = adapter_for(DeepSeekAnthropicProfile::auth_token(), text_turn());
    drain(&adapter, draft()).await;
    let headers = transport.captured().headers;
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "authorization" && value == "Bearer deepseek-test-key"),
        "{headers:?}"
    );
    assert!(
        !headers.iter().any(|(name, _)| name == "x-api-key"),
        "{headers:?}"
    );
}

#[tokio::test]
async fn operation_credentials_resolve_once_per_turn_and_rotation_reaches_the_next_request() {
    let route = CredentialHandle::new("DEEPSEEK_API_KEY").unwrap();
    let resolver = Arc::new(RotatingCredential {
        route: route.clone(),
        calls: AtomicUsize::new(0),
    });
    let credential = RouteCredential::per_operation(route.clone(), resolver.clone());
    let transport = RotatingTransport::new(vec![text_turn(), text_turn()]);
    let adapter = DeepSeekAnthropicAdapter::with_credential(
        DeepSeekAnthropicProfile::api_key(),
        credential,
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    assert_eq!(
        adapter.authentication_binding(),
        AuthenticationBinding::Credential(route)
    );

    for _ in 0..2 {
        let events = drain(&adapter, draft()).await;
        assert!(events.iter().all(Result::is_ok));
    }

    assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
    let headers = transport.headers();
    assert_eq!(headers.len(), 2);
    assert_eq!(
        headers[0]
            .iter()
            .find(|(name, _)| name == "x-api-key")
            .map(|(_, value)| value.as_str()),
        Some("deepseek-operation-key-one")
    );
    assert_eq!(
        headers[1]
            .iter()
            .find(|(name, _)| name == "x-api-key")
            .map(|(_, value)| value.as_str()),
        Some("deepseek-operation-key-two")
    );
}

#[tokio::test]
async fn a_default_request_enables_thinking_and_carries_effort_in_output_config() {
    // DeepSeek's Anthropic column toggles thinking with Anthropic's own
    // `thinking` block and controls effort with `output_config.effort`.
    let (adapter, transport) = adapter(text_turn());
    drain(&adapter, draft()).await;
    let body = transport.captured().body;
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["thinking"]["budget_tokens"], 1_024);
    assert_eq!(body["output_config"]["effort"], "high");
    // `thinking.display` is Anthropic's; DeepSeek's table has no row for it,
    // so it is never sent.
    assert!(body["thinking"].get("display").is_none(), "{body}");
}

#[tokio::test]
async fn each_offered_effort_sends_its_exact_documented_wire_value() {
    for (effort, expected) in [("low", "low"), ("high", "high"), ("max", "max")] {
        let (adapter, transport) = adapter(text_turn());
        let mut request = draft();
        request.reasoning_effort = Some(ReasoningEffortId::new(effort).unwrap());
        drain(&adapter, request).await;
        let body = transport.captured().body;
        assert_eq!(body["thinking"]["type"], "enabled", "{effort}");
        assert_eq!(body["output_config"]["effort"], expected, "{effort}");
    }
}

#[tokio::test]
async fn the_none_choice_sends_the_disabled_toggle_and_no_output_config_at_all() {
    let (adapter, transport) = adapter(text_turn());
    let mut request = draft();
    request.reasoning_effort = Some(ReasoningEffortId::new("none").unwrap());
    drain(&adapter, request).await;
    let body = transport.captured().body;
    assert_eq!(body["thinking"], serde_json::json!({"type":"disabled"}));
    assert!(body.get("output_config").is_none(), "{body}");
}

#[tokio::test]
async fn a_thinking_request_never_carries_temperature_so_deepseeks_conflicting_pages_do_not_matter()
{
    // The compatibility table calls `temperature` Fully Supported; the
    // thinking-mode guide says thinking ignores it. The field is not sent with
    // thinking enabled, so neither sentence has to be right.
    let (adapter, transport) = adapter(text_turn());
    let mut request = draft();
    request.temperature = Some(0.7);
    drain(&adapter, request).await;
    let body = transport.captured().body;
    assert_eq!(body["thinking"]["type"], "enabled");
    assert!(body.get("temperature").is_none(), "{body}");
}

#[tokio::test]
async fn a_non_thinking_request_does_carry_temperature_which_the_table_marks_supported() {
    let (adapter, transport) = adapter(text_turn());
    let mut request = draft();
    request.reasoning_effort = Some(ReasoningEffortId::new("none").unwrap());
    request.temperature = Some(0.7);
    drain(&adapter, request).await;
    // The draft carries an `f32`; JSON widens it, so the comparison is made in
    // `f64` rather than against a literal that cannot be represented exactly.
    let temperature = transport.captured().body["temperature"]
        .as_f64()
        .expect("temperature must be sent as a number");
    assert!((temperature - 0.7).abs() < 1e-6, "{temperature}");
}

#[tokio::test]
async fn tool_definitions_travel_with_a_tool_choice_whose_ignored_sub_field_cannot_change_behaviour()
 {
    // DeepSeek ignores `disable_parallel_tool_use`. heycode sends it as `false`,
    // which is the value that means "do not disable" — so a field DeepSeek
    // drops asks for exactly what dropping it produces.
    let (adapter, transport) = adapter(signed_reasoning_tool_turn());
    let mut request = draft();
    request.tools = vec![read_tool()];
    drain(&adapter, request).await;
    let body = transport.captured().body;
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["description"], "Read a file");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["tool_choice"]["type"], "auto");
    assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], false);
}

#[tokio::test]
async fn no_request_field_deepseek_ignores_is_ever_sent() {
    let (adapter, transport) = adapter(signed_reasoning_tool_turn());
    let mut request = draft();
    request.tools = vec![read_tool()];
    drain(&adapter, request).await;
    let body = transport.captured().body;
    for ignored in [
        "top_k",
        "container",
        "mcp_servers",
        "service_tier",
        "metadata",
    ] {
        assert!(body.get(ignored).is_none(), "{ignored} sent: {body}");
    }
    assert!(body["tools"][0].get("cache_control").is_none(), "{body}");
}

#[tokio::test]
async fn the_max_tokens_default_is_deepseeks_published_ceiling() {
    let (adapter, transport) = adapter(text_turn());
    drain(&adapter, draft()).await;
    assert_eq!(
        transport.captured().body["max_tokens"],
        DEEPSEEK_V4_MAX_OUTPUT_TOKENS
    );
}

fn text_turn() -> Vec<Result<SseEvent, TransportError>> {
    let mut events = vec![
        message_start(),
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
    ];
    events.extend(message_stop("end_turn", 4));
    events
}

// ---------------------------------------------------------------------------
// Tool parity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tool_call_turn_normalizes_to_argument_deltas_and_replayable_state() {
    let (adapter, _) = adapter(signed_reasoning_tool_turn());
    let mut request = draft();
    request.tools = vec![read_tool()];
    let events = drain(&adapter, request).await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");

    let arguments: String = events
        .iter()
        .filter_map(|item| match item {
            Ok(InferenceEvent::ToolCallDelta {
                arguments_delta, ..
            }) => Some(arguments_delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(arguments, r#"{"path":"AGENTS.md"}"#);

    let state = provider_state(&events).expect("a tool turn must publish replayable state");
    let content = state["content"].as_array().unwrap();
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "toolu_deepseek_1");
    assert_eq!(content[1]["name"], "read");
    assert_eq!(content[1]["input"]["path"], "AGENTS.md");
}

#[tokio::test]
async fn a_tool_turn_finishes_on_the_tool_use_stop_reason() {
    let (adapter, _) = adapter(signed_reasoning_tool_turn());
    let mut request = draft();
    request.tools = vec![read_tool()];
    let events = drain(&adapter, request).await;
    assert!(
        matches!(
            events.last(),
            Some(Ok(InferenceEvent::Finish(
                heycode_llm::FinishReason::ToolCalls
            )))
        ),
        "{events:#?}"
    );
}

// ---------------------------------------------------------------------------
// Reasoning parity — and the signature boundary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_signed_thinking_block_streams_reasoning_and_replays_its_signature_verbatim() {
    let (adapter, _) = adapter(signed_reasoning_tool_turn());
    let mut request = draft();
    request.tools = vec![read_tool()];
    let events = drain(&adapter, request).await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");

    let reasoning: String = events
        .iter()
        .filter_map(|item| match item {
            Ok(InferenceEvent::ReasoningDelta(delta)) => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "weigh the options");

    let state = provider_state(&events).expect("a reasoning turn must publish replayable state");
    let thinking = &state["content"][0];
    assert_eq!(thinking["type"], "thinking");
    assert_eq!(thinking["thinking"], "weigh the options");
    // Anthropic requires the signature back unmodified. State that cannot
    // carry it cannot continue a thinking tool loop.
    assert_eq!(thinking["signature"], "EqQBCgIYAhIB");
}

#[tokio::test]
async fn a_reasoning_tool_turn_replays_unchanged_before_its_tool_result() {
    let (first, _) = adapter(signed_reasoning_tool_turn());
    let mut first_request = draft();
    first_request.tools = vec![read_tool()];
    let first_events = drain(&first, first_request).await;
    assert!(first_events.iter().all(Result::is_ok), "{first_events:#?}");
    let state = provider_state_item(&first_events)
        .expect("a successful reasoning tool turn must publish continuation state");

    let (second, transport) = adapter(text_turn());
    let mut second_request = draft();
    second_request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("inspect the repository law")),
        InferenceInput::ProviderState(state.clone()),
        InferenceInput::Message(ChatMessage::tool_result(
            "toolu_deepseek_1",
            "repository law contents",
            false,
        )),
    ];
    second_request.tools = vec![read_tool()];
    let second_events = drain(&second, second_request).await;
    assert!(
        second_events.iter().all(Result::is_ok),
        "{second_events:#?}"
    );

    let body = transport.captured().body;
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{body}");
    assert_eq!(messages[1], *state.data(), "{body}");
    assert_eq!(
        messages[1]["content"][0]["signature"], "EqQBCgIYAhIB",
        "{body}"
    );
    assert_eq!(messages[1]["content"][1]["type"], "tool_use", "{body}");
    assert_eq!(messages[2]["role"], "user", "{body}");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result", "{body}");
    assert_eq!(
        messages[2]["content"][0]["tool_use_id"], "toolu_deepseek_1",
        "{body}"
    );
}

#[tokio::test]
async fn an_unsigned_thinking_block_is_rejected_and_publishes_neither_state_nor_finish() {
    // This is the compatibility boundary. DeepSeek documents the `thinking`
    // block as supported and says nothing about `signature`; heycode's Messages
    // parser refuses a thinking block that ends without one. If DeepSeek's
    // real stream omits it, this is the failure that follows — the route
    // cannot silently degrade into an unreplayable turn.
    let (adapter, _) = adapter(unsigned_reasoning_tool_turn());
    let mut request = draft();
    request.tools = vec![read_tool()];
    let events = drain(&adapter, request).await;

    assert!(
        events.iter().any(Result::is_err),
        "an unsigned thinking block must fail: {events:#?}"
    );
    assert!(
        provider_state(&events).is_none(),
        "a failed turn must publish no replayable state: {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|item| matches!(item, Ok(InferenceEvent::Finish(_)))),
        "a failed turn must not finish: {events:#?}"
    );
}

#[tokio::test]
async fn the_signature_requirement_is_the_parsers_and_is_stated_in_its_failure() {
    let (adapter, _) = adapter(unsigned_reasoning_tool_turn());
    let events = drain(&adapter, draft()).await;
    let failure = events
        .iter()
        .find_map(|item| item.as_ref().err())
        .expect("an unsigned thinking block must fail")
        .to_string();
    assert!(failure.contains("signature"), "{failure}");
}

#[tokio::test]
async fn the_reasoning_and_tool_turn_survives_every_network_fragmentation() {
    // Proves the parity fixture does not depend on chunk boundaries, so a
    // future live capture can be compared against it byte for byte.
    let wire = sse_wire(&signed_reasoning_tool_turn());
    let metadata = ConformanceFixtureMetadata::new(
        "deepseek",
        ConformanceSourceKind::Synthetic,
        "https://api-docs.deepseek.com/guides/anthropic_api/",
        "anthropic-compatibility-guide-2026-08-31",
        1_788_134_400_000,
    )
    .unwrap();
    let fixture =
        SseConformanceFixture::new("deepseek-anthropic/reasoning-tool", metadata.clone(), &wire)
            .unwrap();
    let cases = fixture.fragmentation_cases();
    assert!(cases.len() > 2, "expected a real fragmentation matrix");
    let runs = run_sse_conformance(&cases, |http| async move {
        let adapter = DeepSeekAnthropicAdapter::with_key(
            DeepSeekAnthropicProfile::api_key(),
            "deepseek-test-key",
            http,
        )
        .unwrap();
        let mut request = draft();
        request.tools = vec![read_tool()];
        let call = adapter.resolve(request, &v4_model()).unwrap();
        adapter
            .stream(call)
            .filter_map(|item| async move {
                match item {
                    Ok(InferenceEvent::ReasoningDelta(delta)) => Some(delta),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .await
            .join("")
    })
    .await;
    for run in &runs {
        assert_eq!(run.metadata, metadata, "case {}", run.case);
        assert_eq!(run.output, "weigh the options", "case {}", run.case);
        assert_eq!(run.transport_calls, 1, "case {}", run.case);
    }
}

fn sse_wire(events: &[Result<SseEvent, TransportError>]) -> Vec<u8> {
    let mut wire = String::new();
    for item in events {
        let Ok(event) = item else {
            panic!("fixture wire cannot encode a transport failure");
        };
        wire.push_str("event: ");
        wire.push_str(&event.event);
        wire.push('\n');
        wire.push_str("data: ");
        wire.push_str(&event.data);
        wire.push_str("\n\n");
    }
    wire.into_bytes()
}

// ---------------------------------------------------------------------------
// Live smoke
// ---------------------------------------------------------------------------

const LIVE_SMOKE_MAX_OUTPUT_TOKENS: u64 = 4_096;
const LIVE_SMOKE_PROMPT: &str =
    "Read AGENTS.md with the read tool. Call the tool; do not answer directly.";
const LIVE_SMOKE_TOOL_RESULT: &str =
    "AGENTS.md was read successfully; its first heading is heycode Agent Law.";

/// Whether the live smoke can run, and why not when it cannot.
#[derive(Clone, PartialEq, Eq)]
enum LiveSmokeGate {
    /// Named, reportable reason the live call was not made.
    Skipped(&'static str),
    /// Opt-in and credential both present.
    Ready(String),
}

impl std::fmt::Debug for LiveSmokeGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skipped(reason) => formatter.debug_tuple("Skipped").field(reason).finish(),
            Self::Ready(_) => formatter.write_str("Ready([REDACTED])"),
        }
    }
}

/// Decide the gate from explicit inputs so the decision is testable without
/// mutating this process's environment.
fn live_smoke_gate(opt_in: Option<String>, secret: Option<String>) -> LiveSmokeGate {
    if opt_in.as_deref() != Some("1") {
        return LiveSmokeGate::Skipped("HEYCODE_E2E is not set to 1");
    }
    match secret {
        Some(secret) if !secret.trim().is_empty() => LiveSmokeGate::Ready(secret),
        _ => LiveSmokeGate::Skipped("DEEPSEEK_API_KEY is unset or blank"),
    }
}

/// Closed live-evidence failures. No provider payload, tool argument, model
/// output, request body, or credential has a field in this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveSmokeFailure {
    ProviderStream,
    ModelIdentity,
    MissingReasoning,
    MissingClientToolCall,
    UnexpectedClientToolCount,
    MissingProviderState,
    MissingSignedThinking,
    MissingReplayableToolCall,
    ToolCallIdentity,
    MissingFinalText,
    MissingReplayableFinalText,
    UnexpectedFinish,
}

struct LiveToolContinuation {
    state: heycode_llm::ProviderStateItem,
    tool_call_id: String,
}

fn evaluate_live_tool_turn(
    events: &[Result<InferenceEvent, LlmError>],
    expected_model: &str,
) -> Result<LiveToolContinuation, LiveSmokeFailure> {
    if events.iter().any(Result::is_err) {
        return Err(LiveSmokeFailure::ProviderStream);
    }
    if !events.iter().any(
        |event| matches!(event, Ok(InferenceEvent::ReasoningDelta(delta)) if !delta.is_empty()),
    ) {
        return Err(LiveSmokeFailure::MissingReasoning);
    }
    let streamed_calls = events
        .iter()
        .filter_map(|event| match event {
            Ok(InferenceEvent::ToolCallDelta {
                id: Some(id),
                name: Some(name),
                ..
            }) => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    if streamed_calls.len() != 1 {
        return Err(LiveSmokeFailure::UnexpectedClientToolCount);
    }
    let (streamed_call_id, streamed_name) = streamed_calls[0];
    if streamed_name != "read" {
        return Err(LiveSmokeFailure::MissingClientToolCall);
    }
    let state = provider_state_item(events).ok_or(LiveSmokeFailure::MissingProviderState)?;
    if state.provider() != "deepseek"
        || state.model() != expected_model
        || state.protocol() != heycode_core::ProviderProtocol::AnthropicMessages
        || state.kind() != ProviderStateKind::AnthropicMessage
    {
        return Err(LiveSmokeFailure::ModelIdentity);
    }
    let blocks = state
        .data()
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or(LiveSmokeFailure::MissingProviderState)?;
    let signed_thinking = blocks.iter().any(|block| {
        block.get("type").and_then(serde_json::Value::as_str) == Some("thinking")
            && block
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|signature| !signature.is_empty())
    });
    if !signed_thinking {
        return Err(LiveSmokeFailure::MissingSignedThinking);
    }
    let replayable_tools = blocks
        .iter()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"))
        .collect::<Vec<_>>();
    if replayable_tools.len() != 1 {
        return Err(LiveSmokeFailure::UnexpectedClientToolCount);
    }
    let replayable_tool = replayable_tools[0];
    if replayable_tool
        .get("name")
        .and_then(serde_json::Value::as_str)
        != Some("read")
    {
        return Err(LiveSmokeFailure::MissingReplayableToolCall);
    }
    let replayable_call_id = replayable_tool
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(LiveSmokeFailure::MissingReplayableToolCall)?;
    if replayable_call_id != streamed_call_id {
        return Err(LiveSmokeFailure::ToolCallIdentity);
    }
    let finishes = events
        .iter()
        .filter_map(|event| match event {
            Ok(InferenceEvent::Finish(reason)) => Some(*reason),
            _ => None,
        })
        .collect::<Vec<_>>();
    if finishes != [heycode_llm::FinishReason::ToolCalls] {
        return Err(LiveSmokeFailure::UnexpectedFinish);
    }
    Ok(LiveToolContinuation {
        state,
        tool_call_id: streamed_call_id.to_owned(),
    })
}

fn evaluate_live_final_turn(
    events: &[Result<InferenceEvent, LlmError>],
    expected_model: &str,
) -> Result<(), LiveSmokeFailure> {
    if events.iter().any(Result::is_err) {
        return Err(LiveSmokeFailure::ProviderStream);
    }
    if !events
        .iter()
        .any(|event| matches!(event, Ok(InferenceEvent::TextDelta(text)) if !text.is_empty()))
    {
        return Err(LiveSmokeFailure::MissingFinalText);
    }
    let state = provider_state_item(events).ok_or(LiveSmokeFailure::MissingProviderState)?;
    if state.provider() != "deepseek"
        || state.model() != expected_model
        || state.protocol() != heycode_core::ProviderProtocol::AnthropicMessages
        || state.kind() != ProviderStateKind::AnthropicMessage
    {
        return Err(LiveSmokeFailure::ModelIdentity);
    }
    let replayable_text = state
        .data()
        .get("content")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|text| !text.is_empty())
            })
        });
    if !replayable_text {
        return Err(LiveSmokeFailure::MissingReplayableFinalText);
    }
    let finishes = events
        .iter()
        .filter_map(|event| match event {
            Ok(InferenceEvent::Finish(reason)) => Some(*reason),
            _ => None,
        })
        .collect::<Vec<_>>();
    if finishes != [heycode_llm::FinishReason::Stop] {
        return Err(LiveSmokeFailure::UnexpectedFinish);
    }
    Ok(())
}

#[test]
fn the_live_smoke_names_the_reason_it_skips_rather_than_skipping_silently() {
    assert_eq!(
        live_smoke_gate(None, Some("key".to_owned())),
        LiveSmokeGate::Skipped("HEYCODE_E2E is not set to 1")
    );
    assert_eq!(
        live_smoke_gate(Some("0".to_owned()), Some("key".to_owned())),
        LiveSmokeGate::Skipped("HEYCODE_E2E is not set to 1")
    );
    assert_eq!(
        live_smoke_gate(Some("1".to_owned()), None),
        LiveSmokeGate::Skipped("DEEPSEEK_API_KEY is unset or blank")
    );
    assert_eq!(
        live_smoke_gate(Some("1".to_owned()), Some("   ".to_owned())),
        LiveSmokeGate::Skipped("DEEPSEEK_API_KEY is unset or blank")
    );
    assert_eq!(
        live_smoke_gate(Some("1".to_owned()), Some("key".to_owned())),
        LiveSmokeGate::Ready("key".to_owned())
    );
    let debug = format!("{:?}", LiveSmokeGate::Ready("live-secret".to_owned()));
    assert!(!debug.contains("live-secret"), "{debug}");
    assert!(debug.contains("REDACTED"), "{debug}");
}

#[tokio::test]
async fn the_live_evidence_check_accepts_only_a_signed_reasoning_tool_turn() {
    let (adapter, _) = guarded_adapter(signed_reasoning_tool_turn());
    let mut request = draft();
    request.tools = vec![read_tool()];
    let events = drain(&adapter, request).await;
    match evaluate_live_tool_turn(&events, DEEPSEEK_V4_FLASH) {
        Ok(_) => {}
        Err(failure) => panic!("closed live evidence failure: {failure:?}"),
    }
}

#[tokio::test]
async fn the_live_evidence_check_reports_only_a_closed_failure_class() {
    let (adapter, _) = guarded_adapter(text_turn());
    let events = drain(&adapter, draft()).await;
    assert_eq!(
        evaluate_live_tool_turn(&events, DEEPSEEK_V4_FLASH).err(),
        Some(LiveSmokeFailure::MissingReasoning)
    );
    assert_eq!(
        format!("{:?}", LiveSmokeFailure::ProviderStream),
        "ProviderStream"
    );
}

#[tokio::test]
async fn the_live_final_evidence_check_requires_stop_text_and_exact_model_state() {
    let (adapter, _) = guarded_adapter(text_turn());
    let events = drain(&adapter, draft()).await;
    assert_eq!(evaluate_live_final_turn(&events, DEEPSEEK_V4_FLASH), Ok(()));
}

#[test]
fn the_live_smoke_output_cap_passes_manual_thinking_admission() {
    let (adapter, _) = adapter_for(
        DeepSeekAnthropicProfile::api_key().with_max_output_tokens(LIVE_SMOKE_MAX_OUTPUT_TOKENS),
        text_turn(),
    );
    let mut request = draft();
    request.model = DEEPSEEK_V4_PRO.to_owned();
    request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    assert!(adapter.resolve(request, &v4_pro_model()).is_ok());
}

#[tokio::test]
async fn live_deepseek_anthropic_route_answers_a_reasoning_tool_turn_when_enabled() {
    // NOT EXECUTED for PDS04: no DeepSeek credential exists on this host, so
    // this returns at the gate. It is the only path here that can settle the
    // undocumented route suffix, signed-thinking behavior, exact tool-result
    // continuation and terminal stop shape. Response usage/cache fields stay
    // Unknown unless a live response actually reports them.
    let LiveSmokeGate::Ready(secret) = live_smoke_gate(
        std::env::var("HEYCODE_E2E").ok(),
        std::env::var("DEEPSEEK_API_KEY").ok(),
    ) else {
        return;
    };

    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let adapter = DeepSeekAnthropicAdapter::with_key(
        DeepSeekAnthropicProfile::api_key().with_max_output_tokens(LIVE_SMOKE_MAX_OUTPUT_TOKENS),
        secret,
        heycode_http::HttpService::new(Arc::new(transport)),
    )
    .unwrap();

    let mut first_request = draft();
    first_request.model = DEEPSEEK_V4_PRO.to_owned();
    first_request.inputs = vec![InferenceInput::Message(ChatMessage::user(
        LIVE_SMOKE_PROMPT,
    ))];
    first_request.tools = vec![read_tool()];
    first_request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    let live_model = v4_pro_model();
    let first_events = drain_with_model(&adapter, first_request, &live_model).await;
    let continuation = match evaluate_live_tool_turn(&first_events, DEEPSEEK_V4_PRO) {
        Ok(continuation) => continuation,
        Err(failure) => panic!("DeepSeek Anthropic live first leg failed: {failure:?}"),
    };

    let mut second_request = draft();
    second_request.model = DEEPSEEK_V4_PRO.to_owned();
    second_request.inputs = vec![
        InferenceInput::Message(ChatMessage::user(LIVE_SMOKE_PROMPT)),
        InferenceInput::ProviderState(continuation.state),
        InferenceInput::Message(ChatMessage::tool_result(
            continuation.tool_call_id,
            LIVE_SMOKE_TOOL_RESULT,
            false,
        )),
    ];
    second_request.tools = vec![read_tool()];
    second_request.reasoning_effort = Some(ReasoningEffortId::new("high").unwrap());
    let second_events = drain_with_model(&adapter, second_request, &live_model).await;

    // Failures are closed classes. Raw response bodies, normalized text,
    // tool arguments, call ids and the credential have no diagnostic field.
    assert_eq!(
        evaluate_live_final_turn(&second_events, DEEPSEEK_V4_PRO),
        Ok(())
    );
}

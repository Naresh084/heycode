//! PAN04 native server-compaction definition and continuation state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceInput, InputModality, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, NativeFeature, Provider,
    RequestDraft, RetrySafety,
};
use heycode_provider_anthropic::{
    ANTHROPIC_CLAUDE_OPUS_5, AnthropicCompactionCheckpoint, AnthropicCompactionDefinition,
    AnthropicCompactionFault, AnthropicProvider, anthropic_compaction_support,
};
use tokio_util::sync::CancellationToken;

const MODEL: &str = ANTHROPIC_CLAUDE_OPUS_5;
const OPAQUE: &str = "<summary>OPAQUE COMPACTION CANARY; preserve exactly</summary>";
const SIGNATURE: &str = "EosnCkYICxIMMb3LzNrMu-compaction-signature";

#[derive(Default)]
struct RecordingTransport {
    requests: Mutex<Vec<serde_json::Value>>,
    headers: Mutex<Vec<Vec<(String, String)>>>,
}

impl HttpTransport for RecordingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        let body: serde_json::Value =
            serde_json::from_slice(request.body().expect("Messages request has a body")).unwrap();
        let is_compaction = body.get("context_management").is_some();
        self.requests.lock().unwrap().push(body);
        self.headers.lock().unwrap().push(
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        );
        let script = if is_compaction {
            compaction_script()
        } else {
            terminal_script()
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

fn terminal_script() -> Vec<SseEvent> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start",
                "message":{"id":"msg_after_compaction","type":"message","role":"assistant",
                    "model":MODEL,"content":[],"stop_reason":serde_json::Value::Null,
                    "stop_sequence":serde_json::Value::Null,
                    "usage":{"input_tokens":5,"output_tokens":0}}
            }),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta",
                "delta":{"stop_reason":"end_turn","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":1}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn compaction_script() -> Vec<SseEvent> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start",
                "message":{"id":"msg_compaction","type":"message","role":"assistant",
                    "model":MODEL,"content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":5,"output_tokens":0,
                        "iterations":[{"type":"compaction","input_tokens":5,
                            "output_tokens":1}]}}
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({"type":"content_block_start","index":0,
                "content_block":{"type":"thinking","thinking":"","signature":SIGNATURE}}),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "content_block_start",
            serde_json::json!({"type":"content_block_start","index":1,
                "content_block":{"type":"compaction","content":""}}),
        ),
        event(
            "content_block_delta",
            serde_json::json!({"type":"content_block_delta","index":1,
                "delta":{"type":"compaction_delta","content":OPAQUE}}),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":1}),
        ),
        event(
            "message_delta",
            serde_json::json!({"type":"message_delta",
                "delta":{"stop_reason":"compaction","stop_sequence":null},
                "usage":{"output_tokens":1}}),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn compaction_state() -> ProviderStateItem {
    ProviderStateItem::new(
        "anthropic",
        MODEL,
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[
                {"type":"thinking","thinking":"","signature":SIGNATURE},
                {"type":"compaction","content":OPAQUE,
                    "provider_extension":{"must_survive":true}},
                {"type":"text","text":"Continuing after compaction."}
            ]
        }),
    )
    .unwrap()
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: MODEL.to_owned(),
        display_name: "Claude Opus 5".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_000_000),
        max_output_tokens: Some(64_000),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            reasoning: CapabilitySupport::Supported,
            native_compaction: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft(inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        provider: "anthropic".to_owned(),
        model: MODEL.to_owned(),
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
        max_output_tokens: Some(8_192),
        purpose: CallPurpose::Conversation,
    }
}

fn compaction_draft(inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        native_features: vec![NativeFeature::Compaction],
        purpose: CallPurpose::Compaction,
        ..draft(inputs)
    }
}

#[test]
fn exact_definition_is_beta_gated_and_capability_specific() {
    assert_eq!(
        anthropic_compaction_support(MODEL),
        CapabilitySupport::Supported
    );
    assert_eq!(
        anthropic_compaction_support(ANTHROPIC_CLAUDE_OPUS_5),
        CapabilitySupport::Supported
    );
    assert_eq!(
        anthropic_compaction_support("unlisted-model"),
        CapabilitySupport::Unknown
    );

    let definition = AnthropicCompactionDefinition::new(Some(50_000), true).unwrap();
    assert_eq!(definition.beta_headers(), &["compact-2026-01-12"]);
    assert_eq!(
        definition.request_fields_for(MODEL).unwrap(),
        serde_json::json!({
            "context_management":{"edits":[{
                "type":"compact_20260112",
                "trigger":{"type":"input_tokens","value":50_000},
                "pause_after_compaction":true
            }]}
        })
    );
    assert_eq!(
        definition.request_fields_for("unlisted-model").unwrap_err(),
        AnthropicCompactionFault::UnprovenCapability
    );
}

#[tokio::test]
async fn provider_native_compaction_crosses_the_c12_adapter_and_returns_exact_state() {
    let transport = Arc::new(RecordingTransport::default());
    let provider = AnthropicProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(MODEL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            compaction_draft(vec![InferenceInput::Message(ChatMessage::user(
                "compact this conversation",
            ))]),
            &model(),
        )
        .unwrap();
    let checkpoint = adapter
        .native_compaction()
        .expect("Anthropic advertises the production C12 operation")
        .compact(call, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(checkpoint.provider(), "anthropic");
    assert_eq!(checkpoint.model(), MODEL);
    assert_eq!(checkpoint.protocol(), ProviderProtocol::AnthropicMessages);
    assert_eq!(checkpoint.items().len(), 1);
    let state = &checkpoint.items()[0];
    assert_eq!(state.data()["content"][1]["content"], OPAQUE);
    assert_eq!(state.data()["content"][0]["signature"], SIGNATURE);
    assert_eq!(checkpoint.usage().unwrap().prompt_tokens, 5);

    let continuation = adapter
        .resolve(
            draft(vec![
                InferenceInput::ProviderState(state.clone()),
                InferenceInput::Message(ChatMessage::user("continue after compaction")),
            ]),
            &model(),
        )
        .unwrap();
    let mut continuation_stream = adapter.stream(continuation);
    while let Some(event) = continuation_stream.next().await {
        event.unwrap();
    }

    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]["context_management"]["edits"][0]["type"],
        "compact_20260112"
    );
    assert_eq!(
        requests[0]["context_management"]["edits"][0]["pause_after_compaction"],
        true
    );
    assert_eq!(requests[1]["messages"][0], state.data().clone());
    let headers = transport.headers.lock().unwrap();
    assert_eq!(
        headers[0]
            .iter()
            .find(|(name, _)| name == "anthropic-beta")
            .map(|(_, value)| value.as_str()),
        Some("compact-2026-01-12")
    );
}

#[tokio::test]
async fn cancelled_native_compaction_settles_without_transport_or_checkpoint() {
    let transport = Arc::new(RecordingTransport::default());
    let provider = AnthropicProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(MODEL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            compaction_draft(vec![InferenceInput::Message(ChatMessage::user("compact"))]),
            &model(),
        )
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        adapter
            .native_compaction()
            .unwrap()
            .compact(call, cancellation)
            .await
            .unwrap_err(),
        heycode_llm::NativeCompactionError::Cancelled
    );
    assert!(transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn complete_assistant_state_is_durable_and_continues_byte_exact() {
    let state = compaction_state();
    let checkpoint = AnthropicCompactionCheckpoint::from_state(MODEL, state.clone()).unwrap();
    assert_eq!(checkpoint.state(), &state);
    assert_eq!(checkpoint.compaction(), &state.data()["content"][1]);
    assert!(!format!("{checkpoint:?}").contains(OPAQUE));

    let durable = serde_json::to_vec(checkpoint.state()).unwrap();
    let restored: ProviderStateItem = serde_json::from_slice(&durable).unwrap();
    assert_eq!(restored, state);

    let transport = Arc::new(RecordingTransport::default());
    let provider = AnthropicProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(MODEL.to_owned()),
    )
    .unwrap();
    let call = Provider::inference_adapter(&provider)
        .unwrap()
        .resolve(
            draft(vec![
                InferenceInput::ProviderState(restored),
                InferenceInput::Message(ChatMessage::user("continue")),
            ]),
            &model(),
        )
        .unwrap();
    let mut stream = Provider::inference_adapter(&provider).unwrap().stream(call);
    while let Some(event) = stream.next().await {
        event.unwrap();
    }

    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(&requests[0]["messages"][0], state.data());
    assert_eq!(requests[0]["messages"][0]["content"][1]["content"], OPAQUE);
    assert_eq!(
        requests[0]["messages"][0]["content"][1]["provider_extension"]["must_survive"],
        true
    );
}

#[test]
fn malformed_or_wrong_route_state_refuses_without_echoing_content() {
    let wrong = ProviderStateItem::new(
        "anthropic",
        MODEL,
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{"type":"compaction","content":""},
                       {"type":"text","text":OPAQUE}]
        }),
    )
    .unwrap();
    let fault = AnthropicCompactionCheckpoint::from_state(MODEL, wrong).unwrap_err();
    assert_eq!(fault, AnthropicCompactionFault::InvalidState);
    assert!(!format!("{fault:?} {fault}").contains(OPAQUE));

    let fault = AnthropicCompactionCheckpoint::from_state("unlisted-model", compaction_state())
        .unwrap_err();
    assert_eq!(fault, AnthropicCompactionFault::UnprovenCapability);
}

#[test]
fn post_compaction_continuation_withholds_pre_output_replay() {
    let provider = AnthropicProvider::new(
        HttpService::new(Arc::new(RecordingTransport::default())),
        "test-key",
        Some(MODEL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();

    let continuation = adapter
        .resolve(
            draft(vec![
                InferenceInput::ProviderState(compaction_state()),
                InferenceInput::Message(ChatMessage::user("continue after compaction")),
            ]),
            &model(),
        )
        .unwrap();
    assert!(
        continuation.native_features().is_empty(),
        "the turn after a native compaction carries no native feature, so only the compaction \
         block in provider state proves the request is not replayable"
    );
    assert_eq!(
        continuation.retry_spec().safety(),
        RetrySafety::Never,
        "a partially committed compacted turn must never be replayed pre-output"
    );

    let plain = adapter
        .resolve(
            draft(vec![InferenceInput::Message(ChatMessage::user("hello"))]),
            &model(),
        )
        .unwrap();
    assert_eq!(
        plain.retry_spec().safety(),
        RetrySafety::StatelessPreOutput,
        "a bare conversation turn keeps the neutral replayable policy"
    );
}

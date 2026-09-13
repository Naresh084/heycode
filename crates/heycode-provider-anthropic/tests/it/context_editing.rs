//! PAN05 exact context-editing policy, applied metadata and cache impact.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceInput, InputModality, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, Provider, RequestDraft,
};
use heycode_provider_anthropic::{
    ANTHROPIC_CLAUDE_OPUS_5, AnthropicCacheImpact, AnthropicContextEditKind,
    AnthropicContextEditReport, AnthropicContextEditingFault, AnthropicContextEditingPolicy,
    AnthropicProvider, AnthropicThinkingClear, AnthropicThinkingKeep, AnthropicToolClear,
    anthropic_context_editing_support,
};
use tokio_util::sync::CancellationToken;

type RecordedRequest = (serde_json::Value, Vec<(String, String)>);

#[derive(Default)]
struct RecordingTransport(Mutex<Vec<RecordedRequest>>);

impl HttpTransport for RecordingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.0.lock().unwrap().push((
            serde_json::from_slice(request.body().expect("Messages body")).unwrap(),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        ));
        Box::pin(futures::stream::iter(edit_script().into_iter().map(Ok)))
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

fn edit_script() -> Vec<SseEvent> {
    vec![
        event(
            "message_start",
            serde_json::json!({"type":"message_start","message":{
                "id":"msg_edit","type":"message","role":"assistant",
                "model":ANTHROPIC_CLAUDE_OPUS_5,"content":[],"stop_reason":null,
                "stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":0}}}),
        ),
        event(
            "message_delta",
            serde_json::json!({"type":"message_delta",
            "delta":{"stop_reason":"end_turn","stop_sequence":null},
            "usage":{"output_tokens":1},
            "context_management":{"applied_edits":[
                {"type":"clear_thinking_20251015","cleared_thinking_turns":2,
                    "cleared_input_tokens":1000},
                {"type":"clear_tool_uses_20250919","cleared_tool_uses":4,
                    "cleared_input_tokens":9000}
            ]}}),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        display_name: "Claude Opus 5".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_000_000),
        max_output_tokens: Some(128_000),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            reasoning: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft(provider: &AnthropicProvider) -> RequestDraft {
    RequestDraft {
        provider: "anthropic".to_owned(),
        model: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("continue"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Provider::request_options(provider),
        temperature: None,
        max_output_tokens: Some(1_024),
        purpose: CallPurpose::Conversation,
    }
}

#[test]
fn combined_policy_emits_thinking_first_and_exact_tool_clearing_controls() {
    assert_eq!(
        anthropic_context_editing_support(ANTHROPIC_CLAUDE_OPUS_5),
        CapabilitySupport::Supported
    );
    assert_eq!(
        anthropic_context_editing_support("unlisted-model"),
        CapabilitySupport::Unknown
    );
    let thinking = AnthropicThinkingClear::new(AnthropicThinkingKeep::Turns(2)).unwrap();
    let tools = AnthropicToolClear::new(
        Some(50_000),
        Some(5),
        Some(5_000),
        true,
        vec!["web_search".to_owned()],
    )
    .unwrap();
    let policy = AnthropicContextEditingPolicy::new(Some(thinking), Some(tools)).unwrap();

    assert_eq!(policy.beta_headers(), &["context-management-2025-06-27"]);
    assert_eq!(
        policy.request_fields(),
        &serde_json::json!({
            "context_management":{"edits":[
                {"type":"clear_thinking_20251015",
                 "keep":{"type":"thinking_turns","value":2}},
                {"type":"clear_tool_uses_20250919",
                 "trigger":{"type":"input_tokens","value":50_000},
                 "keep":{"type":"tool_uses","value":5},
                 "clear_at_least":{"type":"input_tokens","value":5_000},
                 "exclude_tools":["web_search"],
                 "clear_tool_inputs":true}
            ]}
        })
    );
    let rendered = format!("{policy:?}");
    assert!(!rendered.contains("web_search"));
}

#[test]
fn applied_metadata_is_lossless_and_cache_invalidation_is_explicit() {
    let terminal = serde_json::json!({
        "type":"message_delta",
        "delta":{"stop_reason":"end_turn","stop_sequence":null},
        "usage":{"output_tokens":1024},
        "context_management":{"applied_edits":[
            {"type":"clear_thinking_20251015","cleared_thinking_turns":3,
             "cleared_input_tokens":15_000,"future":{"kept":true}},
            {"type":"clear_tool_uses_20250919","cleared_tool_uses":8,
             "cleared_input_tokens":50_000,"future":{"kept":true}}
        ]}
    });
    let report = AnthropicContextEditReport::from_terminal(&terminal).unwrap();
    assert_eq!(report.edits().len(), 2);
    assert_eq!(
        report.edits()[0].kind(),
        AnthropicContextEditKind::ClearThinking
    );
    assert_eq!(report.edits()[0].cleared_units(), 3);
    assert_eq!(report.edits()[0].cleared_input_tokens(), 15_000);
    assert_eq!(
        report.edits()[0].raw(),
        &terminal["context_management"]["applied_edits"][0]
    );
    assert_eq!(
        report.edits()[1].kind(),
        AnthropicContextEditKind::ClearToolUses
    );
    assert_eq!(report.edits()[1].cleared_units(), 8);
    assert_eq!(report.total_cleared_input_tokens(), 65_000);
    assert_eq!(
        report.cache_impact(),
        AnthropicCacheImpact::InvalidatedAtEdit
    );
    assert!(!format!("{report:?}").contains("future"));

    let durable = serde_json::to_vec(report.edits()[0].raw()).unwrap();
    let restored: serde_json::Value = serde_json::from_slice(&durable).unwrap();
    assert_eq!(restored, *report.edits()[0].raw());

    let no_edits = AnthropicContextEditReport::from_terminal(&serde_json::json!({
        "context_management":{"applied_edits":[]}
    }))
    .unwrap();
    assert_eq!(no_edits.cache_impact(), AnthropicCacheImpact::Preserved);
    assert_eq!(no_edits.total_cleared_input_tokens(), 0);
}

#[test]
fn invalid_policy_or_metadata_refuses_without_echoing_provider_content() {
    let canary = "sk-ant CONTEXT EDIT SECRET CANARY";
    assert_eq!(
        AnthropicThinkingClear::new(AnthropicThinkingKeep::Turns(0)).unwrap_err(),
        AnthropicContextEditingFault::InvalidConfiguration
    );
    let fault =
        AnthropicToolClear::new(Some(50_000), Some(3), None, false, vec![canary.to_owned()])
            .unwrap_err();
    assert_eq!(fault, AnthropicContextEditingFault::InvalidConfiguration);
    assert!(!format!("{fault:?} {fault}").contains(canary));

    for terminal in [
        serde_json::json!({
            "context_management":{"applied_edits":[{
                "type":"future_edit","cleared_input_tokens":10,"content":canary
            }]}
        }),
        serde_json::json!({
            "context_management":{"applied_edits":[
                {"type":"clear_thinking_20251015","cleared_thinking_turns":1,
                 "cleared_input_tokens":u64::MAX},
                {"type":"clear_tool_uses_20250919","cleared_tool_uses":1,
                 "cleared_input_tokens":1}
            ]}
        }),
    ] {
        let fault = AnthropicContextEditReport::from_terminal(&terminal).unwrap_err();
        assert_eq!(fault, AnthropicContextEditingFault::InvalidMetadata);
        assert!(!format!("{fault:?} {fault}").contains(canary));
    }
}

#[tokio::test]
async fn configured_policy_reaches_messages_and_terminal_metadata_commits_once() {
    let policy = AnthropicContextEditingPolicy::new(
        Some(AnthropicThinkingClear::new(AnthropicThinkingKeep::Turns(2)).unwrap()),
        Some(AnthropicToolClear::new(Some(50_000), Some(5), None, false, Vec::new()).unwrap()),
    )
    .unwrap();
    let transport = Arc::new(RecordingTransport::default());
    let provider = AnthropicProvider::new(
        HttpService::new(transport.clone()),
        "test-key",
        Some(ANTHROPIC_CLAUDE_OPUS_5.to_owned()),
    )
    .unwrap()
    .with_context_editing(policy)
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter.resolve(draft(&provider), &model()).unwrap();
    assert_eq!(call.provider_options().len(), 1);
    assert_eq!(call.provider_options()[0].kind(), "context-editing");
    let mut stream = adapter.stream(call);
    let mut durable = None;
    while let Some(event) = stream.next().await {
        if let heycode_llm::InferenceEvent::ResponseMetadata(metadata) = event.unwrap() {
            durable = Some(metadata);
        }
    }
    let durable = durable.expect("neutral metadata precedes terminal usage/finish");
    assert_eq!(durable.context_edits().len(), 2);
    assert_eq!(durable.total_cleared_input_tokens(), Some(10_000));
    assert!(durable.invalidated_cache_prefix());

    let requests = transport.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].0["context_management"]["edits"][0]["type"],
        "clear_thinking_20251015"
    );
    assert_eq!(
        requests[0].0["context_management"]["edits"][1]["type"],
        "clear_tool_uses_20250919"
    );
    let beta = requests[0]
        .1
        .iter()
        .find(|(name, _)| name == "anthropic-beta")
        .map(|(_, value)| value.as_str());
    assert_eq!(beta, Some("context-management-2025-06-27"));
    drop(requests);

    let metadata = provider
        .take_response_metadata("msg_edit")
        .expect("metadata publishes only after successful Finish");
    let report = metadata
        .context_editing()
        .expect("applied edits are visible");
    assert_eq!(report.edits().len(), 2);
    assert_eq!(report.total_cleared_input_tokens(), 10_000);
    assert_eq!(
        report.cache_impact(),
        AnthropicCacheImpact::InvalidatedAtEdit
    );
    assert!(provider.take_response_metadata("msg_edit").is_none());
}

//! Q02 reusable raw-SSE fragmentation and failure matrix.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::StreamExt as _;
use heycode_llm::testing::{
    CatalogConformanceFixture, ConformanceFixtureError, ConformanceFixtureMetadata,
    ConformanceSourceKind, SseConformanceFixture, SseFixtureCase, run_sse_conformance,
};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatMessage,
    FinishReason, InferenceAdapter, InferenceEvent, InferenceInput, InputModality,
    ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing,
    OpenAiChatCompletionsAdapter, OpenAiChatCompletionsConfig, OpenAiResponsesAdapter,
    OpenAiResponsesConfig, ProviderDescriptor, ProviderProtocol, RequestDraft,
};

const CAPTURED_AT_MS: u64 = 1_788_048_000_000;

fn fixture_metadata(provider: &str, source: &str, version: &str) -> ConformanceFixtureMetadata {
    ConformanceFixtureMetadata::new(
        provider,
        ConformanceSourceKind::Synthetic,
        source,
        version,
        CAPTURED_AT_MS,
    )
    .unwrap()
}

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "fixture-provider".to_owned(),
        display_name: "Fixture Provider".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "fixture-model".to_owned(),
        display_name: "Fixture Model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(8_192),
        max_output_tokens: Some(2_048),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Unsupported,
            reasoning: CapabilitySupport::Unsupported,
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
        provider: "fixture-provider".to_owned(),
        model: "fixture-model".to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
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
        purpose: CallPurpose::Evaluation,
    }
}

async fn run_chat(
    http: heycode_http::HttpService,
) -> Vec<Result<InferenceEvent, heycode_llm::LlmError>> {
    let adapter = OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_key(
            provider(),
            "https://fixture.invalid/v1",
            "fixture-key",
        )
        .with_retry_spec(heycode_llm::RetrySpec::no_retry()),
        http,
    )
    .unwrap();
    let spec = adapter.descriptor();
    assert_eq!(spec.id, "fixture-provider");
    let call = adapter.resolve(draft(), &model()).unwrap();
    assert_eq!(
        call.authentication(),
        &AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new())
    );
    adapter.stream(call).collect().await
}

#[tokio::test]
async fn every_raw_sse_split_produces_the_same_normalized_adapter_events() {
    let wire = concat!(
        "data: {\"id\":\"chat_fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hé\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat_fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    let metadata = fixture_metadata(
        "fixture-provider",
        "https://platform.openai.com/docs/api-reference/chat",
        "chat-completions-v1",
    );
    let fixture =
        SseConformanceFixture::new("chat-happy", metadata.clone(), wire.as_bytes()).unwrap();
    let cases = fixture.fragmentation_cases();
    assert_eq!(cases.len(), wire.len() + 1);
    let runs = run_sse_conformance(&cases, run_chat).await;
    let expected = runs[0]
        .output
        .iter()
        .map(|event| event.as_ref().unwrap().clone())
        .collect::<Vec<_>>();
    assert!(matches!(
        expected.as_slice(),
        [
            InferenceEvent::ResponseStarted { .. },
            InferenceEvent::ItemStarted { .. },
            InferenceEvent::TextDelta(text),
            InferenceEvent::ItemFinished { .. },
            InferenceEvent::ProviderState(_),
            InferenceEvent::ResponseFinished { .. },
            InferenceEvent::Usage(_),
            InferenceEvent::Finish(FinishReason::Stop),
        ] if text == "hé"
    ));
    for run in runs {
        assert_eq!(run.metadata, metadata, "case {}", run.case);
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

#[tokio::test]
async fn the_same_fragmentation_runner_drives_a_second_protocol_adapter() {
    let wire = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"sequence_number\":0,\"response\":{\"id\":\"resp_fixture\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
    );
    let metadata = fixture_metadata(
        "responses-fixture",
        "https://platform.openai.com/docs/api-reference/responses",
        "responses-v1",
    );
    let cases = SseConformanceFixture::new("responses-terminal", metadata.clone(), wire.as_bytes())
        .unwrap()
        .fragmentation_cases();
    let runs = run_sse_conformance(&cases, |http| async move {
        let descriptor = ProviderDescriptor {
            id: "responses-fixture".to_owned(),
            display_name: "Responses Fixture".to_owned(),
            protocols: vec![ProviderProtocol::OpenAiResponses],
        };
        let adapter = OpenAiResponsesAdapter::new(
            OpenAiResponsesConfig::with_key(
                descriptor,
                "https://fixture.invalid/v1",
                "fixture-key",
            ),
            http,
        )
        .unwrap();
        let mut request = draft();
        request.provider = "responses-fixture".to_owned();
        let call = adapter.resolve(request, &model()).unwrap();
        adapter.stream(call).collect::<Vec<_>>().await
    })
    .await;
    for run in runs {
        assert_eq!(run.metadata, metadata, "case {}", run.case);
        assert_eq!(run.transport_calls, 1, "case {}", run.case);
        assert!(
            matches!(
                run.output.as_slice(),
                [
                    Ok(InferenceEvent::ResponseFinished { response_id, .. }),
                    Ok(InferenceEvent::Usage(_)),
                    Ok(InferenceEvent::Finish(FinishReason::Stop)),
                ] if response_id == "resp_fixture"
            ),
            "case {}: {:?}",
            run.case,
            run.output
        );
    }
}

#[tokio::test]
async fn reusable_failure_cases_terminate_without_a_fake_finish() {
    let cases = vec![
        SseFixtureCase::new(
            "disconnect-mid-event",
            fixture_metadata(
                "fixture-provider",
                "https://platform.openai.com/docs/api-reference/chat",
                "chat-completions-v1",
            ),
            [b"data: {\"id\":".as_slice()],
        )
        .unwrap()
        .with_terminal_error(heycode_http::TransportError::Network {
            message: "fixture disconnect".to_owned(),
        }),
        SseFixtureCase::new(
            "invalid-utf8",
            fixture_metadata(
                "fixture-provider",
                "https://html.spec.whatwg.org/multipage/server-sent-events.html",
                "living-standard-2026-08-30",
            ),
            [[0xff, b'\n', b'\n'].as_slice()],
        )
        .unwrap(),
    ];
    let runs = run_sse_conformance(&cases, run_chat).await;
    for run in runs {
        assert_eq!(run.transport_calls, 1, "case {}", run.case);
        assert_eq!(run.output.len(), 1, "case {}: {:?}", run.case, run.output);
        assert!(run.output[0].is_err(), "case {}", run.case);
        assert!(
            !run.output
                .iter()
                .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
        );
    }
}

#[test]
fn fixture_boundary_rejects_unsafe_labels_and_empty_wire() {
    let metadata = fixture_metadata(
        "fixture-provider",
        "https://platform.openai.com/docs/api-reference/chat",
        "chat-completions-v1",
    );
    assert!(matches!(
        SseConformanceFixture::new("bad\nlabel", metadata.clone(), b"data: ok\n\n"),
        Err(ConformanceFixtureError::InvalidLabel { .. })
    ));
    assert!(matches!(
        SseConformanceFixture::new("empty", metadata, b""),
        Err(ConformanceFixtureError::EmptyWire { .. })
    ));
}

#[test]
fn catalog_fixture_schema_round_trips_source_version_capture_and_payload() {
    let metadata = ConformanceFixtureMetadata::new(
        "openrouter",
        ConformanceSourceKind::RedactedCapture,
        "https://openrouter.ai/api/v1/model/z-ai/glm-5.3-flash",
        "api-v1",
        CAPTURED_AT_MS,
    )
    .unwrap();
    let fixture = CatalogConformanceFixture::new(
        "openrouter/glm-5.3-flash",
        metadata.clone(),
        serde_json::json!({
            "data": {
                "id": "z-ai/glm-5.3-flash",
                "supported_parameters": ["reasoning", "tool_choice", "tools"]
            }
        }),
    )
    .unwrap();

    let encoded = fixture.to_json().unwrap();
    let decoded = CatalogConformanceFixture::from_json(&encoded).unwrap();
    assert_eq!(decoded.name(), "openrouter/glm-5.3-flash");
    assert_eq!(decoded.metadata(), &metadata);
    assert_eq!(decoded.payload(), fixture.payload());
    assert_eq!(decoded.metadata().provider(), "openrouter");
    assert_eq!(
        decoded.metadata().source_kind(),
        ConformanceSourceKind::RedactedCapture
    );
    assert_eq!(decoded.metadata().source_version(), "api-v1");
    assert_eq!(decoded.metadata().captured_at_ms(), CAPTURED_AT_MS);
}

#[test]
fn catalog_fixture_schema_fails_loud_when_required_provenance_is_missing_or_forged() {
    let missing_capture = br#"{
        "schema_version":1,
        "name":"openrouter/models",
        "provider":"openrouter",
        "source_kind":"redacted_capture",
        "source":"https://openrouter.ai/api/v1/models",
        "source_version":"api-v1",
        "payload":{"data":[]}
    }"#;
    assert!(matches!(
        CatalogConformanceFixture::from_json(missing_capture),
        Err(ConformanceFixtureError::InvalidDocument { .. })
    ));

    let unsafe_source = ConformanceFixtureMetadata::new(
        "openrouter",
        ConformanceSourceKind::OfficialExample,
        "https://user:secret@openrouter.ai/api/v1/models",
        "api-v1",
        CAPTURED_AT_MS,
    );
    assert!(matches!(
        unsafe_source,
        Err(ConformanceFixtureError::InvalidMetadata { field: "source" })
    ));

    let future_schema = br#"{
        "schema_version":2,
        "name":"openrouter/models",
        "provider":"openrouter",
        "source_kind":"official_example",
        "source":"https://openrouter.ai/api/v1/models",
        "source_version":"api-v1",
        "captured_at_ms":1788048000000,
        "payload":{"data":[]}
    }"#;
    assert!(matches!(
        CatalogConformanceFixture::from_json(future_schema),
        Err(ConformanceFixtureError::UnsupportedSchema { found: 2 })
    ));
}

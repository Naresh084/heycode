//! Provider bindings through the shared raw SSE conformance runner.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use futures::StreamExt as _;
use heycode_llm::testing::{
    ConformanceFixtureMetadata, ConformanceSourceKind, SseConformanceFixture, run_sse_conformance,
};
use heycode_llm::{
    CallPurpose, ChatMessage, InferenceEvent, InferenceInput, InputModality, Provider,
    RequestDraft, RouteCredential,
};
use heycode_provider_compatible::{CompatibleProvider, builtin_specs};

#[tokio::test]
async fn each_provider_uses_its_exact_identity_and_streams_through_all_byte_splits() {
    let wire = concat!(
        "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hé\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    for spec in builtin_specs() {
        let metadata = ConformanceFixtureMetadata::new(
            spec.id,
            ConformanceSourceKind::Synthetic,
            "https://console.groq.com/docs/openai",
            "chat-v1",
            1_788_566_400_000,
        )
        .unwrap();
        let fixture =
            SseConformanceFixture::new("compatible-text", metadata, wire.as_bytes()).unwrap();
        let runs = run_sse_conformance(&fixture.fragmentation_cases(), |http| async move {
            let provider = CompatibleProvider::new(
                *spec,
                http,
                spec.base_url,
                spec.default_model,
                RouteCredential::fixed("fixture-key"),
            )
            .unwrap();
            assert_eq!(provider.info().name, spec.id);
            let adapter = provider.inference_adapter().unwrap();
            let draft = RequestDraft {
                provider: spec.id.into(),
                model: spec.default_model.into(),
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
            };
            let call = adapter
                .resolve(draft, &provider.describe_model(spec.default_model))
                .unwrap();
            adapter.stream(call).collect::<Vec<_>>().await
        })
        .await;
        let expected = runs[0]
            .output
            .iter()
            .map(|item| item.as_ref().unwrap().clone())
            .collect::<Vec<_>>();
        assert!(
            expected
                .iter()
                .any(|event| matches!(event, InferenceEvent::TextDelta(text) if text == "hé"))
        );
        assert!(matches!(expected.last(), Some(InferenceEvent::Finish(_))));
        for run in runs {
            assert_eq!(run.transport_calls, 1, "{} {}", spec.id, run.case);
            assert_eq!(
                run.output
                    .into_iter()
                    .map(Result::unwrap)
                    .collect::<Vec<_>>(),
                expected,
                "{} {}",
                spec.id,
                run.case
            );
        }
    }
}

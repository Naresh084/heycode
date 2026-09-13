//! Q04 reusable persisted-session replay oracle across native protocols.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpSseRequest, HttpTransport, SseEventStream,
};
use heycode_llm::{
    AnthropicMessagesAdapter, AnthropicMessagesConfig, BedrockConverseAdapter,
    BedrockConverseConfig, CallPurpose, CapabilitySupport, ChatMessage, GeminiAdapter,
    GeminiConfig, InferenceAdapter, InferenceInput, InputModality, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, OpenAiChatCompletionsAdapter,
    OpenAiChatCompletionsConfig, OpenAiResponsesAdapter, OpenAiResponsesConfig, ProviderDescriptor,
    ProviderProtocol, RequestDraft, RetrySpec, RouteCredential,
};
use tokio_util::sync::CancellationToken;

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }

    fn send(
        &self,
        _request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        Box::pin(async { Err(heycode_http::TransportError::Cancelled) })
    }
}

fn provider(protocol: ProviderProtocol) -> ProviderDescriptor {
    ProviderDescriptor {
        id: "oracle".to_owned(),
        display_name: "Replay oracle".to_owned(),
        protocols: vec![protocol],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: "oracle-model".to_owned(),
        display_name: "Oracle model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(16_384),
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
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "oracle".to_owned(),
        model: "oracle-model".to_owned(),
        catalog_revision: Some(4),
        catalog_fetched_at_ms: Some(5),
        effective_at_ms: 6,
        system: Some("stable system".to_owned()),
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: Some(1_024),
        purpose: CallPurpose::Evaluation,
    }
}

fn adapters() -> Vec<(&'static str, Box<dyn InferenceAdapter>)> {
    let http = heycode_http::HttpService::new(Arc::new(DeadTransport));
    let credential = RouteCredential::fixed("test-key");
    vec![
        (
            "chat",
            Box::new(
                OpenAiChatCompletionsAdapter::new(
                    OpenAiChatCompletionsConfig::with_credential(
                        provider(ProviderProtocol::OpenAiChatCompletions),
                        "https://oracle.test/v1",
                        credential.clone(),
                    )
                    .with_retry_spec(RetrySpec::no_retry()),
                    http.clone(),
                )
                .unwrap(),
            ),
        ),
        (
            "responses",
            Box::new(
                OpenAiResponsesAdapter::new(
                    OpenAiResponsesConfig::with_credential(
                        provider(ProviderProtocol::OpenAiResponses),
                        "https://oracle.test/v1",
                        credential.clone(),
                    )
                    .with_retry_spec(RetrySpec::no_retry()),
                    http.clone(),
                )
                .unwrap(),
            ),
        ),
        (
            "anthropic",
            Box::new(
                AnthropicMessagesAdapter::new(
                    AnthropicMessagesConfig::with_credential(
                        provider(ProviderProtocol::AnthropicMessages),
                        "https://oracle.test/v1",
                        credential.clone(),
                    )
                    .with_retry_spec(RetrySpec::no_retry()),
                    http.clone(),
                )
                .unwrap(),
            ),
        ),
        (
            "gemini",
            Box::new(
                GeminiAdapter::new(
                    GeminiConfig::with_credential(
                        provider(ProviderProtocol::GeminiGenerateContent),
                        "https://oracle.test/v1",
                        credential.clone(),
                    )
                    .with_retry_spec(RetrySpec::no_retry()),
                    http.clone(),
                )
                .unwrap(),
            ),
        ),
        (
            "bedrock",
            Box::new(
                BedrockConverseAdapter::new(
                    BedrockConverseConfig::with_credential(
                        provider(ProviderProtocol::BedrockConverse),
                        "https://oracle.test",
                        credential,
                    )
                    .with_retry_spec(RetrySpec::no_retry()),
                    http,
                )
                .unwrap(),
            ),
        ),
    ]
}

#[test]
fn every_native_protocol_reconstructs_from_reopened_jsonl_before_dispatch() {
    for (label, adapter) in adapters() {
        let root = tempfile::tempdir().unwrap();
        let mut session = heycode_session::Session::create(root.path()).unwrap();
        session
            .append(heycode_session::SessionEventKind::UserMessage {
                text: "hello".to_owned(),
            })
            .unwrap();
        let call = adapter.resolve(draft(), &model()).unwrap();
        let request_id = heycode_core::RequestId::from_raw(format!("request-{label}"));
        let verified = heycode_agent::testing::verify_persisted_replay(
            &mut session,
            1,
            1,
            request_id.clone(),
            call,
            adapter.as_ref(),
            None,
        )
        .unwrap_or_else(|error| panic!("{label}: {error}"));
        drop(verified);

        let reopened = heycode_session::Session::open(session.path().parent().unwrap()).unwrap();
        let projected = heycode_session::project_requests(reopened.events()).unwrap();
        assert_eq!(projected.len(), 1, "{label}");
        assert_eq!(projected[0].request_id, request_id, "{label}");
        assert!(matches!(
            projected[0].inputs.as_slice(),
            [heycode_session::ProjectedInput::Message(message)] if message.content == "hello"
        ));
    }
}

#[test]
fn oracle_reads_physical_jsonl_and_its_failure_never_echoes_request_content() {
    use std::io::{Seek as _, Write as _};

    let root = tempfile::tempdir().unwrap();
    let mut session = heycode_session::Session::create(root.path()).unwrap();
    session
        .append(heycode_session::SessionEventKind::UserMessage {
            text: "hello".to_owned(),
        })
        .unwrap();
    let mut raw = std::fs::OpenOptions::new()
        .write(true)
        .open(session.path())
        .unwrap();
    raw.seek(std::io::SeekFrom::Start(0)).unwrap();
    raw.write_all(b"!").unwrap();
    raw.sync_data().unwrap();

    let (_, adapter) = adapters().into_iter().next().unwrap();
    let mut request = draft();
    request.system = Some("private-oracle-system-canary".to_owned());
    let call = adapter.resolve(request, &model()).unwrap();
    let error = match heycode_agent::testing::verify_persisted_replay(
        &mut session,
        1,
        1,
        heycode_core::RequestId::from_raw("request-corrupt"),
        call,
        adapter.as_ref(),
        None,
    ) {
        Ok(_) => panic!("physical corruption must fail the replay oracle"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        heycode_agent::testing::ReplayOracleError::Reopen
    ));
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains("private-oracle-system-canary"),
        "{rendered}"
    );
}

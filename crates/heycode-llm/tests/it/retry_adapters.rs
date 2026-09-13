//! P08 retry execution across all current native protocol adapters.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt as _;
use heycode_http::{
    HttpErrorMetadata, HttpRetryAfter, HttpSseRequest, HttpTransport, SseEvent, SseEventStream,
    TransportError,
};
use heycode_llm::{
    AnthropicMessagesAdapter, AnthropicMessagesConfig, CallPurpose, CapabilitySupport, ChatMessage,
    DeepSeekProvider, FinishReason, InferenceAdapter, InferenceEvent, InferenceInput,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, NativeFeature, OpenAiChatCompletionsAdapter, OpenAiChatCompletionsConfig,
    OpenAiResponsesAdapter, OpenAiResponsesConfig, OpenRouterProvider, ProviderDescriptor,
    ProviderErrorClass, ProviderProtocol, RequestDraft, RetryJitter, RetrySafety, RetrySpec,
};
use tokio_util::sync::CancellationToken;

struct AttemptTransport {
    attempts: Mutex<VecDeque<Vec<Result<SseEvent, TransportError>>>>,
    calls: AtomicUsize,
}

impl AttemptTransport {
    fn new(attempts: Vec<Vec<Result<SseEvent, TransportError>>>) -> Arc<Self> {
        Arc::new(Self {
            attempts: Mutex::new(attempts.into()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl HttpTransport for AttemptTransport {
    fn sse(&self, _request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if cancellation.is_cancelled() {
            return Box::pin(futures::stream::once(async {
                Err(TransportError::Cancelled)
            }));
        }
        let attempt = self
            .attempts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                vec![Err(TransportError::Network {
                    message: "unexpected extra attempt".to_owned(),
                })]
            });
        Box::pin(futures::stream::iter(attempt))
    }
}

fn event(event: &str, value: serde_json::Value) -> Result<SseEvent, TransportError> {
    Ok(SseEvent {
        event: event.to_owned(),
        data: value.to_string(),
        id: None,
        retry_ms: None,
    })
}

fn http_error(
    status: u16,
    retry_after: Option<HttpRetryAfter>,
) -> Result<SseEvent, TransportError> {
    Err(TransportError::http(
        status,
        r#"{"error":{"type":"overloaded_error","message":"secret body"}}"#,
        HttpErrorMetadata::new(retry_after, None),
    ))
}

fn retry_spec(max_retry_after: Duration) -> RetrySpec {
    RetrySpec::new(
        3,
        Duration::from_millis(1),
        Duration::from_millis(2),
        max_retry_after,
        RetryJitter::None,
        RetrySafety::StatelessPreOutput,
    )
    .unwrap()
}

fn provider(id: &str, protocol: ProviderProtocol) -> ProviderDescriptor {
    ProviderDescriptor {
        id: id.to_owned(),
        display_name: id.to_owned(),
        protocols: vec![protocol],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "model".to_owned(),
        display_name: "Model".to_owned(),
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

fn draft(provider: &str) -> RequestDraft {
    RequestDraft {
        provider: provider.to_owned(),
        model: "model".to_owned(),
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
        max_output_tokens: Some(1_024),
        purpose: CallPurpose::Evaluation,
    }
}

fn chat_success(id: &str) -> Vec<Result<SseEvent, TransportError>> {
    vec![
        event(
            "message",
            serde_json::json!({
                "id":id,
                "choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":2,"completion_tokens":1}
            }),
        ),
        Ok(SseEvent {
            event: "message".to_owned(),
            data: "[DONE]".to_owned(),
            id: None,
            retry_ms: None,
        }),
    ]
}

fn responses_success(id: &str) -> Vec<Result<SseEvent, TransportError>> {
    vec![event(
        "response.completed",
        serde_json::json!({
            "type":"response.completed","sequence_number":0,
            "response":{"id":id,"status":"completed","output":[],
                "usage":{"input_tokens":2,"output_tokens":1}}
        }),
    )]
}

fn anthropic_success(id: &str) -> Vec<Result<SseEvent, TransportError>> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start","message":{
                    "id":id,"type":"message","role":"assistant","model":"model",
                    "content":[],"stop_reason":null,"stop_sequence":null,
                    "usage":{"input_tokens":2,"output_tokens":1}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({"type":"content_block_start","index":0,
                "content_block":{"type":"text","text":""}}),
        ),
        event(
            "content_block_delta",
            serde_json::json!({"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"ok"}}),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "message_delta",
            serde_json::json!({"type":"message_delta",
                "delta":{"stop_reason":"end_turn","stop_sequence":null},
                "usage":{"output_tokens":2}}),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

#[tokio::test]
async fn chat_responses_and_anthropic_retry_definitive_pre_output_failures() {
    let retry = retry_spec(Duration::from_secs(10));

    let chat_transport = AttemptTransport::new(vec![
        vec![http_error(503, None)],
        chat_success("chat_retry"),
    ]);
    let chat = OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_key(
            provider("chat", ProviderProtocol::OpenAiChatCompletions),
            "https://chat.test/v1",
            "key",
        )
        .with_retry_spec(retry.clone()),
        heycode_http::HttpService::new(chat_transport.clone()),
    )
    .unwrap();
    let call = chat.resolve(draft("chat"), &model()).unwrap();
    assert_eq!(call.retry_spec(), &retry);
    let output = chat.stream(call).collect::<Vec<_>>().await;
    assert_eq!(chat_transport.calls(), 2);
    assert!(matches!(
        output.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
    ));

    let responses_transport = AttemptTransport::new(vec![
        vec![event(
            "response.failed",
            serde_json::json!({
                "type":"response.failed","sequence_number":0,
                "response":{"id":"resp_failed","status":"failed","error":{"code":"server_error"}}
            }),
        )],
        responses_success("resp_retry"),
    ]);
    let responses = OpenAiResponsesAdapter::new(
        OpenAiResponsesConfig::with_key(
            provider("responses", ProviderProtocol::OpenAiResponses),
            "https://responses.test/v1",
            "key",
        )
        .with_retry_spec(retry.clone()),
        heycode_http::HttpService::new(responses_transport.clone()),
    )
    .unwrap();
    let call = responses.resolve(draft("responses"), &model()).unwrap();
    let output = responses.stream(call).collect::<Vec<_>>().await;
    assert_eq!(responses_transport.calls(), 2);
    assert!(matches!(
        output.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
    ));

    let anthropic_transport = AttemptTransport::new(vec![
        vec![event(
            "error",
            serde_json::json!({"type":"error","error":{"type":"overloaded_error"}}),
        )],
        anthropic_success("msg_retry"),
    ]);
    let anthropic = AnthropicMessagesAdapter::new(
        AnthropicMessagesConfig::with_key(
            provider("anthropic", ProviderProtocol::AnthropicMessages),
            "https://anthropic.test/v1",
            "key",
        )
        .with_retry_spec(retry)
        .with_default_max_output_tokens(Some(1_024)),
        heycode_http::HttpService::new(anthropic_transport.clone()),
    )
    .unwrap();
    let call = anthropic.resolve(draft("anthropic"), &model()).unwrap();
    let output = anthropic.stream(call).collect::<Vec<_>>().await;
    assert_eq!(anthropic_transport.calls(), 2);
    assert!(matches!(
        output.last(),
        Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
    ));
}

#[tokio::test]
async fn emitted_output_prevents_retry_and_preserves_the_terminal_failure() {
    let transport = AttemptTransport::new(vec![
        vec![
            event(
                "message",
                serde_json::json!({
                    "id":"chat_partial",
                    "choices":[{"index":0,"delta":{
                        "content":"partial",
                        "tool_calls":[{"index":0,"id":"call_partial","type":"function",
                            "function":{"name":"read","arguments":"{"}}]
                    },"finish_reason":null}]
                }),
            ),
            Err(TransportError::Network {
                message: "disconnect".to_owned(),
            }),
        ],
        chat_success("must_not_run"),
    ]);
    let adapter = OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_key(
            provider("chat", ProviderProtocol::OpenAiChatCompletions),
            "https://chat.test/v1",
            "key",
        )
        .with_retry_spec(retry_spec(Duration::from_secs(10))),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let call = adapter.resolve(draft("chat"), &model()).unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert_eq!(transport.calls(), 1);
    assert!(output.iter().any(|item| matches!(
        item,
        Ok(InferenceEvent::TextDelta(text)) if text == "partial"
    )));
    assert!(
        output
            .iter()
            .any(|item| matches!(item, Ok(InferenceEvent::ToolCallDelta { .. })))
    );
    assert_eq!(
        output.last().unwrap().as_ref().unwrap_err().class(),
        ProviderErrorClass::Network
    );
}

#[tokio::test]
async fn pre_cancel_never_dispatches_and_retry_wait_is_cancellable() {
    let transport = AttemptTransport::new(vec![chat_success("must_not_dispatch")]);
    let adapter = OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_key(
            provider("chat", ProviderProtocol::OpenAiChatCompletions),
            "https://chat.test/v1",
            "key",
        )
        .with_retry_spec(retry_spec(Duration::from_secs(120))),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let call = adapter.resolve(draft("chat"), &model()).unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let output = adapter
        .stream_cancellable(call, cancelled)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(transport.calls(), 0);
    assert_eq!(
        output[0].as_ref().unwrap_err().class(),
        ProviderErrorClass::Cancelled
    );

    let transport = AttemptTransport::new(vec![
        vec![http_error(
            429,
            Some(HttpRetryAfter::Delay(Duration::from_secs(60))),
        )],
        chat_success("must_not_retry_after_cancel"),
    ]);
    let adapter = OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_key(
            provider("chat", ProviderProtocol::OpenAiChatCompletions),
            "https://chat.test/v1",
            "key",
        )
        .with_retry_spec(retry_spec(Duration::from_secs(120))),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let call = adapter.resolve(draft("chat"), &model()).unwrap();
    let cancellation = CancellationToken::new();
    let mut stream = adapter.stream_cancellable(call, cancellation.clone());
    let waiter = tokio::spawn(async move { stream.next().await });
    for _ in 0..100 {
        if transport.calls() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(transport.calls(), 1);
    cancellation.cancel();
    let item = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        item.as_ref().unwrap_err().class(),
        ProviderErrorClass::Cancelled
    );
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn native_server_and_container_calls_disable_all_automatic_replay() {
    let transport = AttemptTransport::new(vec![
        vec![http_error(503, None)],
        anthropic_success("must_not_replay"),
    ]);
    let adapter = AnthropicMessagesAdapter::new(
        AnthropicMessagesConfig::with_key(
            provider("anthropic", ProviderProtocol::AnthropicMessages),
            "https://anthropic.test/v1",
            "key",
        )
        .with_default_max_output_tokens(Some(1_024))
        .with_retry_spec(retry_spec(Duration::from_secs(10)))
        .with_server_tool(
            NativeFeature::Web,
            serde_json::json!({"type":"web_search_20250305","name":"web_search"}),
        ),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let mut request = draft("anthropic");
    request.native_features.push(NativeFeature::Web);
    let mut native_model = model();
    native_model.capabilities.native_web = CapabilitySupport::Supported;
    let call = adapter.resolve(request, &native_model).unwrap();
    assert_eq!(call.retry_spec().safety(), RetrySafety::Never);
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert_eq!(transport.calls(), 1);
    assert_eq!(
        output[0].as_ref().unwrap_err().class(),
        ProviderErrorClass::Overloaded
    );

    let container_transport = AttemptTransport::new(vec![
        vec![http_error(503, None)],
        anthropic_success("must_not_replay_container"),
    ]);
    let container_adapter = AnthropicMessagesAdapter::new(
        AnthropicMessagesConfig::with_key(
            provider("anthropic", ProviderProtocol::AnthropicMessages),
            "https://anthropic.test/v1",
            "key",
        )
        .with_default_max_output_tokens(Some(1_024))
        .with_retry_spec(retry_spec(Duration::from_secs(10))),
        heycode_http::HttpService::new(container_transport.clone()),
    )
    .unwrap();
    let state = heycode_llm::ProviderStateItem::new(
        "anthropic",
        "model",
        ProviderProtocol::AnthropicMessages,
        heycode_llm::ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{"type":"text","text":"prior"}],
            "container":{"id":"container_1","expires_at":"2026-08-25T01:02:03Z"}
        }),
    )
    .unwrap();
    let mut container_request = draft("anthropic");
    container_request
        .inputs
        .push(InferenceInput::ProviderState(state));
    let call = container_adapter
        .resolve(container_request, &model())
        .unwrap();
    assert_eq!(call.retry_spec().safety(), RetrySafety::Never);
    let output = container_adapter.stream(call).collect::<Vec<_>>().await;
    assert_eq!(container_transport.calls(), 1);
    assert_eq!(
        output[0].as_ref().unwrap_err().class(),
        ProviderErrorClass::Overloaded
    );

    let responses_transport = AttemptTransport::new(vec![
        vec![http_error(503, None)],
        responses_success("must_not_replay_responses"),
    ]);
    let responses = OpenAiResponsesAdapter::new(
        OpenAiResponsesConfig::with_key(
            provider("responses", ProviderProtocol::OpenAiResponses),
            "https://responses.test/v1",
            "key",
        )
        .with_retry_spec(retry_spec(Duration::from_secs(10))),
        heycode_http::HttpService::new(responses_transport.clone()),
    )
    .unwrap();
    let mut request = draft("responses");
    request.native_features.push(NativeFeature::Web);
    let mut native_model = model();
    native_model.capabilities.native_web = CapabilitySupport::Supported;
    let call = responses.resolve(request, &native_model).unwrap();
    assert_eq!(call.retry_spec().safety(), RetrySafety::Never);
    let output = responses.stream(call).collect::<Vec<_>>().await;
    assert_eq!(responses_transport.calls(), 1);
    assert_eq!(
        output[0].as_ref().unwrap_err().class(),
        ProviderErrorClass::Overloaded
    );
}

#[tokio::test]
async fn openai_and_anthropic_provider_events_classify_without_echoing_payloads() {
    let chat_transport = AttemptTransport::new(vec![vec![event(
        "message",
        serde_json::json!({
            "error":{"type":"overloaded_error","message":"chat secret"}
        }),
    )]]);
    let chat = OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_key(
            provider("chat", ProviderProtocol::OpenAiChatCompletions),
            "https://chat.test/v1",
            "key",
        )
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(chat_transport),
    )
    .unwrap();
    let error = chat
        .stream(chat.resolve(draft("chat"), &model()).unwrap())
        .collect::<Vec<_>>()
        .await
        .remove(0)
        .unwrap_err();
    assert_eq!(error.class(), ProviderErrorClass::Overloaded);
    assert!(!format!("{error:?} {error}").contains("overloaded_error"));
    assert!(!format!("{error:?} {error}").contains("chat secret"));

    let responses_transport = AttemptTransport::new(vec![vec![event(
        "response.failed",
        serde_json::json!({
            "type":"response.failed","sequence_number":0,
            "response":{"id":"resp_failed","status":"failed",
                "error":{"code":"server_error","message":"responses secret"}}
        }),
    )]]);
    let responses = OpenAiResponsesAdapter::new(
        OpenAiResponsesConfig::with_key(
            provider("responses", ProviderProtocol::OpenAiResponses),
            "https://responses.test/v1",
            "key",
        )
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(responses_transport),
    )
    .unwrap();
    let error = responses
        .stream(responses.resolve(draft("responses"), &model()).unwrap())
        .collect::<Vec<_>>()
        .await
        .remove(0)
        .unwrap_err();
    assert_eq!(error.class(), ProviderErrorClass::Server);
    assert!(!format!("{error:?} {error}").contains("server_error"));
    assert!(!format!("{error:?} {error}").contains("responses secret"));

    let anthropic_transport = AttemptTransport::new(vec![vec![event(
        "error",
        serde_json::json!({
            "type":"error","error":{"type":"overloaded_error","message":"anthropic secret"}
        }),
    )]]);
    let anthropic = AnthropicMessagesAdapter::new(
        AnthropicMessagesConfig::with_key(
            provider("anthropic", ProviderProtocol::AnthropicMessages),
            "https://anthropic.test/v1",
            "key",
        )
        .with_default_max_output_tokens(Some(1_024))
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(anthropic_transport),
    )
    .unwrap();
    let error = anthropic
        .stream(anthropic.resolve(draft("anthropic"), &model()).unwrap())
        .collect::<Vec<_>>()
        .await
        .remove(0)
        .unwrap_err();
    assert_eq!(error.class(), ProviderErrorClass::Overloaded);
    assert!(!format!("{error:?} {error}").contains("overloaded_error"));
    assert!(!format!("{error:?} {error}").contains("anthropic secret"));
}

#[tokio::test]
async fn branded_chat_adapters_forward_cancellation_to_the_protocol_adapter() {
    for (provider_id, model_id, is_deepseek) in [
        (
            DeepSeekProvider::NAME,
            DeepSeekProvider::DEFAULT_MODEL,
            true,
        ),
        (
            OpenRouterProvider::NAME,
            OpenRouterProvider::DEFAULT_MODEL,
            false,
        ),
    ] {
        let transport = AttemptTransport::new(vec![chat_success("must_not_dispatch")]);
        let http = heycode_http::HttpService::new(transport.clone());
        let adapter: Box<dyn InferenceAdapter> = if is_deepseek {
            Box::new(
                DeepSeekProvider::from_key_with_transport("key", Some(model_id.to_owned()), http)
                    .unwrap(),
            )
        } else {
            Box::new(
                OpenRouterProvider::from_key_with_transport(
                    "key",
                    Some(model_id.to_owned()),
                    http,
                    super::openrouter_transform_options(),
                )
                .unwrap(),
            )
        };
        let mut request = draft(provider_id);
        request.model = model_id.to_owned();
        let mut descriptor = model();
        descriptor.id = model_id.to_owned();
        let call = adapter.resolve(request, &descriptor).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let output = adapter
            .stream_cancellable(call, cancellation)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(transport.calls(), 0);
        assert_eq!(
            output[0].as_ref().unwrap_err().class(),
            ProviderErrorClass::Cancelled
        );
    }
}

fn neutral_spec() -> heycode_llm::ResolveSpec {
    heycode_llm::ResolveSpec {
        protocol: ProviderProtocol::OpenAiChatCompletions,
        target: heycode_llm::InferenceTarget::Http {
            base_url: "https://neutral.test/v1".to_owned(),
        },
        authentication: heycode_llm::AuthenticationBinding::AdapterOwned(
            heycode_llm::AdapterOwnedAuth::new(),
        ),
        default_max_output_tokens: Some(1_024),
        reasoning_efforts: Vec::new(),
        default_reasoning_effort: None,
    }
}

fn neutral_resolve(draft: RequestDraft, model: &ModelDescriptor) -> heycode_llm::ResolvedCall {
    heycode_llm::resolve_request(
        &provider("neutral", ProviderProtocol::OpenAiChatCompletions),
        draft,
        model,
        &neutral_spec(),
    )
    .unwrap()
}

#[test]
fn neutral_resolution_hands_an_out_of_crate_adapter_a_retrying_policy() {
    let call = neutral_resolve(draft("neutral"), &model());
    assert_eq!(call.retry_spec(), &RetrySpec::standard());
}

#[test]
fn neutral_resolution_withholds_replay_from_provider_executed_routes() {
    let mut native = model();
    native.capabilities.native_web = CapabilitySupport::Supported;
    let withheld = RetrySpec::standard().with_safety(RetrySafety::Never);
    let mut feature_draft = draft("neutral");
    feature_draft.native_features = vec![NativeFeature::Web];
    assert_eq!(
        neutral_resolve(feature_draft, &native).retry_spec(),
        &withheld
    );

    let mut route_draft = draft("neutral");
    route_draft.native_tool_routes = vec![
        heycode_core::NativeToolRoute::new(
            "web_search",
            "neutral:web",
            heycode_core::NativeToolImplementationKind::Provider,
            Some("neutral".to_owned()),
        )
        .unwrap(),
    ];
    assert_eq!(
        neutral_resolve(route_draft, &native).retry_spec(),
        &withheld
    );
}

#[test]
fn an_out_of_crate_adapter_can_narrow_the_resolved_retry_policy() {
    let call = neutral_resolve(draft("neutral"), &model()).with_retry_spec(RetrySpec::no_retry());
    assert_eq!(call.retry_spec(), &RetrySpec::no_retry());
}

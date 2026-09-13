//! Actual MiniMax adapter lifecycle and continuation, using synthetic SSE only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use futures::StreamExt as _;
use heycode_core::Context;
use heycode_credentials::*;
use heycode_http::*;
use heycode_llm::*;
use heycode_provider_minimax::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

struct Keys {
    id: CredentialProviderId,
    value: Arc<Mutex<String>>,
}
impl CredentialProvider for Keys {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }
    fn precedence(&self) -> u16 {
        0
    }
    fn inspect(&self, _: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(CredentialProviderState::configured(
            CredentialSource::Environment,
            false,
        ))
    }
    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        assert_eq!(query.reference.as_str(), "MINIMAX_API_KEY");
        assert_eq!(query.kind.as_str(), "api-key");
        Ok(Some(CredentialSecret::new(
            self.value.lock().unwrap().clone(),
        )))
    }
}
struct Transport {
    requests: Mutex<Vec<(Value, String)>>,
    content: &'static str,
    hold: bool,
}
impl HttpTransport for Transport {
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        assert_eq!(request.url(), "https://api.minimax.io/v1/chat/completions");
        self.requests.lock().unwrap().push((
            serde_json::from_slice(request.body().unwrap()).unwrap(),
            request
                .headers()
                .iter()
                .find(|h| h.name() == "authorization")
                .unwrap()
                .value()
                .into(),
        ));
        if self.hold {
            return Box::pin(futures::stream::once(async move {
                cancellation.cancelled().await;
                Err(TransportError::Cancelled)
            }));
        }
        let event = json!({"id":"mm1","choices":[{"index":0,"delta":{"content":self.content,"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":"task","arguments":"{\"prompt\":\"inspect\",\"background\":true}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":40,"completion_tokens":12}});
        Box::pin(futures::stream::iter([
            Ok(SseEvent {
                event: "message".into(),
                data: event.to_string(),
                id: None,
                retry_ms: None,
            }),
            Ok(SseEvent {
                event: "message".into(),
                data: "[DONE]".into(),
                id: None,
                retry_ms: None,
            }),
        ]))
    }
}
fn world(
    content: &'static str,
    hold: bool,
) -> (
    Context,
    MiniMaxInference<PayAsYouGo>,
    Arc<Transport>,
    Arc<Mutex<String>>,
) {
    let context = Context::new();
    let keys = Arc::new(Mutex::new("fixture-key-1".into()));
    let credentials = CredentialsService::new();
    credentials
        .register(
            &context,
            Arc::new(Keys {
                id: CredentialProviderId::new("fixture").unwrap(),
                value: keys.clone(),
            }),
        )
        .unwrap();
    let transport = Arc::new(Transport {
        requests: Mutex::new(vec![]),
        content,
        hold,
    });
    let route = MiniMaxInference::new(
        HttpService::new(transport.clone()),
        credentials,
        MiniMaxProfile::<PayAsYouGo>::international(),
        MINIMAX_M3.into(),
    )
    .unwrap();
    (context, route, transport, keys)
}
fn draft() -> RequestDraft {
    RequestDraft {
        provider: "minimax".into(),
        model: MINIMAX_M3.into(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("inspect"))],
        tools: vec![ToolSpec {
            name: "task".into(),
            description: "start a task".into(),
            parameters: json!({"type":"object","properties":{"prompt":{"type":"string"},"background":{"type":"boolean"}},"required":["prompt"]}),
        }],
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: vec![],
        native_tool_routes: vec![],
        provider_options: vec![],
        temperature: None,
        max_output_tokens: Some(128),
        purpose: CallPurpose::Conversation,
    }
}
#[tokio::test]
async fn actual_adapter_preserves_think_tags_and_tool_state_with_rotated_credentials() {
    let (_context, route, transport, keys) = world("<think>inspect carefully</think>", false);
    let model = route.describe_model(MINIMAX_M3);
    assert_eq!(model.capabilities.tools, CapabilitySupport::Unknown);
    let call = route.resolve(draft(), &model).unwrap();
    assert_eq!(call.authentication(), &route.authentication_binding());
    let events: Vec<_> = InferenceAdapter::stream(&route, call).collect().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let state = events
        .iter()
        .find_map(|event| {
            if let Ok(InferenceEvent::ProviderState(state)) = event {
                Some(state.clone())
            } else {
                None
            }
        })
        .unwrap();
    *keys.lock().unwrap() = "fixture-key-2".into();
    let mut next = draft();
    next.inputs
        .push(InferenceInput::ProviderState(state.clone()));
    next.inputs.push(InferenceInput::Message(ChatMessage::tool(
        "call1",
        "{\"job_id\":\"job1\"}",
    )));
    let next = route.resolve(next, &model).unwrap();
    let _: Vec<_> = InferenceAdapter::stream(&route, next).collect().await;
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests[0].0["reasoning_split"], false);
    assert_eq!(requests[1].0["messages"][1], *state.data());
    assert_eq!(requests[1].0["messages"][2]["tool_call_id"], "call1");
    assert_eq!(requests[0].1, "Bearer fixture-key-1");
    assert_eq!(requests[1].1, "Bearer fixture-key-2");
    assert_eq!(model.capabilities.tools, CapabilitySupport::Unknown);
}
#[tokio::test]
async fn truncated_reasoning_never_publishes_state_or_finish() {
    let (_context, route, _transport, _keys) = world("<think>truncated", false);
    let call = route
        .resolve(draft(), &route.describe_model(MINIMAX_M3))
        .unwrap();
    let events: Vec<_> = InferenceAdapter::stream(&route, call).collect().await;
    assert!(events.iter().any(Result::is_err));
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(_) | InferenceEvent::Finish(_))
    )));
}
#[tokio::test]
async fn wrong_product_key_is_refused_before_transport_even_after_rotation() {
    let (_context, route, transport, keys) = world("<think>valid</think>", false);
    *keys.lock().unwrap() = "sk-cp-wrong-product".into();
    let call = route
        .resolve(draft(), &route.describe_model(MINIMAX_M3))
        .unwrap();
    let events: Vec<_> = InferenceAdapter::stream(&route, call).collect().await;
    assert!(events.iter().any(Result::is_err));
    assert!(transport.requests.lock().unwrap().is_empty());
    assert!(!format!("{events:?}").contains("sk-cp-wrong-product"));
}
#[tokio::test]
async fn cancellation_reaches_the_active_transport_and_settles_without_finish() {
    let (_context, route, transport, _keys) = world("", true);
    let call = route
        .resolve(draft(), &route.describe_model(MINIMAX_M3))
        .unwrap();
    let cancel = CancellationToken::new();
    let stream = route.stream_cancellable(call, cancel.clone());
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
    cancel.cancel();
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        stream.collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    assert!(events.iter().any(Result::is_err));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Ok(InferenceEvent::Finish(_))))
    );
}
#[test]
fn explicit_unsupported_tools_and_neutral_lossy_history_are_refused() {
    let (_context, route, transport, _keys) = world("", false);
    let mut model = route.describe_model(MINIMAX_M3);
    model.capabilities.tools = CapabilitySupport::Unsupported;
    assert!(route.resolve(draft(), &model).is_err());
    model.capabilities.tools = CapabilitySupport::Unknown;
    let mut request = draft();
    request.inputs.push(InferenceInput::Message(
        ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "call1".into(),
                name: "task".into(),
                arguments: "{}".into(),
            }],
        ),
    ));
    assert!(route.resolve(request, &model).is_err());
    assert!(transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn m3_images_use_the_documented_detail_without_inventing_m2_vision() {
    let (_context, route, transport, _keys) = world("<think>inspect</think>", false);
    let mut request = draft();
    request.inputs = vec![InferenceInput::Message(ChatMessage::user_with_images(
        "inspect",
        vec![
            ChatImage::new(
                heycode_core::AttachmentMediaType::new("image/png").unwrap(),
                vec![1, 2, 3],
            )
            .unwrap(),
        ],
    ))];
    request.input_modalities.push(InputModality::Image);
    let call = route
        .resolve(request.clone(), &route.describe_model(MINIMAX_M3))
        .unwrap();
    let _: Vec<_> = InferenceAdapter::stream(&route, call).collect().await;
    assert_eq!(
        transport.requests.lock().unwrap()[0].0["messages"][0]["content"][1]["image_url"]["detail"],
        "default"
    );
    request.model = "MiniMax-M2.7".into();
    assert!(
        route
            .resolve(request, &route.describe_model("MiniMax-M2.7"))
            .is_err()
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

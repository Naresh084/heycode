//! DeepSeek and OpenRouter consume the same provider-neutral HTTP service.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt as _;
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    ChatMessage, ChatRequest, DeepSeekProvider, FinishReason, OpenRouterProvider, Provider,
    StreamChunk,
};
use tokio_util::sync::CancellationToken;

struct ScriptedTransport {
    calls: AtomicUsize,
}

impl HttpTransport for ScriptedTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter([
            Ok(SseEvent {
                event: "message".to_owned(),
                data: r#"{"id":"chat_shared","choices":[{"index":0,"delta":{"content":"shared"},"finish_reason":null}]}"#.to_owned(),
                id: None,
                retry_ms: None,
            }),
            Ok(SseEvent {
                event: "message".to_owned(),
                data: r#"{"id":"chat_shared","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#.to_owned(),
                id: None,
                retry_ms: None,
            }),
            Ok(SseEvent {
                event: "message".to_owned(),
                data: "[DONE]".to_owned(),
                id: None,
                retry_ms: None,
            }),
        ]))
    }
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![ChatMessage::user("hello")],
        tools: None,
        temperature: None,
        max_tokens: None,
    }
}

#[tokio::test]
async fn both_legacy_adapters_share_transport_but_own_event_meaning() {
    let transport = Arc::new(ScriptedTransport {
        calls: AtomicUsize::new(0),
    });
    let service = HttpService::new(transport.clone());
    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(DeepSeekProvider::from_key_with_transport("test", None, service.clone()).unwrap()),
        Box::new(
            OpenRouterProvider::from_key_with_transport(
                "test",
                None,
                service,
                super::openrouter_transform_options(),
            )
            .unwrap(),
        ),
    ];

    for provider in providers {
        let mut stream = provider.stream(request(&provider.info().default_model));
        let chunks = stream.by_ref().collect::<Vec<_>>().await;
        assert!(matches!(
            &chunks[..],
            [
                Ok(StreamChunk::TextDelta(text)),
                Ok(StreamChunk::Usage(usage)),
                Ok(StreamChunk::Finish(FinishReason::Stop)),
            ] if text == "shared"
                && usage.prompt_tokens == 2
                && usage.completion_tokens == 1
        ));
    }
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_configured_base_url_replaces_the_official_host_for_inference() {
    use std::sync::Mutex;
    struct RecordingTransport(Mutex<Vec<String>>);
    impl HttpTransport for RecordingTransport {
        fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
            self.0.lock().unwrap().push(request.url().to_string());
            Box::pin(futures::stream::iter([Ok(SseEvent {
                event: "message".to_owned(),
                data: "[DONE]".to_owned(),
                id: None,
                retry_ms: None,
            })]))
        }
    }
    let transport = Arc::new(RecordingTransport(Mutex::new(Vec::new())));
    let service = HttpService::new(transport.clone());
    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(
            DeepSeekProvider::from_credential_with_transport_at(
                heycode_llm::RouteCredential::fixed("gateway-key"),
                None,
                service.clone(),
                "http://127.0.0.1:9/deepseek/",
            )
            .unwrap(),
        ),
        Box::new(
            OpenRouterProvider::from_credential_with_transport_at(
                heycode_llm::RouteCredential::fixed("gateway-key"),
                None,
                service,
                super::openrouter_transform_options(),
                "http://127.0.0.1:9/openrouter",
            )
            .unwrap(),
        ),
    ];
    for provider in providers {
        let mut stream = provider.stream(request(&provider.info().default_model));
        let _ = stream.by_ref().collect::<Vec<_>>().await;
    }
    let urls = transport.0.lock().unwrap().clone();
    assert_eq!(
        urls,
        [
            "http://127.0.0.1:9/deepseek/chat/completions",
            "http://127.0.0.1:9/openrouter/chat/completions",
        ],
        "no request names the official host"
    );
}

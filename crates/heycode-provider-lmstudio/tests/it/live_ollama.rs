//! Explicitly gated installed Ollama picker and content-withheld Chat smoke.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use heycode_http::{HttpService, ReqwestHttpTransport};
use heycode_llm::{ChatMessage, ChatRequest, FinishReason, Provider, StreamChunk};
use heycode_provider_lmstudio::{OllamaEndpoint, OllamaInference, OllamaInspector};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn installed_ollama_picker_and_chat_smoke_are_explicit_and_tool_free() {
    if std::env::var("HEYCODE_OLLAMA_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let model = std::env::var("HEYCODE_OLLAMA_MODEL")
        .expect("HEYCODE_OLLAMA_MODEL is required for the explicit canary");
    let http = HttpService::new(Arc::new(
        ReqwestHttpTransport::with_timeout(Duration::from_secs(30)).unwrap(),
    ));
    let endpoint = OllamaEndpoint::local();
    let readiness = tokio::time::timeout(
        Duration::from_secs(20),
        OllamaInspector::new(http.clone(), endpoint.clone())
            .inspect(&model, CancellationToken::new()),
    )
    .await
    .expect("installed Ollama readiness inspection timed out")
    .unwrap();
    assert_eq!(readiness.model(), model);
    assert!(!readiness.live_smoke_passed());

    let inference = OllamaInference::new(http, endpoint, model.clone()).unwrap();
    let request = ChatRequest {
        model,
        messages: vec![ChatMessage::user(
            "Reply with a brief acknowledgement. No tools are available.",
        )],
        tools: None,
        temperature: Some(0.0),
        max_tokens: Some(32),
    };
    let mut stream = Provider::stream(&inference, request);
    let mut generated_text = false;
    let mut tool_call = false;
    let mut stream_failed = false;
    let mut finish = None;
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await
        .expect("installed Ollama Chat stream timed out")
    {
        match chunk {
            Ok(StreamChunk::TextDelta(text)) => generated_text |= !text.trim().is_empty(),
            Ok(StreamChunk::ToolCallDelta { .. }) => tool_call = true,
            Ok(StreamChunk::Finish(reason)) => finish = Some(reason),
            Ok(StreamChunk::ReasoningDelta(_) | StreamChunk::Usage(_)) => {}
            Err(_) => {
                stream_failed = true;
                break;
            }
        }
    }

    assert!(!stream_failed, "installed Ollama Chat stream failed");
    assert!(!tool_call, "installed Ollama Chat canary requested a tool");
    assert!(
        generated_text,
        "installed Ollama Chat canary returned no text"
    );
    assert_eq!(finish, Some(FinishReason::Stop));
}

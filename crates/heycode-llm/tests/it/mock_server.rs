//! Wire-level tests against a handcrafted HTTP/SSE server on localhost.
//! No real network: every connection lands on a `tokio::net::TcpListener`
//! serving canned bytes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::time::Duration;

use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use heycode_llm::{
    ChatMessage, ChatRequest, LlmError, OpenAiCompatClient, OpenAiCompatConfig, StreamChunk,
};

/// Key env var name guaranteed unset, for missing-key tests.
const UNSET_KEY_ENV: &str = "HEYCODE_TEST_NO_KEY_42";
/// Per-test ceiling so a broken server fails fast instead of hanging CI.
const TIMEOUT: Duration = Duration::from_secs(5);

fn client(base_url: String) -> OpenAiCompatClient {
    OpenAiCompatClient::with_key(
        OpenAiCompatConfig {
            base_url,
            api_key_env: "HEYCODE_TEST_MOCK_KEY",
            extra_headers: vec![("X-Test".to_owned(), "yes".to_owned())],
        },
        "test-key-1",
    )
    .unwrap()
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "test-model".into(),
        messages: vec![ChatMessage::user("hi")],
        tools: None,
        temperature: Some(0.2),
        max_tokens: None,
    }
}

/// Serve up to three sequential connections with canned `response` bytes
/// after reading each request head (plus its content-length body), then
/// close. Returns the base URL to dial.
async fn spawn_server(response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..3 {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            if read_full_request(&mut socket).await.is_err() {
                return;
            }
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// Read until the request head terminator and any declared body arrived.
async fn read_full_request(
    socket: &mut tokio::net::TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let head_end = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4);
        if let Some(head_end) = head_end {
            let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let body_start = head_end;
            if let Some(len) = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
            {
                let len: usize = len.trim().parse()?;
                if buf.len() >= body_start + len {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn drain(stream: &mut heycode_llm::ChunkStream) -> Vec<Result<StreamChunk, LlmError>> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

const HAPPY_SSE: &str = concat!(
    r#"data: {"id":"chat_test","choices":[{"index":0,"delta":{"role":"assistant","content":"He"},"finish_reason":null}]}"#,
    "\n\n",
    r#"data: {"id":"chat_test","choices":[{"index":0,"delta":{"content":"llo"}}]}"#,
    "\n\n",
    r#"data: {"id":"chat_test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"p\""}}]}}]}"#,
    "\n\n",
    r#"data: {"id":"chat_test","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#,
    "\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn happy_path_decodes_sse_and_sends_contract_headers() {
    // Capture the request so the wire contract is pinned end-to-end.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{HAPPY_SSE}"
    )
    .into_bytes();
    let captured = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let n = socket.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let _ = socket.write_all(&response).await;
        let _ = socket.shutdown().await;
        String::from_utf8_lossy(&buf).to_lowercase()
    });

    let mut stream = client(format!("http://{addr}")).stream("test-model", &request());
    let items = timeout(drain(&mut stream)).await;

    assert_eq!(items.len(), 5, "got {items:?}");
    assert!(matches!(&items[0], Ok(StreamChunk::TextDelta(text)) if text == "He"));
    assert!(matches!(&items[1], Ok(StreamChunk::TextDelta(text)) if text == "llo"));
    match &items[2] {
        Ok(StreamChunk::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        }) => {
            assert_eq!(
                (
                    *index,
                    id.as_deref(),
                    name.as_deref(),
                    arguments_delta.as_str()
                ),
                (0, Some("call_1"), Some("read"), "{\"p\"")
            );
        }
        other => panic!("expected tool-call delta, got {other:?}"),
    }
    // The stream contract: Usage immediately before Finish, nothing after.
    assert!(matches!(&items[3], Ok(StreamChunk::Usage(usage))
        if usage.prompt_tokens == 11 && usage.completion_tokens == 7));
    assert!(matches!(
        &items[4],
        Ok(StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls))
    ));

    let request_text = captured.await.unwrap();
    assert!(
        request_text.starts_with("post /chat/completions "),
        "head was: {request_text}"
    );
    assert!(request_text.contains("authorization: bearer test-key-1"));
    assert!(request_text.contains("x-test: yes"));
    assert!(
        request_text.contains("\"stream\":true") && request_text.contains("\"include_usage\":true"),
        "body must enable streaming with usage: {request_text}"
    );
}

#[tokio::test]
async fn non_2xx_becomes_one_body_free_classified_error() {
    let status_line = "HTTP/1.1 401 Unauthorized\r\n";
    let body = "{\"error\":{\"message\":\"invalid api key\"}}";
    let response =
        format!("{status_line}Content-Type: application/json\r\nConnection: close\r\n\r\n{body}")
            .into_bytes();

    let mut stream = client(spawn_server(response).await).stream("m", &request());
    let items = timeout(drain(&mut stream)).await;

    assert_eq!(items.len(), 1);
    let error = items[0].as_ref().unwrap_err();
    assert_eq!(
        error.class(),
        heycode_llm::ProviderErrorClass::Authentication
    );
    assert!(!format!("{error:?} {error}").contains("invalid api key"));
}

#[tokio::test]
async fn unreachable_endpoint_yields_transport_error() {
    // Bind then drop the listener, leaving a closed port.
    let addr = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();

    let mut stream = client(format!("http://{addr}")).stream("m", &request());
    let items = timeout(drain(&mut stream)).await;

    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].as_ref().unwrap_err().class(),
        heycode_llm::ProviderErrorClass::Network,
        "got {items:?}"
    );
}

#[tokio::test]
async fn constructing_without_key_fails_loud() {
    let err = OpenAiCompatClient::new(OpenAiCompatConfig {
        base_url: "https://example.invalid".into(),
        api_key_env: UNSET_KEY_ENV,
        extra_headers: Vec::new(),
    })
    .unwrap_err();
    assert!(matches!(err, LlmError::MissingApiKey { .. }), "got {err:?}");
}

/// Run `future` under [`TIMEOUT`], panicking with context on expiry.
async fn timeout<T>(future: impl Future<Output = T>) -> T {
    match tokio::time::timeout(TIMEOUT, future).await {
        Ok(value) => value,
        Err(_) => panic!("test exceeded {TIMEOUT:?}"),
    }
}

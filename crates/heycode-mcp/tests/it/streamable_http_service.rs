//! MCP03 production-path fixtures: initialize, paginated list, call and cancel
//! over real HTTP through the composed `http` service.
//!
//! Nothing here is scripted at the heycode-mcp boundary — the bytes cross a real
//! socket, are formed by `heycode_http::ReqwestHttpTransport` and are decoded by
//! the same protocol engine the plugin uses.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_http::{HttpService, ReqwestHttpTransport};
use heycode_mcp::{
    McpHttpError, McpNotificationRouter, McpRequestChannel, McpStreamableHttpClient,
    McpStreamableHttpTransport, McpTimeouts,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const SESSION: &str = "LIVE-SESSION-CANARY";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Answer every request promptly with a well-formed session.
    Normal,
    /// Accept the request and never answer, so cancellation must win.
    Stall,
    /// Answer with a session id outside the visible-ASCII contract.
    MalformedSession,
}

#[derive(Clone)]
struct RecordedRequest {
    method: String,
    headers: Vec<(String, String)>,
    body: serde_json::Value,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct MockServer {
    endpoint: String,
    seen: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: CancellationToken,
}

impl MockServer {
    async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = CancellationToken::new();
        let accepted = Arc::clone(&seen);
        let cancelled = stop.clone();
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    () = cancelled.cancelled() => return,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => return,
                    },
                };
                let seen = Arc::clone(&accepted);
                tokio::spawn(async move { serve(stream, seen, mode).await });
            }
        });
        Self {
            endpoint: format!("http://127.0.0.1:{port}/mcp"),
            seen,
            stop,
        }
    }

    fn seen(&self) -> Vec<RecordedRequest> {
        self.seen.lock().unwrap().clone()
    }

    fn client(&self, router: McpNotificationRouter) -> McpStreamableHttpClient {
        let http = HttpService::new(Arc::new(ReqwestHttpTransport::new().unwrap()));
        let definition =
            McpStreamableHttpTransport::new(self.endpoint.clone(), BTreeMap::new()).unwrap();
        McpStreamableHttpClient::new(
            http,
            &definition,
            router,
            McpTimeouts::new(5_000, 5_000, 60_000, 5_000).unwrap(),
        )
        .unwrap()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn serve(mut stream: TcpStream, seen: Arc<Mutex<Vec<RecordedRequest>>>, mode: Mode) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    seen.lock().unwrap().push(request.clone());
    if mode == Mode::Stall {
        tokio::time::sleep(Duration::from_secs(30)).await;
        return;
    }
    let response = respond(mode, &request.method, &request.body);
    let _written = stream.write_all(response.as_bytes()).await;
    let _flushed = stream.flush().await;
}

async fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..read]);
        if let Some(index) = find(&raw, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let method = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').next())
        .unwrap_or_default()
        .to_owned();
    let mut headers = Vec::new();
    let mut length = 0_usize;
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_owned();
        let value = value.trim().to_owned();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().unwrap_or_default();
        }
        headers.push((name, value));
    }
    while raw.len() < head_end + length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    let body = serde_json::from_slice(&raw[head_end..head_end + length])
        .unwrap_or(serde_json::Value::Null);
    Some(RecordedRequest {
        method,
        headers,
        body,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn respond(mode: Mode, http_method: &str, body: &serde_json::Value) -> String {
    if http_method == "DELETE" {
        return "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    }
    let id = body.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = body
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match method {
        "initialize" => session_response(
            mode,
            &serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "live-fixture", "version": "9.0"}
                }
            }),
        ),
        "notifications/initialized" => {
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        }
        "tools/list" => {
            let cursor = body
                .get("params")
                .and_then(|params| params.get("cursor"))
                .and_then(serde_json::Value::as_str);
            match cursor {
                // An empty string is a valid cursor: more results follow.
                None => json_response(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "tools": [{"name": "alpha", "description": "first",
                                   "inputSchema": {"type": "object"}}],
                        "nextCursor": ""
                    }
                })),
                Some("") => sse_response(&format!(
                    "event: message\ndata: {}\n\n",
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {"tools": [{"name": "beta", "description": "second",
                                              "inputSchema": {"type": "object"}}]}
                    })
                )),
                Some(_) => json_response(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32602, "message": "invalid cursor"}
                })),
            }
        }
        "tools/call" => sse_response(&format!(
            ": keep-alive\n\ndata: {}\n\ndata: {}\n\n",
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
            serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"content": [{"type": "text", "text": "ECHO"}], "isError": false}
            })
        )),
        _ => json_response(&serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": "unknown method"}
        })),
    }
}

fn session_response(mode: Mode, body: &serde_json::Value) -> String {
    let session = if mode == Mode::MalformedSession {
        "not a session"
    } else {
        SESSION
    };
    let body = body.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: {session}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn json_response(body: &serde_json::Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sse_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn initialize_list_and_call_round_trip_over_the_composed_http_service() {
    let server = MockServer::start(Mode::Normal).await;
    let router = McpNotificationRouter::new();
    let client = server.client(router.clone());
    let cancellation = CancellationToken::new();

    let handshake = client.initialize(&cancellation).await.unwrap();
    assert_eq!(handshake.protocol_version(), "2025-11-25");
    assert_eq!(handshake.server_name(), "live-fixture");
    assert!(handshake.capabilities().tools);

    let first = client
        .call("tools/list", serde_json::json!({}), &cancellation)
        .await
        .unwrap();
    assert_eq!(first["tools"][0]["name"], "alpha");
    assert_eq!(first["nextCursor"], "");

    // The second page arrives as an SSE response stream for the same method.
    let second = client
        .call(
            "tools/list",
            serde_json::json!({"cursor": ""}),
            &cancellation,
        )
        .await
        .unwrap();
    assert_eq!(second["tools"][0]["name"], "beta");
    assert!(second.get("nextCursor").is_none());

    let before = router.tools().epoch();
    let called = client
        .call(
            "tools/call",
            serde_json::json!({"name": "alpha", "arguments": {}}),
            &cancellation,
        )
        .await
        .unwrap();
    assert_eq!(called["content"][0]["text"], "ECHO");
    assert_ne!(
        router.tools().epoch(),
        before,
        "a list-changed notification on a live response stream must invalidate listings"
    );

    client.terminate(&cancellation).await.unwrap();

    let seen = server.seen();
    assert_eq!(seen.len(), 6);
    for request in &seen {
        assert!(
            request.header("origin").is_none(),
            "a non-browser client must never assert a browser Origin"
        );
    }
    assert_eq!(
        seen[0].header("accept"),
        Some("application/json, text/event-stream")
    );
    assert!(
        seen[0].header("mcp-session-id").is_none(),
        "the InitializeRequest carries no session id"
    );
    assert!(seen[0].header("mcp-protocol-version").is_none());
    for request in seen.iter().take(5).skip(1) {
        assert_eq!(
            request.header("accept"),
            Some("application/json, text/event-stream")
        );
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(request.header("mcp-protocol-version"), Some("2025-11-25"));
        assert_eq!(
            request.header("mcp-session-id"),
            Some(SESSION),
            "the server-assigned session must be echoed on every later request"
        );
    }
    let terminate = &seen[5];
    assert_eq!(terminate.method, "DELETE");
    assert_eq!(terminate.header("mcp-session-id"), Some(SESSION));
    assert_eq!(terminate.header("mcp-protocol-version"), Some("2025-11-25"));

    // The session is gone: a second termination sends nothing.
    client.terminate(&cancellation).await.unwrap();
    assert_eq!(server.seen().len(), 6);
}

#[tokio::test]
async fn a_cancelled_live_request_settles_without_a_response() {
    let server = MockServer::start(Mode::Stall).await;
    let client = server.client(McpNotificationRouter::new());
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel.cancel();
    });

    let error = client.initialize(&cancellation).await.unwrap_err();

    assert_eq!(error, McpHttpError::Cancelled);
}

#[tokio::test]
async fn a_malformed_live_session_id_is_refused_before_it_reaches_a_request_header() {
    let server = MockServer::start(Mode::MalformedSession).await;
    let client = server.client(McpNotificationRouter::new());

    let error = client
        .initialize(&CancellationToken::new())
        .await
        .unwrap_err();

    assert!(
        matches!(
            error,
            McpHttpError::InvalidField {
                field: "session id",
                ..
            }
        ),
        "a session id outside visible ASCII must never be echoed back"
    );
    assert_eq!(
        server.seen().len(),
        1,
        "the handshake must not be confirmed on an unusable session"
    );
}

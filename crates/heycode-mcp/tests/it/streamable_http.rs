//! MCP03 Streamable HTTP transport contracts.
//!
//! Every case drives the production protocol engine against a scripted
//! `heycode_http::HttpTransport`, so the bytes travel the same composed-service
//! path production uses. The exact wire obligations of the legacy Streamable
//! HTTP revisions (`2025-06-18` / `2025-11-25`) are pinned: header set, session
//! identity, content negotiation, session termination, cancellation budgets
//! and bounded body-free failures.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpResponse, HttpService, HttpSseRequest,
    HttpTransport, SseEventStream, TransportError,
};
use heycode_mcp::{
    McpHttpError, McpNotificationRouter, McpProtocolVersion, McpSessionId, McpStreamableHttpClient,
    McpStreamableHttpTransport, McpTimeouts,
};
use tokio_util::sync::CancellationToken;

const ENDPOINT: &str = "https://mcp.example.test/mcp-URL-CANARY";
const SESSION: &str = "SESSION-CANARY-01HZ";
const REPLACEMENT_SESSION: &str = "SESSION-CANARY-SECOND";

#[derive(Clone)]
struct RecordedRequest {
    method: HttpMethod,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(self.body.as_deref().unwrap_or(b"null")).unwrap()
    }
}

enum Scripted {
    Reply(HttpResponse),
    Failure(TransportError),
    Stall,
}

struct ScriptedTransport {
    replies: Mutex<VecDeque<Scripted>>,
    seen: Mutex<Vec<RecordedRequest>>,
}

impl ScriptedTransport {
    fn with(replies: Vec<Scripted>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<RecordedRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl HttpTransport for ScriptedTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        // Recorded unconditionally, so "sends nothing" assertions pin the
        // client's own pre-dispatch cancellation check, not the double's.
        self.seen.lock().unwrap().push(RecordedRequest {
            method: request.method(),
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().map(<[u8]>::to_vec),
        });
        let scripted = self.replies.lock().unwrap().pop_front();
        Box::pin(async move {
            match scripted {
                Some(Scripted::Reply(response)) => Ok(response),
                Some(Scripted::Failure(error)) => Err(error),
                Some(Scripted::Stall) => {
                    cancellation.cancelled().await;
                    Err(TransportError::Cancelled)
                }
                None => panic!("unscripted MCP HTTP request"),
            }
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        // This transport speaks only the buffered path; an SSE response arrives
        // as a `text/event-stream` body on `send`.
        Box::pin(futures::stream::empty())
    }
}

fn initialize_result(id: u64, protocol_version: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": protocol_version,
            "capabilities": {"tools": {"listChanged": true}, "logging": {}},
            "serverInfo": {"name": "fixture", "version": "3.1"}
        }
    })
}

fn response(status: u16, content_type: Option<&str>, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status,
        content_type: content_type.map(str::to_owned),
        headers: BTreeMap::new(),
        body,
    }
}

fn json_reply(status: u16, body: &serde_json::Value) -> Scripted {
    Scripted::Reply(response(
        status,
        Some("application/json"),
        serde_json::to_vec(body).unwrap(),
    ))
}

fn session_reply(status: u16, body: &serde_json::Value, session: &str) -> Scripted {
    let mut reply = response(
        status,
        Some("application/json"),
        serde_json::to_vec(body).unwrap(),
    );
    reply
        .headers
        .insert("mcp-session-id".to_owned(), session.to_owned());
    Scripted::Reply(reply)
}

fn sse_reply(status: u16, raw: &str) -> Scripted {
    Scripted::Reply(response(
        status,
        Some("text/event-stream"),
        raw.as_bytes().to_vec(),
    ))
}

fn accepted() -> Scripted {
    Scripted::Reply(response(202, None, Vec::new()))
}

fn status_reply(status: u16, body: &str) -> Scripted {
    Scripted::Reply(response(
        status,
        Some("application/json"),
        body.as_bytes().to_vec(),
    ))
}

fn endpoint() -> McpStreamableHttpTransport {
    McpStreamableHttpTransport::new(ENDPOINT, BTreeMap::new()).unwrap()
}

fn service(transport: &Arc<ScriptedTransport>) -> HttpService {
    HttpService::new(Arc::clone(transport) as Arc<dyn HttpTransport>)
}

fn client(transport: &Arc<ScriptedTransport>) -> McpStreamableHttpClient {
    McpStreamableHttpClient::new(
        service(transport),
        &endpoint(),
        McpNotificationRouter::new(),
        McpTimeouts::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn initialize_sends_the_exact_required_headers_without_a_session_id() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
    ]);
    let client = client(&transport);

    let handshake = client.initialize(&CancellationToken::new()).await.unwrap();

    assert_eq!(handshake.protocol_version(), "2025-11-25");
    assert_eq!(handshake.server_name(), "fixture");
    assert_eq!(handshake.server_version(), "3.1");
    assert!(handshake.capabilities().tools);
    assert!(handshake.capabilities().logging);
    assert!(!handshake.capabilities().prompts);

    let seen = transport.seen();
    assert_eq!(seen.len(), 2, "initialize must be followed by initialized");

    let initialize = &seen[0];
    assert_eq!(initialize.method, HttpMethod::Post);
    assert_eq!(initialize.url, ENDPOINT);
    assert_eq!(
        initialize.header("accept"),
        Some("application/json, text/event-stream")
    );
    assert_eq!(initialize.header("content-type"), Some("application/json"));
    assert!(
        initialize.header("mcp-session-id").is_none(),
        "no session exists before the InitializeResult"
    );
    assert!(
        initialize.header("mcp-protocol-version").is_none(),
        "no version is negotiated before the InitializeResult"
    );
    assert!(
        initialize.header("origin").is_none(),
        "a non-browser client must never assert a browser Origin"
    );
    assert!(initialize.header("last-event-id").is_none());
    let body = initialize.json();
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["method"], "initialize");
    assert_eq!(body["params"]["protocolVersion"], "2025-11-25");
    assert_eq!(body["params"]["clientInfo"]["name"], "heycode");
    assert!(body["id"].is_number());

    let initialized = &seen[1];
    assert_eq!(initialized.json()["method"], "notifications/initialized");
    assert!(
        initialized.json().get("id").is_none(),
        "a notification must carry no id"
    );
    assert_eq!(initialized.header("mcp-session-id"), Some(SESSION));
    assert_eq!(
        initialized.header("mcp-protocol-version"),
        Some("2025-11-25")
    );
}

#[tokio::test]
async fn every_request_after_initialize_echoes_the_session_and_negotiated_version() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-06-18"), SESSION),
        accepted(),
        json_reply(
            200,
            &serde_json::json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}}),
        ),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    let result = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result, serde_json::json!({"tools": []}));
    let seen = transport.seen();
    let list = seen.last().unwrap();
    assert_eq!(list.header("mcp-session-id"), Some(SESSION));
    assert_eq!(list.header("mcp-protocol-version"), Some("2025-06-18"));
    assert_eq!(
        list.header("accept"),
        Some("application/json, text/event-stream")
    );
    assert_eq!(list.json()["method"], "tools/list");
}

#[tokio::test]
async fn a_request_accepts_both_json_and_event_stream_responses() {
    let stream = concat!(
        ": keep-alive comment\n",
        "\n",
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[{\"name\":\"streamed\"}]}}\n",
        "\n",
    );
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        json_reply(
            200,
            &serde_json::json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"buffered"}]}}),
        ),
        sse_reply(200, stream),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    let buffered = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let streamed = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(buffered["tools"][0]["name"], "buffered");
    assert_eq!(streamed["tools"][0]["name"], "streamed");
}

#[tokio::test]
async fn a_response_stream_notification_marks_the_list_change_watch() {
    let stream = concat!(
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n",
        "\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n",
        "\n",
    );
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        sse_reply(200, stream),
    ]);
    let router = McpNotificationRouter::new();
    let client = McpStreamableHttpClient::new(
        service(&transport),
        &endpoint(),
        router.clone(),
        McpTimeouts::default(),
    )
    .unwrap();
    client.initialize(&CancellationToken::new()).await.unwrap();
    let before = router.tools().epoch();

    client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_ne!(
        router.tools().epoch(),
        before,
        "a list-changed notification on the response stream must invalidate cached listings"
    );
}

/// Every family a server can announce on a response stream must be routed, and
/// each into its own epoch. Before MCP08/MCP09 the transport recognised only
/// `tools/list_changed`, so a resource update arriving on the same stream as a
/// tool response was silently discarded.
#[tokio::test]
async fn a_response_stream_routes_every_notification_family_into_its_own_epoch() {
    let stream = concat!(
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/prompts/list_changed\"}\n",
        "\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/list_changed\"}\n",
        "\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n",
        "\n",
    );
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        sse_reply(200, stream),
    ]);
    let router = McpNotificationRouter::new();
    let client = McpStreamableHttpClient::new(
        service(&transport),
        &endpoint(),
        router.clone(),
        McpTimeouts::default(),
    )
    .unwrap();
    client.initialize(&CancellationToken::new()).await.unwrap();

    client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        router.prompts().epoch(),
        1,
        "the prompt walk is invalidated"
    );
    assert_eq!(
        router.resources().inspect().list_change_epoch(),
        1,
        "the resource walk is invalidated"
    );
    assert_eq!(
        router.tools().epoch(),
        0,
        "neither announcement is a tool-list change"
    );
}

/// The session exposes the same routing plane its generation owners consume, so
/// a Consumer cannot bind a watch the transport never feeds.
#[tokio::test]
async fn the_session_exposes_the_routing_plane_it_feeds() {
    let transport = ScriptedTransport::with(vec![]);
    let router = McpNotificationRouter::new();
    let client = McpStreamableHttpClient::new(
        service(&transport),
        &endpoint(),
        router.clone(),
        McpTimeouts::default(),
    )
    .unwrap();

    client.router().observe(
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
    );

    assert_eq!(router.tools().epoch(), 1);
}

#[tokio::test]
async fn a_terminated_session_reinitializes_without_a_session_id_and_retries_once() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        status_reply(404, "{\"error\":\"session gone\"}"),
        session_reply(
            200,
            &initialize_result(3, "2025-11-25"),
            REPLACEMENT_SESSION,
        ),
        accepted(),
        json_reply(
            200,
            &serde_json::json!({"jsonrpc":"2.0","id":4,"result":{"tools":[]}}),
        ),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    let result = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result, serde_json::json!({"tools": []}));
    let seen = transport.seen();
    assert_eq!(seen.len(), 6);
    assert_eq!(seen[2].header("mcp-session-id"), Some(SESSION));
    assert_eq!(seen[3].json()["method"], "initialize");
    assert!(
        seen[3].header("mcp-session-id").is_none(),
        "a new session MUST be started without the terminated session id"
    );
    assert_eq!(
        seen[5].header("mcp-session-id"),
        Some(REPLACEMENT_SESSION),
        "the retried request must carry the replacement session"
    );
    assert_eq!(seen[5].json()["method"], "tools/list");
}

#[tokio::test]
async fn a_repeated_session_termination_fails_instead_of_looping() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        status_reply(404, "gone"),
        session_reply(
            200,
            &initialize_result(3, "2025-11-25"),
            REPLACEMENT_SESSION,
        ),
        accepted(),
        status_reply(404, "gone again"),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    let error = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, McpHttpError::Status { status: 404 }));
    assert_eq!(transport.seen().len(), 6);
}

#[tokio::test]
async fn an_unsupported_negotiated_version_disconnects_before_the_initialized_notification() {
    let transport = ScriptedTransport::with(vec![session_reply(
        200,
        &initialize_result(1, "1999-01-01"),
        SESSION,
    )]);
    let client = client(&transport);

    let error = client
        .initialize(&CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(error, McpHttpError::Protocol { .. }));
    assert_eq!(
        transport.seen().len(),
        1,
        "an unusable negotiated version must not be confirmed"
    );
}

#[test]
fn a_session_id_accepts_only_visible_ascii_within_bounds() {
    assert!(McpSessionId::new("A1-_.~").is_ok());
    assert!(McpSessionId::new("x".repeat(512)).is_ok());
    assert!(McpSessionId::new("").is_err());
    assert!(McpSessionId::new("x".repeat(513)).is_err());
    assert!(McpSessionId::new("has space").is_err());
    assert!(McpSessionId::new("has\ttab").is_err());
    assert!(McpSessionId::new("has\r\ninjection").is_err());
    assert!(McpSessionId::new("münchen").is_err());
    assert!(McpSessionId::new("del\u{7f}").is_err());
}

#[test]
fn a_session_id_is_redacted_in_diagnostics() {
    let session = McpSessionId::new(SESSION).unwrap();
    assert_eq!(session.expose(), SESSION);
    let rendered = format!("{session:?}");
    assert!(!rendered.contains(SESSION), "session id leaked into Debug");
}

#[tokio::test]
async fn failures_never_expose_the_endpoint_session_or_response_body() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        status_reply(500, "BODY-CANARY internal detail"),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    let error = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("BODY-CANARY"));
    assert!(!rendered.contains("URL-CANARY"));
    assert!(!rendered.contains(SESSION));
    assert!(!rendered.contains("mcp.example.test"));
    assert!(matches!(error, McpHttpError::Status { status: 500 }));
}

#[tokio::test]
async fn a_json_rpc_error_result_is_reported_without_the_server_message() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        json_reply(
            200,
            &serde_json::json!({
                "jsonrpc":"2.0","id":2,
                "error":{"code":-32602,"message":"RPC-MESSAGE-CANARY"}
            }),
        ),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    let error = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, McpHttpError::Rpc { code: -32602 }));
    assert!(!format!("{error} {error:?}").contains("RPC-MESSAGE-CANARY"));
}

#[tokio::test]
async fn a_mismatched_response_id_is_a_protocol_failure() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        json_reply(
            200,
            &serde_json::json!({"jsonrpc":"2.0","id":9999,"result":{}}),
        ),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    let error = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, McpHttpError::Protocol { .. }));
}

#[tokio::test]
async fn a_transport_failure_is_reported_without_detail() {
    let transport = ScriptedTransport::with(vec![Scripted::Failure(TransportError::Network {
        message: "NETWORK-CANARY refused".to_owned(),
    })]);
    let client = client(&transport);

    let error = client
        .initialize(&CancellationToken::new())
        .await
        .unwrap_err();

    assert_eq!(error, McpHttpError::Transport);
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("NETWORK-CANARY"));
    assert!(!rendered.contains("mcp.example.test"));
}

#[tokio::test]
async fn cancellation_before_dispatch_sends_nothing() {
    let transport = ScriptedTransport::with(Vec::new());
    let client = client(&transport);
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = client.initialize(&cancellation).await.unwrap_err();

    assert!(matches!(error, McpHttpError::Cancelled));
    assert!(transport.seen().is_empty());
}

#[tokio::test]
async fn cancellation_during_a_request_settles_as_cancelled() {
    let transport = ScriptedTransport::with(vec![Scripted::Stall]);
    let client = client(&transport);
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });

    let error = client.initialize(&cancellation).await.unwrap_err();

    assert!(matches!(error, McpHttpError::Cancelled));
}

#[tokio::test]
async fn an_exhausted_request_budget_times_out() {
    let transport = ScriptedTransport::with(vec![Scripted::Stall]);
    let client = McpStreamableHttpClient::new(
        service(&transport),
        &endpoint(),
        McpNotificationRouter::new(),
        McpTimeouts::new(25, 25, 60_000, 5_000).unwrap(),
    )
    .unwrap();

    let error = client
        .initialize(&CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(error, McpHttpError::TimedOut));
}

#[tokio::test]
async fn terminating_a_session_deletes_it_and_tolerates_method_not_allowed() {
    let transport = ScriptedTransport::with(vec![
        session_reply(200, &initialize_result(1, "2025-11-25"), SESSION),
        accepted(),
        Scripted::Reply(response(405, None, Vec::new())),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    client.terminate(&CancellationToken::new()).await.unwrap();

    let seen = transport.seen();
    let delete = seen.last().unwrap();
    assert_eq!(delete.method, HttpMethod::Delete);
    assert_eq!(delete.header("mcp-session-id"), Some(SESSION));
    assert_eq!(delete.header("mcp-protocol-version"), Some("2025-11-25"));
    assert!(delete.body.is_none());
}

#[tokio::test]
async fn terminating_a_session_less_connection_sends_nothing() {
    let transport = ScriptedTransport::with(vec![
        json_reply(200, &initialize_result(1, "2025-11-25")),
        accepted(),
    ]);
    let client = client(&transport);
    client.initialize(&CancellationToken::new()).await.unwrap();

    client.terminate(&CancellationToken::new()).await.unwrap();

    assert_eq!(
        transport.seen().len(),
        2,
        "a server that assigned no session has nothing to delete"
    );
}

#[tokio::test]
async fn credential_backed_headers_require_an_authorization_provider() {
    let headers = BTreeMap::from([(
        "authorization".to_owned(),
        heycode_mcp::McpSecretReference::new("mcp/example/token").unwrap(),
    )]);
    let definition = McpStreamableHttpTransport::new(ENDPOINT, headers).unwrap();
    let transport = ScriptedTransport::with(Vec::new());

    let outcome = McpStreamableHttpClient::new(
        service(&transport),
        &definition,
        McpNotificationRouter::new(),
        McpTimeouts::default(),
    );

    let Err(error) = outcome else {
        panic!("credential-backed headers must not connect without a provider");
    };
    assert!(matches!(error, McpHttpError::Unauthorized));
}

#[test]
fn the_supported_protocol_versions_are_a_closed_set() {
    assert_eq!(McpProtocolVersion::LATEST.as_str(), "2025-11-25");
    assert_eq!(
        McpProtocolVersion::parse("2025-11-25"),
        Some(McpProtocolVersion::V20251125)
    );
    assert_eq!(
        McpProtocolVersion::parse("2025-06-18"),
        Some(McpProtocolVersion::V20250618)
    );
    assert_eq!(McpProtocolVersion::parse("2026-07-28"), None);
    assert_eq!(McpProtocolVersion::parse("2025-03-26"), None);
    assert_eq!(McpProtocolVersion::parse(""), None);
}

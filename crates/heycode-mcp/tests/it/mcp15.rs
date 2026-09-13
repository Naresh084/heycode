//! MCP15 local conformance: public stdio/HTTP clients and a stateful OAuth server.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpResponse, HttpService, HttpSseRequest,
    HttpTransport, ReqwestHttpTransport, SseEventStream, TransportError,
};
use heycode_mcp::oauth::{
    McpOAuthClient, OAuthAuthorizationBinding, OAuthClientRegistration, OAuthDiscovery, OAuthFault,
    OAuthRedirectUri, TokenExchange,
};
use heycode_mcp::results::McpResultBlockKind;
use heycode_mcp::{
    McpChannelError, McpClientEvent, McpClientEventRouter, McpClientEventSink, McpClientRoute,
    McpConnection, McpElicitationCapabilities, McpElicitationFailure, McpElicitationHandler,
    McpElicitationRequest, McpElicitationResponse, McpHttpError, McpNotificationRouter,
    McpRequestChannel, McpServerConfig, McpServerId, McpStreamableHttpClient,
    McpStreamableHttpTransport, McpTimeouts,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

const SERVER_CANARY: &str = "MCP15-SERVER-BODY-CANARY";
const OAUTH_CANARY: &str = "MCP15-OAUTH-BODY-CANARY";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn output_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "properties":{"echo":{"type":"string"}},
        "required":["echo"]
    })
}

fn assert_rich_result(value: &serde_json::Value, expected: &str) {
    let result = heycode_mcp::results::McpToolResult::parse(value, Some(&output_schema())).unwrap();
    assert_eq!(
        result
            .blocks()
            .iter()
            .map(|block| block.kind())
            .collect::<Vec<_>>(),
        [
            McpResultBlockKind::Text,
            McpResultBlockKind::ResourceLink,
            McpResultBlockKind::EmbeddedResource,
            McpResultBlockKind::Text,
        ]
    );
    assert_eq!(result.structured().unwrap()["echo"], expected);
    assert!(result.check().conforms());
    assert!(!result.is_error());
}

#[tokio::test]
async fn production_stdio_transport_covers_listings_rich_results_cancellation_and_safe_errors() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("stdio.jsonl");
    let connection = Arc::new(
        McpConnection::spawn(
            "mcp15-stdio",
            &McpServerConfig {
                command: "python3".to_owned(),
                args: vec![
                    fixture("mcp15_server.py").display().to_string(),
                    "stdio".to_owned(),
                    "--log-file".to_owned(),
                    log.display().to_string(),
                ],
                env: std::collections::HashMap::new(),
                required: false,
            },
        )
        .await
        .unwrap(),
    );
    let cancellation = CancellationToken::new();

    let tools = connection
        .call("tools/list", serde_json::json!({}), &cancellation)
        .await
        .unwrap();
    assert_eq!(tools["tools"].as_array().unwrap().len(), 7);
    assert_eq!(tools["tools"][0]["outputSchema"], output_schema());
    assert_eq!(tools["tools"][0]["annotations"]["readOnlyHint"], true);
    let resources = connection
        .call("resources/list", serde_json::json!({}), &cancellation)
        .await
        .unwrap();
    assert_eq!(resources["resources"][0]["uri"], "fixture://resource/1");
    let prompts = connection
        .call("prompts/list", serde_json::json!({}), &cancellation)
        .await
        .unwrap();
    assert_eq!(prompts["prompts"][0]["name"], "fixture_prompt");
    let rich = connection
        .call(
            "tools/call",
            serde_json::json!({"name":"rich_echo","arguments":{"text":"stdio"}}),
            &cancellation,
        )
        .await
        .unwrap();
    assert_rich_result(&rich, "stdio");

    let failure = connection
        .call(
            "tools/call",
            serde_json::json!({"name":"fail","arguments":{}}),
            &cancellation,
        )
        .await
        .unwrap_err();
    assert_eq!(failure, McpChannelError::Rpc { code: -32000 });
    let rendered = format!("{failure:?} {failure}");
    assert!(!rendered.contains(SERVER_CANARY));

    let slow_token = CancellationToken::new();
    let slow = {
        let connection = Arc::clone(&connection);
        let token = slow_token.clone();
        tokio::spawn(async move {
            connection
                .call(
                    "tools/call",
                    serde_json::json!({"name":"slow","arguments":{}}),
                    &token,
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(25)).await;
    slow_token.cancel();
    assert_eq!(slow.await.unwrap(), Err(McpChannelError::Cancelled));
    wait_for_log(&log, "notifications/cancelled").await;
    connection.kill();
}

struct FixtureProcess {
    child: Child,
    _temp: tempfile::TempDir,
    port: u16,
    log: PathBuf,
}

impl FixtureProcess {
    fn start(script: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("port");
        let log = temp.path().join("requests.jsonl");
        let child = Command::new("python3")
            .arg(fixture(script))
            .arg("--ready-file")
            .arg(&ready)
            .arg("--log-file")
            .arg(&log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let port = wait_for_port(&ready);
        Self {
            child,
            _temp: temp,
            port,
            log,
        }
    }

    fn start_mcp() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("port");
        let log = temp.path().join("requests.jsonl");
        let child = Command::new("python3")
            .arg(fixture("mcp15_server.py"))
            .arg("http")
            .arg("--ready-file")
            .arg(&ready)
            .arg("--log-file")
            .arg(&log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let port = wait_for_port(&ready);
        Self {
            child,
            _temp: temp,
            port,
            log,
        }
    }
}

impl Drop for FixtureProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for_port(path: &Path) -> u16 {
    for _ in 0..100 {
        if let Ok(value) = std::fs::read_to_string(path)
            && let Ok(port) = value.parse()
        {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("local fixture did not publish its port")
}

async fn wait_for_log(path: &Path, needle: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(path).is_ok_and(|body| body.contains(needle)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn production_reqwest_streamable_http_covers_listings_rich_results_and_cancellation() {
    let server = FixtureProcess::start_mcp();
    let endpoint = format!("http://127.0.0.1:{}/mcp", server.port);
    let definition = McpStreamableHttpTransport::new(&endpoint, BTreeMap::new()).unwrap();
    let http = HttpService::new(Arc::new(
        ReqwestHttpTransport::with_timeout(Duration::from_secs(15)).unwrap(),
    ));
    let elicitation = Arc::new(LocalElicitation::default());
    let events = Arc::new(LocalEvents::default());
    let event_router = McpClientEventRouter::new(
        McpServerId::new("mcp15-http").unwrap(),
        McpClientRoute::new("mcp15-session").unwrap(),
        McpElicitationCapabilities::form_and_url(),
        elicitation.clone(),
        events.clone(),
    );
    let client = Arc::new(
        McpStreamableHttpClient::new(
            http,
            &definition,
            McpNotificationRouter::with_client_events(
                heycode_mcp::resources::McpResourceListLimits::default(),
                event_router.clone(),
            ),
            McpTimeouts::new(5_000, 15_000, 15_000, 1_000).unwrap(),
        )
        .unwrap(),
    );
    client.initialize(&CancellationToken::new()).await.unwrap();
    let tools = client
        .request(
            "tools/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(tools["tools"].as_array().unwrap().len(), 7);
    let resources = client
        .request(
            "resources/list",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(resources["resources"][0]["name"], "fixture-resource");
    let prompts = client
        .request(
            "prompts/get",
            serde_json::json!({"name":"fixture_prompt","arguments":{"topic":"http"}}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(prompts["messages"][0]["content"]["text"], "topic:http");
    let rich = client
        .request(
            "tools/call",
            serde_json::json!({"name":"rich_echo","arguments":{"text":"http"}}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_rich_result(&rich, "http");

    let progress = event_router.begin_progress();
    let elicited = client
        .request(
            "tools/call",
            serde_json::json!({
                "name":"elicit",
                "arguments":{},
                "_meta":{"progressToken":progress.token().to_json()}
            }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        elicited["content"][0]["text"],
        "finite elicitation dispatched"
    );
    assert_eq!(elicitation.calls.load(Ordering::SeqCst), 1);
    assert!(
        events
            .values
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, McpClientEvent::Progress(_)))
    );
    assert!(
        events
            .values
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, McpClientEvent::Log(_)))
    );
    wait_for_log(&server.log, "client_response").await;
    assert!(
        std::fs::read_to_string(&server.log)
            .unwrap()
            .contains("accepted")
    );
    let responses_before_cancel = std::fs::read_to_string(&server.log)
        .unwrap()
        .matches("client_response")
        .count();
    let cancelled_elicitation = client
        .request(
            "tools/call",
            serde_json::json!({"name":"elicit_cancel","arguments":{}}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        cancelled_elicitation["content"][0]["text"],
        "finite elicitation dispatched"
    );
    assert_eq!(
        elicitation.calls.load(Ordering::SeqCst),
        1,
        "the exact pending interaction must cancel before handler dispatch"
    );
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(
        std::fs::read_to_string(&server.log)
            .unwrap()
            .matches("client_response")
            .count(),
        responses_before_cancel,
        "a cancelled server request receives no late response"
    );

    let open_progress = event_router.begin_progress();
    let open = client
        .request(
            "tools/call",
            serde_json::json!({
                "name":"elicit_open",
                "arguments":{},
                "_meta":{"progressToken":open_progress.token().to_json()}
            }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(open["content"][0]["text"], "finite elicitation dispatched");
    let responses_before_open_cancel = std::fs::read_to_string(&server.log)
        .unwrap()
        .matches("client_response")
        .count();

    let open_cancel = client
        .request(
            "tools/call",
            serde_json::json!({"name":"elicit_cancel_open","arguments":{}}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        open_cancel["content"][0]["text"],
        "finite elicitation dispatched"
    );
    assert_eq!(elicitation.calls.load(Ordering::SeqCst), 3);
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(
        std::fs::read_to_string(&server.log)
            .unwrap()
            .matches("client_response")
            .count(),
        responses_before_open_cancel,
        "open-stream cancellation must not send a late elicitation reply"
    );

    let failure = client
        .request(
            "tools/call",
            serde_json::json!({"name":"fail","arguments":{}}),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(failure, McpHttpError::Rpc { code: -32000 });
    assert!(!format!("{failure:?} {failure}").contains(SERVER_CANARY));

    let slow_token = CancellationToken::new();
    let slow = {
        let client = Arc::clone(&client);
        let token = slow_token.clone();
        tokio::spawn(async move {
            client
                .request(
                    "tools/call",
                    serde_json::json!({"name":"slow","arguments":{}}),
                    &token,
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(25)).await;
    slow_token.cancel();
    assert_eq!(slow.await.unwrap(), Err(McpHttpError::Cancelled));
    assert!(
        std::fs::read_to_string(&server.log)
            .unwrap()
            .contains("tools/call")
    );
}

#[derive(Default)]
struct LocalElicitation {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl McpElicitationHandler for LocalElicitation {
    async fn elicit(
        &self,
        request: McpElicitationRequest,
        cancellation: CancellationToken,
    ) -> Result<McpElicitationResponse, McpElicitationFailure> {
        assert_eq!(request.server().as_str(), "mcp15-http");
        assert_eq!(request.route().expose(), "mcp15-session");
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.message() == "Wait for cancellation." {
            cancellation.cancelled().await;
            return Err(McpElicitationFailure::Cancelled);
        }
        Ok(McpElicitationResponse::accept(
            serde_json::json!({"answer":"accepted"}),
        ))
    }
}

#[derive(Default)]
struct LocalEvents {
    values: Mutex<Vec<McpClientEvent>>,
}

impl McpClientEventSink for LocalEvents {
    fn publish(&self, event: McpClientEvent) {
        self.values.lock().unwrap().push(event);
    }
}

#[derive(Clone)]
struct LoopbackHttpsTransport;

impl HttpTransport for LoopbackHttpsTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(TransportError::Cancelled),
                response = socket_exchange(request) => response,
            }
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

async fn socket_exchange(request: HttpRequest) -> Result<HttpResponse, TransportError> {
    let url = url::Url::parse(request.url()).map_err(|_| local_network_error())?;
    let port = url.port().ok_or_else(local_network_error)?;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|_| local_network_error())?;
    let method = match request.method() {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Delete => "DELETE",
    };
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let body = request.body().unwrap_or_default();
    let mut bytes = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        url.host_str().unwrap_or_default(),
        body.len()
    )
    .into_bytes();
    for header in request.headers() {
        bytes.extend_from_slice(header.name().as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend_from_slice(header.value().as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(body);
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| local_network_error())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|_| local_network_error())?;
    parse_http_response(&response)
}

fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse, TransportError> {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(local_network_error)?;
    let head = std::str::from_utf8(&bytes[..split]).map_err(|_| local_network_error())?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(local_network_error)?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let content_type = headers.get("content-type").cloned();
    Ok(HttpResponse {
        status,
        content_type,
        headers,
        body: bytes[split + 4..].to_vec(),
    })
}

fn local_network_error() -> TransportError {
    TransportError::Network {
        message: "local OAuth fixture transport failed".to_owned(),
    }
}

#[tokio::test]
async fn actual_local_oauth_server_proves_resource_issuer_state_pkce_refresh_and_safe_errors() {
    let server = FixtureProcess::start("mcp15_oauth_server.py");
    let resource = format!("https://resource.fixture.test:{}/mcp", server.port);
    let issuer = format!("https://auth.fixture.test:{}", server.port);
    let transport = LoopbackHttpsTransport;
    let discovery =
        OAuthDiscovery::discover(&transport, &resource, None, None, CancellationToken::new())
            .await
            .unwrap();
    assert_eq!(discovery.resource(), resource);
    assert_eq!(discovery.authorization_server().issuer(), issuer);
    let (resource_id, metadata) = discovery.into_parts();
    let redirect = OAuthRedirectUri::new("http://127.0.0.1:45123/callback").unwrap();
    let registration =
        OAuthClientRegistration::pre_registered_public(&issuer, "mcp15-client", redirect.clone())
            .unwrap();
    let binding =
        OAuthAuthorizationBinding::new(metadata, registration, resource_id, redirect.clone())
            .unwrap();
    let mut client = McpOAuthClient::new(binding.clone());
    let pending = client.begin();
    let authorization_url = pending
        .authorization_url(&["mcp:read", "mcp:tools"])
        .unwrap();
    let authorization_response = transport
        .send(
            HttpRequest::get(&authorization_url).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(authorization_response.status, 302);
    let callback = url::Url::parse(&authorization_response.headers["location"]).unwrap();
    let callback_values: BTreeMap<_, _> = callback.query_pairs().into_owned().collect();
    assert_eq!(
        pending.accept_callback(
            redirect.as_str(),
            "wrong-state",
            callback_values.get("code").map(String::as_str),
            callback_values.get("iss").map(String::as_str),
        ),
        Err(OAuthFault::StateMismatch)
    );
    assert_eq!(
        pending.accept_callback(
            redirect.as_str(),
            callback_values["state"].as_str(),
            callback_values.get("code").map(String::as_str),
            Some("https://attacker.invalid"),
        ),
        Err(OAuthFault::IssuerMismatch)
    );
    let code = pending
        .accept_callback(
            redirect.as_str(),
            callback_values["state"].as_str(),
            callback_values.get("code").map(String::as_str),
            callback_values.get("iss").map(String::as_str),
        )
        .unwrap();

    let denied = TokenExchange::new(binding.clone())
        .redeem_code(
            &transport,
            &pending,
            "wrong-code",
            SystemTime::now(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(denied, OAuthFault::Denied);
    assert!(!format!("{denied:?} {denied}").contains(OAUTH_CANARY));
    assert!(!format!("{denied:?} {denied}").contains(pending.expose_verifier()));

    let tokens = TokenExchange::new(binding.clone())
        .redeem_code(
            &transport,
            &pending,
            &code,
            SystemTime::now(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    client.complete(pending, tokens).unwrap();
    let access = client.state().tokens().unwrap().expose_access().to_owned();
    let protected = HttpRequest::post(
        &resource,
        serde_json::to_vec(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}
        }))
        .unwrap(),
    )
    .unwrap()
    .header("authorization", &format!("Bearer {access}"))
    .unwrap();
    let protected_response = transport
        .send(protected, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(protected_response.status, 200);

    let refreshed = TokenExchange::new(binding)
        .redeem_refresh(
            &transport,
            client.expose_refresh_token().unwrap(),
            SystemTime::now(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    client.refreshed(refreshed).unwrap();
    assert_eq!(
        client.state().tokens().unwrap().expose_access(),
        "mcp15-local-access-refreshed"
    );

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        OAuthDiscovery::discover(&transport, &resource, None, None, cancelled).await,
        Err(OAuthFault::Cancelled)
    ));
    assert!(
        std::fs::read_to_string(&server.log)
            .unwrap()
            .contains("oauth-authorization-server")
    );
}

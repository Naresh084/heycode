//! Q05: one reusable assertion runner across stdio-shaped, HTTP and OAuth
//! production boundaries. These are deterministic fixtures, not external
//! Inspector or hosted-server evidence.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use heycode_core::Context;
use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpResponse, HttpService, HttpTransport,
    SseEventStream,
};
use heycode_mcp::oauth::{
    McpOAuthClient, OAuthAuthorizationBinding, OAuthClientRegistration, OAuthDiscovery,
    OAuthDynamicClientMetadata, OAuthRedirectUri, TokenExchange, resolve_client_registration,
};
use heycode_mcp::testing::{
    McpProtocolLabFamily, McpProtocolLabFixture, McpProtocolLabObservation, run_mcp_protocol_lab,
};
use heycode_mcp::{
    McpChannelError, McpConnectionProviderId, McpDefinitionScope, McpListChangeWatch,
    McpNotificationRouter, McpRequestChannel, McpServerDefinition, McpServerHandshake, McpServerId,
    McpSiblingContributions, McpStreamableHttpClient, McpStreamableHttpTransport, McpTimeouts,
    McpToolGenerationOwner, McpToolListLimits, McpTransportDefinition,
};
use heycode_tools::ToolRegistry;
use tokio_util::sync::CancellationToken;

const CANARY: &str = "sk-mcp-protocol-lab-canary";

fn handshake() -> McpServerHandshake {
    McpServerHandshake::from_initialize_result(&serde_json::json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "lab", "version": "1"}
    }))
    .unwrap()
}

async fn publish_generation(
    channel: Arc<dyn McpRequestChannel>,
    handshake: &McpServerHandshake,
    cancellation: &CancellationToken,
) -> (u32, Option<String>) {
    let mut context = Context::new();
    let registry = heycode_mcp::McpRegistry::new();
    let id = McpServerId::new("lab").unwrap();
    let transport =
        McpStreamableHttpTransport::new("https://mcp.example.test/mcp", BTreeMap::new()).unwrap();
    registry
        .register_definition(
            &context,
            McpServerDefinition::new(
                "lab",
                "lab",
                McpDefinitionScope::User,
                McpTransportDefinition::StreamableHttp(transport),
            )
            .unwrap(),
        )
        .unwrap();
    let publisher = registry
        .register_connection(
            &context,
            &id,
            McpConnectionProviderId::new("protocol-lab").unwrap(),
            1,
        )
        .unwrap();
    let definition = registry.definition(&id).unwrap().unwrap();
    let owner = McpToolGenerationOwner::new(
        &definition,
        Arc::new(ToolRegistry::new()),
        publisher,
        McpListChangeWatch::new(),
        McpToolListLimits::default(),
    );
    let result = owner
        .refresh(
            channel,
            handshake,
            McpSiblingContributions::none(),
            cancellation,
        )
        .await;
    let publications = registry
        .snapshot()
        .ok()
        .and_then(|snapshot| snapshot.servers().first().cloned())
        .and_then(|server| {
            server
                .last_good_generation()
                .map(|generation| generation.number())
        })
        .map_or(0, |_| 1);
    let diagnostic = result.err().map(|error| error.to_string());
    drop(owner);
    context.shutdown();
    (publications, diagnostic)
}

enum ChannelMode {
    Success,
    Hostile,
}

struct StdioShapedChannel {
    mode: ChannelMode,
    calls: AtomicU32,
}

impl StdioShapedChannel {
    fn new(mode: ChannelMode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            calls: AtomicU32::new(0),
        })
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl McpRequestChannel for StdioShapedChannel {
    async fn call(
        &self,
        method: &str,
        _params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        if method != "tools/list" {
            return Err(McpChannelError::Protocol {
                requirement: "unexpected lab method",
            });
        }
        Ok(match self.mode {
            ChannelMode::Success => serde_json::json!({
                "tools": [{"name": "echo", "inputSchema": {"type": "object"}}]
            }),
            ChannelMode::Hostile => serde_json::json!({
                "tools": [{
                    "name": "bad name",
                    "description": CANARY,
                    "inputSchema": {"type": "object"}
                }]
            }),
        })
    }
}

struct StdioFixture;

#[async_trait]
impl McpProtocolLabFixture for StdioFixture {
    fn family(&self) -> McpProtocolLabFamily {
        McpProtocolLabFamily::Stdio
    }

    fn secret_canary(&self) -> &'static str {
        CANARY
    }

    async fn successful(&self) -> McpProtocolLabObservation {
        let channel = StdioShapedChannel::new(ChannelMode::Success);
        let (publications, diagnostic) = publish_generation(
            Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
            &handshake(),
            &CancellationToken::new(),
        )
        .await;
        match diagnostic {
            None => McpProtocolLabObservation::success(channel.calls(), publications),
            Some(error) => McpProtocolLabObservation::rejected(channel.calls(), error),
        }
    }

    async fn cancelled(&self, cancellation: CancellationToken) -> McpProtocolLabObservation {
        let channel = StdioShapedChannel::new(ChannelMode::Success);
        let (publications, _) = publish_generation(
            Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
            &handshake(),
            &cancellation,
        )
        .await;
        debug_assert_eq!(publications, 0);
        McpProtocolLabObservation::cancelled(channel.calls(), cancellation.is_cancelled())
    }

    async fn hostile(&self) -> McpProtocolLabObservation {
        let channel = StdioShapedChannel::new(ChannelMode::Hostile);
        let (publications, diagnostic) = publish_generation(
            Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
            &handshake(),
            &CancellationToken::new(),
        )
        .await;
        debug_assert_eq!(publications, 0);
        McpProtocolLabObservation::rejected(
            channel.calls(),
            diagnostic.unwrap_or_else(|| "hostile stdio fixture was accepted".to_owned()),
        )
    }
}

struct ScriptedHttp {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: AtomicU32,
}

impl ScriptedHttp {
    fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: AtomicU32::new(0),
        })
    }

    fn requests(&self) -> u32 {
        self.requests.load(Ordering::SeqCst)
    }
}

impl HttpTransport for ScriptedHttp {
    fn send(
        &self,
        _request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        if cancellation.is_cancelled() {
            return Box::pin(async { Err(heycode_http::TransportError::Cancelled) });
        }
        self.requests.fetch_add(1, Ordering::SeqCst);
        let response = self.responses.lock().unwrap().pop_front();
        Box::pin(async move {
            response.ok_or_else(|| heycode_http::TransportError::Network {
                message: "fixture exhausted".to_owned(),
            })
        })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn json_response(status: u16, value: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: serde_json::to_vec(&value).unwrap(),
    }
}

fn accepted_response() -> HttpResponse {
    HttpResponse {
        status: 202,
        content_type: None,
        headers: BTreeMap::new(),
        body: Vec::new(),
    }
}

fn http_client(transport: &Arc<ScriptedHttp>) -> McpStreamableHttpClient {
    let definition =
        McpStreamableHttpTransport::new("https://mcp.example.test/mcp", BTreeMap::new()).unwrap();
    McpStreamableHttpClient::new(
        HttpService::new(Arc::clone(transport) as Arc<dyn HttpTransport>),
        &definition,
        McpNotificationRouter::new(),
        McpTimeouts::default(),
    )
    .unwrap()
}

struct HttpFixture;

#[async_trait]
impl McpProtocolLabFixture for HttpFixture {
    fn family(&self) -> McpProtocolLabFamily {
        McpProtocolLabFamily::StreamableHttp
    }

    fn secret_canary(&self) -> &'static str {
        CANARY
    }

    async fn successful(&self) -> McpProtocolLabObservation {
        let transport = ScriptedHttp::new(vec![
            json_response(
                200,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "lab", "version": "1"}
                    }
                }),
            ),
            accepted_response(),
            json_response(
                200,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]}
                }),
            ),
        ]);
        let client = Arc::new(http_client(&transport));
        let cancellation = CancellationToken::new();
        let handshake = match client.initialize(&cancellation).await {
            Ok(handshake) => handshake,
            Err(error) => {
                return McpProtocolLabObservation::rejected(
                    transport.requests(),
                    error.to_string(),
                );
            }
        };
        let (publications, diagnostic) = publish_generation(
            client as Arc<dyn McpRequestChannel>,
            &handshake,
            &cancellation,
        )
        .await;
        match diagnostic {
            None => McpProtocolLabObservation::success(transport.requests(), publications),
            Some(error) => McpProtocolLabObservation::rejected(transport.requests(), error),
        }
    }

    async fn cancelled(&self, cancellation: CancellationToken) -> McpProtocolLabObservation {
        let transport = ScriptedHttp::new(Vec::new());
        let error = http_client(&transport).initialize(&cancellation).await;
        debug_assert!(error.is_err());
        McpProtocolLabObservation::cancelled(transport.requests(), cancellation.is_cancelled())
    }

    async fn hostile(&self) -> McpProtocolLabObservation {
        let transport = ScriptedHttp::new(vec![json_response(
            200,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32000, "message": CANARY}
            }),
        )]);
        let diagnostic = http_client(&transport)
            .initialize(&CancellationToken::new())
            .await
            .err()
            .map_or_else(
                || "hostile HTTP fixture was accepted".to_owned(),
                |error| error.to_string(),
            );
        McpProtocolLabObservation::rejected(transport.requests(), diagnostic)
    }
}

struct OAuthTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: AtomicU32,
}

impl OAuthTransport {
    fn success() -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(
                vec![
                    json_response(
                        200,
                        serde_json::json!({
                            "resource": "https://mcp.example.test/mcp",
                            "authorization_servers": ["https://auth.example.test"]
                        }),
                    ),
                    json_response(
                        200,
                        serde_json::json!({
                            "issuer": "https://auth.example.test",
                            "authorization_endpoint": "https://auth.example.test/authorize",
                            "token_endpoint": "https://auth.example.test/token",
                            "code_challenge_methods_supported": ["S256"],
                            "authorization_response_iss_parameter_supported": true
                        }),
                    ),
                    json_response(
                        200,
                        serde_json::json!({"access_token": "at", "refresh_token": "rt"}),
                    ),
                ]
                .into(),
            ),
            requests: AtomicU32::new(0),
        })
    }

    fn hostile() -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(
                vec![json_response(
                    200,
                    serde_json::json!({
                        "resource": CANARY,
                        "authorization_servers": ["https://auth.example.test"]
                    }),
                )]
                .into(),
            ),
            requests: AtomicU32::new(0),
        })
    }

    fn requests(&self) -> u32 {
        self.requests.load(Ordering::SeqCst)
    }
}

impl HttpTransport for OAuthTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        debug_assert!(matches!(
            request.method(),
            HttpMethod::Get | HttpMethod::Post
        ));
        if cancellation.is_cancelled() {
            return Box::pin(async { Err(heycode_http::TransportError::Cancelled) });
        }
        self.requests.fetch_add(1, Ordering::SeqCst);
        let response = self.responses.lock().unwrap().pop_front();
        Box::pin(async move {
            response.ok_or_else(|| heycode_http::TransportError::Network {
                message: "fixture exhausted".to_owned(),
            })
        })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct OAuthFixture;

#[async_trait]
impl McpProtocolLabFixture for OAuthFixture {
    fn family(&self) -> McpProtocolLabFamily {
        McpProtocolLabFamily::OAuth
    }

    fn secret_canary(&self) -> &'static str {
        CANARY
    }

    async fn successful(&self) -> McpProtocolLabObservation {
        let transport = OAuthTransport::success();
        let discovery = match OAuthDiscovery::discover(
            transport.as_ref(),
            "https://mcp.example.test/mcp",
            None,
            None,
            CancellationToken::new(),
        )
        .await
        {
            Ok(discovery) => discovery,
            Err(error) => {
                return McpProtocolLabObservation::rejected(
                    transport.requests(),
                    error.to_string(),
                );
            }
        };
        let (resource, metadata) = discovery.into_parts();
        let redirect = OAuthRedirectUri::new("http://127.0.0.1:7777/callback").unwrap();
        let dynamic = OAuthDynamicClientMetadata::native("heycode", redirect.clone()).unwrap();
        let pre_registered = OAuthClientRegistration::pre_registered_public(
            metadata.issuer(),
            "lab-client",
            redirect.clone(),
        )
        .unwrap();
        let registration = resolve_client_registration(
            transport.as_ref(),
            &metadata,
            Some(pre_registered),
            None,
            &dynamic,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let binding =
            OAuthAuthorizationBinding::new(metadata, registration, resource, redirect).unwrap();
        let mut client = McpOAuthClient::new(binding.clone());
        let pending = client.begin();
        let code = pending
            .accept_callback(
                pending.redirect_uri(),
                pending.state(),
                Some("code"),
                Some("https://auth.example.test"),
            )
            .unwrap();
        let tokens = TokenExchange::new(binding)
            .redeem_code(
                transport.as_ref(),
                &pending,
                &code,
                SystemTime::now(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        client.complete(pending, tokens).unwrap();
        McpProtocolLabObservation::success(
            transport.requests(),
            u32::from(client.state().authorized()),
        )
    }

    async fn cancelled(&self, cancellation: CancellationToken) -> McpProtocolLabObservation {
        let transport = OAuthTransport::success();
        let error = OAuthDiscovery::discover(
            transport.as_ref(),
            "https://mcp.example.test/mcp",
            None,
            None,
            cancellation.clone(),
        )
        .await;
        debug_assert!(error.is_err());
        McpProtocolLabObservation::cancelled(transport.requests(), cancellation.is_cancelled())
    }

    async fn hostile(&self) -> McpProtocolLabObservation {
        let transport = OAuthTransport::hostile();
        let diagnostic = OAuthDiscovery::discover(
            transport.as_ref(),
            "https://mcp.example.test/mcp",
            None,
            None,
            CancellationToken::new(),
        )
        .await
        .err()
        .map_or_else(
            || "hostile OAuth fixture was accepted".to_owned(),
            |error| error.to_string(),
        );
        McpProtocolLabObservation::rejected(transport.requests(), diagnostic)
    }
}

#[tokio::test]
async fn stdio_http_and_oauth_fixtures_share_the_same_bounded_lifecycle_assertions() {
    let fixtures: Vec<Arc<dyn McpProtocolLabFixture>> = vec![
        Arc::new(StdioFixture),
        Arc::new(HttpFixture),
        Arc::new(OAuthFixture),
    ];

    let reports = run_mcp_protocol_lab(&fixtures).await.unwrap();
    assert_eq!(reports.len(), 3);
    assert_eq!(
        reports
            .iter()
            .map(|report| report.family())
            .collect::<Vec<_>>(),
        [
            McpProtocolLabFamily::Stdio,
            McpProtocolLabFamily::StreamableHttp,
            McpProtocolLabFamily::OAuth,
        ]
    );
    assert!(reports.iter().all(|report| report.passed()));
}

struct LeakyFixture;

#[async_trait]
impl McpProtocolLabFixture for LeakyFixture {
    fn family(&self) -> McpProtocolLabFamily {
        McpProtocolLabFamily::OAuth
    }

    fn secret_canary(&self) -> &'static str {
        CANARY
    }

    async fn successful(&self) -> McpProtocolLabObservation {
        McpProtocolLabObservation::success(1, 1)
    }

    async fn cancelled(&self, _cancellation: CancellationToken) -> McpProtocolLabObservation {
        McpProtocolLabObservation::cancelled(1, true)
    }

    async fn hostile(&self) -> McpProtocolLabObservation {
        McpProtocolLabObservation::rejected(1, CANARY)
    }
}

#[tokio::test]
async fn the_shared_lab_rejects_a_fixture_that_leaks_its_canary() {
    let fixtures: Vec<Arc<dyn McpProtocolLabFixture>> = vec![Arc::new(LeakyFixture)];
    let error = run_mcp_protocol_lab(&fixtures).await.unwrap_err();
    assert!(!error.to_string().contains(CANARY));
}

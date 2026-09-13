//! Operation-time MCP credential binding for provider-owned bundles.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
};
use heycode_mcp::{
    McpApprovalMode, McpBoundEnvironmentSource, McpBoundServer, McpCredentialBinding,
    McpCredentialBindings, McpCredentialEncoding, McpDefinitionScope, McpEnvironmentValue,
    McpNotificationRouter, McpSecretReference, McpServerDefinition, McpStdioTransport,
    McpStreamableHttpClient, McpStreamableHttpTransport, McpTimeouts, McpToolApprovalDecision,
    McpToolApprovalHandler, McpToolApprovalRequest, McpToolPolicy, McpTransportDefinition,
    mcp_bound_servers_plugin, mcp_bound_servers_product_plugin,
};
use tokio_util::sync::CancellationToken;

struct RotatingProvider {
    id: CredentialProviderId,
    value: Mutex<String>,
    resolves: Mutex<usize>,
}

impl RotatingProvider {
    fn new(value: &str) -> Arc<Self> {
        Arc::new(Self {
            id: CredentialProviderId::new("mcp-test").unwrap(),
            value: Mutex::new(value.to_owned()),
            resolves: Mutex::new(0),
        })
    }

    fn rotate(&self, value: &str) {
        *self.value.lock().unwrap() = value.to_owned();
    }
}

impl CredentialProvider for RotatingProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(CredentialProviderState::configured(
            CredentialSource::Environment,
            false,
        ))
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        *self.resolves.lock().unwrap() += 1;
        Ok(Some(CredentialSecret::new(
            self.value.lock().unwrap().clone(),
        )))
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("MCP_TOKEN").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn bindings(encoding: McpCredentialEncoding) -> McpCredentialBindings {
    let mut bindings = McpCredentialBindings::new();
    bindings
        .insert(
            McpSecretReference::new("MCP_TOKEN").unwrap(),
            McpCredentialBinding::new(query(), encoding),
        )
        .unwrap();
    bindings
}

fn credentials(
    value: &str,
) -> (
    heycode_core::Context,
    CredentialsService,
    Arc<RotatingProvider>,
) {
    let context = heycode_core::Context::new();
    let service = CredentialsService::new();
    let provider = RotatingProvider::new(value);
    service.register(&context, provider.clone()).unwrap();
    (context, service, provider)
}

struct RecordingHttp {
    replies: Mutex<VecDeque<heycode_http::HttpResponse>>,
    auth: Mutex<Vec<Option<String>>>,
}

impl heycode_http::HttpTransport for RecordingHttp {
    fn send(
        &self,
        request: heycode_http::HttpRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::BufferedResponseFuture {
        self.auth.lock().unwrap().push(
            request
                .headers()
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("authorization"))
                .map(|header| header.value().to_owned()),
        );
        let reply = self.replies.lock().unwrap().pop_front();
        Box::pin(async move { Ok(reply.expect("scripted HTTP response")) })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn json_response(value: serde_json::Value) -> heycode_http::HttpResponse {
    heycode_http::HttpResponse {
        status: 200,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: serde_json::to_vec(&value).unwrap(),
    }
}

#[tokio::test]
async fn http_headers_resolve_only_when_each_operation_is_dispatched() {
    let (_context, credentials, provider) = credentials("first-secret");
    let transport = Arc::new(RecordingHttp {
        replies: Mutex::new(
            vec![
                json_response(serde_json::json!({
                    "jsonrpc":"2.0","id":1,"result":{
                        "protocolVersion":"2025-11-25","capabilities":{},
                        "serverInfo":{"name":"credential","version":"1"}
                    }
                })),
                heycode_http::HttpResponse {
                    status: 202,
                    content_type: None,
                    headers: BTreeMap::new(),
                    body: Vec::new(),
                },
                json_response(serde_json::json!({"jsonrpc":"2.0","id":2,"result":{"turn":1}})),
                json_response(serde_json::json!({"jsonrpc":"2.0","id":3,"result":{"turn":2}})),
            ]
            .into_iter()
            .collect(),
        ),
        auth: Mutex::new(Vec::new()),
    });
    let definition = McpStreamableHttpTransport::new(
        "https://mcp.example.test/credential",
        BTreeMap::from([(
            "authorization".to_owned(),
            McpSecretReference::new("MCP_TOKEN").unwrap(),
        )]),
    )
    .unwrap();
    let client = McpStreamableHttpClient::new_with_credentials(
        heycode_http::HttpService::new(transport.clone()),
        &definition,
        McpNotificationRouter::new(),
        McpTimeouts::default(),
        credentials,
        bindings(McpCredentialEncoding::Bearer),
    )
    .unwrap();

    client.initialize(&CancellationToken::new()).await.unwrap();
    provider.rotate("second-secret");
    client
        .request(
            "tools/call",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    provider.rotate("third-secret");
    client
        .request(
            "tools/call",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    let auth = transport.auth.lock().unwrap();
    assert_eq!(auth[0].as_deref(), Some("Bearer first-secret"));
    assert_eq!(auth[1].as_deref(), Some("Bearer first-secret"));
    assert_eq!(auth[2].as_deref(), Some("Bearer second-secret"));
    assert_eq!(auth[3].as_deref(), Some("Bearer third-secret"));
    assert!(!format!("{:?}", bindings(McpCredentialEncoding::Bearer)).contains("secret"));
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_environment_resolves_at_process_launch_without_entering_the_definition() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("credential-seen");
    let script = r#"
import json,os,sys
assert os.environ['MCP_TOKEN']=='stdio-secret'
open(sys.argv[1],'w').write('ok')
request=json.loads(sys.stdin.readline())
print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{'protocolVersion':'2025-11-25','capabilities':{},'serverInfo':{'name':'stdio-credential','version':'1'}}}),flush=True)
sys.stdin.readline()
sys.stdin.readline()
"#;
    let (_context, credentials, _provider) = credentials("stdio-secret");
    let exact = McpStdioTransport::new(
        "python3",
        std::env::current_dir().unwrap(),
        vec![
            heycode_mcp::McpArgument::literal("-u").unwrap(),
            heycode_mcp::McpArgument::literal("-c").unwrap(),
            heycode_mcp::McpArgument::literal(script).unwrap(),
            heycode_mcp::McpArgument::literal(marker.display().to_string()).unwrap(),
        ],
        BTreeMap::from([(
            "MCP_TOKEN".to_owned(),
            McpEnvironmentValue::credential(McpSecretReference::new("MCP_TOKEN").unwrap()),
        )]),
    )
    .unwrap();
    let connection = heycode_mcp::McpConnection::spawn_definition_with_credentials(
        "stdio-credential",
        &exact,
        credentials,
        bindings(McpCredentialEncoding::Raw),
        None,
    )
    .await
    .unwrap();
    assert!(marker.exists());
    connection.kill();
}

#[test]
fn a_binding_cannot_invent_the_reference_that_the_bundle_erased() {
    let mut bindings = McpCredentialBindings::new();
    let error = bindings
        .insert(
            McpSecretReference::new("OTHER_TOKEN").unwrap(),
            McpCredentialBinding::new(query(), McpCredentialEncoding::Raw),
        )
        .unwrap_err();
    assert_eq!(error, heycode_mcp::McpCredentialError::ReferenceMismatch);
    assert!(bindings.is_empty());
}

#[test]
fn bound_server_requires_an_exact_complete_raw_stdio_binding_set() {
    let definition = || {
        McpServerDefinition::new(
            "bound-check",
            "Bound Check",
            McpDefinitionScope::User,
            McpTransportDefinition::Stdio(
                McpStdioTransport::new(
                    "/bin/echo",
                    std::env::current_dir().unwrap(),
                    Vec::new(),
                    BTreeMap::from([(
                        "MCP_TOKEN".to_owned(),
                        McpEnvironmentValue::credential(
                            McpSecretReference::new("MCP_TOKEN").unwrap(),
                        ),
                    )]),
                )
                .unwrap(),
            ),
        )
        .unwrap()
    };
    assert!(matches!(
        McpBoundServer::new(definition(), McpCredentialBindings::new()),
        Err(heycode_mcp::McpBoundServerError::MissingBinding)
    ));
    assert!(matches!(
        McpBoundServer::new(definition(), bindings(McpCredentialEncoding::Bearer)),
        Err(heycode_mcp::McpBoundServerError::InvalidStdioEncoding)
    ));
}

#[test]
fn provider_launch_builders_preserve_reference_only_prompt_policy_for_stdio_and_http() {
    let reference = McpSecretReference::new("MCP_TOKEN").unwrap();
    let stdio = McpBoundServer::provider_stdio(
        "provider-stdio",
        "Provider stdio",
        "/bin/echo",
        std::env::current_dir().unwrap(),
        vec!["package@1.2.3".to_owned()],
        BTreeMap::from([
            (
                "MODE".to_owned(),
                McpBoundEnvironmentSource::Literal("safe".to_owned()),
            ),
            (
                "MCP_TOKEN".to_owned(),
                McpBoundEnvironmentSource::Credential {
                    reference: reference.clone(),
                    query: query(),
                },
            ),
        ]),
        std::collections::BTreeSet::from(["search".to_owned()]),
        false,
    )
    .unwrap();
    assert!(stdio.bindings().contains(&reference));
    assert_eq!(
        stdio.definition().tool_policy().default_approval(),
        McpApprovalMode::Prompt
    );
    assert_eq!(
        stdio.definition().tool_policy().enabled_tools().unwrap(),
        &std::collections::BTreeSet::from(["search".to_owned()])
    );
    assert!(!stdio.definition().exposure().resources);
    assert!(!stdio.definition().exposure().prompts);
    assert!(!stdio.definition().exposure().instructions);

    let http = McpBoundServer::provider_streamable_http_bearer(
        "provider-http",
        "Provider HTTP",
        "https://mcp.example.test/mcp",
        "authorization",
        reference.clone(),
        query(),
        std::collections::BTreeSet::from(["read".to_owned()]),
        false,
    )
    .unwrap();
    assert_eq!(
        http.bindings().binding(&reference).unwrap().encoding(),
        McpCredentialEncoding::Bearer
    );
    assert_eq!(
        http.definition().tool_policy().default_approval(),
        McpApprovalMode::Prompt
    );
}

struct NeverHttp;

struct NoopLifecycleHooks;

#[async_trait::async_trait]
impl heycode_mcp::McpLifecycleHookPort for NoopLifecycleHooks {
    async fn run(
        &self,
        _request: heycode_mcp::McpLifecycleHookRequest,
        _cancellation: CancellationToken,
    ) -> heycode_mcp::McpLifecycleHookReport {
        heycode_mcp::McpLifecycleHookReport::proceed()
    }
}

#[test]
fn product_bound_connections_require_one_exact_client_route_per_enabled_server() {
    let definition = McpServerDefinition::new(
        "fixture",
        "Fixture",
        McpDefinitionScope::User,
        McpTransportDefinition::StreamableHttp(
            McpStreamableHttpTransport::new("https://mcp.example.test/mcp", BTreeMap::new())
                .unwrap(),
        ),
    )
    .unwrap();
    let server = McpBoundServer::new(definition, McpCredentialBindings::new()).unwrap();
    assert!(matches!(
        mcp_bound_servers_product_plugin(
            "product-bound-fixture",
            vec![server],
            Arc::new(AllowApproval),
            Arc::new(NoopLifecycleHooks),
        ),
        Err(heycode_mcp::McpBoundServerError::MissingRoute)
    ));
}

impl heycode_http::HttpTransport for NeverHttp {
    fn send(
        &self,
        _request: heycode_http::HttpRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::BufferedResponseFuture {
        Box::pin(async { panic!("stdio fixture must not use HTTP") })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct FixtureServices(Arc<RotatingProvider>);

impl heycode_core::Plugin for FixtureServices {
    fn name(&self) -> &'static str {
        "mcp-bound-fixture-services"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            self.name(),
            env!("CARGO_PKG_VERSION"),
            &[heycode_core::PluginContributionKind::Service],
        )
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[
            heycode_tools::SERVICE_TOOLS,
            heycode_exec::SERVICE_SUBPROCESS,
            heycode_http::SERVICE_HTTP,
            heycode_credentials::SERVICE_CREDENTIALS,
            heycode_mcp::SERVICE_MCP,
        ]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
        let credentials = CredentialsService::new();
        credentials.register(context, self.0.clone()).map_err(|_| {
            heycode_core::CoreError::other("credential fixture registration failed")
        })?;
        context.provide(
            heycode_tools::SERVICE_TOOLS,
            self.name(),
            heycode_tools::ToolRegistry::new(),
        )?;
        context.provide(
            heycode_exec::SERVICE_SUBPROCESS,
            self.name(),
            heycode_exec::SubprocessService::local(),
        )?;
        context.provide(
            heycode_http::SERVICE_HTTP,
            self.name(),
            heycode_http::HttpService::new(Arc::new(NeverHttp)),
        )?;
        context.provide(
            heycode_credentials::SERVICE_CREDENTIALS,
            self.name(),
            credentials,
        )?;
        context.provide(
            heycode_mcp::SERVICE_MCP,
            self.name(),
            heycode_mcp::McpRegistry::new(),
        )
    }
}

struct AllowApproval;

#[async_trait::async_trait]
impl McpToolApprovalHandler for AllowApproval {
    async fn decide(
        &self,
        _request: McpToolApprovalRequest,
        _cancellation: CancellationToken,
    ) -> McpToolApprovalDecision {
        McpToolApprovalDecision::Allow
    }
}

#[cfg(unix)]
#[tokio::test]
async fn bound_plugin_reuses_the_ordinary_generation_tool_and_disposal_owners() {
    let script = r#"
import json,os,sys
assert os.environ['MCP_TOKEN']=='bound-secret'
while True:
  line=sys.stdin.readline()
  if not line: break
  request=json.loads(line)
  method=request.get('method')
  if method=='initialize': result={'protocolVersion':'2025-11-25','capabilities':{'tools':{}},'serverInfo':{'name':'bound','version':'1'}}
  elif method=='tools/list': result={'tools':[{'name':'alpha','description':'alpha','inputSchema':{'type':'object'}}]}
  elif method=='tools/call': result={'content':[{'type':'text','text':'bound-ok'}]}
  else: continue
  print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#;
    let transport = McpStdioTransport::new(
        "python3",
        std::env::current_dir().unwrap(),
        vec![
            heycode_mcp::McpArgument::literal("-u").unwrap(),
            heycode_mcp::McpArgument::literal("-c").unwrap(),
            heycode_mcp::McpArgument::literal(script).unwrap(),
        ],
        BTreeMap::from([(
            "MCP_TOKEN".to_owned(),
            McpEnvironmentValue::credential(McpSecretReference::new("MCP_TOKEN").unwrap()),
        )]),
    )
    .unwrap();
    let policy = McpToolPolicy::new(
        Some(std::collections::BTreeSet::from(["alpha".to_owned()])),
        std::collections::BTreeSet::new(),
        McpApprovalMode::Allow,
        BTreeMap::new(),
    )
    .unwrap();
    let definition = McpServerDefinition::new(
        "bound",
        "Bound",
        McpDefinitionScope::User,
        McpTransportDefinition::Stdio(transport),
    )
    .unwrap()
    .with_required(true)
    .with_tool_policy(policy);
    let server = McpBoundServer::new(definition, bindings(McpCredentialEncoding::Raw)).unwrap();
    let provider = RotatingProvider::new("bound-secret");
    let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
        Box::new(FixtureServices(provider)),
        mcp_bound_servers_plugin("mcp-bound-fixture", vec![server], Arc::new(AllowApproval))
            .unwrap(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tool = tools.get("mcp__bound__alpha").unwrap();
    let output = tool
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(output["blocks"][0]["text"], "bound-ok");
    context.shutdown();
    assert!(tools.get("mcp__bound__alpha").is_none());
}

//! One MCP server surface: what management stores, what configuration
//! declares and what the session actually runs are the same set.
//!
//! The four claims here were each a shipped defect. A server added through
//! `heycode mcp add` never spawned; a server declared in `heycode.toml` was invisible
//! to `/mcp`; a bundled definition's tool policy was carried, displayed and
//! ignored; a Streamable HTTP endpoint produced a definition and no
//! connection. Each test states the user-visible fact, not the mechanism.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::{Plugin, compose};
use heycode_mcp::management::{McpManagement, SERVICE_MCP_MANAGEMENT};
use heycode_mcp::{
    McpApprovalMode, McpClientEvent, McpClientEventRouter, McpClientEventSink, McpClientRoute,
    McpConnectionState, McpDefinitionScope, McpElicitationCapabilities, McpElicitationFailure,
    McpElicitationHandler, McpElicitationRequest, McpElicitationResponse, McpLifecycleHookPort,
    McpLifecycleHookReport, McpLifecycleHookRequest, McpRecoveryAdmission, McpRegistry,
    McpRuntimeControl, McpRuntimeControlError, McpServerConfig, McpServerDefinition, McpServerId,
    McpServerSpec, McpStdioTransport, McpToolApprovalDecision, McpToolApprovalHandler,
    McpToolApprovalRequest, McpToolPolicy, McpTransportDefinition, McpTransportKind, SERVICE_MCP,
    SERVICE_MCP_RUNTIME_CONTROL, mcp_plugin, mcp_product_plugin, mcp_registry_plugin,
};
use heycode_tools::{ToolCtx, ToolRegistry};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------- fixtures

const MOCK: &str = r#"import sys, json
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"unified","version":"1.0"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}},{"name":"danger","description":"Danger","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":"OK"}],"isError":False}
    else:
        result = {}
    if ident is not None:
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;

/// Answers the handshake and the listing, then dies on the first call.
const DYING: &str = r#"import sys, json, os
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"dying","version":"1.0"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        os._exit(7)
    else:
        result = {}
    if ident is not None:
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;

/// An absolute interpreter path: a `#!` line cannot be resolved from `PATH`.
fn python3() -> Option<String> {
    let located = std::process::Command::new("/usr/bin/env")
        .args(["sh", "-c", "command -v python3"])
        .output()
        .ok()?;
    if !located.status.success() {
        return None;
    }
    let path = String::from_utf8(located.stdout).ok()?.trim().to_owned();
    (!path.is_empty() && std::path::Path::new(&path).is_absolute()).then_some(path)
}

/// Write `body` as an executable script whose only argument-free invocation
/// runs it: the management store holds a bare command with no argv.
fn executable_script(dir: &std::path::Path, name: &str, python: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!{python}\n{body}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_str().unwrap().to_owned()
}

struct AllowApproval;

#[async_trait]
impl McpToolApprovalHandler for AllowApproval {
    async fn decide(
        &self,
        _request: McpToolApprovalRequest,
        _cancellation: CancellationToken,
    ) -> McpToolApprovalDecision {
        McpToolApprovalDecision::Allow
    }
}

struct NoElicitation;

#[async_trait]
impl McpElicitationHandler for NoElicitation {
    async fn elicit(
        &self,
        _request: McpElicitationRequest,
        _cancellation: CancellationToken,
    ) -> Result<McpElicitationResponse, McpElicitationFailure> {
        Err(McpElicitationFailure::Unavailable)
    }
}

struct DiscardSink;

impl McpClientEventSink for DiscardSink {
    fn publish(&self, _event: McpClientEvent) {}
}

/// Records every MCP11 event the router published, so a test can state that a
/// notification reached the product sink rather than that it was parsed.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<String>>);

impl RecordingSink {
    fn rendered(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl McpClientEventSink for RecordingSink {
    fn publish(&self, event: McpClientEvent) {
        // `Debug` redacts the payload, so record the exact log body instead:
        // the claim is which frame arrived, not how it renders.
        let rendered = match &event {
            McpClientEvent::Log(log) => format!("log:{}", log.data()),
            other => format!("{other:?}"),
        };
        self.0.lock().unwrap().push(rendered);
    }
}

/// Answers every server elicitation with an explicit decline: the test cares
/// that the request was routed to a handler at all, not what it decided.
struct DecliningElicitation;

#[async_trait]
impl McpElicitationHandler for DecliningElicitation {
    async fn elicit(
        &self,
        _request: McpElicitationRequest,
        _cancellation: CancellationToken,
    ) -> Result<McpElicitationResponse, McpElicitationFailure> {
        Ok(McpElicitationResponse::decline())
    }
}

struct NoopHooks;

#[async_trait]
impl McpLifecycleHookPort for NoopHooks {
    async fn run(
        &self,
        _request: McpLifecycleHookRequest,
        _cancellation: CancellationToken,
    ) -> McpLifecycleHookReport {
        McpLifecycleHookReport::proceed()
    }
}

fn route(server: &str) -> McpClientEventRouter {
    McpClientEventRouter::new(
        McpServerId::new(server).unwrap(),
        McpClientRoute::new("session-unified").unwrap(),
        McpElicitationCapabilities::form_and_url(),
        Arc::new(NoElicitation),
        Arc::new(DiscardSink),
    )
}

/// The world the product root composes around `mcp`: settings, management,
/// http and the MCP registry, in the order `BUILTIN_PLUGIN_ORDER` uses.
fn product_world(
    servers: HashMap<String, McpServerSpec>,
    settings: heycode_settings::SettingsDocuments,
    cwd: &std::path::Path,
) -> Vec<Box<dyn Plugin>> {
    let mut routers = HashMap::new();
    for name in servers.keys() {
        routers.insert(name.clone(), route(name));
    }
    let mcp = mcp_product_plugin(
        servers,
        cwd.to_path_buf(),
        Arc::new(AllowApproval),
        routers,
        Arc::new(NoopHooks),
    )
    .unwrap();
    vec![
        heycode_settings::settings_plugin(settings),
        heycode_http::http_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(cwd.to_path_buf(), Duration::from_secs(30))
                .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        mcp_registry_plugin(),
        heycode_mcp::management::mcp_management_plugin(),
        mcp,
    ]
}

fn stored_servers_document(rows: serde_json::Value) -> heycode_settings::SettingsDocuments {
    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_user(
            heycode_mcp::management::settings_namespace().unwrap(),
            serde_json::json!({"servers": rows}),
        )
        .unwrap();
    documents
}

// ------------------------------------------------------- (a) direction one

/// `heycode mcp add` writes the management store. If that store is not a
/// connection source, the CLI's "added" and "enabled" are false statements.
#[test]
fn a_server_only_in_the_management_store_still_connects_and_publishes_its_tools() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let command = executable_script(temp.path(), "managed-server", &python, MOCK);
    let documents = stored_servers_document(serde_json::json!({
        "managed": {"transport": "stdio", "target": command, "enabled": true}
    }));

    let plugins = product_world(HashMap::new(), documents, temp.path());
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        tools.names().contains(&"mcp__managed__echo".to_owned()),
        "a stored, enabled server must be connected by the session that lists it as enabled: {:?}",
        tools.names()
    );
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let snapshot = registry.snapshot().unwrap();
    assert_eq!(snapshot.servers().len(), 1);
    context.shutdown();
}

/// A disabled row is a definition the operator turned off. It must not run.
#[test]
fn a_disabled_stored_server_is_not_connected() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let command = executable_script(temp.path(), "off-server", &python, MOCK);
    let documents = stored_servers_document(serde_json::json!({
        "off": {"transport": "stdio", "target": command, "enabled": false}
    }));

    let plugins = product_world(HashMap::new(), documents, temp.path());
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        !tools
            .names()
            .iter()
            .any(|name| name.starts_with("mcp__off__")),
        "a disabled definition must stay off"
    );
    context.shutdown();
}

/// A row the user typed into `heycode mcp add` is a guess, not a guarantee: the
/// store validates that the target is non-empty, never that it exists. A
/// mistyped or not-yet-installed command must show up as a failed server in
/// `/mcp`, not stop heycode from starting at all.
#[test]
fn a_stored_row_whose_command_does_not_exist_still_lets_the_session_start() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("nonexistent-binary");
    let documents = stored_servers_document(serde_json::json!({
        "typo": {
            "transport": "stdio",
            "target": missing.to_str().unwrap(),
            "enabled": true
        }
    }));

    let plugins = product_world(HashMap::new(), documents, temp.path());
    let mut context =
        compose(&plugins).expect("a mistyped `heycode mcp add` row must never brick composition");
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        !tools
            .names()
            .iter()
            .any(|name| name.starts_with("mcp__typo__")),
        "a server that never connected publishes no tools: {:?}",
        tools.names()
    );
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let snapshot = registry.snapshot().unwrap();
    let server = snapshot
        .servers()
        .iter()
        .find(|server| server.definition().id().as_str() == "typo")
        .expect("the row stays visible as a definition");
    assert!(
        matches!(server.state(), McpConnectionState::Failed { .. }),
        "the failure must be visible in `/mcp`, not silent: {:?}",
        server.state()
    );
    let control = context
        .get::<McpRuntimeControl>(SERVICE_MCP_RUNTIME_CONTROL)
        .expect("the runtime control exists even when a server never established");
    assert_eq!(
        control.reconnect("typo"),
        Err(McpRuntimeControlError::NoLiveConnection {
            name: "typo".to_owned()
        }),
        "an enabled failed definition is distinct from an unknown server"
    );
    context.shutdown();
}

/// A `[mcp.servers]` entry is user data too: a mistyped command, a binary not
/// installed yet or a server that is down must show as a failed row in `/mcp`,
/// never stop heycode from starting. Only an explicit `required = true` makes a
/// server's failure the host's failure.
#[test]
fn a_configured_server_that_cannot_start_degrades_to_a_failed_row() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("nonexistent-binary");
    let mut servers = HashMap::new();
    servers.insert(
        "declared".to_owned(),
        McpServerSpec::Stdio(McpServerConfig {
            command: missing.to_str().unwrap().to_owned(),
            args: Vec::new(),
            env: HashMap::new(),
            required: false,
        }),
    );

    let plugins = product_world(
        servers,
        heycode_settings::SettingsDocuments::new(),
        temp.path(),
    );
    let mut context =
        compose(&plugins).expect("a broken [mcp.servers] entry must never brick composition");
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let snapshot = registry.snapshot().unwrap();
    let server = snapshot
        .servers()
        .iter()
        .find(|server| server.definition().id().as_str() == "declared")
        .expect("the row stays visible as a definition");
    assert!(
        matches!(server.state(), McpConnectionState::Failed { .. }),
        "the failure must be visible in `/mcp`: {:?}",
        server.state()
    );
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        !tools
            .names()
            .iter()
            .any(|name| name.starts_with("mcp__declared__"))
    );
    context.shutdown();
}

/// The opt-in half: `required = true` says this session is meaningless without
/// the server, so a failure to connect fails composition — and the message
/// names the server and its program so the user knows what to fix.
#[test]
fn a_required_server_that_cannot_start_fails_composition_naming_itself() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("nonexistent-binary");
    let mut servers = HashMap::new();
    servers.insert(
        "declared".to_owned(),
        McpServerSpec::Stdio(McpServerConfig {
            command: missing.to_str().unwrap().to_owned(),
            args: Vec::new(),
            env: HashMap::new(),
            required: true,
        }),
    );

    let plugins = product_world(
        servers,
        heycode_settings::SettingsDocuments::new(),
        temp.path(),
    );
    let refused = compose(&plugins)
        .err()
        .expect("a required server that cannot connect fails the composition");
    let text = refused.to_string();
    assert!(text.contains("`declared`"), "names the server: {text}");
    assert!(
        text.contains("nonexistent-binary"),
        "names the program: {text}"
    );
}

/// `heycode mcp add --url` must refuse what the connection path would refuse,
/// with the same message, at the moment the user types it.
#[test]
fn a_stored_http_row_is_validated_as_a_url_when_it_is_created() {
    let error = heycode_mcp::management::StoredServer::new(
        "bad",
        heycode_mcp::McpTransportKind::StreamableHttp,
        "notaurl",
    )
    .expect_err("a non-URL target is refused at add time");
    assert!(
        matches!(
            error,
            heycode_mcp::management::McpManagementError::InvalidTransport(_)
        ),
        "{error:?}"
    );
    let ok = heycode_mcp::management::StoredServer::new(
        "good",
        heycode_mcp::McpTransportKind::StreamableHttp,
        "https://example.com/mcp",
    );
    assert!(ok.is_ok());
}

/// A stored stdio row may carry arguments and environment, exactly as
/// `[mcp.servers.<name>]` can, and they reach the spawned child.
#[test]
fn a_stored_row_carries_arguments_and_environment_to_the_child() {
    let mut row = heycode_mcp::management::StoredServer::new(
        "with-args",
        heycode_mcp::McpTransportKind::Stdio,
        "server-bin",
    )
    .unwrap();
    row.args = vec!["--port".to_owned(), "8080".to_owned()];
    row.env.insert("SERVER_MODE".to_owned(), "fast".to_owned());
    let spec = heycode_mcp::stored_server_spec(&row);
    let McpServerSpec::Stdio(config) = spec else {
        panic!("a stdio row maps to a stdio spec");
    };
    assert_eq!(config.command, "server-bin");
    assert_eq!(config.args, ["--port", "8080"]);
    assert_eq!(
        config.env.get("SERVER_MODE").map(String::as_str),
        Some("fast")
    );
    assert!(!config.required, "a stored row is never required");
}

// ------------------------------------------------------- (a) direction two

/// The inverse: a `[mcp.servers.<name>]` entry is the only kind that used to
/// connect, and `/mcp` could not see, list or explain it.
#[test]
fn a_configuration_declared_server_is_visible_to_the_management_surface() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let mut servers = HashMap::new();
    servers.insert(
        "declared".to_owned(),
        McpServerSpec::Stdio(McpServerConfig {
            command: python.clone(),
            args: vec!["-u".to_owned(), "-c".to_owned(), MOCK.to_owned()],
            env: HashMap::new(),
            required: false,
        }),
    );

    let plugins = product_world(
        servers,
        heycode_settings::SettingsDocuments::new(),
        temp.path(),
    );
    let mut context = compose(&plugins).unwrap();
    let management = context
        .get::<McpManagement>(SERVICE_MCP_MANAGEMENT)
        .unwrap();
    let listed = management.list().unwrap();
    let row = listed
        .iter()
        .find(|row| row.server.name == "declared")
        .expect("a running configured server must appear in the management listing");
    assert!(row.server.enabled);
    assert_eq!(row.server.transport, McpTransportKind::Stdio);

    // It is not editable through the store, because the store is not where it
    // lives — and saying so is the whole point of listing it.
    let refused = management
        .remove("declared")
        .expect_err("a configuration-declared server cannot be removed from the settings store");
    let rendered = refused.to_string();
    assert!(rendered.contains("declared"), "{rendered}");
    assert!(rendered.contains("configuration"), "{rendered}");
    context.shutdown();
}

// ------------------------------------------------------------ (b) policy

/// A bundled package's definition carries an allowlist and per-tool approval.
/// A definition whose policy denies a tool must not publish that tool, whether
/// or not the host installed an approval broker.
#[test]
fn a_definition_tool_policy_filters_the_published_generation_without_an_approval_broker() {
    let Some(python) = python3() else {
        return;
    };
    let cwd = std::env::current_dir().unwrap();
    let transport = McpStdioTransport::new(
        python,
        cwd.clone(),
        vec![
            heycode_mcp::McpArgument::literal("-u").unwrap(),
            heycode_mcp::McpArgument::literal("-c").unwrap(),
            heycode_mcp::McpArgument::literal(MOCK).unwrap(),
        ],
        BTreeMap::new(),
    )
    .unwrap();
    let policy = McpToolPolicy::new(
        Some(BTreeSet::from(["echo".to_owned()])),
        BTreeSet::new(),
        McpApprovalMode::Allow,
        BTreeMap::new(),
    )
    .unwrap();
    let definition = McpServerDefinition::new(
        "bundled",
        "Bundled",
        McpDefinitionScope::Plugin,
        McpTransportDefinition::Stdio(transport),
    )
    .unwrap()
    .with_tool_policy(policy);

    let mut servers = HashMap::new();
    servers.insert(
        "bundled".to_owned(),
        McpServerSpec::Definition(definition.clone()),
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(cwd.clone(), Duration::from_secs(30)).unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        mcp_registry_plugin(),
        mcp_plugin(servers, cwd),
    ];
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let names = tools.names();
    assert!(
        names.contains(&"mcp__bundled__echo".to_owned()),
        "an allowlisted tool stays published: {names:?}"
    );
    assert!(
        !names.contains(&"mcp__bundled__danger".to_owned()),
        "a tool outside the definition's allowlist must never reach the model: {names:?}"
    );
    context.shutdown();
}

// -------------------------------------------------------------- (c) HTTP

/// `url = "…"` in `heycode.toml` is documented as a transport, not as an
/// inspectable placeholder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_configured_streamable_http_server_connects_in_the_product_root() {
    let server = MockHttpServer::start().await;
    let temp = tempfile::tempdir().unwrap();
    let mut servers = HashMap::new();
    servers.insert(
        "remote".to_owned(),
        McpServerSpec::StreamableHttp {
            url: server.endpoint.clone(),
            required: false,
        },
    );
    let plugins = product_world(
        servers,
        heycode_settings::SettingsDocuments::new(),
        temp.path(),
    );
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        tools.names().contains(&"mcp__remote__ping".to_owned()),
        "a configured HTTP endpoint must produce a live connection and its tools: {:?}",
        tools.names()
    );
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let snapshot = registry.snapshot().unwrap();
    assert!(matches!(
        snapshot.servers()[0].state(),
        McpConnectionState::Ready { .. }
    ));
    context.shutdown();
}

// ----------------------------------------------------- (d) liveness truth

/// The registry must never publish `Ready` for a transport that is gone.
#[test]
fn a_dead_stdio_child_stops_being_published_as_ready() {
    let Some(python) = python3() else {
        return;
    };
    let cwd = std::env::current_dir().unwrap();
    let mut servers = HashMap::new();
    servers.insert(
        "dying".to_owned(),
        McpServerSpec::Stdio(McpServerConfig {
            command: python,
            args: vec!["-u".to_owned(), "-c".to_owned(), DYING.to_owned()],
            env: HashMap::new(),
            required: false,
        }),
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(cwd.clone(), Duration::from_secs(30)).unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        mcp_registry_plugin(),
        mcp_plugin(servers, cwd),
    ];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tool = tools.get("mcp__dying__echo").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let called = runtime.block_on(tool.run(serde_json::json!({}), &ToolCtx::default()));
    assert!(called.is_err(), "the child died mid-call");

    let mut observed = None;
    for _ in 0..100 {
        let snapshot = registry.snapshot().unwrap();
        let state = snapshot.servers()[0].state().clone();
        if !matches!(state, McpConnectionState::Ready { .. }) {
            observed = Some(state);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        observed.is_some(),
        "a connection whose child exited must not keep publishing Ready"
    );
    context.shutdown();
}

// ------------------------------------------------------- HTTP mock server

struct MockHttpServer {
    endpoint: String,
    stop: CancellationToken,
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl MockHttpServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let session = Arc::new(Mutex::new(0_u32));
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    () = cancelled.cancelled() => return,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => return,
                    },
                };
                let session = Arc::clone(&session);
                tokio::spawn(async move { serve_one(stream, &session).await });
            }
        });
        Self {
            endpoint: format!("http://127.0.0.1:{port}/mcp"),
            stop,
        }
    }
}

async fn serve_one(mut stream: tokio::net::TcpStream, session: &Arc<Mutex<u32>>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 2048];
    let head_end = loop {
        let Ok(read) = stream.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        raw.extend_from_slice(&chunk[..read]);
        if let Some(index) = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
        {
            break index;
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let http_method = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').next())
        .unwrap_or_default()
        .to_owned();
    let mut length = 0_usize;
    for line in head.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or_default();
        }
    }
    while raw.len() < head_end + length {
        let Ok(read) = stream.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    let body: serde_json::Value = serde_json::from_slice(&raw[head_end..head_end + length])
        .unwrap_or(serde_json::Value::Null);
    let response = http_response(&http_method, &body, session);
    let _written = stream.write_all(response.as_bytes()).await;
    let _flushed = stream.flush().await;
}

fn http_response(http_method: &str, body: &serde_json::Value, session: &Arc<Mutex<u32>>) -> String {
    if http_method == "DELETE" {
        return "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    }
    let id = body.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = body
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let payload = match method {
        "initialize" => {
            *session.lock().unwrap() += 1;
            serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "http-fixture", "version": "1.0"}
                }
            })
        }
        "notifications/initialized" => {
            return "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_owned();
        }
        "tools/list" => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"tools": [
                {"name": "ping", "description": "Ping", "inputSchema": {"type": "object"}}
            ]}
        }),
        "tools/call" => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"content": [{"type": "text", "text": "PONG"}], "isError": false}
        }),
        _ => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}),
    };
    let rendered = payload.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: UNIFIED-SESSION\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{rendered}",
        rendered.len()
    )
}

// ------------------------------------------------- (d) bounded recovery

/// A crashed child is recovered in-session. Before the supervisor was armed it
/// was fully built and fully tested and nothing ever called `recover()`, so the
/// only cure for a crashed MCP server was restarting heycode.
#[test]
fn a_crashed_stdio_child_is_reconnected_and_serves_again() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("first-run").display().to_string();
    let script = format!(
        r#"import sys, json, os
marker = {marker:?}
first = not os.path.exists(marker)
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        result = {{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"flaky","version":"1.0"}}}}
    elif method == "tools/list":
        result = {{"tools":[{{"name":"echo","description":"Echo","inputSchema":{{"type":"object"}}}}]}}
    elif method == "tools/call":
        if first:
            open(marker, "w").write("x")
            os._exit(9)
        result = {{"content":[{{"type":"text","text":"RECOVERED"}}],"isError":False}}
    else:
        result = {{}}
    if ident is not None:
        sys.stdout.write(json.dumps({{"jsonrpc":"2.0","id":ident,"result":result}})+"\n"); sys.stdout.flush()
"#
    );
    let cwd = std::env::current_dir().unwrap();
    let mut servers = HashMap::new();
    servers.insert(
        "flaky".to_owned(),
        McpServerSpec::Stdio(McpServerConfig {
            command: python,
            args: vec!["-u".to_owned(), "-c".to_owned(), script],
            env: HashMap::new(),
            required: false,
        }),
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(cwd.clone(), Duration::from_secs(30)).unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        mcp_registry_plugin(),
        mcp_plugin(servers, cwd),
    ];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let first = runtime.block_on(
        tools
            .get("mcp__flaky__echo")
            .unwrap()
            .run(serde_json::json!({}), &ToolCtx::default()),
    );
    assert!(first.is_err(), "the first child exits on its first call");

    let mut recovered = None;
    for _ in 0..200 {
        let snapshot = registry.snapshot().unwrap();
        let server = &snapshot.servers()[0];
        if matches!(server.state(), McpConnectionState::Ready { .. })
            && server
                .last_good_generation()
                .is_some_and(|generation| generation.number() >= 2)
        {
            recovered = Some(());
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        recovered.is_some(),
        "a bounded reconnect must republish a complete generation"
    );
    let second = runtime.block_on(
        tools
            .get("mcp__flaky__echo")
            .unwrap()
            .run(serde_json::json!({}), &ToolCtx::default()),
    );
    let value = second.expect("the recovered connection serves calls again");
    assert_eq!(value["blocks"][0]["text"], "RECOVERED");
    context.shutdown();
}

/// An operator reconnect reaches the exact same bounded supervisor used for a
/// crash. It neither fans out to sibling servers nor retries any model action,
/// and context shutdown wins during backoff before a third child can spawn.
#[test]
fn named_manual_reconnect_is_single_flight_exact_and_cancelled_with_the_context() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let script = r#"import sys, json, os
server, count_path, journal_path = sys.argv[1:4]
try:
    with open(count_path) as handle:
        run = int(handle.read()) + 1
except (FileNotFoundError, ValueError):
    run = 1
with open(count_path, "w") as handle:
    handle.write(str(run))
with open(journal_path, "a") as journal:
    journal.write(f"{server}:start:{run}:pid:{os.getpid()}\n")
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    with open(journal_path, "a") as journal:
        journal.write(f"{server}:method:{method}\n")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":server,"version":str(run)}}
    elif method == "tools/list":
        result = {"tools":[{"name":f"{server}_v{run}","description":"generation marker","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":"unexpected"}],"isError":False}
    else:
        result = {}
    if ident is not None:
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;
    let journal = temp.path().join("journal");
    let mut servers = HashMap::new();
    for name in ["alpha", "beta"] {
        servers.insert(
            name.to_owned(),
            McpServerSpec::Stdio(McpServerConfig {
                command: python.clone(),
                args: vec![
                    "-u".to_owned(),
                    "-c".to_owned(),
                    script.to_owned(),
                    name.to_owned(),
                    temp.path()
                        .join(format!("{name}-count"))
                        .display()
                        .to_string(),
                    journal.display().to_string(),
                ],
                env: HashMap::new(),
                required: true,
            }),
        );
    }

    let plugins = product_world(
        servers,
        heycode_settings::SettingsDocuments::new(),
        temp.path(),
    );
    let mut context = compose(&plugins).unwrap();
    let control = context
        .get::<McpRuntimeControl>(SERVICE_MCP_RUNTIME_CONTROL)
        .expect("the product MCP plugin publishes its exact runtime owner");
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(tools.names().contains(&"mcp__alpha__alpha_v1".to_owned()));
    assert!(tools.names().contains(&"mcp__beta__beta_v1".to_owned()));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert_eq!(
            control.reconnect("alpha").unwrap(),
            McpRecoveryAdmission::Started
        );
        assert_eq!(
            control.reconnect("alpha").unwrap(),
            McpRecoveryAdmission::AlreadyRunning,
            "the selected server owns only one reconnect episode"
        );
        assert!(
            tools.names().contains(&"mcp__alpha__alpha_v1".to_owned()),
            "the last complete generation stays whole during backoff"
        );

        let mut recovered = false;
        for _ in 0..200 {
            let names = tools.names();
            if names.contains(&"mcp__alpha__alpha_v2".to_owned()) {
                recovered = true;
                assert!(!names.contains(&"mcp__alpha__alpha_v1".to_owned()));
                assert!(names.contains(&"mcp__beta__beta_v1".to_owned()));
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(recovered, "a whole second alpha generation must publish");

        let journal_before_shutdown = std::fs::read_to_string(&journal).unwrap();
        assert_eq!(journal_before_shutdown.matches("alpha:start:").count(), 2);
        assert_eq!(journal_before_shutdown.matches("beta:start:").count(), 1);
        assert!(
            !journal_before_shutdown.contains("method:tools/call"),
            "manual recovery must not infer or replay a model tool action"
        );

        assert_eq!(
            control.reconnect("alpha").unwrap(),
            McpRecoveryAdmission::Started
        );
    });
    context.shutdown();
    std::thread::sleep(Duration::from_millis(700));

    let journal_after_shutdown = std::fs::read_to_string(&journal).unwrap();
    assert_eq!(
        journal_after_shutdown.matches("alpha:start:").count(),
        2,
        "shutdown during backoff must prevent another process spawn"
    );
    assert_eq!(
        control.reconnect("alpha"),
        Err(McpRuntimeControlError::ShutDown),
        "a retained service handle must not outlive its product context"
    );
}

// ------------------------------- (e) MCP11 survives a bounded reconnect

/// A recovered connection is a whole connection. The routing plane is shared
/// with the UI and reused by the reconnector, so retiring it when a child dies
/// used to answer every later elicitation `-32603 … shutting down` and drop
/// every later log frame — silently, for the rest of the session.
#[test]
fn a_reconnected_child_still_routes_notifications_and_elicitations() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("first-run").display().to_string();
    let script = format!(
        r#"import sys, json, os
marker = {marker:?}
first = not os.path.exists(marker)

def send(frame):
    sys.stdout.write(json.dumps(frame) + "\n"); sys.stdout.flush()

while True:
    line = sys.stdin.readline()
    if not line:
        break
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        result = {{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}},"logging":{{}}}},"serverInfo":{{"name":"talkative","version":"1.0"}}}}
    elif method == "tools/list":
        result = {{"tools":[{{"name":"echo","description":"Echo","inputSchema":{{"type":"object"}}}}]}}
    elif method == "tools/call":
        if first:
            open(marker, "w").write("x")
            os._exit(9)
        send({{"jsonrpc":"2.0","method":"notifications/message","params":{{"level":"info","logger":"talkative","data":"AFTER-RECONNECT"}}}})
        send({{"jsonrpc":"2.0","id":"elicit-1","method":"elicitation/create","params":{{"message":"still routed?","mode":"form","requestedSchema":{{"type":"object","properties":{{"answer":{{"type":"string"}}}}}}}}}})
        answer = "no-reply"
        while True:
            reply_line = sys.stdin.readline()
            if not reply_line:
                break
            reply = json.loads(reply_line)
            if reply.get("id") == "elicit-1":
                if "result" in reply:
                    answer = "routed:" + str(reply["result"].get("action"))
                else:
                    answer = "refused:" + str(reply.get("error", {{}}).get("code"))
                break
        result = {{"content":[{{"type":"text","text":answer}}],"isError":False}}
    else:
        result = {{}}
    if ident is not None:
        send({{"jsonrpc":"2.0","id":ident,"result":result}})
"#
    );
    let cwd = std::env::current_dir().unwrap();
    let mut servers = HashMap::new();
    servers.insert(
        "talkative".to_owned(),
        McpServerSpec::Stdio(McpServerConfig {
            command: python,
            args: vec!["-u".to_owned(), "-c".to_owned(), script],
            env: HashMap::new(),
            required: false,
        }),
    );
    let sink = Arc::new(RecordingSink::default());
    let mut routers = HashMap::new();
    routers.insert(
        "talkative".to_owned(),
        McpClientEventRouter::new(
            McpServerId::new("talkative").unwrap(),
            McpClientRoute::new("session-unified").unwrap(),
            McpElicitationCapabilities::form_and_url(),
            Arc::new(DecliningElicitation),
            Arc::clone(&sink) as Arc<dyn McpClientEventSink>,
        ),
    );
    let mcp = mcp_product_plugin(
        servers,
        cwd.clone(),
        Arc::new(AllowApproval),
        routers,
        Arc::new(NoopHooks),
    )
    .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        heycode_http::http_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(cwd.clone(), Duration::from_secs(30)).unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        mcp_registry_plugin(),
        heycode_mcp::management::mcp_management_plugin(),
        mcp,
    ];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let first = runtime.block_on(
        tools
            .get("mcp__talkative__echo")
            .unwrap()
            .run(serde_json::json!({}), &ToolCtx::default()),
    );
    assert!(first.is_err(), "the first child exits on its first call");

    let mut recovered = false;
    for _ in 0..200 {
        let snapshot = registry.snapshot().unwrap();
        let server = &snapshot.servers()[0];
        if matches!(server.state(), McpConnectionState::Ready { .. })
            && server
                .last_good_generation()
                .is_some_and(|generation| generation.number() >= 2)
        {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(recovered, "the bounded reconnect must republish");

    let second = runtime
        .block_on(
            tools
                .get("mcp__talkative__echo")
                .unwrap()
                .run(serde_json::json!({}), &ToolCtx::default()),
        )
        .expect("the recovered connection serves calls again");
    assert_eq!(
        second["blocks"][0]["text"], "routed:decline",
        "a server elicitation on the recovered child must still reach the product handler"
    );
    let published = sink.rendered();
    assert!(
        published
            .iter()
            .any(|event| event == "log:\"AFTER-RECONNECT\""),
        "a log notification from the recovered child must still reach the product sink: {published:?}"
    );
    context.shutdown();
}

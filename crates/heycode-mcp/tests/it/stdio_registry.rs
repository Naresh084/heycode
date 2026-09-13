//! MCP02 stdio bridge-to-registry product contract.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::HashMap;

use heycode_core::{Plugin, compose};
use heycode_mcp::{
    McpAuthenticationState, McpConnectionState, McpDefinitionScope, McpRegistry, McpServerConfig,
    McpServerId, McpTransportDefinition, SERVICE_MCP, mcp_plugin, mcp_registry_plugin,
};
use heycode_tools::{ToolCtx, ToolRegistry};

const MOCK: &str = r#"
import os, sys, json
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{},"logging":{}},"serverInfo":{"name":"registry-fixture","version":"2.0"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":"ECHO:"+req["params"]["arguments"].get("text","")+":"+os.environ["MCP_REGISTRY_FIXTURE"]}],"isError":False}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[tokio::test]
async fn stdio_plugin_publishes_exact_definition_ready_generation_and_live_tool() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let config = McpServerConfig {
        command: "python3".to_owned(),
        args: vec!["-u".to_owned(), "-c".to_owned(), MOCK.to_owned()],
        env: HashMap::from([("MCP_REGISTRY_FIXTURE".to_owned(), "present".to_owned())]),
        required: true,
    };
    let mut servers = HashMap::new();
    servers.insert(
        "fixture".to_owned(),
        heycode_mcp::McpServerSpec::Stdio(config),
    );
    let mcp = mcp_plugin(servers, cwd.clone());
    assert!(mcp.inject().contains(&SERVICE_MCP));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        mcp_registry_plugin(),
        mcp,
    ];

    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let id = McpServerId::new("fixture").unwrap();
    let exact = registry.definition(&id).unwrap().unwrap();
    assert_eq!(exact.scope(), McpDefinitionScope::User);
    assert!(
        exact.required(),
        "`required = true` on the configured entry reaches the exact definition"
    );
    // A configured stdio child gets bounded reconnect: the supervisor is armed
    // by the driver loop's own liveness signal, and the budget is terminal so a
    // crash loop still ends in a classified failure rather than a spin.
    assert!(exact.reconnect().enabled());
    assert_eq!(exact.reconnect().max_attempts(), 10);
    assert_eq!(exact.reconnect().initial_delay_ms(), 500);
    assert_eq!(exact.reconnect().max_delay_ms(), 30_000);
    let McpTransportDefinition::Stdio(transport) = exact.transport() else {
        panic!("stdio config must register an exact stdio definition");
    };
    assert_eq!(transport.command(), "python3");
    assert_eq!(transport.cwd(), cwd);
    assert_eq!(transport.arguments()[2].literal_value(), Some(MOCK));
    assert_eq!(
        transport.environment()["MCP_REGISTRY_FIXTURE"].literal_value(),
        Some("present")
    );

    let snapshot = registry.snapshot().unwrap();
    assert_eq!(snapshot.servers().len(), 1);
    let server = &snapshot.servers()[0];
    assert_eq!(
        server.state(),
        &McpConnectionState::Ready {
            since_ms: server.last_good_generation().unwrap().committed_at_ms()
        }
    );
    assert_eq!(server.authentication(), McpAuthenticationState::NotRequired);
    assert_eq!(
        server.connection_provider().unwrap().as_str(),
        "stdio-local"
    );
    let generation = server.last_good_generation().unwrap();
    assert_eq!(generation.number(), 1);
    assert_eq!(generation.protocol_version(), "2024-11-05");
    assert_eq!(generation.server_name(), "registry-fixture");
    assert_eq!(generation.server_version(), "2.0");
    assert!(generation.capabilities().tools);
    assert!(generation.capabilities().logging);
    assert_eq!(generation.contributions().tools, 1);

    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tool = tools.get("mcp__fixture__echo").unwrap();
    let result = tool
        .run(serde_json::json!({"text":"hello"}), &ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(result["schemaVersion"], 1);
    assert_eq!(result["blocks"][0]["type"], "text");
    assert_eq!(result["blocks"][0]["text"], "ECHO:hello:present");

    context.shutdown();
    let stopped = registry.snapshot().unwrap();
    assert!(!stopped.active());
    assert!(stopped.servers().is_empty());
    assert!(tools.get("mcp__fixture__echo").is_none());
    assert!(!tools.names().contains(&"mcp__fixture__echo".to_owned()));
}

#[test]
fn a_configured_http_server_is_a_visible_definition_without_a_stdio_connection() {
    use heycode_mcp::{McpRegistry, McpServerId, McpServerSpec, SERVICE_MCP};

    let mut servers = std::collections::HashMap::new();
    servers.insert(
        "remote".to_owned(),
        McpServerSpec::StreamableHttp {
            url: "https://mcp.example.test/v1".to_owned(),
            required: false,
        },
    );
    let cwd = std::env::current_dir().unwrap();
    let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        heycode_mcp::mcp_registry_plugin(),
        heycode_mcp::mcp_plugin(servers, cwd),
    ];
    let mut ctx = heycode_core::compose(&plugins).unwrap();
    let registry = ctx.get::<McpRegistry>(SERVICE_MCP).unwrap();

    // The operator configured it, so it must be inspectable rather than
    // silently absent — even though this bridge drives only stdio.
    let id = McpServerId::new("remote").unwrap();
    let definition = registry
        .definition(&id)
        .unwrap()
        .expect("a configured HTTP server must be a visible definition");
    assert_eq!(definition.id().as_str(), "remote");

    // No stdio process was launched for it, and no tools were registered.
    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        !tools
            .names()
            .iter()
            .any(|name| name.starts_with("mcp__remote__")),
        "MCP04 owns activating an HTTP connection; nothing may be faked here"
    );
    ctx.shutdown();
}

/// A server that advertises all three listings and serves each of them.
///
/// The prompt row declares a required argument, so the published count is a
/// real walk of a real template rather than an empty array.
const THREE_LISTING_MOCK: &str = r#"
import sys, json
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{"subscribe":True},"prompts":{"listChanged":True}},"serverInfo":{"name":"three","version":"1.0"},"instructions":"Prefer the review prompt."}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}
    elif method == "resources/list":
        result = {"resources":[{"uri":"file:///a","name":"a"},{"uri":"file:///b","name":"b"}]}
    elif method == "prompts/list":
        result = {"prompts":[{"name":"code_review","arguments":[{"name":"code","required":True}]}]}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;

/// A server that advertises prompts and then refuses to list them.
const BROKEN_PROMPTS_MOCK: &str = r#"
import sys, json
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{},"prompts":{}},"serverInfo":{"name":"broken","version":"1.0"}}
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush(); continue
    if method == "prompts/list":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"error":{"code":-32603,"message":"boom"}})+"\n"); sys.stdout.flush(); continue
    if method == "tools/list":
        result = {"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;

/// A world whose one server is `required`, so an incomplete or racing
/// connection is a composition failure rather than a failed row.
fn world(server: &str, source: &str, cwd: &std::path::Path) -> Vec<Box<dyn Plugin>> {
    let config = McpServerConfig {
        command: "python3".to_owned(),
        args: vec!["-u".to_owned(), "-c".to_owned(), source.to_owned()],
        env: HashMap::new(),
        required: true,
    };
    let mut servers = HashMap::new();
    servers.insert(server.to_owned(), heycode_mcp::McpServerSpec::Stdio(config));
    vec![
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.to_path_buf(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        mcp_registry_plugin(),
        mcp_plugin(servers, cwd.to_path_buf()),
    ]
}

/// One connection publishes one generation carrying all three counts.
#[tokio::test]
async fn a_stdio_connection_publishes_one_generation_with_all_three_counts() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = world("three", THREE_LISTING_MOCK, &cwd);
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();

    let snapshot = registry.snapshot().unwrap();
    let server = &snapshot.servers()[0];
    let generation = server
        .last_good_generation()
        .expect("a complete connection publishes a generation");

    assert!(matches!(server.state(), McpConnectionState::Ready { .. }));
    assert_eq!(generation.number(), 1, "one connection, one generation");
    assert!(generation.capabilities().resources);
    assert!(generation.capabilities().prompts);
    assert_eq!(generation.contributions().tools, 1);
    assert_eq!(
        generation.contributions().resources,
        2,
        "the resource walk actually ran"
    );
    assert_eq!(
        generation.contributions().prompts,
        1,
        "the prompt walk actually ran"
    );

    context.shutdown();
}

/// An advertised listing heycode could not walk fails composition rather than
/// producing a world whose generation claims the server has none.
#[tokio::test]
async fn an_unwalkable_advertised_listing_fails_loudly_instead_of_reporting_zero() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = world("broken", BROKEN_PROMPTS_MOCK, &cwd);

    // A required server whose advertised listing cannot be walked is an
    // incomplete connection, and the registry calls its counts complete. Failing
    // at load is the same rule the bridge already applies to a failed tool walk;
    // the alternative is a published generation reading "advertises prompts,
    // has none" when the truth is that the walk errored.
    let Err(error) = compose(&plugins) else {
        panic!("an incomplete connection must fail loudly");
    };

    assert!(
        format!("{error}").contains("mcp protocol failed"),
        "error was {error}"
    );
}

/// Emits a `prompts/list_changed` while serving `prompts/list`, so the walk
/// spans the change it announced.
const PROMPT_RACE_MOCK: &str = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o)+"\n"); sys.stdout.flush()
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":ident,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{},"prompts":{}},"serverInfo":{"name":"race","version":"1.0"}}}); continue
    if method == "prompts/list":
        send({"jsonrpc":"2.0","method":"notifications/prompts/list_changed"})
        send({"jsonrpc":"2.0","id":ident,"result":{"prompts":[{"name":"p"}]}}); continue
    if method == "tools/list":
        send({"jsonrpc":"2.0","id":ident,"result":{"tools":[{"name":"echo","description":"E","inputSchema":{"type":"object"}}]}}); continue
    send({"jsonrpc":"2.0","id":ident,"result":{}})
"#;

/// Emits a `resources/list_changed` while serving `resources/list`.
const RESOURCE_RACE_MOCK: &str = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o)+"\n"); sys.stdout.flush()
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":ident,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"race","version":"1.0"}}}); continue
    if method == "resources/list":
        send({"jsonrpc":"2.0","method":"notifications/resources/list_changed"})
        send({"jsonrpc":"2.0","id":ident,"result":{"resources":[{"uri":"file:///a","name":"a"}]}}); continue
    if method == "tools/list":
        send({"jsonrpc":"2.0","id":ident,"result":{"tools":[{"name":"echo","description":"E","inputSchema":{"type":"object"}}]}}); continue
    send({"jsonrpc":"2.0","id":ident,"result":{}})
"#;

/// Announces each family's change during a *different* family's walk.
const CROSS_FAMILY_MOCK: &str = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o)+"\n"); sys.stdout.flush()
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":ident,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{},"prompts":{}},"serverInfo":{"name":"cross","version":"1.0"}}}); continue
    if method == "resources/list":
        send({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})
        send({"jsonrpc":"2.0","id":ident,"result":{"resources":[{"uri":"file:///a","name":"a"}]}}); continue
    if method == "prompts/list":
        send({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})
        send({"jsonrpc":"2.0","id":ident,"result":{"prompts":[{"name":"p"}]}}); continue
    if method == "tools/list":
        send({"jsonrpc":"2.0","method":"notifications/prompts/list_changed"})
        send({"jsonrpc":"2.0","method":"notifications/resources/list_changed"})
        send({"jsonrpc":"2.0","id":ident,"result":{"tools":[{"name":"echo","description":"E","inputSchema":{"type":"object"}}]}}); continue
    send({"jsonrpc":"2.0","id":ident,"result":{}})
"#;

/// The stdio driver routes a prompt list-change into the prompt epoch.
///
/// The walk spans the change, so the candidate is torn and the connection
/// fails. If the driver still recognised only `tools/list_changed` the walk
/// would publish a listing the server had already invalidated.
#[tokio::test]
async fn the_stdio_driver_routes_a_prompt_list_change_into_the_prompt_epoch() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = world("race", PROMPT_RACE_MOCK, &cwd);

    let Err(error) = compose(&plugins) else {
        panic!("a prompt walk spanning its own list change must not publish");
    };

    assert!(
        format!("{error}").contains("mcp protocol failed"),
        "{error}"
    );
}

/// The stdio driver routes a resource list-change into the resource epoch.
#[tokio::test]
async fn the_stdio_driver_routes_a_resource_list_change_into_the_resource_epoch() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = world("race", RESOURCE_RACE_MOCK, &cwd);

    let Err(error) = compose(&plugins) else {
        panic!("a resource walk spanning its own list change must not publish");
    };

    assert!(
        format!("{error}").contains("mcp protocol failed"),
        "{error}"
    );
}

/// One family's change never discards another family's walk.
///
/// Every walk here spans a change announced for a *different* family. With one
/// shared epoch each walk would be torn and the connection would fail; with one
/// epoch per family all three complete and publish together.
#[tokio::test]
async fn a_change_in_one_family_never_discards_another_familys_walk() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = world("cross", CROSS_FAMILY_MOCK, &cwd);

    let mut context = compose(&plugins).expect("separate epochs must not cross");
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let snapshot = registry.snapshot().unwrap();
    let generation = snapshot.servers()[0].last_good_generation().unwrap();

    assert_eq!(generation.contributions().tools, 1);
    assert_eq!(generation.contributions().resources, 1);
    assert_eq!(generation.contributions().prompts, 1);

    context.shutdown();
}

/// A server that is slow to answer `initialize` delays only itself. Three of
/// them used to cost three startup waits before the shell appeared, because
/// composition connected them one after another.
#[tokio::test]
async fn every_configured_server_hands_shakes_at_once_not_one_after_another() {
    if !python3_available() {
        return;
    }
    const SLOW: &str = r#"
import sys, json, time
for line in sys.stdin:
    req = json.loads(line); method = req.get("method"); ident = req.get("id")
    if method == "initialize":
        time.sleep(0.6)
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"slow","version":"1.0"}}
    elif method == "tools/list":
        result = {"tools":[]}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#;
    let cwd = std::env::current_dir().unwrap();
    let mut servers = HashMap::new();
    for index in 0..4 {
        servers.insert(
            format!("slow-{index}"),
            heycode_mcp::McpServerSpec::Stdio(McpServerConfig {
                command: "python3".to_owned(),
                args: vec!["-u".to_owned(), "-c".to_owned(), SLOW.to_owned()],
                env: HashMap::new(),
                required: true,
            }),
        );
    }
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
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

    let started = std::time::Instant::now();
    let mut context = compose(&plugins).unwrap();
    let elapsed = started.elapsed();

    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let snapshot = registry.snapshot().unwrap();
    assert_eq!(snapshot.servers().len(), 4);
    for server in snapshot.servers() {
        assert!(
            matches!(server.state(), McpConnectionState::Ready { .. }),
            "every server still connects: {:?}",
            server.state()
        );
    }
    assert!(
        elapsed < std::time::Duration::from_millis(1_800),
        "four 600ms handshakes overlap instead of costing 2.4s: {elapsed:?}"
    );
    context.shutdown();
}

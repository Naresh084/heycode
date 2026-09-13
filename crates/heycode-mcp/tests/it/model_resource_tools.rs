//! Model-facing MCP resource tools over an actual stdio fixture process.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use heycode_core::{Plugin, UntrustedContentBoundary, compose};
use heycode_mcp::{McpServerConfig, McpServerSpec, mcp_plugin, mcp_registry_plugin};
use heycode_tools::{ToolCtx, ToolRegistry};

const REAL_MCP_NODE_ENV: &str = "HEYCODE_REAL_MCP_NODE";
const REAL_MCP_XCODEBUILDMCP_ENV: &str = "HEYCODE_REAL_MCP_XCODEBUILDMCP";

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn plugins(cwd: &std::path::Path, resource_mode: &str) -> Vec<Box<dyn Plugin>> {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp15_server.py");
    let config = McpServerConfig {
        command: "python3".into(),
        args: vec![
            "-u".into(),
            fixture.to_string_lossy().into_owned(),
            "stdio".into(),
        ],
        env: HashMap::from([("HEYCODE_MCP_RESOURCE_MODE".into(), resource_mode.into())]),
        required: true,
    };
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
        mcp_plugin(
            HashMap::from([("fixture".into(), McpServerSpec::Stdio(config))]),
            cwd.to_path_buf(),
        ),
    ]
}

fn production_mcp_plugins(cwd: &Path, node: &Path, cli: &Path) -> Vec<Box<dyn Plugin>> {
    let config = McpServerConfig {
        command: node.to_string_lossy().into_owned(),
        args: vec![cli.to_string_lossy().into_owned(), "mcp".into()],
        env: HashMap::from([
            ("HOME".into(), cwd.to_string_lossy().into_owned()),
            ("TMPDIR".into(), cwd.to_string_lossy().into_owned()),
            (
                "XCODEBUILDMCP_CWD".into(),
                cwd.to_string_lossy().into_owned(),
            ),
            ("XCODEBUILDMCP_SENTRY_DISABLED".into(), "true".into()),
            (
                "XCODEBUILDMCP_DISABLE_SESSION_DEFAULTS".into(),
                "true".into(),
            ),
            ("XCODEBUILDMCP_DISABLE_XCODE_AUTO_SYNC".into(), "1".into()),
        ]),
        required: true,
    };
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
        mcp_plugin(
            HashMap::from([("xcodebuildmcp".into(), McpServerSpec::Stdio(config))]),
            cwd.to_path_buf(),
        ),
    ]
}

/// Matches the extension host's nested MCP activation after the ordinary CLI
/// transport plugin, without installing a real external plugin or provider.
struct BundledMcpFixture(Box<dyn Plugin>);

impl Plugin for BundledMcpFixture {
    fn name(&self) -> &'static str {
        "bundled-mcp-fixture"
    }
    fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
        self.0.apply(context)
    }
}

#[tokio::test]
async fn ordinary_and_bundled_mcp_activations_share_model_resource_access() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let mut plugins = plugins(&cwd, "default");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp15_server.py");
    let bundled = mcp_plugin(
        HashMap::from([(
            "bundled".into(),
            McpServerSpec::Stdio(McpServerConfig {
                command: "python3".into(),
                args: vec![
                    "-u".into(),
                    fixture.to_string_lossy().into_owned(),
                    "stdio".into(),
                ],
                env: HashMap::new(),
                required: true,
            }),
        )]),
        cwd,
    );
    plugins.push(Box::new(BundledMcpFixture(bundled)));
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let list = tools.get("list_mcp_resources").unwrap();
    let servers = list
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(servers["returned"], 2);
    for server in ["fixture", "bundled"] {
        let resources = list
            .run(serde_json::json!({"server":server}), &ToolCtx::default())
            .await
            .unwrap();
        assert_eq!(resources["resources"][0]["uri"], "fixture://resource/1");
        let read = tools
            .get("read_mcp_resource")
            .unwrap()
            .run(
                serde_json::json!({"server":server,"uri":"fixture://resource/1"}),
                &ToolCtx::default(),
            )
            .await
            .unwrap();
        assert_eq!(read["contents"][0]["body"]["text"], "fixture resource body");
    }
    let ready = tools
        .get("wait_for_mcp_servers")
        .unwrap()
        .run(
            serde_json::json!({"servers":["fixture","bundled"],"timeout_ms":0}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(ready["outcome"], "all_ready");
    context.shutdown();
    for name in [
        "list_mcp_resources",
        "read_mcp_resource",
        "wait_for_mcp_servers",
    ] {
        assert!(
            tools.get(name).is_none(),
            "{name} must retire after both activations"
        );
    }
}

/// Opt-in compatibility canary for an installed open-source MCP package. The
/// server receives a synthetic cwd and synthetic home only; the test invokes no
/// build, device, simulator, UI, credential or network operation.
#[tokio::test]
#[ignore = "set HEYCODE_REAL_MCP_NODE and HEYCODE_REAL_MCP_XCODEBUILDMCP to absolute installed paths"]
#[allow(clippy::print_stderr)]
async fn installed_xcodebuildmcp_resources_cross_the_public_model_tools() {
    let node = PathBuf::from(std::env::var_os(REAL_MCP_NODE_ENV).expect(REAL_MCP_NODE_ENV));
    let cli = PathBuf::from(
        std::env::var_os(REAL_MCP_XCODEBUILDMCP_ENV).expect(REAL_MCP_XCODEBUILDMCP_ENV),
    );
    assert!(node.is_absolute() && cli.is_absolute());
    let version = Command::new(&node)
        .arg(&cli)
        .arg("--version")
        .output()
        .expect("XcodeBuildMCP version probe must start");
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).unwrap().trim().to_owned();

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("synthetic.txt"), "compatibility fixture only\n").unwrap();
    let plugins = production_mcp_plugins(&root, &node, &cli);
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();

    let wait = tools.get("wait_for_mcp_servers").unwrap();
    let ready = wait
        .run(
            serde_json::json!({"servers":["xcodebuildmcp"],"timeout_ms":15_000}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(ready["outcome"], "all_ready");

    let list = tools.get("list_mcp_resources").unwrap();
    let resources = list
        .run(
            serde_json::json!({"server":"xcodebuildmcp","limit":32}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    let rows = resources["resources"].as_array().unwrap();
    assert!(rows.len() >= 4, "unexpected resource listing: {resources}");
    assert!(
        rows.iter()
            .any(|row| { row["uri"] == serde_json::json!("xcodebuildmcp://session-status") })
    );

    let read = tools.get("read_mcp_resource").unwrap();
    let session = read
        .run(
            serde_json::json!({
                "server":"xcodebuildmcp",
                "uri":"xcodebuildmcp://session-status"
            }),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(session["contents"][0]["body"]["kind"], "text");
    let body = session["contents"][0]["body"]["text"].as_str().unwrap();
    serde_json::from_str::<serde_json::Value>(body).expect("session status must remain JSON");
    assert!(
        !body.contains(std::env::current_dir().unwrap().to_string_lossy().as_ref()),
        "the third-party server exposed the real checkout"
    );

    let error = read
        .run(
            serde_json::json!({
                "server":"xcodebuildmcp",
                "uri":"xcodebuildmcp://not-listed"
            }),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "MCP resource is not in the committed server listing"
    );
    assert!(!error.message.contains("not-listed"));

    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    let error = read
        .run(
            serde_json::json!({
                "server":"xcodebuildmcp",
                "uri":"xcodebuildmcp://session-status"
            }),
            &ToolCtx {
                cwd: root.clone(),
                cancellation: cancelled,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "MCP resource read failed: MCP request was cancelled"
    );

    eprintln!(
        "REAL_MCP_COMPAT package=xcodebuildmcp version={version:?} resources={} ready=all_ready read=session-status invalid_uri=body_free precancel=cancelled cwd=<TEMP> home=<TEMP>",
        rows.len(),
    );
    context.shutdown();
    for name in [
        "list_mcp_resources",
        "read_mcp_resource",
        "wait_for_mcp_servers",
    ] {
        assert!(tools.get(name).is_none(), "{name} must retire");
    }
}

#[tokio::test]
async fn composed_resource_tools_list_read_wait_and_retire_with_the_connection() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = plugins(&cwd, "default");
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();

    let list = tools.get("list_mcp_resources").unwrap();
    assert_eq!(
        list.untrusted_content(),
        Some(UntrustedContentBoundary::mcp())
    );
    let servers = list
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(servers["returned"], 1);
    assert_eq!(servers["servers"][0]["server"], "fixture");
    assert_eq!(servers["servers"][0]["state"], "ready");
    assert_eq!(servers["servers"][0]["resources_exposed"], true);
    assert_eq!(servers["servers"][0]["resource_count"], 1);

    let resources = list
        .run(
            serde_json::json!({"server":"fixture","limit":1}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(resources["resources"][0]["uri"], "fixture://resource/1");
    assert_eq!(resources["continuation"], serde_json::Value::Null);

    let read = tools.get("read_mcp_resource").unwrap();
    assert_eq!(read.prerequisite_status().configured, Some(true));
    assert_eq!(
        read.untrusted_content(),
        Some(UntrustedContentBoundary::mcp())
    );
    let content = read
        .run(
            serde_json::json!({
                "server":"fixture",
                "uri":"fixture://resource/1"
            }),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(content["contents"][0]["body"]["kind"], "text");
    assert_eq!(
        content["contents"][0]["body"]["text"],
        "fixture resource body"
    );
    assert_eq!(content["truncated"], false);

    let wait = tools.get("wait_for_mcp_servers").unwrap();
    let settled = wait
        .run(
            serde_json::json!({"servers":["fixture"],"timeout_ms":0}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(settled["outcome"], "all_ready");
    assert_eq!(settled["all_ready"], true);

    context.shutdown();
    for name in [
        "list_mcp_resources",
        "read_mcp_resource",
        "wait_for_mcp_servers",
    ] {
        assert!(tools.get(name).is_none(), "{name} must be effect-owned");
    }
}

#[tokio::test]
async fn pagination_truncation_binary_omission_and_argument_failures_are_bounded() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = plugins(&cwd, "rich");
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let list = tools.get("list_mcp_resources").unwrap();
    let first = list
        .run(
            serde_json::json!({"server":"fixture","limit":1}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(first["returned"], 1);
    assert_eq!(first["total"], 3);
    let continuation = first["continuation"].clone();
    let second = list
        .run(
            serde_json::json!({
                "server":"fixture","limit":1,"cursor":continuation
            }),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(second["resources"][0]["uri"], "fixture://resource/rich");

    let mut stale = second["continuation"].clone();
    stale["resource_generation"] = serde_json::json!(999_999);
    let error = list
        .run(
            serde_json::json!({"server":"fixture","limit":1,"cursor":stale}),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("stale"));

    let read = tools.get("read_mcp_resource").unwrap();
    let error = read
        .run(
            serde_json::json!({
                "server":"fixture","uri":"fixture://resource/not-listed"
            }),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "MCP resource is not in the committed server listing"
    );
    assert!(!error.message.contains("not-listed"));
    let content = read
        .run(
            serde_json::json!({
                "server":"fixture","uri":"fixture://resource/rich"
            }),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        content["contents"][0]["body"]["text"]
            .as_str()
            .unwrap()
            .len(),
        64 * 1024
    );
    assert_eq!(content["contents"][1]["body"]["kind"], "binary_omitted");
    assert_eq!(content["contents"][1]["body"]["bytes"], 14);
    assert_eq!(content["truncated"], true);

    for (tool, args) in [
        (
            "list_mcp_resources",
            serde_json::json!({"server":"INVALID\nCANARY"}),
        ),
        (
            "read_mcp_resource",
            serde_json::json!({"server":"INVALID\nCANARY","uri":"fixture://resource/1"}),
        ),
        (
            "wait_for_mcp_servers",
            serde_json::json!({"servers":["INVALID\nCANARY"],"timeout_ms":0}),
        ),
    ] {
        let error = tools
            .get(tool)
            .unwrap()
            .run(args, &ToolCtx::default())
            .await
            .unwrap_err();
        assert!(!error.message.contains("CANARY"), "{tool}: {error}");
    }
    context.shutdown();
}

#[tokio::test]
async fn resource_read_cancellation_settles_and_teardown_retires_every_tool() {
    if !python3_available() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let plugins = plugins(&cwd, "slow");
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let read = tools.get("read_mcp_resource").unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let task = {
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            read.run(
                serde_json::json!({
                    "server":"fixture","uri":"fixture://resource/1"
                }),
                &ToolCtx {
                    cwd: std::path::PathBuf::from("."),
                    cancellation,
                },
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancellation.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("cancelled resource read must settle")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.message,
        "MCP resource read failed: MCP request was cancelled"
    );
    context.shutdown();
    for name in [
        "list_mcp_resources",
        "read_mcp_resource",
        "wait_for_mcp_servers",
    ] {
        assert!(tools.get(name).is_none(), "{name} must retire");
    }
}

#[test]
fn resource_tools_are_available_even_when_no_servers_are_configured() {
    let cwd = std::env::current_dir().unwrap();
    let mut servers = HashMap::new();
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
        mcp_plugin(std::mem::take(&mut servers), cwd),
    ];
    let mut context = compose(&plugins).unwrap();
    let tools = context
        .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    for name in [
        "list_mcp_resources",
        "read_mcp_resource",
        "wait_for_mcp_servers",
    ] {
        assert!(tools.get(name).is_some(), "{name} must be discoverable");
    }
    assert_eq!(
        tools
            .get("list_mcp_resources")
            .unwrap()
            .prerequisite_status()
            .configured,
        Some(true)
    );
    assert_eq!(
        tools
            .get("wait_for_mcp_servers")
            .unwrap()
            .prerequisite_status()
            .configured,
        Some(true)
    );
    assert_eq!(
        tools
            .get("read_mcp_resource")
            .unwrap()
            .prerequisite_status()
            .configured,
        Some(false)
    );
    context.shutdown();
}

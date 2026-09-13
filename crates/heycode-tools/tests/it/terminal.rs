//! E07 model-facing persistent terminals: a real PTY through the tool surface.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_exec::{SubprocessService, TerminalOwner, TerminalService};
use heycode_tools::{Tool, ToolCtx, builtins::terminal::terminal_tools};
use serde_json::{Value, json};

fn tools(owner: &str) -> (TerminalService, Vec<Arc<dyn Tool>>) {
    let subprocess = SubprocessService::local();
    let terminals = TerminalService::new(subprocess.clone());
    let tools = terminal_tools(
        terminals.clone(),
        subprocess,
        TerminalOwner::new(owner).unwrap(),
        std::env::current_dir().unwrap(),
    );
    (terminals, tools)
}

fn tool<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
    tools
        .iter()
        .find(|tool| tool.spec().name == name)
        .expect("tool must be registered")
}

async fn call(tools: &[Arc<dyn Tool>], name: &str, args: Value) -> Value {
    tool(tools, name)
        .run(args, &ToolCtx::default())
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {error}"))
}

/// Drain until `needle` appears or the budget runs out. A PTY delivers output
/// asynchronously, so a single read is not a fair test.
async fn read_until(tools: &[Arc<dyn Tool>], id: &str, needle: &str) -> String {
    let mut seen = String::new();
    for _ in 0..200 {
        let read = call(tools, "terminal_read", json!({"terminal_id": id})).await;
        seen.push_str(read["output"].as_str().unwrap_or_default());
        if seen.contains(needle) {
            return seen;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("terminal never produced `{needle}`; saw: {seen}");
}

#[cfg(unix)]
#[tokio::test]
async fn the_tool_surface_opens_writes_reads_resizes_lists_and_kills_one_terminal() {
    let (_service, tools) = tools("session-a");
    assert_eq!(tools.len(), 6);

    let opened = call(
        &tools,
        "terminal_open",
        json!({"command": "sh", "cols": 80, "rows": 24}),
    )
    .await;
    let id = opened["terminal_id"].as_str().unwrap().to_owned();

    // A persistent terminal keeps state between calls: the variable set by the
    // first write is still there for the second, which a one-shot bash tool
    // could not do.
    call(
        &tools,
        "terminal_write",
        json!({"terminal_id": id, "input": "MARK=alpha\n"}),
    )
    .await;
    call(
        &tools,
        "terminal_write",
        json!({"terminal_id": id, "input": "echo value-$MARK\n"}),
    )
    .await;
    read_until(&tools, &id, "value-alpha").await;

    // Resize is observed by the live child, not just recorded.
    call(
        &tools,
        "terminal_resize",
        json!({"terminal_id": id, "cols": 120, "rows": 40}),
    )
    .await;
    call(
        &tools,
        "terminal_write",
        json!({"terminal_id": id, "input": "stty size\n"}),
    )
    .await;
    read_until(&tools, &id, "40 120").await;

    let listed = call(&tools, "terminal_list", json!({})).await;
    let rows = listed.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["terminal_id"], id.as_str());
    assert_eq!(rows[0]["cols"], 120);
    assert_eq!(rows[0]["rows"], 40);

    call(&tools, "terminal_kill", json!({"terminal_id": id})).await;
    assert!(
        call(&tools, "terminal_list", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty(),
        "a killed terminal leaves the list"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn one_session_cannot_name_another_sessions_terminal() {
    let subprocess = SubprocessService::local();
    let terminals = TerminalService::new(subprocess.clone());
    let cwd = std::env::current_dir().unwrap();
    let make = |owner: &str| {
        terminal_tools(
            terminals.clone(),
            subprocess.clone(),
            TerminalOwner::new(owner).unwrap(),
            cwd.clone(),
        )
    };
    let mine = make("session-a");
    let theirs = make("session-b");

    let opened = call(&mine, "terminal_open", json!({"command": "sh"})).await;
    let id = opened["terminal_id"].as_str().unwrap().to_owned();

    // The other session sees nothing, and every operation on a known-good id
    // is refused indistinguishably from an unknown one — the id is not a
    // capability.
    assert!(
        call(&theirs, "terminal_list", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    for (name, args) in [
        ("terminal_read", json!({"terminal_id": id})),
        (
            "terminal_write",
            json!({"terminal_id": id, "input": "echo leak\n"}),
        ),
        (
            "terminal_resize",
            json!({"terminal_id": id, "cols": 10, "rows": 10}),
        ),
        ("terminal_kill", json!({"terminal_id": id})),
    ] {
        let error = tool(&theirs, name)
            .run(args, &ToolCtx::default())
            .await
            .expect_err("a foreign session must be refused");
        assert!(
            error.to_string().contains("no such terminal"),
            "{name} must not reveal that the id exists: {error}"
        );
    }

    // The owning session is unaffected by the attempts.
    assert_eq!(
        call(&mine, "terminal_list", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    call(&mine, "terminal_kill", json!({"terminal_id": id})).await;
}

#[tokio::test]
async fn tool_failures_stay_fixed_text_and_never_echo_the_terminal_stream() {
    let (_service, tools) = tools("session-a");
    // An unknown id is refused without disclosing anything.
    let error = tool(&tools, "terminal_read")
        .run(
            json!({"terminal_id": "00000000-0000-4000-8000-000000000000"}),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no such terminal"));

    // A malformed id is rejected at the boundary, not passed through.
    assert!(
        tool(&tools, "terminal_write")
            .run(
                json!({"terminal_id": "not-a-uuid", "input": "x"}),
                &ToolCtx::default()
            )
            .await
            .is_err()
    );
    // Oversized input is bounded before it reaches the terminal.
    assert!(
        tool(&tools, "terminal_write")
            .run(
                json!({
                    "terminal_id": "00000000-0000-4000-8000-000000000000",
                    "input": "x".repeat(70 * 1024)
                }),
                &ToolCtx::default()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("64 KiB")
    );
}

struct NoPtyShell;
#[async_trait::async_trait]
impl heycode_exec::ShellBackend for NoPtyShell {
    fn resolve(
        &self,
        _: heycode_exec::ShellRequest,
    ) -> Result<heycode_exec::ShellSpec, heycode_exec::ProcessError> {
        Err(heycode_exec::ProcessError::new(
            heycode_exec::ProcessErrorCode::Unsupported,
        ))
    }
    async fn execute(
        &self,
        _: heycode_exec::ShellSpec,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_exec::ProcessOutput, heycode_exec::ProcessError> {
        Err(heycode_exec::ProcessError::new(
            heycode_exec::ProcessErrorCode::Unsupported,
        ))
    }
}

#[tokio::test]
async fn unsupported_workspace_pty_cannot_fall_back_to_parent_executor() {
    let root = tempfile::tempdir().unwrap();
    let policy = heycode_exec::FileSystemPolicy::from_sandbox(&heycode_exec::SandboxPolicy {
        mode: heycode_exec::SandboxMode::WorkspaceWrite,
        workspace_root: root.path().to_path_buf(),
    })
    .unwrap();
    let filesystem = heycode_exec::FileSystemService::local(policy).unwrap();
    let shell = heycode_exec::ShellService::new(Arc::new(NoPtyShell));
    let (service, tools) = tools("no-parent-fallback");
    let rebound = tool(&tools, "terminal_open")
        .rebind_workspace(&filesystem, &shell)
        .unwrap();
    let result = rebound
        .run(json!({"command":"sh"}), &ToolCtx::default())
        .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("does not support terminal launch")
    );
    assert!(
        service
            .list(&TerminalOwner::new("no-parent-fallback").unwrap())
            .await
            .is_empty()
    );
}

//! Real production composition acceptance for workspace authority changes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_agent::workspace_transition::{
    SERVICE_WORKSPACE_TRANSITION, WorkspaceTransitionHandle, WorkspaceTransitionOrigin,
};
use heycode_cli::testing::RealCompositionHarness;
use heycode_exec::{FileSystemService, PathRequest, ReadFileSpec, ShellRequest, ShellService};
use heycode_llm::{ChatRequest, FinishReason, Provider, ProviderInfo, StreamChunk};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn token() -> CancellationToken {
    CancellationToken::new()
}
fn native(context: &heycode_core::Context) -> Arc<heycode_agent::Agent> {
    context.get(heycode_agent::SERVICE_AGENT).unwrap()
}
async fn command(context: &heycode_core::Context, name: &str, args: &str) -> anyhow::Result<()> {
    context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get(name)
        .unwrap()
        .unwrap()
        .execute(&native(context), args)
        .await
}
async fn pwd(shell: &ShellService) -> PathBuf {
    let out = shell
        .execute(
            shell.resolve(ShellRequest::new("pwd -P").unwrap()).unwrap(),
            token(),
        )
        .await
        .unwrap();
    assert!(out.exit().is_success());
    PathBuf::from(String::from_utf8(out.stdout().to_vec()).unwrap().trim())
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn repository(root: &Path) {
    std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["add", "tracked.txt"]);
    git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
}

#[derive(Default)]
struct Capture {
    requests: Mutex<Vec<ChatRequest>>,
    scripts: Mutex<Vec<Vec<StreamChunk>>>,
}
#[async_trait::async_trait]
impl Provider for Capture {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "fake".into(),
            default_model: "fake-model".into(),
        }
    }
    fn stream(&self, request: ChatRequest) -> heycode_llm::ChunkStream {
        self.requests.lock().unwrap().push(request);
        let mut scripts = self.scripts.lock().unwrap();
        let chunks = if scripts.is_empty() {
            vec![
                StreamChunk::TextDelta("done".into()),
                StreamChunk::Finish(FinishReason::Stop),
            ]
        } else {
            scripts.remove(0)
        };
        Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
    }
}

#[tokio::test]
async fn production_add_cd_updates_captured_file_shell_tools_and_prompt_provenance() {
    let harness = RealCompositionHarness::new()
        .unwrap()
        .with_trusted_workspace();
    let original = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    let child = original.join("nested dir");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(original.join("AGENTS.md"), "OLD_ROOT_GUIDANCE").unwrap();
    std::fs::write(child.join("AGENTS.md"), "NEW_CURRENT_GUIDANCE").unwrap();
    let actual_memory = harness
        .sessions_dir()
        .join(".agent-memory/user/workspace-fixture");
    let incorrect_memory = harness
        .credentials_root()
        .join(".agent-memory/user/workspace-fixture");
    for path in [&actual_memory, &incorrect_memory] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(actual_memory.join("MEMORY.md"), "ACTUAL_CHILD_MEMORY").unwrap();
    std::fs::write(incorrect_memory.join("MEMORY.md"), "WRONG_CREDENTIAL_ROOT").unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = std::fs::canonicalize(outside.path()).unwrap();
    std::fs::write(outside_path.join("readme.txt"), "authorized outside\n").unwrap();
    let capture = Arc::new(Capture::default());
    let world = harness.with_provider(capture.clone()).compose().unwrap();
    let context = world.context();
    let memory = context
        .get::<heycode_tui::memory_commands::MemorySourceManagerHandle>(
            heycode_tui::memory_commands::SERVICE_MEMORY_SOURCES,
        )
        .unwrap();
    assert_eq!(
        memory.0.read("auto:user:workspace-fixture").unwrap().text,
        "ACTUAL_CHILD_MEMORY"
    );
    let filesystem = context
        .get::<FileSystemService>(heycode_exec::SERVICE_FILESYSTEM)
        .unwrap();
    let shell = context
        .get::<ShellService>(heycode_exec::SERVICE_SHELL)
        .unwrap();
    let registry = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        filesystem
            .resolve(PathRequest::new(&original, outside_path.join("readme.txt")).unwrap())
            .is_err()
    );
    command(context, "cd", "nested dir").await.unwrap();
    assert_eq!(native(context).cwd(), child);
    assert_eq!(pwd(&shell).await, child);
    native(context)
        .send("Report current guidance")
        .await
        .unwrap();
    let request = capture.requests.lock().unwrap()[0].clone();
    let rendered = request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("NEW_CURRENT_GUIDANCE"),
        "sources={:?}\nrequest={rendered}",
        native(context).current_instruction_sources()
    );
    assert!(!rendered.contains("OLD_ROOT_GUIDANCE"));
    std::fs::write(child.join("AGENTS.md"), "UPDATED_AFTER_COMPOSITION").unwrap();
    native(context)
        .send("Read refreshed guidance")
        .await
        .unwrap();
    assert!(
        capture.requests.lock().unwrap()[1]
            .messages
            .iter()
            .any(|message| message.content.contains("UPDATED_AFTER_COMPOSITION"))
    );
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    assert_eq!(
        commands
            .get("memory")
            .unwrap()
            .unwrap()
            .descriptor()
            .source(),
        &heycode_agent::CommandSource::from_plugin("memory-commands").unwrap()
    );
    assert_eq!(
        commands
            .get("reload-skills")
            .unwrap()
            .unwrap()
            .descriptor()
            .source(),
        &heycode_agent::CommandSource::from_plugin("skills").unwrap()
    );
    let reload_availability = commands
        .get("reload-skills")
        .unwrap()
        .unwrap()
        .availability();
    assert!(!reload_availability.is_available());
    assert!(
        reload_availability.reason().is_some_and(
            |reason| reason.contains("workspace changed") && reason.contains("recompose")
        ),
        "unexpected reload availability: {reload_availability:?}"
    );
    command(context, "add-dir", outside_path.to_str().unwrap())
        .await
        .unwrap();
    command(context, "cd", outside_path.to_str().unwrap())
        .await
        .unwrap();
    let resolved = filesystem
        .resolve(PathRequest::new(&outside_path, "readme.txt").unwrap())
        .unwrap();
    filesystem
        .read(ReadFileSpec::new(resolved.clone(), 4096).unwrap(), token())
        .await
        .unwrap();
    assert!(
        registry.observations().contains(resolved.as_path()),
        "registry diagnostics follow the active filesystem log"
    );
    assert_eq!(pwd(&shell).await, outside_path);
    assert!(
        native(context)
            .current_instruction_sources()
            .unwrap()
            .workspace
            .is_none(),
        "file grants do not trust project guidance"
    );
}

#[tokio::test]
async fn production_worktree_commands_rebind_existing_tools_and_preserve_dirty_source() {
    let harness = RealCompositionHarness::new().unwrap();
    let root = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    repository(&root);
    std::fs::write(root.join("tracked.txt"), "dirty\n").unwrap();
    std::fs::write(root.join("untracked.txt"), "new\n").unwrap();
    let status = git(&root, &["status", "--porcelain=v1", "-z"]);
    let world = harness.compose().unwrap();
    let context = world.context();
    let shell = context
        .get::<ShellService>(heycode_exec::SERVICE_SHELL)
        .unwrap();
    command(context, "worktree", "enter").await.unwrap();
    let active = native(context).workspace_snapshot().unwrap().unwrap();
    assert_ne!(active.cwd, root);
    assert_eq!(pwd(&shell).await, active.cwd);
    assert_eq!(
        std::fs::read(active.cwd.join("tracked.txt")).unwrap(),
        b"dirty\n"
    );
    assert_eq!(
        std::fs::read(active.cwd.join("untracked.txt")).unwrap(),
        b"new\n"
    );
    assert_eq!(git(&root, &["status", "--porcelain=v1", "-z"]), status);
    command(context, "worktree", "exit").await.unwrap();
    assert_eq!(native(context).cwd(), root);
    assert_eq!(pwd(&shell).await, root);
    assert!(active.cwd.exists());
    assert_eq!(
        context
            .get::<WorkspaceTransitionHandle>(SERVICE_WORKSPACE_TRANSITION)
            .unwrap()
            .0
            .snapshot()
            .unwrap()
            .retained_worktrees,
        vec![active.cwd]
    );
}

#[tokio::test]
async fn production_busy_job_and_open_terminal_refuse_without_sidecar_changes() {
    let harness = RealCompositionHarness::new().unwrap();
    let root = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    std::fs::create_dir(root.join("child")).unwrap();
    let world = harness.compose().unwrap();
    let context = world.context();
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let id = jobs
        .spawn(
            "workspace fixture",
            heycode_session::InboxDelivery::Inject,
            |_id, cancellation| async move { cancellation.cancelled().await },
        )
        .unwrap();
    assert!(
        command(context, "cd", "child")
            .await
            .unwrap_err()
            .to_string()
            .contains("jobs")
    );
    let rejected = native(context).acquire_recomposition_permit().await;
    assert!(rejected.err().unwrap().to_string().contains("jobs"));
    let _ = jobs.cancel(&id);
    jobs.wait_for_task_exit(&id).await.unwrap();
    let terminal = context
        .get::<heycode_exec::TerminalService>(heycode_exec::SERVICE_TERMINAL)
        .unwrap();
    let shell = context
        .get::<ShellService>(heycode_exec::SERVICE_SHELL)
        .unwrap();
    let owner = heycode_exec::TerminalOwner::new("workspace-fixture").unwrap();
    let spec = heycode_exec::TerminalSpec::new(
        shell
            .resolve(ShellRequest::new("cat").unwrap().without_timeout())
            .unwrap()
            .into_process()
            .with_interactive_stdio(),
    )
    .unwrap();
    let id = terminal.open(&owner, spec).await.unwrap();
    assert!(
        command(context, "cd", "child")
            .await
            .unwrap_err()
            .to_string()
            .contains("terminals")
    );
    let rejected = native(context).acquire_recomposition_permit().await;
    assert!(rejected.err().unwrap().to_string().contains("terminals"));
    terminal.kill(&owner, &id).await.unwrap();
    command(context, "cd", "child").await.unwrap();
    assert_eq!(native(context).cwd(), root.join("child"));
}

#[tokio::test]
async fn direct_model_tool_and_auxiliary_capture_cannot_bypass_admission() {
    let harness = RealCompositionHarness::new().unwrap();
    let root = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    repository(&root);
    let world = harness.compose().unwrap();
    let context = world.context();
    let service = context
        .get::<WorkspaceTransitionHandle>(SERVICE_WORKSPACE_TRANSITION)
        .unwrap();
    assert!(
        service
            .0
            .enter_worktree(WorkspaceTransitionOrigin::ModelTool, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("foreground tool barrier")
    );
    let capture = service.0.pin_activity().unwrap();
    assert!(
        command(context, "worktree", "enter")
            .await
            .unwrap_err()
            .to_string()
            .contains("operations still own")
    );
    drop(capture);
    command(context, "worktree", "enter").await.unwrap();
}

fn tool_script(name: &str, args: serde_json::Value) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: Some(format!("fixture-{name}")),
            name: Some(name.into()),
            arguments_delta: args.to_string(),
        },
        StreamChunk::Finish(FinishReason::ToolCalls),
    ]
}

#[tokio::test]
async fn real_foreground_model_barrier_can_enter_read_and_exit_in_one_turn() {
    let harness = RealCompositionHarness::new().unwrap();
    let root = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    repository(&root);
    let capture = Arc::new(Capture::default());
    *capture.scripts.lock().unwrap() = vec![
        tool_script("EnterWorktree", serde_json::json!({})),
        tool_script("bash", serde_json::json!({"command":"pwd -P"})),
        tool_script("ExitWorktree", serde_json::json!({})),
    ];
    let world = harness.with_provider(capture.clone()).compose().unwrap();
    native(world.context())
        .send("Enter a worktree, report pwd, then exit it")
        .await
        .unwrap();
    let snapshot = native(world.context())
        .workspace_snapshot()
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.cwd, root);
    assert_eq!(snapshot.retained_worktrees.len(), 1);
    let requests = capture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let observed = requests[2]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        observed.contains(snapshot.retained_worktrees[0].to_str().unwrap()),
        "shell must execute in the newly admitted worktree: {observed}"
    );
}

async fn rpc(
    context: &heycode_core::Context,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let server = context
        .get::<heycode_app_server::AppServer>(heycode_app_server::SERVICE_APP_SERVER)
        .unwrap();
    let (events, _receiver) = tokio::sync::mpsc::channel(64);
    let response = server
        .request(
            &serde_json::json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params})
                .to_string(),
            events,
            token(),
        )
        .await;
    serde_json::from_str(&response).unwrap()
}

#[tokio::test]
async fn native_protocol_reports_current_workspace_and_recomposition_closes_admission() {
    let harness = RealCompositionHarness::new().unwrap();
    let root = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    std::fs::create_dir(root.join("child")).unwrap();
    let world = harness.compose().unwrap();
    let context = world.context();
    let opened = rpc(context, "session/open", serde_json::json!({})).await;
    assert_eq!(opened["result"]["cwd"], root.to_str().unwrap(), "{opened}");
    command(context, "cd", "child").await.unwrap();
    let reopened = rpc(context, "session/open", serde_json::json!({})).await;
    assert_eq!(
        reopened["result"]["cwd"],
        root.join("child").to_str().unwrap(),
        "{reopened}"
    );
    let agent = native(context);
    let workspace = agent.workspace_service().unwrap();
    let permit = agent.acquire_recomposition_permit().await.unwrap();
    assert!(workspace.pin_activity().is_err());
    drop(permit);
    assert!(workspace.pin_activity().is_ok());
    let permit = agent.acquire_recomposition_permit().await.unwrap();
    permit.begin_shutdown();
    assert!(workspace.pin_activity().is_err());
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        rpc(
            context,
            "session/close",
            serde_json::json!({"sessionId":opened["result"]["sessionId"]}),
        ),
    )
    .await
    .unwrap();
    assert!(closed.get("error").is_none(), "{closed}");
    let refused = rpc(context, "session/open", serde_json::json!({})).await;
    assert!(refused.get("error").is_some());
    tokio::time::timeout(std::time::Duration::from_secs(2), agent.shutdown_and_wait())
        .await
        .unwrap();
}

#[tokio::test]
async fn production_terminal_launch_uses_the_current_workspace_and_blocks_transition() {
    let harness = RealCompositionHarness::new().unwrap();
    let root = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    std::fs::create_dir(root.join("child")).unwrap();
    let capture = Arc::new(Capture::default());
    *capture.scripts.lock().unwrap() = vec![tool_script(
        "terminal_open",
        serde_json::json!({"command":"sh", "args":["-c", "pwd -P; sleep 30"]}),
    )];
    let world = harness.with_provider(capture).compose().unwrap();
    let context = world.context();
    command(context, "cd", "child").await.unwrap();
    let agent = native(context);
    agent.send("Open the cwd fixture terminal").await.unwrap();
    let terminals = context
        .get::<heycode_exec::TerminalService>(heycode_exec::SERVICE_TERMINAL)
        .unwrap();
    let owner = heycode_exec::TerminalOwner::new(format!(
        "session:{}",
        agent.session().lock().unwrap().id()
    ))
    .unwrap();
    let opened = terminals.list(&owner).await;
    assert_eq!(opened.len(), 1);
    let id = opened[0].id();
    let mut output = Vec::new();
    for _ in 0..30 {
        output.extend_from_slice(terminals.read(&owner, id, 4096).await.unwrap().bytes());
        if !output.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        String::from_utf8(output).unwrap().trim(),
        root.join("child").to_str().unwrap()
    );
    assert!(command(context, "cd", "..").await.is_err());
    terminals.kill(&owner, id).await.unwrap();
    command(context, "cd", "..").await.unwrap();
}

#[tokio::test]
async fn production_no_argument_add_directory_uses_attached_prompt_and_rechecks_authority() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use heycode_tui::add_directory::{
        AddDirectoryBridge, AddDirectoryOutcome, SERVICE_ADD_DIRECTORY_PROMPT,
    };
    let harness = RealCompositionHarness::new().unwrap();
    let original = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    let child = original.join("nested");
    std::fs::create_dir(&child).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = std::fs::canonicalize(outside.path()).unwrap();
    std::fs::write(outside_path.join("readme.txt"), "dialog granted\n").unwrap();
    let world = harness.compose().unwrap();
    let context = world.context();
    let service = context
        .get::<WorkspaceTransitionHandle>(SERVICE_WORKSPACE_TRANSITION)
        .unwrap()
        .0
        .clone();
    let filesystem = context
        .get::<FileSystemService>(heycode_exec::SERVICE_FILESYSTEM)
        .unwrap();
    let bridge = context
        .get::<AddDirectoryBridge>(SERVICE_ADD_DIRECTORY_PROMPT)
        .unwrap();
    assert!(
        command(context, "add-dir", "")
            .await
            .unwrap_err()
            .to_string()
            .contains("interactive terminal")
    );
    let attachment = bridge.attach().unwrap();
    command(context, "add-dir", "").await.unwrap();
    assert!(command(context, "add-dir", "").await.is_err());
    let mut dialog = bridge.take().unwrap();
    dialog.paste(&outside_path.display().to_string());
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    dialog.key(enter);
    assert_eq!(service.snapshot().unwrap().revision, 0);
    assert!(
        filesystem
            .resolve(PathRequest::new(&original, outside_path.join("readme.txt")).unwrap())
            .is_err()
    );
    command(context, "cd", "nested").await.unwrap();
    dialog.key(enter);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while dialog.running() {
            assert!(dialog.poll().is_none());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        dialog
            .plain_lines()
            .join("\n")
            .contains("Workspace changed")
    );
    assert_eq!(service.snapshot().unwrap().roots.len(), 1);
    dialog.key(enter);
    dialog.key(enter);
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(result) = dialog.poll() {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let AddDirectoryOutcome::Granted { path, snapshot } = result else {
        panic!("grant expected")
    };
    assert_eq!(path, outside_path);
    assert_eq!(snapshot.cwd, child);
    assert_eq!(snapshot.roots.len(), 2);
    let resolved = filesystem
        .resolve(PathRequest::new(&child, outside_path.join("readme.txt")).unwrap())
        .unwrap();
    let read = filesystem
        .read(ReadFileSpec::new(resolved, 4096).unwrap(), token())
        .await
        .unwrap();
    assert_eq!(read.bytes(), b"dialog granted\n");
    drop(dialog);
    command(context, "add-dir", "").await.unwrap();
    drop(attachment);
    assert!(bridge.take().is_none());
    assert_eq!(service.snapshot().unwrap().roots.len(), 2);
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use heycode_exec::{
    FileSystemErrorCode, LocalShellConfig, PathRequest, ReadFileSpec, ShellRequest, WriteFileSpec,
};
use tempfile::TempDir;

use super::*;

struct Admission {
    busy: AtomicBool,
}

#[tokio::test]
async fn resume_rejects_exact_sandbox_mode_drift() {
    let fixture = Fixture::new();
    fixture
        .service
        .add_directory(
            &fixture.extra,
            WorkspaceTransitionOrigin::HumanCommand,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut journal = state::WorkspaceJournal::load(&fixture.state)
        .unwrap()
        .unwrap();
    assert_eq!(journal.sandbox_mode, SandboxMode::Off.as_str());
    journal.sandbox_mode = SandboxMode::WorkspaceWrite.as_str().to_owned();
    journal.save(&fixture.state).unwrap();
    let result = WorkspaceTransitionService::open(
        fixture.state.clone(),
        fixture.root.clone(),
        fixture.sandbox.clone(),
        fixture.resolver.clone(),
        SubprocessService::local(),
        fixture.worktrees.clone(),
    );
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("original root and sandbox mode")
    );
}

#[tokio::test]
async fn prepared_directory_grant_rechecks_revision_identity_and_cancellation() {
    let fixture = Fixture::new();
    let candidate = fixture
        .service
        .prepare_directory_grant(&fixture.extra)
        .unwrap();
    assert_eq!(candidate.path(), fixture.extra);
    assert!(
        !fixture.state.exists(),
        "preview must not grant or journal authority"
    );
    let child = fixture.root.join("child");
    std::fs::create_dir(&child).unwrap();
    fixture
        .service
        .change_directory(&child, HUMAN, token())
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .confirm_directory_grant(candidate, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("Workspace changed")
    );
    let candidate = fixture
        .service
        .prepare_directory_grant(&fixture.extra)
        .unwrap();
    std::fs::rename(&fixture.extra, fixture.extra.with_file_name("old-extra")).unwrap();
    std::fs::create_dir(&fixture.extra).unwrap();
    assert!(
        fixture
            .service
            .confirm_directory_grant(candidate, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("Directory changed")
    );
    let candidate = fixture
        .service
        .prepare_directory_grant(&fixture.extra)
        .unwrap();
    let cancelled = token();
    cancelled.cancel();
    assert!(
        fixture
            .service
            .confirm_directory_grant(candidate, cancelled)
            .await
            .is_err()
    );
    assert_eq!(fixture.service.snapshot().unwrap().roots.len(), 1);
    let candidate = fixture
        .service
        .prepare_directory_grant(&fixture.extra)
        .unwrap();
    let result = fixture
        .service
        .confirm_directory_grant(candidate, token())
        .await
        .unwrap();
    assert_eq!(result.roots.len(), 2);
    assert_eq!(result.cwd, child);
}

#[tokio::test]
async fn entry_from_repository_subdirectory_requires_the_complete_repo_grant() {
    let fixture = Fixture::new();
    fixture.git(&["init", "-q"]);
    let child = fixture.root.join("child");
    std::fs::create_dir(&child).unwrap();
    let sandbox = SandboxService::new(SandboxMode::Off, child.clone(), None).unwrap();
    let service = WorkspaceTransitionService::open(
        fixture.state.clone(),
        child.clone(),
        sandbox,
        ShellService::local(
            LocalShellConfig::platform(child.clone(), Duration::from_secs(5)).unwrap(),
        ),
        SubprocessService::local(),
        fixture.worktrees.clone(),
    )
    .unwrap();
    service.install_guard(fixture.guard.clone()).unwrap();
    let refused = service
        .enter_worktree(
            WorkspaceTransitionOrigin::HumanCommand,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("repository root is outside authorized"),
        "{refused}"
    );
    assert_eq!(service.snapshot().unwrap().cwd, child);
    assert!(!fixture.state.exists());
    assert!(!fixture.worktrees.exists());
}
#[async_trait]
impl WorkspaceTransitionGuard for Admission {
    async fn acquire(
        &self,
        _origin: WorkspaceTransitionOrigin,
    ) -> Result<Box<dyn WorkspaceTransitionPermit>> {
        if self.busy.load(Ordering::SeqCst) {
            Err(WorkspaceTransitionError::new(
                "A delegated runtime, resumable child, or active job owns the workspace",
            ))
        } else {
            // Fixtures have one sequential host admission owner. The service's
            // local operation fence is exercised independently below.
            Ok(Box::new(()))
        }
    }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    extra: PathBuf,
    state: PathBuf,
    worktrees: PathBuf,
    sandbox: SandboxService,
    resolver: ShellService,
    guard: Arc<Admission>,
    service: Arc<WorkspaceTransitionService>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let root = base.join("source");
        let extra = base.join("extra");
        let session = base.join("session");
        for directory in [&root, &extra, &session] {
            std::fs::create_dir(directory).unwrap();
        }
        let state = session.join("workspace.json");
        let worktrees = base.join("worktrees");
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(extra.join("external.txt"), "outside\n").unwrap();
        let sandbox = SandboxService::new(SandboxMode::Off, root.clone(), None).unwrap();
        let resolver = ShellService::local(
            LocalShellConfig::platform(root.clone(), Duration::from_secs(5)).unwrap(),
        );
        let service = WorkspaceTransitionService::open(
            state.clone(),
            root.clone(),
            sandbox.clone(),
            resolver.clone(),
            SubprocessService::local(),
            worktrees.clone(),
        )
        .unwrap();
        let guard = Arc::new(Admission {
            busy: AtomicBool::new(false),
        });
        service.install_guard(guard.clone()).unwrap();
        Self {
            _temp: temp,
            root,
            extra,
            state,
            worktrees,
            sandbox,
            resolver,
            guard,
            service,
        }
    }
    fn reopen(&self) -> Arc<WorkspaceTransitionService> {
        let service = WorkspaceTransitionService::open(
            self.state.clone(),
            self.root.clone(),
            self.sandbox.clone(),
            self.resolver.clone(),
            SubprocessService::local(),
            self.worktrees.clone(),
        )
        .unwrap();
        service.install_guard(self.guard.clone()).unwrap();
        service
    }
    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = std::process::Command::new("git")
            .current_dir(&self.root)
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
    fn repository(&self) {
        self.git(&["init", "-q"]);
        self.git(&["add", "tracked.txt"]);
        self.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ]);
    }
}

fn token() -> CancellationToken {
    CancellationToken::new()
}
const HUMAN: WorkspaceTransitionOrigin = WorkspaceTransitionOrigin::HumanCommand;
const MODEL: WorkspaceTransitionOrigin = WorkspaceTransitionOrigin::ModelTool;

async fn read(
    filesystem: &FileSystemService,
    cwd: &Path,
    path: &Path,
) -> std::result::Result<Vec<u8>, heycode_exec::FileSystemError> {
    let path = filesystem.resolve(PathRequest::new(cwd, path)?)?;
    Ok(filesystem
        .read(ReadFileSpec::new(path, 4096)?, token())
        .await?
        .bytes()
        .to_vec())
}
async fn pwd(shell: &ShellService) -> PathBuf {
    let spec = shell.resolve(ShellRequest::new("pwd -P").unwrap()).unwrap();
    let output = shell.execute(spec, token()).await.unwrap();
    assert!(output.exit().is_success());
    PathBuf::from(String::from_utf8(output.stdout().to_vec()).unwrap().trim())
}

#[tokio::test]
async fn add_and_cd_rebind_existing_handles_and_reopen_committed_state() {
    let fixture = Fixture::new();
    let fs = fixture.service.filesystem();
    let shell = fixture.service.shell();
    let pinned_fs = fixture.service.pinned_filesystem().unwrap();
    let pinned_shell = fixture.service.pinned_shell().unwrap();
    assert_eq!(
        read(&fs, &fixture.root, &fixture.extra.join("external.txt"))
            .await
            .unwrap_err()
            .code(),
        FileSystemErrorCode::OutsideAllowedRoots
    );
    assert!(
        fixture
            .service
            .change_directory(&fixture.extra, HUMAN, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("/add-dir")
    );
    fixture
        .service
        .add_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
    assert_eq!(
        read(&fs, &fixture.root, &fixture.extra.join("external.txt"))
            .await
            .unwrap(),
        b"outside\n"
    );
    assert!(
        read(
            &pinned_fs,
            &fixture.root,
            &fixture.extra.join("external.txt")
        )
        .await
        .is_err()
    );
    fixture
        .service
        .change_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
    assert_eq!(pwd(&shell).await, fixture.extra);
    assert_eq!(pwd(&pinned_shell).await, fixture.root);
    // Explicit request cwd survives a changed provider default.
    let spec = shell
        .resolve(
            ShellRequest::new("pwd -P")
                .unwrap()
                .with_cwd(fixture.root.clone())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(spec.cwd(), fixture.root);
    let created = fs
        .resolve(PathRequest::new(&fixture.extra, "created.txt").unwrap())
        .unwrap();
    fs.write(
        WriteFileSpec::new(created, b"new scope\n").unwrap(),
        token(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(fixture.extra.join("created.txt")).unwrap(),
        b"new scope\n"
    );
    assert!(!fixture.root.join("created.txt").exists());
    let resumed = fixture.reopen();
    assert_eq!(resumed.snapshot().unwrap().cwd, fixture.extra);
    assert_eq!(pwd(&resumed.shell()).await, fixture.extra);
    assert_eq!(resumed.snapshot().unwrap().revision, 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&fixture.state)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn grants_do_not_trust_new_project_instructions_and_cd_refreshes_source() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("AGENTS.md"), "old guidance").unwrap();
    let child = fixture.root.join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("AGENTS.md"), "new child guidance").unwrap();
    fixture
        .service
        .change_directory(&child, HUMAN, token())
        .await
        .unwrap();
    let sources = fixture.service.instruction_sources(None, true).unwrap();
    let rendered = heycode_prompt::instructions::render_instructions(
        &heycode_prompt::instructions::discover_instructions(&sources),
    );
    assert!(rendered.contains("new child guidance"));
    assert!(!rendered.contains("old guidance"));
    fixture
        .service
        .add_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
    fixture
        .service
        .change_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .instruction_sources(None, true)
            .unwrap()
            .workspace
            .is_none()
    );
    assert!(
        fixture
            .service
            .instruction_sources(None, false)
            .unwrap()
            .workspace
            .is_none()
    );
}

#[tokio::test]
async fn live_operations_and_host_ownership_refuse_without_mutating_authority() {
    let fixture = Fixture::new();
    fixture.guard.busy.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .service
            .add_directory(&fixture.extra, HUMAN, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("owns the workspace")
    );
    fixture.guard.busy.store(false, Ordering::SeqCst);
    let operation = fixture.service.operation().unwrap();
    assert!(
        fixture
            .service
            .add_directory(&fixture.extra, HUMAN, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("operations still own")
    );
    drop(operation);
    let cancelled = token();
    cancelled.cancel();
    assert!(
        fixture
            .service
            .add_directory(&fixture.extra, HUMAN, cancelled)
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .add_directory(&fixture.extra, MODEL, token())
            .await
            .is_err()
    );
    assert_eq!(fixture.service.snapshot().unwrap().revision, 0);
    assert!(!fixture.state.exists());
    fixture
        .service
        .add_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
}

#[tokio::test]
async fn actual_shell_execution_pins_scope_until_process_settles() {
    let fixture = Fixture::new();
    let shell = fixture.service.shell();
    let marker = fixture.root.join("started");
    let spec = shell
        .resolve(ShellRequest::new("touch started; sleep 0.3").unwrap())
        .unwrap();
    let task = tokio::spawn(async move { shell.execute(spec, token()).await });
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(marker.exists());
    assert!(
        fixture
            .service
            .add_directory(&fixture.extra, HUMAN, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("operations still own")
    );
    assert!(task.await.unwrap().unwrap().exit().is_success());
    fixture
        .service
        .add_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
}

#[tokio::test]
async fn worktree_entry_and_exit_rebind_file_shell_scope_and_preserve_source_results() {
    let fixture = Fixture::new();
    fixture.repository();
    std::fs::write(fixture.root.join("tracked.txt"), "dirty edit\n").unwrap();
    std::fs::write(fixture.root.join("untracked.txt"), "new source file\n").unwrap();
    let before_status = fixture.git(&["status", "--porcelain=v1", "-z"]);
    let before_index = std::fs::read(fixture.root.join(".git/index")).unwrap();
    let fs = fixture.service.filesystem();
    let shell = fixture.service.shell();
    let active = fixture
        .service
        .enter_worktree(MODEL, token())
        .await
        .unwrap();
    let path = active.cwd.clone();
    assert_ne!(path, fixture.root);
    assert_eq!(pwd(&shell).await, path);
    assert_eq!(
        read(&fs, &path, Path::new("tracked.txt")).await.unwrap(),
        b"dirty edit\n"
    );
    assert_eq!(
        read(&fs, &path, Path::new("untracked.txt")).await.unwrap(),
        b"new source file\n"
    );
    assert!(
        read(&fs, &path, &fixture.root.join("tracked.txt"))
            .await
            .is_err()
    );
    assert_eq!(
        fixture.git(&["status", "--porcelain=v1", "-z"]),
        before_status
    );
    assert_eq!(
        std::fs::read(fixture.root.join(".git/index")).unwrap(),
        before_index
    );
    assert!(
        fixture
            .service
            .enter_worktree(MODEL, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("Already in")
    );
    assert_eq!(
        fixture
            .service
            .instruction_sources(None, true)
            .unwrap()
            .workspace,
        Some(path.clone())
    );
    let resumed = fixture.reopen();
    assert_eq!(resumed.snapshot().unwrap().cwd, path);
    assert_eq!(pwd(&resumed.shell()).await, path);
    let stale_spec = shell.resolve(ShellRequest::new("pwd -P").unwrap()).unwrap();
    std::fs::write(path.join("result.txt"), "retained result\n").unwrap();
    let returned = fixture.service.exit_worktree(MODEL, token()).await.unwrap();
    assert_eq!(returned.cwd, fixture.root);
    assert!(returned.retained_worktrees.contains(&path));
    assert_eq!(pwd(&shell).await, fixture.root);
    assert!(shell.execute(stale_spec, token()).await.is_err());
    assert!(
        read(&fs, &fixture.root, &path.join("result.txt"))
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(path.join("result.txt")).unwrap(),
        b"retained result\n"
    );
    assert!(!fixture.root.join("result.txt").exists());
}

#[tokio::test]
async fn clean_worktrees_are_durably_retained_on_exit_and_later_entry() {
    let fixture = Fixture::new();
    fixture.repository();
    let first = fixture
        .service
        .enter_worktree(HUMAN, token())
        .await
        .unwrap();
    let tree = first.worktree.clone().unwrap();
    fixture.service.exit_worktree(HUMAN, token()).await.unwrap();
    assert!(tree.path.exists());
    let manager = GitWorktreeManager::new(
        SubprocessService::local(),
        fixture.root.clone(),
        tree.manager_root,
        WorktreeRetention::RetainOnFailure,
    )
    .unwrap();
    manager.recover(token()).await.unwrap();
    assert!(
        tree.path.exists(),
        "explicit retention survives ordinary manager recovery"
    );
    fixture
        .service
        .enter_worktree(HUMAN, token())
        .await
        .unwrap();
    assert!(
        tree.path.exists(),
        "a later entry cannot implicitly recover/delete an earlier checkout"
    );
}

#[tokio::test]
async fn interrupted_setup_is_visible_recoverable_and_never_changes_scope() {
    let fixture = Fixture::new();
    // Valid repository authority; no commit exists, so setup fails after its
    // durable pending boundary rather than during the read-only preflight.
    fixture.git(&["init", "-q"]);
    let error = fixture
        .service
        .enter_worktree(HUMAN, token())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Recovery inspection"));
    let pending = fixture.service.snapshot().unwrap();
    assert_eq!(pending.cwd, fixture.root);
    assert!(pending.pending_recovery.is_some());
    let resumed = fixture.reopen();
    assert!(resumed.snapshot().unwrap().pending_recovery.is_some());
    assert!(
        resumed
            .add_directory(&fixture.extra, HUMAN, token())
            .await
            .unwrap_err()
            .to_string()
            .contains("recovery")
    );
    assert!(resumed.recover_pending(MODEL, token()).await.is_err());
    let recovered = resumed.recover_pending(HUMAN, token()).await.unwrap();
    assert!(recovered.pending_recovery.is_none());
    assert_eq!(recovered.cwd, fixture.root);
    assert_eq!(recovered.retained_worktrees.len(), 1);
    resumed
        .add_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_durable_commit_leaves_actual_file_and_shell_authority_unchanged() {
    let fixture = Fixture::new();
    std::fs::create_dir(&fixture.state).unwrap();
    assert!(
        fixture
            .service
            .add_directory(&fixture.extra, HUMAN, token())
            .await
            .is_err()
    );
    assert_eq!(fixture.service.snapshot().unwrap().roots.len(), 1);
    assert_eq!(pwd(&fixture.service.shell()).await, fixture.root);
    assert!(
        read(
            &fixture.service.filesystem(),
            &fixture.root,
            &fixture.extra.join("external.txt")
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn saved_root_replacement_is_refused_on_reopen() {
    let fixture = Fixture::new();
    fixture
        .service
        .add_directory(&fixture.extra, HUMAN, token())
        .await
        .unwrap();
    let moved = fixture.extra.with_file_name("moved");
    std::fs::rename(&fixture.extra, &moved).unwrap();
    std::fs::create_dir(&fixture.extra).unwrap();
    assert!(
        WorkspaceTransitionService::open(
            fixture.state.clone(),
            fixture.root.clone(),
            fixture.sandbox.clone(),
            fixture.resolver.clone(),
            SubprocessService::local(),
            fixture.worktrees.clone()
        )
        .err()
        .unwrap()
        .to_string()
        .contains("identity changed")
    );
}

#[tokio::test]
async fn worktree_tools_refuse_force_or_cleanup_options_and_use_mutation_barriers() {
    use heycode_tools::{Tool, ToolCtx, ToolEffect};
    let fixture = Fixture::new();
    let enter = EnterWorktreeTool::new(fixture.service.clone());
    let exit = ExitWorktreeTool::new(fixture.service.clone());
    assert_eq!(enter.effect(), ToolEffect::Mutates);
    assert_eq!(exit.effect(), ToolEffect::Mutates);
    assert!(!enter.supports_background());
    assert!(
        enter
            .run(serde_json::json!({"force":true}), &ToolCtx::default())
            .await
            .is_err()
    );
    assert!(
        exit.run(serde_json::json!({"delete":true}), &ToolCtx::default())
            .await
            .is_err()
    );
}

//! Actual scoped registries: provider authority, cwd, handle ownership and lifecycle.
use super::*;

async fn scoped_call(
    parent: &Context,
    registry: &ToolRegistry,
    root: &Path,
    name: &str,
    args: Value,
) -> anyhow::Result<crate::ToolOutcome> {
    crate::execute_tool(
        registry,
        &parent
            .get::<heycode_core::Waterfall<crate::PreToolDecision>>(crate::SEAM_PRE_TOOL)
            .unwrap(),
        crate::ToolCallInput {
            name: name.into(),
            args,
        },
        &ToolCtx::default().with_cwd(root.to_path_buf()),
    )
    .await
}

#[tokio::test]
async fn child_file_authority_and_artifact_state_never_retain_parent_scope() {
    let parent_dir = tempfile::tempdir().unwrap();
    let child_dir = tempfile::tempdir().unwrap();
    let sibling_dir = tempfile::tempdir().unwrap();
    let original = sample().to_string();
    std::fs::write(parent_dir.path().join("n.ipynb"), &original).unwrap();
    std::fs::write(parent_dir.path().join("parent.txt"), "parent artifact").unwrap();
    std::fs::write(parent_dir.path().join("parent.wav"), wav()).unwrap();
    std::fs::write(child_dir.path().join("n.ipynb"), &original).unwrap();
    std::fs::write(child_dir.path().join("child.txt"), "child artifact").unwrap();
    let mut parent = world(parent_dir.path(), None);
    let tools = parent.get::<ToolRegistry>(crate::SERVICE_TOOLS).unwrap();
    let shell = parent
        .get::<heycode_exec::ShellService>(heycode_exec::SERVICE_SHELL)
        .unwrap();
    let filesystem = crate::builtins::test_filesystem(child_dir.path());
    let child = tools.for_workspace(&filesystem, &shell).unwrap();
    let sibling = tools
        .for_workspace(
            &crate::builtins::test_filesystem(sibling_dir.path()),
            &shell,
        )
        .unwrap();
    let parent_artifact = call(
        &parent,
        parent_dir.path(),
        "artifact",
        json!({"action":"register","path":"parent.txt"}),
    )
    .await
    .unwrap()
    .value;
    assert!(
        scoped_call(
            &parent,
            &child,
            child_dir.path(),
            "artifact",
            json!({"action":"preview","id":parent_artifact["id"]})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("Unknown artifact")
    );
    let child_artifact = scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "artifact",
        json!({"action":"register","path":"child.txt"}),
    )
    .await
    .unwrap()
    .value;
    assert_eq!(
        scoped_call(
            &parent,
            &child,
            child_dir.path(),
            "artifact",
            json!({"action":"preview","id":child_artifact["id"]})
        )
        .await
        .unwrap()
        .value["text"],
        "child artifact"
    );
    assert!(
        scoped_call(
            &parent,
            &sibling,
            sibling_dir.path(),
            "artifact",
            json!({"action":"preview","id":child_artifact["id"]})
        )
        .await
        .is_err()
    );
    let read = scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "notebook_read",
        json!({"path":"n.ipynb"}),
    )
    .await
    .unwrap()
    .value;
    scoped_call(&parent,&child,child_dir.path(),"notebook_edit",json!({"path":"n.ipynb","expected_revision":read["revision"],"action":"replace","cell_index":0,"source":"print('child only')"})).await.unwrap();
    assert!(
        std::fs::read_to_string(child_dir.path().join("n.ipynb"))
            .unwrap()
            .contains("child only")
    );
    assert_eq!(
        std::fs::read_to_string(parent_dir.path().join("n.ipynb")).unwrap(),
        original
    );
    for (name, args) in [
        (
            "notebook_read",
            json!({"path":parent_dir.path().join("n.ipynb")}),
        ),
        (
            "notebook_edit",
            json!({"path":parent_dir.path().join("n.ipynb"),"expected_revision":read["revision"],"action":"delete","cell_index":0}),
        ),
        (
            "artifact",
            json!({"action":"register","path":parent_dir.path().join("parent.txt")}),
        ),
        (
            "browser",
            json!({"action":"preview","session":1,"path":parent_dir.path().join("parent.txt")}),
        ),
    ] {
        assert!(
            scoped_call(&parent, &child, child_dir.path(), name, args)
                .await
                .unwrap_err()
                .to_string()
                .contains("outside"),
            "{name} must use child file authority"
        );
    }
    #[cfg(target_os = "macos")]
    assert!(scoped_call(&parent,&child,child_dir.path(),"computer",json!({"action":"screenshot","bundle_id":"org.heycode.audit-computer-fixture","path":parent_dir.path().join("forbidden.png")})).await.unwrap_err().to_string().contains("outside"),"file admission must deny before any native helper/capture");
    assert!(!parent_dir.path().join("forbidden.png").exists());
    let readonly = heycode_exec::FileSystemService::local(
        heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
            child_dir.path(),
            heycode_exec::FileSystemRootAccess::ReadOnly,
        )
        .unwrap()])
        .unwrap(),
    )
    .unwrap();
    let nested = child.for_workspace(&readonly, &shell).unwrap();
    let read = scoped_call(
        &parent,
        &nested,
        child_dir.path(),
        "notebook_read",
        json!({"path":"n.ipynb"}),
    )
    .await
    .unwrap()
    .value;
    assert!(scoped_call(&parent,&nested,child_dir.path(),"notebook_edit",json!({"path":"n.ipynb","expected_revision":read["revision"],"action":"delete","cell_index":0})).await.is_err(),"nested rebind must preserve read-only policy");
    drop(sibling);
    assert_eq!(
        call(
            &parent,
            parent_dir.path(),
            "artifact",
            json!({"action":"preview","id":parent_artifact["id"]})
        )
        .await
        .unwrap()
        .value["text"],
        "parent artifact"
    );
    let held = child.get("notebook_read").unwrap();
    drop(child);
    parent.shutdown();
    assert!(
        held.run(
            json!({"path":"n.ipynb"}),
            &ToolCtx::default().with_cwd(child_dir.path().to_path_buf())
        )
        .await
        .unwrap_err()
        .message
        .contains("stopped")
    );
}

struct NoProcessShell;
#[async_trait::async_trait]
impl heycode_exec::ShellBackend for NoProcessShell {
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
        _: CancellationToken,
    ) -> Result<heycode_exec::ProcessOutput, heycode_exec::ProcessError> {
        Err(heycode_exec::ProcessError::new(
            heycode_exec::ProcessErrorCode::Unsupported,
        ))
    }
}
#[tokio::test]
async fn absent_child_executor_cannot_fall_back_to_parent_processes() {
    let parent_dir = tempfile::tempdir().unwrap();
    let child_dir = tempfile::tempdir().unwrap();
    std::fs::write(child_dir.path().join("n.ipynb"), sample().to_string()).unwrap();
    let parent = world(parent_dir.path(), None);
    let tools = parent.get::<ToolRegistry>(crate::SERVICE_TOOLS).unwrap();
    let child = tools
        .for_workspace(
            &crate::builtins::test_filesystem(child_dir.path()),
            &heycode_exec::ShellService::new(Arc::new(NoProcessShell)),
        )
        .unwrap();
    for name in ["browser", "computer", "transcribe_audio"] {
        assert!(
            scoped_call(
                &parent,
                &child,
                child_dir.path(),
                name,
                json!({"action":"status"})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("provided workspace services")
        );
    }
    scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "notebook_read",
        json!({"path":"n.ipynb"}),
    )
    .await
    .unwrap();
    scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "artifact",
        json!({"action":"register","path":"n.ipynb"}),
    )
    .await
    .unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn child_speech_uses_exact_cwd_and_supplied_process_sandbox() {
    let parent_dir = tempfile::tempdir().unwrap();
    let child_dir = tempfile::tempdir().unwrap();
    let forbidden = parent_dir.path().join("protected.txt");
    std::fs::write(&forbidden, "parent unchanged").unwrap();
    std::fs::write(parent_dir.path().join("parent.wav"), wav()).unwrap();
    std::fs::write(child_dir.path().join("child.wav"), wav()).unwrap();
    let config=SpeechCommandConfig::new("/bin/sh".into(),vec!["-c".into(),"cat >/dev/null; /bin/pwd > speech-cwd.txt; if printf changed > \"$1\"; then printf escaped; else printf scoped-transcript; fi".into(),"fixture".into(),forbidden.to_string_lossy().into_owned()]).unwrap();
    let parent = world_with_speech(parent_dir.path(), None, Some(config));
    let sandbox = heycode_exec::SandboxService::new(
        heycode_exec::SandboxMode::WorkspaceWrite,
        child_dir.path(),
        Some(heycode_sandbox::platform_default().unwrap()),
    )
    .unwrap();
    let filesystem = heycode_exec::FileSystemService::local(
        heycode_exec::FileSystemPolicy::from_sandbox(sandbox.policy()).unwrap(),
    )
    .unwrap();
    let shell = parent
        .get::<heycode_exec::ShellService>(heycode_exec::SERVICE_SHELL)
        .unwrap()
        .with_executor(heycode_exec::SubprocessService::local_with_sandbox(sandbox));
    let child = parent
        .get::<ToolRegistry>(crate::SERVICE_TOOLS)
        .unwrap()
        .for_workspace(&filesystem, &shell)
        .unwrap();
    let result = scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "transcribe_audio",
        json!({"action":"transcribe","path":"child.wav"}),
    )
    .await
    .unwrap()
    .value;
    assert_eq!(result["text"], "scoped-transcript");
    assert_eq!(
        std::fs::read_to_string(child_dir.path().join("speech-cwd.txt"))
            .unwrap()
            .trim(),
        std::fs::canonicalize(child_dir.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(!parent_dir.path().join("speech-cwd.txt").exists());
    assert_eq!(
        std::fs::read_to_string(&forbidden).unwrap(),
        "parent unchanged"
    );
    assert!(
        scoped_call(
            &parent,
            &child,
            child_dir.path(),
            "transcribe_audio",
            json!({"action":"transcribe","path":parent_dir.path().join("parent.wav")})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("outside")
    );
}

#[cfg(target_os = "macos")]
struct RecordingRaw {
    inner: heycode_exec::SubprocessService,
    cwds: std::sync::Mutex<Vec<std::path::PathBuf>>,
}
#[cfg(target_os = "macos")]
#[async_trait::async_trait]
impl heycode_exec::SubprocessBackend for RecordingRaw {
    fn resolve_program(
        &self,
        program: &std::ffi::OsStr,
    ) -> Result<std::path::PathBuf, heycode_exec::ProcessError> {
        self.inner.resolve_program(program)
    }
    fn containment(&self) -> heycode_exec::SubprocessContainment {
        self.inner.containment()
    }
    async fn output(
        &self,
        spec: heycode_exec::ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::ProcessOutput, heycode_exec::ProcessError> {
        self.inner.output(spec, cancellation).await
    }
    async fn spawn(
        &self,
        _: heycode_exec::ProcessSpec,
        _: CancellationToken,
    ) -> Result<Box<dyn heycode_exec::ManagedProcessHandle>, heycode_exec::ProcessError> {
        Err(heycode_exec::ProcessError::new(
            heycode_exec::ProcessErrorCode::Unsupported,
        ))
    }
    async fn spawn_interactive(
        &self,
        _: heycode_exec::ProcessSpec,
        _: CancellationToken,
    ) -> Result<heycode_exec::InteractiveProcess, heycode_exec::ProcessError> {
        Err(heycode_exec::ProcessError::new(
            heycode_exec::ProcessErrorCode::Unsupported,
        ))
    }
    async fn spawn_interactive_raw(
        &self,
        spec: heycode_exec::ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::RawInteractiveProcess, heycode_exec::ProcessError> {
        self.cwds.lock().unwrap().push(spec.cwd().to_path_buf());
        self.inner.spawn_interactive_raw(spec, cancellation).await
    }
}
#[cfg(target_os = "macos")]
fn browser_scope(
    parent: &Context,
    root: &Path,
    mode: heycode_exec::SandboxMode,
) -> (ToolRegistry, Arc<RecordingRaw>) {
    let sandbox = heycode_exec::SandboxService::new(
        mode,
        root,
        Some(heycode_sandbox::platform_default().unwrap()),
    )
    .unwrap();
    let fs = heycode_exec::FileSystemService::local(
        heycode_exec::FileSystemPolicy::from_sandbox(sandbox.policy()).unwrap(),
    )
    .unwrap();
    let recorder = Arc::new(RecordingRaw {
        inner: heycode_exec::SubprocessService::local_with_sandbox(sandbox),
        cwds: std::sync::Mutex::new(Vec::new()),
    });
    let shell = parent
        .get::<heycode_exec::ShellService>(heycode_exec::SERVICE_SHELL)
        .unwrap()
        .with_executor(heycode_exec::SubprocessService::new(recorder.clone()));
    (
        parent
            .get::<ToolRegistry>(crate::SERVICE_TOOLS)
            .unwrap()
            .for_workspace(&fs, &shell)
            .unwrap(),
        recorder,
    )
}
#[cfg(target_os = "macos")]
fn private_browser_directories(root: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".heycode-browser-")
        })
        .collect()
}
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires explicit browser installation; isolated headless scopes only, no desktop capture"]
async fn browser_child_scopes_have_independent_handles_cwd_files_and_lifetime() {
    let parent_dir = tempfile::tempdir().unwrap();
    let child_dir = tempfile::tempdir().unwrap();
    let sibling_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        parent_dir.path().join("parent.html"),
        "<h1>Parent private preview</h1>",
    )
    .unwrap();
    std::fs::write(
        child_dir.path().join("child.html"),
        "<h1>Child generated preview</h1>",
    )
    .unwrap();
    let fixture = Fixture::start().await;
    let parent = world(
        parent_dir.path(),
        Some(BrowserConfig::from_environment().expect("Set browser paths")),
    );
    let root_browser = call(
        &parent,
        parent_dir.path(),
        "browser",
        json!({"action":"open","url":fixture.url,"allow_local":true}),
    )
    .await
    .unwrap()
    .value;
    let (readonly, readonly_process) = browser_scope(
        &parent,
        child_dir.path(),
        heycode_exec::SandboxMode::ReadOnly,
    );
    assert!(
        scoped_call(
            &parent,
            &readonly,
            child_dir.path(),
            "browser",
            json!({"action":"open","url":fixture.url,"allow_local":true})
        )
        .await
        .is_err(),
        "a child launch denied by process policy must not fall back to the running parent browser/executor"
    );
    assert!(readonly_process.cwds.lock().unwrap().is_empty());
    assert!(private_browser_directories(child_dir.path()).is_empty());
    drop(readonly);
    drop(readonly_process);
    // macOS cannot nest Chromium's own Seatbelt inside the workspace Seatbelt.
    // A specific error and clean shutdown are required, never a silent fallback.
    let (workspace_write, _) = browser_scope(
        &parent,
        child_dir.path(),
        heycode_exec::SandboxMode::WorkspaceWrite,
    );
    assert!(
        scoped_call(
            &parent,
            &workspace_write,
            child_dir.path(),
            "browser",
            json!({"action":"open","url":fixture.url,"allow_local":true})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("could not initialize its sandbox")
    );
    assert!(private_browser_directories(child_dir.path()).is_empty());
    drop(workspace_write);
    // Full access is selected explicitly by the host test configuration.
    let (child, child_process) =
        browser_scope(&parent, child_dir.path(), heycode_exec::SandboxMode::Off);
    let (sibling, sibling_process) =
        browser_scope(&parent, sibling_dir.path(), heycode_exec::SandboxMode::Off);
    let child_browser = scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "browser",
        json!({"action":"open","url":fixture.url,"allow_local":true}),
    )
    .await
    .unwrap()
    .value;
    let sibling_browser = scoped_call(
        &parent,
        &sibling,
        sibling_dir.path(),
        "browser",
        json!({"action":"open","url":fixture.url,"allow_local":true}),
    )
    .await
    .unwrap()
    .value;
    assert_ne!(root_browser["session"], child_browser["session"]);
    assert_ne!(child_browser["session"], sibling_browser["session"]);
    for root in [parent_dir.path(), child_dir.path(), sibling_dir.path()] {
        let directories = private_browser_directories(root);
        assert_eq!(directories.len(), 1);
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&directories[0])
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    for foreign in [&root_browser["session"], &sibling_browser["session"]] {
        assert!(
            scoped_call(
                &parent,
                &child,
                child_dir.path(),
                "browser",
                json!({"action":"inspect","session":foreign})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("Unknown browser session")
        );
    }
    let preview = scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "browser",
        json!({"action":"preview","session":child_browser["session"],"path":"child.html"}),
    )
    .await
    .unwrap()
    .value;
    assert!(
        preview["text"]
            .as_str()
            .unwrap()
            .contains("Child generated preview")
    );
    scoped_call(
        &parent,
        &child,
        child_dir.path(),
        "browser",
        json!({"action":"screenshot","session":child_browser["session"],"path":"child.png"}),
    )
    .await
    .unwrap();
    assert!(
        std::fs::read(child_dir.path().join("child.png"))
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert!(!parent_dir.path().join("child.png").exists());
    assert_eq!(
        *child_process.cwds.lock().unwrap(),
        vec![std::fs::canonicalize(child_dir.path()).unwrap()]
    );
    assert_eq!(
        *sibling_process.cwds.lock().unwrap(),
        vec![std::fs::canonicalize(sibling_dir.path()).unwrap()]
    );
    for (action, path) in [
        ("preview", parent_dir.path().join("parent.html")),
        ("screenshot", parent_dir.path().join("forbidden.png")),
    ] {
        assert!(
            scoped_call(
                &parent,
                &child,
                child_dir.path(),
                "browser",
                json!({"action":action,"session":child_browser["session"],"path":path})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("outside")
        );
    }
    assert!(!parent_dir.path().join("forbidden.png").exists());
    drop(child);
    drop(child_process);
    let root_alive = call(
        &parent,
        parent_dir.path(),
        "browser",
        json!({"action":"inspect","session":root_browser["session"]}),
    )
    .await
    .unwrap()
    .value;
    assert!(
        root_alive["text"]
            .as_str()
            .unwrap()
            .contains("Browser fixture")
    );
    let sibling_alive = scoped_call(
        &parent,
        &sibling,
        sibling_dir.path(),
        "browser",
        json!({"action":"inspect","session":sibling_browser["session"]}),
    )
    .await
    .unwrap()
    .value;
    assert!(
        sibling_alive["text"]
            .as_str()
            .unwrap()
            .contains("Browser fixture")
    );
    scoped_call(
        &parent,
        &sibling,
        sibling_dir.path(),
        "browser",
        json!({"action":"close","session":sibling_browser["session"]}),
    )
    .await
    .unwrap();
    assert!(private_browser_directories(sibling_dir.path()).is_empty());
    call(
        &parent,
        parent_dir.path(),
        "browser",
        json!({"action":"close","session":root_browser["session"]}),
    )
    .await
    .unwrap();
    assert!(private_browser_directories(parent_dir.path()).is_empty());
    fixture.close().await;
}

//! Mandatory sandbox-policy routing below every local process launch.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_exec::{
    ProcessSpec, SERVICE_SANDBOX, SERVICE_SUBPROCESS, Sandbox, SandboxError, SandboxMode,
    SandboxPolicy, SandboxService, SubprocessService, local_subprocess_plugin,
    sandbox_service_plugin,
};
use tokio_util::sync::CancellationToken;

type RecordedCalls = Arc<Mutex<Vec<(Vec<String>, SandboxPolicy)>>>;

struct RecordingSandbox {
    calls: RecordedCalls,
}

impl Sandbox for RecordingSandbox {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn capabilities(&self) -> heycode_exec::SandboxBackendCapabilities {
        heycode_exec::SandboxBackendCapabilities {
            read_only: heycode_exec::SandboxSupport::Supported,
            workspace_write: heycode_exec::SandboxSupport::Supported,
            network_isolation: heycode_exec::SandboxSupport::Unsupported,
        }
    }

    fn confine(
        &self,
        argv: &[String],
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        self.calls
            .lock()
            .unwrap()
            .push((argv.to_vec(), policy.clone()));
        Ok(argv.to_vec())
    }
}

#[tokio::test]
async fn subprocess_local_requires_and_routes_through_the_effective_sandbox_service() {
    let missing = vec![local_subprocess_plugin()];
    assert!(heycode_core::compose(&missing).is_err());

    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sandbox = SandboxService::new(
        SandboxMode::WorkspaceWrite,
        root.path().canonicalize().unwrap(),
        Some(Arc::new(RecordingSandbox {
            calls: calls.clone(),
        })),
    )
    .unwrap();
    let plugins = vec![sandbox_service_plugin(sandbox), local_subprocess_plugin()];
    let mut context = heycode_core::compose(&plugins).unwrap();
    assert_eq!(context.owner_of(SERVICE_SANDBOX), Some("sandbox-policy"));
    assert_eq!(
        context.owner_of(SERVICE_SUBPROCESS),
        Some("subprocess-local")
    );
    let subprocess = context
        .get::<SubprocessService>(SERVICE_SUBPROCESS)
        .unwrap();
    let output = subprocess
        .output(
            ProcessSpec::new(
                std::env::current_exe().unwrap(),
                root.path().canonicalize().unwrap(),
            )
            .unwrap()
            .with_args([
                "--exact".to_owned(),
                super::test_name(module_path!(), "sandbox_helper"),
                "--nocapture".to_owned(),
            ])
            .unwrap()
            .with_environment([("HEYCODE_SANDBOX_HELPER", "1")])
            .unwrap()
            .with_timeout(Some(Duration::from_secs(3)))
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.exit().is_success());
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].1.mode, SandboxMode::WorkspaceWrite);
    assert_eq!(
        recorded[0].1.workspace_root,
        root.path().canonicalize().unwrap()
    );
    drop(recorded);
    context.shutdown();
}

#[tokio::test]
async fn full_access_choice_keeps_backend_available_but_does_not_wrap() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sandbox = SandboxService::new(
        SandboxMode::Off,
        root.path().canonicalize().unwrap(),
        Some(Arc::new(RecordingSandbox {
            calls: calls.clone(),
        })),
    )
    .unwrap();
    let plugins = vec![sandbox_service_plugin(sandbox), local_subprocess_plugin()];
    let context = heycode_core::compose(&plugins).unwrap();
    let subprocess = context
        .get::<SubprocessService>(SERVICE_SUBPROCESS)
        .unwrap();
    let output = subprocess
        .output(
            ProcessSpec::new(
                std::env::current_exe().unwrap(),
                root.path().canonicalize().unwrap(),
            )
            .unwrap()
            .with_args([
                "--exact".to_owned(),
                super::test_name(module_path!(), "sandbox_helper"),
                "--nocapture".to_owned(),
            ])
            .unwrap()
            .with_environment([("HEYCODE_SANDBOX_HELPER", "1")])
            .unwrap()
            .with_timeout(Some(Duration::from_secs(3)))
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.exit().is_success());
    assert!(calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn restrictive_process_policy_rejects_a_swapped_workspace_root() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    let parked = parent.path().join("parked");
    std::fs::create_dir(&workspace).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sandbox = SandboxService::new(
        SandboxMode::WorkspaceWrite,
        &workspace,
        Some(Arc::new(RecordingSandbox {
            calls: calls.clone(),
        })),
    )
    .unwrap();
    let plugins = vec![sandbox_service_plugin(sandbox), local_subprocess_plugin()];
    let context = heycode_core::compose(&plugins).unwrap();
    let subprocess = context
        .get::<SubprocessService>(SERVICE_SUBPROCESS)
        .unwrap();
    std::fs::rename(&workspace, &parked).unwrap();
    symlink(outside.path(), &workspace).unwrap();

    let error = subprocess
        .output(
            ProcessSpec::new(std::env::current_exe().unwrap(), &parked).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), heycode_exec::ProcessErrorCode::Sandbox);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn sandbox_helper() {}

#[tokio::test]
async fn interactive_stdio_uses_the_same_sandbox_and_owned_tree_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sandbox = SandboxService::new(
        SandboxMode::ReadOnly,
        root.path().canonicalize().unwrap(),
        Some(Arc::new(RecordingSandbox {
            calls: calls.clone(),
        })),
    )
    .unwrap();
    let plugins = vec![sandbox_service_plugin(sandbox), local_subprocess_plugin()];
    let context = heycode_core::compose(&plugins).unwrap();
    let subprocess = context
        .get::<SubprocessService>(SERVICE_SUBPROCESS)
        .unwrap();
    let interactive = subprocess
        .spawn_interactive(
            ProcessSpec::new(
                std::env::current_exe().unwrap(),
                root.path().canonicalize().unwrap(),
            )
            .unwrap()
            .with_args([
                "--exact".to_owned(),
                super::test_name(module_path!(), "sandbox_interactive_helper"),
                "--nocapture".to_owned(),
            ])
            .unwrap()
            .with_environment([("HEYCODE_INTERACTIVE_HELPER", "1")])
            .unwrap()
            .with_timeout(Some(Duration::from_secs(3)))
            .unwrap()
            .with_interactive_stdio(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let (process, mut input, mut lines) = interactive.into_parts();
    input.write_line("hello").await.unwrap();
    let mut echoed = false;
    for _ in 0..20 {
        let Some(line) = lines.next_line().await.unwrap() else {
            break;
        };
        if line == "ECHO:hello" {
            echoed = true;
            break;
        }
    }
    assert!(echoed, "interactive helper did not echo its input");
    input.finish().await.unwrap();
    assert!(process.wait().await.unwrap().is_success());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[test]
fn sandbox_interactive_helper() {
    if std::env::var_os("HEYCODE_INTERACTIVE_HELPER").is_none() {
        return;
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    use std::io::Write as _;
    writeln!(std::io::stdout(), "ECHO:{}", line.trim_end()).unwrap();
    std::io::stdout().flush().unwrap();
}

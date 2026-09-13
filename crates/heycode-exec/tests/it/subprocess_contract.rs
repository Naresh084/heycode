//! Live exact-argv, environment and process-tree lifecycle contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use heycode_exec::{
    ProcessErrorCode, ProcessExit, ProcessSpec, SERVICE_SUBPROCESS, SandboxMode, SandboxService,
    SubprocessService, local_subprocess_plugin, sandbox_service_plugin,
};
use tokio_util::sync::CancellationToken;

fn helper_spec(root: &Path, mode: &str) -> ProcessSpec {
    ProcessSpec::new(
        std::env::current_exe().unwrap(),
        root.canonicalize().unwrap(),
    )
    .unwrap()
    .with_args([
        OsString::from("--exact"),
        OsString::from(super::test_name(module_path!(), "helper_process")),
        OsString::from("--nocapture"),
    ])
    .unwrap()
    .with_environment([(OsString::from("HEYCODE_EXEC_HELPER"), OsString::from(mode))])
    .unwrap()
    .with_timeout(Some(Duration::from_secs(5)))
    .unwrap()
}

fn service() -> SubprocessService {
    SubprocessService::local()
}

#[tokio::test]
async fn exact_run_clears_inherited_environment_and_reports_nonzero_as_output() {
    let root = tempfile::tempdir().unwrap();
    let spec = helper_spec(root.path(), "output")
        .with_environment([
            (
                OsString::from("HEYCODE_EXEC_HELPER"),
                OsString::from("output"),
            ),
            (
                OsString::from("HEYCODE_EXEC_VISIBLE"),
                OsString::from("visible-secret"),
            ),
        ])
        .unwrap();

    let output = service()
        .output(spec, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(output.exit(), &ProcessExit::Exited { code: 7 });
    assert!(
        String::from_utf8_lossy(output.stdout()).contains("path=absent visible=visible-secret")
    );
    assert!(output.stderr().contains("stderr-canary"));
    let debug = format!("{output:?}");
    assert!(!debug.contains("visible-secret"), "{debug}");
    assert!(!debug.contains("stderr-canary"), "{debug}");
}

#[tokio::test]
async fn timeout_is_a_result_and_output_overflow_fails_loud_without_body_leakage() {
    let root = tempfile::tempdir().unwrap();
    let timed_out = service()
        .output(
            helper_spec(root.path(), "sleep")
                .with_timeout(Some(Duration::from_millis(40)))
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(timed_out.exit(), &ProcessExit::TimedOut);

    let error = service()
        .output(
            helper_spec(root.path(), "flood")
                .with_output_limit_bytes(1024)
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::OutputLimit);
    assert!(!error.to_string().contains("flood-canary"));
    assert!(!format!("{error:?}").contains("flood-canary"));
}

#[tokio::test]
async fn spawn_wait_and_caller_cancellation_settle_quiescently() {
    let root = tempfile::tempdir().unwrap();
    let completed = service()
        .spawn(
            helper_spec(root.path(), "success"),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(completed, ProcessExit::Exited { code: 0 });

    let cancellation = CancellationToken::new();
    let running = service()
        .spawn(helper_spec(root.path(), "sleep"), cancellation.clone())
        .await
        .unwrap();
    cancellation.cancel();
    let error = running.wait().await.unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Cancelled);

    let running = service()
        .spawn(helper_spec(root.path(), "sleep"), CancellationToken::new())
        .await
        .unwrap();
    running.cancel().await.unwrap();

    let running = service()
        .spawn(helper_spec(root.path(), "sleep"), CancellationToken::new())
        .await
        .unwrap();
    let exit = running.terminate(Duration::from_millis(20)).await.unwrap();
    assert!(!exit.is_success());
}

#[tokio::test]
async fn hard_kill_reaps_the_descendant_tree() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("descendant-finished");
    let ready = root.path().join("descendant-started");
    let spec = helper_spec(root.path(), "tree-parent")
        .with_environment([
            (
                OsString::from("HEYCODE_EXEC_HELPER"),
                OsString::from("tree-parent"),
            ),
            (
                OsString::from("HEYCODE_EXEC_MARKER"),
                marker.clone().into_os_string(),
            ),
            (
                OsString::from("HEYCODE_EXEC_READY"),
                ready.clone().into_os_string(),
            ),
        ])
        .unwrap();
    let process = service()
        .spawn(spec, CancellationToken::new())
        .await
        .unwrap();

    wait_for_file(&ready).await;
    let exit = process.kill().await.unwrap();
    assert!(!exit.is_success());
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!marker.exists(), "descendant survived whole-tree kill");
}

#[tokio::test]
async fn dropping_the_owned_handle_reaps_the_descendant_tree() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("drop-descendant-finished");
    let ready = root.path().join("drop-descendant-started");
    let spec = helper_spec(root.path(), "tree-parent")
        .with_environment([
            (
                OsString::from("HEYCODE_EXEC_HELPER"),
                OsString::from("tree-parent"),
            ),
            (
                OsString::from("HEYCODE_EXEC_MARKER"),
                marker.clone().into_os_string(),
            ),
            (
                OsString::from("HEYCODE_EXEC_READY"),
                ready.clone().into_os_string(),
            ),
        ])
        .unwrap();
    let process = service()
        .spawn(spec, CancellationToken::new())
        .await
        .unwrap();

    wait_for_file(&ready).await;
    drop(process);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!marker.exists(), "descendant survived handle drop");
}

#[tokio::test]
async fn plugin_shutdown_cancels_active_processes_and_rejects_new_work() {
    let root = tempfile::tempdir().unwrap();
    let plugins = vec![
        sandbox_service_plugin(
            SandboxService::new(SandboxMode::Off, root.path().canonicalize().unwrap(), None)
                .unwrap(),
        ),
        local_subprocess_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    assert_eq!(
        context.owner_of(SERVICE_SUBPROCESS),
        Some("subprocess-local")
    );
    let service = context
        .get::<SubprocessService>(SERVICE_SUBPROCESS)
        .expect("subprocess service");
    let running = service
        .spawn(helper_spec(root.path(), "sleep"), CancellationToken::new())
        .await
        .unwrap();

    context.shutdown();
    let error = running.wait().await.unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Cancelled);
    let error = service
        .output(
            helper_spec(root.path(), "success"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::ServiceStopped);
}

#[test]
fn process_specs_reject_ambiguous_or_unbounded_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let absolute_root = root.path().canonicalize().unwrap();
    let executable = std::env::current_exe().unwrap();

    assert_eq!(
        ProcessSpec::new("relative-program", &absolute_root)
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        ProcessSpec::new(&executable, "relative-cwd")
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        ProcessSpec::new(&executable, &absolute_root)
            .unwrap()
            .with_environment([(OsString::from("BAD=NAME"), OsString::from("value"))])
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        ProcessSpec::new(&executable, &absolute_root)
            .unwrap()
            .with_timeout(Some(Duration::ZERO))
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        ProcessSpec::new(executable, absolute_root)
            .unwrap()
            .with_output_limit_bytes(0)
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
}

#[tokio::test]
async fn launch_failures_never_echo_the_executable_path() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("secret-path-canary-missing");
    let error = service()
        .output(
            ProcessSpec::new(program, root.path().canonicalize().unwrap()).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::NotFound);
    assert!(!error.to_string().contains("secret-path-canary"));
    assert!(!format!("{error:?}").contains("secret-path-canary"));
}

async fn wait_for_file(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("helper readiness marker was not created");
}

#[test]
#[allow(clippy::zombie_processes)]
fn helper_process() {
    let Ok(mode) = std::env::var("HEYCODE_EXEC_HELPER") else {
        return;
    };
    match mode.as_str() {
        "success" => {}
        "output" => {
            let path = if std::env::var_os("PATH").is_none() {
                "absent"
            } else {
                "present"
            };
            let visible = std::env::var("HEYCODE_EXEC_VISIBLE").unwrap();
            std::io::stdout()
                .write_all(format!("path={path} visible={visible}\n").as_bytes())
                .unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stderr().write_all(b"stderr-canary\n").unwrap();
            std::io::stderr().flush().unwrap();
            std::process::exit(7);
        }
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "flood" => {
            std::io::stdout()
                .write_all("flood-canary".repeat(1024).as_bytes())
                .unwrap();
            std::io::stdout().flush().unwrap();
        }
        "tree-parent" => {
            let marker = PathBuf::from(std::env::var_os("HEYCODE_EXEC_MARKER").unwrap());
            let ready = PathBuf::from(std::env::var_os("HEYCODE_EXEC_READY").unwrap());
            let _child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact".to_owned(),
                    super::test_name(module_path!(), "helper_process"),
                    "--nocapture".to_owned(),
                ])
                .env_clear()
                .env("HEYCODE_EXEC_HELPER", "tree-child")
                .env("HEYCODE_EXEC_MARKER", marker)
                .spawn()
                .unwrap();
            std::fs::write(ready, b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        "tree-child" => {
            let marker = PathBuf::from(std::env::var_os("HEYCODE_EXEC_MARKER").unwrap());
            std::thread::sleep(Duration::from_millis(650));
            std::fs::write(marker, b"survived").unwrap();
        }
        other => panic!("unknown helper mode {other}"),
    }
}

#[tokio::test]
async fn scoped_service_last_owner_drop_cancels_its_process_tree() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("scope-descendant-finished");
    let ready = root.path().join("scope-descendant-started");
    let scoped = SubprocessService::local_with_sandbox(
        SandboxService::new(SandboxMode::Off, root.path(), None).unwrap(),
    );
    let retained_scope = scoped.clone();
    let process = scoped
        .spawn(
            helper_spec(root.path(), "tree-parent")
                .with_environment([
                    (
                        OsString::from("HEYCODE_EXEC_HELPER"),
                        OsString::from("tree-parent"),
                    ),
                    (
                        OsString::from("HEYCODE_EXEC_MARKER"),
                        marker.clone().into_os_string(),
                    ),
                    (
                        OsString::from("HEYCODE_EXEC_READY"),
                        ready.clone().into_os_string(),
                    ),
                ])
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    drop(scoped);
    wait_for_file(&ready).await;
    drop(retained_scope);
    let error = tokio::time::timeout(Duration::from_secs(3), process.wait())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Cancelled);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !marker.exists(),
        "descendant survived scoped service shutdown"
    );
}

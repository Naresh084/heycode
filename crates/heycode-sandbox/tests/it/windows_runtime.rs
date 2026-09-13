//! Runtime evidence for Windows process-tree containment and sandbox truthfulness.
//!
//! Windows Job Objects contain and reap process descendants, but heycode does not
//! currently ship a Windows filesystem or network sandbox backend. The matrix
//! therefore proves both facts independently: process cleanup works, while
//! restrictive filesystem choices fail before publication.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(not(windows))]
#[test]
#[allow(clippy::print_stderr)]
fn windows_runtime_matrix_is_windows_only() {
    eprintln!("SKIP: Windows Job Object runtime evidence requires cfg(windows)");
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use heycode_exec::{
        ContainmentMechanism, FileReadScope, FileWriteScope, ManagedProcess, NetworkScope,
        ProcessErrorCode, ProcessSpec, SandboxMode, SandboxService, SubprocessService,
    };
    use tokio_util::sync::CancellationToken;

    const PROBE_ROLE: &str = "HEYCODE_WINDOWS_PROBE_ROLE";
    const PROBE_READY: &str = "HEYCODE_WINDOWS_PROBE_READY";
    const PROBE_RELEASE: &str = "HEYCODE_WINDOWS_PROBE_RELEASE";
    const PROBE_SURVIVAL: &str = "HEYCODE_WINDOWS_PROBE_SURVIVAL";
    const PARENT_ROLE: &str = "parent";
    const CHILD_ROLE: &str = "child";

    struct TreeProbe {
        process: ManagedProcess,
        caller: CancellationToken,
        release_marker: PathBuf,
        survival_marker: PathBuf,
    }

    #[test]
    fn restrictive_modes_are_truthfully_unavailable_without_a_windows_backend() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        let error = match heycode_sandbox::platform_default() {
            Ok(_) => panic!("Windows must not publish a sandbox backend without enforcement"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("no sandbox provider"));

        let service = SandboxService::new(SandboxMode::Off, &workspace, None).unwrap();
        let report = service.capability_report();
        assert_eq!(report.effective_mode, SandboxMode::Off);
        assert_eq!(report.active_backend, None);
        assert_eq!(report.available_backend, None);

        let full_access = report.choice(SandboxMode::Off).unwrap();
        assert!(full_access.selectable);
        assert_eq!(full_access.file_read, FileReadScope::Host);
        assert_eq!(full_access.file_write, FileWriteScope::Host);
        assert_eq!(full_access.network, NetworkScope::Host);

        for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
            let choice = report.choice(mode).unwrap();
            assert!(!choice.selectable);
            assert_eq!(choice.file_read, FileReadScope::Unspecified);
            assert_eq!(choice.file_write, FileWriteScope::Unspecified);
            assert_eq!(choice.network, NetworkScope::Unspecified);
            assert!(
                SandboxService::new(mode, &workspace, None).is_err(),
                "a restrictive Windows choice without a backend must fail closed"
            );
        }
    }

    #[tokio::test]
    async fn job_object_explicit_cancel_reaps_the_live_descendant() {
        let root = tempfile::tempdir().unwrap();
        let probe = spawn_tree(root.path()).await;
        let release_marker = probe.release_marker.clone();
        let survival_marker = probe.survival_marker.clone();
        probe.process.cancel().await.unwrap();
        assert_descendant_did_not_survive(&release_marker, &survival_marker).await;
    }

    #[tokio::test]
    async fn job_object_caller_cancellation_reaps_the_live_descendant() {
        let root = tempfile::tempdir().unwrap();
        let probe = spawn_tree(root.path()).await;
        let release_marker = probe.release_marker.clone();
        let survival_marker = probe.survival_marker.clone();
        probe.caller.cancel();
        let error = probe.process.wait().await.unwrap_err();
        assert_eq!(error.code(), ProcessErrorCode::Cancelled);
        assert_descendant_did_not_survive(&release_marker, &survival_marker).await;
    }

    #[tokio::test]
    async fn job_object_hard_kill_reaps_the_live_descendant() {
        let root = tempfile::tempdir().unwrap();
        let probe = spawn_tree(root.path()).await;
        let release_marker = probe.release_marker.clone();
        let survival_marker = probe.survival_marker.clone();
        let exit = probe.process.kill().await.unwrap();
        assert!(!exit.is_success());
        assert_descendant_did_not_survive(&release_marker, &survival_marker).await;
    }

    #[tokio::test]
    async fn closing_the_owned_job_handle_reaps_the_live_descendant() {
        let root = tempfile::tempdir().unwrap();
        let probe = spawn_tree(root.path()).await;
        let release_marker = probe.release_marker.clone();
        let survival_marker = probe.survival_marker.clone();
        drop(probe.process);
        assert_descendant_did_not_survive(&release_marker, &survival_marker).await;
    }

    async fn spawn_tree(root: &Path) -> TreeProbe {
        let root = root.canonicalize().unwrap();
        let ready = root.join("descendant-ready");
        let release_marker = root.join("descendant-release");
        let survival_marker = root.join("descendant-survived");
        let caller = CancellationToken::new();
        let service = SubprocessService::local();
        let containment = service.containment();
        assert_eq!(containment.mechanism(), &ContainmentMechanism::JobObject);
        assert!(containment.tree_kill_on_drop());
        assert!(containment.resists_session_escape());

        let spec = ProcessSpec::new(std::env::current_exe().unwrap(), &root)
            .unwrap()
            .with_args([
                OsString::from("--exact"),
                OsString::from(crate::it::test_name(module_path!(), "windows_tree_probe")),
                OsString::from("--nocapture"),
            ])
            .unwrap()
            .with_environment([
                (OsString::from(PROBE_ROLE), OsString::from(PARENT_ROLE)),
                (OsString::from(PROBE_READY), ready.clone().into_os_string()),
                (
                    OsString::from(PROBE_RELEASE),
                    release_marker.clone().into_os_string(),
                ),
                (
                    OsString::from(PROBE_SURVIVAL),
                    survival_marker.clone().into_os_string(),
                ),
            ])
            .unwrap()
            .with_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let process = service.spawn(spec, caller.clone()).await.unwrap();
        assert_eq!(
            process.containment().mechanism(),
            &ContainmentMechanism::JobObject
        );
        wait_for_file(&ready).await;
        TreeProbe {
            process,
            caller,
            release_marker,
            survival_marker,
        }
    }

    async fn wait_for_file(path: &Path) {
        for _ in 0..250 {
            if path.is_file() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("Windows descendant readiness marker was not created");
    }

    async fn assert_descendant_did_not_survive(release: &Path, survived: &Path) {
        // A wall-clock child delay is not ordering: a loaded runner can let the
        // descendant write before the parent regains CPU to cancel it. Release
        // the child only after the process owner reports terminal settlement;
        // then only a genuine escape can observe the marker and write back.
        std::fs::write(release, b"release").unwrap();
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(
            !survived.exists(),
            "a descendant escaped the Windows Job Object cleanup boundary"
        );
    }

    #[test]
    #[allow(clippy::zombie_processes)]
    fn windows_tree_probe() {
        let Some(role) = std::env::var_os(PROBE_ROLE) else {
            return;
        };
        let ready = PathBuf::from(std::env::var_os(PROBE_READY).unwrap());
        let release = PathBuf::from(std::env::var_os(PROBE_RELEASE).unwrap());
        let survival_marker = PathBuf::from(std::env::var_os(PROBE_SURVIVAL).unwrap());
        match role.to_string_lossy().as_ref() {
            PARENT_ROLE => {
                let _child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact".to_owned(),
                        crate::it::test_name(module_path!(), "windows_tree_probe"),
                        "--nocapture".to_owned(),
                    ])
                    .env_clear()
                    .env(PROBE_ROLE, CHILD_ROLE)
                    .env(PROBE_READY, &ready)
                    .env(PROBE_RELEASE, &release)
                    .env(PROBE_SURVIVAL, &survival_marker)
                    .spawn()
                    .unwrap();
                std::thread::sleep(Duration::from_secs(20));
            }
            CHILD_ROLE => {
                std::fs::write(ready, b"ready").unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(20);
                while !release.is_file() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                std::fs::write(survival_marker, b"escaped").unwrap();
            }
            other => panic!("unknown Windows process probe role {other}"),
        }
    }
}

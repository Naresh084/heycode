//! Live owner-scoped persistent terminal registry contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsString;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use heycode_exec::{
    MAX_TERMINAL_READ_BYTES, MAX_TERMINAL_SESSIONS_PER_OWNER, ProcessErrorCode, ProcessSpec,
    SERVICE_TERMINAL, SandboxMode, SandboxService, SubprocessService, TerminalId, TerminalOwner,
    TerminalService, TerminalSize, TerminalSpec, local_subprocess_plugin, sandbox_service_plugin,
    terminal_registry_plugin,
};

fn helper_spec(root: &Path, mode: &str) -> ProcessSpec {
    ProcessSpec::new(
        std::env::current_exe().unwrap(),
        root.canonicalize().unwrap(),
    )
    .unwrap()
    .with_args([
        OsString::from("--exact"),
        OsString::from(super::test_name(module_path!(), "terminal_helper_process")),
        OsString::from("--nocapture"),
    ])
    .unwrap()
    .with_environment([(
        OsString::from("HEYCODE_TERMINAL_HELPER"),
        OsString::from(mode),
    )])
    .unwrap()
    .with_timeout(Some(Duration::from_secs(30)))
    .unwrap()
    .with_interactive_stdio()
}

fn tree_spec(root: &Path, marker: &Path, ready: &Path) -> ProcessSpec {
    helper_spec(root, "tree-parent")
        .with_environment([
            (
                OsString::from("HEYCODE_TERMINAL_HELPER"),
                OsString::from("tree-parent"),
            ),
            (
                OsString::from("HEYCODE_TERMINAL_MARKER"),
                marker.to_path_buf().into_os_string(),
            ),
            (
                OsString::from("HEYCODE_TERMINAL_READY"),
                ready.to_path_buf().into_os_string(),
            ),
        ])
        .unwrap()
}

fn service() -> TerminalService {
    TerminalService::new(SubprocessService::local())
}

fn owner(name: &str) -> TerminalOwner {
    TerminalOwner::new(name).unwrap()
}

/// Drain until `needle` appears or the deadline elapses. Returns everything read.
async fn read_until(
    service: &TerminalService,
    owner: &TerminalOwner,
    id: &TerminalId,
    needle: &str,
) -> String {
    let mut seen = Vec::new();
    for _ in 0..300 {
        let read = service
            .read(owner, id, MAX_TERMINAL_READ_BYTES)
            .await
            .unwrap();
        seen.extend_from_slice(read.bytes());
        if String::from_utf8_lossy(&seen).contains(needle) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    String::from_utf8_lossy(&seen).into_owned()
}

async fn wait_for_file(path: &Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("helper readiness marker was not created");
}

#[tokio::test]
async fn sessions_are_owner_scoped_across_write_read_resize_kill_and_list() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-mine");
    let other = owner("owner-other");

    let id = service
        .open(
            &mine,
            TerminalSpec::new(helper_spec(root.path(), "echo")).unwrap(),
        )
        .await
        .unwrap();

    service
        .write(&mine, &id, b"hello-terminal\n")
        .await
        .unwrap();
    let seen = read_until(&service, &mine, &id, "ECHO:hello-terminal").await;
    assert!(seen.contains("ECHO:hello-terminal"), "{seen:?}");

    // Every operation is scoped to the opening owner, and a foreign owner cannot
    // distinguish "not yours" from "does not exist".
    for code in [
        service.write(&other, &id, b"x\n").await.unwrap_err().code(),
        service.read(&other, &id, 128).await.unwrap_err().code(),
        service
            .resize(&other, &id, TerminalSize::new(100, 30).unwrap())
            .await
            .unwrap_err()
            .code(),
        service.kill(&other, &id).await.unwrap_err().code(),
    ] {
        assert_eq!(code, ProcessErrorCode::UnknownTerminal);
    }
    assert!(service.list(&other).await.is_empty());
    let listed = service.list(&mine).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id(), &id);

    let exit = service.kill(&mine, &id).await.unwrap();
    assert!(!exit.is_success());
    assert!(service.list(&mine).await.is_empty());
    assert_eq!(
        service.read(&mine, &id, 128).await.unwrap_err().code(),
        ProcessErrorCode::UnknownTerminal
    );
}

#[cfg(unix)]
#[tokio::test]
async fn resize_changes_the_live_terminal_geometry_the_child_observes() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-resize");
    let spec = ProcessSpec::new("/bin/sh", root.path().canonicalize().unwrap())
        .unwrap()
        .with_timeout(Some(Duration::from_secs(30)))
        .unwrap()
        .with_interactive_stdio();
    let id = service
        .open(
            &mine,
            TerminalSpec::new(spec)
                .unwrap()
                .with_size(TerminalSize::new(80, 24).unwrap()),
        )
        .await
        .unwrap();

    service
        .write(&mine, &id, b"stty size | tr ' ' -\n")
        .await
        .unwrap();
    let before = read_until(&service, &mine, &id, "24-80").await;
    assert!(before.contains("24-80"), "initial geometry: {before:?}");

    service
        .resize(&mine, &id, TerminalSize::new(120, 40).unwrap())
        .await
        .unwrap();
    service
        .write(&mine, &id, b"stty size | tr ' ' -\n")
        .await
        .unwrap();
    let after = read_until(&service, &mine, &id, "40-120").await;
    assert!(after.contains("40-120"), "resized geometry: {after:?}");

    service.kill(&mine, &id).await.unwrap();
}

#[tokio::test]
async fn retained_output_is_bounded_drops_oldest_and_reports_the_loss() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-flood");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(helper_spec(root.path(), "flood"))
                .unwrap()
                .with_retained_bytes(4096)
                .unwrap(),
        )
        .await
        .unwrap();

    // Let the child outrun the bound without any reader draining it.
    let mut dropped = 0;
    for _ in 0..300 {
        let status = service
            .list(&mine)
            .await
            .into_iter()
            .find(|status| status.id() == &id)
            .unwrap();
        assert!(
            status.pending_bytes() <= 4096,
            "retention bound exceeded: {}",
            status.pending_bytes()
        );
        dropped = status.dropped_bytes();
        if dropped > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(dropped > 0, "unbounded terminal output was retained");

    // The newest bytes survive the bound.
    let tail = read_until(&service, &mine, &id, "FLOOD-TAIL").await;
    assert!(tail.contains("FLOOD-TAIL"), "newest output was lost");

    service.kill(&mine, &id).await.unwrap();
}

#[tokio::test]
async fn one_read_is_bounded_below_the_retention_bound() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-read-bound");
    let retained = MAX_TERMINAL_READ_BYTES * 4;
    let id = service
        .open(
            &mine,
            TerminalSpec::new(helper_spec(root.path(), "flood"))
                .unwrap()
                .with_retained_bytes(retained)
                .unwrap(),
        )
        .await
        .unwrap();

    // Retain deliberately more than one read can return.
    let mut pending = 0;
    for _ in 0..300 {
        pending = service
            .list(&mine)
            .await
            .into_iter()
            .find(|status| status.id() == &id)
            .unwrap()
            .pending_bytes();
        assert!(pending <= retained, "retention bound exceeded: {pending}");
        if pending > MAX_TERMINAL_READ_BYTES {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        pending > MAX_TERMINAL_READ_BYTES,
        "not enough retained output"
    );

    let read = service.read(&mine, &id, usize::MAX).await.unwrap();
    assert_eq!(read.bytes().len(), MAX_TERMINAL_READ_BYTES);
    let left = service
        .list(&mine)
        .await
        .into_iter()
        .find(|status| status.id() == &id)
        .unwrap()
        .pending_bytes();
    assert!(left > 0, "a bounded read must not discard the remainder");
    assert_eq!(
        service.read(&mine, &id, 0).await.unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );

    service.kill(&mine, &id).await.unwrap();
}

#[tokio::test]
async fn a_failed_launch_returns_its_reserved_session_slot() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-reservation");
    let missing = ProcessSpec::new(
        root.path().canonicalize().unwrap().join("absent-program"),
        root.path().canonicalize().unwrap(),
    )
    .unwrap()
    .with_interactive_stdio();

    // More failures than the owner's whole budget: a leaked reservation would
    // exhaust it and the last open would report capacity, not a launch failure.
    for _ in 0..(MAX_TERMINAL_SESSIONS_PER_OWNER + 2) {
        let error = service
            .open(&mine, TerminalSpec::new(missing.clone()).unwrap())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ProcessErrorCode::NotFound);
    }
    assert!(service.list(&mine).await.is_empty());

    let id = service
        .open(
            &mine,
            TerminalSpec::new(helper_spec(root.path(), "idle")).unwrap(),
        )
        .await
        .unwrap();
    service.kill(&mine, &id).await.unwrap();
}

#[tokio::test]
async fn killing_a_terminal_reaps_its_descendant_tree() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("kill-descendant-finished");
    let ready = root.path().join("kill-descendant-started");
    let service = service();
    let mine = owner("owner-kill");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(tree_spec(root.path(), &marker, &ready)).unwrap(),
        )
        .await
        .unwrap();

    wait_for_file(&ready).await;
    let exit = service.kill(&mine, &id).await.unwrap();
    assert!(!exit.is_success());
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!marker.exists(), "descendant survived terminal kill");
}

#[tokio::test]
async fn context_shutdown_reaps_live_terminals_and_rejects_new_sessions() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("shutdown-descendant-finished");
    let ready = root.path().join("shutdown-descendant-started");
    let plugins = vec![
        sandbox_service_plugin(
            SandboxService::new(SandboxMode::Off, root.path().canonicalize().unwrap(), None)
                .unwrap(),
        ),
        local_subprocess_plugin(),
        terminal_registry_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    assert_eq!(
        context.owner_of(SERVICE_TERMINAL),
        Some("terminal-registry")
    );
    let service = context
        .get::<TerminalService>(SERVICE_TERMINAL)
        .expect("terminal service");
    let mine = owner("owner-shutdown");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(tree_spec(root.path(), &marker, &ready)).unwrap(),
        )
        .await
        .unwrap();
    wait_for_file(&ready).await;

    context.shutdown();

    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!marker.exists(), "descendant survived context shutdown");
    assert_eq!(
        service.read(&mine, &id, 128).await.unwrap_err().code(),
        ProcessErrorCode::ServiceStopped
    );
    assert_eq!(
        service
            .open(
                &mine,
                TerminalSpec::new(helper_spec(root.path(), "echo")).unwrap()
            )
            .await
            .unwrap_err()
            .code(),
        ProcessErrorCode::ServiceStopped
    );
}

#[tokio::test]
async fn closing_the_registry_reaps_live_terminals_and_refuses_further_work() {
    // The registry is built on a standalone subprocess service, so no backend
    // shutdown token exists and the service handle stays alive: the only thing
    // that can reap this tree is the disposer body itself.
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("close-descendant-finished");
    let ready = root.path().join("close-descendant-started");
    let service = service();
    let mine = owner("owner-close");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(tree_spec(root.path(), &marker, &ready)).unwrap(),
        )
        .await
        .unwrap();
    wait_for_file(&ready).await;

    service.close();

    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!marker.exists(), "descendant survived registry close");
    assert_eq!(
        service.read(&mine, &id, 128).await.unwrap_err().code(),
        ProcessErrorCode::ServiceStopped
    );
    assert_eq!(
        service.kill(&mine, &id).await.unwrap_err().code(),
        ProcessErrorCode::ServiceStopped
    );
    assert_eq!(
        service
            .open(
                &mine,
                TerminalSpec::new(helper_spec(root.path(), "idle")).unwrap()
            )
            .await
            .unwrap_err()
            .code(),
        ProcessErrorCode::ServiceStopped
    );
    assert!(service.list(&mine).await.is_empty());
    service.close();
}

#[tokio::test]
async fn concurrent_opens_cannot_exceed_the_owner_session_bound() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-race");
    let attempts = MAX_TERMINAL_SESSIONS_PER_OWNER + 4;
    let opens = (0..attempts).map(|_| {
        let service = service.clone();
        let mine = mine.clone();
        let spec = TerminalSpec::new(helper_spec(root.path(), "idle")).unwrap();
        async move { service.open(&mine, spec).await }
    });
    let results = futures::future::join_all(opens).await;

    let admitted = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(admitted, MAX_TERMINAL_SESSIONS_PER_OWNER);
    for result in &results {
        if let Err(error) = result {
            assert_eq!(error.code(), ProcessErrorCode::TerminalCapacity);
        }
    }
    assert_eq!(
        service.list(&mine).await.len(),
        MAX_TERMINAL_SESSIONS_PER_OWNER
    );
}

#[tokio::test]
async fn dropping_the_last_service_handle_reaps_terminal_descendants() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("drop-descendant-finished");
    let ready = root.path().join("drop-descendant-started");
    let service = service();
    let mine = owner("owner-drop");
    service
        .open(
            &mine,
            TerminalSpec::new(tree_spec(root.path(), &marker, &ready)).unwrap(),
        )
        .await
        .unwrap();

    wait_for_file(&ready).await;
    drop(service);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!marker.exists(), "descendant survived registry drop");
}

#[tokio::test]
async fn sessions_per_owner_are_bounded() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-capacity");
    for _ in 0..MAX_TERMINAL_SESSIONS_PER_OWNER {
        service
            .open(
                &mine,
                TerminalSpec::new(helper_spec(root.path(), "idle")).unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        service
            .open(
                &mine,
                TerminalSpec::new(helper_spec(root.path(), "idle")).unwrap()
            )
            .await
            .unwrap_err()
            .code(),
        ProcessErrorCode::TerminalCapacity
    );
    // A different owner still has its own budget.
    service
        .open(
            &owner("owner-capacity-other"),
            TerminalSpec::new(helper_spec(root.path(), "idle")).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn writes_and_resizes_after_the_child_exits_report_the_terminal_ended() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-exited");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(helper_spec(root.path(), "exit")).unwrap(),
        )
        .await
        .unwrap();

    let mut ended = false;
    for _ in 0..300 {
        if service
            .read(&mine, &id, MAX_TERMINAL_READ_BYTES)
            .await
            .unwrap()
            .ended()
        {
            ended = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ended, "terminal output never reported EOF");

    assert_eq!(
        service.write(&mine, &id, b"x\n").await.unwrap_err().code(),
        ProcessErrorCode::TerminalExited
    );
    assert_eq!(
        service
            .resize(&mine, &id, TerminalSize::new(100, 30).unwrap())
            .await
            .unwrap_err()
            .code(),
        ProcessErrorCode::TerminalExited
    );
    // The session is still owned and still reapable.
    service.kill(&mine, &id).await.unwrap();
}

#[tokio::test]
async fn wait_retires_a_naturally_settled_terminal_and_reports_its_exit() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-wait");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(helper_spec(root.path(), "exit-seven")).unwrap(),
        )
        .await
        .unwrap();

    let exit = service
        .wait(&mine, &id, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(exit, heycode_exec::ProcessExit::Exited { code: 7 });
    assert!(service.list(&mine).await.is_empty());
    assert_eq!(
        service
            .wait(&mine, &id, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        ProcessErrorCode::UnknownTerminal
    );
}

#[tokio::test]
async fn cancelling_wait_reaps_the_terminal_tree_before_returning() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("wait-descendant-finished");
    let ready = root.path().join("wait-descendant-started");
    let service = service();
    let mine = owner("owner-wait-cancel");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(tree_spec(root.path(), &marker, &ready)).unwrap(),
        )
        .await
        .unwrap();
    wait_for_file(&ready).await;

    let cancellation = tokio_util::sync::CancellationToken::new();
    let cancel = cancellation.clone();
    let waiter = tokio::spawn({
        let service = service.clone();
        let mine = mine.clone();
        let id = id.clone();
        async move { service.wait(&mine, &id, cancellation).await }
    });
    cancel.cancel();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Cancelled);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !marker.exists(),
        "descendant survived cancelled terminal wait"
    );
    assert!(service.list(&mine).await.is_empty());
}

#[tokio::test]
async fn hard_kill_joins_an_existing_waiter_instead_of_stranding_its_tree() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("joined-wait-descendant-finished");
    let ready = root.path().join("joined-wait-descendant-started");
    let service = service();
    let mine = owner("owner-wait-kill");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(tree_spec(root.path(), &marker, &ready)).unwrap(),
        )
        .await
        .unwrap();
    wait_for_file(&ready).await;
    let waiter = tokio::spawn({
        let service = service.clone();
        let mine = mine.clone();
        let id = id.clone();
        async move {
            service
                .wait(&mine, &id, tokio_util::sync::CancellationToken::new())
                .await
        }
    });
    tokio::task::yield_now().await;

    let killed = service.kill(&mine, &id).await;
    assert!(
        killed.is_ok()
            || killed
                .as_ref()
                .is_err_and(|error| error.code() == ProcessErrorCode::Cancelled)
    );
    let waited = waiter.await.unwrap();
    assert!(
        waited.is_ok()
            || waited
                .as_ref()
                .is_err_and(|error| error.code() == ProcessErrorCode::Cancelled)
    );
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !marker.exists(),
        "descendant survived kill joined to waiter"
    );
    assert!(service.list(&mine).await.is_empty());
}

#[tokio::test]
async fn terminal_boundaries_reject_invalid_values_and_diagnostics_stay_body_free() {
    let root = tempfile::tempdir().unwrap();
    let absolute_root = root.path().canonicalize().unwrap();
    let executable = std::env::current_exe().unwrap();

    assert_eq!(
        TerminalSize::new(0, 24).unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        TerminalSize::new(80, 0).unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        TerminalOwner::new("").unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        TerminalOwner::new("owner\u{0}canary").unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        TerminalOwner::new("owner\ncanary").unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        TerminalId::parse("not-a-terminal-id").unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );

    // A terminal is an interactive process by construction.
    let non_interactive = ProcessSpec::new(&executable, &absolute_root).unwrap();
    assert_eq!(
        TerminalSpec::new(non_interactive).unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );

    let interactive = ProcessSpec::new(&executable, &absolute_root)
        .unwrap()
        .with_environment([(
            OsString::from("HEYCODE_TERMINAL_SECRET"),
            OsString::from("terminal-env-canary"),
        )])
        .unwrap()
        .with_interactive_stdio();
    let spec = TerminalSpec::new(interactive).unwrap();
    assert_eq!(
        spec.clone().with_retained_bytes(0).unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        spec.clone()
            .with_retained_bytes(usize::MAX)
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );

    let debug = format!("{spec:?}");
    assert!(!debug.contains("terminal-env-canary"), "{debug}");
    assert!(
        !debug.contains(absolute_root.to_string_lossy().as_ref()),
        "{debug}"
    );

    for code in [
        ProcessErrorCode::UnknownTerminal,
        ProcessErrorCode::TerminalExited,
        ProcessErrorCode::TerminalCapacity,
    ] {
        let error = heycode_exec::ProcessError::new(code);
        assert_eq!(error.code(), code);
        assert!(!error.to_string().is_empty());
    }
}

#[tokio::test]
async fn reads_expose_the_byte_exact_terminal_stream_without_leaking_it_into_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let service = service();
    let mine = owner("owner-bytes");
    let id = service
        .open(
            &mine,
            TerminalSpec::new(helper_spec(root.path(), "escapes")).unwrap(),
        )
        .await
        .unwrap();

    let mut seen = Vec::new();
    for _ in 0..300 {
        let read = service
            .read(&mine, &id, MAX_TERMINAL_READ_BYTES)
            .await
            .unwrap();
        let debug = format!("{read:?}");
        assert!(!debug.contains("ESC-CANARY"), "{debug}");
        seen.extend_from_slice(read.bytes());
        if seen.windows(10).any(|window| window == b"ESC-CANARY") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The raw plane keeps the VT escape and CR framing a line decoder would eat.
    assert!(
        seen.windows(4).any(|window| window == b"\x1b[31"),
        "escape bytes were not preserved: {:?}",
        String::from_utf8_lossy(&seen)
    );
    assert!(seen.contains(&b'\r'), "carriage returns were not preserved");

    service.kill(&mine, &id).await.unwrap();
}

#[test]
#[allow(clippy::zombie_processes)]
fn terminal_helper_process() {
    let Ok(mode) = std::env::var("HEYCODE_TERMINAL_HELPER") else {
        return;
    };
    match mode.as_str() {
        "idle" => std::thread::sleep(Duration::from_secs(30)),
        "exit" => {}
        "exit-seven" => std::process::exit(7),
        "echo" => {
            let stdin = std::io::stdin();
            let mut line = String::new();
            while stdin.lock().read_line(&mut line).unwrap_or(0) > 0 {
                let text = line.trim_end_matches(['\r', '\n']).to_owned();
                std::io::stdout()
                    .write_all(format!("ECHO:{text}\n").as_bytes())
                    .unwrap();
                std::io::stdout().flush().unwrap();
                line.clear();
            }
        }
        "flood" => {
            let block = "flood-canary".repeat(64);
            for _ in 0..2048 {
                std::io::stdout().write_all(block.as_bytes()).unwrap();
            }
            std::io::stdout().write_all(b"\nFLOOD-TAIL\n").unwrap();
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        "escapes" => {
            std::io::stdout()
                .write_all(b"\x1b[31mESC-CANARY\x1b[0m\rredraw\n")
                .unwrap();
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        "tree-parent" => {
            let marker = PathBuf::from(std::env::var_os("HEYCODE_TERMINAL_MARKER").unwrap());
            let ready = PathBuf::from(std::env::var_os("HEYCODE_TERMINAL_READY").unwrap());
            let _child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact".to_owned(),
                    super::test_name(module_path!(), "terminal_helper_process"),
                    "--nocapture".to_owned(),
                ])
                .env_clear()
                .env("HEYCODE_TERMINAL_HELPER", "tree-child")
                .env("HEYCODE_TERMINAL_MARKER", marker)
                .spawn()
                .unwrap();
            std::fs::write(ready, b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        "tree-child" => {
            let marker = PathBuf::from(std::env::var_os("HEYCODE_TERMINAL_MARKER").unwrap());
            std::thread::sleep(Duration::from_millis(650));
            std::fs::write(marker, b"survived").unwrap();
        }
        other => panic!("unknown terminal helper mode {other}"),
    }
}

//! Raw interactive stdout framing, bounds, cancellation and teardown contracts.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use heycode_exec::{
    MAX_PROCESS_OUTPUT_CHUNK_BYTES, ProcessErrorCode, ProcessExit, ProcessOutputChunk, ProcessSpec,
    SERVICE_SUBPROCESS, SandboxMode, SandboxService, SubprocessService, local_subprocess_plugin,
    sandbox_service_plugin,
};
use tokio_util::sync::CancellationToken;

const RAW_START: &[u8] = b"<dshx-raw-start>";
const RAW_END: &[u8] = b"<dshx-raw-end>";

fn raw_spec(root: &Path, mode: &str) -> ProcessSpec {
    ProcessSpec::new(
        std::env::current_exe().unwrap(),
        root.canonicalize().unwrap(),
    )
    .unwrap()
    .with_args([
        OsString::from("--exact"),
        OsString::from(super::test_name(module_path!(), "raw_helper_process")),
        OsString::from("--nocapture"),
    ])
    .unwrap()
    .with_environment([(
        OsString::from("HEYCODE_EXEC_RAW_HELPER"),
        OsString::from(mode),
    )])
    .unwrap()
    .with_timeout(Some(Duration::from_secs(8)))
    .unwrap()
    .with_output_limit_bytes(64)
    .unwrap()
    .with_interactive_stdio()
}

#[tokio::test]
async fn invalid_utf8_and_split_frames_are_preserved_before_string_decoding() {
    let root = tempfile::tempdir().unwrap();
    let raw = SubprocessService::local()
        .spawn_interactive_raw(
            raw_spec(root.path(), "invalid-and-split"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let (process, input, mut output) = raw.into_raw_parts();
    input.finish().await.unwrap();

    let mut bytes = Vec::new();
    let mut chunks = 0usize;
    while let ProcessOutputChunk::Data(chunk) =
        output.read_chunk(CancellationToken::new()).await.unwrap()
    {
        assert!(!chunk.is_empty());
        assert!(chunk.len() <= MAX_PROCESS_OUTPUT_CHUNK_BYTES);
        chunks += 1;
        bytes.extend(chunk);
    }
    let payload = between_markers(&bytes);
    assert_eq!(&payload[..3], &[0xff, 0xfe, b'\n']);
    let expected_tail = (0..(MAX_PROCESS_OUTPUT_CHUNK_BYTES * 2 + 17))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    assert_eq!(&payload[3..], expected_tail);
    assert!(chunks >= 3, "fixed chunk bound did not split large frame");
    assert_eq!(
        output.read_chunk(CancellationToken::new()).await.unwrap(),
        ProcessOutputChunk::Eof
    );
    assert_eq!(
        process.wait().await.unwrap(),
        ProcessExit::Exited { code: 0 }
    );
}

#[tokio::test]
async fn large_cumulative_output_has_no_legacy_capture_ceiling_or_drop() {
    let root = tempfile::tempdir().unwrap();
    let raw = SubprocessService::local()
        .spawn_interactive_raw(
            raw_spec(root.path(), "large-cumulative"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let (process, input, mut output) = raw.into_raw_parts();
    input.finish().await.unwrap();

    let mut output_bytes = Vec::new();
    while let ProcessOutputChunk::Data(chunk) =
        output.read_chunk(CancellationToken::new()).await.unwrap()
    {
        assert!(chunk.len() <= MAX_PROCESS_OUTPUT_CHUNK_BYTES);
        output_bytes.extend(chunk);
    }
    assert_eq!(between_markers(&output_bytes).len(), 4 * 1024 * 1024);
    assert_eq!(
        process.wait().await.unwrap(),
        ProcessExit::Exited { code: 0 }
    );
}

#[tokio::test]
async fn cancelling_one_read_does_not_consume_bytes_or_cancel_the_process() {
    let root = tempfile::tempdir().unwrap();
    let raw = SubprocessService::local()
        .spawn_interactive_raw(raw_spec(root.path(), "delayed"), CancellationToken::new())
        .await
        .unwrap();
    let (process, input, mut output) = raw.into_raw_parts();
    input.finish().await.unwrap();

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = output.read_chunk(cancellation).await.unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Cancelled);
    let mut bytes = Vec::new();
    while let ProcessOutputChunk::Data(chunk) =
        output.read_chunk(CancellationToken::new()).await.unwrap()
    {
        bytes.extend(chunk);
    }
    assert!(
        bytes
            .windows(b"after-cancel".len())
            .any(|window| window == b"after-cancel")
    );
    assert_eq!(
        process.wait().await.unwrap(),
        ProcessExit::Exited { code: 0 }
    );
}

#[tokio::test]
async fn stalled_raw_consumer_does_not_defeat_quiescent_tree_teardown() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("raw-tree-ready");
    let survived = root.path().join("raw-tree-survived");
    let spec = raw_spec(root.path(), "blocked-tree")
        .with_environment([
            (
                OsString::from("HEYCODE_EXEC_RAW_HELPER"),
                OsString::from("blocked-tree"),
            ),
            (
                OsString::from("HEYCODE_EXEC_RAW_READY"),
                ready.clone().into_os_string(),
            ),
            (
                OsString::from("HEYCODE_EXEC_RAW_SURVIVED"),
                survived.clone().into_os_string(),
            ),
        ])
        .unwrap();
    let raw = SubprocessService::local()
        .spawn_interactive_raw(spec, CancellationToken::new())
        .await
        .unwrap();
    let (process, input, _output) = raw.into_raw_parts();
    drop(input);
    wait_for_file(&ready).await;
    tokio::time::timeout(Duration::from_secs(3), process.cancel())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !survived.exists(),
        "raw-output backpressure escaped tree teardown"
    );
}

#[tokio::test]
async fn plugin_shutdown_closes_raw_backpressure_and_reaps_the_tree() {
    let root = tempfile::tempdir().unwrap();
    let canonical_root = root.path().canonicalize().unwrap();
    let ready = root.path().join("raw-plugin-ready");
    let survived = root.path().join("raw-plugin-survived");
    let plugins = vec![
        sandbox_service_plugin(
            SandboxService::new(SandboxMode::Off, &canonical_root, None).unwrap(),
        ),
        local_subprocess_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let service = context
        .get::<SubprocessService>(SERVICE_SUBPROCESS)
        .unwrap();
    let spec = raw_spec(root.path(), "blocked-tree")
        .with_environment([
            (
                OsString::from("HEYCODE_EXEC_RAW_HELPER"),
                OsString::from("blocked-tree"),
            ),
            (
                OsString::from("HEYCODE_EXEC_RAW_READY"),
                ready.clone().into_os_string(),
            ),
            (
                OsString::from("HEYCODE_EXEC_RAW_SURVIVED"),
                survived.clone().into_os_string(),
            ),
        ])
        .unwrap();
    let raw = service
        .spawn_interactive_raw(spec, CancellationToken::new())
        .await
        .unwrap();
    let (process, input, _output) = raw.into_raw_parts();
    drop(input);
    wait_for_file(&ready).await;
    context.shutdown();
    let error = tokio::time::timeout(Duration::from_secs(3), process.wait())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Cancelled);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !survived.exists(),
        "plugin shutdown left raw descendant alive"
    );
}

#[tokio::test]
async fn raw_spawn_rejects_noninteractive_specs_and_precancellation() {
    let root = tempfile::tempdir().unwrap();
    let noninteractive = ProcessSpec::new(
        std::env::current_exe().unwrap(),
        root.path().canonicalize().unwrap(),
    )
    .unwrap();
    assert_eq!(
        SubprocessService::local()
            .spawn_interactive_raw(noninteractive, CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        SubprocessService::local()
            .spawn_interactive_raw(raw_spec(root.path(), "delayed"), cancellation)
            .await
            .unwrap_err()
            .code(),
        ProcessErrorCode::Cancelled
    );
}

async fn wait_for_file(path: &Path) {
    for _ in 0..200 {
        if path.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("raw helper readiness marker was not created");
}

fn between_markers(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .windows(RAW_START.len())
        .position(|window| window == RAW_START)
        .map(|index| index + RAW_START.len())
        .expect("raw start marker");
    let end = bytes[start..]
        .windows(RAW_END.len())
        .position(|window| window == RAW_END)
        .map(|index| start + index)
        .expect("raw end marker");
    &bytes[start..end]
}

#[test]
#[allow(clippy::zombie_processes)]
fn raw_helper_process() {
    let Ok(mode) = std::env::var("HEYCODE_EXEC_RAW_HELPER") else {
        return;
    };
    match mode.as_str() {
        "invalid-and-split" => {
            let mut stdout = std::io::stdout();
            stdout.write_all(RAW_START).unwrap();
            stdout.write_all(&[0xff, 0xfe, b'\n']).unwrap();
            let payload = (0..(MAX_PROCESS_OUTPUT_CHUNK_BYTES * 2 + 17))
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>();
            stdout.write_all(&payload).unwrap();
            stdout.write_all(RAW_END).unwrap();
            stdout.flush().unwrap();
        }
        "large-cumulative" => {
            let block = vec![b'x'; 1024];
            let mut stdout = std::io::stdout();
            stdout.write_all(RAW_START).unwrap();
            for _ in 0..4096 {
                stdout.write_all(&block).unwrap();
            }
            stdout.write_all(RAW_END).unwrap();
            stdout.flush().unwrap();
        }
        "delayed" => {
            std::thread::sleep(Duration::from_millis(100));
            std::io::stdout().write_all(b"after-cancel").unwrap();
            std::io::stdout().flush().unwrap();
        }
        "blocked-tree" => {
            let ready = PathBuf::from(std::env::var_os("HEYCODE_EXEC_RAW_READY").unwrap());
            let survived = PathBuf::from(std::env::var_os("HEYCODE_EXEC_RAW_SURVIVED").unwrap());
            let _child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact".to_owned(),
                    super::test_name(module_path!(), "raw_helper_process"),
                    "--nocapture".to_owned(),
                ])
                .env_clear()
                .env("HEYCODE_EXEC_RAW_HELPER", "raw-tree-child")
                .env("HEYCODE_EXEC_RAW_SURVIVED", &survived)
                .spawn()
                .unwrap();
            std::fs::write(ready, b"ready").unwrap();
            let block = vec![b'x'; MAX_PROCESS_OUTPUT_CHUNK_BYTES];
            loop {
                std::io::stdout().write_all(&block).unwrap();
                std::io::stdout().flush().unwrap();
            }
        }
        "raw-tree-child" => {
            let survived = PathBuf::from(std::env::var_os("HEYCODE_EXEC_RAW_SURVIVED").unwrap());
            std::thread::sleep(Duration::from_millis(650));
            std::fs::write(survived, b"survived").unwrap();
        }
        other => panic!("unknown raw helper mode {other}"),
    }
}

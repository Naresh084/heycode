//! Exact executable-image and explicit process-authority contracts.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::Write as _;

#[cfg(unix)]
use heycode_exec::{
    ExactExecutable, ProcessAuthority, ProcessErrorCode, ProcessOutputChunk, ProcessSpec,
    SubprocessService,
};
#[cfg(unix)]
use sha2::{Digest as _, Sha256};
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

const HELPER_ENV: &str = "HEYCODE_EXACT_PROCESS_HELPER";

#[cfg(unix)]
#[tokio::test]
async fn exact_launch_uses_admitted_bytes_and_an_explicit_empty_environment() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("exact-image-ran");
    let current = std::env::current_exe().unwrap();
    let admitted = fs::read(&current).unwrap();
    let digest = render_sha256(&admitted);
    let exact = ExactExecutable::new(admitted, &digest).unwrap();

    let replaced = root.path().join("replaced-program");
    fs::write(&replaced, b"not the admitted executable").unwrap();
    let spec = ProcessSpec::new(&replaced, root.path().canonicalize().unwrap())
        .unwrap()
        .with_args([
            OsString::from("--exact"),
            OsString::from(super::test_name(module_path!(), "exact_process_helper")),
            OsString::from("--nocapture"),
        ])
        .unwrap()
        .with_environment([(
            OsString::from(HELPER_ENV),
            OsString::from("must-not-arrive"),
        )])
        .unwrap()
        .with_interactive_stdio();

    let raw = SubprocessService::local()
        .spawn_exact_interactive_raw(
            exact,
            spec,
            ProcessAuthority::new(true, true, true, true),
            Vec::<(OsString, OsString)>::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let (process, input, mut output) = raw.into_raw_parts();
    input.finish().await.unwrap();
    while let ProcessOutputChunk::Data(chunk) =
        output.read_chunk(CancellationToken::new()).await.unwrap()
    {
        assert!(!chunk.is_empty());
    }
    assert!(process.wait().await.unwrap().is_success());
    assert_eq!(fs::read(marker).unwrap(), b"environment-was-empty");
}

#[cfg(unix)]
#[tokio::test]
async fn authority_that_cannot_be_enforced_fails_before_spawn() {
    let root = tempfile::tempdir().unwrap();
    let current = std::env::current_exe().unwrap();
    let admitted = fs::read(&current).unwrap();
    let exact = ExactExecutable::new(admitted.clone(), &render_sha256(&admitted)).unwrap();
    let spec = ProcessSpec::new(&current, root.path().canonicalize().unwrap())
        .unwrap()
        .with_interactive_stdio();

    let error = SubprocessService::local()
        .spawn_exact_interactive_raw(
            exact,
            spec,
            ProcessAuthority::deny_all(),
            Vec::<(OsString, OsString)>::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Sandbox);
}

#[cfg(unix)]
#[test]
fn executable_digest_mismatch_fails_before_any_staging() {
    let error = ExactExecutable::new(b"ELF bytes".to_vec(), &format!("sha256:{}", "0".repeat(64)))
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::InvalidSpec);
}

#[test]
fn exact_process_helper() {
    let exact_name = super::test_name(module_path!(), "exact_process_helper");
    if !std::env::args().any(|argument| argument == exact_name) {
        return;
    }
    assert!(std::env::var_os(HELPER_ENV).is_none());
    std::fs::write("exact-image-ran", b"environment-was-empty").unwrap();
    std::io::stdout().write_all(b"ok\n").unwrap();
}

#[cfg(unix)]
fn render_sha256(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut rendered = String::from("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(rendered, "{byte:02x}").unwrap();
    }
    rendered
}

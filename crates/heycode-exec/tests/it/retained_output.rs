//! E06 owner-only retained-output spill service.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use heycode_core::compose;
use heycode_exec::{
    MAX_RETAINED_OUTPUT_READ_BYTES, RetainedOutputConfig, RetainedOutputErrorCode,
    RetainedOutputOwner, RetainedOutputService, SERVICE_RETAINED_OUTPUT, retained_output_plugin,
};
use tokio_util::sync::CancellationToken;

fn config(root: PathBuf) -> RetainedOutputConfig {
    RetainedOutputConfig::new(root).unwrap()
}

fn owner(value: &str) -> RetainedOutputOwner {
    RetainedOutputOwner::new(value).unwrap()
}

fn regular_files(root: &Path) -> Vec<PathBuf> {
    fn visit(path: &Path, output: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(&path, output);
            } else if metadata.is_file() {
                output.push(path);
            }
        }
    }
    let mut output = Vec::new();
    visit(root, &mut output);
    output
}

#[cfg(unix)]
fn assert_owner_only_tree(root: &Path) {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    fn visit(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
                visit(&path);
            } else {
                assert!(metadata.is_file());
                assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
                assert_eq!(metadata.nlink(), 1);
            }
        }
    }

    let metadata = fs::symlink_metadata(root).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    visit(root);
}

#[cfg(unix)]
#[test]
fn content_identity_is_atomic_deduplicated_and_owner_only() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("retained");
    let service = RetainedOutputService::open(config(root.clone())).unwrap();
    let mine = owner("session-a");
    let theirs = owner("session-b");
    let first = service
        .retain(
            &mine,
            b"same complete output",
            256,
            CancellationToken::new(),
        )
        .unwrap();
    let second = service
        .retain(
            &mine,
            b"same complete output",
            256,
            CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(first.id(), second.id());
    assert_eq!(first.total_bytes(), b"same complete output".len() as u64);
    assert_eq!(regular_files(&root).len(), 1);
    assert_owner_only_tree(&root);

    assert_eq!(
        service
            .read(&theirs, first.id(), 0, 64, CancellationToken::new(),)
            .unwrap_err()
            .code(),
        RetainedOutputErrorCode::UnknownOutput
    );
    let theirs_receipt = service
        .retain(
            &theirs,
            b"same complete output",
            256,
            CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(theirs_receipt.id(), first.id());
    assert_eq!(regular_files(&root).len(), 2);
}

#[cfg(unix)]
#[test]
fn complete_view_cap_includes_wrapper_and_metadata_overhead() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("retained");
    let service = RetainedOutputService::open(config(root.clone())).unwrap();
    let bytes = vec![b'a'; 4096];
    let receipt = service
        .retain(&owner("session"), &bytes, 240, CancellationToken::new())
        .unwrap();
    assert_eq!(receipt.rendered_preview().len(), 240);
    assert!(receipt.rendered_preview().contains(receipt.id().as_str()));
    assert!(receipt.rendered_preview().contains("4096 bytes"));
    assert!(receipt.preview_source_bytes() < bytes.len() as u64);
    assert!(receipt.truncated());

    let before = regular_files(&root).len();
    let error = service
        .retain(&owner("session"), b"body", 32, CancellationToken::new())
        .unwrap_err();
    assert_eq!(error.code(), RetainedOutputErrorCode::InvalidSpec);
    assert_eq!(regular_files(&root).len(), before);
}

#[cfg(unix)]
#[test]
fn object_and_generation_byte_caps_are_independent() {
    let temp = tempfile::tempdir().unwrap();
    let service = RetainedOutputService::open(
        config(temp.path().join("retained"))
            .with_max_object_bytes(6)
            .unwrap()
            .with_max_total_bytes(10)
            .unwrap(),
    )
    .unwrap();
    let mine = owner("mine");
    assert_eq!(
        service
            .retain(&mine, b"1234567", 256, CancellationToken::new())
            .unwrap_err()
            .code(),
        RetainedOutputErrorCode::InvalidSpec
    );
    service
        .retain(&mine, b"123456", 256, CancellationToken::new())
        .unwrap();
    assert_eq!(
        service
            .retain(&mine, b"abcde", 256, CancellationToken::new())
            .unwrap_err()
            .code(),
        RetainedOutputErrorCode::Capacity
    );
}

#[cfg(unix)]
#[test]
fn reads_are_range_bounded_and_foreign_ids_are_indistinguishable() {
    let temp = tempfile::tempdir().unwrap();
    let service = RetainedOutputService::open(config(temp.path().join("retained"))).unwrap();
    let mine = owner("mine");
    let theirs = owner("theirs");
    let bytes = vec![b'z'; MAX_RETAINED_OUTPUT_READ_BYTES + 33];
    let receipt = service
        .retain(&mine, &bytes, 256, CancellationToken::new())
        .unwrap();

    let first = service
        .read(&mine, receipt.id(), 0, usize::MAX, CancellationToken::new())
        .unwrap();
    assert_eq!(first.bytes().len(), MAX_RETAINED_OUTPUT_READ_BYTES);
    assert_eq!(first.offset(), 0);
    assert_eq!(first.total_bytes(), bytes.len() as u64);
    assert!(!first.eof());

    let last = service
        .read(
            &mine,
            receipt.id(),
            MAX_RETAINED_OUTPUT_READ_BYTES as u64,
            usize::MAX,
            CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(last.bytes(), &[b'z'; 33]);
    assert!(last.eof());

    for unknown_owner in [&theirs, &mine] {
        let id = if unknown_owner == &mine {
            heycode_exec::RetainedOutputId::parse(
                "sha256-0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap()
        } else {
            receipt.id().clone()
        };
        assert_eq!(
            service
                .read(unknown_owner, &id, 0, 1, CancellationToken::new(),)
                .unwrap_err()
                .code(),
            RetainedOutputErrorCode::UnknownOutput
        );
    }
}

#[cfg(unix)]
#[test]
fn corruption_breaks_content_identity_instead_of_returning_mutated_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("retained");
    let service = RetainedOutputService::open(config(root.clone())).unwrap();
    let mine = owner("mine");
    let receipt = service
        .retain(
            &mine,
            b"immutable retained bytes",
            256,
            CancellationToken::new(),
        )
        .unwrap();
    let object = regular_files(&root).pop().expect("one object");
    fs::write(&object, b"tampered").unwrap();
    assert_eq!(
        service
            .read(&mine, receipt.id(), 0, 64, CancellationToken::new(),)
            .unwrap_err()
            .code(),
        RetainedOutputErrorCode::Corrupt
    );
}

#[cfg(unix)]
#[test]
fn cancellation_commits_nothing_and_context_shutdown_cleans_held_service_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("retained");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let standalone = RetainedOutputService::open(config(root.clone())).unwrap();
    assert_eq!(
        standalone
            .retain(&owner("mine"), b"never stored", 256, cancelled)
            .unwrap_err()
            .code(),
        RetainedOutputErrorCode::Cancelled
    );
    assert!(regular_files(&root).is_empty());
    standalone.close().unwrap();

    let plugins = vec![retained_output_plugin(config(root.clone()))];
    let mut context = compose(&plugins).unwrap();
    let held = context
        .get::<RetainedOutputService>(SERVICE_RETAINED_OUTPUT)
        .unwrap();
    let receipt = held
        .retain(&owner("mine"), b"cleanup me", 256, CancellationToken::new())
        .unwrap();
    assert!(!regular_files(&root).is_empty());
    context.shutdown();
    assert!(regular_files(&root).is_empty());
    assert_eq!(
        held.read(&owner("mine"), receipt.id(), 0, 1, CancellationToken::new(),)
            .unwrap_err()
            .code(),
        RetainedOutputErrorCode::ServiceStopped
    );
}

//! Local filesystem Provider and Service Definition contract tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{Plugin, compose};
use heycode_exec::{
    EditFileSpec, FileSystemErrorCode, FileSystemPolicy, FileSystemRoot, FileSystemRootAccess,
    FileSystemService, GlobSpec, GrepSpec, PathRequest, ReadFileSpec, SERVICE_FILESYSTEM,
    SandboxMode, SandboxService, WriteFileSpec, local_filesystem_plugin, sandbox_service_plugin,
};
use tokio_util::sync::CancellationToken;

const IGNORED: &[&str] = &[".git", "node_modules", "target", "dist", ".next", "venv"];

fn local(root: &std::path::Path) -> FileSystemService {
    let policy =
        FileSystemPolicy::new([
            FileSystemRoot::new(root, FileSystemRootAccess::ReadWrite).expect("root")
        ])
        .expect("policy");
    FileSystemService::local(policy).expect("filesystem")
}

fn resolve(
    fs: &FileSystemService,
    cwd: &std::path::Path,
    path: &str,
) -> heycode_exec::ResolvedPath {
    fs.resolve(PathRequest::new(cwd, path).expect("valid path request"))
        .expect("resolved path")
}

#[tokio::test]
async fn ordered_multi_edit_validates_every_step_before_committing_and_supports_preview() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let fs = local(dir.path());
    let path = resolve(&fs, dir.path(), "f.txt");
    let read = fs
        .read(
            ReadFileSpec::new(path.clone(), 4096)
                .unwrap()
                .with_window(1, 200, None)
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let revision = read.page().unwrap().revision.clone();
    let failing = heycode_exec::MultiEditSpec::new(vec![
        EditFileSpec::new(path.clone(), "alpha", "new", false).unwrap(),
        EditFileSpec::new(path.clone(), "absent", "bad", false).unwrap(),
    ])
    .unwrap();
    let error = fs
        .edit_many(failing, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.edit_index(), Some(2));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "alpha\nbeta\ngamma\n"
    );
    let valid = heycode_exec::MultiEditSpec::new(vec![
        EditFileSpec::new(path.clone(), "alpha", "é", false).unwrap(),
        EditFileSpec::new(path.clone(), "é", "🙂", false).unwrap(),
    ])
    .unwrap()
    .with_expected_revision(revision.clone())
    .unwrap();
    let preview = fs
        .edit_many(valid.clone().with_dry_run(true), CancellationToken::new())
        .await
        .unwrap();
    assert!(preview.dry_run && preview.changed);
    assert_eq!(preview.revision, revision);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "alpha\nbeta\ngamma\n"
    );
    let applied = fs.edit_many(valid, CancellationToken::new()).await.unwrap();
    assert_eq!(applied.edits.len(), 2);
    assert_ne!(applied.revision, revision);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "🙂\nbeta\ngamma\n"
    );
}

#[tokio::test]
async fn checked_write_refuses_create_overwrite_and_stale_replace_and_preserves_noops() {
    let dir = tempfile::tempdir().unwrap();
    let fs = local(dir.path());
    let path = resolve(&fs, dir.path(), "f.txt");
    let write = WriteFileSpec::new(path.clone(), "hello\n").unwrap();
    let created = fs
        .write_checked(
            heycode_exec::CheckedWriteSpec::create(write.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(created.changed);
    assert!(
        fs.write_checked(
            heycode_exec::CheckedWriteSpec::create(write.clone()),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    let noop = fs
        .write_checked(
            heycode_exec::CheckedWriteSpec::replace(write, created.revision.clone()).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!noop.changed);
    assert_eq!(noop.revision, created.revision);
    std::fs::write(dir.path().join("f.txt"), "external change\n").unwrap();
    // Even a fresh unrelated observation must not make an old explicit revision current.
    fs.observations().mark(&dir.path().join("f.txt"));
    let stale = fs
        .write_checked(
            heycode_exec::CheckedWriteSpec::replace(
                WriteFileSpec::new(path, "wrong").unwrap(),
                created.revision,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code(), FileSystemErrorCode::StaleObservation);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "external change\n"
    );
}

#[tokio::test]
async fn multi_edit_replacement_growth_is_bounded_before_allocating_and_cancelled_batches_do_not_write()
 {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a".repeat(10000)).unwrap();
    let fs = local(dir.path());
    let path = resolve(&fs, dir.path(), "f.txt");
    fs.read(
        ReadFileSpec::new(path.clone(), 64).unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let too_large = heycode_exec::MultiEditSpec::new(vec![
        EditFileSpec::new(path.clone(), "a", "x".repeat(10000), true).unwrap(),
    ])
    .unwrap();
    assert_eq!(
        fs.edit_many(too_large, CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        FileSystemErrorCode::InvalidSpec
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let valid =
        heycode_exec::MultiEditSpec::new(vec![EditFileSpec::new(path, "a", "b", true).unwrap()])
            .unwrap();
    assert_eq!(
        fs.edit_many(valid, cancelled).await.unwrap_err().code(),
        FileSystemErrorCode::Cancelled
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "a".repeat(10000)
    );
}

#[test]
fn plugin_publishes_the_replaceable_service_and_shutdown_is_terminal() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let sandbox = SandboxService::new(
        SandboxMode::Off,
        dir.path().canonicalize().expect("canonical root"),
        None,
    )
    .expect("sandbox");
    let plugins: Vec<Box<dyn Plugin>> =
        vec![sandbox_service_plugin(sandbox), local_filesystem_plugin()];
    let mut context = compose(&plugins).expect("filesystem composition");
    assert_eq!(
        context.owner_of(SERVICE_FILESYSTEM),
        Some("filesystem-local")
    );
    let filesystem = context
        .get::<FileSystemService>(SERVICE_FILESYSTEM)
        .expect("filesystem service");

    context.shutdown();
    let error = filesystem
        .resolve(PathRequest::new(dir.path(), "after-shutdown.txt").expect("request"))
        .expect_err("held service must fail after provider shutdown");
    assert_eq!(error.code(), FileSystemErrorCode::ServiceStopped);
}

#[test]
fn plugin_requires_sandbox_and_derives_its_workspace_write_grant() {
    let filesystem_only: Vec<Box<dyn Plugin>> = vec![local_filesystem_plugin()];
    assert!(compose(&filesystem_only).is_err());

    let dir = tempfile::tempdir().expect("temporary directory");
    let sandbox = SandboxService::new(
        SandboxMode::ReadOnly,
        dir.path().canonicalize().expect("canonical root"),
        Some(std::sync::Arc::new(ReadOnlyBackend)),
    )
    .expect("sandbox");
    let plugins: Vec<Box<dyn Plugin>> =
        vec![sandbox_service_plugin(sandbox), local_filesystem_plugin()];
    let context = compose(&plugins).expect("composition");
    let filesystem = context
        .get::<FileSystemService>(SERVICE_FILESYSTEM)
        .expect("filesystem");
    assert_eq!(
        filesystem.policy().roots()[0].access(),
        FileSystemRootAccess::ReadOnly
    );
}

struct ReadOnlyBackend;

impl heycode_exec::Sandbox for ReadOnlyBackend {
    fn name(&self) -> &'static str {
        "read-only-fixture"
    }

    fn capabilities(&self) -> heycode_exec::SandboxBackendCapabilities {
        heycode_exec::SandboxBackendCapabilities {
            read_only: heycode_exec::SandboxSupport::Supported,
            workspace_write: heycode_exec::SandboxSupport::Unsupported,
            network_isolation: heycode_exec::SandboxSupport::Unsupported,
        }
    }

    fn confine(
        &self,
        argv: &[String],
        _policy: &heycode_exec::SandboxPolicy,
    ) -> Result<Vec<String>, heycode_exec::SandboxError> {
        Ok(argv.to_vec())
    }
}

#[tokio::test]
async fn bounded_text_read_marks_only_successful_observations() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let text_path = dir.path().join("text.txt");
    std::fs::write(&text_path, "alpha\nbeta\n").expect("seed text");
    let binary_path = dir.path().join("binary.bin");
    std::fs::write(&binary_path, vec![0_u8; 128]).expect("seed binary");

    let filesystem = local(dir.path());
    let text = resolve(&filesystem, dir.path(), "text.txt");
    let output = filesystem
        .read(
            ReadFileSpec::new(text.clone(), 5).expect("read spec"),
            CancellationToken::new(),
        )
        .await
        .expect("bounded read");
    assert_eq!(output.bytes(), b"alpha");
    assert!(output.truncated());
    assert!(filesystem.observations().contains(text.as_path()));
    assert!(filesystem.observations().is_fresh(text.as_path()));
    assert!(!format!("{output:?}").contains("alpha"));

    let binary = resolve(&filesystem, dir.path(), "binary.bin");
    let error = filesystem
        .read(
            ReadFileSpec::new(binary.clone(), 1024).expect("read spec"),
            CancellationToken::new(),
        )
        .await
        .expect_err("binary input must fail");
    assert_eq!(error.code(), FileSystemErrorCode::Binary);
    assert!(!filesystem.observations().contains(binary.as_path()));
    let raw = filesystem
        .read(
            ReadFileSpec::new_binary(binary.clone(), 8).expect("binary spec"),
            CancellationToken::new(),
        )
        .await
        .expect("explicit binary read");
    assert_eq!(raw.bytes(), &[0u8; 8]);
    assert!(raw.truncated());
    assert!(filesystem.observations().is_fresh(binary.as_path()));
}

#[tokio::test]
async fn write_and_edit_preserve_read_before_mutation_and_freshness() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let existing_path = dir.path().join("existing.txt");
    std::fs::write(&existing_path, "hello world\n").expect("seed existing file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&existing_path, std::fs::Permissions::from_mode(0o640))
            .expect("set original mode");
    }
    let filesystem = local(dir.path());
    let existing = resolve(&filesystem, dir.path(), "existing.txt");

    let error = filesystem
        .write(
            WriteFileSpec::new(existing.clone(), "blind overwrite").expect("write spec"),
            CancellationToken::new(),
        )
        .await
        .expect_err("blind overwrite must fail");
    assert_eq!(error.code(), FileSystemErrorCode::NotObserved);
    assert_eq!(
        std::fs::read_to_string(&existing_path).unwrap(),
        "hello world\n"
    );

    filesystem
        .read(
            ReadFileSpec::new(existing.clone(), 1024).expect("read spec"),
            CancellationToken::new(),
        )
        .await
        .expect("observe file");
    let edit = filesystem
        .edit(
            EditFileSpec::new(existing.clone(), "world", "rust", false).expect("edit spec"),
            CancellationToken::new(),
        )
        .await
        .expect("fresh edit");
    assert_eq!(edit.replacements(), 1);
    assert_eq!(edit.line(), 1);
    assert_eq!(edit.removed_line(), "hello world");
    assert_eq!(edit.inserted_line(), "hello rust");
    assert_eq!(
        std::fs::read_to_string(&existing_path).unwrap(),
        "hello rust\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&existing_path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640,
            "atomic edit must preserve the existing mode"
        );
    }
    assert!(filesystem.observations().is_fresh(existing.as_path()));

    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(&existing_path, "changed elsewhere\n").expect("external change");
    let error = filesystem
        .edit(
            EditFileSpec::new(existing, "changed", "edited", false).expect("edit spec"),
            CancellationToken::new(),
        )
        .await
        .expect_err("stale edit must fail");
    assert_eq!(error.code(), FileSystemErrorCode::StaleObservation);

    let created = resolve(&filesystem, dir.path(), "nested/new.txt");
    filesystem
        .write(
            WriteFileSpec::new(created.clone(), "new\nfile\n").expect("write spec"),
            CancellationToken::new(),
        )
        .await
        .expect("new file write");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("nested/new.txt")).unwrap(),
        "new\nfile\n"
    );
    assert!(filesystem.observations().contains(created.as_path()));
}

#[tokio::test]
async fn metadata_and_directory_creation_are_provider_operations() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let filesystem = local(dir.path());
    let nested = resolve(&filesystem, dir.path(), "one/two");
    filesystem
        .create_dir_all(nested.clone(), CancellationToken::new())
        .await
        .expect("create directory tree");
    let metadata = filesystem
        .metadata(nested, CancellationToken::new())
        .await
        .expect("directory metadata");
    assert_eq!(metadata.kind(), heycode_exec::FileEntryKind::Directory);
}

#[tokio::test]
async fn glob_and_grep_are_sorted_bounded_and_skip_ignored_directories() {
    let dir = tempfile::tempdir().expect("temporary directory");
    for (path, body) in [
        ("src/main.rs", "fn main() { /* needle */ }\n"),
        ("src/lib.rs", "// needle\n"),
        ("docs/readme.md", "needle\n"),
        ("node_modules/pkg/hidden.rs", "needle\n"),
        ("target/debug/hidden.rs", "needle\n"),
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).expect("parent");
        std::fs::write(path, body).expect("seed file");
    }
    let filesystem = local(dir.path());
    let root = resolve(&filesystem, dir.path(), ".");

    let glob = filesystem
        .glob(
            GlobSpec::new(root.clone(), "**/*.rs", IGNORED, 100).expect("glob spec"),
            CancellationToken::new(),
        )
        .await
        .expect("glob");
    assert_eq!(glob.matches(), ["src/lib.rs", "src/main.rs"]);
    assert_eq!(glob.total_matches(), 2);

    let grep = filesystem
        .grep(
            GrepSpec::new(root, "needle", Some("*.rs"), IGNORED, 200).expect("grep spec"),
            CancellationToken::new(),
        )
        .await
        .expect("grep");
    assert_eq!(grep.total_matches(), 2);
    assert_eq!(grep.matches()[0].path(), "src/lib.rs");
    assert_eq!(grep.matches()[0].line(), 1);
    assert_eq!(grep.matches()[0].text(), "// needle");
    assert_eq!(grep.matches()[1].path(), "src/main.rs");
    assert!(!format!("{grep:?}").contains("needle"));
}

#[tokio::test]
async fn cancellation_and_failures_use_stable_body_free_classes() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let filesystem = local(dir.path());
    let missing = resolve(&filesystem, dir.path(), "canary-secret-name.txt");
    let cancelled = CancellationToken::new();
    cancelled.cancel();

    let error = filesystem
        .read(
            ReadFileSpec::new(missing.clone(), 1024).expect("read spec"),
            cancelled,
        )
        .await
        .expect_err("pre-cancelled read must fail");
    assert_eq!(error.code(), FileSystemErrorCode::Cancelled);

    let error = filesystem
        .read(
            ReadFileSpec::new(missing, 1024).expect("read spec"),
            CancellationToken::new(),
        )
        .await
        .expect_err("missing file must fail");
    assert_eq!(error.code(), FileSystemErrorCode::NotFound);
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains("canary-secret-name"));
    assert!(!diagnostic.contains(dir.path().to_string_lossy().as_ref()));
}

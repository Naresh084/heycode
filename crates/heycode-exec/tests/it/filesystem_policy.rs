//! E05 canonical-root, symlink, and operation authority contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_exec::{
    EditFileSpec, FileSystemErrorCode, FileSystemPolicy, FileSystemRoot, FileSystemRootAccess,
    FileSystemService, PathRequest, ReadFileSpec, SandboxMode, SandboxService, WriteFileSpec,
};
#[cfg(unix)]
use heycode_exec::{GlobSpec, GrepSpec};
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
const IGNORED: &[&str] = &[".git", "node_modules", "target", "dist", ".next", "venv"];

fn policy(workspace: &std::path::Path, access: FileSystemRootAccess) -> FileSystemPolicy {
    FileSystemPolicy::new([FileSystemRoot::new(workspace, access).unwrap()]).unwrap()
}

fn local(workspace: &std::path::Path) -> FileSystemService {
    FileSystemService::local(policy(workspace, FileSystemRootAccess::ReadWrite)).unwrap()
}

fn resolve(
    filesystem: &FileSystemService,
    cwd: &std::path::Path,
    path: impl AsRef<std::path::Path>,
) -> Result<heycode_exec::ResolvedPath, heycode_exec::FileSystemError> {
    filesystem.resolve(PathRequest::new(cwd, path.as_ref()).unwrap())
}

#[test]
fn policy_is_explicit_bounded_and_redacted() {
    let workspace = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let policy = FileSystemPolicy::new([
        FileSystemRoot::new(workspace.path(), FileSystemRootAccess::ReadWrite).unwrap(),
        FileSystemRoot::new(extra.path(), FileSystemRootAccess::ReadOnly).unwrap(),
    ])
    .unwrap();
    assert_eq!(policy.roots().len(), 2);
    assert_eq!(policy.roots()[0].access(), FileSystemRootAccess::ReadWrite);
    assert_eq!(policy.roots()[1].access(), FileSystemRootAccess::ReadOnly);
    let diagnostic = format!("{policy:?}");
    assert!(!diagnostic.contains(workspace.path().to_string_lossy().as_ref()));
    assert!(!diagnostic.contains(extra.path().to_string_lossy().as_ref()));

    assert!(FileSystemPolicy::new(Vec::<FileSystemRoot>::new()).is_err());
    assert!(FileSystemRoot::new("relative", FileSystemRootAccess::ReadWrite).is_err());
}

#[cfg(unix)]
#[test]
fn local_provider_rejects_two_logical_roots_with_one_canonical_identity() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let real = parent.path().join("real");
    let alias = parent.path().join("alias");
    std::fs::create_dir(&real).unwrap();
    symlink(&real, &alias).unwrap();
    let policy = FileSystemPolicy::new([
        FileSystemRoot::new(&real, FileSystemRootAccess::ReadWrite).unwrap(),
        FileSystemRoot::new(&alias, FileSystemRootAccess::ReadOnly).unwrap(),
    ])
    .unwrap();
    let error = FileSystemService::local(policy).expect_err("canonical alias collision");
    assert_eq!(error.code(), FileSystemErrorCode::InvalidSpec);
}

#[cfg(unix)]
#[test]
fn symlink_loops_have_the_stable_path_traversal_class() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    symlink("loop-b", workspace.path().join("loop-a")).unwrap();
    symlink("loop-a", workspace.path().join("loop-b")).unwrap();
    let filesystem = local(workspace.path());
    let error = resolve(&filesystem, workspace.path(), "loop-a/file.txt")
        .expect_err("symlink loop must fail before access");
    assert_eq!(error.code(), FileSystemErrorCode::PathTraversal);
}

#[test]
fn sandbox_and_filesystem_share_one_canonical_workspace_root() {
    let outer = tempfile::tempdir().unwrap();
    let workspace = outer.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let noncanonical = workspace.join(".");
    let sandbox = SandboxService::new(SandboxMode::Off, noncanonical, None).unwrap();
    let policy = FileSystemPolicy::from_sandbox(sandbox.policy()).unwrap();
    assert_eq!(
        sandbox.policy().workspace_root,
        workspace.canonicalize().unwrap()
    );
    assert_eq!(policy.roots()[0].path(), sandbox.policy().workspace_root);
}

#[tokio::test]
async fn canonical_resolution_denies_parent_and_absolute_root_escapes() {
    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = parent.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    let filesystem = local(&workspace);

    let inside = resolve(&filesystem, &workspace, "./nested/../file.txt")
        .expect_err("parent components are rejected instead of reinterpreted");
    assert_eq!(inside.code(), FileSystemErrorCode::PathTraversal);

    let error = resolve(&filesystem, &workspace, &outside).expect_err("absolute escape");
    assert_eq!(error.code(), FileSystemErrorCode::OutsideAllowedRoots);
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains("outside.txt"));
    assert!(!diagnostic.contains(parent.path().to_string_lossy().as_ref()));
}

#[tokio::test]
async fn explicit_extra_root_is_readable_but_read_only_for_mutations() {
    let workspace = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let extra_file = extra.path().join("extra.txt");
    std::fs::write(&extra_file, "external\n").unwrap();
    let policy = FileSystemPolicy::new([
        FileSystemRoot::new(workspace.path(), FileSystemRootAccess::ReadWrite).unwrap(),
        FileSystemRoot::new(extra.path(), FileSystemRootAccess::ReadOnly).unwrap(),
    ])
    .unwrap();
    let filesystem = FileSystemService::local(policy).unwrap();
    let resolved = resolve(&filesystem, workspace.path(), &extra_file).unwrap();
    let output = filesystem
        .read(
            ReadFileSpec::new(resolved.clone(), 1024).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.bytes(), b"external\n");

    let error = filesystem
        .write(
            WriteFileSpec::new(resolved, "blocked").unwrap(),
            CancellationToken::new(),
        )
        .await
        .expect_err("read-only root must fail before commit");
    assert_eq!(error.code(), FileSystemErrorCode::ReadOnlyRoot);
    assert_eq!(std::fs::read_to_string(extra_file).unwrap(), "external\n");
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_may_stay_inside_but_never_cross_an_allowed_root() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    let outside = parent.path().join("outside");
    std::fs::create_dir_all(workspace.join("real")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(workspace.join("real/in.txt"), "inside\n").unwrap();
    std::fs::write(outside.join("secret.txt"), "outside\n").unwrap();
    symlink("real", workspace.join("inside-link")).unwrap();
    symlink(&outside, workspace.join("outside-link")).unwrap();
    let filesystem = local(&workspace);

    let inside = resolve(&filesystem, &workspace, "inside-link/in.txt").unwrap();
    assert_eq!(
        inside.as_path(),
        workspace.join("real/in.txt").canonicalize().unwrap()
    );
    assert_eq!(
        filesystem
            .read(
                ReadFileSpec::new(inside, 1024).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .bytes(),
        b"inside\n"
    );

    let error = resolve(&filesystem, &workspace, "outside-link/secret.txt")
        .expect_err("symlink escape must fail during resolution");
    assert_eq!(error.code(), FileSystemErrorCode::OutsideAllowedRoots);

    let crafted = heycode_exec::ResolvedPath::new(
        workspace
            .canonicalize()
            .unwrap()
            .join("outside-link/secret.txt"),
    )
    .unwrap();
    let error = filesystem
        .read(
            ReadFileSpec::new(crafted.clone(), 1024).unwrap(),
            CancellationToken::new(),
        )
        .await
        .expect_err("constructed paths still cross provider policy");
    assert_eq!(error.code(), FileSystemErrorCode::PathTraversal);
    let error = filesystem
        .write(
            WriteFileSpec::new(crafted, "overwrite").unwrap(),
            CancellationToken::new(),
        )
        .await
        .expect_err("write must not follow an outside symlink");
    assert!(matches!(
        error.code(),
        FileSystemErrorCode::PathTraversal | FileSystemErrorCode::ReadOnlyRoot
    ));
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "outside\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn search_and_directory_creation_cannot_cross_symlinked_parents() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    let outside = parent.path().join("outside");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.rs"), "needle\n").unwrap();
    symlink(&outside, workspace.join("escape")).unwrap();
    let filesystem = local(&workspace);
    let canonical_workspace = workspace.canonicalize().unwrap();
    let crafted_root = heycode_exec::ResolvedPath::new(canonical_workspace.join("escape")).unwrap();

    for code in [
        filesystem
            .glob(
                GlobSpec::new(crafted_root.clone(), "**/*.rs", IGNORED, 100).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .code(),
        filesystem
            .grep(
                GrepSpec::new(crafted_root.clone(), "needle", None, IGNORED, 100).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .code(),
        filesystem
            .create_dir_all(
                heycode_exec::ResolvedPath::new(canonical_workspace.join("escape/new-dir"))
                    .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .code(),
    ] {
        assert_eq!(code, FileSystemErrorCode::PathTraversal);
    }
    assert!(!outside.join("new-dir").exists());
}

#[tokio::test]
async fn write_and_edit_reject_a_target_changed_after_observation() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("file.txt");
    std::fs::write(&path, "observed\n").unwrap();
    let filesystem = local(workspace.path());
    let resolved = resolve(&filesystem, workspace.path(), &path).unwrap();
    filesystem
        .read(
            ReadFileSpec::new(resolved.clone(), 1024).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let replacement = workspace.path().join("replacement.txt");
    std::fs::write(&replacement, "swapped\n").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    let write_error = filesystem
        .write(
            WriteFileSpec::new(resolved.clone(), "must not commit").unwrap(),
            CancellationToken::new(),
        )
        .await
        .expect_err("write target identity changed");
    assert_eq!(write_error.code(), FileSystemErrorCode::StaleObservation);
    let edit_error = filesystem
        .edit(
            EditFileSpec::new(resolved, "swapped", "edited", false).unwrap(),
            CancellationToken::new(),
        )
        .await
        .expect_err("edit target identity changed");
    assert_eq!(edit_error.code(), FileSystemErrorCode::StaleObservation);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "swapped\n");
}

//! QSEC03 — adversarial path-escape matrix against the E05 canonical path and
//! symlink policy.
//!
//! The policy under attack lives in `heycode-exec`; this suite drives it through
//! its public surface only. It is filed here because the boundary it defends —
//! "the workspace and nothing else" — is the same boundary the sandbox
//! backends fence at the process level, and the two must not disagree.
//!
//! Cases are ordinary filesystem races and link tricks: a symlink out of the
//! workspace, a symlink planted *between* the check and the use, a hard-linked
//! alias, parent traversal, root substitution, and case-insensitive collisions
//! on the host filesystem. Everything runs in a private temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use heycode_exec::{
    FileSystemError, FileSystemErrorCode, FileSystemPolicy, FileSystemRoot, FileSystemRootAccess,
    FileSystemService, PathRequest, ReadFileSpec, ResolvedPath, WriteFileSpec,
};
use tokio_util::sync::CancellationToken;

/// Deadline for every case, so a lock or a race that hangs fails the gate
/// instead of stalling it.
const CASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

struct Arena {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
}

fn arena() -> Arena {
    let root = tempfile::tempdir().expect("create arena");
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&outside).expect("create outside");
    std::fs::write(outside.join("secret.txt"), b"SECRET").expect("seed the outside secret");
    Arena {
        _root: root,
        workspace,
        outside,
    }
}

fn service(roots: &[(&Path, FileSystemRootAccess)]) -> FileSystemService {
    let declared = roots
        .iter()
        .map(|(path, access)| FileSystemRoot::new(*path, *access).expect("declare a root"));
    FileSystemService::local(FileSystemPolicy::new(declared).expect("build the policy"))
        .expect("build the local provider")
}

fn workspace_service(arena: &Arena) -> FileSystemService {
    service(&[(arena.workspace.as_path(), FileSystemRootAccess::ReadWrite)])
}

fn request(cwd: &Path, path: &str) -> PathRequest {
    PathRequest::new(cwd, path).expect("build a path request")
}

fn code(error: &FileSystemError) -> FileSystemErrorCode {
    error.code()
}

// ── Technique: symlink out of the workspace ─────────────────────────────────

/// A symlink whose target is outside the workspace is resolved before the
/// boundary check, so it cannot be used to name an outside path — whether it
/// points at a directory, a file, or a chain of further symlinks.
#[test]
fn a_symlink_out_of_the_workspace_is_resolved_before_the_boundary_check() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let arena = arena();
        symlink(&arena.outside, arena.workspace.join("dir-link")).expect("link a directory out");
        symlink(
            arena.outside.join("secret.txt"),
            arena.workspace.join("file-link"),
        )
        .expect("link a file out");
        symlink("dir-link", arena.workspace.join("hop")).expect("link to the link");
        symlink("/etc/passwd", arena.workspace.join("absolute-link")).expect("link to an absolute");

        let service = workspace_service(&arena);
        for escape in [
            "dir-link/secret.txt",
            "file-link",
            "hop/secret.txt",
            "absolute-link",
        ] {
            let error = service
                .resolve(request(&arena.workspace, escape))
                .expect_err(&format!("{escape} must not resolve inside the workspace"));
            assert_eq!(
                code(&error),
                FileSystemErrorCode::OutsideAllowedRoots,
                "{escape} resolved to {error}"
            );
        }
    }
}

/// A symlink that stays inside the workspace is ordinary and must keep
/// working; a boundary that rejected it would be over-blocking, not security.
#[test]
fn a_symlink_that_stays_inside_the_workspace_still_resolves() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let arena = arena();
        std::fs::create_dir_all(arena.workspace.join("real")).expect("create the target directory");
        std::fs::write(arena.workspace.join("real/file.txt"), b"inside").expect("seed");
        symlink(
            arena.workspace.join("real"),
            arena.workspace.join("inner-link"),
        )
        .expect("link inside");

        let resolved = workspace_service(&arena)
            .resolve(request(&arena.workspace, "inner-link/file.txt"))
            .expect("an inside symlink must resolve");
        assert!(
            resolved
                .as_path()
                .starts_with(std::fs::canonicalize(&arena.workspace).expect("canonical workspace"))
        );
    }
}

// ── Technique: parent traversal and absolute escape ─────────────────────────

/// Traversal is refused on the request, before any filesystem access, so a
/// crafted path cannot be used to probe for the existence of outside files.
#[test]
fn parent_traversal_is_refused_on_the_request_and_never_reaches_the_filesystem() {
    let arena = arena();
    let service = workspace_service(&arena);
    for traversal in [
        "../outside/secret.txt",
        "sub/../../outside/secret.txt",
        "./../../etc/passwd",
        "..",
        "a/b/c/../../../../outside/secret.txt",
    ] {
        let error = service
            .resolve(request(&arena.workspace, traversal))
            .expect_err(&format!("{traversal} must be refused"));
        assert_eq!(
            code(&error),
            FileSystemErrorCode::PathTraversal,
            "{traversal}"
        );
    }
}

/// An absolute path outside every root is refused as outside the boundary
/// rather than as malformed, so the failure is attributable.
#[test]
fn an_absolute_path_outside_every_root_is_refused_as_outside_the_boundary() {
    let arena = arena();
    let service = workspace_service(&arena);
    let outside = arena.outside.join("secret.txt").display().to_string();
    for absolute in [outside.as_str(), "/etc/passwd", "/"] {
        let error = service
            .resolve(request(&arena.workspace, absolute))
            .expect_err(&format!("{absolute} must be refused"));
        assert_eq!(
            code(&error),
            FileSystemErrorCode::OutsideAllowedRoots,
            "{absolute}"
        );
    }
}

/// A path whose *name* only looks like a sibling of the root must not be
/// admitted by a string prefix match.
#[tokio::test]
async fn a_sibling_directory_sharing_the_root_name_prefix_is_not_inside_the_root() {
    let arena = arena();
    let sibling = arena.workspace.with_file_name("workspace-evil");
    std::fs::create_dir_all(&sibling).expect("create the lookalike sibling");
    std::fs::write(sibling.join("planted.txt"), b"planted").expect("seed the sibling");

    let service = workspace_service(&arena);
    let error = service
        .resolve(request(
            &arena.workspace,
            &sibling.join("planted.txt").display().to_string(),
        ))
        .expect_err("a name-prefix sibling is outside the root");
    assert_eq!(code(&error), FileSystemErrorCode::OutsideAllowedRoots);
}

// ── Technique: TOCTOU between resolution and use ────────────────────────────

/// The load-bearing race: a path is resolved while it is honest, then a
/// component is swapped for a symlink out of the workspace before the write
/// commits. The capability directory must refuse to traverse it.
#[tokio::test]
async fn a_directory_swapped_for_an_escaping_symlink_after_resolution_is_refused_at_use() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let arena = arena();
        let honest = arena.workspace.join("sub");
        std::fs::create_dir_all(&honest).expect("create the honest directory");
        let service = workspace_service(&arena);

        let resolved = service
            .resolve(request(&arena.workspace, "sub/file.txt"))
            .expect("the honest path resolves inside the workspace");

        // The swap happens here — after the check, before the use.
        std::fs::remove_dir_all(&honest).expect("remove the honest directory");
        symlink(&arena.outside, &honest).expect("swap in an escaping symlink");

        let outcome = tokio::time::timeout(
            CASE_TIMEOUT,
            service.write(
                WriteFileSpec::new(resolved, b"ESCAPED").expect("build the write"),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("the write returns within the case deadline");
        assert!(
            outcome.is_err(),
            "a post-resolution symlink swap must not be followed"
        );
        assert!(
            !arena.outside.join("file.txt").exists(),
            "nothing may be created outside the workspace"
        );
    }
}

/// The same race one level up: the *root itself* is replaced between the
/// service being built and the write committing. Root identity is stamped, so
/// the commit must refuse rather than write into the substitute.
#[tokio::test]
async fn a_workspace_root_replaced_after_the_service_is_built_is_refused_at_commit() {
    let arena = arena();
    let service = workspace_service(&arena);
    let resolved = service
        .resolve(request(&arena.workspace, "file.txt"))
        .expect("resolve inside the original root");

    let decoy = arena.outside.join("decoy-root");
    std::fs::create_dir_all(&decoy).expect("create the decoy root");
    std::fs::remove_dir_all(&arena.workspace).expect("remove the original root");
    std::fs::rename(&decoy, &arena.workspace).expect("swap the decoy into place");

    let outcome = tokio::time::timeout(
        CASE_TIMEOUT,
        service.write(
            WriteFileSpec::new(resolved, b"SUBSTITUTED").expect("build the write"),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the write returns within the case deadline");
    let error = outcome.expect_err("a substituted root must be refused");
    assert_eq!(
        code(&error),
        FileSystemErrorCode::ChangedAtCommit,
        "root substitution must be attributable, not a generic I/O failure"
    );
    assert!(!arena.workspace.join("file.txt").exists());
}

/// A hard link is an alias for an inode, and no path check can see it. The
/// atomic-write design is what defeats it: the commit renames a fresh file
/// over the name, breaking the alias instead of writing through it.
#[tokio::test]
async fn a_hard_linked_alias_is_replaced_rather_than_written_through() {
    let arena = arena();
    let victim = arena.outside.join("secret.txt");
    let alias = arena.workspace.join("alias.txt");
    std::fs::hard_link(&victim, &alias).expect("plant the alias inside the workspace");

    let service = workspace_service(&arena);
    let resolved = service
        .resolve(request(&arena.workspace, "alias.txt"))
        .expect("the alias resolves inside the workspace");

    // An existing file needs a fresh observation before it can be overwritten.
    let read = tokio::time::timeout(
        CASE_TIMEOUT,
        service.read(
            ReadFileSpec::new(resolved.clone(), 4_096).expect("build the read"),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the read returns within the case deadline")
    .expect("the alias is readable");
    assert_eq!(read.bytes(), b"SECRET");

    tokio::time::timeout(
        CASE_TIMEOUT,
        service.write(
            WriteFileSpec::new(resolved, b"OVERWRITTEN").expect("build the write"),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the write returns within the case deadline")
    .expect("writing a file inside the workspace is permitted");

    assert_eq!(
        std::fs::read_to_string(&victim).expect("the outside file still exists"),
        "SECRET",
        "the outside inode must be untouched: an atomic rename breaks the alias"
    );
    assert_eq!(
        std::fs::read_to_string(&alias).expect("the workspace file exists"),
        "OVERWRITTEN"
    );
}

// ── Technique: root selection and access downgrade ──────────────────────────

/// With nested roots the most specific one wins, so a read-only subtree cannot
/// be written by naming it through its read-write parent.
#[tokio::test]
async fn a_read_only_subtree_cannot_be_written_through_its_read_write_parent() {
    let arena = arena();
    let protected = arena.workspace.join("protected");
    std::fs::create_dir_all(&protected).expect("create the protected subtree");
    let service = service(&[
        (arena.workspace.as_path(), FileSystemRootAccess::ReadWrite),
        (protected.as_path(), FileSystemRootAccess::ReadOnly),
    ]);

    let resolved = service
        .resolve(request(&arena.workspace, "protected/file.txt"))
        .expect("the protected path still resolves for reading");
    let error = tokio::time::timeout(
        CASE_TIMEOUT,
        service.write(
            WriteFileSpec::new(resolved, b"WRITTEN").expect("build the write"),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the write returns within the case deadline")
    .expect_err("a read-only subtree must refuse a write");
    assert_eq!(code(&error), FileSystemErrorCode::ReadOnlyRoot);
    assert!(!protected.join("file.txt").exists());
}

/// The same attack spelled with a different letter case. On a case-insensitive
/// filesystem `PROTECTED` and `protected` are one directory, and a boundary
/// that compared the caller's spelling would attribute the path to the
/// read-write parent instead. This host's `canonicalize` is what closes it, so
/// the test asserts both the outcome and the mechanism.
#[tokio::test]
async fn a_case_variant_of_a_read_only_subtree_is_still_attributed_to_that_subtree() {
    let arena = arena();
    let protected = arena.workspace.join("protected");
    std::fs::create_dir_all(&protected).expect("create the protected subtree");

    let variant = arena.workspace.join("PROTECTED");
    let case_insensitive = variant.is_dir();
    let service = service(&[
        (arena.workspace.as_path(), FileSystemRootAccess::ReadWrite),
        (protected.as_path(), FileSystemRootAccess::ReadOnly),
    ]);

    let resolved = service.resolve(request(&arena.workspace, "PROTECTED/file.txt"));
    if !case_insensitive {
        // Case-sensitive host: `PROTECTED` simply does not exist, and the
        // nearest-existing-ancestor rule places the path under the read-write
        // parent. That is correct there and is not the attacked cell.
        assert!(
            resolved.is_ok(),
            "a case-sensitive host has no collision to exploit"
        );
        return;
    }

    let resolved = resolved.expect("the case variant resolves on a case-insensitive host");
    assert!(
        resolved
            .as_path()
            .starts_with(std::fs::canonicalize(&protected).expect("canonical protected subtree")),
        "canonicalisation must fold the spelling back onto the on-disk name, or the read-only \
         root can be bypassed by case alone: {resolved:?}"
    );
    let error = tokio::time::timeout(
        CASE_TIMEOUT,
        service.write(
            WriteFileSpec::new(resolved, b"WRITTEN").expect("build the write"),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the write returns within the case deadline")
    .expect_err("the case variant must inherit the read-only grant");
    assert_eq!(code(&error), FileSystemErrorCode::ReadOnlyRoot);
    assert!(!protected.join("file.txt").exists());
}

/// Two declarations that name the same directory by different routes are one
/// root, and the policy refuses the ambiguity rather than picking a winner.
#[test]
fn two_roots_that_canonicalise_to_the_same_directory_are_refused() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let arena = arena();
        let alias = arena.outside.join("workspace-alias");
        symlink(&arena.workspace, &alias).expect("alias the workspace");
        let policy = FileSystemPolicy::new([
            FileSystemRoot::new(&arena.workspace, FileSystemRootAccess::ReadWrite).unwrap(),
            FileSystemRoot::new(&alias, FileSystemRootAccess::ReadOnly).unwrap(),
        ])
        .expect("two distinct declarations build a policy");
        assert!(
            FileSystemService::local(policy).is_err(),
            "two routes to one directory must not produce two grants"
        );
    }
}

/// Root declarations are validated before anything is opened: traversal,
/// relative paths, and embedded NUL are all refused up front.
#[test]
fn malformed_root_and_request_shapes_are_refused_before_any_filesystem_access() {
    assert!(FileSystemRoot::new("relative/path", FileSystemRootAccess::ReadWrite).is_err());
    assert!(FileSystemRoot::new("/ws/../etc", FileSystemRootAccess::ReadWrite).is_err());
    assert!(FileSystemRoot::new("", FileSystemRootAccess::ReadWrite).is_err());
    assert!(FileSystemPolicy::new([]).is_err());

    assert!(PathRequest::new("relative", "also-relative").is_err());
    assert!(PathRequest::new("/ws", "").is_err());
    assert!(PathRequest::new("/ws", "file\0.txt").is_err());
    assert!(PathRequest::new("/ws\0", "file.txt").is_err());
    assert!(ResolvedPath::new("relative/path").is_err());
    assert!(ResolvedPath::new("/ws/file\0.txt").is_err());
}

/// A hand-built `ResolvedPath` skips resolution entirely, which is exactly
/// what an attacker with access to the service would do. Every operation must
/// re-check the boundary rather than trusting the type.
#[tokio::test]
async fn a_hand_built_resolved_path_is_still_checked_against_the_roots() {
    let arena = arena();
    let service = workspace_service(&arena);
    let canonical_workspace = std::fs::canonicalize(&arena.workspace).expect("canonical workspace");

    for forged in [
        arena.outside.join("secret.txt"),
        PathBuf::from("/etc/passwd"),
        canonical_workspace
            .join("..")
            .join("outside")
            .join("secret.txt"),
        canonical_workspace.join(".").join("file.txt"),
    ] {
        let path = ResolvedPath::new(&forged).expect("an absolute path builds a ResolvedPath");
        let read = tokio::time::timeout(
            CASE_TIMEOUT,
            service.read(
                ReadFileSpec::new(path.clone(), 4_096).expect("build the read"),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("the read returns within the case deadline");
        assert!(read.is_err(), "{} must not be readable", forged.display());

        let write = tokio::time::timeout(
            CASE_TIMEOUT,
            service.write(
                WriteFileSpec::new(path, b"FORGED").expect("build the write"),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("the write returns within the case deadline");
        assert!(write.is_err(), "{} must not be writable", forged.display());
    }
    assert_eq!(
        std::fs::read_to_string(arena.outside.join("secret.txt")).expect("the secret survives"),
        "SECRET"
    );
}

/// Windows device aliases and alternate streams are lexical namespace escapes,
/// not ordinary files beneath the workspace. They must fail before a provider
/// can open them.
#[cfg(windows)]
#[test]
fn windows_device_alias_and_alternate_stream_names_are_refused() {
    let arena = arena();
    let service = workspace_service(&arena);
    for unsafe_name in [
        "CON",
        "con.txt",
        "NUL.json",
        "CLOCK$",
        "CONIN$",
        "CONOUT$",
        "COM1",
        "COM¹.txt",
        "LPT9.log",
        "LPT³.txt",
        "file.txt:secret",
        "trailing.",
        "trailing ",
    ] {
        assert!(
            service
                .resolve(request(&arena.workspace, unsafe_name))
                .is_err(),
            "Windows-special path was admitted: {unsafe_name:?}"
        );
    }
}

/// Windows cells whose real filesystem semantics cannot execute away from a
/// Windows host. Named so the remaining matrix records them as Unknown rather
/// than passing.
#[cfg(not(windows))]
#[test]
#[allow(clippy::print_stderr)]
fn windows_path_escape_cells_are_unknown_on_this_host() {
    eprintln!(
        "SKIP: directory junctions, mount points, 8.3 short-name aliases and `\\\\?\\` \
         namespace behavior are not exercised off Windows"
    );
}

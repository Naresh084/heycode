//! QSEC03 — adversarial sandbox-escape matrix, runtime half.
//!
//! Every case here launches a real confined process and tries to get past the
//! fence. The probe child is this same test executable re-entered under the
//! backend's wrapper, so no interpreter, shell quoting or PATH entry is part
//! of the result.
//!
//! Cells not exercised on the running host are named explicitly by the skip
//! functions at the bottom of this file. A cell that could not run is Unknown,
//! never "pass".

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(target_os = "macos")]
mod macos {
    use std::fs;
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use heycode_exec::{
        FileWriteScope, NetworkScope, Sandbox, SandboxMode, SandboxPolicy, SandboxService,
    };
    use heycode_sandbox::SeatbeltSandbox;

    const PROBE_KIND: &str = "HEYCODE_ESCAPE_PROBE_KIND";
    const PROBE_TARGET: &str = "HEYCODE_ESCAPE_PROBE_TARGET";
    const PROBE_SECOND: &str = "HEYCODE_ESCAPE_PROBE_SECOND";
    const PROBE_OK: &str = "HEYCODE_ESCAPE_PROBE_OK";
    const PROBE_DENIED: &str = "HEYCODE_ESCAPE_PROBE_DENIED";
    const DENIED_EXIT: i32 = 73;

    /// Outcome of one confined attempt, as observed from outside the sandbox.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Allowed,
        Denied,
    }

    fn policy(mode: SandboxMode, workspace: &Path) -> SandboxPolicy {
        SandboxPolicy {
            mode,
            workspace_root: workspace.to_path_buf(),
        }
    }

    /// Run one operation inside the sandbox and report whether the OS let it
    /// through. `second` carries the destination for the two-path operations.
    fn attempt(
        backend: &SeatbeltSandbox,
        policy: &SandboxPolicy,
        kind: &str,
        target: &Path,
        second: Option<&Path>,
    ) -> Outcome {
        let executable = std::env::current_exe().expect("resolve integration-test binary");
        let child = vec![
            executable.display().to_string(),
            "--exact".to_owned(),
            crate::it::test_name(module_path!(), "escape_probe_child"),
            "--nocapture".to_owned(),
        ];
        let confined = backend
            .confine(&child, policy)
            .expect("build confined argv");
        let mut command = Command::new(&confined[0]);
        command
            .args(&confined[1..])
            .env_clear()
            .env(PROBE_KIND, kind)
            .env(PROBE_TARGET, target.display().to_string());
        if let Some(second) = second {
            command.env(PROBE_SECOND, second.display().to_string());
        }
        let output = command.output().expect("run confined probe child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains(PROBE_OK) || stdout.contains(PROBE_DENIED),
            "the probe child never reported for {kind} on {}; status={:?}; stdout={stdout:?}; \
             stderr={stderr:?}",
            target.display(),
            output.status.code()
        );
        if stdout.contains(PROBE_OK) {
            assert!(output.status.success(), "an allowed probe must exit zero");
            Outcome::Allowed
        } else {
            assert_eq!(output.status.code(), Some(DENIED_EXIT));
            Outcome::Denied
        }
    }

    fn backend() -> SeatbeltSandbox {
        SeatbeltSandbox::new()
            .expect("macOS runtime evidence requires the system sandbox-exec binary")
    }

    /// A workspace and an unrelated sibling directory outside it, both real.
    struct Arena {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        outside: PathBuf,
    }

    fn arena() -> Arena {
        let root = tempfile::tempdir().expect("create arena");
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&outside).expect("create outside");
        Arena {
            _root: root,
            workspace,
            outside,
        }
    }

    // ── E10 acceptance: the report is the specification ─────────────────────

    /// E10's acceptance is that the reported choices match enforcement. This
    /// drives the checks from the report itself rather than from a hardcoded
    /// expectation, so a row that starts over-promising fails here.
    #[test]
    fn every_selectable_choice_enforces_exactly_the_scope_its_report_row_promises() {
        let arena = arena();
        let backend = backend();
        let shared: std::sync::Arc<dyn Sandbox> = std::sync::Arc::new(
            SeatbeltSandbox::new()
                .expect("macOS runtime evidence requires the system sandbox-exec binary"),
        );
        let service =
            SandboxService::new(SandboxMode::WorkspaceWrite, &arena.workspace, Some(shared))
                .expect("build a workspace-write service");
        let report = service.capability_report();
        assert_eq!(report.available_backend, Some("seatbelt"));

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind network probe listener");
        let listener_address = listener.local_addr().expect("probe listener address");

        let mut exercised = 0_usize;
        for choice in &report.choices {
            if !choice.selectable {
                continue;
            }
            exercised += 1;
            let policy = policy(choice.mode, &arena.workspace);
            let inside = arena
                .workspace
                .join(format!("{}-inside", choice.mode.as_str()));
            let outside = arena
                .outside
                .join(format!("{}-outside", choice.mode.as_str()));
            let temp_root =
                PathBuf::from(format!("/private/tmp/dshx-qsec03-{}", std::process::id()));
            let temp = temp_root.join(format!("{}-temp", choice.mode.as_str()));
            fs::create_dir_all(&temp_root).expect("create shared temp probe root");

            let (expect_inside, expect_outside, expect_temp) = match choice.file_write {
                FileWriteScope::Host => (Outcome::Allowed, Outcome::Allowed, Outcome::Allowed),
                FileWriteScope::DeviceOnly => (Outcome::Denied, Outcome::Denied, Outcome::Denied),
                FileWriteScope::WorkspaceAndTemp => {
                    (Outcome::Allowed, Outcome::Denied, Outcome::Allowed)
                }
                FileWriteScope::Unspecified => {
                    panic!("a selectable choice must not report an unspecified write scope")
                }
            };
            assert_eq!(
                attempt(&backend, &policy, "write", &inside, None),
                expect_inside,
                "{:?} promises {:?} for a workspace write",
                choice.mode,
                choice.file_write
            );
            assert_eq!(
                attempt(&backend, &policy, "write", &outside, None),
                expect_outside,
                "{:?} promises {:?} for a write outside the workspace",
                choice.mode,
                choice.file_write
            );
            assert_eq!(
                attempt(&backend, &policy, "write", &temp, None),
                expect_temp,
                "{:?} promises {:?} for a write to the shared host temp root",
                choice.mode,
                choice.file_write
            );
            // The device sink is writable under every selectable choice; that
            // is the whole of what `DeviceOnly` grants.
            assert_eq!(
                attempt(
                    &backend,
                    &policy,
                    "write-dev-null",
                    Path::new("/dev/null"),
                    None
                ),
                Outcome::Allowed,
                "{:?} must keep the device sink writable",
                choice.mode
            );
            assert_eq!(
                choice.network,
                NetworkScope::Host,
                "no filesystem-only backend may claim network isolation"
            );
            assert_eq!(
                attempt(
                    &backend,
                    &policy,
                    "tcp-connect",
                    Path::new(&listener_address.to_string()),
                    None,
                ),
                Outcome::Allowed,
                "{:?} reports host networking, so loopback must still connect",
                choice.mode
            );
            let _ = fs::remove_file(&temp);
            let _ = fs::remove_dir(&temp_root);
        }
        assert_eq!(
            exercised, 3,
            "all three rows are selectable with a Seatbelt backend present"
        );
    }

    // ── Technique: link-based escapes ───────────────────────────────────────

    /// A symlink out of the workspace is followed to its target before the
    /// policy is applied, so it grants nothing. The mirror case — a symlink
    /// created from *inside* the sandbox — must also not become a foothold.
    #[test]
    fn a_symlink_pointing_out_of_the_workspace_grants_nothing_in_either_direction() {
        let arena = arena();
        let backend = backend();
        let policy = policy(SandboxMode::WorkspaceWrite, &arena.workspace);

        let escape = arena.workspace.join("escape-link");
        symlink(&arena.outside, &escape).expect("plant an escaping symlink");
        assert_eq!(
            attempt(
                &backend,
                &policy,
                "write",
                &escape.join("through.txt"),
                None
            ),
            Outcome::Denied,
            "writing through a symlink out of the workspace must be denied"
        );
        assert!(!arena.outside.join("through.txt").exists());

        let planted = arena.workspace.join("planted-link");
        assert_eq!(
            attempt(
                &backend,
                &policy,
                "symlink-create",
                &arena.outside.join("victim.txt"),
                Some(&planted),
            ),
            Outcome::Allowed,
            "creating a symlink inside the workspace is an ordinary workspace write"
        );
        assert_eq!(
            attempt(&backend, &policy, "write", &planted, None),
            Outcome::Denied,
            "a symlink created inside the sandbox must not become a write channel out of it"
        );
        assert!(!arena.outside.join("victim.txt").exists());
    }

    /// Creating a hard link to an outside file is refused from inside the
    /// sandbox — Seatbelt checks the source of the link, not only the
    /// destination directory.
    #[test]
    fn creating_a_hard_link_to_a_file_outside_the_workspace_is_denied() {
        let arena = arena();
        let backend = backend();
        let policy = policy(SandboxMode::WorkspaceWrite, &arena.workspace);
        let victim = arena.outside.join("secret.txt");
        fs::write(&victim, b"original").expect("seed the outside file");

        assert_eq!(
            attempt(
                &backend,
                &policy,
                "hardlink-create",
                &victim,
                Some(&arena.workspace.join("hard.txt")),
            ),
            Outcome::Denied,
            "linking an outside inode into the workspace must be denied"
        );
        assert!(!arena.workspace.join("hard.txt").exists());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "original");
    }

    /// QSEC03 finding S2. Seatbelt authorises by path, not by inode, so a hard
    /// link that is *already* inside the workspace when confinement starts is
    /// a writable alias for the file it points at, wherever that file lives.
    /// The fence cannot see it and does not stop it.
    #[test]
    fn a_preexisting_hard_link_inside_the_workspace_writes_through_to_the_outside_inode() {
        let arena = arena();
        let backend = backend();
        let policy = policy(SandboxMode::WorkspaceWrite, &arena.workspace);
        let victim = arena.outside.join("secret.txt");
        fs::write(&victim, b"original").expect("seed the outside file");
        let alias = arena.workspace.join("alias.txt");
        fs::hard_link(&victim, &alias).expect("plant the alias before confinement starts");

        let outcome = attempt(&backend, &policy, "write", &alias, None);
        assert_eq!(
            outcome,
            Outcome::Allowed,
            "the alias is no longer writable — the fence grew inode awareness and QSEC03 finding \
             S2 can be closed"
        );
        assert_eq!(
            fs::read_to_string(&victim).unwrap(),
            "probe",
            "writing the in-workspace alias changed the outside file: this is the finding"
        );
    }

    /// Ordinary parent traversal out of the workspace is resolved by the
    /// kernel before the profile matches, so it is denied.
    #[test]
    fn parent_traversal_out_of_the_workspace_is_denied_after_the_kernel_resolves_it() {
        let arena = arena();
        let backend = backend();
        let policy = policy(SandboxMode::WorkspaceWrite, &arena.workspace);
        let traversed = arena.workspace.join("..").join("outside").join("up.txt");
        assert_eq!(
            attempt(&backend, &policy, "write", &traversed, None),
            Outcome::Denied
        );
        assert!(!arena.outside.join("up.txt").exists());
    }

    // ── Technique: mutation classes beyond plain writes ─────────────────────

    /// The fence is a write fence, not a create fence: removal, renaming and
    /// permission changes outside the workspace must all be denied too.
    #[test]
    fn removal_renaming_and_permission_changes_outside_the_workspace_are_denied() {
        let arena = arena();
        let backend = backend();
        let policy = policy(SandboxMode::WorkspaceWrite, &arena.workspace);
        let victim = arena.outside.join("victim.txt");
        fs::write(&victim, b"original").expect("seed the outside file");

        assert_eq!(
            attempt(&backend, &policy, "unlink", &victim, None),
            Outcome::Denied
        );
        assert!(victim.exists(), "the outside file must survive");
        assert_eq!(
            attempt(
                &backend,
                &policy,
                "rename",
                &victim,
                Some(&arena.outside.join("moved.txt")),
            ),
            Outcome::Denied
        );
        assert!(victim.exists());
        assert_eq!(
            attempt(&backend, &policy, "chmod", &victim, None),
            Outcome::Denied
        );
        assert_eq!(
            attempt(
                &backend,
                &policy,
                "mkdir",
                &arena.outside.join("new-dir"),
                None
            ),
            Outcome::Denied
        );
        assert!(!arena.outside.join("new-dir").exists());
    }

    /// Read-only means every mutation class is denied, including inside the
    /// workspace — that is what makes `FileWriteScope::DeviceOnly` true.
    #[test]
    fn read_only_denies_every_mutation_class_even_inside_the_workspace() {
        let arena = arena();
        let backend = backend();
        let policy = policy(SandboxMode::ReadOnly, &arena.workspace);
        let existing = arena.workspace.join("existing.txt");
        fs::write(&existing, b"original").expect("seed a workspace file");

        for (kind, target, second) in [
            ("write", arena.workspace.join("new.txt"), None),
            ("write", existing.clone(), None),
            ("unlink", existing.clone(), None),
            ("mkdir", arena.workspace.join("new-dir"), None),
            ("chmod", existing.clone(), None),
            (
                "rename",
                existing.clone(),
                Some(arena.workspace.join("renamed.txt")),
            ),
            (
                "symlink-create",
                existing.clone(),
                Some(arena.workspace.join("link")),
            ),
        ] {
            assert_eq!(
                attempt(&backend, &policy, kind, &target, second.as_deref()),
                Outcome::Denied,
                "read-only must deny {kind} on {}",
                target.display()
            );
        }
        assert_eq!(fs::read_to_string(&existing).unwrap(), "original");
        assert!(!arena.workspace.join("new.txt").exists());
        assert!(!arena.workspace.join("new-dir").exists());
        // Reading is still permitted; a read-only sandbox that denied reads
        // would be a different, unreported guarantee.
        assert_eq!(
            attempt(&backend, &policy, "read", &existing, None),
            Outcome::Allowed
        );
    }

    /// Confinement is inherited: a process the confined child spawns is
    /// bounded by the same fence, so exec is not an escape.
    #[test]
    fn a_descendant_process_inherits_the_fence_and_cannot_write_outside_it() {
        let arena = arena();
        let backend = backend();
        let policy = policy(SandboxMode::WorkspaceWrite, &arena.workspace);
        let target = arena.outside.join("by-grandchild.txt");
        assert_eq!(
            attempt(&backend, &policy, "grandchild-write", &target, None),
            Outcome::Denied,
            "a grandchild must not be able to write where its parent cannot"
        );
        assert!(!target.exists());
    }

    /// A workspace root reached through a symlink is canonicalised before the
    /// profile is built, so the fence lands on the real directory and not on
    /// the link's own parent.
    #[test]
    fn a_symlinked_workspace_root_confines_to_the_resolved_directory() {
        let arena = arena();
        let backend = backend();
        let real = arena.workspace.join("real");
        fs::create_dir_all(&real).expect("create the real workspace");
        let link = arena.workspace.join("link-to-real");
        symlink(&real, &link).expect("create the workspace symlink");

        let policy = policy(SandboxMode::WorkspaceWrite, &link);
        assert_eq!(
            attempt(&backend, &policy, "write", &real.join("inside.txt"), None),
            Outcome::Allowed,
            "the resolved directory must be writable"
        );
        assert_eq!(
            attempt(
                &backend,
                &policy,
                "write",
                &arena.workspace.join("sibling.txt"),
                None,
            ),
            Outcome::Denied,
            "the link's parent must not become writable"
        );
        assert!(!arena.workspace.join("sibling.txt").exists());
    }

    /// QSEC03 finding S1, runtime half. A workspace root whose *name* would
    /// close the profile's `(subpath "…")` form must remain data and leave the
    /// outside write fence intact.
    #[test]
    fn qsec03_requirement_a_crafted_workspace_root_cannot_widen_the_write_fence_at_runtime() {
        let arena = arena();
        let backend = backend();
        let victim = arena.outside.join("secret.txt");
        fs::write(&victim, b"original").expect("seed the outside file");

        let benign = arena.workspace.join("ws");
        fs::create_dir_all(&benign).expect("create the benign root");
        assert_eq!(
            attempt(
                &backend,
                &policy(SandboxMode::WorkspaceWrite, &benign),
                "write",
                &victim,
                None,
            ),
            Outcome::Denied,
            "control: a benign root must deny the outside write"
        );

        let crafted = arena.workspace.join(format!(
            "ws\"))(allow file-write*)(allow file-write* (subpath \"{}/ws",
            arena.workspace.display()
        ));
        fs::create_dir_all(&crafted).expect("create the crafted root");
        assert_eq!(
            attempt(
                &backend,
                &policy(SandboxMode::WorkspaceWrite, &crafted),
                "write",
                &victim,
                None,
            ),
            Outcome::Denied,
            "a workspace root must never be able to widen the fence"
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "original");
    }

    /// The confined half of every case above. Returns silently when it is not
    /// being used as a probe so that an ordinary run is unaffected.
    #[test]
    fn escape_probe_child() {
        let Ok(kind) = std::env::var(PROBE_KIND) else {
            return;
        };
        let target = PathBuf::from(std::env::var(PROBE_TARGET).expect("probe target"));
        let second = std::env::var(PROBE_SECOND).ok().map(PathBuf::from);
        let result: std::io::Result<()> = match kind.as_str() {
            "read" => fs::read(&target).map(|_| ()),
            "write" => fs::write(&target, b"probe"),
            "write-dev-null" => fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .and_then(|mut sink| sink.write_all(b"probe")),
            "unlink" => fs::remove_file(&target),
            "mkdir" => fs::create_dir(&target),
            "chmod" => fs::set_permissions(&target, fs::Permissions::from_mode(0o600)),
            "rename" => fs::rename(&target, second.as_ref().expect("rename destination")),
            "hardlink-create" => fs::hard_link(&target, second.as_ref().expect("link destination")),
            "symlink-create" => symlink(&target, second.as_ref().expect("link destination")),
            "tcp-connect" => TcpStream::connect(target.to_string_lossy().as_ref()).map(|_| ()),
            "grandchild-write" => Command::new("/bin/dd")
                .arg("if=/dev/zero")
                .arg(format!("of={}", target.display()))
                .arg("bs=1")
                .arg("count=1")
                .output()
                .and_then(|output| {
                    if output.status.success() {
                        Ok(())
                    } else {
                        Err(std::io::Error::other("grandchild write failed"))
                    }
                }),
            other => panic!("unknown escape probe kind {other}"),
        };

        let (marker, status) = if result.is_ok() {
            (PROBE_OK, 0)
        } else {
            (PROBE_DENIED, DENIED_EXIT)
        };
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{marker}:{kind}").expect("write the probe marker");
        stdout.flush().expect("flush the probe marker");
        std::process::exit(status);
    }
}

/// Cells this suite cannot exercise away from macOS. Named so the matrix is
/// explicit that they are Unknown rather than passing.
#[cfg(not(target_os = "macos"))]
#[test]
#[allow(clippy::print_stderr)]
fn seatbelt_escape_cells_are_unknown_off_macos() {
    eprintln!(
        "SKIP: Seatbelt escape cells (profile injection, hard-link alias, symlink escape, \
         descendant inheritance, report-versus-enforcement) require target_os=macos"
    );
}

/// Landlock and bubblewrap enforcement is not exercised by this suite on any
/// host; only their profile construction is, in `src/escape_matrix.rs`.
#[test]
#[allow(clippy::print_stderr)]
fn linux_backend_runtime_escape_cells_are_not_exercised_by_this_suite() {
    if cfg!(target_os = "linux") {
        eprintln!(
            "SKIP: Landlock/bwrap runtime escape cells are covered by it::linux_runtime; this \
             suite exercises their profile construction only"
        );
    }
}

//! heycode-sandbox — process confinement via OS primitives.
//!
//! macOS ships a **Seatbelt** provider wrapping the whole bash invocation
//! with `sandbox-exec -p <profile> -- …`. Profiles derive from ONE home for
//! "what workspace-write means" so the fence and any future executors cannot
//! drift. Workspace-write roots are encoded before they enter a profile,
//! JSON, or argv string boundary; unsupported control-bearing Seatbelt roots
//! and non-UTF-8 roots fail closed instead of being rewritten. Linux
//! (Landlock/bwrap) follows the same trait; requesting them on an unsupported
//! platform fails loud at composition.
//!
//! Seatbelt remains path-authorized: a pre-existing hard link inside the
//! workspace can alias an inode outside it. The QSEC03 runtime matrix retains
//! that limitation explicitly rather than claiming inode confinement.

use std::sync::Arc;

use heycode_core::{Context, CoreResult, Plugin};
mod landlock;

/// QSEC03 adversarial profile-shape matrix; see `escape_matrix.rs`.
#[cfg(test)]
#[path = "escape_matrix.rs"]
mod escape_matrix;

pub use heycode_exec::SERVICE_SANDBOX;
pub use landlock::{LandlockRules, LandlockSandbox};

/// Apply a ruleset to the current process then exec `argv` (Linux only).
///
/// # Errors
/// Kernel/ABI/path/exec failures surface verbatim.
#[cfg(target_os = "linux")]
pub fn apply_landlock(rules: &LandlockRules, argv: &[String]) -> Result<(), String> {
    landlock::linux_impl::apply_and_exec(rules, argv).map_err(|e| e.to_string())
}

/// Non-Linux placeholder that always fails.
#[cfg(not(target_os = "linux"))]
pub fn apply_landlock(_rules: &LandlockRules, _argv: &[String]) -> Result<(), String> {
    Err("landlock is Linux-only".to_owned())
}

use heycode_exec::{
    Sandbox, SandboxBackendCapabilities, SandboxError, SandboxMode, SandboxPolicy, SandboxService,
    SandboxSupport,
};

/// macOS Seatbelt (`sandbox-exec`) backend.
pub struct SeatbeltSandbox {
    /// Absolute path of `sandbox-exec`, resolved once.
    binary: std::path::PathBuf,
}

impl SeatbeltSandbox {
    /// Resolve `sandbox-exec` from PATH; fails loud with install guidance.
    ///
    /// # Errors
    /// [`SandboxError`] when the binary is absent or unusable.
    pub fn new() -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            return Err(SandboxError::new(
                "Seatbelt sandbox is macOS-only; on Linux use Landlock/bwrap providers",
            ));
        }
        let bin = which("sandbox-exec").ok_or_else(|| {
            SandboxError::new(
                "sandbox-exec not found in PATH — it ships with macOS; \
                 ensure /usr/bin is on PATH",
            )
        })?;
        Ok(Self { binary: bin })
    }

    fn profile(&self, policy: &SandboxPolicy) -> Result<String, SandboxError> {
        match policy.mode {
            SandboxMode::Off => Ok("(version 1)(allow default)".to_owned()),
            SandboxMode::ReadOnly => Ok("(version 1)(allow default)(deny file-write*)(allow file-write* (literal \"/dev/null\"))".to_string()),
            SandboxMode::WorkspaceWrite => {
                let root = seatbelt_path_literal(&policy.workspace_root)?;
                Ok(format!(
                "(version 1)(allow default)(deny file-write*)\
                 (allow file-write* (literal \"/dev/null\"))\
                 (allow file-write* (subpath {root}))\
                 (allow file-write* (subpath \"/private/tmp\"))\
                 (allow file-write* (subpath \"/tmp\"))"
                ))
            }
        }
    }
}

fn seatbelt_path_literal(path: &std::path::Path) -> Result<String, SandboxError> {
    let value = utf8_workspace_root(path)?;
    if value.chars().any(char::is_control) {
        return Err(SandboxError::new(
            "workspace root contains a control character unsupported by Seatbelt profiles",
        ));
    }
    let mut literal = String::with_capacity(value.len().saturating_add(2));
    literal.push('"');
    for character in value.chars() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            other => literal.push(other),
        }
    }
    literal.push('"');
    Ok(literal)
}

fn utf8_workspace_root(path: &std::path::Path) -> Result<&str, SandboxError> {
    path.to_str().ok_or_else(|| {
        SandboxError::new("sandbox workspace root is not representable without data loss")
    })
}

impl Sandbox for SeatbeltSandbox {
    fn name(&self) -> &'static str {
        "seatbelt"
    }

    fn capabilities(&self) -> SandboxBackendCapabilities {
        filesystem_only_capabilities()
    }

    fn confine(
        &self,
        argv: &[String],
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        if argv.is_empty() {
            return Err(SandboxError::new("cannot confine an empty argv"));
        }
        // Seatbelt matches RESOLVED paths: /var is a symlink to /private/var,
        // so the policy must speak canonical paths or writes "escape" on paper.
        let policy = SandboxPolicy {
            mode: policy.mode,
            workspace_root: std::fs::canonicalize(&policy.workspace_root)
                .unwrap_or_else(|_| policy.workspace_root.clone()),
        };
        let mut out = vec![
            self.binary.display().to_string(),
            "-p".to_owned(),
            self.profile(&policy)?,
        ];
        // `--` ends seatbench flags: everything after is the child command verbatim.
        out.push("--".into());
        out.extend(argv.iter().cloned());
        Ok(out)
    }
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Platform default chain, mirroring dsh: macOS → Seatbelt, Linux → bwrap.
///
/// # Errors
/// Propagates the selected provider's resolution failure.
pub fn platform_default() -> Result<Arc<dyn Sandbox>, SandboxError> {
    if cfg!(target_os = "macos") {
        Ok(Arc::new(SeatbeltSandbox::new()?))
    } else if cfg!(target_os = "linux") {
        match LandlockSandbox::new() {
            Ok(l) => Ok(Arc::new(l)),
            Err(_) => Ok(Arc::new(BwrapSandbox::new()?)),
        }
    } else {
        Err(SandboxError::new(
            "no sandbox provider for this platform (supported: macOS Seatbelt, Linux bubblewrap)",
        ))
    }
}

/// Provide service `"sandbox"` from an already-resolved effective policy.
///
/// The composition root resolves the backend once from `[sandbox] mode` and
/// hands the same `Arc` here and to the `ToolCtx` that `bash` receives — two
/// independently-resolved backends would be two sources of truth.
#[must_use]
pub fn sandbox_plugin(service: SandboxService) -> Box<dyn Plugin> {
    struct SandboxPlugin(SandboxService);
    impl Plugin for SandboxPlugin {
        fn name(&self) -> &'static str {
            "sandbox"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "sandbox",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SANDBOX]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            ctx.provide(SERVICE_SANDBOX, "sandbox", self.0.clone())
        }
    }
    Box::new(SandboxPlugin(service))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn backend_or_skip() -> Option<SeatbeltSandbox> {
        SeatbeltSandbox::new().ok()
    }

    #[test]
    fn every_backend_reports_filesystem_modes_without_network_isolation() {
        let seatbelt = SeatbeltSandbox {
            binary: std::path::PathBuf::from("/usr/bin/sandbox-exec"),
        };
        let bwrap = BwrapSandbox {
            binary: std::path::PathBuf::from("/usr/bin/bwrap"),
        };
        let landlock = LandlockSandbox;
        for backend in [&seatbelt as &dyn Sandbox, &bwrap, &landlock] {
            let capabilities = backend.capabilities();
            assert_eq!(capabilities.read_only, SandboxSupport::Supported);
            assert_eq!(capabilities.workspace_write, SandboxSupport::Supported);
            assert_eq!(capabilities.network_isolation, SandboxSupport::Unsupported);
        }
    }

    #[test]
    fn wraps_argv_with_profile_and_separator() {
        let Some(sb) = backend_or_skip() else { return };
        let policy = SandboxPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: std::path::PathBuf::from("/tmp/ws"),
        };
        let argv = sb
            .confine(&["bash".into(), "-c".into(), "echo hi".into()], &policy)
            .unwrap();
        assert_eq!(argv[0], sb.binary.display().to_string());
        assert_eq!(argv[1], "-p");
        assert!(argv[2].contains("(deny file-write*)"));
        assert!(argv[2].contains("/tmp/ws"));
        assert_eq!(argv[3], "--");
        assert_eq!(&argv[4..], &["bash", "-c", "echo hi"]);
    }

    #[test]
    fn readonly_profile_has_no_workspace_write() {
        let Some(sb) = backend_or_skip() else { return };
        let policy = SandboxPolicy {
            mode: SandboxMode::ReadOnly,
            workspace_root: std::path::PathBuf::from("/tmp/ws"),
        };
        let argv = sb.confine(&["true".into()], &policy).unwrap();
        assert!(!argv[2].contains("subpath \"/tmp/ws\""), "{}", argv[2]);
    }

    #[tokio::test]
    async fn confined_echo_runs_and_writes_outside_are_denied() {
        let Some(sb) = backend_or_skip() else { return };
        let ws = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: ws.path().to_path_buf(),
        };

        // Sanity: reading works under confinement.
        let argv = sb.confine(&["echo".into(), "ok".into()], &policy).unwrap();
        let out = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");

        // Writing INSIDE the workspace is allowed.
        let inside = ws.path().join("w.txt");
        let script = format!("printf x > {}", inside.display());
        let argv = sb
            .confine(&["bash".into(), "-c".into(), script], &policy)
            .unwrap();
        let out = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(inside.exists());

        // Writing OUTSIDE is denied by the profile.
        let outside = std::env::temp_dir().join(format!("heycode-sb-deny-{}", std::process::id()));
        let _ = std::fs::remove_file(&outside);
        let script = format!("printf x > {}", outside.display());
        let argv = sb
            .confine(&["bash".into(), "-c".into(), script], &policy)
            .unwrap();
        let out = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .await
            .unwrap();
        assert!(!out.status.success(), "write outside workspace must fail");
        assert!(!outside.exists());
        let _ = std::fs::remove_file(&outside);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod cwd_repro {
    use super::*;

    #[tokio::test]
    async fn mirrors_bash_tool_mechanics_exactly() {
        let Some(sb) = SeatbeltSandbox::new().ok() else {
            return;
        };
        let ws = tempfile::tempdir().unwrap();
        let target = ws.path().join("w.txt");
        let policy = SandboxPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: ws.path().to_path_buf(),
        };
        let script = format!("printf x > {}", target.display());
        let argv = sb
            .confine(&["bash".into(), "-c".into(), script], &policy)
            .unwrap();
        let out = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(ws.path()) // ← the one difference
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "x");
    }
}

// ─── Bubblewrap (Linux) ─────────────────────────────────────────────────────

/// Linux bubblewrap (`bwrap`) backend: read-only root bind, private /tmp under
/// workspace-write, PID namespace so magic links can't escape, die-with-parent
/// so children never outlive the harness.
pub struct BwrapSandbox {
    binary: std::path::PathBuf,
}

impl BwrapSandbox {
    /// Resolve `bwrap` from PATH.
    ///
    /// # Errors
    /// [`SandboxError`] when absent (install guidance) or non-Linux.
    pub fn new() -> Result<Self, SandboxError> {
        if !cfg!(target_os = "linux") {
            return Err(SandboxError::new(
                "bubblewrap sandbox is Linux-only; on macOS use the Seatbelt provider",
            ));
        }
        let bin = which("bwrap").ok_or_else(|| {
            SandboxError::new(
                "bwrap not found in PATH — install bubblewrap (apt install bubblewrap)",
            )
        })?;
        Ok(Self { binary: bin })
    }

    fn argv(&self, argv: &[String], policy: &SandboxPolicy, root: &str) -> Vec<String> {
        let mut out = vec![
            self.binary.display().to_string(),
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--dev".into(),
            "/dev".into(),
            "--proc".into(),
            "/proc".into(),
            "--unshare-pid".into(),
            "--die-with-parent".into(),
        ];
        if policy.mode == SandboxMode::WorkspaceWrite {
            // Ephemeral /tmp plus a writable bind of the workspace over itself.
            out.extend([
                "--tmpfs".into(),
                "/tmp".into(),
                "--bind".into(),
                root.to_owned(),
                root.to_owned(),
            ]);
        }
        out.push("--".into());
        out.extend(argv.iter().cloned());
        out
    }
}

impl Sandbox for BwrapSandbox {
    fn name(&self) -> &'static str {
        "bwrap"
    }

    fn capabilities(&self) -> SandboxBackendCapabilities {
        filesystem_only_capabilities()
    }

    fn confine(
        &self,
        argv: &[String],
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        if argv.is_empty() {
            return Err(SandboxError::new("cannot confine an empty argv"));
        }
        // bwrap resolves paths literally; canonicalize the workspace like the
        // Seatbelt path does.
        let policy = SandboxPolicy {
            mode: policy.mode,
            workspace_root: std::fs::canonicalize(&policy.workspace_root)
                .unwrap_or_else(|_| policy.workspace_root.clone()),
        };
        let root = if policy.mode == SandboxMode::WorkspaceWrite {
            utf8_workspace_root(&policy.workspace_root)?
        } else {
            ""
        };
        Ok(self.argv(argv, &policy, root))
    }
}

fn filesystem_only_capabilities() -> SandboxBackendCapabilities {
    SandboxBackendCapabilities {
        read_only: SandboxSupport::Supported,
        workspace_write: SandboxSupport::Supported,
        network_isolation: SandboxSupport::Unsupported,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod bwrap_tests {
    use super::*;

    #[test]
    fn shape_matches_the_dsh_wrap_contract() {
        if !cfg!(target_os = "linux") {
            assert!(BwrapSandbox::new().is_err());
            return;
        }
        let sb = BwrapSandbox::new().unwrap();
        let policy = SandboxPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: std::path::PathBuf::from("/tmp/ws"),
        };
        let argv = sb
            .confine(&["bash".into(), "-c".into(), "echo".into()], &policy)
            .unwrap();
        assert_eq!(
            &argv[..10],
            &[
                "bwrap",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--unshare-pid",
                "--die-with-parent",
            ]
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--bind" && w[1].contains("/tmp/ws"))
        );
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[sep + 1..], &["bash", "-c", "echo"]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod landlock_shape_tests {
    use super::*;
    use crate::landlock::{LandlockSandbox, profile_for};

    #[test]
    fn profile_readonly_uses_exact_landlock_v1_uapi_bits() {
        let policy = SandboxPolicy {
            mode: SandboxMode::ReadOnly,
            workspace_root: std::path::PathBuf::from("/tmp/ws"),
        };
        let rules = profile_for(&policy).expect("build read-only Landlock profile");
        const EXECUTE: u64 = 1 << 0;
        const WRITE_FILE: u64 = 1 << 1;
        const READ_FILE: u64 = 1 << 2;
        const READ_DIR: u64 = 1 << 3;
        const ALL_V1: u64 = (1 << 13) - 1;
        assert_eq!(rules.handled_access, ALL_V1);
        let world = rules
            .grants
            .iter()
            .find(|g| g.path == "/")
            .expect("world read");
        assert_eq!(world.access, EXECUTE | READ_FILE | READ_DIR);
        let dev_null = rules
            .grants
            .iter()
            .find(|grant| grant.path == "/dev/null")
            .expect("device sink grant");
        assert_eq!(dev_null.access, WRITE_FILE);
        assert_eq!(rules.grants.iter().filter(|g| g.path == "/tmp").count(), 0);
    }

    #[test]
    fn profile_workspace_write_grants_root_and_tmp() {
        let policy = SandboxPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: std::path::PathBuf::from("/tmp/ws"),
        };
        let rules = profile_for(&policy).expect("build workspace-write Landlock profile");
        let workspace = rules
            .grants
            .iter()
            .find(|grant| grant.path.contains("/tmp/ws"))
            .expect("workspace grant");
        assert_eq!(workspace.access, (1 << 13) - 1);
        assert_eq!(rules.grants.iter().filter(|g| g.path == "/tmp").count(), 1);
        assert_eq!(
            rules
                .grants
                .iter()
                .filter(|grant| grant.path == "/private/tmp")
                .count(),
            usize::from(std::path::Path::new("/private/tmp").is_dir())
        );
        const WRITE_FILE: u64 = 1 << 1;
        let ws = rules
            .grants
            .iter()
            .find(|g| g.path.contains("/tmp/ws"))
            .unwrap();
        assert_ne!(ws.access & WRITE_FILE, 0);
    }

    /// The argv-wrapping half is pure and must be verified on EVERY host.
    /// Hiding it behind `cfg(target_os = "linux")` let a use of an
    /// out-of-scope `argv` binding sit in the tree uncompiled.
    #[test]
    fn launcher_argv_wraps_with_hidden_subcommand() {
        let sb = LandlockSandbox;
        let policy = SandboxPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: std::path::PathBuf::from("/tmp/ws"),
        };
        let argv = sb
            .confine(&["bash".into(), "-c".into(), "echo".into()], &policy)
            .unwrap();
        assert_eq!(argv[1], "__landlock");
        assert!(argv[2].contains("grants"));
        assert_eq!(argv[3], "--");
        assert_eq!(&argv[4..], &["bash", "-c", "echo"]);
    }
}

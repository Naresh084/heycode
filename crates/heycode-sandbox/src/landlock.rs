//! Landlock provider (Linux ≥ 5.13, kernel ABI v1+).
//!
//! Wrap model identical to Seatbelt/bwrap: `confine()` returns THE ARGV to
//! spawn — here `[heycode, "__landlock", <rules-json>, "--", ...argv]`. The
//! hidden `__landlock` subcommand applies the ruleset to ITSELF then
//! `exec`s the wrapped command (ruleset survives execve; the whole child
//! tree is confined while heycode stays unrestricted).
//!
//! Only the syscall/exec island is Linux-gated; every host compiles and tests
//! the policy and launcher shape.

use heycode_exec::{
    Sandbox, SandboxBackendCapabilities, SandboxError, SandboxMode, SandboxPolicy, SandboxSupport,
};

// Linux UAPI values through Landlock ABI v1. Keep these explicit so the
// serialized launcher contract remains platform-independent and shape-tested
// even when this crate is compiled away from Linux.
const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_CHAR: u64 = 1 << 6;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_SOCK: u64 = 1 << 9;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_BLOCK: u64 = 1 << 11;
const MAKE_SYM: u64 = 1 << 12;
const READ_EXECUTE_V1: u64 = EXECUTE | READ_FILE | READ_DIR;
const WRITE_V1: u64 = WRITE_FILE
    | REMOVE_DIR
    | REMOVE_FILE
    | MAKE_CHAR
    | MAKE_DIR
    | MAKE_REG
    | MAKE_SOCK
    | MAKE_FIFO
    | MAKE_BLOCK
    | MAKE_SYM;
const ALL_V1: u64 = READ_EXECUTE_V1 | WRITE_V1;

/// One filesystem grant.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Grant {
    /// Directory/file that may be accessed.
    pub path: String,
    /// Hex bitmask of access rights (opaque outside Linux builds).
    pub access: u64,
}

/// The full ruleset handed to the launcher subcommand.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LandlockRules {
    /// ABI-v1 baseline rights that every accepted launcher must handle.
    ///
    /// The Linux launcher expands these categories with typed rights supported
    /// by newer, tested Landlock ABIs before it creates the live ruleset.
    pub handled_access: u64,
    /// Grants applied on top of "no filesystem access".
    pub grants: Vec<Grant>,
}

/// Build the ruleset for a policy. Platform-independent so the argv shape is
/// testable everywhere.
///
/// # Errors
/// A workspace-write root that cannot cross the JSON/argv boundary losslessly
/// is refused.
pub fn profile_for(policy: &SandboxPolicy) -> Result<LandlockRules, SandboxError> {
    let mut grants = vec![
        // Execute and read everything. Without EXECUTE the launcher could
        // install a ruleset but never exec the requested program.
        Grant {
            path: "/".into(),
            access: READ_EXECUTE_V1,
        },
        // `/dev/null` needs only file-write permission. Directory creation
        // rights are invalid for a file-backed PathBeneath rule.
        Grant {
            path: "/dev/null".into(),
            access: WRITE_FILE,
        },
    ];
    if policy.mode == SandboxMode::WorkspaceWrite {
        let workspace_root = policy.workspace_root.to_str().ok_or_else(|| {
            SandboxError::new("sandbox workspace root is not representable without data loss")
        })?;
        grants.push(Grant {
            path: workspace_root.to_owned(),
            access: ALL_V1,
        });
        // Keep provider temp roots in fixed order, but never put a path that
        // does not exist into the launcher: PathFd would fail the whole launch.
        // `/private/tmp` is common on macOS and uncommon on Linux.
        for path in ["/tmp", "/private/tmp"] {
            if std::path::Path::new(path).is_dir() && !grants.iter().any(|grant| grant.path == path)
            {
                grants.push(Grant {
                    path: path.to_owned(),
                    access: ALL_V1,
                });
            }
        }
    }
    Ok(LandlockRules {
        handled_access: ALL_V1,
        grants,
    })
}

/// Build the launcher argv: `[heycode, "__landlock", <rules-json>, "--", ...argv]`.
///
/// Platform-independent on purpose — this is pure argv construction, and
/// keeping it out of the `cfg(target_os = "linux")` island is what makes it
/// compile and test on every host.
///
/// # Errors
/// Empty argv, an unresolvable current executable, or rules that will not
/// serialize.
pub fn launcher_argv(rules: &LandlockRules, argv: &[String]) -> Result<Vec<String>, SandboxError> {
    if argv.is_empty() {
        return Err(SandboxError::new("cannot confine an empty argv"));
    }
    let exe =
        std::env::current_exe().map_err(|e| SandboxError::new(format!("current_exe: {e}")))?;
    let exe = exe.to_str().ok_or_else(|| {
        SandboxError::new("current executable is not representable without data loss")
    })?;
    let mut out = vec![
        exe.to_owned(),
        "__landlock".to_owned(),
        serde_json::to_string(rules)
            .map_err(|e| SandboxError::new(format!("rules serialize: {e}")))?,
        "--".into(),
    ];
    out.extend(argv.iter().cloned());
    Ok(out)
}

/// Linux backend applying the ruleset to the current process.
#[cfg(target_os = "linux")]
pub mod linux_impl {
    use super::*;

    use landlock::{
        ABI, Access, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
        RulesetAttr, RulesetCreatedAttr as _, RulesetStatus,
    };

    fn access_for_grant(raw: u64) -> Result<BitFlags<AccessFs>, SandboxError> {
        match raw {
            READ_EXECUTE_V1 => Ok(AccessFs::from_read(ABI::V8)),
            WRITE_FILE => Ok(AccessFs::WriteFile.into()),
            ALL_V1 => Ok(AccessFs::from_all(ABI::V8)),
            _ => Err(SandboxError::new("unsupported Landlock grant access")),
        }
    }

    /// Verify that the running kernel can enforce the complete ABI-v1
    /// filesystem baseline without restricting the calling process.
    ///
    /// # Errors
    /// Missing or disabled Landlock support fails closed.
    pub fn preflight() -> Result<(), SandboxError> {
        let _created = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V1))
            .and_then(|ruleset| ruleset.create())
            .map_err(|error| SandboxError::new(format!("Landlock ABI-v1 preflight: {error}")))?;
        Ok(())
    }

    /// Apply a ruleset to the CURRENT process (launcher side).
    ///
    /// # Errors
    /// Kernel too old / unsupported access / bad path fd.
    pub fn apply_and_exec(rules: &LandlockRules, argv: &[String]) -> Result<(), SandboxError> {
        if argv.is_empty() {
            return Err(SandboxError::new("empty exec argv"));
        }
        if rules.handled_access != ALL_V1 || rules.grants.is_empty() {
            return Err(SandboxError::new("invalid Landlock rules baseline"));
        }

        // ABI v1 is a hard minimum. Rights introduced by newer ABIs are
        // requested through the crate's typed vocabulary and filtered by its
        // compatibility layer, preventing truncate/refer/device gaps on newer
        // kernels without dropping support for Linux 5.13. ABI v9's
        // `ResolveUnix` is deliberately excluded: this provider reports host
        // networking, so it must not silently restrict pathname socket use.
        let mut created = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V1))
            .map(|ruleset| ruleset.set_compatibility(CompatLevel::BestEffort))
            .and_then(|ruleset| ruleset.handle_access(AccessFs::from_all(ABI::V8)))
            .and_then(|ruleset| ruleset.create())
            .map_err(|e| SandboxError::new(format!("ruleset create: {e}")))?;

        for grant in &rules.grants {
            let access = access_for_grant(grant.access)?;
            let fd = PathFd::new(&grant.path)
                .map_err(|e| SandboxError::new(format!("open {}: {e}", grant.path)))?;
            created = created
                .add_rule(PathBeneath::new(fd, access))
                .map_err(|e| SandboxError::new(format!("add rule {}: {e}", grant.path)))?;
        }

        let status = created
            .restrict_self()
            .map_err(|e| SandboxError::new(format!("restrict: {e}")))?;
        if status.ruleset == RulesetStatus::NotEnforced || !status.no_new_privs {
            return Err(SandboxError::new(
                "Landlock ruleset did not reach an enforced state",
            ));
        }

        // Ruleset survives execve: whole child tree inherits confinement.
        let Some((program, rest)) = argv.split_first() else {
            return Err(SandboxError::new("empty argv: nothing to exec"));
        };
        use std::os::unix::process::CommandExt as _;
        let mut command = std::process::Command::new(program);
        command.args(rest);
        let error = command.exec();
        Err(SandboxError::new(format!("exec {program}: {error}")))
    }
}

/// Cross-platform facade implementing the [`Sandbox`] trait.
pub struct LandlockSandbox;

impl LandlockSandbox {
    /// Construct (Linux-only; other platforms fail loud at composition).
    ///
    /// # Errors
    /// A non-Linux platform or a Linux kernel that cannot enforce the ABI-v1
    /// filesystem baseline fails closed.
    pub fn new() -> Result<Self, SandboxError> {
        if !cfg!(target_os = "linux") {
            return Err(SandboxError::new(
                "landlock sandbox is Linux-only (kernel ≥5.13); \
                 on macOS use the Seatbelt provider",
            ));
        }
        #[cfg(target_os = "linux")]
        linux_impl::preflight()?;
        Ok(Self)
    }
}

impl Sandbox for LandlockSandbox {
    fn name(&self) -> &'static str {
        "landlock"
    }

    fn capabilities(&self) -> SandboxBackendCapabilities {
        SandboxBackendCapabilities {
            read_only: SandboxSupport::Supported,
            workspace_write: SandboxSupport::Supported,
            network_isolation: SandboxSupport::Unsupported,
        }
    }

    fn confine(
        &self,
        argv: &[String],
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        // Canonicalize like every provider so grants match resolved paths.
        let policy = SandboxPolicy {
            mode: policy.mode,
            workspace_root: std::fs::canonicalize(&policy.workspace_root)
                .unwrap_or_else(|_| policy.workspace_root.clone()),
        };
        let rules = profile_for(&policy)?;
        // One path on every host: `LandlockSandbox::new()` is what refuses
        // non-Linux, so `confine` stays pure argv construction and stays
        // compiled everywhere.
        launcher_argv(&rules, argv)
    }
}

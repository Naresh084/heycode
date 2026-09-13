//! Runtime evidence for the filesystem-only Linux Landlock and bubblewrap backends.
//!
//! Every expected denial comes from a probe that successfully entered the
//! backend and reported its own filesystem result. A launcher failure has a
//! distinct marker and exit code, so CI cannot mistake an unavailable backend
//! for confinement.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(not(target_os = "linux"))]
#[test]
#[allow(clippy::print_stderr)]
fn linux_sandbox_runtime_matrix_is_linux_only() {
    eprintln!("SKIP: Landlock/bubblewrap runtime evidence requires target_os=linux");
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::fs::symlink;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use heycode_exec::{Sandbox, SandboxMode, SandboxPolicy, SandboxSupport};
    use heycode_sandbox::{
        BwrapSandbox, LandlockRules, LandlockSandbox, apply_landlock, platform_default,
    };

    const PROBE_KIND: &str = "HEYCODE_LINUX_SANDBOX_PROBE_KIND";
    const PROBE_TARGET: &str = "HEYCODE_LINUX_SANDBOX_PROBE_TARGET";
    const LANDLOCK_RULES: &str = "HEYCODE_LINUX_LANDLOCK_RULES";
    const PROBE_OK: &str = "HEYCODE_LINUX_SANDBOX_PROBE_OK";
    const PROBE_DENIED: &str = "HEYCODE_LINUX_SANDBOX_PROBE_DENIED";
    const TREE_CONFINED: &str = "HEYCODE_LINUX_SANDBOX_TREE_CONFINED";
    const TREE_ESCAPED: &str = "HEYCODE_LINUX_SANDBOX_TREE_ESCAPED";
    const LAUNCHER_ERROR: &str = "HEYCODE_LINUX_SANDBOX_LAUNCHER_ERROR";
    const DENIED_EXIT: i32 = 73;
    const LAUNCHER_EXIT: i32 = 74;
    const TREE_ESCAPE_EXIT: i32 = 75;

    #[derive(Clone, Copy, Debug)]
    enum ProbeKind {
        Read,
        Write,
        TcpConnect,
        UnixConnect,
        DescendantWrite,
    }

    impl ProbeKind {
        fn as_str(self) -> &'static str {
            match self {
                Self::Read => "read",
                Self::Write => "write",
                Self::TcpConnect => "tcp-connect",
                Self::UnixConnect => "unix-connect",
                Self::DescendantWrite => "descendant-write",
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Expected {
        Allowed,
        Denied,
        TreeConfined,
    }

    fn policy(mode: SandboxMode, workspace: &Path) -> SandboxPolicy {
        SandboxPolicy {
            mode,
            workspace_root: workspace.to_path_buf(),
        }
    }

    fn probe_argv() -> Vec<String> {
        vec![
            std::env::current_exe()
                .expect("resolve integration-test binary")
                .display()
                .to_string(),
            "--exact".to_owned(),
            crate::it::test_name(module_path!(), "sandbox_probe_child"),
            "--nocapture".to_owned(),
        ]
    }

    fn execute(confined: &[String], kind: ProbeKind, target: &Path) -> Output {
        Command::new(&confined[0])
            .args(&confined[1..])
            .env_clear()
            .env(PROBE_KIND, kind.as_str())
            .env(PROBE_TARGET, target)
            .output()
            .expect("start confined Linux probe")
    }

    fn execute_landlock(
        backend: &LandlockSandbox,
        sandbox_policy: &SandboxPolicy,
        kind: ProbeKind,
        target: &Path,
    ) -> Output {
        let wrapped = backend
            .confine(&probe_argv(), sandbox_policy)
            .expect("construct Landlock launcher argv");
        assert_eq!(wrapped.get(1).map(String::as_str), Some("__landlock"));
        let rules = wrapped
            .get(2)
            .expect("Landlock launcher carries serialized rules");
        let test_executable = std::env::current_exe().expect("resolve integration-test binary");
        Command::new(test_executable)
            .args([
                "--exact".to_owned(),
                crate::it::test_name(module_path!(), "landlock_launcher_child"),
                "--nocapture".to_owned(),
            ])
            .env_clear()
            .env(PROBE_KIND, kind.as_str())
            .env(PROBE_TARGET, target)
            .env(LANDLOCK_RULES, rules)
            .output()
            .expect("start Landlock launcher probe")
    }

    fn execute_bwrap(
        backend: &BwrapSandbox,
        sandbox_policy: &SandboxPolicy,
        kind: ProbeKind,
        target: &Path,
    ) -> Output {
        let confined = backend
            .confine(&probe_argv(), sandbox_policy)
            .expect("construct bubblewrap argv");
        execute(&confined, kind, target)
    }

    fn assert_probe(backend: &str, kind: ProbeKind, expected: Expected, output: &Output) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stdout.contains(LAUNCHER_ERROR) && output.status.code() != Some(LAUNCHER_EXIT),
            "{backend} launcher failed before the probe; status={:?}; stdout={stdout:?}; stderr={stderr:?}",
            output.status.code()
        );
        match expected {
            Expected::Allowed => assert!(
                output.status.success() && stdout.contains(PROBE_OK),
                "{backend} {} probe should be allowed; status={:?}; stdout={stdout:?}; stderr={stderr:?}",
                kind.as_str(),
                output.status.code()
            ),
            Expected::Denied => assert!(
                output.status.code() == Some(DENIED_EXIT) && stdout.contains(PROBE_DENIED),
                "{backend} {} probe should be denied after launch; status={:?}; stdout={stdout:?}; stderr={stderr:?}",
                kind.as_str(),
                output.status.code()
            ),
            Expected::TreeConfined => assert!(
                output.status.success() && stdout.contains(TREE_CONFINED),
                "{backend} descendant should inherit confinement; status={:?}; stdout={stdout:?}; stderr={stderr:?}",
                output.status.code()
            ),
        }
    }

    fn workspace_boundary(label: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let current = std::env::current_dir()
            .expect("read current directory")
            .canonicalize()
            .expect("canonicalize current directory");
        let boundary = tempfile::Builder::new()
            .prefix(&format!(".heycode-e11-{label}-"))
            .tempdir_in(current)
            .expect("create boundary outside provider temp grants");
        let workspace = boundary.path().join("workspace");
        let outside = boundary.path().join("outside");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&outside).expect("create outside directory");
        (boundary, workspace, outside)
    }

    fn run_matrix(
        backend_name: &str,
        run: impl Fn(&SandboxPolicy, ProbeKind, &Path) -> Output,
        label: &str,
    ) {
        let (_boundary, workspace, outside) = workspace_boundary(label);
        let readable = outside.join("readable.txt");
        fs::write(&readable, b"readable").expect("seed readable file");

        let read_only = policy(SandboxMode::ReadOnly, &workspace);
        let output = run(&read_only, ProbeKind::Read, &readable);
        assert_probe(backend_name, ProbeKind::Read, Expected::Allowed, &output);
        let readonly_inside = workspace.join("readonly-inside.txt");
        let output = run(&read_only, ProbeKind::Write, &readonly_inside);
        assert_probe(backend_name, ProbeKind::Write, Expected::Denied, &output);
        assert!(!readonly_inside.exists());
        let readonly_outside = outside.join("readonly-outside.txt");
        let output = run(&read_only, ProbeKind::Write, &readonly_outside);
        assert_probe(backend_name, ProbeKind::Write, Expected::Denied, &output);
        assert!(!readonly_outside.exists());

        let workspace_write = policy(SandboxMode::WorkspaceWrite, &workspace);
        let output = run(&workspace_write, ProbeKind::Read, &readable);
        assert_probe(backend_name, ProbeKind::Read, Expected::Allowed, &output);
        let inside = workspace.join("inside.txt");
        let output = run(&workspace_write, ProbeKind::Write, &inside);
        assert_probe(backend_name, ProbeKind::Write, Expected::Allowed, &output);
        assert_eq!(fs::read_to_string(&inside).unwrap(), "probe");

        let outside_write = outside.join("outside.txt");
        let output = run(&workspace_write, ProbeKind::Write, &outside_write);
        assert_probe(backend_name, ProbeKind::Write, Expected::Denied, &output);
        assert!(!outside_write.exists());

        let outside_link = workspace.join("outside-link");
        symlink(&outside, &outside_link).expect("create workspace escape symlink");
        let escaped = outside_link.join("escaped.txt");
        let output = run(&workspace_write, ProbeKind::Write, &escaped);
        assert_probe(backend_name, ProbeKind::Write, Expected::Denied, &output);
        assert!(!outside.join("escaped.txt").exists());

        let descendant_target = outside.join("descendant.txt");
        let output = run(
            &workspace_write,
            ProbeKind::DescendantWrite,
            &descendant_target,
        );
        assert_probe(
            backend_name,
            ProbeKind::DescendantWrite,
            Expected::TreeConfined,
            &output,
        );
        assert!(!descendant_target.exists());

        // Both implementations publish filesystem-only guarantees. A live
        // loopback connection proves host networking remains available.
        for sandbox_policy in [&read_only, &workspace_write] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind TCP probe");
            let target = PathBuf::from(listener.local_addr().unwrap().to_string());
            let output = run(sandbox_policy, ProbeKind::TcpConnect, &target);
            assert_probe(
                backend_name,
                ProbeKind::TcpConnect,
                Expected::Allowed,
                &output,
            );

            let socket_path = outside.join(format!(
                "{}-{}.sock",
                backend_name,
                sandbox_policy.mode.as_str()
            ));
            let _listener = UnixListener::bind(&socket_path).expect("bind Unix socket probe");
            let output = run(sandbox_policy, ProbeKind::UnixConnect, &socket_path);
            assert_probe(
                backend_name,
                ProbeKind::UnixConnect,
                Expected::Allowed,
                &output,
            );
        }
    }

    #[test]
    fn platform_default_uses_live_landlock_preflight_or_bwrap_fallback() {
        let selected = platform_default().expect("select preflighted Linux backend");
        let bwrap =
            BwrapSandbox::new().expect("Linux runtime CI must install the bubblewrap fallback");
        assert_eq!(bwrap.name(), "bwrap");
        match LandlockSandbox::new() {
            Ok(landlock) => {
                assert_eq!(landlock.name(), "landlock");
                assert_eq!(
                    selected.name(),
                    "landlock",
                    "a successful Landlock ABI preflight must win the default chain"
                );
            }
            Err(_) => assert_eq!(
                selected.name(),
                "bwrap",
                "an unavailable Landlock ABI must select the runnable fallback"
            ),
        }
    }

    #[test]
    fn landlock_runtime_matrix_matches_declared_filesystem_only_guarantees() {
        let backend =
            LandlockSandbox::new().expect("Linux runtime CI must expose a supported Landlock ABI");
        let capabilities = backend.capabilities();
        assert_eq!(capabilities.read_only, SandboxSupport::Supported);
        assert_eq!(capabilities.workspace_write, SandboxSupport::Supported);
        assert_eq!(
            capabilities.network_isolation,
            SandboxSupport::Unsupported,
            "Landlock must not claim network isolation"
        );
        run_matrix(
            backend.name(),
            |sandbox_policy, kind, target| execute_landlock(&backend, sandbox_policy, kind, target),
            "landlock",
        );
    }

    #[test]
    fn bubblewrap_runtime_matrix_matches_declared_filesystem_only_guarantees() {
        let backend =
            BwrapSandbox::new().expect("Linux runtime CI must install the bubblewrap fallback");
        let capabilities = backend.capabilities();
        assert_eq!(capabilities.read_only, SandboxSupport::Supported);
        assert_eq!(capabilities.workspace_write, SandboxSupport::Supported);
        assert_eq!(
            capabilities.network_isolation,
            SandboxSupport::Unsupported,
            "bubblewrap argv must not claim network isolation"
        );
        run_matrix(
            backend.name(),
            |sandbox_policy, kind, target| execute_bwrap(&backend, sandbox_policy, kind, target),
            "bwrap",
        );
    }

    #[test]
    fn landlock_launcher_child() {
        let Ok(serialized) = std::env::var(LANDLOCK_RULES) else {
            return;
        };
        let rules: LandlockRules =
            serde_json::from_str(&serialized).expect("decode production Landlock rules");
        let argv = probe_argv();
        if let Err(error) = apply_landlock(&rules, &argv) {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{LAUNCHER_ERROR}:{error}").expect("write launcher marker");
            stdout.flush().expect("flush launcher marker");
            std::process::exit(LAUNCHER_EXIT);
        }
        unreachable!("successful Landlock launcher replaces the process image");
    }

    #[test]
    fn sandbox_probe_child() {
        let Ok(kind) = std::env::var(PROBE_KIND) else {
            return;
        };
        let target = std::env::var(PROBE_TARGET).expect("probe target is present");
        if kind == ProbeKind::DescendantWrite.as_str() {
            run_descendant_probe(&target);
        }
        let result = match kind.as_str() {
            "read" => fs::read_to_string(&target).map(|_| ()),
            "write" => fs::write(&target, b"probe"),
            "tcp-connect" => TcpStream::connect(&target).map(|_| ()),
            "unix-connect" => UnixStream::connect(&target).map(|_| ()),
            other => panic!("unknown Linux sandbox probe kind {other}"),
        };
        let (marker, status) = if result.is_ok() {
            (PROBE_OK, 0)
        } else {
            (PROBE_DENIED, DENIED_EXIT)
        };
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{marker}:{kind}").expect("write probe marker");
        stdout.flush().expect("flush probe marker");
        std::process::exit(status);
    }

    fn run_descendant_probe(target: &str) -> ! {
        let executable = std::env::current_exe().expect("resolve descendant executable");
        let output = Command::new(executable)
            .args([
                "--exact".to_owned(),
                crate::it::test_name(module_path!(), "sandbox_probe_child"),
                "--nocapture".to_owned(),
            ])
            .env(PROBE_KIND, ProbeKind::Write.as_str())
            .env(PROBE_TARGET, target)
            .output()
            .expect("spawn sandbox descendant");
        let child_stdout = String::from_utf8_lossy(&output.stdout);
        let confined = output.status.code() == Some(DENIED_EXIT)
            && child_stdout.contains(PROBE_DENIED)
            && !Path::new(target).exists();
        let (marker, status) = if confined {
            (TREE_CONFINED, 0)
        } else {
            (TREE_ESCAPED, TREE_ESCAPE_EXIT)
        };
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{marker}").expect("write descendant marker");
        stdout.flush().expect("flush descendant marker");
        std::process::exit(status);
    }
}

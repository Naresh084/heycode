//! Runtime evidence for the filesystem-only macOS Seatbelt backend.
//!
//! The child probe re-enters this test executable under `sandbox-exec`. That
//! keeps the matrix independent of optional interpreters and shell quoting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(not(target_os = "macos"))]
#[test]
#[allow(clippy::print_stderr)]
fn seatbelt_runtime_matrix_is_macos_only() {
    eprintln!("SKIP: macOS Seatbelt runtime evidence requires target_os=macos");
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::fs::symlink;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use heycode_exec::{Sandbox, SandboxMode, SandboxPolicy, SandboxSupport};
    use heycode_sandbox::SeatbeltSandbox;

    const PROBE_KIND: &str = "HEYCODE_SEATBELT_PROBE_KIND";
    const PROBE_TARGET: &str = "HEYCODE_SEATBELT_PROBE_TARGET";
    const PROBE_OK: &str = "HEYCODE_SEATBELT_PROBE_OK";
    const PROBE_DENIED: &str = "HEYCODE_SEATBELT_PROBE_DENIED";
    const DENIED_EXIT: i32 = 73;

    #[derive(Clone, Copy, Debug)]
    enum ProbeKind {
        Read,
        Write,
        TcpConnect,
        UnixConnect,
        UnixBind,
        DevNullWrite,
    }

    impl ProbeKind {
        fn as_str(self) -> &'static str {
            match self {
                Self::Read => "read",
                Self::Write => "write",
                Self::TcpConnect => "tcp-connect",
                Self::UnixConnect => "unix-connect",
                Self::UnixBind => "unix-bind",
                Self::DevNullWrite => "dev-null-write",
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Expected {
        Allowed,
        Denied,
    }

    fn policy(mode: SandboxMode, workspace: &Path) -> SandboxPolicy {
        SandboxPolicy {
            mode,
            workspace_root: workspace.to_path_buf(),
        }
    }

    fn probe(
        backend: &SeatbeltSandbox,
        policy: &SandboxPolicy,
        kind: ProbeKind,
        target: &str,
        expected: Expected,
    ) -> Output {
        let test_executable = std::env::current_exe().expect("resolve integration-test binary");
        let child_argv = vec![
            test_executable.display().to_string(),
            "--exact".to_owned(),
            crate::it::test_name(module_path!(), "seatbelt_probe_child"),
            "--nocapture".to_owned(),
        ];
        let confined = backend
            .confine(&child_argv, policy)
            .expect("build Seatbelt argv");
        let output = Command::new(&confined[0])
            .args(&confined[1..])
            .env_clear()
            .env(PROBE_KIND, kind.as_str())
            .env(PROBE_TARGET, target)
            .output()
            .expect("run Seatbelt child probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        match expected {
            Expected::Allowed => assert!(
                output.status.success() && stdout.contains(PROBE_OK),
                "{} should be allowed; status={:?}; stdout={stdout:?}; stderr={stderr:?}",
                kind.as_str(),
                output.status.code()
            ),
            Expected::Denied => assert!(
                output.status.code() == Some(DENIED_EXIT) && stdout.contains(PROBE_DENIED),
                "{} should be denied by Seatbelt after the helper launched; status={:?}; stdout={stdout:?}; stderr={stderr:?}",
                kind.as_str(),
                output.status.code()
            ),
        }
        output
    }

    fn unique_private_tmp_path() -> PathBuf {
        PathBuf::from(format!(
            "/private/tmp/dshx-seatbelt-e12-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("runtime")
        ))
    }

    #[test]
    fn seatbelt_runtime_matrix_matches_declared_filesystem_only_guarantees() {
        let backend = SeatbeltSandbox::new()
            .expect("macOS runtime CI must provide the system sandbox-exec binary");
        let capabilities = backend.capabilities();
        assert_eq!(capabilities.read_only, SandboxSupport::Supported);
        assert_eq!(capabilities.workspace_write, SandboxSupport::Supported);
        assert_eq!(
            capabilities.network_isolation,
            SandboxSupport::Unsupported,
            "Seatbelt profiles must not claim network isolation"
        );

        let boundary = tempfile::tempdir().expect("create isolated boundary");
        let workspace = boundary.path().join("workspace");
        let outside = boundary.path().join("outside");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&outside).expect("create outside directory");
        let readable = outside.join("readable.txt");
        fs::write(&readable, b"readable").expect("seed readable file");

        let read_only = policy(SandboxMode::ReadOnly, &workspace);
        probe(
            &backend,
            &read_only,
            ProbeKind::Read,
            &readable.display().to_string(),
            Expected::Allowed,
        );
        probe(
            &backend,
            &read_only,
            ProbeKind::Write,
            &workspace
                .join("readonly-workspace.txt")
                .display()
                .to_string(),
            Expected::Denied,
        );
        probe(
            &backend,
            &read_only,
            ProbeKind::Write,
            &outside.join("readonly-outside.txt").display().to_string(),
            Expected::Denied,
        );
        probe(
            &backend,
            &read_only,
            ProbeKind::DevNullWrite,
            "/dev/null",
            Expected::Allowed,
        );

        let workspace_write = policy(SandboxMode::WorkspaceWrite, &workspace);
        let inside_write = workspace.join("inside.txt");
        probe(
            &backend,
            &workspace_write,
            ProbeKind::Write,
            &inside_write.display().to_string(),
            Expected::Allowed,
        );
        assert_eq!(fs::read_to_string(&inside_write).unwrap(), "probe");

        let outside_write = outside.join("outside.txt");
        probe(
            &backend,
            &workspace_write,
            ProbeKind::Write,
            &outside_write.display().to_string(),
            Expected::Denied,
        );
        assert!(!outside_write.exists());

        let outside_link = workspace.join("outside-link");
        symlink(&outside, &outside_link).expect("create workspace escape symlink");
        let escaped_write = outside_link.join("escaped.txt");
        probe(
            &backend,
            &workspace_write,
            ProbeKind::Write,
            &escaped_write.display().to_string(),
            Expected::Denied,
        );
        assert!(!outside.join("escaped.txt").exists());

        let private_tmp = unique_private_tmp_path();
        let _ = fs::remove_file(&private_tmp);
        probe(
            &backend,
            &workspace_write,
            ProbeKind::Write,
            &private_tmp.display().to_string(),
            Expected::Allowed,
        );
        assert_eq!(fs::read_to_string(&private_tmp).unwrap(), "probe");
        fs::remove_file(&private_tmp).expect("remove private tmp probe");

        // Unsupported network isolation means these filesystem-only profiles
        // continue to allow loopback networking. This is evidence against a
        // stronger claim, not a network-confinement test.
        for policy in [&read_only, &workspace_write] {
            let tcp = TcpListener::bind("127.0.0.1:0").expect("bind TCP probe");
            probe(
                &backend,
                policy,
                ProbeKind::TcpConnect,
                &tcp.local_addr().unwrap().to_string(),
                Expected::Allowed,
            );

            let socket_path = workspace.join(format!(
                "connect-{}.sock",
                match policy.mode {
                    SandboxMode::ReadOnly => "readonly",
                    SandboxMode::WorkspaceWrite => "workspace",
                    SandboxMode::Off => unreachable!("matrix excludes off mode"),
                }
            ));
            let _listener = UnixListener::bind(&socket_path).expect("bind Unix connect probe");
            probe(
                &backend,
                policy,
                ProbeKind::UnixConnect,
                &socket_path.display().to_string(),
                Expected::Allowed,
            );
        }

        let readonly_socket = workspace.join("readonly-bind.sock");
        probe(
            &backend,
            &read_only,
            ProbeKind::UnixBind,
            &readonly_socket.display().to_string(),
            Expected::Denied,
        );
        assert!(!readonly_socket.exists());

        let inside_socket = workspace.join("inside-bind.sock");
        probe(
            &backend,
            &workspace_write,
            ProbeKind::UnixBind,
            &inside_socket.display().to_string(),
            Expected::Allowed,
        );
        assert!(inside_socket.exists());
        fs::remove_file(&inside_socket).expect("remove inside socket");

        let outside_socket = outside.join("outside-bind.sock");
        probe(
            &backend,
            &workspace_write,
            ProbeKind::UnixBind,
            &outside_socket.display().to_string(),
            Expected::Denied,
        );
        assert!(!outside_socket.exists());
    }

    #[test]
    fn seatbelt_probe_child() {
        let Ok(kind) = std::env::var(PROBE_KIND) else {
            return;
        };
        let target = std::env::var(PROBE_TARGET).expect("probe target is present");
        let result = match kind.as_str() {
            "read" => fs::read_to_string(&target).map(|_| ()),
            "write" => fs::write(&target, b"probe"),
            "tcp-connect" => TcpStream::connect(&target).map(|_| ()),
            "unix-connect" => UnixStream::connect(&target).map(|_| ()),
            "unix-bind" => UnixListener::bind(&target).map(|_| ()),
            "dev-null-write" => OpenOptions::new()
                .write(true)
                .open(&target)
                .and_then(|mut file| file.write_all(b"probe")),
            other => panic!("unknown Seatbelt probe kind {other}"),
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
}

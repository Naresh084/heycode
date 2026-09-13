//! E04 source gate: product process creation has one local backend.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn production_process_spawn_is_owned_only_by_heycode_exec() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let crates = workspace.join("crates");
    let mut files = Vec::new();
    collect_rust_files(&crates, &mut files);
    let mut offenders = Vec::new();
    for path in files {
        let relative = path.strip_prefix(workspace).unwrap();
        if !relative
            .components()
            .any(|component| component.as_os_str() == "src")
        {
            continue;
        }
        if relative == std::path::Path::new("crates/heycode-exec/src/local.rs") {
            continue;
        }
        // This out-of-line unit module is compiled only under cfg(test), just
        // like the inline test modules stripped below. Verify its declaration
        // rather than exempting arbitrary production files named tests.rs.
        if relative
            == std::path::Path::new("crates/heycode-agent/src/workspace_transition/tests.rs")
        {
            let owner = std::fs::read_to_string(
                workspace.join("crates/heycode-agent/src/workspace_transition.rs"),
            )
            .unwrap();
            assert!(owner.contains("#[cfg(test)]\nmod tests;"));
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let mut production = source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(&source)
            .to_owned();
        if relative == std::path::Path::new("crates/heycode-cli/src/session_background.rs") {
            // The terminal broker exists before service composition. Its only
            // process boundaries are the exact same-executable bootstrap and
            // the owned PTY worker; neither accepts an arbitrary executable.
            // Remove only these verified expressions, not the whole module, so
            // any additional direct process creation still fails this gate.
            const BOOTSTRAP: &str = r#"Command::new(std::env::current_exe()?)
        .arg("__session-host")
        .arg(&path)
        .env_remove(wire::HOST_ENV)
        .env_remove(wire::TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()"#;
            const PTY_WORKER: &str = r#"let mut command = CommandBuilder::new(std::env::current_exe()?);
    command.args(&launch.args);
    command.cwd(&launch.cwd);
    command.env(wire::HOST_ENV, &socket);
    command.env(wire::TOKEN_ENV, &launch.token);
    let mut child = pair.slave.spawn_command(command)?;"#;
            for expression in [BOOTSTRAP, PTY_WORKER] {
                assert_eq!(
                    production.matches(expression).count(),
                    1,
                    "broker launch boundary changed and requires review"
                );
                production = production.replacen(
                    expression,
                    "/* verified same-executable terminal owner */",
                    1,
                );
            }
        }
        let direct_spawn = production.contains("tokio::process::Command")
            || production.contains(".spawn()")
            || production.contains(".spawn_command(")
            || production.contains(".output().await")
            || production.contains(".output()") && production.contains("std::process::Command");
        let landlock_exec = relative
            == std::path::Path::new("crates/heycode-sandbox/src/landlock.rs")
            && production.contains("std::process::Command")
            && production.contains(".exec()");
        if direct_spawn && !landlock_exec {
            offenders.push(relative.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "direct product process creation bypasses heycode-exec: {offenders:?}"
    );
}

fn collect_rust_files(root: &std::path::Path, output: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rust_files(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

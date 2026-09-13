//! Explicit shell request resolution and subprocess delegation contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use heycode_exec::{
    LocalShellConfig, ProcessErrorCode, ProcessExit, SERVICE_SHELL, SandboxMode, SandboxService,
    ShellRequest, ShellService, local_shell_plugin, local_subprocess_plugin,
    sandbox_service_plugin,
};
use tokio_util::sync::CancellationToken;

fn config(cwd: &Path) -> LocalShellConfig {
    LocalShellConfig::platform(cwd.canonicalize().unwrap(), Duration::from_secs(3)).unwrap()
}

fn composed_shell(cwd: &Path) -> (heycode_core::Context, std::sync::Arc<ShellService>) {
    let plugins = vec![
        sandbox_service_plugin(
            SandboxService::new(SandboxMode::Off, cwd.canonicalize().unwrap(), None).unwrap(),
        ),
        local_subprocess_plugin(),
        local_shell_plugin(config(cwd)),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let shell = context
        .get::<ShellService>(SERVICE_SHELL)
        .expect("shell service");
    (context, shell)
}

#[tokio::test]
async fn resolve_materializes_every_default_once_then_execute_uses_the_spec() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let (_context, shell) = composed_shell(root.path());
    let command = success_command();
    let spec = shell
        .resolve(
            ShellRequest::new(command)
                .unwrap()
                .with_cwd(nested.canonicalize().unwrap())
                .unwrap(),
        )
        .unwrap();

    assert!(!spec.shell_id().as_str().is_empty());
    assert_eq!(spec.cwd(), nested.canonicalize().unwrap());
    assert_eq!(spec.timeout(), Some(Duration::from_secs(3)));
    assert_eq!(spec.output_limit_bytes(), 64 * 1024);
    assert!(Path::new(&spec.launch_argv()[0]).is_absolute());
    assert!(spec.environment().iter().all(|(name, _)| {
        name.to_str()
            .is_some_and(|name| !looks_like_credential(name))
    }));

    let output = shell.execute(spec, CancellationToken::new()).await.unwrap();
    assert_eq!(output.exit(), &ProcessExit::Exited { code: 0 });
    assert!(String::from_utf8_lossy(output.stdout()).contains("shell-ok"));
}

#[tokio::test]
async fn nonzero_and_timeout_are_results_while_cancellation_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let (_context, shell) = composed_shell(root.path());

    let nonzero = shell
        .resolve(ShellRequest::new(nonzero_command()).unwrap())
        .unwrap();
    let output = shell
        .execute(nonzero, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.exit(), &ProcessExit::Exited { code: 7 });

    let timed = shell
        .resolve(
            ShellRequest::new(sleep_command())
                .unwrap()
                .with_timeout(Duration::from_millis(40))
                .unwrap(),
        )
        .unwrap();
    let output = shell
        .execute(timed, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.exit(), &ProcessExit::TimedOut);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let spec = shell
        .resolve(ShellRequest::new(success_command()).unwrap())
        .unwrap();
    let error = shell.execute(spec, cancelled).await.unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::Cancelled);
}

#[test]
fn request_and_wrapper_boundaries_reject_ambiguous_values_without_debug_leaks() {
    let root = tempfile::tempdir().unwrap();
    let (_context, shell) = composed_shell(root.path());
    assert_eq!(
        ShellRequest::new("   ").unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        ShellRequest::new("bad\0command").unwrap_err().code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        ShellRequest::new("secret-command-canary")
            .unwrap()
            .with_cwd("relative")
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        ShellRequest::new("secret-command-canary")
            .unwrap()
            .with_timeout(Duration::ZERO)
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );

    let request = ShellRequest::new("secret-command-canary").unwrap();
    assert!(!format!("{request:?}").contains("secret-command-canary"));
    let spec = shell.resolve(request).unwrap();
    assert!(!format!("{spec:?}").contains("secret-command-canary"));
    assert_eq!(
        spec.clone()
            .with_launch_argv(Vec::<OsString>::new())
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
    assert_eq!(
        spec.with_launch_argv([OsString::from("relative-wrapper")])
            .unwrap_err()
            .code(),
        ProcessErrorCode::InvalidSpec
    );
}

#[tokio::test]
async fn shell_plugin_fails_without_subprocess_and_publishes_with_it() {
    let root = tempfile::tempdir().unwrap();
    let missing = vec![local_shell_plugin(config(root.path()))];
    assert!(heycode_core::compose(&missing).is_err());

    let plugins = vec![
        sandbox_service_plugin(
            SandboxService::new(SandboxMode::Off, root.path().canonicalize().unwrap(), None)
                .unwrap(),
        ),
        local_subprocess_plugin(),
        local_shell_plugin(config(root.path())),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    assert_eq!(context.owner_of(SERVICE_SHELL), Some("shell-local"));
    let shell = context.get::<ShellService>(SERVICE_SHELL).unwrap();
    context.shutdown();
    let spec = shell
        .resolve(ShellRequest::new(success_command()).unwrap())
        .unwrap();
    let error = shell
        .execute(spec, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), ProcessErrorCode::ServiceStopped);
}

fn looks_like_credential(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "PASSWORD", "SECRET", "TOKEN"]
        .iter()
        .any(|needle| upper.contains(needle))
}

#[cfg(unix)]
fn success_command() -> &'static str {
    "printf shell-ok"
}

#[cfg(windows)]
fn success_command() -> &'static str {
    "echo shell-ok"
}

#[cfg(unix)]
fn nonzero_command() -> &'static str {
    "exit 7"
}

#[cfg(windows)]
fn nonzero_command() -> &'static str {
    "exit /b 7"
}

#[cfg(unix)]
fn sleep_command() -> &'static str {
    "sleep 30"
}

#[cfg(windows)]
fn sleep_command() -> &'static str {
    "ping -n 30 127.0.0.1 >NUL"
}

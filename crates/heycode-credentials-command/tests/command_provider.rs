//! S10 timeout, empty output, rotation, lifecycle and redaction contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::compose;
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialQuery, CredentialReference, CredentialSource,
    CredentialsService, SERVICE_CREDENTIALS, credentials_plugin,
};
use heycode_credentials_command::{
    CommandCredentialProvider, CommandCredentialSpec, command_credentials_plugin,
};
use heycode_settings::{SettingsDocuments, settings_plugin};

struct RecordingSandbox(std::sync::Arc<std::sync::Mutex<usize>>);

impl heycode_exec::Sandbox for RecordingSandbox {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn capabilities(&self) -> heycode_exec::SandboxBackendCapabilities {
        heycode_exec::SandboxBackendCapabilities {
            read_only: heycode_exec::SandboxSupport::Supported,
            workspace_write: heycode_exec::SandboxSupport::Supported,
            network_isolation: heycode_exec::SandboxSupport::Unsupported,
        }
    }

    fn confine(
        &self,
        argv: &[String],
        _policy: &heycode_exec::SandboxPolicy,
    ) -> Result<Vec<String>, heycode_exec::SandboxError> {
        *self.0.lock().unwrap() += 1;
        Ok(argv.to_vec())
    }
}

fn query(name: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(name).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

#[cfg(unix)]
fn cat_spec(reference: &str, path: &std::path::Path) -> CommandCredentialSpec {
    CommandCredentialSpec::new(
        CredentialReference::new(reference).unwrap(),
        "/bin/cat",
        [path.to_string_lossy().into_owned()],
    )
    .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn every_resolve_executes_again_inside_a_runtime_so_rotation_is_immediate() {
    let dir = tempfile::tempdir().unwrap();
    let value_path = dir.path().join("value");
    std::fs::write(&value_path, "first-secret\n").unwrap();
    let provider = CommandCredentialProvider::new([cat_spec("ROTATING_KEY", &value_path)]).unwrap();
    let request = query("ROTATING_KEY");

    let first = provider.resolve(&request).unwrap().unwrap();
    assert_eq!(first.expose(), "first-secret");
    std::fs::write(&value_path, "second-secret\n").unwrap();
    let second = provider.resolve(&request).unwrap().unwrap();
    assert_eq!(second.expose(), "second-secret");

    provider.inspect(&request).unwrap();
    assert!(provider.resolve(&query("UNKNOWN")).unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn timeout_empty_nonzero_and_oversized_outputs_are_fixed_redacted_errors() {
    let canary = "command-secret-canary-never-emit";
    let timeout = CommandCredentialSpec::new(
        CredentialReference::new("TIMEOUT_KEY").unwrap(),
        "/bin/sleep",
        ["5"],
    )
    .unwrap()
    .with_timeout(std::time::Duration::from_millis(50))
    .unwrap();
    let empty = CommandCredentialSpec::new(
        CredentialReference::new("EMPTY_KEY").unwrap(),
        "/usr/bin/true",
        std::iter::empty::<&str>(),
    )
    .unwrap();
    let nonzero = CommandCredentialSpec::new(
        CredentialReference::new("FAILED_KEY").unwrap(),
        "/bin/sh",
        ["-c", &format!("printf '{canary}' >&2; exit 9")],
    )
    .unwrap();
    let oversized = CommandCredentialSpec::new(
        CredentialReference::new("LARGE_KEY").unwrap(),
        "/usr/bin/yes",
        [canary],
    )
    .unwrap();
    let provider = CommandCredentialProvider::new([timeout, empty, nonzero, oversized]).unwrap();

    for (reference, expected) in [
        ("TIMEOUT_KEY", "timed out"),
        ("EMPTY_KEY", "empty output"),
        ("FAILED_KEY", "exited unsuccessfully"),
        ("LARGE_KEY", "output limit"),
    ] {
        let error = provider.resolve(&query(reference)).unwrap_err();
        assert!(error.contains(expected), "{reference}: {error}");
        assert!(!error.contains(canary), "{reference}: {error}");
    }
    assert!(!format!("{provider:?}").contains(canary));
}

#[cfg(unix)]
#[test]
fn command_timeout_reaps_descendants_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("credential-descendant-survived");
    let command = format!(
        "(sleep 0.65; printf survived > {}) & sleep 5",
        marker.display()
    );
    let spec = CommandCredentialSpec::new(
        CredentialReference::new("TREE_TIMEOUT_KEY").unwrap(),
        "/bin/sh",
        ["-c", &command],
    )
    .unwrap()
    .with_timeout(std::time::Duration::from_millis(50))
    .unwrap();
    let provider = CommandCredentialProvider::new([spec]).unwrap();
    let error = provider.resolve(&query("TREE_TIMEOUT_KEY")).unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    std::thread::sleep(std::time::Duration::from_millis(900));
    assert!(
        !marker.exists(),
        "credential helper descendant escaped timeout"
    );
}

#[cfg(unix)]
#[test]
fn plugin_registration_is_effect_owned_and_duplicate_specs_fail_loud() {
    let dir = tempfile::tempdir().unwrap();
    let value_path = dir.path().join("value");
    std::fs::write(&value_path, "plugin-secret").unwrap();
    let provider = CommandCredentialProvider::new([cat_spec("PLUGIN_KEY", &value_path)]).unwrap();
    let sandbox_calls = std::sync::Arc::new(std::sync::Mutex::new(0));
    let cwd = std::env::current_dir().unwrap();
    let shell =
        heycode_exec::LocalShellConfig::platform(cwd.clone(), std::time::Duration::from_secs(30))
            .unwrap();
    let sandbox = heycode_exec::SandboxService::new(
        heycode_exec::SandboxMode::ReadOnly,
        cwd,
        Some(std::sync::Arc::new(RecordingSandbox(sandbox_calls.clone()))),
    )
    .unwrap();
    let plugins = vec![
        heycode_exec::local_execution_plugin_with_sandbox(shell, sandbox),
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        command_credentials_plugin(provider),
    ];
    let mut context = compose(&plugins).unwrap();
    let credentials = context
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    let descriptor = credentials.describe(&query("PLUGIN_KEY")).unwrap();
    assert!(descriptor.configured);
    assert_eq!(descriptor.source, Some(CredentialSource::Command));
    assert_eq!(
        credentials
            .resolve(&query("PLUGIN_KEY"))
            .unwrap()
            .unwrap()
            .expose(),
        "plugin-secret"
    );
    assert_eq!(*sandbox_calls.lock().unwrap(), 1);
    context.shutdown();
    assert!(
        !credentials
            .describe(&query("PLUGIN_KEY"))
            .unwrap()
            .configured
    );

    let duplicate = CommandCredentialProvider::new([
        cat_spec("DUPLICATE_KEY", &value_path),
        cat_spec("DUPLICATE_KEY", &value_path),
    ])
    .unwrap_err()
    .to_string();
    assert!(duplicate.contains("DUPLICATE_KEY"), "{duplicate}");
}

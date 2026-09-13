//! Q14/Q15 product manager: verified local bundle -> policy -> durable install.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::Arc;

use heycode_config::ConfigVersionState;
use heycode_install::{
    ArtifactDigest, InstallDisposition, PluginCompatibilitySet, ReleaseApplyOutcome, ReleaseBundle,
    ReleaseChannel, ReleaseManager, ReleaseManagerError, ReleasePolicyRefusal, RollbackOutcome,
};

use super::support::{
    FixtureVerifier, artifact_bytes, attestation_bundle, attested_manifest_json, platform, trust,
    version,
};

fn bundle(version: &str, plugin_api: u32) -> ReleaseBundle {
    bundle_with_artifact(version, plugin_api, artifact_bytes(version))
}

fn bundle_with_artifact(version: &str, plugin_api: u32, artifact: Vec<u8>) -> ReleaseBundle {
    let artifact_bundle = attestation_bundle(&artifact);
    let manifest = attested_manifest_json(
        version,
        25,
        plugin_api,
        &artifact,
        &ArtifactDigest::of_bytes(&artifact_bundle),
    );
    let manifest_bundle = attestation_bundle(manifest.as_bytes());
    ReleaseBundle::new(
        manifest.into_bytes(),
        manifest_bundle,
        artifact,
        artifact_bundle,
    )
}

#[cfg(feature = "local-release-evidence")]
#[test]
fn local_built_heycode_runs_the_complete_content_withheld_release_transaction() {
    use std::ffi::OsStr;
    use std::fs::OpenOptions;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use heycode_install::{PluginApiCompatibility, ReleasePolicyRefusal, RollbackRefusal};

    fn required_path(name: &str) -> PathBuf {
        let path = PathBuf::from(std::env::var_os(name).expect("required evidence path"));
        assert!(path.is_absolute(), "evidence inputs must be absolute");
        path
    }

    fn fake_turn(binary: &Path, root: &Path, suffix: &str) {
        let home = root.join(format!("home-{suffix}"));
        let workspace = root.join(format!("workspace-{suffix}"));
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let output = Command::new(binary)
            .current_dir(workspace)
            .env("HEYCODE_HOME", home)
            .args([
                OsStr::new("--restricted-workspace"),
                OsStr::new("--fake"),
                OsStr::new("run"),
                OsStr::new("local release transaction smoke"),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("FAKE-REPLY"));
    }

    let artifact_path = required_path("HEYCODE_LOCAL_RELEASE_ARTIFACT");
    let evidence_path = required_path("HEYCODE_LOCAL_RELEASE_EVIDENCE");
    let metadata = std::fs::symlink_metadata(&artifact_path).unwrap();
    assert!(!metadata.file_type().is_symlink() && metadata.is_file());
    assert!(!evidence_path.exists());
    let artifact = std::fs::read(&artifact_path).unwrap();
    assert!(!artifact.is_empty());

    let temp = tempfile::tempdir().unwrap();
    let manager = ReleaseManager::new(
        temp.path().join("install"),
        platform(),
        trust(),
        Arc::new(FixtureVerifier),
    )
    .unwrap();

    let first = manager
        .apply(
            bundle_with_artifact("1.0.0", 1, artifact.clone()),
            ReleaseChannel::Stable,
            PluginCompatibilitySet::empty(),
        )
        .unwrap();
    assert!(matches!(
        first,
        ReleaseApplyOutcome::Installed(ref outcome)
            if outcome.disposition == InstallDisposition::FreshInstall
    ));
    let stable_binary = temp.path().join("install/bin").join(if cfg!(windows) {
        "heycode.exe"
    } else {
        "heycode"
    });
    fake_turn(&stable_binary, temp.path(), "fresh");

    assert!(matches!(
        manager.apply(
            bundle_with_artifact("1.1.0-preview.1", 1, artifact.clone()),
            ReleaseChannel::Stable,
            PluginCompatibilitySet::empty(),
        ),
        Err(ReleaseManagerError::Policy(
            ReleasePolicyRefusal::PreviewNotAllowed { .. }
        ))
    ));

    let requires_v2 = PluginCompatibilitySet::new(vec![
        PluginApiCompatibility::new("fixture/requires-v2", 2, 2).unwrap(),
    ])
    .unwrap();
    assert!(matches!(
        manager.apply(
            bundle_with_artifact("2.0.0", 1, artifact.clone()),
            ReleaseChannel::Stable,
            requires_v2.clone(),
        ),
        Err(ReleaseManagerError::Policy(
            ReleasePolicyRefusal::PluginApiIncompatible { .. }
        ))
    ));

    let second = manager
        .apply(
            bundle_with_artifact("2.0.0", 2, artifact.clone()),
            ReleaseChannel::Stable,
            requires_v2.clone(),
        )
        .unwrap();
    assert!(matches!(
        second,
        ReleaseApplyOutcome::Installed(ref outcome)
            if outcome.disposition == InstallDisposition::Update
                && outcome.previous == Some(version("1.0.0"))
    ));
    fake_turn(&stable_binary, temp.path(), "updated");

    assert_eq!(
        manager
            .rollback(ConfigVersionState::Current(25), requires_v2)
            .unwrap(),
        RollbackOutcome::Refused(RollbackRefusal::PluginApiIncompatible {
            plugin: "fixture/requires-v2".to_owned(),
            target_api: 1,
            minimum: 2,
            maximum: 2,
        })
    );
    assert_eq!(
        manager
            .rollback(
                ConfigVersionState::Current(25),
                PluginCompatibilitySet::empty(),
            )
            .unwrap(),
        RollbackOutcome::RolledBack {
            to: version("1.0.0")
        }
    );
    assert_eq!(manager.state().unwrap().previous, None);
    assert_eq!(
        ArtifactDigest::of_bytes(&std::fs::read(&stable_binary).unwrap()),
        ArtifactDigest::of_bytes(&artifact),
        "the directional rollback must restore the exact authenticated artifact"
    );
    fake_turn(&stable_binary, temp.path(), "rolled-back");

    let evidence = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "artifact": "local_built_heycode",
        "artifact_content": "withheld",
        "signature_evidence": "deterministic_fixture_not_github",
        "external_github_attestation_observed": false,
        "external_real_provider_turn_observed": false,
        "checks": [
            "fresh_install",
            "installed_artifact_fake_turn",
            "stable_preview_refusal",
            "plugin_api_update_refusal",
            "stable_update",
            "plugin_api_rollback_refusal",
            "directional_rollback"
        ]
    }))
    .unwrap();
    let evidence_text = String::from_utf8(evidence.clone()).unwrap();
    assert!(!evidence_text.contains("FAKE-REPLY"));
    assert!(!evidence_text.contains(&artifact_path.to_string_lossy().to_string()));

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut output = options.open(evidence_path).unwrap();
    output.write_all(&evidence).unwrap();
    output.sync_all().unwrap();
}

#[test]
fn manager_runs_fresh_update_and_directional_rollback_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let manager = ReleaseManager::new(
        temp.path().join("install"),
        platform(),
        trust(),
        Arc::new(FixtureVerifier),
    )
    .unwrap();

    let first = manager
        .apply(
            bundle("1.0.0", 1),
            ReleaseChannel::Stable,
            PluginCompatibilitySet::empty(),
        )
        .unwrap();
    let ReleaseApplyOutcome::Installed(first) = first else {
        panic!("fresh release must install");
    };
    assert_eq!(first.disposition, InstallDisposition::FreshInstall);

    let second = manager
        .apply(
            bundle("2.0.0", 1),
            ReleaseChannel::Stable,
            PluginCompatibilitySet::empty(),
        )
        .unwrap();
    let ReleaseApplyOutcome::Installed(second) = second else {
        panic!("newer release must install");
    };
    assert_eq!(second.disposition, InstallDisposition::Update);
    assert_eq!(second.previous, Some(version("1.0.0")));

    assert_eq!(
        manager
            .rollback(
                ConfigVersionState::Current(25),
                PluginCompatibilitySet::empty(),
            )
            .unwrap(),
        RollbackOutcome::RolledBack {
            to: version("1.0.0")
        }
    );
    assert_eq!(manager.state().unwrap().previous, None);
}

#[test]
fn fresh_install_obeys_channel_and_enabled_plugin_policy() {
    let temp = tempfile::tempdir().unwrap();
    let manager = ReleaseManager::new(
        temp.path().join("install"),
        platform(),
        trust(),
        Arc::new(FixtureVerifier),
    )
    .unwrap();

    assert!(matches!(
        manager.apply(
            bundle("2.0.0-preview.1", 1),
            ReleaseChannel::Stable,
            PluginCompatibilitySet::empty(),
        ),
        Err(ReleaseManagerError::Policy(
            ReleasePolicyRefusal::PreviewNotAllowed { .. }
        ))
    ));
    assert_eq!(manager.state().unwrap().current, None);
}

#[test]
fn manager_close_makes_held_handles_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let manager = ReleaseManager::new(
        temp.path().join("install"),
        platform(),
        trust(),
        Arc::new(FixtureVerifier),
    )
    .unwrap();
    manager.close();

    assert_eq!(manager.state().unwrap_err(), ReleaseManagerError::Closed);
    assert_eq!(
        manager
            .apply(
                bundle("1.0.0", 1),
                ReleaseChannel::Stable,
                PluginCompatibilitySet::empty(),
            )
            .unwrap_err(),
        ReleaseManagerError::Closed
    );
}

#[test]
fn manager_construction_and_invalid_signature_create_no_installation_state() {
    let temp = tempfile::tempdir().unwrap();
    let install = temp.path().join("install");
    let manager =
        ReleaseManager::new(&install, platform(), trust(), Arc::new(FixtureVerifier)).unwrap();
    assert!(!install.exists());

    let candidate = ReleaseBundle::new(
        b"not-the-signed-manifest".to_vec(),
        b"invalid-bundle".to_vec(),
        artifact_bytes("1.0.0"),
        b"invalid-artifact-bundle".to_vec(),
    );
    assert!(
        manager
            .apply(
                candidate,
                ReleaseChannel::Stable,
                PluginCompatibilitySet::empty(),
            )
            .is_err()
    );
    assert!(!install.exists());
}

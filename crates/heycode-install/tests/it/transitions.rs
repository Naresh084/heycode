//! Q14 acceptance: a fresh install lands, and a prior-version rollback lands.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_config::ConfigVersionState;
use heycode_install::{
    ArtifactMismatch, InstallDisposition, InstallRoot, PluginCompatibilitySet, RollbackOutcome,
    RollbackRefusal, UpdateError,
};

use super::support::{
    FixtureVerifier, approved_update, artifact_bytes, attestation_bundle, attested_release,
    platform, trust, verified_release, version,
};

#[test]
fn stable_binary_path_uses_the_native_executable_name() {
    let temp = tempfile::tempdir().unwrap();
    let root = InstallRoot::open(temp.path().join("install")).unwrap();
    #[cfg(windows)]
    assert_eq!(root.current_binary().file_name().unwrap(), "heycode.exe");
    #[cfg(not(windows))]
    assert_eq!(root.current_binary().file_name().unwrap(), "heycode");
}

fn root(temp: &tempfile::TempDir) -> InstallRoot {
    InstallRoot::open(temp.path().join("opt/heycode")).unwrap()
}

/// Acceptance transition one.
#[test]
fn a_fresh_install_lands_the_binary_and_records_no_rollback_target() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (artifact, bytes) = verified_release("1.0.0", 18, 1);

    let outcome = root.install(artifact).unwrap();
    assert_eq!(outcome.disposition, InstallDisposition::FreshInstall);
    assert_eq!(outcome.version, version("1.0.0"));
    assert_eq!(outcome.previous, None);

    assert_eq!(std::fs::read(root.current_binary()).unwrap(), bytes);
    let state = root.state().unwrap();
    assert_eq!(state.current, Some(version("1.0.0")));
    assert_eq!(
        state.previous, None,
        "a fresh install has nothing to roll back to"
    );
}

#[cfg(unix)]
#[test]
fn a_freshly_installed_binary_is_executable() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (artifact, _bytes) = verified_release("1.0.0", 18, 1);
    root.install(artifact).unwrap();

    let mode = std::fs::metadata(root.current_binary())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o111,
        0o111,
        "an installed binary nobody can run is not installed"
    );
}

/// The substitution case: something served different bytes under a version and
/// platform that is already trusted.
#[test]
fn artifact_bytes_that_miss_the_pinned_digest_are_refused_before_anything_is_written() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (manifest, _bytes, _bundle) = attested_release("1.0.0", 18, 1);
    let substituted = artifact_bytes("1.0.0-evil");

    let error = manifest
        .verify_artifact(
            &platform(),
            substituted.clone(),
            &attestation_bundle(&substituted),
            &FixtureVerifier,
        )
        .err()
        .expect("substituted bytes must be refused");
    let UpdateError::Mismatch(mismatch) = error else {
        panic!("substituted bytes must be reported as a mismatch");
    };
    assert!(matches!(*mismatch, ArtifactMismatch::Digest { .. }));

    assert!(
        !root.current_binary().exists(),
        "a refused install must not leave a binary behind"
    );
    assert_eq!(root.state().unwrap().current, None);
    assert!(root.retained_versions().unwrap().is_empty());
}

#[test]
fn an_update_retains_the_version_it_displaced_as_the_rollback_target() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 19, 1);
    root.install(first).unwrap();

    let outcome = root.install(second).unwrap();
    assert_eq!(outcome.disposition, InstallDisposition::Update);
    assert_eq!(outcome.previous, Some(version("1.0.0")));

    assert_eq!(std::fs::read(root.current_binary()).unwrap(), second_bytes);
    assert_eq!(
        root.retained_versions().unwrap(),
        vec![version("1.0.0"), version("2.0.0")]
    );
}

/// Acceptance transition two.
#[test]
fn a_rollback_returns_the_current_binary_to_the_retained_prior_version() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, _second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();

    let outcome = root
        .rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier,
        )
        .unwrap();
    assert_eq!(
        outcome,
        RollbackOutcome::RolledBack {
            to: version("1.0.0")
        }
    );
    assert_eq!(std::fs::read(root.current_binary()).unwrap(), first_bytes);
    assert_eq!(root.state().unwrap().current, Some(version("1.0.0")));
}

/// PL06's rule, kept: a target the disk no longer holds fails loudly instead
/// of leaving an installation pointing at nothing.
#[test]
fn a_rollback_to_a_pruned_version_is_refused_rather_than_leaving_a_dangling_current() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    std::fs::remove_dir_all(root.root().join("versions/1.0.0")).unwrap();

    let outcome = root
        .rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier,
        )
        .unwrap();
    assert_eq!(
        outcome,
        RollbackOutcome::Refused(RollbackRefusal::TargetNotRetained {
            version: version("1.0.0")
        })
    );
    assert_eq!(
        std::fs::read(root.current_binary()).unwrap(),
        second_bytes,
        "a refused rollback must not move the current binary"
    );
    assert_eq!(root.state().unwrap().current, Some(version("2.0.0")));
}

#[test]
fn a_rollback_with_no_prior_version_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (artifact, _bytes) = verified_release("1.0.0", 18, 1);
    root.install(artifact).unwrap();

    assert_eq!(
        root.rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier
        )
        .unwrap(),
        RollbackOutcome::Refused(RollbackRefusal::NothingToRollBackTo)
    );
}

/// The B03 gate end to end: v2 migrated the configuration past what v1 reads,
/// so the rollback is refused before the swap rather than producing an
/// installation that starts and then fails on its own configuration.
#[test]
fn a_rollback_is_refused_when_the_configuration_is_newer_than_the_target_understands() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 19, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();

    let outcome = root
        .rollback(
            ConfigVersionState::Current(19),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier,
        )
        .unwrap();
    assert_eq!(
        outcome,
        RollbackOutcome::Refused(RollbackRefusal::ConfigurationTooNew {
            document: 19,
            target_supports: 18,
        })
    );
    assert_eq!(
        std::fs::read(root.current_binary()).unwrap(),
        second_bytes,
        "the gate must decide before the binary moves"
    );
    assert_eq!(root.state().unwrap().current, Some(version("2.0.0")));
}

/// The deliberate divergence from PL06. A plugin rollback swaps, so it can be
/// undone in one command. A binary rollback does not: the version just escaped
/// is the one an operator rolled back *because* it was broken, and moving to
/// it re-applies a configuration migration rather than reversing one.
#[test]
fn a_rollback_does_not_arm_another_rollback_back_to_the_version_it_escaped() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, _second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    root.rollback(
        ConfigVersionState::Current(18),
        &PluginCompatibilitySet::empty(),
        &trust(),
        &FixtureVerifier,
    )
    .unwrap();

    let state = root.state().unwrap();
    assert_eq!(state.current, Some(version("1.0.0")));
    assert_eq!(
        state.previous, None,
        "the escaped version must not become the next rollback target"
    );
    assert_eq!(
        root.rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier
        )
        .unwrap(),
        RollbackOutcome::Refused(RollbackRefusal::NothingToRollBackTo)
    );
}

/// Directional does not mean lossy: retention is derived from disk, so the
/// escaped version is still there to install again deliberately.
#[test]
fn the_version_a_rollback_escaped_stays_retained_and_can_be_installed_again() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, _second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    root.rollback(
        ConfigVersionState::Current(18),
        &PluginCompatibilitySet::empty(),
        &trust(),
        &FixtureVerifier,
    )
    .unwrap();

    assert_eq!(
        root.retained_versions().unwrap(),
        vec![version("1.0.0"), version("2.0.0")]
    );
    let (second_again, _bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    let outcome = root.install(second_again).unwrap();
    assert_eq!(outcome.disposition, InstallDisposition::Update);
    assert_eq!(outcome.previous, Some(version("1.0.0")));
}

/// A digest checked once at install time and never again would roll back into
/// whatever the retained directory now contains.
#[test]
fn a_retained_copy_that_no_longer_matches_its_record_is_refused_at_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    std::fs::write(
        root.root().join("versions/1.0.0/heycode"),
        artifact_bytes("1.0.0-tampered"),
    )
    .unwrap();

    let error = root
        .rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier,
        )
        .unwrap_err();
    let UpdateError::Mismatch(mismatch) = error else {
        panic!("a tampered retained copy must be reported as a mismatch");
    };
    assert!(matches!(*mismatch, ArtifactMismatch::Retained { .. }));
    assert_eq!(
        std::fs::read(root.current_binary()).unwrap(),
        second_bytes,
        "a refused rollback must not move the current binary"
    );
}

#[test]
fn a_retained_signature_bundle_is_reverified_before_rollback_publication() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    std::fs::write(
        root.root().join("versions/1.0.0/artifact.sigstore.json"),
        b"substituted bundle",
    )
    .unwrap();

    assert!(
        root.rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier
        )
        .is_err()
    );
    assert_eq!(std::fs::read(root.current_binary()).unwrap(), second_bytes);
    assert_eq!(root.state().unwrap().current, Some(version("2.0.0")));
}

#[cfg(unix)]
#[test]
fn reopening_completes_an_update_interrupted_after_atomic_binary_publication() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let install_path = temp.path().join("opt/heycode");
    let root = InstallRoot::open(&install_path).unwrap();
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();

    std::fs::set_permissions(root.root(), std::fs::Permissions::from_mode(0o555)).unwrap();
    assert!(root.install(second).is_err());
    assert_eq!(std::fs::read(root.current_binary()).unwrap(), second_bytes);
    std::fs::set_permissions(root.root(), std::fs::Permissions::from_mode(0o755)).unwrap();
    drop(root);

    let recovered = InstallRoot::open(&install_path).unwrap();
    assert_eq!(recovered.state().unwrap().current, Some(version("2.0.0")));
    assert_eq!(recovered.state().unwrap().previous, Some(version("1.0.0")));
    assert!(!recovered.root().join("bin/.transition").exists());
}

#[test]
fn installing_the_current_version_again_is_reported_rather_than_silently_repeated() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (artifact, _bytes) = verified_release("1.0.0", 18, 1);
    root.install(artifact).unwrap();

    let (same, _bytes) = verified_release("1.0.0", 18, 1);
    assert_eq!(
        root.install(same).unwrap_err(),
        UpdateError::AlreadyCurrent {
            version: version("1.0.0")
        }
    );
    assert_eq!(
        root.state().unwrap().previous,
        None,
        "a refused reinstall must not consume the rollback target"
    );
}

/// A retention list inside the record could disagree with the disk, and the
/// disagreement would surface as a rollback into an empty directory.
#[test]
fn retained_versions_are_derived_from_disk_rather_than_from_the_record() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, _second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    assert_eq!(root.retained_versions().unwrap().len(), 2);

    std::fs::remove_dir_all(root.root().join("versions/1.0.0")).unwrap();
    assert_eq!(
        root.retained_versions().unwrap(),
        vec![version("2.0.0")],
        "the record still names 1.0.0; the disk is the authority"
    );
}

/// A retained directory can lose either of its two files independently, so a
/// test that deletes both cannot say which check refused. These two can.
#[test]
fn a_rollback_target_missing_its_release_record_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    std::fs::remove_file(root.root().join("versions/1.0.0/release.json")).unwrap();

    assert_eq!(
        root.rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier
        )
        .unwrap(),
        RollbackOutcome::Refused(RollbackRefusal::TargetNotRetained {
            version: version("1.0.0")
        })
    );
    assert_eq!(std::fs::read(root.current_binary()).unwrap(), second_bytes);
}

#[test]
fn a_rollback_target_missing_its_binary_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    std::fs::remove_file(root.root().join("versions/1.0.0/heycode")).unwrap();

    assert_eq!(
        root.rollback(
            ConfigVersionState::Current(18),
            &PluginCompatibilitySet::empty(),
            &trust(),
            &FixtureVerifier
        )
        .unwrap(),
        RollbackOutcome::Refused(RollbackRefusal::TargetNotRetained {
            version: version("1.0.0")
        })
    );
    assert_eq!(std::fs::read(root.current_binary()).unwrap(), second_bytes);
}

/// Deriving retention from the disk means more than "the directory is gone".
/// A directory left behind by a partial write or a partial deletion is not a
/// version anything may roll back to, and reporting it as retained would offer
/// an operator a target the rollback then refuses.
#[test]
fn a_version_directory_without_a_valid_release_record_is_not_reported_as_retained() {
    let temp = tempfile::tempdir().unwrap();
    let root = root(&temp);
    let (first, _first_bytes) = verified_release("1.0.0", 18, 1);
    let (second, _second_bytes) = approved_update("1.0.0", "2.0.0", 18, 1);
    root.install(first).unwrap();
    root.install(second).unwrap();
    std::fs::remove_file(root.root().join("versions/1.0.0/release.json")).unwrap();

    assert!(
        root.root().join("versions/1.0.0").is_dir(),
        "the directory must survive or this test proves nothing"
    );
    assert_eq!(
        root.retained_versions().unwrap(),
        vec![version("2.0.0")],
        "a directory is not a retained version; a valid record is"
    );
}

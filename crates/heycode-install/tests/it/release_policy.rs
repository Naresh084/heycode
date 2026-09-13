//! Q15 stable/preview/pinned and plugin-host API compatibility policy.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_config::ConfigVersionState;
use heycode_install::{
    InstallRoot, PluginApiCompatibility, PluginCompatibilitySet, ReleaseChannel, ReleasePolicy,
    ReleasePolicyRefusal, ReleasePolicyVerdict, RollbackOutcome, RollbackRefusal, UpdateError,
};

use super::support::{
    FixtureVerifier, approved_update, attested_release, platform, trust, verified_release, version,
};

fn policy(channel: ReleaseChannel) -> ReleasePolicy {
    ReleasePolicy::new(channel, PluginCompatibilitySet::empty())
}

#[test]
fn stable_accepts_only_a_semantically_newer_non_prerelease() {
    let (stable, _bytes, _bundle) = attested_release("2.0.0", 24, 1);
    let (preview, _bytes, _bundle) = attested_release("3.0.0-preview.1", 24, 1);

    assert!(matches!(
        policy(ReleaseChannel::Stable).evaluate(&version("1.9.9"), &stable),
        ReleasePolicyVerdict::Approved(_)
    ));
    assert_eq!(
        policy(ReleaseChannel::Stable)
            .evaluate(&version("1.9.9"), &preview)
            .refusal(),
        Some(&ReleasePolicyRefusal::PreviewNotAllowed {
            candidate: version("3.0.0-preview.1"),
        })
    );
}

#[test]
fn preview_accepts_a_newer_prerelease_but_never_an_automatic_downgrade() {
    let (newer, _bytes, _bundle) = attested_release("2.0.0-preview.1", 24, 1);
    let (older, _bytes, _bundle) = attested_release("1.9.9", 24, 1);
    let preview = policy(ReleaseChannel::Preview);

    assert!(matches!(
        preview.evaluate(&version("1.9.9"), &newer),
        ReleasePolicyVerdict::Approved(_)
    ));
    assert_eq!(
        preview.evaluate(&version("2.0.0"), &older).refusal(),
        Some(&ReleasePolicyRefusal::NotNewer {
            current: version("2.0.0"),
            candidate: version("1.9.9"),
        })
    );
}

#[test]
fn pinned_selects_only_the_exact_version_and_may_restore_an_older_pin() {
    let pin = version("1.5.0");
    let (exact, _bytes, _bundle) = attested_release("1.5.0", 24, 1);
    let (other, _bytes, _bundle) = attested_release("2.0.0", 24, 1);
    let pinned = policy(ReleaseChannel::Pinned(pin.clone()));

    assert!(matches!(
        pinned.evaluate(&version("2.0.0"), &exact),
        ReleasePolicyVerdict::Approved(_)
    ));
    assert_eq!(
        pinned.evaluate(&version("1.0.0"), &other).refusal(),
        Some(&ReleasePolicyRefusal::PinMismatch {
            pinned: pin,
            candidate: version("2.0.0"),
        })
    );
}

#[test]
fn the_current_version_is_a_no_change_not_a_second_install() {
    let (candidate, _bytes, _bundle) = attested_release("2.0.0", 24, 1);
    assert!(matches!(
        policy(ReleaseChannel::Preview).evaluate(&version("2.0.0"), &candidate),
        ReleasePolicyVerdict::Current
    ));
}

#[test]
fn an_enabled_plugin_incompatible_with_the_candidate_host_api_blocks_the_update() {
    let plugins = PluginCompatibilitySet::new(vec![
        PluginApiCompatibility::new("acme/current", 1, 2).unwrap(),
        PluginApiCompatibility::new("acme/legacy", 1, 1).unwrap(),
    ])
    .unwrap();
    let policy = ReleasePolicy::new(ReleaseChannel::Stable, plugins);
    let (candidate, _bytes, _bundle) = attested_release("2.0.0", 24, 2);

    assert_eq!(
        policy.evaluate(&version("1.0.0"), &candidate).refusal(),
        Some(&ReleasePolicyRefusal::PluginApiIncompatible {
            plugin: "acme/legacy".to_owned(),
            candidate_api: 2,
            minimum: 1,
            maximum: 1,
        })
    );
}

#[test]
fn an_existing_install_refuses_a_signed_update_that_bypassed_channel_policy() {
    let temp = tempfile::tempdir().unwrap();
    let root = InstallRoot::open(temp.path().join("install")).unwrap();
    let (first, _bytes) = verified_release("1.0.0", 24, 1);
    let (second, _bytes) = verified_release("2.0.0", 24, 1);
    root.install(first).unwrap();

    assert_eq!(
        root.install(second).unwrap_err(),
        UpdateError::UpdatePolicyRequired {
            current: version("1.0.0"),
            candidate: version("2.0.0"),
        }
    );
}

#[test]
fn an_approved_release_token_is_bound_to_the_observed_current_version() {
    let temp = tempfile::tempdir().unwrap();
    let root = InstallRoot::open(temp.path().join("install")).unwrap();
    let (first, _bytes) = verified_release("1.0.0", 24, 1);
    root.install(first).unwrap();
    let (candidate, bytes, bundle) = attested_release("2.0.0", 24, 1);
    let release_policy = policy(ReleaseChannel::Stable);
    let verdict = release_policy.evaluate(&version("0.9.0"), &candidate);
    let ReleasePolicyVerdict::Approved(approved) = verdict else {
        panic!("the policy fixture must approve the semantic update");
    };
    let artifact = approved
        .verify_artifact(&platform(), bytes, &bundle, &FixtureVerifier)
        .unwrap();

    assert_eq!(
        root.install(artifact).unwrap_err(),
        UpdateError::UpdatePolicyRequired {
            current: version("1.0.0"),
            candidate: version("2.0.0"),
        }
    );
    assert_eq!(root.state().unwrap().current, Some(version("1.0.0")));
}

#[test]
fn rollback_refuses_a_target_that_would_strand_an_enabled_plugin() {
    let temp = tempfile::tempdir().unwrap();
    let root = InstallRoot::open(temp.path().join("install")).unwrap();
    let (first, _bytes) = verified_release("1.0.0", 24, 1);
    let (second, second_bytes) = approved_update("1.0.0", "2.0.0", 24, 2);
    root.install(first).unwrap();
    root.install(second).unwrap();
    let plugins = PluginCompatibilitySet::new(vec![
        PluginApiCompatibility::new("acme/requires-v2", 2, 2).unwrap(),
    ])
    .unwrap();

    assert_eq!(
        root.rollback(
            ConfigVersionState::Current(24),
            &plugins,
            &trust(),
            &FixtureVerifier,
        )
        .unwrap(),
        RollbackOutcome::Refused(RollbackRefusal::PluginApiIncompatible {
            plugin: "acme/requires-v2".to_owned(),
            target_api: 1,
            minimum: 2,
            maximum: 2,
        })
    );
    assert_eq!(std::fs::read(root.current_binary()).unwrap(), second_bytes);
}

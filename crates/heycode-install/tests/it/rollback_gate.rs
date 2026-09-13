//! Q14/B03: whether the configuration on disk permits a rollback at all.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_config::ConfigVersionState;
use heycode_install::{RollbackRefusal, RollbackVerdict, rollback_config_verdict};

/// The failure this gate exists for: v2 migrated the configuration, and v1
/// cannot read what v2 wrote. Rolling the binary back without noticing leaves
/// an installation that starts and then fails on its own configuration.
#[test]
fn a_configuration_newer_than_the_target_binary_refuses_the_rollback() {
    assert_eq!(
        rollback_config_verdict(ConfigVersionState::Current(19), 18),
        RollbackVerdict::Refused(RollbackRefusal::ConfigurationTooNew {
            document: 19,
            target_supports: 18,
        })
    );
}

#[test]
fn a_configuration_the_target_binary_still_understands_permits_the_rollback() {
    assert_eq!(
        rollback_config_verdict(ConfigVersionState::Current(18), 18),
        RollbackVerdict::Permitted
    );
    assert_eq!(
        rollback_config_verdict(ConfigVersionState::Older(17), 18),
        RollbackVerdict::Permitted
    );
}

/// An unversioned document predates the marker entirely, so every binary that
/// has ever shipped reads it the same way. Refusing here would block rollback
/// for exactly the oldest installations that most need it.
#[test]
fn an_unversioned_configuration_permits_the_rollback() {
    assert_eq!(
        rollback_config_verdict(ConfigVersionState::Unversioned, 1),
        RollbackVerdict::Permitted
    );
}

/// `Newer` is relative to the binary doing the classifying, not to the
/// rollback target. A document the *running* binary calls `Newer` may still be
/// readable by the target, and the gate must compare against the target.
#[test]
fn the_verdict_compares_against_the_target_not_the_classifying_binary() {
    assert_eq!(
        rollback_config_verdict(ConfigVersionState::Newer(20), 20),
        RollbackVerdict::Permitted
    );
    assert_eq!(
        rollback_config_verdict(ConfigVersionState::Newer(20), 19),
        RollbackVerdict::Refused(RollbackRefusal::ConfigurationTooNew {
            document: 20,
            target_supports: 19,
        })
    );
}

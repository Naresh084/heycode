//! PL06 CLI parity over the shared lifecycle layer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_cli::plugin_cli;
use heycode_extensions::lifecycle::{
    InstalledVersions, LifecycleError, PluginLifecycle, PluginOperation, PluginState,
    PluginStateStore,
};
use heycode_extensions::{PluginId, PluginVersion};

#[derive(Default)]
struct MemoryStore(Mutex<BTreeMap<String, PluginState>>);

impl PluginStateStore for MemoryStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        *self.0.lock().unwrap() = states.clone();
        Ok(())
    }
}

struct Cached(Vec<PluginVersion>);
impl InstalledVersions for Cached {
    fn versions(&self, _id: &PluginId) -> Vec<PluginVersion> {
        self.0.clone()
    }
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn lifecycle() -> PluginLifecycle {
    PluginLifecycle::new(
        Arc::new(MemoryStore::default()) as Arc<dyn PluginStateStore>,
        Arc::new(Cached(vec![
            PluginVersion::parse("1.0.0").unwrap(),
            PluginVersion::parse("2.0.0").unwrap(),
        ])) as Arc<dyn InstalledVersions>,
    )
}

/// THE parity test: every lifecycle operation is reachable from the CLI and
/// dispatches to itself.
#[test]
fn every_lifecycle_operation_is_reachable_from_the_command_line() {
    for operation in PluginOperation::ALL {
        let argv = match operation {
            PluginOperation::Install => args(&["install", "acme/tools", "1.0.0"]),
            PluginOperation::Update => args(&["update", "acme/tools", "2.0.0"]),
            PluginOperation::Enable => args(&["enable", "acme/tools"]),
            PluginOperation::Disable => args(&["disable", "acme/tools"]),
            PluginOperation::Rollback => args(&["rollback", "acme/tools"]),
            PluginOperation::Remove => args(&["remove", "acme/tools"]),
        };
        let parsed = plugin_cli::parse(&argv)
            .unwrap_or_else(|error| panic!("`{operation}` unreachable: {error}"))
            .unwrap_or_else(|| panic!("`{operation}` parsed as the listing"));
        assert_eq!(parsed.operation(), operation, "`{operation}` misdispatched");
        assert!(plugin_cli::usage().contains(operation.as_str()));
    }
}

#[test]
fn a_plugin_installs_updates_rolls_back_and_is_removed_end_to_end() {
    let lifecycle = lifecycle();
    let run = |argv: &[&str]| {
        let command = plugin_cli::parse(&args(argv)).expect("parses");
        plugin_cli::run(&lifecycle, command.as_ref())
    };

    assert_eq!(run(&["list"]).unwrap(), "no plugins installed");
    assert_eq!(
        run(&["install", "acme/tools", "1.0.0"]).unwrap(),
        "installed `acme/tools` 1.0.0"
    );
    let listed = run(&["list"]).unwrap();
    assert!(listed.contains("acme/tools"), "{listed}");
    assert!(listed.contains("enabled"), "{listed}");
    assert!(
        listed.contains("(no rollback target)"),
        "a fresh install must say it has nowhere to roll back to: {listed}"
    );

    assert_eq!(
        run(&["update", "acme/tools", "2.0.0"]).unwrap(),
        "updated `acme/tools` to 2.0.0"
    );
    assert!(run(&["list"]).unwrap().contains("rollback -> 1.0.0"));

    assert_eq!(
        run(&["rollback", "acme/tools"]).unwrap(),
        "rolled `acme/tools` back to 1.0.0"
    );
    assert_eq!(
        run(&["disable", "acme/tools"]).unwrap(),
        "disabled `acme/tools`"
    );
    assert!(run(&["list"]).unwrap().contains("disabled"));

    assert_eq!(
        run(&["remove", "acme/tools"]).unwrap(),
        "removed `acme/tools`"
    );
    assert_eq!(run(&["list"]).unwrap(), "no plugins installed");
}

/// `list` is a query, not a lifecycle transition, so it is deliberately outside
/// the closed operation set — and parses to no command.
#[test]
fn list_is_a_query_and_not_part_of_the_operation_set() {
    assert!(plugin_cli::parse(&args(&["list"])).unwrap().is_none());
    assert_eq!(PluginOperation::parse("list"), None);
    assert!(plugin_cli::usage().contains("list"));
}

#[test]
fn a_malformed_id_or_version_is_refused_at_the_boundary() {
    for argv in [
        args(&["install", "no-slash", "1.0.0"]),
        args(&["install", "acme/tools", "not-a-version"]),
        args(&["install", "acme/tools"]),
        args(&["enable"]),
    ] {
        assert!(plugin_cli::parse(&argv).is_err(), "accepted {argv:?}");
    }
}

#[test]
fn an_unknown_operation_lists_the_available_ones() {
    let error = plugin_cli::parse(&args(&["frobnicate"])).unwrap_err();
    assert!(
        error.contains("unknown plugin operation `frobnicate`"),
        "{error}"
    );
    for operation in PluginOperation::ALL {
        assert!(error.contains(operation.as_str()), "{error}");
    }
    assert!(
        plugin_cli::parse(&[])
            .unwrap_err()
            .contains("usage: heycode plugin")
    );
}

#[test]
fn lifecycle_failures_reach_the_operator_verbatim() {
    let lifecycle = lifecycle();
    let run = |argv: &[&str]| {
        let command = plugin_cli::parse(&args(argv)).expect("parses");
        plugin_cli::run(&lifecycle, command.as_ref())
    };
    run(&["install", "acme/tools", "1.0.0"]).unwrap();

    assert!(
        run(&["install", "acme/tools", "2.0.0"])
            .unwrap_err()
            .contains("already installed")
    );
    assert!(
        run(&["rollback", "acme/tools"])
            .unwrap_err()
            .contains("no previous version")
    );
    assert!(
        run(&["update", "acme/tools", "9.9.9"])
            .unwrap_err()
            .contains("no installed version `9.9.9`")
    );
    assert!(
        run(&["remove", "ghost/plugin"])
            .unwrap_err()
            .contains("not installed")
    );
}

/// `enable` and `disable` are two words over one command carrying a flag; the
/// flag must survive parsing in both directions.
#[test]
fn enable_and_disable_are_distinct_operations_over_one_command() {
    let enable = plugin_cli::parse(&args(&["enable", "acme/tools"]))
        .unwrap()
        .unwrap();
    let disable = plugin_cli::parse(&args(&["disable", "acme/tools"]))
        .unwrap()
        .unwrap();
    assert_eq!(enable.operation(), PluginOperation::Enable);
    assert_eq!(disable.operation(), PluginOperation::Disable);
    assert_ne!(enable, disable);
}

/// A version that is not in the cache cannot be installed, and asking does not
/// create the cache. Declarative packages need no managed policy; the policy
/// gate applies to packages that ship code (see the e2e install journey).
#[test]
fn production_lifecycle_refuses_an_uncached_version_without_creating_the_cache() {
    let root = tempfile::tempdir().unwrap();
    let (mut context, lifecycle) = plugin_cli::compose_lifecycle_world(
        root.path().join("settings.toml"),
        root.path().join("plugin-cache"),
    )
    .unwrap();
    let error = lifecycle
        .install(
            &PluginId::new("acme/tools").unwrap(),
            &PluginVersion::parse("1.0.0").unwrap(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        !error.contains("managed plugin policy"),
        "a missing package is not a policy failure: {error}"
    );
    assert!(!root.path().join("plugin-cache").exists());
    context.shutdown();
}

#[test]
fn an_absent_package_cache_projects_as_verified_empty_without_creating_it() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("absent-cache");
    let index = plugin_cli::package_index(&cache).unwrap();
    assert!(index.versions("acme/tools").is_empty());
    assert!(!cache.exists());
}

#[cfg(unix)]
#[test]
fn a_dangling_cache_symlink_is_not_misreported_as_absent() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("plugin-cache");
    symlink(root.path().join("missing-target"), &cache).unwrap();
    assert!(plugin_cli::package_index(&cache).is_err());
}

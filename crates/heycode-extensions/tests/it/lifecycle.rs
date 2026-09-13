//! PL06: the six lifecycle operations, and what a failed transition must not do.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_extensions::lifecycle::{
    InstalledVersions, LifecycleError, PluginLifecycle, PluginOperation, PluginState,
    PluginStateStore, RequireManagedPluginPolicy, SERVICE_PLUGIN_LIFECYCLE,
    plugin_lifecycle_plugin,
};
use heycode_extensions::{PluginId, PluginVersion};

#[derive(Default)]
struct MemoryStore {
    states: Mutex<BTreeMap<String, PluginState>>,
    writes: Mutex<usize>,
}

impl PluginStateStore for MemoryStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        Ok(self.states.lock().unwrap().clone())
    }
    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        *self.states.lock().unwrap() = states.clone();
        *self.writes.lock().unwrap() += 1;
        Ok(())
    }
}

struct Cached(Vec<PluginVersion>);
impl InstalledVersions for Cached {
    fn versions(&self, _id: &PluginId) -> Vec<PluginVersion> {
        self.0.clone()
    }
}

fn id() -> PluginId {
    PluginId::new("acme/tools").unwrap()
}
fn v(text: &str) -> PluginVersion {
    PluginVersion::parse(text).unwrap()
}

fn lifecycle(cached: &[&str]) -> (Arc<MemoryStore>, PluginLifecycle) {
    let store = Arc::new(MemoryStore::default());
    let cache = Arc::new(Cached(cached.iter().map(|t| v(t)).collect()));
    let lifecycle = PluginLifecycle::new(
        store.clone() as Arc<dyn PluginStateStore>,
        cache as Arc<dyn InstalledVersions>,
    );
    (store, lifecycle)
}

/// The parity mechanism, as in MCP10: every operation is in `ALL` and round
/// trips through its word, so a surface can enumerate them and no operation can
/// be quietly unreachable.
#[test]
fn every_operation_is_listed_in_all_and_round_trips_through_its_word() {
    assert_eq!(PluginOperation::ALL.len(), 6);
    let words: Vec<_> = PluginOperation::ALL.iter().map(|o| o.as_str()).collect();
    assert_eq!(
        words,
        vec![
            "install", "enable", "disable", "update", "rollback", "remove"
        ]
    );
    for operation in PluginOperation::ALL {
        assert_eq!(PluginOperation::parse(operation.as_str()), Some(operation));
    }
    assert_eq!(PluginOperation::parse("frobnicate"), None);
}

#[test]
fn install_makes_a_cached_version_active_and_enabled_with_no_rollback_target() {
    let (_store, lifecycle) = lifecycle(&["1.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();

    let states = lifecycle.list().unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].active, v("1.0.0"));
    assert!(states[0].enabled);
    assert_eq!(
        states[0].previous, None,
        "a fresh install has nothing to roll back to"
    );
}

/// Installing over an existing plugin would silently discard its rollback
/// target — moving between versions is `update`.
#[test]
fn install_refuses_a_plugin_that_is_already_installed() {
    let (_store, lifecycle) = lifecycle(&["1.0.0", "2.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    assert_eq!(
        lifecycle.install(&id(), &v("2.0.0")).unwrap_err(),
        LifecycleError::AlreadyInstalled("acme/tools".to_owned())
    );
    assert_eq!(lifecycle.list().unwrap()[0].active, v("1.0.0"));
}

#[test]
fn enable_and_disable_toggle_without_touching_versions() {
    let (_store, lifecycle) = lifecycle(&["1.0.0", "2.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    lifecycle.update(&id(), &v("2.0.0")).unwrap();

    lifecycle.set_enabled(&id(), false).unwrap();
    let state = lifecycle.list().unwrap().remove(0);
    assert!(!state.enabled);
    assert_eq!(state.active, v("2.0.0"));
    assert_eq!(state.previous, Some(v("1.0.0")));

    lifecycle.set_enabled(&id(), true).unwrap();
    assert!(lifecycle.list().unwrap()[0].enabled);
}

#[test]
fn update_moves_forward_and_remembers_the_version_it_left() {
    let (_store, lifecycle) = lifecycle(&["1.0.0", "2.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    lifecycle.update(&id(), &v("2.0.0")).unwrap();

    let state = lifecycle.list().unwrap().remove(0);
    assert_eq!(state.active, v("2.0.0"));
    assert_eq!(state.previous, Some(v("1.0.0")));
}

/// K10's rule applied to plugins: a transition that cannot complete leaves the
/// last good state untouched — including the rollback target.
#[test]
fn an_update_to_an_uncached_version_changes_nothing_at_all() {
    let (store, lifecycle) = lifecycle(&["1.0.0", "2.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    lifecycle.update(&id(), &v("2.0.0")).unwrap();
    let writes_before = *store.writes.lock().unwrap();

    assert_eq!(
        lifecycle.update(&id(), &v("9.9.9")).unwrap_err(),
        LifecycleError::VersionUnavailable {
            id: "acme/tools".to_owned(),
            version: "9.9.9".to_owned()
        }
    );

    let state = lifecycle.list().unwrap().remove(0);
    assert_eq!(state.active, v("2.0.0"), "the active version must survive");
    assert_eq!(
        state.previous,
        Some(v("1.0.0")),
        "a failed update must not consume the rollback target"
    );
    assert_eq!(
        *store.writes.lock().unwrap(),
        writes_before,
        "a failed update must not write"
    );
}

#[test]
fn updating_to_the_active_version_is_reported_rather_than_silently_accepted() {
    let (_store, lifecycle) = lifecycle(&["1.0.0", "2.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    lifecycle.update(&id(), &v("2.0.0")).unwrap();

    assert_eq!(
        lifecycle.update(&id(), &v("2.0.0")).unwrap_err(),
        LifecycleError::AlreadyAtVersion {
            id: "acme/tools".to_owned(),
            version: "2.0.0".to_owned()
        }
    );
    assert_eq!(
        lifecycle.list().unwrap()[0].previous,
        Some(v("1.0.0")),
        "a no-op update must not overwrite the rollback target with itself"
    );
}

#[test]
fn rollback_returns_to_the_previous_version() {
    let (_store, lifecycle) = lifecycle(&["1.0.0", "2.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    lifecycle.update(&id(), &v("2.0.0")).unwrap();

    assert_eq!(lifecycle.rollback(&id()).unwrap(), v("1.0.0"));
    let state = lifecycle.list().unwrap().remove(0);
    assert_eq!(state.active, v("1.0.0"));
}

/// An operator who rolls back by mistake is one command from undoing it.
#[test]
fn a_rollback_can_itself_be_rolled_back() {
    let (_store, lifecycle) = lifecycle(&["1.0.0", "2.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    lifecycle.update(&id(), &v("2.0.0")).unwrap();

    lifecycle.rollback(&id()).unwrap();
    assert_eq!(lifecycle.list().unwrap()[0].active, v("1.0.0"));
    assert_eq!(lifecycle.rollback(&id()).unwrap(), v("2.0.0"));
    assert_eq!(lifecycle.list().unwrap()[0].active, v("2.0.0"));
}

#[test]
fn rollback_with_no_previous_version_is_refused() {
    let (_store, lifecycle) = lifecycle(&["1.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    assert_eq!(
        lifecycle.rollback(&id()).unwrap_err(),
        LifecycleError::NothingToRollBackTo("acme/tools".to_owned())
    );
}

/// Retention is the cache's policy, not this layer's assumption. A remembered
/// version that has been pruned must fail loudly rather than leave a reference
/// to something that cannot activate.
#[test]
fn rollback_to_a_version_the_cache_no_longer_holds_fails_loudly() {
    let store = Arc::new(MemoryStore::default());
    store
        .persist(&BTreeMap::from([(
            "acme/tools".to_owned(),
            PluginState {
                id: id(),
                active: v("2.0.0"),
                previous: Some(v("1.0.0")),
                enabled: true,
            },
        )]))
        .unwrap();
    // The cache has since pruned 1.0.0.
    let lifecycle = PluginLifecycle::new(
        store as Arc<dyn PluginStateStore>,
        Arc::new(Cached(vec![v("2.0.0")])) as Arc<dyn InstalledVersions>,
    );

    assert_eq!(
        lifecycle.rollback(&id()).unwrap_err(),
        LifecycleError::VersionUnavailable {
            id: "acme/tools".to_owned(),
            version: "1.0.0".to_owned()
        }
    );
    assert_eq!(
        lifecycle.list().unwrap()[0].active,
        v("2.0.0"),
        "a refused rollback must leave the active version alone"
    );
}

#[test]
fn remove_forgets_the_plugin_and_an_unknown_id_is_reported() {
    let (_store, lifecycle) = lifecycle(&["1.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();

    let ghost = PluginId::new("ghost/plugin").unwrap();
    assert_eq!(
        lifecycle.remove(&ghost).unwrap_err(),
        LifecycleError::NotInstalled("ghost/plugin".to_owned())
    );
    assert_eq!(lifecycle.list().unwrap().len(), 1);

    lifecycle.remove(&id()).unwrap();
    assert!(lifecycle.list().unwrap().is_empty());
}

#[test]
fn missing_managed_authority_denies_every_activation_increase_but_allows_reduction() {
    let store = Arc::new(MemoryStore::default());
    store
        .persist(&BTreeMap::from([(
            "acme/tools".to_owned(),
            PluginState {
                id: id(),
                active: v("1.0.0"),
                previous: Some(v("2.0.0")),
                enabled: false,
            },
        )]))
        .unwrap();
    let lifecycle = PluginLifecycle::with_admission(
        store.clone() as Arc<dyn PluginStateStore>,
        Arc::new(Cached(vec![v("1.0.0"), v("2.0.0")])) as Arc<dyn InstalledVersions>,
        Arc::new(RequireManagedPluginPolicy),
    );
    let writes = *store.writes.lock().unwrap();
    for error in [
        lifecycle.set_enabled(&id(), true).unwrap_err(),
        lifecycle.update(&id(), &v("2.0.0")).unwrap_err(),
        lifecycle.rollback(&id()).unwrap_err(),
    ] {
        assert!(matches!(
            error,
            LifecycleError::ManagedPolicyUnavailable { .. }
        ));
    }
    assert_eq!(*store.writes.lock().unwrap(), writes);

    lifecycle.set_enabled(&id(), false).unwrap();
    lifecycle.remove(&id()).unwrap();
    assert!(lifecycle.list().unwrap().is_empty());
}

#[test]
fn lifecycle_plugin_publishes_one_effect_owned_service_with_managed_admission() {
    let plugins = vec![
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        plugin_lifecycle_plugin(
            Arc::new(Cached(vec![v("1.0.0")])) as Arc<dyn InstalledVersions>,
            Arc::new(RequireManagedPluginPolicy),
        ),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    assert_eq!(
        context.owner_of(SERVICE_PLUGIN_LIFECYCLE),
        Some("plugin-lifecycle")
    );
    let lifecycle = context
        .get::<PluginLifecycle>(SERVICE_PLUGIN_LIFECYCLE)
        .expect("the lifecycle service is published");
    assert!(lifecycle.list().unwrap().is_empty());
    assert!(matches!(
        lifecycle.install(&id(), &v("1.0.0")),
        Err(LifecycleError::ManagedPolicyUnavailable { .. })
    ));
    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert_eq!(
        inventory
            .contributions
            .iter()
            .filter(|row| row.plugin == "plugin-lifecycle")
            .map(|row| (row.kind, row.name.as_str()))
            .collect::<Vec<_>>(),
        [
            (heycode_core::ContributionKind::SettingsNamespace, "plugins"),
            (
                heycode_core::ContributionKind::Service,
                SERVICE_PLUGIN_LIFECYCLE.as_str()
            ),
        ]
    );

    context.shutdown();
    assert_eq!(lifecycle.list().unwrap_err(), LifecycleError::Closed);
}

#[test]
fn every_operation_on_an_uninstalled_plugin_names_it() {
    let (_store, lifecycle) = lifecycle(&["1.0.0"]);
    let ghost = PluginId::new("ghost/plugin").unwrap();
    for error in [
        lifecycle.set_enabled(&ghost, true).unwrap_err(),
        lifecycle.update(&ghost, &v("1.0.0")).unwrap_err(),
        lifecycle.rollback(&ghost).unwrap_err(),
        lifecycle.remove(&ghost).unwrap_err(),
    ] {
        assert_eq!(
            error,
            LifecycleError::NotInstalled("ghost/plugin".to_owned())
        );
    }
}

/// Installing a version the cache does not hold must fail before any state is
/// recorded, or `list` would advertise a plugin that cannot activate.
#[test]
fn installing_an_uncached_version_records_nothing() {
    let (store, lifecycle) = lifecycle(&["1.0.0"]);
    assert!(matches!(
        lifecycle.install(&id(), &v("3.0.0")).unwrap_err(),
        LifecycleError::VersionUnavailable { .. }
    ));
    assert!(lifecycle.list().unwrap().is_empty());
    assert_eq!(*store.writes.lock().unwrap(), 0);
}

/// A read must never write — `list` behind a refreshing panel would otherwise
/// rewrite state on every frame.
#[test]
fn listing_never_writes() {
    let (store, lifecycle) = lifecycle(&["1.0.0"]);
    lifecycle.install(&id(), &v("1.0.0")).unwrap();
    let writes = *store.writes.lock().unwrap();
    lifecycle.list().unwrap();
    assert_eq!(*store.writes.lock().unwrap(), writes);
}

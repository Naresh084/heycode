//! The plugin context: service map, effects stack, event bus.

use std::any::Any;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use crate::error::CoreError;
use crate::events::EventBus;
use crate::{PluginDescriptor, ServiceKey};

type Disposer = Box<dyn FnOnce() + Send>;

/// The runtime context plugins contribute into.
///
/// A `Context` holds:
/// - a type-erased **service map** keyed by typed [`ServiceKey`] constants,
/// - an **effects stack** whose disposers unwind LIFO on [`Context::shutdown`],
/// - the shared [`EventBus`].
pub struct Context {
    services: HashMap<ServiceKey, (Arc<dyn Any + Send + Sync>, &'static str)>,
    plugins: Vec<&'static str>,
    plugin_descriptors: Vec<PluginDescriptor>,
    plugin_scopes: Vec<crate::PluginScope>,
    inventory: crate::PluginInventory,
    applying_plugin: Option<&'static str>,
    activation: Option<crate::activation::ActivationTransaction>,
    effects: Mutex<Vec<Disposer>>,
    /// The shared event bus for cross-plugin notifications.
    pub events: EventBus,
    closed: bool,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    /// An empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            plugins: Vec::new(),
            plugin_descriptors: Vec::new(),
            plugin_scopes: Vec::new(),
            inventory: crate::PluginInventory::default(),
            applying_plugin: None,
            activation: None,
            effects: Mutex::new(Vec::new()),
            events: EventBus::default(),
            closed: false,
        }
    }

    /// Publish a service under `key`. A second claimant for a live key fails loud.
    ///
    /// # Errors
    /// [`CoreError::DuplicateService`] when `key` is already claimed.
    pub fn provide<T: Send + Sync + 'static>(
        &mut self,
        key: ServiceKey,
        owner: &'static str,
        service: T,
    ) -> Result<(), CoreError> {
        if self.services.contains_key(&key) {
            let existing = self.services[&key].1;
            return Err(CoreError::DuplicateService {
                key: key.to_string(),
                existing: existing.to_owned(),
                claimant: owner.to_owned(),
            });
        }
        let plugin = self.applying_plugin.unwrap_or(owner);
        self.inventory.contribute(
            plugin,
            crate::PluginContributionSpec::new(crate::ContributionKind::Service, key.as_str()),
        )?;
        self.services.insert(key, (Arc::new(service), owner));
        if let Some(activation) = self.activation.as_mut() {
            activation.services.push(key);
        }
        Ok(())
    }

    /// Fetch a shared handle to the service registered under `key`.
    #[must_use]
    pub fn get<T: Send + Sync + 'static>(&self, key: ServiceKey) -> Option<Arc<T>> {
        self.services
            .get(&key)
            .and_then(|(svc, _)| svc.clone().downcast::<T>().ok())
    }

    /// Whether `key` has been provided.
    #[must_use]
    pub fn has(&self, key: ServiceKey) -> bool {
        self.services.contains_key(&key)
    }

    /// Which plugin owns `key` (diagnostics).
    #[must_use]
    pub fn owner_of(&self, key: ServiceKey) -> Option<&'static str> {
        self.services.get(&key).map(|(_, owner)| *owner)
    }

    /// All provided keys with owners, unsorted insertion order (diagnostics).
    #[must_use]
    pub fn services(&self) -> Vec<(ServiceKey, &'static str)> {
        self.services.iter().map(|(k, (_, o))| (*k, *o)).collect()
    }

    /// Successfully applied plugin names in composition order.
    ///
    /// A plugin appears only after its [`crate::Plugin::apply`] call succeeds,
    /// so this is a truthful inventory of the live context.
    #[must_use]
    pub fn plugins(&self) -> &[&'static str] {
        &self.plugins
    }

    /// Descriptors for successfully applied plugins in composition order.
    #[must_use]
    pub fn plugin_descriptors(&self) -> &[PluginDescriptor] {
        &self.plugin_descriptors
    }

    /// Effective activation scopes in the same order as [`Self::plugins`].
    #[must_use]
    pub fn plugin_scopes(&self) -> &[crate::PluginScope] {
        &self.plugin_scopes
    }

    /// Shared exact named contribution inventory.
    #[must_use]
    pub fn plugin_inventory(&self) -> crate::PluginInventory {
        self.inventory.clone()
    }

    /// Register a dynamic exact row for the plugin currently applying.
    ///
    /// # Errors
    /// Registration outside apply, invalid names, duplicates, or inventory
    /// lock failure.
    pub fn contribute(
        &self,
        kind: crate::ContributionKind,
        name: impl Into<String>,
    ) -> Result<(), CoreError> {
        let plugin = self
            .applying_plugin
            .ok_or(CoreError::ContributionOutsideApply)?;
        self.inventory
            .contribute(plugin, crate::PluginContributionSpec::new(kind, name))
    }

    /// Open the activation transaction for `plugin`, capturing the state its
    /// rollback must restore.
    pub(crate) fn begin_activation(&mut self, plugin: &'static str) -> Result<(), CoreError> {
        let before = crate::activation::ContextFingerprint::capture(self)?;
        self.activation = Some(crate::activation::ActivationTransaction {
            services: Vec::new(),
            contributions: before.contribution_count(),
            effects: before.effect_count(),
            before,
        });
        self.applying_plugin = Some(plugin);
        Ok(())
    }

    /// Publish the open transaction: from here its contributions are live.
    pub(crate) fn commit_activation(&mut self) {
        self.applying_plugin = None;
        self.activation = None;
    }

    /// Undo the open transaction and verify that nothing it registered
    /// survived. Returns a description of the residue when verification fails.
    pub(crate) fn rollback_activation(&mut self) -> Option<String> {
        self.applying_plugin = None;
        let transaction = self.activation.take()?;
        for key in &transaction.services {
            self.services.remove(key);
        }
        let unwound = self.inventory.rollback(transaction.contributions);
        self.dispose_effects_above(transaction.effects);
        if !unwound {
            return Some("inventory unavailable".to_owned());
        }
        match crate::activation::ContextFingerprint::capture(self) {
            Ok(after) => transaction.before.residue(&after),
            Err(_) => Some("inventory unavailable".to_owned()),
        }
    }

    /// Number of registered disposers awaiting shutdown.
    pub(crate) fn pending_effects(&self) -> usize {
        self.effects.lock().map_or(0, |effects| effects.len())
    }

    /// Run and drop every disposer registered after `floor`, LIFO.
    fn dispose_effects_above(&self, floor: usize) {
        let unwind = match self.effects.lock() {
            Ok(mut effects) if effects.len() > floor => effects.split_off(floor),
            _ => return,
        };
        for disposer in unwind.into_iter().rev() {
            let _ = catch_unwind(AssertUnwindSafe(disposer));
        }
    }

    pub(crate) fn record_plugin(
        &mut self,
        descriptor: PluginDescriptor,
        scope: crate::PluginScope,
    ) -> Result<(), CoreError> {
        self.inventory.record_plugin(descriptor, scope)?;
        self.plugins.push(descriptor.id);
        self.plugin_descriptors.push(descriptor);
        self.plugin_scopes.push(scope);
        Ok(())
    }

    /// Register a disposer to run on shutdown (LIFO).
    pub fn effect(&self, disposer: impl FnOnce() + Send + 'static) {
        if let Ok(mut effects) = self.effects.lock() {
            effects.push(Box::new(disposer));
        }
    }

    /// Run every disposer LIFO. Panicking disposers are contained so the rest
    /// still unwind. Idempotent; further provides are rejected afterwards.
    pub fn shutdown(&mut self) {
        self.closed = true;
        let effects = match self.effects.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(_) => return,
        };
        for disposer in effects.into_iter().rev() {
            let _ = catch_unwind(AssertUnwindSafe(disposer));
        }
    }

    /// True after [`Context::shutdown`].
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Every injected key from `inject` must already be present.
    ///
    /// # Errors
    /// [`CoreError::UnsatisfiedInject`] naming the missing keys.
    pub fn verify_injects(
        &self,
        plugin_name: &str,
        inject: &[ServiceKey],
    ) -> Result<(), CoreError> {
        let missing: Vec<String> = inject
            .iter()
            .filter(|key| !self.has(**key))
            .map(|key| key.to_string())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(CoreError::UnsatisfiedInject {
                plugin: plugin_name.to_owned(),
                missing,
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::CoreError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter(i32);
    const A: crate::ServiceKey = crate::ServiceKey::new("a");
    const B: crate::ServiceKey = crate::ServiceKey::new("b");
    const C: crate::ServiceKey = crate::ServiceKey::new("c");
    const COUNTER: crate::ServiceKey = crate::ServiceKey::new("counter");
    const NOPE: crate::ServiceKey = crate::ServiceKey::new("nope");
    const THING: crate::ServiceKey = crate::ServiceKey::new("thing");

    #[test]
    fn provide_then_get_round_trips() {
        let mut ctx = Context::new();
        ctx.provide(COUNTER, "p", Counter(7)).unwrap();
        let c = ctx.get::<Counter>(COUNTER).unwrap();
        assert_eq!(c.0, 7);
        assert!(ctx.has(COUNTER));
        assert_eq!(ctx.owner_of(COUNTER), Some("p"));
        assert_eq!(COUNTER.as_str(), "counter");
    }

    #[test]
    fn duplicate_service_fails_loud_naming_both_plugins() {
        let mut ctx = Context::new();
        ctx.provide::<Counter>(COUNTER, "a", Counter(1)).unwrap();
        let err = ctx
            .provide::<Counter>(COUNTER, "b", Counter(2))
            .unwrap_err();
        match err {
            CoreError::DuplicateService {
                key,
                existing,
                claimant,
            } => {
                assert_eq!(
                    (key.as_str(), existing.as_str(), claimant.as_str()),
                    ("counter", "a", "b")
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn get_with_wrong_type_is_none() {
        let mut ctx = Context::new();
        ctx.provide(THING, "p", 42_i64).unwrap();
        assert!(ctx.get::<String>(THING).is_none());
        assert!(ctx.get::<i64>(NOPE).is_none());
    }

    #[test]
    fn effects_unwind_in_reverse_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut ctx = Context::new();
        for name in ["first", "second", "third"] {
            let order = order.clone();
            ctx.effect(move || order.lock().unwrap().push(name));
        }
        ctx.shutdown();
        assert_eq!(*order.lock().unwrap(), vec!["third", "second", "first"]);
    }

    #[test]
    fn panicking_disposer_does_not_block_the_rest() {
        let hits = Arc::new(AtomicUsize::new(0));
        let mut ctx = Context::new();
        let h2 = hits.clone();
        ctx.effect(move || {
            h2.fetch_add(1, Ordering::SeqCst);
        });
        ctx.effect(|| panic!("disposer exploded"));
        ctx.shutdown();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(ctx.is_closed());
    }

    #[test]
    fn verify_injects_names_missing_keys() {
        let mut ctx = Context::new();
        ctx.provide::<Counter>(A, "p", Counter(0)).unwrap();
        let ok = ctx.verify_injects("plug", &[A]);
        assert!(ok.is_ok());
        let err = ctx.verify_injects("plug", &[A, B, C]).unwrap_err();
        match err {
            CoreError::UnsatisfiedInject { plugin, missing } => {
                assert_eq!(plugin, "plug");
                assert_eq!(missing, vec!["b".to_owned(), "c".to_owned()]);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

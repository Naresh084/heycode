//! Namespace resolution, immutable snapshots, and provider-backed replacement.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, Weak};

use heycode_core::Context;
use serde_json::Value;

use crate::redaction::{
    ExposureInputs, FieldRoles, leaf_paths, paths_overlap, redact_for_debug, scrub_message,
    verify_and_project,
};
use crate::{
    SettingsApplies, SettingsDefinition, SettingsDocuments, SettingsError, SettingsLayer,
    SettingsNamespace, SettingsUpdateSource, SettingsWireProjection, WireExposureFault,
};

/// Synchronous persistence boundary for the user settings document.
///
/// Implementations must make the write durable before returning `Ok`; the
/// service publishes the candidate snapshot only after this call succeeds.
pub trait SettingsWriter: Send + Sync {
    /// Persist one complete user namespace section.
    ///
    /// # Errors
    /// Return a redacted actionable message. Secret values must never appear.
    fn persist_user(&self, namespace: &SettingsNamespace, section: &Value) -> Result<(), String>;
}

/// Immutable resolved namespace and its detached contributing layers.
pub struct SettingsSnapshot {
    namespace: SettingsNamespace,
    schema: Value,
    defaults: Value,
    base: Option<Value>,
    user: Option<Value>,
    project: Option<Value>,
    override_layer: Option<Value>,
    managed: Option<Value>,
    resolved: Value,
    applies: SettingsApplies,
    revision: u64,
    managed_locks: Vec<String>,
    projection: Option<SettingsWireProjection>,
    roles: FieldRoles,
}

impl std::fmt::Debug for SettingsSnapshot {
    /// Owner values stay readable in process, but a rendered snapshot is a
    /// diagnostic plane: every declared secret path and every recognized
    /// credential shape is replaced before it can reach a log.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let render = |value: &Value| redact_for_debug(value, &self.roles);
        formatter
            .debug_struct("SettingsSnapshot")
            .field("namespace", &self.namespace)
            .field("schema", &render(&self.schema))
            .field("defaults", &render(&self.defaults))
            .field("base", &self.base.as_ref().map(&render))
            .field("user", &self.user.as_ref().map(&render))
            .field("project", &self.project.as_ref().map(&render))
            .field("override", &self.override_layer.as_ref().map(&render))
            .field("managed", &self.managed.as_ref().map(&render))
            .field("resolved", &render(&self.resolved))
            .field("applies", &self.applies)
            .field("revision", &self.revision)
            .field("managed_locks", &self.managed_locks)
            .field("wire_exposed", &self.projection.is_some())
            .finish()
    }
}

impl SettingsSnapshot {
    /// Namespace identity.
    #[must_use]
    pub fn namespace(&self) -> &SettingsNamespace {
        &self.namespace
    }

    /// JSON-schema metadata declared by the owner.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// Immutable schema-default layer.
    #[must_use]
    pub fn defaults(&self) -> &Value {
        &self.defaults
    }

    /// Optional composition base layer.
    #[must_use]
    pub fn base(&self) -> Option<&Value> {
        self.base.as_ref()
    }

    /// Optional user layer.
    #[must_use]
    pub fn user(&self) -> Option<&Value> {
        self.user.as_ref()
    }

    /// Optional project layer.
    #[must_use]
    pub fn project(&self) -> Option<&Value> {
        self.project.as_ref()
    }

    /// Optional administrator-managed layer.
    #[must_use]
    pub fn managed(&self) -> Option<&Value> {
        self.managed.as_ref()
    }

    /// Ephemeral command-line override layer, when this process was started
    /// with an explicit flag for this namespace and no in-session user write
    /// has superseded it yet.
    #[must_use]
    pub fn override_layer(&self) -> Option<&Value> {
        self.override_layer.as_ref()
    }

    /// Leaf paths the administrator layer owns, in path order.
    ///
    /// A user write naming any of these fails rather than persisting a value
    /// resolution would ignore.
    #[must_use]
    pub fn managed_locks(&self) -> &[String] {
        &self.managed_locks
    }

    /// Verified redacted projection, present only where exposure was proved.
    #[must_use]
    pub fn wire_projection(&self) -> Option<&SettingsWireProjection> {
        self.projection.as_ref()
    }

    /// Fully resolved immutable value.
    #[must_use]
    pub fn resolved(&self) -> &Value {
        &self.resolved
    }

    /// Owner-declared application timing.
    #[must_use]
    pub fn applies(&self) -> SettingsApplies {
        self.applies
    }

    /// Monotonic revision of the raw user section.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether this namespace is proved safe for local app/UI wire
    /// projection. An owner attestation alone never sets it.
    #[must_use]
    pub const fn wire_exposed(&self) -> bool {
        self.projection.is_some()
    }
}

/// One committed namespace transition delivered to watchers.
#[derive(Debug, Clone)]
pub struct SettingsChange {
    previous: Arc<SettingsSnapshot>,
    next: Arc<SettingsSnapshot>,
    source: SettingsUpdateSource,
}

impl SettingsChange {
    /// Snapshot authoritative before the commit.
    #[must_use]
    pub fn previous(&self) -> &Arc<SettingsSnapshot> {
        &self.previous
    }

    /// Snapshot authoritative after the commit.
    #[must_use]
    pub fn next(&self) -> &Arc<SettingsSnapshot> {
        &self.next
    }

    /// Commit origin.
    #[must_use]
    pub fn source(&self) -> SettingsUpdateSource {
        self.source
    }
}

type WatcherCallback = Arc<dyn Fn(SettingsChange) + Send + Sync>;

thread_local! {
    static IN_WATCHER_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

struct RegisteredNamespace {
    definition: SettingsDefinition,
    snapshot: Arc<SettingsSnapshot>,
    token: Arc<()>,
    watchers: BTreeMap<u64, WatcherCallback>,
}

struct SettingsState {
    documents: SettingsDocuments,
    registrations: BTreeMap<SettingsNamespace, RegisteredNamespace>,
    next_watcher_id: u64,
}

struct SettingsInner {
    state: Mutex<SettingsState>,
    writer: Option<Arc<dyn SettingsWriter>>,
    operations: Mutex<()>,
}

/// Shared layered settings service.
#[derive(Clone)]
pub struct SettingsService {
    inner: Arc<SettingsInner>,
}

impl SettingsService {
    /// Build a read-only service over detached provider documents.
    #[must_use]
    pub fn new(documents: SettingsDocuments) -> Self {
        Self::build(documents, None)
    }

    /// Build a writable service over detached documents and a durable writer.
    #[must_use]
    pub fn with_writer(documents: SettingsDocuments, writer: Arc<dyn SettingsWriter>) -> Self {
        Self::build(documents, Some(writer))
    }

    fn build(documents: SettingsDocuments, writer: Option<Arc<dyn SettingsWriter>>) -> Self {
        Self {
            inner: Arc::new(SettingsInner {
                state: Mutex::new(SettingsState {
                    documents,
                    registrations: BTreeMap::new(),
                    next_watcher_id: 1,
                }),
                writer,
                operations: Mutex::new(()),
            }),
        }
    }

    /// Whether user sections can be persisted.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.inner.writer.is_some()
    }

    /// Register and resolve one plugin-owned namespace as a context effect.
    ///
    /// Shutdown or a later composition failure removes the registration in
    /// normal LIFO order; consumers receive only the frozen snapshot.
    ///
    /// # Errors
    /// Duplicate namespaces, validator failures, or a poisoned registry fail
    /// loud before a snapshot is published.
    pub fn register(
        &self,
        context: &Context,
        definition: SettingsDefinition,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        let registration = self.register_owned(definition)?;
        let snapshot = registration.snapshot();
        context.effect(move || drop(registration));
        Ok(snapshot)
    }

    fn register_owned(
        &self,
        definition: SettingsDefinition,
    ) -> Result<SettingsRegistration, SettingsError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        if state.registrations.contains_key(&definition.namespace) {
            return Err(SettingsError::DuplicateNamespace {
                namespace: definition.namespace.to_string(),
            });
        }
        let snapshot = resolve(&definition, &state.documents, 0)?;
        let token = Arc::new(());
        state.registrations.insert(
            definition.namespace.clone(),
            RegisteredNamespace {
                definition: definition.clone(),
                snapshot: snapshot.clone(),
                token: token.clone(),
                watchers: BTreeMap::new(),
            },
        );
        Ok(SettingsRegistration {
            inner: Arc::downgrade(&self.inner),
            namespace: definition.namespace,
            snapshot,
            token,
            active: true,
        })
    }

    /// Replace one complete user section, persist it, then publish its new
    /// immutable resolution.
    ///
    /// # Errors
    /// Read-only/unknown namespace, stale revision, non-object section,
    /// validation, provider, or poisoned-registry failures publish no state.
    pub fn replace_user(
        &self,
        namespace: &SettingsNamespace,
        section: Value,
        expected_revision: Option<u64>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.replace_user_inner(namespace, section, expected_revision, None, false, |_| {
            Ok(())
        })
    }

    /// Replace this process's ephemeral layer without writing a document.
    /// Schema validation, managed locks, revision checks and watcher publication
    /// use the same commit boundary as persisted Settings writes. A later user
    /// write clears this layer; rebuilding the Settings service does not restore it.
    ///
    /// # Errors
    /// Invalid values, managed locks, stale revisions or registry failures.
    pub fn replace_override(
        &self,
        namespace: &SettingsNamespace,
        section: Value,
        expected_revision: Option<u64>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.replace_user_inner(
            namespace,
            section,
            expected_revision,
            None,
            true,
            |_| Ok(()),
        )
    }

    /// Commit an automatic update only against the exact snapshot read by its
    /// caller. Preserves command-line overrides and validates the fully resolved
    /// candidate before persistence or notification. The validator must not
    /// re-enter this service.
    ///
    /// # Errors
    /// Stale snapshot, validation, managed lock, or persistence failure leaves
    /// the current settings untouched.
    pub fn replace_user_automatically(
        &self,
        namespace: &SettingsNamespace,
        section: Value,
        expected: &Arc<SettingsSnapshot>,
        validate: impl FnOnce(&SettingsSnapshot) -> Result<(), SettingsError>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.replace_user_inner(namespace, section, None, Some(expected), false, validate)
    }

    fn replace_user_inner(
        &self,
        namespace: &SettingsNamespace,
        section: Value,
        expected_revision: Option<u64>,
        expected_snapshot: Option<&Arc<SettingsSnapshot>>,
        ephemeral: bool,
        validate: impl FnOnce(&SettingsSnapshot) -> Result<(), SettingsError>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        if IN_WATCHER_CALLBACK.get() {
            return Err(SettingsError::ReentrantWrite);
        }
        if !section.is_object() {
            return Err(SettingsError::LayerMustBeObject {
                namespace: namespace.to_string(),
                layer: if ephemeral {
                    SettingsLayer::Override
                } else {
                    SettingsLayer::User
                },
            });
        }
        let writer = self.inner.writer.clone();
        if !ephemeral && writer.is_none() {
            return Err(SettingsError::ReadOnly);
        }
        let _operation = self
            .inner
            .operations
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        let Some(registration) = state.registrations.get(namespace) else {
            return Err(SettingsError::UnknownNamespace {
                namespace: namespace.to_string(),
            });
        };
        if let Some(expected) = expected_snapshot
            && !Arc::ptr_eq(expected, &registration.snapshot)
        {
            return Err(SettingsError::StaleSnapshot {
                namespace: namespace.to_string(),
            });
        }
        let current_revision = registration.snapshot.revision;
        if let Some(expected) = expected_revision
            && expected != current_revision
        {
            return Err(SettingsError::Conflict {
                namespace: namespace.to_string(),
                expected,
                actual: current_revision,
            });
        }
        for leaf in leaf_paths(&section) {
            if let Some(lock) = registration
                .snapshot
                .managed_locks
                .iter()
                .find(|lock| paths_overlap(lock, &leaf))
            {
                return Err(SettingsError::ManagedLock {
                    namespace: namespace.to_string(),
                    path: lock.clone(),
                });
            }
        }
        // The user's in-session choice supersedes an ephemeral command-line
        // override: `--model x` then `/model y` must land on `y`.
        let mut definition = registration.definition.clone();
        if ephemeral {
            definition.override_layer = Some(section.clone());
        } else if expected_snapshot.is_none() {
            definition.override_layer = None;
        }
        let previous = registration.snapshot.clone();
        let callbacks: Vec<_> = registration.watchers.values().cloned().collect();
        let mut next_documents = state.documents.clone();
        if !ephemeral {
            next_documents.set_user(namespace.clone(), section.clone())?;
        }
        let next_revision = advance_revision(namespace, current_revision)?;
        let next_snapshot = resolve(&definition, &next_documents, next_revision)?;
        validate(&next_snapshot)?;

        if !ephemeral && let Some(writer) = writer {
            writer
                .persist_user(namespace, &section)
                .map_err(|message| SettingsError::Provider {
                    message: scrub_message(message),
                })?;
        }

        state.documents = next_documents;
        if let Some(registration) = state.registrations.get_mut(namespace) {
            registration.definition = definition;
            registration.snapshot = next_snapshot.clone();
        }
        drop(state);
        notify(
            &callbacks,
            SettingsChange {
                previous,
                next: next_snapshot.clone(),
                source: if ephemeral {
                    SettingsUpdateSource::OverrideWrite
                } else {
                    SettingsUpdateSource::UserWrite
                },
            },
        );
        Ok(next_snapshot)
    }

    /// Watch committed raw/resolved transitions for one namespace.
    ///
    /// Callbacks run synchronously, one commit at a time, after publication.
    /// Panics are contained so one watcher cannot starve later watchers.
    /// Shutdown of `context` removes the watcher before future commits.
    ///
    /// # Errors
    /// Unknown namespace, id exhaustion, or a poisoned registry.
    pub fn watch(
        &self,
        context: &Context,
        namespace: &SettingsNamespace,
        callback: impl Fn(SettingsChange) + Send + Sync + 'static,
    ) -> Result<(), SettingsError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        if !state.registrations.contains_key(namespace) {
            return Err(SettingsError::UnknownNamespace {
                namespace: namespace.to_string(),
            });
        }
        let watcher_id = state.next_watcher_id;
        state.next_watcher_id = state
            .next_watcher_id
            .checked_add(1)
            .ok_or(SettingsError::WatcherIdExhausted)?;
        if let Some(registration) = state.registrations.get_mut(namespace) {
            registration.watchers.insert(watcher_id, Arc::new(callback));
        }
        let handle = SettingsWatchRegistration {
            inner: Arc::downgrade(&self.inner),
            namespace: namespace.clone(),
            watcher_id,
            active: true,
        };
        context.effect(move || drop(handle));
        Ok(())
    }

    /// Publish externally reloaded provider documents.
    ///
    /// Every live namespace resolves and validates before any state commits.
    /// Changed namespaces then publish in deterministic name order.
    ///
    /// # Errors
    /// Validation/revision/registry failures keep the last good generation.
    pub fn publish_documents(&self, documents: SettingsDocuments) -> Result<(), SettingsError> {
        if IN_WATCHER_CALLBACK.get() {
            return Err(SettingsError::ReentrantWrite);
        }
        let _operation = self
            .inner
            .operations
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        let mut updates = Vec::new();
        for (namespace, registration) in &state.registrations {
            let user_changed = state.documents.user(namespace) != documents.user(namespace);
            let revision = if user_changed {
                advance_revision(namespace, registration.snapshot.revision)?
            } else {
                registration.snapshot.revision
            };
            let next = resolve(&registration.definition, &documents, revision)?;
            let changed = user_changed
                || registration.snapshot.project != next.project
                || registration.snapshot.managed != next.managed
                || registration.snapshot.resolved != next.resolved;
            if changed {
                updates.push((
                    namespace.clone(),
                    registration.snapshot.clone(),
                    next,
                    registration.watchers.values().cloned().collect::<Vec<_>>(),
                ));
            }
        }
        state.documents = documents;
        for (namespace, _previous, next, _callbacks) in &updates {
            if let Some(registration) = state.registrations.get_mut(namespace) {
                registration.snapshot = next.clone();
            }
        }
        drop(state);
        for (_namespace, previous, next, callbacks) in updates {
            notify(
                &callbacks,
                SettingsChange {
                    previous,
                    next,
                    source: SettingsUpdateSource::ProviderReload,
                },
            );
        }
        Ok(())
    }

    /// Read one registered immutable snapshot.
    ///
    /// # Errors
    /// A poisoned registry returns [`SettingsError::RegistryUnavailable`].
    pub fn get(
        &self,
        namespace: &SettingsNamespace,
    ) -> Result<Option<Arc<SettingsSnapshot>>, SettingsError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        Ok(state
            .registrations
            .get(namespace)
            .map(|registration| registration.snapshot.clone()))
    }

    /// Resolve the saved layers and composition defaults without this process's
    /// ephemeral override. This is read-only and does not publish a transition.
    ///
    /// # Errors
    /// Registry failure or invalid saved resolution.
    pub fn get_without_override(
        &self,
        namespace: &SettingsNamespace,
    ) -> Result<Option<Arc<SettingsSnapshot>>, SettingsError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        let Some(registration) = state.registrations.get(namespace) else {
            return Ok(None);
        };
        let mut definition = registration.definition.clone();
        definition.override_layer = None;
        resolve(
            &definition,
            &state.documents,
            registration.snapshot.revision,
        )
        .map(Some)
    }

    /// Describe every registered namespace in deterministic name order.
    ///
    /// # Errors
    /// A poisoned registry returns [`SettingsError::RegistryUnavailable`].
    pub fn describe(&self) -> Result<Vec<Arc<SettingsSnapshot>>, SettingsError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| SettingsError::RegistryUnavailable)?;
        Ok(state
            .registrations
            .values()
            .map(|registration| registration.snapshot.clone())
            .collect())
    }
}

struct SettingsRegistration {
    inner: Weak<SettingsInner>,
    namespace: SettingsNamespace,
    snapshot: Arc<SettingsSnapshot>,
    token: Arc<()>,
    active: bool,
}

impl SettingsRegistration {
    fn snapshot(&self) -> Arc<SettingsSnapshot> {
        self.snapshot.clone()
    }

    fn remove(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.state.lock() else {
            return;
        };
        if state
            .registrations
            .get(&self.namespace)
            .is_some_and(|current| Arc::ptr_eq(&current.token, &self.token))
        {
            state.registrations.remove(&self.namespace);
        }
    }
}

impl Drop for SettingsRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}

struct SettingsWatchRegistration {
    inner: Weak<SettingsInner>,
    namespace: SettingsNamespace,
    watcher_id: u64,
    active: bool,
}

impl SettingsWatchRegistration {
    fn remove(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.state.lock() else {
            return;
        };
        if let Some(registration) = state.registrations.get_mut(&self.namespace) {
            registration.watchers.remove(&self.watcher_id);
        }
    }
}

impl Drop for SettingsWatchRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Merge, prove and validate one namespace generation.
///
/// The exposure proof runs before the owner validator so a value this crate
/// refuses to project can never reach owner-authored error text either.
fn resolve(
    definition: &SettingsDefinition,
    documents: &SettingsDocuments,
    revision: u64,
) -> Result<Arc<SettingsSnapshot>, SettingsError> {
    let user = documents.user(&definition.namespace).cloned();
    let project = documents.project(&definition.namespace).cloned();
    let managed = documents.managed(&definition.namespace).cloned();
    let mut resolved = definition.schema.defaults().clone();
    for layer in [
        definition.base.as_ref(),
        user.as_ref(),
        project.as_ref(),
        definition.override_layer.as_ref(),
        managed.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        merge_layer(&mut resolved, layer);
    }

    let roles = definition.schema.roles();
    if let Some(path) = roles.contradiction() {
        return Err(SettingsError::UnprovableWireExposure {
            namespace: definition.namespace.to_string(),
            path: path.to_string(),
            fault: WireExposureFault::ContradictoryRole,
        });
    }
    let projection = if definition.schema.wire_exposed() {
        let inputs = ExposureInputs {
            schema: definition.schema.document(),
            defaults: definition.schema.defaults(),
            base: definition.base.as_ref(),
            user: user.as_ref(),
            project: project.as_ref(),
            override_layer: definition.override_layer.as_ref(),
            managed: managed.as_ref(),
            resolved: &resolved,
        };
        Some(verify_and_project(&inputs, roles).map_err(|(path, fault)| {
            SettingsError::UnprovableWireExposure {
                namespace: definition.namespace.to_string(),
                path,
                fault,
            }
        })?)
    } else {
        None
    };

    definition
        .schema
        .validate(&resolved)
        .map_err(|message| SettingsError::InvalidResolved {
            namespace: definition.namespace.to_string(),
            message: scrub_message(message),
        })?;
    let managed_locks = managed.as_ref().map(leaf_paths).unwrap_or_default();
    Ok(Arc::new(SettingsSnapshot {
        namespace: definition.namespace.clone(),
        schema: definition.schema.document().clone(),
        defaults: definition.schema.defaults().clone(),
        base: definition.base.clone(),
        user,
        project,
        override_layer: definition.override_layer.clone(),
        managed,
        resolved,
        applies: definition.applies,
        revision,
        managed_locks,
        projection,
        roles: roles.clone(),
    }))
}

fn advance_revision(namespace: &SettingsNamespace, revision: u64) -> Result<u64, SettingsError> {
    revision
        .checked_add(1)
        .ok_or_else(|| SettingsError::RevisionExhausted {
            namespace: namespace.to_string(),
        })
}

fn notify(callbacks: &[WatcherCallback], change: SettingsChange) {
    for callback in callbacks {
        let callback = callback.clone();
        let change = change.clone();
        IN_WATCHER_CALLBACK.set(true);
        let _ = catch_unwind(AssertUnwindSafe(move || callback(change)));
        IN_WATCHER_CALLBACK.set(false);
    }
}

fn merge_layer(under: &mut Value, over: &Value) {
    match (under, over) {
        (Value::Object(under_map), Value::Object(over_map)) => {
            for (key, value) in over_map {
                if let Some(existing) = under_map.get_mut(key) {
                    merge_layer(existing, value);
                } else {
                    under_map.insert(key.clone(), value.clone());
                }
            }
        }
        (under_value, over_value) => *under_value = over_value.clone(),
    }
}

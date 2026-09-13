//! PL06 plugin lifecycle: install, enable, disable, update, rollback, remove.
//!
//! PL02's cache stores *versions*; it has no opinion about which one is live.
//! That opinion is this module's, and keeping the two separate is what makes
//! rollback possible at all: the cache retains prior versions, so rolling back
//! is re-pointing a reference rather than re-downloading anything.
//!
//! The operation set is a closed enum both surfaces match exhaustively, the
//! same mechanism MCP10 uses for CLI/TUI parity (AGENTS §3, GOTCHAS #158).

use std::collections::BTreeMap;

use crate::{PluginId, PluginVersion, ResolvedPluginGraph};

/// Every plugin lifecycle operation heycode exposes.
///
/// Deliberately **not** `#[non_exhaustive]`: adding an operation must fail to
/// compile in every surface until it is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginOperation {
    /// Add a package and make it the active version.
    Install,
    /// Activate an installed plugin.
    Enable,
    /// Deactivate without uninstalling.
    Disable,
    /// Move to a newer installed version, remembering the previous one.
    Update,
    /// Return to the remembered previous version.
    Rollback,
    /// Forget a plugin entirely.
    Remove,
}

impl PluginOperation {
    /// Every operation, in the order the acceptance criterion names them.
    pub const ALL: [Self; 6] = [
        Self::Install,
        Self::Enable,
        Self::Disable,
        Self::Update,
        Self::Rollback,
        Self::Remove,
    ];

    /// The subcommand word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Update => "update",
            Self::Rollback => "rollback",
            Self::Remove => "remove",
        }
    }

    /// Parse a subcommand word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == word)
    }
}

impl std::fmt::Display for PluginOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a lifecycle operation could not be carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LifecycleError {
    /// The owning plugin Context has shut down.
    Closed,
    /// The plugin is not installed.
    NotInstalled(String),
    /// The plugin is already installed at this version.
    AlreadyInstalled(String),
    /// There is no remembered previous version to return to.
    NothingToRollBackTo(String),
    /// The requested version is not in the cache.
    VersionUnavailable {
        /// Plugin id.
        id: String,
        /// The version that was asked for.
        version: String,
    },
    /// An update was asked to move to the version already active.
    AlreadyAtVersion {
        /// Plugin id.
        id: String,
        /// The active version.
        version: String,
    },
    /// The state store could not be read or written.
    Store(String),
    /// Product lifecycle has no administrator-supplied managed policy.
    ManagedPolicyUnavailable {
        /// Attempted authority-increasing operation.
        operation: PluginOperation,
        /// Target plugin id.
        id: String,
    },
    /// Current managed policy rejected or could not prove the target version.
    ManagedPolicyRejected {
        /// Attempted authority-increasing operation.
        operation: PluginOperation,
        /// Target plugin id.
        id: String,
    },
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("plugin lifecycle service is closed"),
            Self::NotInstalled(id) => write!(f, "plugin `{id}` is not installed"),
            Self::AlreadyInstalled(id) => write!(f, "plugin `{id}` is already installed"),
            Self::NothingToRollBackTo(id) => {
                write!(f, "plugin `{id}` has no previous version to roll back to")
            }
            Self::VersionUnavailable { id, version } => {
                write!(f, "plugin `{id}` has no installed version `{version}`")
            }
            Self::AlreadyAtVersion { id, version } => {
                write!(f, "plugin `{id}` is already at version `{version}`")
            }
            Self::Store(message) => write!(f, "plugin state store: {message}"),
            Self::ManagedPolicyUnavailable { operation, id } => write!(
                f,
                "`{operation}` on `{id}` needs managed plugin policy: this package ships code, which only an administrator-supplied policy may admit (declarative packages install without it)"
            ),
            Self::ManagedPolicyRejected { operation, id } => {
                write!(f, "managed plugin policy rejected `{operation}` on `{id}`")
            }
        }
    }
}

impl std::error::Error for LifecycleError {}

/// What heycode believes about one installed plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginState {
    /// Validated plugin identity.
    pub id: PluginId,
    /// The version that activates.
    pub active: PluginVersion,
    /// The version `rollback` would return to, when there is one.
    ///
    /// Exactly one step of history. A rollback is an escape from a bad update,
    /// not a version-control system, and pretending to offer arbitrary history
    /// would promise more than the cache's retention policy can keep.
    pub previous: Option<PluginVersion>,
    /// Whether the plugin participates in composition.
    pub enabled: bool,
}

/// Durable home for lifecycle state.
///
/// Separate from the package cache: the cache knows which versions exist, this
/// knows which one is live. Injected as a trait so the operations are testable
/// without a filesystem, and so every surface runs the same code.
pub trait PluginStateStore: Send + Sync {
    /// Read every plugin's state.
    ///
    /// # Errors
    /// Redacted, actionable store text only.
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError>;

    /// Replace the whole set.
    ///
    /// # Errors
    /// Redacted, actionable store text only.
    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError>;
}

/// Which versions of a plugin the package cache actually holds.
///
/// Injected rather than reaching into `PluginInstallCache` directly, so the
/// lifecycle rules can be tested against an arbitrary retention state — the
/// interesting cases are all about a version being absent.
pub trait InstalledVersions: Send + Sync {
    /// Versions present in the cache for `id`, in any order.
    fn versions(&self, id: &PluginId) -> Vec<PluginVersion>;
}

/// Admission gate for lifecycle transitions that can activate package code or
/// declarative contributions.
pub trait PluginLifecycleAdmission: Send + Sync {
    /// Authorize one exact target version before lifecycle state mutation.
    ///
    /// # Errors
    /// A missing, denied or unproven managed policy fails with body-free text.
    fn authorize(
        &self,
        operation: PluginOperation,
        id: &PluginId,
        version: &PluginVersion,
    ) -> Result<(), LifecycleError>;
}

/// Fail-closed product admission until managed policy authority is supplied.
#[derive(Debug, Default)]
pub struct RequireManagedPluginPolicy;

impl PluginLifecycleAdmission for RequireManagedPluginPolicy {
    fn authorize(
        &self,
        operation: PluginOperation,
        id: &PluginId,
        _version: &PluginVersion,
    ) -> Result<(), LifecycleError> {
        if matches!(
            operation,
            PluginOperation::Disable | PluginOperation::Remove
        ) {
            return Ok(());
        }
        Err(LifecycleError::ManagedPolicyUnavailable {
            operation,
            id: id.as_str().to_owned(),
        })
    }
}

/// User-scope admission: a package with no `code` section — skills, commands,
/// agents, hooks, themes, MCP definitions — is the user's own declarative
/// configuration and may be installed, enabled, updated and rolled back
/// without an administrator. A package that ships code still needs managed
/// policy authority; `heycode plugin install <dir>` says so instead of failing
/// every user with "managed plugin policy is unavailable".
pub struct DeclarativeUserPluginPolicy {
    ships_code: Box<ShipsCodeLookup>,
}

/// Whether a cached version ships code; `None` when it is not cached.
type ShipsCodeLookup = dyn Fn(&PluginId, &PluginVersion) -> Option<bool> + Send + Sync;

impl DeclarativeUserPluginPolicy {
    /// Build over a lookup that says whether the cached package ships code
    /// (`None` when the version is not cached).
    pub fn new(
        ships_code: impl Fn(&PluginId, &PluginVersion) -> Option<bool> + Send + Sync + 'static,
    ) -> Self {
        Self {
            ships_code: Box::new(ships_code),
        }
    }
}

impl std::fmt::Debug for DeclarativeUserPluginPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeclarativeUserPluginPolicy")
    }
}

impl PluginLifecycleAdmission for DeclarativeUserPluginPolicy {
    fn authorize(
        &self,
        operation: PluginOperation,
        id: &PluginId,
        version: &PluginVersion,
    ) -> Result<(), LifecycleError> {
        if matches!(
            operation,
            PluginOperation::Disable | PluginOperation::Remove
        ) {
            return Ok(());
        }
        match (self.ships_code)(id, version) {
            Some(false) => Ok(()),
            // Not cached: the lifecycle reports that precisely; unknown code
            // status must not be admitted by default.
            None => Ok(()),
            Some(true) => Err(LifecycleError::ManagedPolicyUnavailable {
                operation,
                id: id.as_str().to_owned(),
            }),
        }
    }
}

struct UnmanagedAdmission;

impl PluginLifecycleAdmission for UnmanagedAdmission {
    fn authorize(
        &self,
        _operation: PluginOperation,
        _id: &PluginId,
        _version: &PluginVersion,
    ) -> Result<(), LifecycleError> {
        Ok(())
    }
}

/// The six operations, implemented once for every surface.
pub struct PluginLifecycle {
    store: std::sync::Arc<dyn PluginStateStore>,
    cache: std::sync::Arc<dyn InstalledVersions>,
    admission: std::sync::Arc<dyn PluginLifecycleAdmission>,
    state: std::sync::Arc<PluginLifecycleState>,
}

#[derive(Default)]
struct PluginLifecycleState {
    closed: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for PluginLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PluginLifecycle")
    }
}

impl PluginLifecycle {
    /// Lifecycle over a state store and a view of the package cache.
    #[must_use]
    pub fn new(
        store: std::sync::Arc<dyn PluginStateStore>,
        cache: std::sync::Arc<dyn InstalledVersions>,
    ) -> Self {
        Self {
            store,
            cache,
            admission: std::sync::Arc::new(UnmanagedAdmission),
            state: std::sync::Arc::new(PluginLifecycleState::default()),
        }
    }

    /// Lifecycle with an explicit activation admission provider.
    #[must_use]
    pub fn with_admission(
        store: std::sync::Arc<dyn PluginStateStore>,
        cache: std::sync::Arc<dyn InstalledVersions>,
        admission: std::sync::Arc<dyn PluginLifecycleAdmission>,
    ) -> Self {
        Self {
            store,
            cache,
            admission,
            state: std::sync::Arc::new(PluginLifecycleState::default()),
        }
    }

    fn ensure_open(&self) -> Result<(), LifecycleError> {
        if self.state.closed.load(std::sync::atomic::Ordering::SeqCst) {
            Err(LifecycleError::Closed)
        } else {
            Ok(())
        }
    }

    fn close_handle(&self) -> std::sync::Arc<PluginLifecycleState> {
        std::sync::Arc::clone(&self.state)
    }

    fn locate(
        states: &BTreeMap<String, PluginState>,
        id: &PluginId,
    ) -> Result<PluginState, LifecycleError> {
        states
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| LifecycleError::NotInstalled(id.as_str().to_owned()))
    }

    fn require_cached(&self, id: &PluginId, version: &PluginVersion) -> Result<(), LifecycleError> {
        if self.cache.versions(id).iter().any(|held| held == version) {
            return Ok(());
        }
        Err(LifecycleError::VersionUnavailable {
            id: id.as_str().to_owned(),
            version: version.to_string(),
        })
    }

    /// Every plugin's state, id-ordered.
    ///
    /// # Errors
    /// Store read failure.
    pub fn list(&self) -> Result<Vec<PluginState>, LifecycleError> {
        self.ensure_open()?;
        Ok(self.store.load()?.into_values().collect())
    }

    /// Reconcile lifecycle state to one complete resolved active generation.
    ///
    /// Every target version is cache-checked and admission-checked in the
    /// graph's dependency-first order before a candidate map is changed or the
    /// store is called. Existing rows outside the graph are disabled rather
    /// than removed, preserving PL06 rollback/install history. One successful
    /// generation produces at most one state-store write.
    ///
    /// # Errors
    /// Cache absence, managed-policy refusal, or store failure. No lifecycle
    /// state is published when any preflight row fails.
    pub fn apply_resolved_graph(&self, graph: &ResolvedPluginGraph) -> Result<(), LifecycleError> {
        self.ensure_open()?;
        let original = self.store.load()?;

        for manifest in graph.manifests() {
            let operation = match original.get(manifest.id().as_str()) {
                None => PluginOperation::Install,
                Some(state) if &state.active == manifest.version() => PluginOperation::Enable,
                Some(state) if state.previous.as_ref() == Some(manifest.version()) => {
                    PluginOperation::Rollback
                }
                Some(_) => PluginOperation::Update,
            };
            self.admission
                .authorize(operation, manifest.id(), manifest.version())?;
            self.require_cached(manifest.id(), manifest.version())?;
        }
        for state in original
            .values()
            .filter(|state| state.enabled && graph.manifest(&state.id).is_none())
        {
            self.admission
                .authorize(PluginOperation::Disable, &state.id, &state.active)?;
        }

        let mut candidate = original.clone();
        for manifest in graph.manifests() {
            match candidate.get_mut(manifest.id().as_str()) {
                Some(state) => {
                    if &state.active != manifest.version() {
                        state.previous = Some(std::mem::replace(
                            &mut state.active,
                            manifest.version().clone(),
                        ));
                    }
                    state.enabled = true;
                }
                None => {
                    candidate.insert(
                        manifest.id().as_str().to_owned(),
                        PluginState {
                            id: manifest.id().clone(),
                            active: manifest.version().clone(),
                            previous: None,
                            enabled: true,
                        },
                    );
                }
            }
        }
        for state in candidate.values_mut() {
            if graph.manifest(&state.id).is_none() {
                state.enabled = false;
            }
        }
        if candidate != original {
            self.store.persist(&candidate)?;
        }
        Ok(())
    }

    /// `install`: record a newly cached version as active and enabled.
    ///
    /// # Errors
    /// [`LifecycleError::AlreadyInstalled`] when the plugin is already known —
    /// moving to another version is `update`, and conflating the two would let
    /// an install silently discard the rollback target.
    pub fn install(&self, id: &PluginId, version: &PluginVersion) -> Result<(), LifecycleError> {
        self.ensure_open()?;
        self.admission
            .authorize(PluginOperation::Install, id, version)?;
        let mut states = self.store.load()?;
        if states.contains_key(id.as_str()) {
            return Err(LifecycleError::AlreadyInstalled(id.as_str().to_owned()));
        }
        self.require_cached(id, version)?;
        states.insert(
            id.as_str().to_owned(),
            PluginState {
                id: id.clone(),
                active: version.clone(),
                previous: None,
                enabled: true,
            },
        );
        self.store.persist(&states)
    }

    /// `enable` / `disable`: toggle participation without touching versions.
    ///
    /// # Errors
    /// [`LifecycleError::NotInstalled`].
    pub fn set_enabled(&self, id: &PluginId, enabled: bool) -> Result<(), LifecycleError> {
        self.ensure_open()?;
        let mut states = self.store.load()?;
        let mut state = Self::locate(&states, id)?;
        if enabled {
            self.admission
                .authorize(PluginOperation::Enable, id, &state.active)?;
        }
        state.enabled = enabled;
        states.insert(id.as_str().to_owned(), state);
        self.store.persist(&states)
    }

    /// `update`: move to another cached version, remembering the current one.
    ///
    /// The version must already be in the cache. An update that cannot resolve
    /// its target changes nothing — the same rule K10 applies to reloads: the
    /// last good state survives a failed transition.
    ///
    /// # Errors
    /// [`LifecycleError::NotInstalled`], [`LifecycleError::AlreadyAtVersion`],
    /// or [`LifecycleError::VersionUnavailable`].
    pub fn update(&self, id: &PluginId, to: &PluginVersion) -> Result<(), LifecycleError> {
        self.ensure_open()?;
        self.admission.authorize(PluginOperation::Update, id, to)?;
        let mut states = self.store.load()?;
        let mut state = Self::locate(&states, id)?;
        if &state.active == to {
            return Err(LifecycleError::AlreadyAtVersion {
                id: id.as_str().to_owned(),
                version: to.to_string(),
            });
        }
        // Checked before anything is mutated, so a failed update leaves the
        // active version and the rollback target exactly as they were.
        self.require_cached(id, to)?;
        state.previous = Some(std::mem::replace(&mut state.active, to.clone()));
        states.insert(id.as_str().to_owned(), state);
        self.store.persist(&states)
    }

    /// `rollback`: return to the remembered previous version.
    ///
    /// Rolling back swaps the two, so a rollback can itself be rolled back —
    /// an operator who rolls back by mistake is one command from undoing it.
    ///
    /// # Errors
    /// [`LifecycleError::NotInstalled`], [`LifecycleError::NothingToRollBackTo`]
    /// when no previous version is remembered, or
    /// [`LifecycleError::VersionUnavailable`] when the cache no longer holds it.
    pub fn rollback(&self, id: &PluginId) -> Result<PluginVersion, LifecycleError> {
        self.ensure_open()?;
        let mut states = self.store.load()?;
        let mut state = Self::locate(&states, id)?;
        let previous = state
            .previous
            .clone()
            .ok_or_else(|| LifecycleError::NothingToRollBackTo(id.as_str().to_owned()))?;
        self.admission
            .authorize(PluginOperation::Rollback, id, &previous)?;
        // Retention is the cache's policy, not this module's assumption: a
        // remembered version that has since been pruned must fail loudly rather
        // than leave a reference to something that cannot activate.
        self.require_cached(id, &previous)?;
        state.previous = Some(std::mem::replace(&mut state.active, previous.clone()));
        states.insert(id.as_str().to_owned(), state);
        self.store.persist(&states)?;
        Ok(previous)
    }

    /// `remove`: forget a plugin.
    ///
    /// # Errors
    /// [`LifecycleError::NotInstalled`] — removing something absent is
    /// reported, so a mistyped id cannot look like success.
    pub fn remove(&self, id: &PluginId) -> Result<(), LifecycleError> {
        self.ensure_open()?;
        let mut states = self.store.load()?;
        Self::locate(&states, id)?;
        states.remove(id.as_str());
        self.store.persist(&states)
    }
}

/// Plugin lifecycle settings namespace.
///
/// Distinct from K05's `PluginDirective` enable/disable, which selects
/// **composed** plugins by factory id inside a profile. This namespace records
/// installed **marketplace packages** by their two-segment id, with the version
/// that activates. PL03 is the bridge: it reads this state to decide which
/// installed package contributes to a real composition.
///
/// # Errors
/// Static namespace validation failure.
pub fn settings_namespace()
-> Result<heycode_settings::SettingsNamespace, heycode_settings::SettingsError> {
    heycode_settings::SettingsNamespace::new("plugins")
}

/// The settings definition lifecycle state persists into.
///
/// # Errors
/// Static schema validation failure.
pub fn settings_definition()
-> Result<heycode_settings::SettingsDefinition, heycode_settings::SettingsError> {
    let namespace = settings_namespace()?;
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({
            "type": "object",
            "properties": {
                "installed": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "active": {"type": "string"},
                            "previous": {"type": ["string", "null"]},
                            "enabled": {"type": "boolean"}
                        }
                    }
                }
            }
        }),
        serde_json::json!({"installed": {}}),
        validate_installed_section,
    )?
    // Ids and versions only — no path, no source URL, no credential.
    .with_wire_exposure();
    Ok(heycode_settings::SettingsDefinition::new(namespace, schema))
}

fn validate_installed_section(value: &serde_json::Value) -> Result<(), String> {
    let installed = value
        .get("installed")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "installed must be an object".to_owned())?;
    for (id, entry) in installed {
        PluginId::new(id.clone()).map_err(|error| format!("plugin id `{id}`: {error}"))?;
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("plugin `{id}` must be an object"))?;
        let active = entry
            .get("active")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("plugin `{id}` needs an active version"))?;
        PluginVersion::parse(active.to_owned())
            .map_err(|error| format!("plugin `{id}` active version: {error}"))?;
        if let Some(previous) = entry.get("previous").and_then(serde_json::Value::as_str) {
            PluginVersion::parse(previous.to_owned())
                .map_err(|error| format!("plugin `{id}` previous version: {error}"))?;
        }
    }
    Ok(())
}

/// Lifecycle state persisted in the layered settings stack.
pub struct SettingsBackedStateStore {
    settings: std::sync::Arc<heycode_settings::SettingsService>,
}

impl std::fmt::Debug for SettingsBackedStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SettingsBackedStateStore")
    }
}

impl SettingsBackedStateStore {
    /// Bind to a settings service.
    #[must_use]
    pub const fn new(settings: std::sync::Arc<heycode_settings::SettingsService>) -> Self {
        Self { settings }
    }
}

/// One state as the settings document stores it.
///
/// `previous` is omitted rather than written as `null`: the user layer is
/// TOML, which has no null, so a fresh install with no rollback target used to
/// fail to persist with "unsupported unit type".
fn state_row(state: &PluginState) -> serde_json::Value {
    let mut row = serde_json::json!({
        "active": state.active.to_string(),
        "enabled": state.enabled,
    });
    if let Some(previous) = &state.previous {
        row["previous"] = serde_json::Value::String(previous.to_string());
    }
    row
}

impl PluginStateStore for SettingsBackedStateStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        let namespace =
            settings_namespace().map_err(|error| LifecycleError::Store(error.to_string()))?;
        let Some(snapshot) = self
            .settings
            .get(&namespace)
            .map_err(|error| LifecycleError::Store(error.to_string()))?
        else {
            return Ok(BTreeMap::new());
        };
        let mut states = BTreeMap::new();
        let Some(installed) = snapshot
            .resolved()
            .get("installed")
            .and_then(serde_json::Value::as_object)
        else {
            return Ok(states);
        };
        for (raw_id, entry) in installed {
            let (Ok(id), Some(active)) = (
                PluginId::new(raw_id.clone()),
                entry.get("active").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            let Ok(active) = PluginVersion::parse(active.to_owned()) else {
                continue;
            };
            let previous = entry
                .get("previous")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| PluginVersion::parse(raw.to_owned()).ok());
            states.insert(
                raw_id.clone(),
                PluginState {
                    id,
                    active,
                    previous,
                    enabled: entry
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true),
                },
            );
        }
        Ok(states)
    }

    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        let namespace =
            settings_namespace().map_err(|error| LifecycleError::Store(error.to_string()))?;
        let mut installed = serde_json::Map::new();
        for (id, state) in states {
            installed.insert(id.clone(), state_row(state));
        }
        self.settings
            .replace_user(
                &namespace,
                serde_json::json!({"installed": serde_json::Value::Object(installed)}),
                None,
            )
            .map(|_| ())
            .map_err(|error| LifecycleError::Store(error.to_string()))
    }
}

/// Effect-owned plugin lifecycle service.
pub const SERVICE_PLUGIN_LIFECYCLE: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("plugin-lifecycle");

/// Register the plugin lifecycle settings namespace and live service.
///
/// The installed-versions argument must describe the verified PL02 cache
/// generation the product selected. Admission is the single policy path for
/// install, enable, update and rollback; production supplies managed admission
/// or the fail-closed RequireManagedPluginPolicy.
#[must_use]
pub fn plugin_lifecycle_plugin(
    installed_versions: std::sync::Arc<dyn InstalledVersions>,
    admission: std::sync::Arc<dyn PluginLifecycleAdmission>,
) -> Box<dyn heycode_core::Plugin> {
    struct PluginLifecyclePlugin {
        installed_versions: std::sync::Arc<dyn InstalledVersions>,
        admission: std::sync::Arc<dyn PluginLifecycleAdmission>,
    }

    impl heycode_core::Plugin for PluginLifecyclePlugin {
        fn name(&self) -> &'static str {
            "plugin-lifecycle"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_PLUGIN_LIFECYCLE]
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "plugins",
            )]
        }
        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| heycode_core::CoreError::other("settings missing"))?;
            settings
                .register(
                    context,
                    settings_definition()
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let lifecycle = PluginLifecycle::with_admission(
                std::sync::Arc::new(SettingsBackedStateStore::new(settings))
                    as std::sync::Arc<dyn PluginStateStore>,
                std::sync::Arc::clone(&self.installed_versions),
                std::sync::Arc::clone(&self.admission),
            );
            let state = lifecycle.close_handle();
            context.effect(move || {
                state
                    .closed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            });
            context.provide(SERVICE_PLUGIN_LIFECYCLE, self.name(), lifecycle)?;
            Ok(())
        }
    }
    Box::new(PluginLifecyclePlugin {
        installed_versions,
        admission,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod state_row_tests {
    use super::*;

    #[test]
    fn a_fresh_install_row_has_no_null_previous_because_toml_has_no_null() {
        let state = PluginState {
            id: PluginId::new("me/hello").unwrap(),
            active: PluginVersion::parse("1.0.0".to_owned()).unwrap(),
            previous: None,
            enabled: true,
        };
        let row = state_row(&state);
        assert!(row.get("previous").is_none(), "{row}");
        assert_eq!(row["active"], "1.0.0");
        let rolled = PluginState {
            previous: Some(PluginVersion::parse("0.9.0".to_owned()).unwrap()),
            ..state
        };
        assert_eq!(state_row(&rolled)["previous"], "0.9.0");
    }
}

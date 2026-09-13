//! Durable-before-live route transitions.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_llm::{
    CatalogRefreshMode, CatalogRegistry, CatalogSnapshot, CatalogView, ConnectionProfile,
    ProviderProfile, ProviderRegistry, ReasoningEffortId, ReasoningEffortOptions,
};
use heycode_settings::{SettingsNamespace, SettingsService};

use crate::{RoutingError, RoutingSelection};

/// Lifetime of a user-selected model or effort.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionScope {
    /// Persist the choice for future sessions.
    #[default]
    Default,
    /// Keep the choice only in this composed session.
    Session,
}

/// One model picker choice applied with a single settings revision.
#[derive(Clone, Copy, Debug)]
pub struct ModelControlChoice<'a> {
    /// Exact catalog-proven model id.
    pub model: &'a str,
    /// Explicitly previewed effort, when the user adjusted it.
    pub effort: Option<&'a str>,
    /// Whether to save the choice or use it only for this session.
    pub scope: SelectionScope,
}

/// Live delegated backend operations supplied by the app-server integration.
///
/// The routing crate owns this interface so command and TUI callers do not
/// branch on a concrete runtime or depend on the higher-level app-server crate.
#[async_trait]
pub trait DelegatedRuntimeControls: Send + Sync {
    /// Runtime currently bound to the live app-server backend.
    fn active_runtime_id(&self) -> Result<String, String>;

    /// Discover the active runtime's normalized model catalog.
    async fn models(
        &self,
        expected_runtime: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<CatalogSnapshot, String>;

    /// Discover exact effort choices advertised per runtime model.
    async fn model_configurations(
        &self,
        expected_runtime: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Vec<heycode_runtime::RuntimeModelConfiguration>, String>;

    /// Apply a partial configuration to the already-open live backend.
    async fn configure(
        &self,
        expected_runtime: &str,
        update: heycode_runtime::RuntimeConfiguration,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<AppliedDelegatedConfiguration, String>;

    /// Retire the exact backend generation that accepted an update which
    /// could not be published durably. A replacement generation must not be
    /// affected.
    async fn invalidate(
        &self,
        expected_runtime: &str,
        backend_generation: u64,
    ) -> Result<(), String>;

    /// Detach and close the active backend after durable logout.
    async fn disconnect(&self, expected_runtime: &str) -> Result<(), String>;
}

/// Effective configuration plus the exact backend generation that accepted it.
#[derive(Debug, Clone)]
pub struct AppliedDelegatedConfiguration {
    configuration: heycode_runtime::RuntimeConfiguration,
    backend_generation: u64,
}

impl AppliedDelegatedConfiguration {
    /// Bind an effective response to its backend generation.
    #[must_use]
    pub const fn new(
        configuration: heycode_runtime::RuntimeConfiguration,
        backend_generation: u64,
    ) -> Self {
        Self {
            configuration,
            backend_generation,
        }
    }

    /// Complete effective configuration returned by the backend.
    #[must_use]
    pub const fn configuration(&self) -> &heycode_runtime::RuntimeConfiguration {
        &self.configuration
    }

    /// Opaque generation used only to retire this exact backend.
    #[must_use]
    pub const fn backend_generation(&self) -> u64 {
        self.backend_generation
    }
}

/// Exact effort vocabulary for the active backend model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEffortCatalog {
    current: Option<String>,
    choices: Vec<String>,
    default: Option<String>,
}

impl BackendEffortCatalog {
    /// Current explicit selection, absent when the backend owns its default.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Exact accepted ids in backend display order.
    #[must_use]
    pub fn choices(&self) -> &[String] {
        &self.choices
    }

    /// Backend-advertised default for the active model.
    #[must_use]
    pub fn default(&self) -> Option<&str> {
        self.default.as_deref()
    }
}

/// Settlement of one explicit opaque-state provider switch.
#[derive(Debug)]
pub enum ProviderSwitchOutcome {
    /// Portable/direct preparation completed and Settings/live route committed.
    Applied(RoutingSelection),
    /// Explicit cancel left Settings, Agent selection and session unchanged.
    Cancelled,
    /// A pre-checkpoint child committed; the current route remains unchanged.
    Forked {
        /// Durable child identity.
        session_id: heycode_core::SessionId,
        /// Exact inherited parent event count.
        fork_event_count: u64,
    },
}

/// Effective model/effort owner projected from the durable routing tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRoutingConfiguration {
    owner: heycode_agent::BackendControlOwner,
    model: Option<String>,
    effort: Option<String>,
    revision: u64,
}

impl ActiveRoutingConfiguration {
    /// Backend that must serve discovery and application.
    #[must_use]
    pub const fn owner(&self) -> &heycode_agent::BackendControlOwner {
        &self.owner
    }

    /// Explicit current model, absent when a delegated runtime owns its default.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Explicit current effort, absent for backend/provider default.
    #[must_use]
    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    /// Monotonic Settings revision used to reject stale picker submissions.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// Settings-CAS-backed effective route owner used by commands and TUI pickers.
#[derive(Clone)]
pub struct RoutingService {
    settings: Arc<SettingsService>,
    namespace: SettingsNamespace,
    agent: Arc<heycode_agent::Agent>,
    providers: Arc<ProviderRegistry>,
    models: Arc<CatalogRegistry>,
    activatable_runtimes: BTreeSet<String>,
    override_notice: Option<String>,
    connection_profiles: Vec<ConnectionProfile>,
    delegated_controls: Arc<std::sync::RwLock<Option<Arc<dyn DelegatedRuntimeControls>>>>,
}

impl RoutingService {
    pub(crate) fn new(
        settings: Arc<SettingsService>,
        namespace: SettingsNamespace,
        agent: Arc<heycode_agent::Agent>,
        providers: Arc<ProviderRegistry>,
        models: Arc<CatalogRegistry>,
        activatable_runtimes: BTreeSet<String>,
        override_notice: Option<String>,
    ) -> Self {
        Self {
            settings,
            namespace,
            agent,
            providers,
            models,
            activatable_runtimes,
            override_notice,
            connection_profiles: Vec::new(),
            delegated_controls: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Bind the live delegated control plane supplied by app-server composition.
    ///
    /// # Errors
    /// A poisoned registration lock leaves the service unavailable.
    pub fn register_delegated_controls(
        &self,
        controls: Arc<dyn DelegatedRuntimeControls>,
    ) -> Result<(), RoutingError> {
        let mut slot = self
            .delegated_controls
            .write()
            .map_err(|_| RoutingError::RegistryUnavailable)?;
        *slot = Some(controls);
        Ok(())
    }

    pub(crate) fn with_connection_profiles(mut self, profiles: Vec<ConnectionProfile>) -> Self {
        let mut profiles = profiles
            .into_iter()
            .map(|profile| (profile.registry_name.clone(), profile))
            .collect::<std::collections::BTreeMap<_, _>>();
        for profile in self.providers.profiles() {
            if let Some(connection) = profiles.get_mut(&profile.registry_name) {
                if profile.credential_reference.is_some() {
                    connection.credential_reference = profile.credential_reference;
                }
            } else {
                profiles.insert(profile.registry_name.clone(), profile.into());
            }
        }
        self.connection_profiles = profiles.into_values().collect();
        self
    }

    /// Provider-owned profiles that can be activated by a fresh composition.
    #[must_use]
    pub fn connection_profiles(&self) -> &[ConnectionProfile] {
        &self.connection_profiles
    }

    /// Whether a contributed runtime can own the top-level route.
    #[must_use]
    pub fn has_runtime(&self, runtime: &str) -> bool {
        self.activatable_runtimes.contains(runtime)
    }

    /// Whether the durable logout latch currently blocks inference.
    ///
    /// # Errors
    /// Missing, poisoned, or malformed routing Settings fail closed.
    pub fn requires_setup(&self) -> Result<bool, RoutingError> {
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or_else(|| RoutingError::Settings("routing namespace is not registered".into()))?;
        crate::model::snapshot_requires_setup(&snapshot)
    }

    pub(crate) fn disconnect_inference(&self) {
        self.agent.disconnect_inference();
    }

    /// Clear the user-owned route, latch mandatory setup, and retire live inference.
    ///
    /// # Errors
    /// A stale route or failed durable Settings write leaves the live route unchanged.
    pub async fn require_setup(&self, expected: &RoutingSelection) -> Result<(), RoutingError> {
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or_else(|| RoutingError::Settings("routing namespace is not registered".into()))?;
        if &RoutingSelection::from_value(snapshot.resolved())? != expected {
            return Err(RoutingError::InvalidSelection("stale logout route"));
        }
        let written = self
            .settings
            .replace_user(
                &self.namespace,
                serde_json::json!({"setup_required": true}),
                Some(snapshot.revision()),
            )
            .map_err(|error| RoutingError::Settings(error.to_string()))?;
        if !crate::model::snapshot_requires_setup(&written)? {
            return Err(RoutingError::Settings(
                "a higher-priority route prevents logout".to_owned(),
            ));
        }
        self.agent.disconnect_inference();
        let controls = self
            .delegated_controls
            .read()
            .ok()
            .and_then(|slot| slot.clone());
        if let Some(controls) = controls {
            let _closed = controls.disconnect(expected.runtime()).await;
        }
        Ok(())
    }

    /// Persist a connection for the next composition without changing the live Agent.
    ///
    /// # Errors
    /// Unknown provider, unproven model, or Settings CAS/persistence failure.
    pub fn stage_connection(
        &self,
        provider: &str,
        model: &str,
        catalog: Option<&heycode_llm::CatalogSnapshot>,
    ) -> Result<(), RoutingError> {
        self.stage_connection_endpoint(provider, model, catalog, None)
    }

    /// Stage a discovered endpoint and model atomically for the next composition.
    ///
    /// # Errors
    /// Invalid endpoint, unknown provider, unproven model or persistence failure.
    pub fn stage_connection_endpoint(
        &self,
        provider: &str,
        model: &str,
        catalog: Option<&heycode_llm::CatalogSnapshot>,
        endpoint: Option<&str>,
    ) -> Result<(), RoutingError> {
        self.stage_connection_authenticated(provider, model, catalog, endpoint, None)
    }

    /// Stage the endpoint, selected model and validated credential reference together.
    ///
    /// # Errors
    /// Invalid endpoint, unknown provider, unproven model or persistence failure.
    pub fn stage_connection_authenticated(
        &self,
        provider: &str,
        model: &str,
        catalog: Option<&heycode_llm::CatalogSnapshot>,
        endpoint: Option<&str>,
        credential_reference: Option<&heycode_credentials::CredentialReference>,
    ) -> Result<(), RoutingError> {
        let candidate = RoutingSelection::new("native", provider, model, None)?
            .with_endpoint(endpoint.map(str::to_owned))?
            .with_credential_reference(credential_reference.cloned());
        self.stage_connection_selection(&candidate, catalog)
    }

    /// Persist one complete native connection for activation by the next composition.
    ///
    /// # Errors
    /// Delegated route, unknown provider, unproven model or persistence failure.
    pub fn stage_connection_selection(
        &self,
        candidate: &RoutingSelection,
        catalog: Option<&heycode_llm::CatalogSnapshot>,
    ) -> Result<(), RoutingError> {
        if candidate.runtime() != "native" {
            return Err(RoutingError::InvalidSelection("connection runtime"));
        }
        let provider = candidate.provider();
        let model = candidate.model();
        let profile = self
            .connection_profiles
            .iter()
            .find(|profile| profile.registry_name == provider)
            .ok_or_else(|| RoutingError::UnknownProvider(provider.into()))?;
        let proven = catalog.is_some_and(|catalog| {
            catalog.provider.id == provider
                && catalog
                    .models
                    .iter()
                    .any(|row| row.id == model && row.lifecycle.is_selectable(unix_time_ms()))
        });
        if !profile.admits_model(model)
            || (profile.default_model.as_deref() != Some(model)
                && !proven
                && !profile.allows_explicit_model())
        {
            return Err(RoutingError::UnselectableModel {
                provider: provider.into(),
                model: model.into(),
            });
        }
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or(RoutingError::RegistryUnavailable)?;
        let mut value = self.selection()?.to_value();
        value["pending_connection"] = serde_json::json!({"provider":provider,"model":model});
        if let Some(endpoint) = candidate.endpoint() {
            value["pending_connection"]["endpoint"] = serde_json::Value::String(endpoint.into());
        }
        if let Some(reference) = candidate.credential_reference() {
            value["pending_connection"]["credential_reference"] =
                serde_json::Value::String(reference.as_str().into());
        }
        value["pending_connection"]["parameters"] = serde_json::json!(candidate.parameters());
        let written = self
            .settings
            .replace_user(&self.namespace, value, Some(snapshot.revision()))
            .map_err(|error| RoutingError::Settings(error.to_string()))?;
        if crate::model::snapshot_requires_setup(&written)? {
            return Err(RoutingError::Settings(
                "a higher-priority setting still requires connection setup".into(),
            ));
        }
        let pending =
            crate::model::pending_connection(written.resolved().get("pending_connection"))?;
        if !pending.is_some_and(|selection| {
            selection.provider() == provider
                && selection.model() == model
                && selection.endpoint() == candidate.endpoint()
                && selection.credential_reference() == candidate.credential_reference()
                && selection.parameters() == candidate.parameters()
        }) {
            return Err(RoutingError::Settings(
                "a higher-priority route prevents this connection".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn activate_pending(
        &self,
        snapshot: &heycode_settings::SettingsSnapshot,
    ) -> Result<bool, RoutingError> {
        let Some(pending) =
            crate::model::pending_connection(snapshot.resolved().get("pending_connection"))?
        else {
            return Ok(false);
        };
        if self.agent.selection().provider_name != pending.provider() {
            return Ok(false);
        }
        self.commit(pending)?;
        Ok(true)
    }

    /// What this process's command line displaced in the persisted route, for
    /// the shell to show once at startup. `None` when no flag pinned anything
    /// or every pin agreed with what is stored.
    #[must_use]
    pub fn override_notice(&self) -> Option<&str> {
        self.override_notice.as_deref()
    }

    /// Read the authoritative resolved selection.
    ///
    /// # Errors
    /// Missing/poisoned settings state or invalid resolved shape.
    pub fn selection(&self) -> Result<RoutingSelection, RoutingError> {
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or_else(|| RoutingError::Settings("routing namespace is not registered".into()))?;
        RoutingSelection::from_value(snapshot.resolved())
    }

    /// Project the active control plane without confusing a delegated runtime
    /// with the dormant native provider fallback.
    ///
    /// # Errors
    /// Missing/invalid routing Settings.
    pub fn active_configuration(&self) -> Result<ActiveRoutingConfiguration, RoutingError> {
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or_else(|| RoutingError::Settings("routing namespace is not registered".into()))?;
        let selection = RoutingSelection::from_value(snapshot.resolved())?;
        if crate::model::snapshot_requires_setup(&snapshot)? {
            return Err(RoutingError::SetupRequired);
        }
        let revision = snapshot.revision();
        if selection.runtime() == self.agent.runtime_id() {
            Ok(ActiveRoutingConfiguration {
                owner: heycode_agent::BackendControlOwner::NativeInference {
                    provider: selection.provider().to_owned(),
                },
                model: Some(selection.model().to_owned()),
                effort: selection.effort().map(str::to_owned),
                revision,
            })
        } else {
            Ok(ActiveRoutingConfiguration {
                owner: heycode_agent::BackendControlOwner::DelegatedRuntime {
                    runtime: selection.runtime().to_owned(),
                },
                model: selection.runtime_model().map(str::to_owned),
                effort: selection.runtime_effort().map(str::to_owned),
                revision,
            })
        }
    }

    /// Read or refresh the model catalog owned by the active backend.
    ///
    /// # Errors
    /// Stale ownership, catalog failures, missing delegated controls, or a
    /// backend identity mismatch return before any selection is published.
    pub async fn models_owned(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        mode: CatalogRefreshMode,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<CatalogView, RoutingError> {
        let active = self.active_configuration()?;
        if active.owner() != owner {
            return Err(RoutingError::InvalidSelection("stale model picker"));
        }
        match owner {
            heycode_agent::BackendControlOwner::NativeInference { provider } => self
                .models
                .refresh(provider, mode, cancellation)
                .await
                .map_err(|error| RoutingError::BackendControl(error.to_string())),
            heycode_agent::BackendControlOwner::DelegatedRuntime { runtime } => {
                let controls = self.delegated_controls()?;
                self.require_control_runtime(controls.as_ref(), runtime)?;
                let snapshot = controls
                    .models(runtime, cancellation)
                    .await
                    .map_err(RoutingError::BackendControl)?;
                if snapshot.provider.id != *runtime {
                    return Err(RoutingError::BackendControl(
                        "delegated model catalog identity changed".to_owned(),
                    ));
                }
                let after = self.active_configuration()?;
                if after.owner() != owner || after.revision() != active.revision() {
                    return Err(RoutingError::InvalidSelection("stale model picker"));
                }
                Ok(CatalogView {
                    snapshot: Arc::new(snapshot),
                    freshness: heycode_llm::CatalogFreshness::Live,
                    warning: None,
                })
            }
        }
    }

    /// Discover metadata for the exact model currently owned by a delegated
    /// runtime. The owner, Settings revision, and model are checked again
    /// after discovery so a late response cannot relabel a newer route.
    ///
    /// # Errors
    /// Native ownership, stale routing state, missing delegated controls, or
    /// an absent model row returns without publishing metadata.
    pub async fn model_configuration_owned(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_runtime::RuntimeModelConfiguration, RoutingError> {
        let active = self.active_configuration()?;
        if active.owner() != owner || active.revision() != revision {
            return Err(RoutingError::InvalidSelection("stale model metadata"));
        }
        let heycode_agent::BackendControlOwner::DelegatedRuntime { runtime } = owner else {
            return Err(RoutingError::InvalidSelection(
                "native model metadata is provider-owned",
            ));
        };
        let model = active
            .model()
            .ok_or(RoutingError::InvalidSelection("runtime model unavailable"))?
            .to_owned();
        let controls = self.delegated_controls()?;
        self.require_control_runtime(controls.as_ref(), runtime)?;
        let row = controls
            .model_configurations(runtime, cancellation)
            .await
            .map_err(RoutingError::BackendControl)?
            .into_iter()
            .find(|row| row.model == model)
            .ok_or(RoutingError::InvalidSelection(
                "runtime model metadata unavailable",
            ))?;
        let after = self.active_configuration()?;
        if after.owner() != owner
            || after.revision() != revision
            || after.model() != Some(model.as_str())
        {
            return Err(RoutingError::InvalidSelection("stale model metadata"));
        }
        Ok(row)
    }

    /// Exact effort choices advertised by the active native adapter/model.
    /// Delegated runtimes intentionally return no native fallback choices.
    ///
    /// # Errors
    /// Missing route state or malformed adapter metadata.
    pub fn effort_options(&self) -> Result<Option<ReasoningEffortOptions>, RoutingError> {
        let selection = self.selection()?;
        if selection.runtime() != self.agent.runtime_id() {
            return Ok(None);
        }
        self.native_effort_options(&selection)
    }

    /// Whether `/effort` can attempt live discovery for the current owner.
    #[must_use]
    pub fn effort_command_available(&self) -> bool {
        match self.active_configuration() {
            Ok(active) => match active.owner() {
                heycode_agent::BackendControlOwner::NativeInference { provider } => {
                    self.effort_options().ok().flatten().is_some()
                        || active.model().is_some_and(|model| {
                            self.models
                                .resolve_model(provider, model, unix_time_ms())
                                .is_err()
                        })
                }
                heycode_agent::BackendControlOwner::DelegatedRuntime { .. } => self
                    .delegated_controls
                    .read()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .is_some(),
            },
            Err(_) => false,
        }
    }

    /// Discover exact effort choices for the active backend model.
    ///
    /// # Errors
    /// Stale ownership/revision, unsupported metadata, or delegated discovery
    /// failure leaves the picker closed and routing unchanged.
    pub async fn effort_catalog_owned(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<BackendEffortCatalog, RoutingError> {
        let active = self.active_configuration()?;
        let model = active.model().ok_or(RoutingError::EffortUnavailable)?;
        self.model_effort_catalog_owned(owner, revision, model, cancellation)
            .await
    }

    /// Discover effort values for a highlighted model without changing routing.
    /// The exact backend and revision must still own the picker after discovery.
    ///
    /// # Errors
    /// Unknown or unsupported models, stale ownership, and discovery failure.
    pub async fn model_effort_catalog_owned(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        model: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<BackendEffortCatalog, RoutingError> {
        let active = self.active_configuration()?;
        if active.owner() != owner || active.revision() != revision {
            return Err(RoutingError::InvalidSelection("stale effort picker"));
        }
        let saved = self
            .settings
            .get_without_override(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or(RoutingError::RegistryUnavailable)?;
        let saved = RoutingSelection::from_value(saved.resolved())?;
        match owner {
            heycode_agent::BackendControlOwner::NativeInference { provider } => {
                let current_model = active.model() == Some(model);
                if self
                    .models
                    .resolve_model(provider, model, unix_time_ms())
                    .is_err()
                    && (!current_model || self.effort_options()?.is_none())
                {
                    self.models
                        .refresh(provider, CatalogRefreshMode::PreferCache, cancellation)
                        .await
                        .map_err(|error| RoutingError::BackendControl(error.to_string()))?;
                    let after = self.active_configuration()?;
                    if after.owner() != owner || after.revision() != revision {
                        return Err(RoutingError::InvalidSelection("stale effort picker"));
                    }
                }
                let target = if current_model {
                    self.selection()?
                } else {
                    self.model_target(model)?
                };
                let options = self
                    .native_effort_options(&target)?
                    .ok_or(RoutingError::EffortUnavailable)?;
                Ok(BackendEffortCatalog {
                    current: current_model
                        .then(|| active.effort().map(str::to_owned))
                        .flatten(),
                    choices: options
                        .choices()
                        .iter()
                        .map(|choice| choice.as_str().to_owned())
                        .collect(),
                    default: saved
                        .effort()
                        .filter(|effort| {
                            saved.runtime() == self.agent.runtime_id()
                                && saved.provider() == owner.id()
                                && saved.model() == model
                                && options
                                    .choices()
                                    .iter()
                                    .any(|choice| choice.as_str() == *effort)
                        })
                        .map(str::to_owned)
                        .or_else(|| options.default().map(|effort| effort.as_str().to_owned())),
                })
            }
            heycode_agent::BackendControlOwner::DelegatedRuntime { runtime } => {
                let controls = self.delegated_controls()?;
                self.require_control_runtime(controls.as_ref(), runtime)?;
                let rows = controls
                    .model_configurations(runtime, cancellation)
                    .await
                    .map_err(RoutingError::BackendControl)?;
                let row = rows
                    .into_iter()
                    .find(|row| row.model == model)
                    .ok_or(RoutingError::EffortUnavailable)?;
                if row.reasoning_efforts.is_empty()
                    || row
                        .default_reasoning_effort
                        .as_ref()
                        .is_some_and(|default| {
                            !row.reasoning_efforts.iter().any(|choice| choice == default)
                        })
                {
                    return Err(RoutingError::EffortUnavailable);
                }
                let after = self.active_configuration()?;
                if after.owner() != owner || after.revision() != revision {
                    return Err(RoutingError::InvalidSelection("stale effort picker"));
                }
                let default = saved
                    .runtime_effort()
                    .filter(|effort| {
                        saved.runtime() == runtime
                            && saved.runtime_model() == Some(model)
                            && row.reasoning_efforts.iter().any(|choice| choice == *effort)
                    })
                    .map(str::to_owned)
                    .or(row.default_reasoning_effort);
                Ok(BackendEffortCatalog {
                    current: (active.model() == Some(model))
                        .then(|| active.effort().map(str::to_owned))
                        .flatten(),
                    choices: row.reasoning_efforts,
                    default,
                })
            }
        }
    }

    /// Persist a provider plus its provider-owned default model, then apply the
    /// resulting effective selection to the live Agent.
    ///
    /// # Errors
    /// Unknown provider/runtime or Settings CAS/persistence failure.
    pub fn select_provider(&self, provider: &str) -> Result<RoutingSelection, RoutingError> {
        let target = self.provider_target(provider)?;
        if self
            .agent
            .provider_switch_barrier(target.provider(), target.model())
            .map_err(|_| RoutingError::OpaqueStatePreparation)?
            .is_some()
        {
            return Err(RoutingError::OpaqueStateResolutionRequired {
                provider: target.provider().to_owned(),
                model: target.model().to_owned(),
            });
        }
        self.commit(target)
    }

    /// Execute one explicit opaque-state resolution and commit only when the
    /// Agent reports the target ready.
    ///
    /// # Errors
    /// Unknown provider, preparation/fork/compaction failure, or Settings CAS
    /// failure. Cancel/fork are successful non-commit outcomes.
    pub async fn select_provider_resolving(
        &self,
        provider: &str,
        resolution: heycode_agent::OpaqueStateResolution,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<ProviderSwitchOutcome, RoutingError> {
        let target = self.provider_target(provider)?;
        match self
            .agent
            .prepare_provider_switch(target.provider(), target.model(), resolution, cancellation)
            .await
            .map_err(|_| RoutingError::OpaqueStatePreparation)?
        {
            heycode_agent::ProviderSwitchPreparation::Ready => {
                self.commit(target).map(ProviderSwitchOutcome::Applied)
            }
            heycode_agent::ProviderSwitchPreparation::Cancelled => {
                Ok(ProviderSwitchOutcome::Cancelled)
            }
            heycode_agent::ProviderSwitchPreparation::Forked {
                session_id,
                fork_event_count,
                ..
            } => Ok(ProviderSwitchOutcome::Forked {
                session_id,
                fork_event_count,
            }),
        }
    }

    /// Persist a catalog-proven model for the effective provider, then apply.
    /// The provider-owned default is accepted even when no live catalog source
    /// exists; every other id requires current selectable catalog evidence.
    ///
    /// # Errors
    /// Unknown/retired/unproven model or Settings CAS/persistence failure.
    pub fn select_model(&self, model: &str) -> Result<RoutingSelection, RoutingError> {
        let active = self.active_configuration()?;
        let heycode_agent::BackendControlOwner::NativeInference { .. } = active.owner() else {
            return Err(RoutingError::UnknownRuntime(active.owner().id().to_owned()));
        };
        let target = self.model_target(model)?;
        if self
            .agent
            .provider_switch_barrier(target.provider(), target.model())
            .map_err(|_| RoutingError::OpaqueStatePreparation)?
            .is_some()
        {
            return Err(RoutingError::OpaqueStateResolutionRequired {
                provider: target.provider().to_owned(),
                model: target.model().to_owned(),
            });
        }
        self.commit(target)
    }

    /// Resolve opaque state explicitly before committing a model transition.
    ///
    /// # Errors
    /// Unselectable model, preparation/fork/compaction failure, or Settings
    /// CAS failure. Cancel/fork are successful non-commit outcomes.
    pub async fn select_model_resolving(
        &self,
        model: &str,
        resolution: heycode_agent::OpaqueStateResolution,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<ProviderSwitchOutcome, RoutingError> {
        let active = self.active_configuration()?;
        let heycode_agent::BackendControlOwner::NativeInference { .. } = active.owner() else {
            return Err(RoutingError::UnknownRuntime(active.owner().id().to_owned()));
        };
        let target = self.model_target(model)?;
        match self
            .agent
            .prepare_provider_switch(target.provider(), target.model(), resolution, cancellation)
            .await
            .map_err(|_| RoutingError::OpaqueStatePreparation)?
        {
            heycode_agent::ProviderSwitchPreparation::Ready => {
                self.commit(target).map(ProviderSwitchOutcome::Applied)
            }
            heycode_agent::ProviderSwitchPreparation::Cancelled => {
                Ok(ProviderSwitchOutcome::Cancelled)
            }
            heycode_agent::ProviderSwitchPreparation::Forked {
                session_id,
                fork_event_count,
                ..
            } => Ok(ProviderSwitchOutcome::Forked {
                session_id,
                fork_event_count,
            }),
        }
    }

    /// Persist one primary AgentRuntime while retaining the native inference
    /// provider/model fallback, then apply the committed effective selection.
    ///
    /// # Errors
    /// Runtime must have passed composition-time primary-bridge capability
    /// admission; Settings CAS/persistence failures publish no live change.
    pub fn select_runtime(&self, runtime: &str) -> Result<RoutingSelection, RoutingError> {
        let current = self.selection()?;
        self.commit(
            RoutingSelection::new(
                runtime.to_owned(),
                current.provider().to_owned(),
                current.model().to_owned(),
                current.effort().map(str::to_owned),
            )?
            .with_endpoint(current.endpoint().map(str::to_owned))?
            .with_parameters(current.parameters().clone())?
            .with_credential_reference(current.credential_reference().cloned()),
        )
    }

    /// Persist a runtime model proven by its current catalog.
    ///
    /// # Errors
    /// Unknown runtime, missing/retired model, or Settings persistence failure.
    pub fn select_runtime_model(
        &self,
        runtime: &str,
        model: &str,
        catalog: &heycode_llm::CatalogSnapshot,
    ) -> Result<RoutingSelection, RoutingError> {
        if catalog.provider.id != runtime
            || !catalog
                .models
                .iter()
                .any(|row| row.id == model && row.lifecycle.is_selectable(unix_time_ms()))
        {
            return Err(RoutingError::UnselectableModel {
                provider: runtime.into(),
                model: model.into(),
            });
        }
        let current = self.selection()?;
        self.commit(
            RoutingSelection::new(
                runtime,
                current.provider(),
                current.model(),
                current.effort().map(str::to_owned),
            )?
            .with_runtime_model(model)?
            .with_endpoint(current.endpoint().map(str::to_owned))?
            .with_parameters(current.parameters().clone())?
            .with_credential_reference(current.credential_reference().cloned()),
        )
    }

    /// Persist and publish one exact effort accepted by the active native
    /// adapter/model.
    ///
    /// # Errors
    /// Delegated ownership, unsupported choices or Settings failures leave the
    /// durable and live native route unchanged.
    pub fn select_effort(&self, effort: &str) -> Result<RoutingSelection, RoutingError> {
        self.commit(self.effort_target(effort)?)
    }

    fn effort_target(&self, effort: &str) -> Result<RoutingSelection, RoutingError> {
        let current = self.selection()?;
        if current.runtime() != self.agent.runtime_id() {
            return Err(RoutingError::UnknownRuntime(current.runtime().to_owned()));
        }
        let requested =
            ReasoningEffortId::new(effort).map_err(|_| RoutingError::InvalidSelection("effort"))?;
        let options = self
            .native_effort_options(&current)?
            .ok_or(RoutingError::EffortUnavailable)?;
        if !options.choices().contains(&requested) {
            return Err(RoutingError::InvalidSelection(
                "unsupported reasoning effort",
            ));
        }
        current.with_effort(Some(effort.to_owned()))
    }

    /// Apply one picker result only when the same backend and routing revision
    /// still own the loop.
    ///
    /// # Errors
    /// Stale configuration and delegated application fail before persistence.
    pub async fn select_model_owned(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        catalog: Option<&CatalogSnapshot>,
        model: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<RoutingSelection, RoutingError> {
        self.select_model_owned_in_scope(
            owner,
            revision,
            catalog,
            model,
            SelectionScope::Default,
            cancellation,
        )
        .await
    }

    /// Apply a catalog-proven model to either saved defaults or this session.
    /// Both scopes retain backend ownership, revision and opaque-state checks.
    ///
    /// # Errors
    /// Stale configuration, unsupported models and delegated application errors.
    pub async fn select_model_owned_in_scope(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        catalog: Option<&CatalogSnapshot>,
        model: &str,
        scope: SelectionScope,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<RoutingSelection, RoutingError> {
        self.select_model_configuration_owned(
            owner,
            revision,
            catalog,
            ModelControlChoice {
                model,
                effort: None,
                scope,
            },
            cancellation,
        )
        .await
    }

    /// Apply model and optional effort together after the user confirms a picker.
    /// Saved and session choices use one backend application and one settings CAS.
    ///
    /// # Errors
    /// Stale ownership, unsupported model/effort, opaque state or backend failure.
    pub async fn select_model_configuration_owned(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        catalog: Option<&CatalogSnapshot>,
        choice: ModelControlChoice<'_>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<RoutingSelection, RoutingError> {
        let ModelControlChoice {
            model,
            effort,
            scope,
        } = choice;
        let active = self.active_configuration()?;
        if active.owner() != owner || active.revision() != revision {
            return Err(RoutingError::InvalidSelection("stale model picker"));
        }
        if let Some(effort) = effort {
            let choices = self
                .model_effort_catalog_owned(owner, revision, model, cancellation.child_token())
                .await?;
            if !choices.choices().iter().any(|value| value == effort) {
                return Err(RoutingError::InvalidSelection(
                    "unsupported reasoning effort",
                ));
            }
        }
        match owner {
            heycode_agent::BackendControlOwner::NativeInference { provider } => {
                if let Some(catalog) = catalog
                    && (catalog.provider.id != *provider
                        || !catalog.models.iter().any(|row| {
                            row.id == model && row.lifecycle.is_selectable(unix_time_ms())
                        }))
                {
                    return Err(RoutingError::UnselectableModel {
                        provider: provider.clone(),
                        model: model.to_owned(),
                    });
                }
                let target = self
                    .model_target(model)?
                    .with_effort(effort.map(str::to_owned))?;
                if self
                    .agent
                    .provider_switch_barrier(target.provider(), target.model())
                    .map_err(|_| RoutingError::OpaqueStatePreparation)?
                    .is_some()
                {
                    return Err(RoutingError::OpaqueStateResolutionRequired {
                        provider: target.provider().to_owned(),
                        model: target.model().to_owned(),
                    });
                }
                self.commit_in_scope(target, revision, scope)
            }
            heycode_agent::BackendControlOwner::DelegatedRuntime { runtime } => {
                let catalog = catalog.ok_or(RoutingError::InvalidSelection(
                    "delegated model selection requires catalog evidence",
                ))?;
                if catalog.provider.id != *runtime
                    || !catalog
                        .models
                        .iter()
                        .any(|row| row.id == model && row.lifecycle.is_selectable(unix_time_ms()))
                {
                    return Err(RoutingError::UnselectableModel {
                        provider: runtime.clone(),
                        model: model.to_owned(),
                    });
                }
                let current = self.selection()?;
                let candidate = current
                    .clone()
                    .with_runtime_model(model)?
                    .with_runtime_effort(effort.map(str::to_owned))?;
                self.preflight_delegated_write(&candidate, revision, scope)?;
                let controls = self.delegated_controls()?;
                self.require_control_runtime(controls.as_ref(), runtime)?;
                let mut update = heycode_runtime::RuntimeConfiguration::new()
                    .with_model(model)
                    .map_err(|_| RoutingError::InvalidSelection("runtime model"))?;
                if let Some(effort) = effort {
                    update = update
                        .with_reasoning_effort(effort)
                        .map_err(|_| RoutingError::InvalidSelection("runtime effort"))?;
                }
                let applied = controls
                    .configure(runtime, update, cancellation)
                    .await
                    .map_err(RoutingError::BackendControl)?;
                let effective = applied.configuration();
                if effective.model() != Some(model)
                    || effort.is_some_and(|effort| effective.reasoning_effort() != Some(effort))
                {
                    return Err(self
                        .invalidate_applied_configuration(controls.as_ref(), runtime, &applied)
                        .await);
                }
                let candidate = match candidate
                    .with_runtime_effort(effective.reasoning_effort().map(str::to_owned))
                {
                    Ok(candidate) => candidate,
                    Err(_) => {
                        return Err(self
                            .invalidate_applied_configuration(controls.as_ref(), runtime, &applied)
                            .await);
                    }
                };
                match self.persist_delegated(candidate, owner, revision, scope) {
                    Ok(selection) => Ok(selection),
                    Err(_) => Err(self
                        .invalidate_applied_configuration(controls.as_ref(), runtime, &applied)
                        .await),
                }
            }
        }
    }

    /// Apply one effort picker result only when backend ownership and routing
    /// revision are unchanged.
    ///
    /// # Errors
    /// Stale configuration, delegated application or invalid effort values.
    pub async fn select_effort_owned(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        effort: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<RoutingSelection, RoutingError> {
        self.select_effort_owned_in_scope(
            owner,
            revision,
            effort,
            SelectionScope::Default,
            cancellation,
        )
        .await
    }

    /// Apply an exact backend-owned effort for this session or as the default.
    /// Session scope changes only this composed process's Settings override.
    ///
    /// # Errors
    /// Stale ownership, invalid effort, managed locks or backend failure.
    pub async fn select_effort_owned_in_scope(
        &self,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        effort: &str,
        scope: SelectionScope,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<RoutingSelection, RoutingError> {
        let active = self.active_configuration()?;
        if active.owner() != owner || active.revision() != revision {
            return Err(RoutingError::InvalidSelection("stale effort picker"));
        }
        let catalog = self
            .effort_catalog_owned(owner, revision, cancellation.child_token())
            .await?;
        if !catalog.choices().iter().any(|choice| choice == effort) {
            return Err(RoutingError::InvalidSelection(
                "unsupported reasoning effort",
            ));
        }
        match owner {
            heycode_agent::BackendControlOwner::NativeInference { .. } => {
                self.commit_in_scope(self.effort_target(effort)?, revision, scope)
            }
            heycode_agent::BackendControlOwner::DelegatedRuntime { runtime } => {
                let current = self.selection()?;
                let candidate = current
                    .clone()
                    .with_runtime_effort(Some(effort.to_owned()))?;
                self.preflight_delegated_write(&candidate, revision, scope)?;
                let controls = self.delegated_controls()?;
                self.require_control_runtime(controls.as_ref(), runtime)?;
                let update = heycode_runtime::RuntimeConfiguration::new()
                    .with_reasoning_effort(effort)
                    .map_err(|_| RoutingError::InvalidSelection("runtime effort"))?;
                let applied = controls
                    .configure(runtime, update, cancellation)
                    .await
                    .map_err(RoutingError::BackendControl)?;
                let effective = applied.configuration();
                if effective.reasoning_effort() != Some(effort)
                    || effective.model() != active.model()
                {
                    return Err(self
                        .invalidate_applied_configuration(controls.as_ref(), runtime, &applied)
                        .await);
                }
                match self.persist_delegated(candidate, owner, revision, scope) {
                    Ok(selection) => Ok(selection),
                    Err(_) => Err(self
                        .invalidate_applied_configuration(controls.as_ref(), runtime, &applied)
                        .await),
                }
            }
        }
    }

    /// Apply a selection that was read back from Settings rather than chosen
    /// in this session.
    ///
    /// A saved effort the active model no longer publishes is applied as
    /// unset, with an explicit receipt, instead of being silently kept, coerced
    /// into a neighbouring value, or blocking the route entirely. The saved
    /// file is left exactly as written so the operator can see and replace it.
    ///
    /// # Errors
    /// Every failure [`Self::apply_effective`] reports except the retired
    /// effort this method reports as a receipt.
    pub(crate) fn apply_persisted_selection(
        &self,
        selection: &RoutingSelection,
    ) -> Result<(), RoutingError> {
        let Some(saved_effort) = selection.effort() else {
            return self.apply_effective(selection);
        };
        let retained = match self.native_effort_options(selection) {
            Ok(Some(options)) => options
                .choices()
                .iter()
                .any(|choice| choice.as_str() == saved_effort),
            // Absent or unreadable metadata is not evidence that the saved
            // value is wrong; leave that judgement to `apply_effective`.
            Ok(None) | Err(_) => true,
        };
        if retained {
            return self.apply_effective(selection);
        }
        self.emit_error(format!(
            "saved reasoning effort `{saved_effort}` is not published for model `{}`; this session starts with no effort until you choose one",
            selection.model()
        ));
        self.apply_effective(&selection.clone().with_effort(None)?)
    }

    pub(crate) fn apply_effective(&self, selection: &RoutingSelection) -> Result<(), RoutingError> {
        self.validate_effective(selection)?;
        let effort = match selection.effort() {
            Some(value) => {
                let effort = ReasoningEffortId::new(value)
                    .map_err(|_| RoutingError::InvalidSelection("effort"))?;
                let options = self
                    .native_effort_options(selection)?
                    .ok_or(RoutingError::EffortUnavailable)?;
                if !options.choices().contains(&effort) {
                    return Err(RoutingError::InvalidSelection(
                        "unsupported reasoning effort",
                    ));
                }
                Some(effort)
            }
            None => None,
        };
        let current = self.agent.selection();
        if (current.provider_name != selection.provider() || current.model != selection.model())
            && self
                .agent
                .provider_switch_barrier(selection.provider(), selection.model())
                .map_err(|_| RoutingError::OpaqueStatePreparation)?
                .is_some()
        {
            return Err(RoutingError::OpaqueStateResolutionRequired {
                provider: selection.provider().to_owned(),
                model: selection.model().to_owned(),
            });
        }
        self.agent.set_inference_route(
            selection.provider().to_owned(),
            selection.model().to_owned(),
            effort,
        );
        Ok(())
    }

    fn commit(&self, selection: RoutingSelection) -> Result<RoutingSelection, RoutingError> {
        self.validate_effective(&selection)?;
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or_else(|| RoutingError::Settings("routing namespace is not registered".into()))?;
        self.commit_in_scope(selection, snapshot.revision(), SelectionScope::Default)
    }

    fn commit_in_scope(
        &self,
        selection: RoutingSelection,
        revision: u64,
        scope: SelectionScope,
    ) -> Result<RoutingSelection, RoutingError> {
        self.validate_effective(&selection)?;
        let next = self.write_selection(&selection, revision, scope)?;
        let effective = RoutingSelection::from_value(next.resolved())?;
        self.apply_effective(&effective)?;
        Ok(effective)
    }

    fn write_selection(
        &self,
        selection: &RoutingSelection,
        revision: u64,
        scope: SelectionScope,
    ) -> Result<Arc<heycode_settings::SettingsSnapshot>, RoutingError> {
        match scope {
            SelectionScope::Default => {
                self.settings
                    .replace_user(&self.namespace, selection.to_value(), Some(revision))
            }
            SelectionScope::Session => self.settings.replace_override(
                &self.namespace,
                selection.to_value(),
                Some(revision),
            ),
        }
        .map_err(|error| RoutingError::Settings(error.to_string()))
    }

    fn delegated_controls(&self) -> Result<Arc<dyn DelegatedRuntimeControls>, RoutingError> {
        self.delegated_controls
            .read()
            .map_err(|_| RoutingError::RegistryUnavailable)?
            .clone()
            .ok_or_else(|| {
                RoutingError::BackendControl(
                    "delegated backend controls are not registered".to_owned(),
                )
            })
    }

    async fn invalidate_applied_configuration(
        &self,
        controls: &dyn DelegatedRuntimeControls,
        runtime: &str,
        applied: &AppliedDelegatedConfiguration,
    ) -> RoutingError {
        let _retired = controls
            .invalidate(runtime, applied.backend_generation())
            .await;
        RoutingError::BackendSessionRetired
    }

    fn require_control_runtime(
        &self,
        controls: &dyn DelegatedRuntimeControls,
        expected: &str,
    ) -> Result<(), RoutingError> {
        let actual = controls
            .active_runtime_id()
            .map_err(RoutingError::BackendControl)?;
        if actual != expected {
            return Err(RoutingError::InvalidSelection("stale backend owner"));
        }
        Ok(())
    }

    fn preflight_delegated_write(
        &self,
        candidate: &RoutingSelection,
        revision: u64,
        scope: SelectionScope,
    ) -> Result<(), RoutingError> {
        if scope == SelectionScope::Default && !self.settings.writable() {
            return Err(RoutingError::Settings(
                "routing settings are read-only".to_owned(),
            ));
        }
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|error| RoutingError::Settings(error.to_string()))?
            .ok_or_else(|| RoutingError::Settings("routing namespace is not registered".into()))?;
        if snapshot.revision() != revision {
            return Err(RoutingError::InvalidSelection(
                "stale backend configuration",
            ));
        }
        let expected = [
            ("runtime_model", candidate.runtime_model()),
            ("runtime_effort", candidate.runtime_effort()),
        ];
        for layer in [
            if scope == SelectionScope::Default {
                snapshot.project()
            } else {
                None
            },
            snapshot.managed(),
        ]
        .into_iter()
        .flatten()
        {
            for (field, requested) in expected {
                let imposed = match layer.get(field) {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(value)) => Some(value.as_str()),
                    Some(_) => {
                        return Err(RoutingError::InvalidSelection("delegated routing control"));
                    }
                };
                if imposed.is_some() && imposed != requested {
                    return Err(RoutingError::Settings(format!(
                        "a higher-priority setting controls `{field}`"
                    )));
                }
            }
        }
        Ok(())
    }

    fn persist_delegated(
        &self,
        candidate: RoutingSelection,
        owner: &heycode_agent::BackendControlOwner,
        revision: u64,
        scope: SelectionScope,
    ) -> Result<RoutingSelection, RoutingError> {
        let active = self.active_configuration()?;
        if active.owner() != owner || active.revision() != revision {
            return Err(RoutingError::InvalidSelection(
                "stale backend configuration",
            ));
        }
        let next = self.write_selection(&candidate, revision, scope)?;
        let effective = RoutingSelection::from_value(next.resolved())?;
        if effective != candidate {
            return Err(RoutingError::Settings(
                "a higher-priority route prevented the delegated configuration".to_owned(),
            ));
        }
        self.apply_effective(&effective)?;
        Ok(effective)
    }

    pub(crate) fn validate_fallback(
        &self,
        config: &crate::fallback::FallbackConfig,
    ) -> Result<RoutingSelection, RoutingError> {
        let current = self.selection()?;
        if current.runtime() != self.agent.runtime_id() {
            return Err(RoutingError::InvalidSelection(
                "fallback requires native inference",
            ));
        }
        let profile = self
            .provider_profile(&config.provider)
            .ok_or_else(|| RoutingError::UnknownProvider(config.provider.clone()))?;
        if config.model != profile.default_model {
            self.models
                .resolve_model(&config.provider, &config.model, unix_time_ms())
                .map_err(|_| RoutingError::UnselectableModel {
                    provider: config.provider.clone(),
                    model: config.model.clone(),
                })?;
        }
        let mut target =
            RoutingSelection::new(current.runtime(), &config.provider, &config.model, None)?;
        if config.provider == current.provider() {
            target = target
                .with_endpoint(current.endpoint().map(str::to_owned))?
                .with_parameters(current.parameters().clone())?
                .with_credential_reference(current.credential_reference().cloned());
        }
        self.validate_effective(&target)?;
        if self
            .agent
            .provider_switch_barrier(target.provider(), target.model())
            .map_err(|_| RoutingError::OpaqueStatePreparation)?
            .is_some()
        {
            return Err(RoutingError::OpaqueStateResolutionRequired {
                provider: target.provider().into(),
                model: target.model().into(),
            });
        }
        Ok(target)
    }

    pub(crate) fn apply_configured_fallback(
        &self,
        from: &heycode_llm::LlmSelection,
        config: &crate::fallback::FallbackConfig,
    ) -> Result<(), RoutingError> {
        let snapshot = self
            .settings
            .get(&self.namespace)
            .map_err(|e| RoutingError::Settings(e.to_string()))?
            .ok_or_else(|| RoutingError::Settings("routing namespace unavailable".into()))?;
        let current = RoutingSelection::from_value(snapshot.resolved())?;
        if current.provider() != from.provider_name
            || current.model() != from.model
            || self.agent.selection().provider_name != from.provider_name
            || self.agent.selection().model != from.model
        {
            return Err(RoutingError::InvalidSelection(
                "stale fallback source route",
            ));
        }
        let target = self.validate_fallback(config)?;
        let value = target.to_value();
        for layer in [
            snapshot.project(),
            snapshot.managed(),
            snapshot.override_layer(),
        ]
        .into_iter()
        .flatten()
        {
            for field in [
                "provider",
                "model",
                "runtime",
                "effort",
                "endpoint",
                "parameters",
                "credential_reference",
            ] {
                if let Some(imposed) = layer.get(field)
                    && Some(imposed) != value.get(field)
                {
                    return Err(RoutingError::Settings(format!(
                        "a higher-priority setting controls `{field}`; fallback cannot change it"
                    )));
                }
            }
        }
        let next = self
            .settings
            .replace_user_automatically(&self.namespace, value, &snapshot, |candidate| {
                let effective =
                    RoutingSelection::from_value(candidate.resolved()).map_err(|e| {
                        heycode_settings::SettingsError::InvalidResolved {
                            namespace: self.namespace.to_string(),
                            message: e.to_string(),
                        }
                    })?;
                if effective.to_value() != target.to_value() {
                    return Err(heycode_settings::SettingsError::InvalidResolved {
                        namespace: self.namespace.to_string(),
                        message: "fallback route is controlled by higher-priority settings".into(),
                    });
                }
                Ok(())
            })
            .map_err(|e| RoutingError::Settings(e.to_string()))?;
        let effective = RoutingSelection::from_value(next.resolved())?;
        if effective.provider() != target.provider() || effective.model() != target.model() {
            return Err(RoutingError::InvalidSelection(
                "fallback did not become effective",
            ));
        }
        self.apply_effective(&effective)
    }

    fn validate_effective(&self, selection: &RoutingSelection) -> Result<(), RoutingError> {
        if !self.activatable_runtimes.contains(selection.runtime()) {
            return Err(RoutingError::UnknownRuntime(selection.runtime().to_owned()));
        }
        if self.providers.get(selection.provider()).is_none() {
            return Err(RoutingError::UnknownProvider(
                selection.provider().to_owned(),
            ));
        }
        Ok(())
    }

    fn native_effort_options(
        &self,
        selection: &RoutingSelection,
    ) -> Result<Option<ReasoningEffortOptions>, RoutingError> {
        let provider = self
            .providers
            .get(selection.provider())
            .ok_or_else(|| RoutingError::UnknownProvider(selection.provider().to_owned()))?;
        let model = self
            .models
            .resolve_model(selection.provider(), selection.model(), unix_time_ms())
            .map(|resolved| resolved.descriptor)
            .unwrap_or_else(|_| provider.describe_model(selection.model()));
        let Some(adapter) = provider.inference_adapter() else {
            return Ok(None);
        };
        adapter
            .reasoning_effort_options(&model)
            .map_err(|_| RoutingError::InvalidSelection("reasoning effort metadata"))
    }

    pub(crate) fn emit_error(&self, message: String) {
        self.agent
            .ui()
            .emit(heycode_agent::UiEvent::Error { message });
    }

    fn provider_profile(&self, id: &str) -> Option<ProviderProfile> {
        self.providers
            .profiles()
            .into_iter()
            .find(|profile| profile.registry_name == id)
    }

    fn provider_target(&self, provider: &str) -> Result<RoutingSelection, RoutingError> {
        let profile = self
            .provider_profile(provider)
            .ok_or_else(|| RoutingError::UnknownProvider(provider.to_owned()))?;
        let current = self.selection()?;
        RoutingSelection::new(
            current.runtime().to_owned(),
            profile.registry_name,
            profile.default_model,
            None,
        )
    }

    fn model_target(&self, model: &str) -> Result<RoutingSelection, RoutingError> {
        let current = self.selection()?;
        let profile = self
            .provider_profile(current.provider())
            .ok_or_else(|| RoutingError::UnknownProvider(current.provider().to_owned()))?;
        let model = if model == profile.default_model {
            model.to_owned()
        } else {
            self.models
                .resolve_model(current.provider(), model, unix_time_ms())
                .map(|selection| selection.descriptor.id.clone())
                .map_err(|_| RoutingError::UnselectableModel {
                    provider: current.provider().to_owned(),
                    model: model.to_owned(),
                })?
        };
        RoutingSelection::new(
            current.runtime().to_owned(),
            current.provider().to_owned(),
            model,
            None,
        )?
        .with_endpoint(current.endpoint().map(str::to_owned))
        .and_then(|selection| selection.with_parameters(current.parameters().clone()))
        .map(|selection| {
            selection.with_credential_reference(current.credential_reference().cloned())
        })
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

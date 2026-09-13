//! Effect-owned model catalog registry with bounded cache freshness and
//! single-flight refreshes.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::FutureExt;
use heycode_core::{Context, CoreError, Plugin};
use thiserror::Error;
use tokio::sync::Notify;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use crate::{ModelDescriptor, ProviderDescriptor, SERVICE_MODELS};

/// Stable failure class emitted by a provider-owned catalog source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFailureKind {
    /// The catalog operation was cancelled.
    Cancelled,
    /// The provider rejected the active credential.
    Unauthorized,
    /// The provider endpoint could not be reached.
    Network,
    /// The endpoint responded but is temporarily unavailable.
    Unavailable,
    /// Provider bytes did not match the catalog contract.
    InvalidResponse,
}

/// Safe source failure. Messages must never contain credentials or raw
/// provider response bodies.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct CatalogFetchError {
    kind: CatalogFailureKind,
    message: String,
}

/// Safe durable catalog-store failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct CatalogPersistenceError {
    message: String,
}

impl CatalogPersistenceError {
    /// Construct a safe persistence diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Safe diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl CatalogFetchError {
    /// Construct a safe classified source failure.
    #[must_use]
    pub fn new(kind: CatalogFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Standard cancellation failure.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(CatalogFailureKind::Cancelled, "catalog refresh cancelled")
    }

    /// Stable failure class.
    #[must_use]
    pub const fn kind(&self) -> CatalogFailureKind {
        self.kind
    }

    /// Safe diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Catalog registry and refresh failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CatalogError {
    /// Two catalog plugins claimed the same provider id.
    #[error("model catalog `{provider}` is already registered")]
    DuplicateCatalog {
        /// Contested provider id.
        provider: String,
    },
    /// A second durable cache provider attempted to register.
    #[error("model catalog persistence is already registered")]
    DuplicatePersistence,
    /// No catalog plugin owns the requested provider id.
    #[error("no model catalog is registered for `{provider}`")]
    UnknownCatalog {
        /// Requested provider id.
        provider: String,
    },
    /// No successful refresh exists for the provider.
    #[error("model catalog `{provider}` has no cached snapshot")]
    NoCachedCatalog {
        /// Requested provider id.
        provider: String,
    },
    /// Shared registry state could not be accessed.
    #[error("model catalog registry is unavailable")]
    RegistryUnavailable,
    /// The caller or catalog lifecycle cancelled the wait.
    #[error("model catalog refresh for `{provider}` was cancelled")]
    Cancelled {
        /// Provider id being refreshed.
        provider: String,
    },
    /// Provider-owned discovery failed with a safe classified diagnostic.
    #[error("model catalog refresh for `{provider}` failed ({kind:?}): {message}")]
    Refresh {
        /// Provider id being refreshed.
        provider: String,
        /// Stable failure class.
        kind: CatalogFailureKind,
        /// Safe diagnostic message.
        message: String,
    },
    /// A catalog plugin returned structurally invalid model data.
    #[error("model catalog `{provider}` is invalid: {message}")]
    InvalidCatalog {
        /// Provider id whose source returned invalid data.
        provider: String,
        /// Safe structural diagnostic.
        message: String,
    },
    /// Durable cache load or commit failed.
    #[error("model catalog persistence failed: {message}")]
    Persistence {
        /// Safe persistence diagnostic.
        message: String,
    },
}

/// Provider-owned live model discovery boundary.
#[async_trait]
pub trait ModelCatalog: Send + Sync {
    /// Whether this source accepts explicit credentials for a draft endpoint.
    fn supports_endpoint_credentials(&self) -> bool {
        false
    }
    /// Whether this source accepts explicit credentials for draft cloud coordinates.
    fn supports_parameter_credentials(&self) -> bool {
        false
    }
    /// Discover an endpoint using only the explicitly supplied operation credential.
    ///
    /// # Errors
    /// Unsupported authentication, invalid endpoint or classified discovery failure.
    async fn fetch_endpoint_with_credential(
        &self,
        endpoint: &str,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if credential.is_some() {
            return Err(CatalogFetchError::new(
                CatalogFailureKind::Unavailable,
                "This provider does not support authenticated endpoint setup",
            ));
        }
        self.fetch_endpoint(endpoint, cancellation).await
    }
    /// Discover draft cloud coordinates using only the explicitly supplied credential.
    ///
    /// # Errors
    /// Unsupported authentication, invalid coordinates or classified discovery failure.
    async fn fetch_parameters_with_credential(
        &self,
        parameters: &BTreeMap<String, String>,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if credential.is_some() {
            return Err(CatalogFetchError::new(
                CatalogFailureKind::Unavailable,
                "This provider does not support authenticated coordinate setup",
            ));
        }
        self.fetch_parameters(parameters, cancellation).await
    }
    /// Safe provider identity advertised by this source.
    fn provider(&self) -> ProviderDescriptor;

    /// Fetch one complete current model list.
    ///
    /// Implementations must honor `cancellation` and return only safe,
    /// redacted diagnostics.
    ///
    /// # Errors
    /// Returns a classified provider/network/decoding failure.
    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError>;

    /// Discover a draft endpoint with no inherited credentials or cache writes.
    ///
    /// # Errors
    /// Unsupported editing, invalid endpoint, cancellation or provider failure.
    async fn fetch_endpoint(
        &self,
        _endpoint: &str,
        _cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "This provider does not support endpoint editing",
        ))
    }

    /// Discover draft cloud coordinates with no cache writes.
    ///
    /// # Errors
    /// Unsupported coordinates, cancellation or provider failure.
    async fn fetch_parameters(
        &self,
        _parameters: &BTreeMap<String, String>,
        _cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "This provider does not support connection coordinates",
        ))
    }
}

/// Durable whole-generation catalog cache boundary.
///
/// Implementations load and atomically replace the complete set. Values are
/// safe model metadata only; configured provider/model selection remains in
/// settings/config and never enters this store.
pub trait CatalogPersistence: Send + Sync {
    /// Load every durable provider generation.
    ///
    /// # Errors
    /// Malformed/newer data, unsafe paths or I/O failures fail loud without
    /// publishing a partial generation.
    fn load(&self) -> Result<Vec<CatalogSnapshot>, CatalogPersistenceError>;

    /// Durably replace every provider generation before returning success.
    ///
    /// # Errors
    /// Cancellation before the commit point or durable write failure.
    fn save(
        &self,
        generations: &[Arc<CatalogSnapshot>],
        cancellation: &CancellationToken,
    ) -> Result<(), CatalogPersistenceError>;
}

/// Whether a refresh may use a still-fresh cached snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogRefreshMode {
    /// Return a fresh cached snapshot; otherwise perform one live refresh.
    PreferCache,
    /// Perform or join a live refresh even when the cache is fresh.
    Force,
}

/// Provenance of a successful catalog read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFreshness {
    /// This call performed or joined a successful live refresh.
    Live,
    /// The existing cache remained inside its configured TTL.
    FreshCache,
    /// A live refresh failed and the last-good cache was returned visibly.
    StaleFallback,
}

/// One immutable successful provider catalog generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSnapshot {
    /// Provider identity captured at registration.
    pub provider: ProviderDescriptor,
    /// Deterministically id-sorted model rows.
    pub models: Vec<ModelDescriptor>,
    /// Monotonic provider-local successful refresh revision, starting at one.
    pub revision: u64,
    /// Successful refresh commit time in Unix milliseconds.
    pub fetched_at_ms: u64,
}

/// Catalog read result with explicit freshness and optional stale warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogView {
    /// Immutable catalog generation.
    pub snapshot: Arc<CatalogSnapshot>,
    /// How this call obtained the generation.
    pub freshness: CatalogFreshness,
    /// Live refresh failure retained when returning stale last-good data.
    pub warning: Option<CatalogError>,
}

struct CatalogFlight {
    result: Mutex<Option<Result<Arc<CatalogSnapshot>, CatalogError>>>,
    task: Mutex<Option<AbortHandle>>,
    notify: Notify,
}

impl CatalogFlight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            task: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    fn set_task(&self, task: AbortHandle) {
        let completed = self.result.lock().map_or(true, |result| result.is_some());
        if completed {
            task.abort();
        } else if let Ok(mut slot) = self.task.lock() {
            *slot = Some(task);
        } else {
            task.abort();
        }
    }

    fn abort(&self) {
        if let Ok(mut task) = self.task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }

    fn complete(&self, result: Result<Arc<CatalogSnapshot>, CatalogError>) {
        let published = if let Ok(mut slot) = self.result.lock() {
            if slot.is_some() {
                false
            } else {
                *slot = Some(result);
                true
            }
        } else {
            false
        };
        if let Ok(mut task) = self.task.lock() {
            task.take();
        }
        if published {
            self.notify.notify_waiters();
        }
    }

    async fn wait(
        &self,
        provider: &str,
        cancellation: &CancellationToken,
    ) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        loop {
            if cancellation.is_cancelled() {
                return Err(CatalogError::Cancelled {
                    provider: provider.to_owned(),
                });
            }
            let notified = self.notify.notified();
            let result = self
                .result
                .lock()
                .map_err(|_| CatalogError::RegistryUnavailable)?
                .clone();
            if let Some(result) = result {
                return result;
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Err(CatalogError::Cancelled {
                        provider: provider.to_owned(),
                    });
                }
                () = notified => {}
            }
        }
    }
}

struct CatalogState {
    cached: Option<Arc<CatalogSnapshot>>,
    in_flight: Option<Arc<CatalogFlight>>,
    next_revision: u64,
}

struct PersistenceEntry {
    store: Arc<dyn CatalogPersistence>,
    registration: Arc<()>,
    cancellation: CancellationToken,
}

struct CatalogEntry {
    descriptor: ProviderDescriptor,
    source: Arc<dyn ModelCatalog>,
    registration: Arc<()>,
    cancellation: CancellationToken,
    state: Mutex<CatalogState>,
}

impl CatalogEntry {
    fn stop(&self) {
        self.cancellation.cancel();
        let flight = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.in_flight.take());
        if let Some(flight) = flight {
            flight.abort();
            flight.complete(Err(CatalogError::Cancelled {
                provider: self.descriptor.id.clone(),
            }));
        }
    }
}

struct CatalogRegistryInner {
    entries: Mutex<BTreeMap<String, Arc<CatalogEntry>>>,
    generations: Mutex<BTreeMap<String, Arc<CatalogSnapshot>>>,
    persistence: Mutex<Option<PersistenceEntry>>,
    persistence_operations: Mutex<()>,
    ttl: Duration,
    cancellation: CancellationToken,
}

/// Shared provider catalog registry and last-good cache.
#[derive(Clone)]
pub struct CatalogRegistry {
    inner: Arc<CatalogRegistryInner>,
}

impl CatalogRegistry {
    /// Build an empty registry with the freshness duration applied per source.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(CatalogRegistryInner {
                entries: Mutex::new(BTreeMap::new()),
                generations: Mutex::new(BTreeMap::new()),
                persistence: Mutex::new(None),
                persistence_operations: Mutex::new(()),
                ttl,
                cancellation: CancellationToken::new(),
            }),
        }
    }

    /// Register a unique provider catalog as a context-owned effect.
    ///
    /// Context shutdown removes the source, cancels its active refresh and
    /// leaves no registration reachable through this registry.
    ///
    /// # Errors
    /// Empty/duplicate provider ids or poisoned registry state fail before
    /// publication.
    pub fn register(
        &self,
        context: &Context,
        source: Arc<dyn ModelCatalog>,
    ) -> Result<(), CatalogError> {
        let descriptor = source.provider();
        let provider = descriptor.id.trim();
        if provider.is_empty() {
            return Err(CatalogError::InvalidCatalog {
                provider: descriptor.id,
                message: "provider id must not be blank".to_owned(),
            });
        }
        let provider = provider.to_owned();
        let restored = self
            .inner
            .generations
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?
            .get(&provider)
            .cloned();
        let next_revision =
            match restored.as_ref() {
                Some(snapshot) => snapshot.revision.checked_add(1).ok_or_else(|| {
                    CatalogError::InvalidCatalog {
                        provider: provider.clone(),
                        message: "catalog revision is exhausted".to_owned(),
                    }
                })?,
                None => 1,
            };
        let registration = Arc::new(());
        let entry = Arc::new(CatalogEntry {
            descriptor,
            source,
            registration: registration.clone(),
            cancellation: self.inner.cancellation.child_token(),
            state: Mutex::new(CatalogState {
                cached: restored,
                in_flight: None,
                next_revision,
            }),
        });
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?;
        if entries.contains_key(&provider) {
            return Err(CatalogError::DuplicateCatalog { provider });
        }
        entries.insert(provider.clone(), entry);
        drop(entries);

        let registration = CatalogRegistration {
            inner: Arc::downgrade(&self.inner),
            provider,
            token: registration,
            active: true,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Install one durable whole-generation cache provider as a context
    /// effect, validating and restoring all rows before publication.
    ///
    /// Existing in-memory generations win when newer and are durably merged
    /// before the provider becomes active.
    ///
    /// # Errors
    /// Duplicate persistence, load/save failure, invalid generations or
    /// poisoned registry state fail without partial publication.
    pub fn register_persistence(
        &self,
        context: &Context,
        store: Arc<dyn CatalogPersistence>,
    ) -> Result<(), CatalogError> {
        let _operations = self
            .inner
            .persistence_operations
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?;
        if self
            .inner
            .persistence
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?
            .is_some()
        {
            return Err(CatalogError::DuplicatePersistence);
        }

        let loaded = store.load().map_err(persistence_error)?;
        let loaded = validate_persisted_generations(loaded)?;
        let mut merged = loaded.clone();
        let current = self
            .inner
            .generations
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?
            .clone();
        for (provider, snapshot) in current {
            let replace = merged
                .get(&provider)
                .is_none_or(|loaded| generation_is_newer(&snapshot, loaded));
            if replace {
                merged.insert(provider, snapshot);
            }
        }

        let registration = Arc::new(());
        let cancellation = self.inner.cancellation.child_token();
        if merged != loaded {
            let generations: Vec<_> = merged.values().cloned().collect();
            store
                .save(&generations, &cancellation)
                .map_err(persistence_error)?;
        }
        {
            let mut persistence = self
                .inner
                .persistence
                .lock()
                .map_err(|_| CatalogError::RegistryUnavailable)?;
            if persistence.is_some() {
                return Err(CatalogError::DuplicatePersistence);
            }
            *persistence = Some(PersistenceEntry {
                store,
                registration: registration.clone(),
                cancellation,
            });
        }
        *self
            .inner
            .generations
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)? = merged.clone();
        seed_entries(&self.inner, &merged)?;

        let registration = PersistenceRegistration {
            inner: Arc::downgrade(&self.inner),
            token: registration,
            active: true,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Whether a durable catalog cache provider is active.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn has_persistence(&self) -> Result<bool, CatalogError> {
        Ok(self
            .inner
            .persistence
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?
            .is_some())
    }

    /// Safe provider descriptors in stable id order.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn descriptors(&self) -> Result<Vec<ProviderDescriptor>, CatalogError> {
        let entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?;
        Ok(entries
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect())
    }

    /// Discover a draft endpoint without publishing or persisting its model list.
    ///
    /// # Errors
    /// Unknown provider, unsupported endpoint editing, cancellation or invalid discovery.
    pub async fn probe_endpoint(
        &self,
        provider: &str,
        endpoint: &str,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, CatalogError> {
        self.probe_endpoint_with_credential(provider, endpoint, None, cancellation)
            .await
    }

    /// Discover an uncached endpoint with an explicitly entered operation credential.
    ///
    /// # Errors
    /// Unknown provider, cancellation or classified provider discovery failure.
    pub async fn probe_endpoint_with_credential(
        &self,
        provider: &str,
        endpoint: &str,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, CatalogError> {
        let entry = self.entry(provider)?;
        let operation = cancellation.child_token();
        let _guard = operation.clone().drop_guard();
        let cancelled = || CatalogError::Cancelled {
            provider: provider.into(),
        };
        let result = tokio::select! {
            biased;
            _ = operation.cancelled() => return Err(cancelled()),
            _ = entry.cancellation.cancelled() => return Err(cancelled()),
            result = entry.source.fetch_endpoint_with_credential(endpoint, credential, operation.clone()) => result,
        };
        let mut models = result.map_err(|error| CatalogError::Refresh {
            provider: provider.into(),
            kind: error.kind(),
            message: error.message().into(),
        })?;
        validate_models(provider, &mut models)?;
        Ok(CatalogSnapshot {
            provider: entry.descriptor.clone(),
            models,
            revision: 1,
            fetched_at_ms: unix_time_ms(),
        })
    }

    /// Whether authenticated endpoint setup is provided by the selected source.
    ///
    /// # Errors
    /// Unknown provider or unavailable registry.
    pub fn supports_endpoint_credentials(&self, provider: &str) -> Result<bool, CatalogError> {
        Ok(self.entry(provider)?.source.supports_endpoint_credentials())
    }

    /// Discover draft cloud coordinates without publishing or persisting the model list.
    ///
    /// # Errors
    /// Unknown provider, unsupported coordinates, cancellation or invalid discovery.
    pub async fn probe_parameters(
        &self,
        provider: &str,
        parameters: &BTreeMap<String, String>,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, CatalogError> {
        self.probe_parameters_with_credential(provider, parameters, None, cancellation)
            .await
    }

    /// Discover draft cloud coordinates with an explicitly entered operation credential.
    ///
    /// # Errors
    /// Unknown provider, cancellation or classified provider discovery failure.
    pub async fn probe_parameters_with_credential(
        &self,
        provider: &str,
        parameters: &BTreeMap<String, String>,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, CatalogError> {
        let entry = self.entry(provider)?;
        let operation = cancellation.child_token();
        let _guard = operation.clone().drop_guard();
        let cancelled = || CatalogError::Cancelled {
            provider: provider.into(),
        };
        let result = tokio::select! {
            biased;
            _ = operation.cancelled() => return Err(cancelled()),
            _ = entry.cancellation.cancelled() => return Err(cancelled()),
            result = entry.source.fetch_parameters_with_credential(parameters, credential, operation.clone()) => result,
        };
        let mut models = result.map_err(|error| CatalogError::Refresh {
            provider: provider.into(),
            kind: error.kind(),
            message: error.message().into(),
        })?;
        validate_models(provider, &mut models)?;
        Ok(CatalogSnapshot {
            provider: entry.descriptor.clone(),
            models,
            revision: 1,
            fetched_at_ms: unix_time_ms(),
        })
    }

    /// Whether authenticated coordinate setup is provided by the selected source.
    ///
    /// # Errors
    /// Unknown provider or unavailable registry.
    pub fn supports_parameter_credentials(&self, provider: &str) -> Result<bool, CatalogError> {
        Ok(self
            .entry(provider)?
            .source
            .supports_parameter_credentials())
    }

    /// Return the last successful generation without refreshing.
    ///
    /// # Errors
    /// Unknown provider, absent last-good generation, or poisoned state.
    pub fn cached(&self, provider: &str) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        let entry = self.entry(provider)?;
        entry
            .state
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?
            .cached
            .clone()
            .ok_or_else(|| CatalogError::NoCachedCatalog {
                provider: provider.to_owned(),
            })
    }

    /// Resolve a configured model id against the provider's last successful
    /// catalog generation at an explicit Unix-millisecond instant.
    ///
    /// # Errors
    /// Catalog access, unknown model ids and retired models fail before a
    /// request can be dispatched.
    pub fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        at_ms: u64,
    ) -> Result<crate::ResolvedModelSelection, crate::ModelSelectionError> {
        self.cached(provider)?.resolve_model(model, at_ms)
    }

    /// Read or refresh a provider catalog.
    ///
    /// Concurrent refreshes for one provider join one shared operation.
    /// Ordinary refresh failure returns a visible stale last-good snapshot;
    /// forced refresh failure remains an error. Cancelling this caller stops
    /// only its wait, not a refresh shared by other consumers.
    ///
    /// # Errors
    /// Unknown provider, cancellation, source failure without permitted stale
    /// fallback, invalid model data, or poisoned registry state.
    pub async fn refresh(
        &self,
        provider: &str,
        mode: CatalogRefreshMode,
        cancellation: CancellationToken,
    ) -> Result<CatalogView, CatalogError> {
        if cancellation.is_cancelled() {
            return Err(CatalogError::Cancelled {
                provider: provider.to_owned(),
            });
        }
        let entry = self.entry(provider)?;
        let (flight, start) = {
            let mut state = entry
                .state
                .lock()
                .map_err(|_| CatalogError::RegistryUnavailable)?;
            if mode == CatalogRefreshMode::PreferCache
                && let Some(cached) = &state.cached
                && snapshot_is_fresh(cached, self.inner.ttl)
            {
                return Ok(CatalogView {
                    snapshot: cached.clone(),
                    freshness: CatalogFreshness::FreshCache,
                    warning: None,
                });
            }
            if let Some(flight) = &state.in_flight {
                (flight.clone(), false)
            } else {
                let flight = Arc::new(CatalogFlight::new());
                state.in_flight = Some(flight.clone());
                (flight, true)
            }
        };
        if start {
            spawn_refresh(self.inner.clone(), entry.clone(), flight.clone());
        }

        match flight.wait(provider, &cancellation).await {
            Ok(snapshot) => Ok(CatalogView {
                snapshot,
                freshness: CatalogFreshness::Live,
                warning: None,
            }),
            Err(error @ CatalogError::Cancelled { .. }) => Err(error),
            Err(error) if mode == CatalogRefreshMode::PreferCache => {
                let cached = entry
                    .state
                    .lock()
                    .map_err(|_| CatalogError::RegistryUnavailable)?
                    .cached
                    .clone();
                match cached {
                    Some(snapshot) => Ok(CatalogView {
                        snapshot,
                        freshness: CatalogFreshness::StaleFallback,
                        warning: Some(error),
                    }),
                    None => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn entry(&self, provider: &str) -> Result<Arc<CatalogEntry>, CatalogError> {
        self.inner
            .entries
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?
            .get(provider)
            .cloned()
            .ok_or_else(|| CatalogError::UnknownCatalog {
                provider: provider.to_owned(),
            })
    }

    fn stop(&self) {
        self.inner.cancellation.cancel();
        if let Ok(persistence) = self.inner.persistence.lock()
            && let Some(persistence) = persistence.as_ref()
        {
            persistence.cancellation.cancel();
        }
        if let Ok(entries) = self.inner.entries.lock() {
            for entry in entries.values() {
                entry.stop();
            }
        }
    }
}

fn spawn_refresh(
    inner: Arc<CatalogRegistryInner>,
    entry: Arc<CatalogEntry>,
    flight: Arc<CatalogFlight>,
) {
    let task_entry = entry.clone();
    let task_flight = flight.clone();
    let task = tokio::spawn(async move {
        let fetched = AssertUnwindSafe(task_entry.source.fetch(task_entry.cancellation.clone()))
            .catch_unwind()
            .await;
        let result = match fetched {
            Ok(result) => source_result(&task_entry, result),
            Err(_) => Err(CatalogError::Refresh {
                provider: task_entry.descriptor.id.clone(),
                kind: CatalogFailureKind::Unavailable,
                message: "catalog plugin panicked".to_owned(),
            }),
        };
        commit_refresh(&inner, &task_entry, &task_flight, result);
    });
    flight.set_task(task.abort_handle());
    drop(task);
}

fn source_result(
    entry: &CatalogEntry,
    result: Result<Vec<ModelDescriptor>, CatalogFetchError>,
) -> Result<Vec<ModelDescriptor>, CatalogError> {
    if entry.cancellation.is_cancelled() {
        return Err(CatalogError::Cancelled {
            provider: entry.descriptor.id.clone(),
        });
    }
    result.map_err(|error| {
        if error.kind() == CatalogFailureKind::Cancelled {
            CatalogError::Cancelled {
                provider: entry.descriptor.id.clone(),
            }
        } else {
            CatalogError::Refresh {
                provider: entry.descriptor.id.clone(),
                kind: error.kind(),
                message: error.message().to_owned(),
            }
        }
    })
}

fn commit_refresh(
    inner: &CatalogRegistryInner,
    entry: &CatalogEntry,
    flight: &CatalogFlight,
    result: Result<Vec<ModelDescriptor>, CatalogError>,
) {
    let result = prepare_snapshot(entry, result)
        .and_then(|snapshot| persist_generation(inner, entry, snapshot));
    let result = match entry.state.lock() {
        Ok(mut state) => {
            if state
                .in_flight
                .as_ref()
                .is_some_and(|active| std::ptr::eq(Arc::as_ptr(active), flight))
            {
                state.in_flight = None;
            }
            match result {
                Ok(snapshot) => match snapshot.revision.checked_add(1) {
                    Some(next_revision) => {
                        state.next_revision = next_revision;
                        state.cached = Some(snapshot.clone());
                        Ok(snapshot)
                    }
                    None => Err(CatalogError::InvalidCatalog {
                        provider: entry.descriptor.id.clone(),
                        message: "catalog revision is exhausted".to_owned(),
                    }),
                },
                Err(error) => Err(error),
            }
        }
        Err(_) => Err(CatalogError::RegistryUnavailable),
    };
    flight.complete(result);
}

fn prepare_snapshot(
    entry: &CatalogEntry,
    result: Result<Vec<ModelDescriptor>, CatalogError>,
) -> Result<Arc<CatalogSnapshot>, CatalogError> {
    let mut models = result?;
    validate_models(&entry.descriptor.id, &mut models)?;
    let revision = entry
        .state
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)?
        .next_revision;
    Ok(Arc::new(CatalogSnapshot {
        provider: entry.descriptor.clone(),
        models,
        revision,
        fetched_at_ms: unix_time_ms(),
    }))
}

fn persist_generation(
    inner: &CatalogRegistryInner,
    entry: &CatalogEntry,
    snapshot: Arc<CatalogSnapshot>,
) -> Result<Arc<CatalogSnapshot>, CatalogError> {
    let _operations = inner
        .persistence_operations
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)?;
    if entry.cancellation.is_cancelled() {
        return Err(CatalogError::Cancelled {
            provider: entry.descriptor.id.clone(),
        });
    }
    let persistence = inner
        .persistence
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)?
        .as_ref()
        .map(|entry| (entry.store.clone(), entry.cancellation.clone()));
    let mut generations = inner
        .generations
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)?
        .clone();
    generations.insert(entry.descriptor.id.clone(), snapshot.clone());
    if let Some((store, cancellation)) = persistence {
        store
            .save(
                &generations.values().cloned().collect::<Vec<_>>(),
                &cancellation,
            )
            .map_err(|error| {
                if cancellation.is_cancelled() {
                    CatalogError::Cancelled {
                        provider: entry.descriptor.id.clone(),
                    }
                } else {
                    persistence_error(error)
                }
            })?;
    }
    *inner
        .generations
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)? = generations;
    Ok(snapshot)
}

fn validate_models(provider: &str, models: &mut [ModelDescriptor]) -> Result<(), CatalogError> {
    let mut routes = BTreeSet::new();
    for model in models.iter() {
        if model.id.trim().is_empty() {
            return Err(CatalogError::InvalidCatalog {
                provider: provider.to_owned(),
                message: "model id must not be blank".to_owned(),
            });
        }
        if !routes.insert(model.id.as_str()) {
            return Err(CatalogError::InvalidCatalog {
                provider: provider.to_owned(),
                message: format!("duplicate model id `{}`", model.id),
            });
        }
        if model
            .lifecycle
            .replacement_ids
            .iter()
            .any(|replacement| replacement.trim().is_empty())
        {
            return Err(CatalogError::InvalidCatalog {
                provider: provider.to_owned(),
                message: format!("model `{}` has a blank replacement id", model.id),
            });
        }
    }
    for model in models.iter() {
        for alias in &model.aliases {
            if alias.trim().is_empty() {
                return Err(CatalogError::InvalidCatalog {
                    provider: provider.to_owned(),
                    message: format!("model `{}` has a blank alias", model.id),
                });
            }
            if !routes.insert(alias.as_str()) {
                return Err(CatalogError::InvalidCatalog {
                    provider: provider.to_owned(),
                    message: format!("duplicate model id or alias `{alias}`"),
                });
            }
        }
    }
    models.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(())
}

fn validate_persisted_generations(
    generations: Vec<CatalogSnapshot>,
) -> Result<BTreeMap<String, Arc<CatalogSnapshot>>, CatalogError> {
    let mut validated = BTreeMap::new();
    for mut snapshot in generations {
        let provider = snapshot.provider.id.trim().to_owned();
        if provider.is_empty() {
            return Err(CatalogError::Persistence {
                message: "persisted provider id must not be blank".to_owned(),
            });
        }
        if snapshot.revision == 0 {
            return Err(CatalogError::Persistence {
                message: format!("persisted catalog `{provider}` has revision zero"),
            });
        }
        if snapshot.fetched_at_ms == 0 {
            return Err(CatalogError::Persistence {
                message: format!("persisted catalog `{provider}` has no commit timestamp"),
            });
        }
        validate_models(&provider, &mut snapshot.models)?;
        if validated
            .insert(provider.clone(), Arc::new(snapshot))
            .is_some()
        {
            return Err(CatalogError::Persistence {
                message: format!("persisted provider `{provider}` appears more than once"),
            });
        }
    }
    Ok(validated)
}

fn seed_entries(
    inner: &CatalogRegistryInner,
    generations: &BTreeMap<String, Arc<CatalogSnapshot>>,
) -> Result<(), CatalogError> {
    let entries = inner
        .entries
        .lock()
        .map_err(|_| CatalogError::RegistryUnavailable)?
        .clone();
    for (provider, entry) in entries {
        let Some(snapshot) = generations.get(&provider) else {
            continue;
        };
        let next_revision =
            snapshot
                .revision
                .checked_add(1)
                .ok_or_else(|| CatalogError::InvalidCatalog {
                    provider: provider.clone(),
                    message: "catalog revision is exhausted".to_owned(),
                })?;
        let mut state = entry
            .state
            .lock()
            .map_err(|_| CatalogError::RegistryUnavailable)?;
        let replace = state
            .cached
            .as_ref()
            .is_none_or(|current| generation_is_newer(snapshot, current));
        if replace {
            state.cached = Some(snapshot.clone());
            state.next_revision = state.next_revision.max(next_revision);
        }
    }
    Ok(())
}

fn generation_is_newer(candidate: &CatalogSnapshot, current: &CatalogSnapshot) -> bool {
    (candidate.fetched_at_ms, candidate.revision) > (current.fetched_at_ms, current.revision)
}

fn snapshot_is_fresh(snapshot: &CatalogSnapshot, ttl: Duration) -> bool {
    let age_ms = unix_time_ms().saturating_sub(snapshot.fetched_at_ms);
    u128::from(age_ms) < ttl.as_millis()
}

fn persistence_error(error: CatalogPersistenceError) -> CatalogError {
    CatalogError::Persistence {
        message: error.message().to_owned(),
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

struct CatalogRegistration {
    inner: Weak<CatalogRegistryInner>,
    provider: String,
    token: Arc<()>,
    active: bool,
}

impl CatalogRegistration {
    fn remove(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let entry = inner.entries.lock().ok().and_then(|mut entries| {
            let matches = entries
                .get(&self.provider)
                .is_some_and(|entry| Arc::ptr_eq(&entry.registration, &self.token));
            matches.then(|| entries.remove(&self.provider)).flatten()
        });
        if let Some(entry) = entry {
            entry.stop();
        }
    }
}

impl Drop for CatalogRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}

struct PersistenceRegistration {
    inner: Weak<CatalogRegistryInner>,
    token: Arc<()>,
    active: bool,
}

impl PersistenceRegistration {
    fn remove(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let removed = inner.persistence.lock().ok().and_then(|mut persistence| {
            let matches = persistence
                .as_ref()
                .is_some_and(|entry| Arc::ptr_eq(&entry.registration, &self.token));
            matches.then(|| persistence.take()).flatten()
        });
        if let Some(entry) = removed {
            entry.cancellation.cancel();
        }
    }
}

impl Drop for PersistenceRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Publish the token-counter registry with its always-available local
/// estimator.
///
/// The heuristic is registered here rather than being a hidden fallback inside
/// the registry, so `list()` shows exactly what can answer a count and a
/// provider-exact counter simply outranks it.
#[must_use]
pub fn token_counters_plugin() -> Box<dyn Plugin> {
    use crate::token_count::{HeuristicTokenEstimator, TokenCounterRegistry};

    struct TokenCountersPlugin;

    impl Plugin for TokenCountersPlugin {
        fn name(&self) -> &'static str {
            "token-counters"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "token-counters",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Provider,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::TokenCounter,
                HeuristicTokenEstimator::ID,
            )]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_TOKEN_COUNTERS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            context.provide(
                crate::SERVICE_TOKEN_COUNTERS,
                self.name(),
                TokenCounterRegistry::default(),
            )?;
            let registry = context
                .get::<TokenCounterRegistry>(crate::SERVICE_TOKEN_COUNTERS)
                .ok_or_else(|| CoreError::other("token counter registry missing"))?;
            registry
                .register(context, std::sync::Arc::new(HeuristicTokenEstimator::new()))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(TokenCountersPlugin)
}

/// Compose the base `"models"` service plugin with the requested cache TTL.
#[must_use]
pub fn model_catalog_plugin(ttl: Duration) -> Box<dyn Plugin> {
    struct ModelCatalogPlugin {
        ttl: Duration,
    }

    impl Plugin for ModelCatalogPlugin {
        fn name(&self) -> &'static str {
            "models"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "models",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MODELS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let registry = CatalogRegistry::new(self.ttl);
            let shutdown = registry.clone();
            context.effect(move || shutdown.stop());
            context.provide(SERVICE_MODELS, self.name(), registry)
        }
    }

    Box::new(ModelCatalogPlugin { ttl })
}

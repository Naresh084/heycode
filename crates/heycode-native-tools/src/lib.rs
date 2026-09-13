//! Logical native-tool registry and deterministic route selection.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use heycode_core::{
    Context, CoreError, CoreResult, NativeToolImplementationKind, NativeToolRoute, Plugin,
    PluginContributionKind, PluginDescriptor,
};

mod policy;

pub use policy::{
    NativeToolPolicy, NativeToolPolicyError, NativeToolPolicyMode, native_tool_policy_namespace,
    native_tool_policy_plugin,
};

/// Logical native-tool registry service.
pub const SERVICE_NATIVE_TOOLS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("native-tools");

/// One registered implementation candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeToolImplementation {
    route: NativeToolRoute,
    priority: i16,
    models: Option<Vec<String>>,
}

impl NativeToolImplementation {
    /// Construct one validated implementation candidate.
    ///
    /// # Errors
    /// Invalid logical/implementation/provider identity fails.
    pub fn new(
        logical: impl Into<String>,
        implementation: impl Into<String>,
        kind: NativeToolImplementationKind,
        provider: Option<String>,
        priority: i16,
    ) -> Result<Self, NativeToolRegistryError> {
        Ok(Self {
            route: NativeToolRoute::new(logical, implementation, kind, provider)
                .map_err(|error| NativeToolRegistryError::Invalid(error.to_string()))?,
            priority,
            models: None,
        })
    }

    /// Restrict a provider-hosted implementation to the exact configured models.
    /// Local implementations remain portable. Unlisted models fall back under
    /// prefer policies; native-only still refuses rather than changing policy.
    ///
    /// # Errors
    /// Empty, duplicate, malformed model ids or a local candidate are refused.
    pub fn with_models(mut self, models: Vec<String>) -> Result<Self, NativeToolRegistryError> {
        let unique = models.iter().collect::<std::collections::BTreeSet<_>>();
        if self.route.kind() != NativeToolImplementationKind::Provider
            || models.is_empty()
            || unique.len() != models.len()
            || models.iter().any(|id| {
                id.is_empty()
                    || id.len() > 256
                    || id.trim() != id
                    || id.chars().any(char::is_control)
            })
        {
            return Err(NativeToolRegistryError::Invalid(
                "invalid native tool model scope".into(),
            ));
        }
        self.models = Some(models);
        Ok(self)
    }

    /// Stable implementation id.
    #[must_use]
    pub fn id(&self) -> &str {
        self.route.implementation()
    }

    /// Registered route, before provider/model policy selection.
    #[must_use]
    pub fn route(&self) -> &NativeToolRoute {
        &self.route
    }

    /// Exact model restriction, when configured.
    #[must_use]
    pub fn models(&self) -> Option<&[String]> {
        self.models.as_deref()
    }
}

struct Entry {
    implementation: NativeToolImplementation,
    token: Arc<()>,
}

struct RegistryInner {
    entries: Mutex<Vec<Entry>>,
    policy: RwLock<NativeToolPolicy>,
    policy_unavailable: AtomicBool,
}

impl Default for RegistryInner {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            policy: RwLock::new(NativeToolPolicy::default()),
            policy_unavailable: AtomicBool::new(false),
        }
    }
}

/// Effect-owned native-tool implementation registry.
#[derive(Clone, Default)]
pub struct NativeToolRegistry {
    inner: Arc<RegistryInner>,
}

impl NativeToolRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// All live implementation candidates, including currently incompatible routes.
    ///
    /// # Errors
    /// Returns an error if the registry lock is poisoned.
    pub fn implementations(
        &self,
    ) -> Result<Vec<NativeToolImplementation>, NativeToolRegistryError> {
        Ok(self
            .inner
            .entries
            .lock()
            .map_err(|_| NativeToolRegistryError::Unavailable)?
            .iter()
            .map(|entry| entry.implementation.clone())
            .collect())
    }

    /// Register one unique implementation as a Context effect.
    ///
    /// # Errors
    /// Duplicate ids or poisoned state fail before publication.
    pub fn register(
        &self,
        context: &Context,
        implementation: NativeToolImplementation,
    ) -> Result<(), NativeToolRegistryError> {
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| NativeToolRegistryError::Unavailable)?;
        if entries
            .iter()
            .any(|entry| entry.implementation.id() == implementation.id())
        {
            return Err(NativeToolRegistryError::Duplicate(
                implementation.id().to_owned(),
            ));
        }
        let id = implementation.id().to_owned();
        let token = Arc::new(());
        entries.push(Entry {
            implementation,
            token: token.clone(),
        });
        drop(entries);
        let registration = Registration {
            inner: Arc::downgrade(&self.inner),
            id,
            token,
            active: true,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Resolve one implementation per logical capability for the provider.
    ///
    /// Matching provider-native candidates win; otherwise client, then MCP.
    /// Ties use descending priority then implementation id.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn resolve(&self, provider: &str) -> Result<Vec<NativeToolRoute>, NativeToolRegistryError> {
        if self.inner.policy_unavailable.load(Ordering::Acquire) {
            return Err(NativeToolRegistryError::Unavailable);
        }
        let policy = self
            .inner
            .policy
            .read()
            .map_err(|_| NativeToolRegistryError::Unavailable)?
            .clone();
        self.resolve_with_policy(provider, &policy)
    }

    /// Resolve against both provider identity and the selected model.
    ///
    /// # Errors
    /// Invalid policy or unavailable required implementation fails locally.
    pub fn resolve_for_model(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Vec<NativeToolRoute>, NativeToolRegistryError> {
        if self.inner.policy_unavailable.load(Ordering::Acquire) {
            return Err(NativeToolRegistryError::Unavailable);
        }
        let policy = self
            .inner
            .policy
            .read()
            .map_err(|_| NativeToolRegistryError::Unavailable)?
            .clone();
        self.resolve_scoped(provider, Some(model), &policy)
    }

    /// Resolve using one explicit immutable policy snapshot.
    ///
    /// # Errors
    /// Unknown override logical ids, unavailable `only` choices or poisoned
    /// registry state fail with stable actionable errors.
    pub fn resolve_with_policy(
        &self,
        provider: &str,
        policy: &NativeToolPolicy,
    ) -> Result<Vec<NativeToolRoute>, NativeToolRegistryError> {
        self.resolve_scoped(provider, None, policy)
    }

    fn resolve_scoped(
        &self,
        provider: &str,
        model: Option<&str>,
        policy: &NativeToolPolicy,
    ) -> Result<Vec<NativeToolRoute>, NativeToolRegistryError> {
        let entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| NativeToolRegistryError::Unavailable)?;
        let mut grouped: BTreeMap<&str, Vec<&NativeToolImplementation>> = BTreeMap::new();
        for entry in entries.iter() {
            grouped
                .entry(entry.implementation.route.logical())
                .or_default()
                .push(&entry.implementation);
        }
        if let Some(logical) = policy
            .overrides()
            .keys()
            .find(|logical| !grouped.contains_key(logical.as_str()))
        {
            return Err(NativeToolRegistryError::UnknownPolicyLogical(
                logical.clone(),
            ));
        }
        let mut routes = Vec::with_capacity(grouped.len());
        for (logical, mut candidates) in grouped {
            let mode = policy.mode_for(logical);
            candidates.retain(|candidate| {
                model.is_none_or(|model| {
                    candidate
                        .models
                        .as_ref()
                        .is_none_or(|models| models.iter().any(|id| id == model))
                })
            });
            candidates.sort_by(|left, right| {
                candidate_rank(right, provider, mode)
                    .cmp(&candidate_rank(left, provider, mode))
                    .then_with(|| left.id().cmp(right.id()))
            });
            match candidates
                .into_iter()
                .find(|candidate| candidate_rank(candidate, provider, mode).0 > 0)
            {
                Some(candidate) => routes.push(candidate.route.clone()),
                None if mode.is_only() => {
                    return Err(NativeToolRegistryError::UnsupportedPolicy {
                        logical: logical.to_owned(),
                        mode,
                    });
                }
                None => {}
            }
        }
        Ok(routes)
    }

    pub(crate) fn replace_policy(
        &self,
        policy: NativeToolPolicy,
    ) -> Result<(), NativeToolRegistryError> {
        let mut current = self
            .inner
            .policy
            .write()
            .map_err(|_| NativeToolRegistryError::Unavailable)?;
        *current = policy;
        self.inner
            .policy_unavailable
            .store(false, Ordering::Release);
        Ok(())
    }

    pub(crate) fn mark_policy_unavailable(&self) {
        self.inner.policy_unavailable.store(true, Ordering::Release);
    }

    pub(crate) fn reset_policy(&self) {
        match self.inner.policy.write() {
            Ok(mut policy) => {
                *policy = NativeToolPolicy::default();
                self.inner
                    .policy_unavailable
                    .store(false, Ordering::Release);
            }
            Err(_) => self.mark_policy_unavailable(),
        }
    }
}

fn candidate_rank(
    candidate: &NativeToolImplementation,
    provider: &str,
    mode: NativeToolPolicyMode,
) -> (u8, i16) {
    let matching_provider = candidate.route.kind() == NativeToolImplementationKind::Provider
        && candidate.route.provider() == Some(provider);
    let family = match (mode, candidate.route.kind(), matching_provider) {
        (NativeToolPolicyMode::PreferNative | NativeToolPolicyMode::NativeOnly, _, true) => 3,
        (NativeToolPolicyMode::PreferNative, NativeToolImplementationKind::Client, false) => 2,
        (NativeToolPolicyMode::PreferNative, NativeToolImplementationKind::Mcp, false) => 1,
        (
            NativeToolPolicyMode::PreferLocal | NativeToolPolicyMode::LocalOnly,
            NativeToolImplementationKind::Client,
            false,
        ) => 3,
        (
            NativeToolPolicyMode::PreferLocal | NativeToolPolicyMode::LocalOnly,
            NativeToolImplementationKind::Mcp,
            false,
        ) => 2,
        (NativeToolPolicyMode::PreferLocal, _, true) => 1,
        _ => 0,
    };
    (family, candidate.priority)
}

struct Registration {
    inner: Weak<RegistryInner>,
    id: String,
    token: Arc<()>,
    active: bool,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut entries) = inner.entries.lock() else {
            return;
        };
        entries.retain(|entry| {
            entry.implementation.id() != self.id || !Arc::ptr_eq(&entry.token, &self.token)
        });
    }
}

/// Native-tool registry failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeToolRegistryError {
    /// Candidate validation failed.
    #[error("invalid native tool implementation: {0}")]
    Invalid(String),
    /// Exact implementation id already exists.
    #[error("duplicate native tool implementation `{0}`")]
    Duplicate(String),
    /// Registry state is unavailable after a panic.
    #[error("native tool registry is unavailable")]
    Unavailable,
    /// Policy names a logical capability with no registered implementation.
    #[error("native tool policy names unavailable logical capability `{0}`")]
    UnknownPolicyLogical(String),
    /// An `only` policy has no eligible implementation for the active route.
    #[error("native tool `{logical}` has no implementation for policy `{mode}`")]
    UnsupportedPolicy {
        /// Logical capability with no eligible implementation.
        logical: String,
        /// Exact only-mode that refused fallback.
        mode: NativeToolPolicyMode,
    },
}

/// Publish the empty native-tool registry service.
#[must_use]
pub fn native_tools_plugin() -> Box<dyn Plugin> {
    struct NativeToolsPlugin;

    impl Plugin for NativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-tools"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_NATIVE_TOOLS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context
                .provide(SERVICE_NATIVE_TOOLS, self.name(), NativeToolRegistry::new())
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(NativeToolsPlugin)
}

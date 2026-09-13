//! Provider registry and the `"llm"` composition plugin.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use heycode_core::{Context, CoreError, Plugin};

use crate::provider::Provider;
use crate::{
    ProviderDescriptor, ProviderInterception, SERVICE_LLM, SERVICE_PROVIDER_INTERCEPTION,
    SERVICE_PROVIDERS, provider_interception_inventory,
};

/// Safe provider identity/default projected from one registered implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    /// Registry key returned by `ProviderInfo`.
    pub registry_name: String,
    /// Provider capability/protocol identity.
    pub descriptor: ProviderDescriptor,
    /// Provider-owned model default.
    pub default_model: String,
    /// Optional non-secret credential reference for setup/authorization.
    pub credential_reference: Option<String>,
}

/// Name-indexed set of live providers. Duplicate registration is rejected so
/// routing names stay unambiguous.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: Arc<Mutex<HashMap<String, ProviderEntry>>>,
    pub(crate) activation: crate::activation::ActivationSlot,
}

struct ProviderEntry {
    provider: Arc<dyn Provider>,
    token: Arc<()>,
}

/// Exact ownership handle for one late provider contribution.
pub struct ProviderRegistration {
    providers: Weak<Mutex<HashMap<String, ProviderEntry>>>,
    name: String,
    token: Arc<()>,
    active: bool,
}

impl ProviderRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount `provider` under its [`crate::ProviderInfo::name`].
    ///
    /// # Errors
    /// Returns the contested name when a provider already holds it.
    pub fn register(&self, provider: Arc<dyn Provider>) -> Result<(), String> {
        let name = provider.info().name;
        let mut providers = self
            .providers
            .lock()
            .map_err(|_| "provider registry unavailable".to_owned())?;
        if providers.contains_key(&name) {
            return Err(name);
        }
        providers.insert(
            name,
            ProviderEntry {
                provider,
                token: Arc::new(()),
            },
        );
        Ok(())
    }

    /// Register one late provider and return its exact disposer.
    ///
    /// # Errors
    /// Duplicate names or poisoned state fail before publication.
    pub fn register_owned(
        &self,
        provider: Arc<dyn Provider>,
    ) -> Result<ProviderRegistration, String> {
        let name = provider.info().name;
        let token = Arc::new(());
        let mut providers = self
            .providers
            .lock()
            .map_err(|_| "provider registry unavailable".to_owned())?;
        if providers.contains_key(&name) {
            return Err(name);
        }
        providers.insert(
            name.clone(),
            ProviderEntry {
                provider,
                token: token.clone(),
            },
        );
        Ok(ProviderRegistration {
            providers: Arc::downgrade(&self.providers),
            name,
            token,
            active: true,
        })
    }

    /// The provider mounted under `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers
            .lock()
            .ok()?
            .get(name)
            .map(|entry| entry.provider.clone())
    }

    /// All mounted provider names, sorted for deterministic diagnostics.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .lock()
            .map(|providers| providers.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Provider profiles in stable provider-id order for setup and picker consumers.
    #[must_use]
    pub fn profiles(&self) -> Vec<ProviderProfile> {
        let mut profiles: Vec<_> = self
            .providers
            .lock()
            .map(|providers| {
                providers
                    .values()
                    .map(|entry| {
                        let provider = &entry.provider;
                        let info = provider.info();
                        ProviderProfile {
                            registry_name: info.name,
                            descriptor: provider.descriptor(),
                            default_model: info.default_model,
                            credential_reference: provider
                                .credential_reference()
                                .map(str::to_owned),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        profiles.sort_by(|left, right| left.descriptor.id.cmp(&right.descriptor.id));
        profiles
    }
}

impl Drop for ProviderRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(providers) = self.providers.upgrade() else {
            return;
        };
        let Ok(mut providers) = providers.lock() else {
            return;
        };
        let remove = providers
            .get(&self.name)
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token));
        if remove {
            providers.remove(&self.name);
        }
    }
}

/// Active routing decision resolved from config: which provider runs the
/// session and with which model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmSelection {
    /// Provider name looked up in the [`ProviderRegistry`].
    pub provider_name: String,
    /// Model id passed through in every [`crate::ChatRequest`].
    pub model: String,
}

/// Compose the `"llm"` plugin: publishes `"providers"` (a
/// [`ProviderRegistry`] filled with `providers`) and `"llm"` (the active
/// [`LlmSelection`]).
#[must_use]
pub fn llm_plugin(selection: LlmSelection, providers: Vec<Arc<dyn Provider>>) -> Box<dyn Plugin> {
    struct LlmPlugin {
        selection: LlmSelection,
        providers: Vec<Arc<dyn Provider>>,
    }

    impl Plugin for LlmPlugin {
        fn name(&self) -> &'static str {
            "llm"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "llm",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Provider,
                    heycode_core::PluginContributionKind::Waterfall,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            let mut rows = self
                .providers
                .iter()
                .map(|provider| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::InferenceProvider,
                        provider.info().name,
                    )
                })
                .collect::<Vec<_>>();
            rows.extend(provider_interception_inventory());
            rows
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                SERVICE_PROVIDERS,
                SERVICE_LLM,
                SERVICE_PROVIDER_INTERCEPTION,
            ]
        }

        fn apply(&self, ctx: &mut Context) -> heycode_core::CoreResult<()> {
            let registry = ProviderRegistry::new();
            for provider in &self.providers {
                // Providers mount as components of this plugin family; a name
                // clash among them is a duplicate-component load failure.
                registry
                    .register(Arc::clone(provider))
                    .map_err(CoreError::DuplicatePlugin)?;
            }
            ctx.provide(SERVICE_PROVIDERS, "llm", registry)?;
            ctx.provide(SERVICE_LLM, "llm", self.selection.clone())?;
            ctx.provide(
                SERVICE_PROVIDER_INTERCEPTION,
                "llm",
                ProviderInterception::default(),
            )?;
            Ok(())
        }
    }

    Box::new(LlmPlugin {
        selection,
        providers,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provider::ProviderInfo;
    use crate::testing::FakeProvider;

    fn provider_named(name: &'static str) -> Arc<dyn Provider> {
        Arc::new(FakeProvider::named(name, "m", Vec::new()))
    }

    #[test]
    fn register_rejects_duplicates_naming_the_offender() {
        let registry = ProviderRegistry::new();
        registry.register(provider_named("dup")).unwrap();
        assert_eq!(registry.register(provider_named("dup")).unwrap_err(), "dup");
        assert_eq!(registry.names(), vec!["dup"]);
    }

    #[test]
    fn get_and_names_round_trip() {
        let registry = ProviderRegistry::new();
        for name in ["beta", "alpha"] {
            registry.register(provider_named(name)).unwrap();
        }
        assert_eq!(registry.names(), vec!["alpha", "beta"]);
        assert_eq!(registry.get("alpha").unwrap().info().name, "alpha");
        assert!(registry.get("missing").is_none());
        assert_eq!(
            registry
                .profiles()
                .iter()
                .map(|profile| {
                    (
                        profile.registry_name.as_str(),
                        profile.descriptor.id.as_str(),
                        profile.default_model.as_str(),
                        profile.credential_reference.as_deref(),
                    )
                })
                .collect::<Vec<_>>(),
            [("alpha", "alpha", "m", None), ("beta", "beta", "m", None)]
        );
    }

    #[test]
    fn llm_plugin_publishes_registry_and_selection() {
        let selection = LlmSelection {
            provider_name: "fake".into(),
            model: "m-1".into(),
        };
        let plugin = llm_plugin(selection, vec![provider_named("fake")]);
        let ctx = heycode_core::compose(std::slice::from_ref(&plugin)).unwrap();

        assert_eq!(ctx.owner_of(SERVICE_PROVIDERS), Some("llm"));
        assert_eq!(ctx.owner_of(SERVICE_LLM), Some("llm"));
        assert_eq!(ctx.owner_of(SERVICE_PROVIDER_INTERCEPTION), Some("llm"));
        let registry = ctx.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
        assert_eq!(registry.names(), vec!["fake"]);
        let resolved = ctx.get::<LlmSelection>(SERVICE_LLM).unwrap();
        assert_eq!(
            (resolved.provider_name.as_str(), resolved.model.as_str()),
            ("fake", "m-1")
        );
        assert!(
            ctx.get::<ProviderInterception>(SERVICE_PROVIDER_INTERCEPTION)
                .is_some()
        );
        let snapshot = ctx.plugin_inventory().snapshot().unwrap();
        for name in [crate::PROVIDER_REQUEST_SEAM, crate::PROVIDER_RESPONSE_SEAM] {
            assert!(snapshot.contributions.iter().any(|row| {
                row.plugin == "llm"
                    && row.kind == heycode_core::ContributionKind::InterceptionSeam
                    && row.name == name
            }));
        }
    }

    #[test]
    fn llm_plugin_fails_load_on_duplicate_provider_names() {
        let selection = LlmSelection {
            provider_name: "x".into(),
            model: "m".into(),
        };
        let plugin = llm_plugin(
            selection,
            vec![provider_named("same"), provider_named("same")],
        );
        let err = match heycode_core::compose(std::slice::from_ref(&plugin)) {
            Err(err) => err,
            Ok(_) => panic!("expected duplicate-provider load failure"),
        };
        assert!(matches!(
            err,
            CoreError::DuplicateContribution {
                ref kind,
                ref name,
                ref existing,
                ref claimant,
            } if kind == "inference_provider"
                && name == "same"
                && existing == "llm"
                && claimant == "llm"
        ));
    }

    #[test]
    fn info_type_projects_name_and_default_model() {
        let info = ProviderInfo {
            name: "p".to_owned(),
            default_model: "dm".into(),
        };
        assert_eq!(info.name, "p");
        assert_eq!(info.default_model, "dm");
    }

    #[test]
    fn late_provider_registration_disposes_only_its_exact_row() {
        let registry = ProviderRegistry::new();
        let registration = registry.register_owned(provider_named("late")).unwrap();
        assert!(registry.get("late").is_some());
        assert_eq!(
            registry
                .register_owned(provider_named("late"))
                .err()
                .unwrap(),
            "late"
        );
        drop(registration);
        assert!(registry.get("late").is_none());
        assert!(registry.register_owned(provider_named("late")).is_ok());
    }
}

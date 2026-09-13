//! File catalog persistence provider plugin.

use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_llm::{CatalogRegistry, SERVICE_MODELS};

use crate::{
    CatalogOverrides, CatalogOverridesConfig, FileCatalogConfig, FileCatalogPersistence,
    SERVICE_CATALOG_OVERRIDES,
};

/// Open and register versioned owner-only model-catalog persistence.
#[must_use]
pub fn file_catalog_persistence_plugin(config: FileCatalogConfig) -> Box<dyn Plugin> {
    struct FileCatalogPlugin(FileCatalogConfig);

    impl Plugin for FileCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-cache-file"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "catalog-cache-file",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::CatalogPersistence,
                "file",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MODELS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::other("models registry missing"))?;
            let persistence = FileCatalogPersistence::open(self.0.clone())
                .map_err(|error| CoreError::other(error.to_string()))?;
            registry
                .register_persistence(context, Arc::new(persistence))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(FileCatalogPlugin(config))
}

/// Load layered user catalog overrides and publish them as a context service.
///
/// Loading happens at composition: a malformed or unsafe override document
/// fails the world rather than starting with a user's stated intent silently
/// dropped.
#[must_use]
pub fn catalog_overrides_plugin(config: CatalogOverridesConfig) -> Box<dyn Plugin> {
    struct CatalogOverridesPlugin(CatalogOverridesConfig);

    impl Plugin for CatalogOverridesPlugin {
        fn name(&self) -> &'static str {
            "catalog-overrides"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "catalog-overrides",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_CATALOG_OVERRIDES]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let overrides = CatalogOverrides::load(&self.0)
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.provide(SERVICE_CATALOG_OVERRIDES, self.name(), overrides)
        }
    }

    Box::new(CatalogOverridesPlugin(config))
}

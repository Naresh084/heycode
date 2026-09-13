//! Settings service plugin.

use heycode_core::{Context, CoreResult, Plugin, PluginContributionKind, PluginDescriptor};

use crate::{SERVICE_SETTINGS, SettingsDocuments, SettingsService};

/// Mount an immutable layered settings service.
#[must_use]
pub fn settings_plugin(documents: SettingsDocuments) -> Box<dyn Plugin> {
    struct SettingsPlugin(SettingsDocuments);
    impl Plugin for SettingsPlugin {
        fn name(&self) -> &'static str {
            "settings"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "settings",
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Service,
                    PluginContributionKind::Provider,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsProvider,
                "memory",
            )]
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(
                SERVICE_SETTINGS,
                "settings",
                SettingsService::new(self.0.clone()),
            )
        }
    }
    Box::new(SettingsPlugin(documents))
}

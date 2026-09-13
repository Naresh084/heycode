//! UI registry service plugin.

use heycode_core::{Context, CoreResult, Plugin, PluginContributionKind, PluginDescriptor};

use crate::settings_ui::SettingsUiRegistry;
use crate::{SERVICE_SETTINGS_UI, SERVICE_UI, UiRegistry};

/// Provide service `"ui"`.
#[must_use]
pub fn ui_registry_plugin() -> Box<dyn Plugin> {
    struct UiRegistryPlugin;
    impl Plugin for UiRegistryPlugin {
        fn name(&self) -> &'static str {
            "ui"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "ui",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_UI, SERVICE_SETTINGS_UI]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(SERVICE_UI, "ui", UiRegistry::new())?;
            context.provide(SERVICE_SETTINGS_UI, "ui", SettingsUiRegistry::new())
        }
    }
    Box::new(UiRegistryPlugin)
}

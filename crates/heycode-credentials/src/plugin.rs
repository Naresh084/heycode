//! Credentials service and non-secret settings plugin.

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_settings::{SERVICE_SETTINGS, SettingsService};

use crate::{CredentialsService, SERVICE_CREDENTIALS};

/// Mount the credential provider registry and reference settings namespace.
#[must_use]
pub fn credentials_plugin() -> Box<dyn Plugin> {
    struct CredentialsPlugin;
    impl Plugin for CredentialsPlugin {
        fn name(&self) -> &'static str {
            "credentials"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "credentials",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "credentials",
            )]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_CREDENTIALS]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = context
                .get::<SettingsService>(SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings missing"))?;
            context.provide(
                SERVICE_CREDENTIALS,
                "credentials",
                CredentialsService::new(),
            )?;
            settings
                .register(
                    context,
                    crate::settings::definition()
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            Ok(())
        }
    }
    Box::new(CredentialsPlugin)
}

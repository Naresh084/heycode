//! Authorization registry plugin.

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};

use crate::{AuthorizationService, SERVICE_AUTHORIZATION};

/// Mount an empty authorization flow registry.
#[must_use]
pub fn authorization_plugin() -> Box<dyn Plugin> {
    struct AuthorizationPlugin;
    impl Plugin for AuthorizationPlugin {
        fn name(&self) -> &'static str {
            "authorization"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "authorization",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_CREDENTIALS]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AUTHORIZATION]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::other("credentials missing"))?;
            context.provide(
                SERVICE_AUTHORIZATION,
                "authorization",
                AuthorizationService::new(credentials),
            )
        }
    }
    Box::new(AuthorizationPlugin)
}

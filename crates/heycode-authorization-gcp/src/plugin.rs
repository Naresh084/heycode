//! Plugin that mounts the Google Cloud authentication profile service.

use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_http::{HttpService, SERVICE_HTTP};

use crate::SERVICE_GCP_AUTH;
use crate::env::ProcessGcpEnvironment;
use crate::profile::GcpAuthService;

/// Mount [`GcpAuthService`] over the live process environment and the composed
/// HTTP transport.
#[must_use]
pub fn gcp_auth_plugin() -> Box<dyn Plugin> {
    struct GcpAuthPlugin;

    impl Plugin for GcpAuthPlugin {
        fn name(&self) -> &'static str {
            "authorization-gcp"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "authorization-gcp",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_HTTP]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_GCP_AUTH]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::other("http missing"))?;
            context.provide(
                SERVICE_GCP_AUTH,
                self.name(),
                GcpAuthService::new(Arc::new(ProcessGcpEnvironment), (*http).clone()),
            )
        }
    }

    Box::new(GcpAuthPlugin)
}

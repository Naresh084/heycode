//! Built-in reqwest HTTP transport plugin.

use std::sync::Arc;

use heycode_core::{Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor};

use crate::{HttpService, ReqwestHttpTransport, SERVICE_HTTP};

/// Publish the shared reqwest/rustls HTTP transport service.
#[must_use]
pub fn http_plugin() -> Box<dyn Plugin> {
    struct HttpPlugin;

    impl Plugin for HttpPlugin {
        fn name(&self) -> &'static str {
            "http-reqwest"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "http-reqwest",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_HTTP]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let transport =
                ReqwestHttpTransport::new().map_err(|error| CoreError::other(error.to_string()))?;
            context.provide(
                SERVICE_HTTP,
                self.name(),
                HttpService::new(Arc::new(transport)),
            )
        }
    }

    Box::new(HttpPlugin)
}

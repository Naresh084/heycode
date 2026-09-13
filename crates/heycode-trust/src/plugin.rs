//! Plugin composition for the workspace-trust service.

use heycode_core::{Context, CoreResult, Plugin};

use crate::WorkspaceTrustService;

/// Publish one prebuilt service, primarily for embedded/test compositions.
#[must_use]
pub fn trust_service_plugin(service: WorkspaceTrustService) -> Box<dyn Plugin> {
    struct TrustPlugin(WorkspaceTrustService);
    impl Plugin for TrustPlugin {
        fn name(&self) -> &'static str {
            "trust"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_TRUST]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            publish(context, self.0.clone())
        }
    }
    Box::new(TrustPlugin(service))
}

fn publish(context: &mut Context, service: WorkspaceTrustService) -> CoreResult<()> {
    let shutdown = service.clone();
    context.effect(move || shutdown.shutdown());
    context.provide(crate::SERVICE_TRUST, "trust", service)
}

//! Built-in local subprocess provider plugin.

use heycode_core::{Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor};

use crate::{SERVICE_SANDBOX, SERVICE_SUBPROCESS, SandboxService, SubprocessService};

/// Publish the local process-tree subprocess provider.
#[must_use]
pub fn local_subprocess_plugin() -> Box<dyn Plugin> {
    struct LocalSubprocessPlugin;

    impl Plugin for LocalSubprocessPlugin {
        fn name(&self) -> &'static str {
            "subprocess-local"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SUBPROCESS]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SANDBOX]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let sandbox = context
                .get::<SandboxService>(SERVICE_SANDBOX)
                .ok_or_else(|| CoreError::other("sandbox service type mismatch"))?;
            let backend = crate::local::backend((*sandbox).clone());
            let shutdown = backend.shutdown_token();
            context.effect(move || shutdown.cancel());
            context.provide(
                SERVICE_SUBPROCESS,
                self.name(),
                SubprocessService::new(backend),
            )
        }
    }

    Box::new(LocalSubprocessPlugin)
}

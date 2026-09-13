//! Base runtime registry plugin.

use heycode_core::{Context, CoreResult, Plugin};

/// Publish the empty `runtimes` registry. Concrete native/delegated runtime
/// implementations are separate provider plugins.
#[must_use]
pub fn runtime_registry_plugin() -> Box<dyn Plugin> {
    struct RuntimeRegistryPlugin;

    impl Plugin for RuntimeRegistryPlugin {
        fn name(&self) -> &'static str {
            "runtimes"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "runtimes",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_RUNTIMES]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(
                crate::SERVICE_RUNTIMES,
                "runtimes",
                crate::AgentRuntimeRegistry::new(),
            )
        }
    }

    Box::new(RuntimeRegistryPlugin)
}

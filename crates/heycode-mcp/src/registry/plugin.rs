//! Base MCP registry Service Provider.

use heycode_core::{Context, CoreResult, Plugin};

/// Publish the empty `mcp` registry. Definition and stdio/HTTP transport
/// owners are separate provider plugins that register context effects.
#[must_use]
pub fn mcp_registry_plugin() -> Box<dyn Plugin> {
    struct McpRegistryPlugin;

    impl Plugin for McpRegistryPlugin {
        fn name(&self) -> &'static str {
            "mcp-registry"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_MCP]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = crate::McpRegistry::new();
            context.provide(crate::SERVICE_MCP, self.name(), registry.clone())?;
            context.effect(move || registry.shutdown());
            Ok(())
        }
    }

    Box::new(McpRegistryPlugin)
}

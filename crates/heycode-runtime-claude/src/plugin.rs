//! Effect-owned Claude Code runtime Provider plugin.

use std::sync::Arc;

use heycode_core::{Context, CoreError, Plugin};
use heycode_runtime::{AgentRuntime, AgentRuntimeRegistry};

use crate::{CLAUDE_RUNTIME_ID, ClaudeRuntime, ClaudeRuntimeConfig};

/// Register delegated runtime `claude` over the composed subprocess service.
///
/// R07 publishes credential-blind account/version/process health. Session start,
/// resume, event streaming and permission callbacks remain explicitly
/// unsupported until R08/R09.
#[must_use]
pub fn claude_runtime_plugin(config: ClaudeRuntimeConfig) -> Box<dyn Plugin> {
    struct ClaudeRuntimePlugin(ClaudeRuntimeConfig);

    impl Plugin for ClaudeRuntimePlugin {
        fn name(&self) -> &'static str {
            "runtime-claude"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::AgentRuntime,
                CLAUDE_RUNTIME_ID,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_runtime::SERVICE_RUNTIMES,
                heycode_exec::SERVICE_SUBPROCESS,
            ]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let runtimes = context
                .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
                .ok_or_else(|| CoreError::other("runtimes service type mismatch"))?;
            let subprocess = context
                .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                .ok_or_else(|| CoreError::other("subprocess service type mismatch"))?;
            let mut config = self.0.clone();
            config.discovery_workspace = Some(
                heycode_runtime::RuntimeDiscoveryWorkspace::register(context)?,
            );
            let runtime = Arc::new(
                ClaudeRuntime::new((*subprocess).clone(), config)
                    .map_err(|error| CoreError::other(error.to_string()))?,
            );
            let registered: Arc<dyn AgentRuntime> = runtime.clone();
            runtimes
                .register(context, registered)
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.effect(move || runtime.force_close());
            Ok(())
        }
    }

    Box::new(ClaudeRuntimePlugin(config))
}

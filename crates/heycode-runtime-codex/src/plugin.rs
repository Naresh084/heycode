//! Effect-owned Codex delegated runtime registration.

use std::sync::Arc;

use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_exec::{SERVICE_SUBPROCESS, SubprocessService};
use heycode_runtime::{AgentRuntimeRegistry, SERVICE_RUNTIMES};
use tokio_util::sync::CancellationToken;

use crate::{CodexAppServerConfig, CodexRuntime};

/// Register delegated runtime `codex` over the common subprocess service.
///
/// R03/R04 contribute the real process/wire and account/model Provider while
/// AgentRuntime session operations remain explicitly unsupported.
#[must_use]
pub fn codex_runtime_plugin(config: CodexAppServerConfig) -> Box<dyn Plugin> {
    struct CodexRuntimePlugin(Arc<CodexAppServerConfig>);

    impl Plugin for CodexRuntimePlugin {
        fn name(&self) -> &'static str {
            "runtime-codex"
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
                "codex",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_RUNTIMES, SERVICE_SUBPROCESS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let runtimes = context
                .get::<AgentRuntimeRegistry>(SERVICE_RUNTIMES)
                .ok_or_else(|| CoreError::other("runtimes service type mismatch"))?;
            let subprocess = context
                .get::<SubprocessService>(SERVICE_SUBPROCESS)
                .ok_or_else(|| CoreError::other("subprocess service type mismatch"))?;
            let discovery = heycode_runtime::RuntimeDiscoveryWorkspace::register(context)?;
            let config = Arc::new(self.0.as_ref().clone().with_discovery_workspace(discovery));
            let shutdown = CancellationToken::new();
            let runtime =
                CodexRuntime::with_parts(subprocess.as_ref().clone(), config, shutdown.clone())
                    .map_err(|error| CoreError::other(error.to_string()))?;
            context.effect(move || shutdown.cancel());
            runtimes
                .register(context, Arc::new(runtime))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(CodexRuntimePlugin(Arc::new(config)))
}

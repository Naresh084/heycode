//! Effect-owned DeepSeek Harness SDK runtime registration.

use std::sync::Arc;

use heycode_core::{Context, CoreError, Plugin};
use heycode_exec::{ProcessErrorCode, SERVICE_SUBPROCESS, SubprocessService};
use heycode_runtime::{
    AgentRuntime, AgentRuntimeRegistry, SERVICE_RUNTIMES, UnavailableAgentRuntime,
};

use crate::DEEPSEEK_HARNESS_RUNTIME_ID;
use crate::config::DeepSeekHarnessRuntimeConfig;
use crate::runtime::{DeepSeekHarnessRuntime, deepseek_harness_runtime_descriptor};

/// Register the exact SDK v0.0.1 delegated runtime over `heycode-exec`.
#[must_use]
pub fn deepseek_harness_runtime_plugin(config: DeepSeekHarnessRuntimeConfig) -> Box<dyn Plugin> {
    struct DeepSeekHarnessRuntimePlugin(DeepSeekHarnessRuntimeConfig);

    impl Plugin for DeepSeekHarnessRuntimePlugin {
        fn name(&self) -> &'static str {
            "runtime-deepseek-harness"
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
                DEEPSEEK_HARNESS_RUNTIME_ID,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_RUNTIMES, SERVICE_SUBPROCESS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let runtimes = context
                .get::<AgentRuntimeRegistry>(SERVICE_RUNTIMES)
                .ok_or_else(|| CoreError::other("runtimes service type mismatch"))?;
            let subprocess = context
                .get::<SubprocessService>(SERVICE_SUBPROCESS)
                .ok_or_else(|| CoreError::other("subprocess service type mismatch"))?;
            let program = match subprocess.resolve_program(self.0.program()) {
                Ok(program) => program,
                Err(error) if error.code() == ProcessErrorCode::NotFound => {
                    let runtime = Arc::new(UnavailableAgentRuntime::new(
                        deepseek_harness_runtime_descriptor()
                            .map_err(|error| CoreError::other(error.to_string()))?,
                    ));
                    let registered: Arc<dyn AgentRuntime> = runtime.clone();
                    runtimes
                        .register(context, registered)
                        .map_err(|error| CoreError::other(error.to_string()))?;
                    context.effect(move || runtime.shutdown());
                    return Ok(());
                }
                Err(error) => return Err(CoreError::other(error.to_string())),
            };
            let runtime = Arc::new(
                DeepSeekHarnessRuntime::new(subprocess.as_ref().clone(), program, self.0.clone())
                    .map_err(|error| CoreError::other(error.to_string()))?,
            );
            let registered: Arc<dyn AgentRuntime> = runtime.clone();
            runtimes
                .register(context, registered)
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.effect(move || runtime.shutdown());
            Ok(())
        }
    }

    Box::new(DeepSeekHarnessRuntimePlugin(config))
}

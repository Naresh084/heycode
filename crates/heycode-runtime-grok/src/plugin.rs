//! Effect-owned Grok runtime registration.

use std::sync::Arc;

use heycode_core::{Context, CoreError, Plugin};
use heycode_exec::{ProcessErrorCode, SERVICE_SUBPROCESS, SubprocessService};
use heycode_runtime::{
    AccountState, AccountStatus, AcpRuntime, AcpRuntimeConfig, AgentRuntime, AgentRuntimeRegistry,
    SERVICE_RUNTIMES, UnavailableAgentRuntime,
};

use crate::config::GrokRuntimeConfig;
use crate::{GROK_RUNTIME_ID, grok_descriptor};
use heycode_runtime::ManagedAcpProcessFactory;

/// Register the exact-version Grok ACP provider over `heycode-exec`.
#[must_use]
pub fn grok_runtime_plugin(config: GrokRuntimeConfig) -> Box<dyn Plugin> {
    struct GrokRuntimePlugin(GrokRuntimeConfig);

    impl Plugin for GrokRuntimePlugin {
        fn name(&self) -> &'static str {
            "runtime-grok"
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
                GROK_RUNTIME_ID,
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
                        grok_descriptor().map_err(|error| CoreError::other(error.to_string()))?,
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
            let factory = Arc::new(
                ManagedAcpProcessFactory::new(
                    subprocess.as_ref().clone(),
                    program.clone(),
                    crate::PINNED_GROK_VERSION_OUTPUT,
                    arguments(),
                )
                .map_err(|error| CoreError::other(error.to_string()))?,
            );
            let discovery = heycode_runtime::RuntimeDiscoveryWorkspace::register(context)?;
            let config = AcpRuntimeConfig::new(
                grok_descriptor().map_err(|error| CoreError::other(error.to_string()))?,
                &program,
                arguments(),
                self.0.environment().to_vec(),
                self.0.catalog_workspace(),
                AccountState::without_label(AccountStatus::Unknown),
            )
            .and_then(|config| {
                config
                    .with_discovery_workspace(discovery)
                    .with_cached_authentication("cached_token")
            })
            .map_err(|error| CoreError::other(error.to_string()))?;
            let runtime = Arc::new(AcpRuntime::new(config, factory));
            let registered: Arc<dyn AgentRuntime> = runtime.clone();
            runtimes
                .register(context, registered)
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.effect(move || runtime.shutdown());
            Ok(())
        }
    }

    Box::new(GrokRuntimePlugin(config))
}

fn arguments() -> Vec<std::ffi::OsString> {
    ["--no-auto-update", "agent", "--no-leader", "stdio"]
        .into_iter()
        .map(Into::into)
        .collect()
}

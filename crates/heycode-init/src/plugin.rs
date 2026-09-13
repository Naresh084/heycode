//! `/init` command contribution.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandDescriptor, CommandRegistry, CommandSource, CommandTiming,
};
use heycode_core::{Context, CoreError, CoreResult, Plugin};

use crate::InitService;

struct InitCommand {
    descriptor: CommandDescriptor,
    service: Arc<InitService>,
}

#[async_trait]
impl Command for InitCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let text = self.service.execute(args)?;
        agent.ui().emit(heycode_agent::UiEvent::Info { text });
        Ok(())
    }
}

/// Build the workspace-instruction initializer plugin for one workspace root.
#[must_use]
pub fn init_plugin(root: PathBuf) -> Box<dyn Plugin> {
    struct InitPlugin(PathBuf);

    impl Plugin for InitPlugin {
        fn name(&self) -> &'static str {
            "init"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "init",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Command],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::Command,
                "init",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_agent::SERVICE_COMMANDS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands = context
                .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("commands missing"))?;
            let service = Arc::new(
                InitService::new(&self.0).map_err(|error| CoreError::other(error.to_string()))?,
            );
            let source = CommandSource::from_plugin("init")
                .map_err(|error| CoreError::other(error.to_string()))?;
            let descriptor = CommandDescriptor::new(
                "init",
                "Preview or apply managed AGENTS.md workspace guidance",
                vec![
                    CommandArgument::optional("action", "Use `preview` or `apply`")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    CommandArgument::optional("token", "Exact preview token required by apply")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                ],
                CommandTiming::Queued,
                source,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    context,
                    Arc::new(InitCommand {
                        descriptor,
                        service,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(InitPlugin(root))
}

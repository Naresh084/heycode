//! Command provider plugin registration.

use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};

use crate::CommandCredentialProvider;

/// Register one immutable command-backed provider into `credentials`.
#[must_use]
pub fn command_credentials_plugin(provider: CommandCredentialProvider) -> Box<dyn Plugin> {
    struct CommandCredentialsPlugin(CommandCredentialProvider);
    impl Plugin for CommandCredentialsPlugin {
        fn name(&self) -> &'static str {
            "credentials-command"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "credentials-command",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            if self.0.has_specs() {
                &[SERVICE_CREDENTIALS, heycode_exec::SERVICE_SUBPROCESS]
            } else {
                &[SERVICE_CREDENTIALS]
            }
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::CredentialProvider,
                "command",
            )]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::other("credentials missing"))?;
            let provider = if self.0.has_specs() {
                let subprocess = context
                    .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                    .ok_or_else(|| CoreError::other("subprocess missing"))?;
                self.0.clone().with_subprocess((*subprocess).clone())
            } else {
                self.0.clone()
            };
            credentials
                .register(context, Arc::new(provider))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }
    Box::new(CommandCredentialsPlugin(provider))
}

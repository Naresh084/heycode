//! Owner-only file credential provider plugin.

use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};

use crate::{FileCredentialConfig, FileCredentialProvider};

/// Migrate/open and register the precedence-20 file fallback.
#[must_use]
pub fn file_credentials_plugin(config: FileCredentialConfig) -> Box<dyn Plugin> {
    struct FileCredentialsPlugin(FileCredentialConfig);
    impl Plugin for FileCredentialsPlugin {
        fn name(&self) -> &'static str {
            "credentials-file"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "credentials-file",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::CredentialProvider,
                "file",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_CREDENTIALS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::other("credentials missing"))?;
            let provider = FileCredentialProvider::open(self.0.clone())
                .map_err(|error| CoreError::other(error.to_string()))?;
            credentials
                .register(context, Arc::new(provider))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }
    Box::new(FileCredentialsPlugin(config))
}

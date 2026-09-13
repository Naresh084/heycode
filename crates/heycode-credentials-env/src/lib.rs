//! Read-only highest-precedence process-environment credential provider.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::{
    CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialQuery,
    CredentialSecret, CredentialSource, CredentialsError, CredentialsService, SERVICE_CREDENTIALS,
};

type EnvironmentReader = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// Read-only provider over one environment reader.
#[derive(Clone)]
pub struct EnvironmentCredentialProvider {
    id: CredentialProviderId,
    read: EnvironmentReader,
}

impl EnvironmentCredentialProvider {
    /// Use the current process environment without mutating it.
    ///
    /// # Errors
    /// The built-in provider id is validated through the public id boundary.
    pub fn process() -> Result<Self, CredentialsError> {
        Ok(Self {
            id: CredentialProviderId::new("environment")?,
            read: Arc::new(|name| std::env::var_os(name)),
        })
    }

    /// Deterministic map-backed reader for tests and embedders.
    ///
    /// # Errors
    /// The built-in provider id is validated through the public id boundary.
    pub fn from_map(values: BTreeMap<String, String>) -> Result<Self, CredentialsError> {
        let values = Arc::new(values);
        Ok(Self {
            id: CredentialProviderId::new("environment")?,
            read: Arc::new(move |name| values.get(name).map(OsString::from)),
        })
    }

    fn value(&self, query: &CredentialQuery) -> Option<OsString> {
        (self.read)(query.reference.as_str())
            .filter(|value| !value.to_string_lossy().trim().is_empty())
    }
}

impl CredentialProvider for EnvironmentCredentialProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(match self.value(query) {
            Some(_) => CredentialProviderState::configured(CredentialSource::Environment, false),
            None => CredentialProviderState::unconfigured(false),
        })
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        self.value(query)
            .map(|value| {
                value
                    .into_string()
                    .map(CredentialSecret::new)
                    .map_err(|_| "environment value is not valid Unicode".to_owned())
            })
            .transpose()
    }
}

/// Register a process-environment provider into the credential service.
#[must_use]
pub fn environment_credentials_plugin(provider: EnvironmentCredentialProvider) -> Box<dyn Plugin> {
    struct EnvironmentPlugin(EnvironmentCredentialProvider);
    impl Plugin for EnvironmentPlugin {
        fn name(&self) -> &'static str {
            "credentials-env"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "credentials-env",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::CredentialProvider,
                self.0.id().as_str(),
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_CREDENTIALS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::other("credentials missing"))?;
            credentials
                .register(context, Arc::new(self.0.clone()))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }
    Box::new(EnvironmentPlugin(provider))
}

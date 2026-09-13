//! Composed AWS authentication status provider.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use heycode_authorization_api_key::ApiKeyValidator as _;
use heycode_credentials::{CredentialQuery, CredentialsService};
use heycode_http::HttpService;
use tokio_util::sync::CancellationToken;

use crate::validate::{api_key_status, container_role_status};
use crate::{
    AwsAuthReport, AwsBedrockApiKeyValidator, AwsChainDiscovery, AwsCredentialSource,
    AwsCredentialStatus, AwsHost, AwsProfileResolution, AwsRegionResolution, AwsUndetermined,
    chain,
};

struct Inner {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    host: Arc<dyn AwsHost>,
    api_key: CredentialQuery,
    region: Option<crate::AwsRegion>,
}

/// Effective AWS profile, region and credential-path status.
///
/// The service resolves the same facts an AWS SDK would read, then reports
/// them. It never returns an error: a diagnostic that can fail to produce a
/// diagnostic answers no question, so every failure is projected into the
/// status vocabulary instead.
#[derive(Clone)]
pub struct AwsAuthService {
    inner: Arc<Inner>,
}

impl AwsAuthService {
    /// Build over the composed HTTP/credential services and one host view.
    ///
    /// `api_key` is the non-secret credential reference the explicit Amazon
    /// Bedrock API key lives behind. It is the configured reference, not a
    /// provider-default alias, so presence and validation observe the same
    /// record a request would.
    #[must_use]
    pub fn new(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        host: Arc<dyn AwsHost>,
        api_key: CredentialQuery,
    ) -> Self {
        Self::new_with_region(http, credentials, host, api_key, None)
    }

    /// Build with an explicit connection region above host environment/profile defaults.
    #[must_use]
    pub fn new_with_region(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        host: Arc<dyn AwsHost>,
        api_key: CredentialQuery,
        region: Option<crate::AwsRegion>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                credentials,
                host,
                api_key,
                region,
            }),
        }
    }

    /// Effective profile.
    #[must_use]
    pub fn profile(&self) -> AwsProfileResolution {
        AwsProfileResolution::resolve(self.inner.host.as_ref())
    }

    /// Effective region.
    #[must_use]
    pub fn region(&self) -> AwsRegionResolution {
        match &self.inner.region {
            Some(region) => AwsRegionResolution::Resolved {
                region: region.clone(),
                origin: crate::AwsRegionOrigin::Connection,
            },
            None => AwsRegionResolution::resolve(self.inner.host.as_ref(), &self.profile()),
        }
    }

    /// Which credential source the SDK chain resolves to, without any network
    /// access.
    #[must_use]
    pub fn chain_discovery(&self) -> AwsChainDiscovery {
        chain::discover(self.inner.host.as_ref(), &self.profile())
    }

    /// Complete safe status of both credential paths.
    pub async fn report(&self, cancellation: CancellationToken) -> AwsAuthReport {
        let profile = self.profile();
        let region = self.region();
        let api_key = self.api_key_status(&region, cancellation.clone()).await;
        let chain = self.chain_status(&profile, cancellation).await;
        AwsAuthReport {
            profile,
            region,
            api_key,
            chain,
        }
    }

    async fn api_key_status(
        &self,
        region: &AwsRegionResolution,
        cancellation: CancellationToken,
    ) -> AwsCredentialStatus {
        let source = AwsCredentialSource::BedrockApiKey {
            reference: self.inner.api_key.reference.clone(),
        };
        let secret = match self.inner.credentials.resolve(&self.inner.api_key) {
            Ok(Some(secret)) => secret,
            Ok(None) => return AwsCredentialStatus::Absent,
            Err(_) => {
                return AwsCredentialStatus::Undetermined {
                    source: Some(source),
                    reason: AwsUndetermined::CredentialStoreUnavailable,
                };
            }
        };
        let Some(region) = region.region() else {
            return AwsCredentialStatus::Undetermined {
                source: Some(source),
                reason: AwsUndetermined::RegionUnresolved,
            };
        };
        let outcome = AwsBedrockApiKeyValidator::new(self.inner.http.clone(), Some(region))
            .validate(&secret, cancellation)
            .await;
        api_key_status(source, outcome, now_ms())
    }

    async fn chain_status(
        &self,
        profile: &AwsProfileResolution,
        cancellation: CancellationToken,
    ) -> AwsCredentialStatus {
        match chain::discover(self.inner.host.as_ref(), profile) {
            AwsChainDiscovery::Absent => AwsCredentialStatus::Absent,
            AwsChainDiscovery::Unusable { source, reason } => {
                AwsCredentialStatus::Rejected { source, reason }
            }
            AwsChainDiscovery::Blocked(reason) => AwsCredentialStatus::Undetermined {
                source: None,
                reason,
            },
            AwsChainDiscovery::Found(AwsCredentialSource::ContainerRole { variable }) => {
                container_role_status(
                    &self.inner.http,
                    self.inner.host.as_ref(),
                    variable,
                    cancellation,
                    now_ms(),
                )
                .await
            }
            // Every other chain source is proven only by a SigV4-signed
            // request. Reporting one of them as working because it exists
            // would be the exact promotion of Unknown this vocabulary refuses.
            AwsChainDiscovery::Found(source) => AwsCredentialStatus::Undetermined {
                source: Some(source),
                reason: AwsUndetermined::RequiresSignedRequest,
            },
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

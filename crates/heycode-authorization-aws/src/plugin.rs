//! Provider-owned AWS authorization contribution and status service.

use std::sync::Arc;

use heycode_authorization::{AuthorizationFlowId, AuthorizationService, SERVICE_AUTHORIZATION};
use heycode_authorization_api_key::{ApiKeyAuthorizationFlow, ApiKeyFlowConfig, SecretPrompt};
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpService, SERVICE_HTTP};

use crate::{
    AwsAuthService, AwsBedrockApiKeyValidator, AwsHost, SERVICE_AWS_AUTH,
    validate::AWS_BEARER_TOKEN_BEDROCK_VAR,
};

/// Stable Amazon Bedrock API-key authorization flow id.
pub const AWS_BEDROCK_FLOW_ID: &str = "aws-bedrock-api-key";

/// Provider-owned non-secret credential reference for the Bedrock API key.
///
/// It matches the environment variable the Bedrock service itself reads, so a
/// key already exported for the AWS SDKs is the same record heycode resolves.
pub const AWS_BEDROCK_API_KEY_REFERENCE: &str = AWS_BEARER_TOKEN_BEDROCK_VAR;

/// Replaceable collaborators for the AWS authorization plugin.
#[derive(Clone)]
pub struct AwsAuthPluginConfig {
    query: CredentialQuery,
    prompt: Arc<dyn SecretPrompt>,
    host: Arc<dyn AwsHost>,
    region: Option<crate::AwsRegion>,
}

impl AwsAuthPluginConfig {
    /// Use the persisted connection region for catalogs, validation and status.
    #[must_use]
    pub fn with_region(mut self, region: Option<crate::AwsRegion>) -> Self {
        self.region = region;
        self
    }

    /// Build from one exact credential query, the masked-entry broker and the
    /// host view AWS configuration is discovered through.
    #[must_use]
    pub fn new(
        query: CredentialQuery,
        prompt: Arc<dyn SecretPrompt>,
        host: Arc<dyn AwsHost>,
    ) -> Self {
        Self {
            query,
            prompt,
            host,
            region: None,
        }
    }
}

/// Register the AWS authorization flow and publish the AWS status service.
#[must_use]
pub fn aws_authorization_plugin(config: AwsAuthPluginConfig) -> Box<dyn Plugin> {
    struct AwsAuthorizationPlugin(AwsAuthPluginConfig);

    impl Plugin for AwsAuthorizationPlugin {
        fn name(&self) -> &'static str {
            "authorization-aws"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Provider,
                    PluginContributionKind::Service,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            // The `aws-auth` service row is contributed by `Context::provide`
            // itself; declaring it here as well would collide with it.
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::AuthorizationFlow,
                AWS_BEDROCK_FLOW_ID,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AUTHORIZATION, SERVICE_CREDENTIALS, SERVICE_HTTP]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AWS_AUTH]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let authorization = context
                .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
                .ok_or_else(|| CoreError::MissingService(SERVICE_AUTHORIZATION.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let service = AwsAuthService::new_with_region(
                http.as_ref().clone(),
                credentials,
                self.0.host.clone(),
                self.0.query.clone(),
                self.0.region.clone(),
            );
            let region = service.region();
            let flow = ApiKeyAuthorizationFlow::new(
                ApiKeyFlowConfig {
                    id: AuthorizationFlowId::new(AWS_BEDROCK_FLOW_ID)
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    label: "Amazon Bedrock API key".to_owned(),
                    query: self.0.query.clone(),
                    prompt: "Paste your Amazon Bedrock API key".to_owned(),
                },
                self.0.prompt.clone(),
                Arc::new(AwsBedrockApiKeyValidator::new(
                    http.as_ref().clone(),
                    region.region(),
                )),
            );
            authorization
                .register(context, Arc::new(flow))
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.provide(SERVICE_AWS_AUTH, self.name(), service)
        }
    }

    Box::new(AwsAuthorizationPlugin(config))
}

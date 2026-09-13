//! AWS authorization: explicit API key and SDK credential-chain profiles.
//!
//! PAWS01 owns the two ways a heycode world can hold AWS authority and the safe
//! report over both:
//!
//! - an explicit Amazon Bedrock API key, entered through the shared masked
//!   flow and proven against the regional Bedrock control plane;
//! - the AWS SDK credential chain, discovered from the documented environment
//!   variables and shared configuration files.
//!
//! Both report a *status*, and the status vocabulary keeps four outcomes
//! apart: nothing configured, discovered but unproven, proven good, proven
//! bad. `Undetermined` never widens into either verdict — "the check could not
//! reach AWS" is a different fact from "AWS said no", which is a different
//! fact again from "you have no credentials".
//!
//! Nothing here publishes credential material. Provenance is reported as
//! *names* — an environment variable name, a profile name, a heycode credential
//! reference — and no report type has a field for a secret, an account id, a
//! role ARN or an IAM Identity Center start URL.

mod chain;
mod error;
mod host;
mod ini;
mod plugin;
mod profile;
mod region;
mod service;
mod status;
mod validate;

pub use chain::{
    AWS_ACCESS_KEY_ID_VAR, AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
    AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR, AWS_ROLE_ARN_VAR, AWS_SECRET_ACCESS_KEY_VAR,
    AWS_WEB_IDENTITY_TOKEN_FILE_VAR, AwsChainDiscovery, discover as discover_credential_chain,
};
pub use error::AwsAuthError;
pub use host::{AwsFileRead, AwsHost, MapAwsHost, ProcessAwsHost};
pub use plugin::{
    AWS_BEDROCK_API_KEY_REFERENCE, AWS_BEDROCK_FLOW_ID, AwsAuthPluginConfig,
    aws_authorization_plugin,
};
pub use profile::{AWS_DEFAULT_PROFILE, AWS_PROFILE_VAR, AwsProfileName, AwsProfileResolution};
pub use region::{
    AWS_DEFAULT_REGION_VAR, AWS_REGION_VAR, AwsRegion, AwsRegionOrigin, AwsRegionResolution,
};
pub use service::AwsAuthService;
pub use status::{
    AwsAuthReport, AwsCredentialSource, AwsCredentialStatus, AwsRejection, AwsUndetermined,
};
pub use validate::{
    AWS_BEARER_TOKEN_BEDROCK_VAR, AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE_VAR,
    AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR, AwsBedrockApiKeyValidator, ECS_CREDENTIAL_HOST,
    api_key_status, container_role_status,
};

/// Effective AWS profile, region and credential-path status service.
pub const SERVICE_AWS_AUTH: heycode_core::ServiceKey = heycode_core::ServiceKey::new("aws-auth");

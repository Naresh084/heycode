//! Amazon Bedrock Mantle OpenAI-compatible model discovery.
//!
//! PAWS02 owns the `bedrock-mantle` endpoint's model list: one `GET
//! /v1/models` against the regional Mantle endpoint, normalized into one
//! all-or-nothing CAT02 generation.
//!
//! # Why this is a separate provider from PAWS03
//!
//! `bedrock-mantle` and the `bedrock` control plane are two different
//! surfaces, and merging them would state things that are not true. They host
//! overlapping but different model sets, capability policies and model-id
//! namespaces. Mantle names a plain foundation model such as
//! `openai.gpt-oss-120b`, while the Responses API on the runtime endpoint
//! requires a cross-Region inference profile such as `us.openai.gpt-5.6-sol`.
//! The runtime endpoint now exposes compatible Responses, Chat Completions and
//! Messages surfaces too; endpoint capability parity is still not implied.
//! Nothing here assumes an id from one catalog resolves on the other.
//!
//! # What "accessible" means here
//!
//! The list *is* the evidence. `/v1/models` answers for the credential that
//! authorized it and the region it was addressed to, so what it returns is
//! what this account can reach on this endpoint. Discovery therefore publishes
//! exactly the returned rows: it never unions them with a table of models AWS
//! documents elsewhere, because a model this account cannot call must not
//! appear as available.
//!
//! # What "normalized" can honestly mean here
//!
//! Very little, and deliberately so. AWS documents that on this endpoint
//! "only `model.id` is reliable ... other fields on `ModelInfo` may be empty"
//! (<https://docs.aws.amazon.com/bedrock/latest/userguide/models-get-info.html>).
//! So every row becomes [`ModelDescriptor::unknown`]: an exact id, and Unknown
//! for every capability, lifecycle, limit and price. The wire type below has
//! no field for anything else, so there is nothing to be tempted by.
//!
//! **Endpoint capabilities are not model capabilities.** AWS publishes a rich
//! capability list for `bedrock-mantle` — server-side tool use, web search,
//! asynchronous inference, prompt caching. Those describe the *endpoint*.
//! Attributing them to every model the endpoint lists would turn Unknown into
//! Supported for models that support none of them. Endpoint-level facts live
//! in [`mantle_provider_descriptor`]'s protocol list, which is a
//! `ProviderDescriptor`; model-level facts live in `ModelCapabilities`. They
//! are different types and nothing converts one into the other.
//!
//! # Authorization and signing
//!
//! No request signing happens here either. Discovery reuses PAWS03's
//! [`BedrockRequestAuthorizer`](crate::BedrockRequestAuthorizer) seam. AWS
//! documents that the Mantle endpoint accepts both a Bedrock API key and
//! SigV4, and the API key is a plain bearer token, so
//! [`BedrockApiKeyAuthorizer`](crate::BedrockApiKeyAuthorizer) authorizes this
//! endpoint with no signer at all.
//!
//! Endpoint reference:
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/endpoints.html>
//! Models API reference:
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html>

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_authorization_aws::{AwsAuthService, AwsRegion, SERVICE_AWS_AUTH};
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ProviderProtocol, ServiceKey,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpService, SERVICE_HTTP};
use heycode_llm::{
    CatalogFetchError, CatalogRegistry, ModelCatalog, ModelDescriptor, ProviderDescriptor,
    SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::BedrockApiKeyAuthorizer;
use crate::catalog::{
    BedrockRequestAuthorizer, classify_status, map_transport_error, require_json,
    unresolved_region_detail,
};
use crate::discovery::invalid;
use crate::model::BedrockModelId;

/// Stable Amazon Bedrock Mantle provider id.
///
/// Distinct from PAWS03's `bedrock`: `CatalogRegistry` keys by provider id and
/// these are two endpoints with different model sets and protocols.
pub const MANTLE_PROVIDER: &str = "bedrock-mantle";
/// Human display name for the Amazon Bedrock Mantle provider.
pub const MANTLE_DISPLAY_NAME: &str = "Amazon Bedrock (Mantle)";

/// Documented OpenAI-compatible model listing path.
///
/// The documented base URL is `https://bedrock-mantle.{region}.api.aws/v1`
/// and the listing is `GET {base}/models`.
///
/// <https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html>
const MANTLE_MODELS_PATH: &str = "/v1/models";

/// Cap on the discovery response body.
///
/// An OpenAI-shaped list of a few hundred id-only rows is a few tens of
/// kilobytes; this bounds the buffer well above the real size and far below
/// the transport default.
const CATALOG_RESPONSE_LIMIT: usize = 1024 * 1024;

/// Rows one generation may contain, matching the runtime catalog's bound.
const MAX_MODELS: usize = 2048;

/// Regional Amazon Bedrock Mantle origin.
///
/// AWS publishes the Mantle region list, but it is deliberately not encoded
/// here: a hardcoded allowlist would reject a region AWS adds later, which is
/// the failure mode a stale snapshot always produces. An unsupported region
/// resolves DNS or answers 404, and the classifier reports that honestly.
///
/// <https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html>
#[must_use]
pub(crate) fn mantle_origin(region: &AwsRegion) -> String {
    format!("https://bedrock-mantle.{}.api.aws", region.as_str())
}

fn mantle_url(region: &AwsRegion) -> String {
    format!("{}{MANTLE_MODELS_PATH}", mantle_origin(region))
}

/// Safe Amazon Bedrock Mantle provider identity.
///
/// The protocol list is an **endpoint** fact. AWS documents that this endpoint
/// serves the OpenAI Responses and Chat Completions APIs and the Anthropic
/// Messages API, and explicitly does not serve Converse or InvokeModel.
///
/// <https://docs.aws.amazon.com/bedrock/latest/userguide/endpoints.html>
#[must_use]
pub fn mantle_provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: MANTLE_PROVIDER.to_owned(),
        display_name: MANTLE_DISPLAY_NAME.to_owned(),
        protocols: vec![
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions,
            ProviderProtocol::AnthropicMessages,
        ],
    }
}

/// Provider-owned Amazon Bedrock Mantle model catalog source.
///
/// Deliberately has no `Debug` implementation: it owns the authorizer that
/// holds the account's credential query.
pub struct MantleCatalog {
    http: HttpService,
    authorizer: Arc<dyn BedrockRequestAuthorizer>,
    endpoint: String,
    evidence: MantleCatalogEvidence,
}

#[derive(Clone, Default)]
pub(crate) struct MantleCatalogEvidence {
    state: Arc<Mutex<MantleEvidenceState>>,
}

#[derive(Default)]
struct MantleEvidenceState {
    models: BTreeSet<String>,
    attempt: u64,
    consumed_attempt: u64,
    failure: Option<heycode_llm::CatalogFailureKind>,
}

pub(crate) enum MantleCatalogReadiness {
    Ready,
    Failed(heycode_llm::CatalogFailureKind),
    RefreshRequired,
}

impl MantleCatalogEvidence {
    fn record(
        &self,
        result: &Result<Vec<ModelDescriptor>, CatalogFetchError>,
    ) -> Result<(), CatalogFetchError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("Amazon Bedrock Mantle model evidence cache is unavailable"))?;
        state.attempt = state.attempt.saturating_add(1);
        match result {
            Ok(models) => {
                state.models = models.iter().map(|model| model.id.clone()).collect();
                state.failure = None;
            }
            Err(error) => state.failure = Some(error.kind()),
        }
        Ok(())
    }

    pub(crate) fn take_readiness(
        &self,
        model: &str,
    ) -> Result<MantleCatalogReadiness, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Amazon Bedrock Mantle model evidence cache is unavailable")?;
        if state.attempt == state.consumed_attempt {
            return Ok(MantleCatalogReadiness::RefreshRequired);
        }
        state.consumed_attempt = state.attempt;
        if let Some(kind) = state.failure {
            return Ok(MantleCatalogReadiness::Failed(kind));
        }
        Ok(if state.models.contains(model) {
            MantleCatalogReadiness::Ready
        } else {
            MantleCatalogReadiness::RefreshRequired
        })
    }
}

impl MantleCatalog {
    /// Build a source for one validated region's Mantle endpoint.
    #[must_use]
    pub fn new(
        http: HttpService,
        authorizer: Arc<dyn BedrockRequestAuthorizer>,
        region: &AwsRegion,
    ) -> Self {
        Self {
            http,
            authorizer,
            endpoint: mantle_url(region),
            evidence: MantleCatalogEvidence::default(),
        }
    }

    pub(crate) fn evidence(&self) -> MantleCatalogEvidence {
        self.evidence.clone()
    }

    /// The exact endpoint this source addresses.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl ModelCatalog for MantleCatalog {
    fn provider(&self) -> ProviderDescriptor {
        mantle_provider_descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let result = self.fetch_inner(cancellation).await;
        self.evidence.record(&result)?;
        result
    }
}

impl MantleCatalog {
    async fn fetch_inner(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let request = HttpRequest::get(&self.endpoint)
            .and_then(|request| request.header("accept", "application/json"))
            .map(|request| request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT))
            .map_err(|_| {
                invalid("Amazon Bedrock Mantle model list request could not be constructed")
            })?;
        let request = self
            .authorizer
            .authorize(request, cancellation.clone())
            .await?;
        let response = self
            .http
            .send(request, cancellation.clone())
            .await
            .map_err(map_transport_error)?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        classify_status(&response)?;
        if response.body.len() > CATALOG_RESPONSE_LIMIT {
            return Err(invalid(
                "Amazon Bedrock Mantle model list response is too large",
            ));
        }
        require_json(&response)?;
        normalize(&response.body)
    }
}

/// The OpenAI model-list envelope.
///
/// <https://developers.openai.com/api/reference/resources/models>
#[derive(Deserialize)]
struct ModelListResponse {
    #[serde(default)]
    data: Option<Vec<ModelEntry>>,
}

/// One listed model.
///
/// This type has exactly one field on purpose. AWS documents every other
/// `ModelInfo` field on this endpoint as possibly empty, so there is no
/// `created`, `owned_by` or `object` here to be mistaken for evidence.
#[derive(Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: Option<String>,
}

/// Normalize one complete response into an id-ordered generation.
///
/// All-or-nothing: one malformed row rejects the whole generation rather than
/// publishing a partial view of what the account can reach.
fn normalize(body: &[u8]) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
    let response: ModelListResponse = serde_json::from_slice(body)
        .map_err(|_| invalid("Amazon Bedrock Mantle model list has an invalid JSON shape"))?;
    let entries = response
        .data
        .ok_or_else(|| invalid("Amazon Bedrock Mantle model list response contains no data"))?;
    // An empty generation cannot be told apart from a broken response, and
    // publishing it would replace a good catalog with nothing.
    if entries.is_empty() {
        return Err(invalid("Amazon Bedrock Mantle model list is empty"));
    }
    if entries.len() > MAX_MODELS {
        return Err(invalid("Amazon Bedrock Mantle model list is too large"));
    }
    let mut rows: BTreeMap<String, ModelDescriptor> = BTreeMap::new();
    for entry in entries {
        let id = entry.id.and_then(BedrockModelId::new).ok_or_else(|| {
            invalid("Amazon Bedrock Mantle model list contains an invalid model id")
        })?;
        // Every field but the id is Unknown, because every field but the id is
        // documented as unreliable on this endpoint.
        let descriptor = ModelDescriptor::unknown(id.as_str());
        if rows.insert(id.as_str().to_owned(), descriptor).is_some() {
            return Err(invalid(
                "Amazon Bedrock Mantle model list contains duplicate model ids",
            ));
        }
    }
    Ok(rows.into_values().collect())
}

/// Configuration captured by the Mantle catalog contribution plugin.
///
/// Only the credential is configured. The region comes from PAWS01, exactly as
/// it does for the runtime catalog, so the two AWS catalogs cannot disagree
/// about which region the world is pointed at.
#[derive(Clone)]
pub struct MantleCatalogConfig {
    credential: CredentialQuery,
}

impl MantleCatalogConfig {
    /// Authorize discovery with the Amazon Bedrock API key behind `credential`.
    #[must_use]
    pub const fn api_key(credential: CredentialQuery) -> Self {
        Self { credential }
    }
}

/// Register Amazon Bedrock Mantle discovery into the shared catalog registry.
#[must_use]
pub fn mantle_catalog_plugin(config: MantleCatalogConfig) -> Box<dyn Plugin> {
    struct MantleCatalogPlugin(MantleCatalogConfig);

    impl Plugin for MantleCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-bedrock-mantle"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            vec![PluginContributionSpec::new(
                ContributionKind::ModelCatalog,
                MANTLE_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            &[
                SERVICE_MODELS,
                SERVICE_CREDENTIALS,
                SERVICE_HTTP,
                SERVICE_AWS_AUTH,
            ]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let aws = context
                .get::<AwsAuthService>(SERVICE_AWS_AUTH)
                .ok_or_else(|| CoreError::MissingService(SERVICE_AWS_AUTH.to_string()))?;
            let resolution = aws.region();
            let Some(region) = resolution.region() else {
                return Err(CoreError::other(format!(
                    "Amazon Bedrock Mantle catalog cannot address an endpoint: {}",
                    unresolved_region_detail(&resolution)
                )));
            };
            let authorizer = Arc::new(BedrockApiKeyAuthorizer::new(
                credentials,
                self.0.credential.clone(),
            ));
            let source = MantleCatalog::new(http.as_ref().clone(), authorizer, region);
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(MantleCatalogPlugin(config))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn every_validated_region_forms_the_documented_mantle_url() {
        for region in [
            "us-east-1",
            "eu-central-1",
            "ap-northeast-1",
            "us-gov-west-1",
        ] {
            let region = AwsRegion::new(region).unwrap();
            let url = mantle_url(&region);
            assert_eq!(
                url,
                format!(
                    "https://bedrock-mantle.{}.api.aws/v1/models",
                    region.as_str()
                )
            );
            assert!(HttpRequest::get(&url).is_ok(), "`{url}` must be usable");
        }
    }

    #[test]
    fn the_mantle_url_is_never_the_control_plane_or_runtime_host() {
        let region = AwsRegion::new("us-east-1").unwrap();
        let url = mantle_url(&region);
        assert!(!url.contains("amazonaws.com"), "{url}");
        assert!(url.starts_with("https://bedrock-mantle."), "{url}");
    }
}

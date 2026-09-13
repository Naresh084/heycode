//! Runtime Amazon Bedrock `ListFoundationModels` discovery.
//!
//! The source addresses the regional Bedrock **control plane** — a different
//! endpoint from the `bedrock-runtime` plane the Converse protocol adapter
//! uses — reads one complete non-paginated model list and publishes one
//! all-or-nothing generation.
//!
//! # Authorization and signing
//!
//! This row performs **no request signing**. [`BedrockCatalog`] builds an
//! unauthorized request shape and hands it to a [`BedrockRequestAuthorizer`],
//! which is the only place account authority is attached. The Bedrock API
//! model advertises two authentication schemes, `aws.auth#sigv4` and
//! `smithy.api#httpBearerAuth`; only the bearer scheme is implemented here, by
//! [`BedrockApiKeyAuthorizer`]. A SigV4 signer belongs to PAWS01 and plugs in
//! at exactly one point: an `impl BedrockRequestAuthorizer` whose `authorize`
//! canonicalizes the method, path, query, headers and empty payload of the
//! `HttpRequest` it is given and returns it carrying the `Authorization`,
//! `X-Amz-Date` and (for temporary credentials) `X-Amz-Security-Token`
//! headers. Nothing else in this crate has to change.
//!
//! Endpoint reference:
//! <https://docs.aws.amazon.com/general/latest/gr/bedrock.html>
//! Operation reference:
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_ListFoundationModels.html>

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_authorization_aws::{AwsAuthService, AwsRegion, AwsRegionResolution, SERVICE_AWS_AUTH};
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ProviderProtocol, ServiceKey,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CatalogError, CatalogFailureKind, CatalogFetchError, CatalogRegistry, LlmError, ModelCatalog,
    ModelDescriptor, ProviderDescriptor, ProviderErrorClass, ProviderFailure,
    ProviderFailureOrigin, SERVICE_MODELS,
};
use tokio_util::sync::CancellationToken;

use crate::discovery::{invalid, normalize};
use crate::model::BedrockFoundationModel;

/// Stable Amazon Bedrock provider id.
pub const BEDROCK_PROVIDER: &str = "bedrock";
/// Human display name for the Amazon Bedrock provider.
pub const BEDROCK_DISPLAY_NAME: &str = "Amazon Bedrock";

/// Documented `ListFoundationModels` request URI.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_ListFoundationModels.html>
const LIST_FOUNDATION_MODELS_PATH: &str = "/foundation-models";

/// Cap on the discovery response body.
///
/// A regional Bedrock model list is a few hundred kilobytes; this bounds the
/// buffer far below the transport default without being near the real size.
const CATALOG_RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

/// Regional Amazon Bedrock control-plane origin.
///
/// The commercial and GovCloud partitions both use this host shape. No default
/// region is ever substituted: PAWS01 resolves the effective region, and a
/// world with none configured fails loud rather than addressing somebody
/// else's account boundary.
///
/// <https://docs.aws.amazon.com/general/latest/gr/bedrock.html>
#[must_use]
fn control_plane_url(region: &AwsRegion) -> String {
    format!(
        "https://bedrock.{}.amazonaws.com{LIST_FOUNDATION_MODELS_PATH}",
        region.as_str()
    )
}

/// Attaches account authority to one outbound Bedrock control-plane request.
///
/// The catalog constructs the request and never learns how it was authorized,
/// so a credential value exists only inside an implementation of this trait
/// and inside the `HttpRequest` it returns — neither of which has a `Debug`
/// implementation that could render it.
#[async_trait]
pub trait BedrockRequestAuthorizer: Send + Sync {
    /// Return the request carrying whatever this account's authority requires.
    ///
    /// # Errors
    /// Returns [`CatalogFailureKind::Unauthorized`] when no usable credential
    /// exists and [`CatalogFailureKind::Unavailable`] when authority could not
    /// be established. Diagnostics must name neither the credential nor the
    /// response that produced them.
    async fn authorize(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> Result<HttpRequest, CatalogFetchError>;
}

/// Authorizes discovery with an Amazon Bedrock API key.
///
/// The key is a bearer token for both Bedrock planes, so the request carries
/// it in the standard `Authorization` header and requires no signing.
pub struct BedrockApiKeyAuthorizer {
    credentials: Arc<CredentialsService>,
    credential: CredentialQuery,
}

impl BedrockApiKeyAuthorizer {
    /// Authorize with the API key behind one exact credential query.
    #[must_use]
    pub const fn new(credentials: Arc<CredentialsService>, credential: CredentialQuery) -> Self {
        Self {
            credentials,
            credential,
        }
    }
}

#[async_trait]
impl BedrockRequestAuthorizer for BedrockApiKeyAuthorizer {
    async fn authorize(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> Result<HttpRequest, CatalogFetchError> {
        let secret = self
            .credentials
            .resolve(&self.credential)
            .map_err(|_| unavailable("Amazon Bedrock credential store is unavailable"))?
            .ok_or_else(|| unauthorized("Amazon Bedrock credential is not configured"))?;
        request
            .header("authorization", &format!("Bearer {}", secret.expose()))
            // A header that cannot be constructed means the stored key is not
            // a usable bearer token. The rejected value never enters the
            // diagnostic.
            .map_err(|_| unauthorized("Amazon Bedrock credential is not a usable bearer token"))
    }
}

/// Provider-owned Amazon Bedrock model catalog source.
///
/// Deliberately has no `Debug` implementation: it owns the authorizer that
/// holds the account's credential query.
pub struct BedrockCatalog {
    http: HttpService,
    authorizer: Arc<dyn BedrockRequestAuthorizer>,
    region: AwsRegionResolution,
    evidence: BedrockCatalogEvidence,
}

/// Last complete provider-owned rich discovery generation.
///
/// CAT02 can persist only [`ModelDescriptor`] values. This private companion
/// retains the streaming and inference-type facts required by Converse
/// activation, and is populated only by a successful live provider response.
#[derive(Clone, Default)]
pub(crate) struct BedrockCatalogEvidence {
    state: Arc<Mutex<BedrockEvidenceState>>,
}

#[derive(Default)]
struct BedrockEvidenceState {
    rows: Option<BTreeMap<String, BedrockFoundationModel>>,
    attempt: u64,
    consumed_attempt: u64,
    failure: Option<CatalogFailureKind>,
}

pub(crate) enum BedrockCatalogReadiness {
    Ready(Box<BedrockFoundationModel>),
    Failed(CatalogFailureKind),
    RefreshRequired,
}

impl BedrockCatalogEvidence {
    fn record(
        &self,
        result: &Result<Vec<BedrockFoundationModel>, CatalogFetchError>,
    ) -> Result<(), CatalogFetchError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| unavailable("Amazon Bedrock model evidence cache is unavailable"))?;
        state.attempt = state.attempt.saturating_add(1);
        match result {
            Ok(rows) => {
                state.rows = Some(
                    rows.iter()
                        .cloned()
                        .map(|row| (row.descriptor().id.clone(), row))
                        .collect(),
                );
                state.failure = None;
            }
            Err(error) => state.failure = Some(error.kind()),
        }
        Ok(())
    }

    pub(crate) fn get(&self, model: &str) -> Result<Option<BedrockFoundationModel>, &'static str> {
        self.state
            .lock()
            .map_err(|_| "Amazon Bedrock model evidence cache is unavailable")
            .map(|state| {
                state
                    .rows
                    .as_ref()
                    .and_then(|rows| rows.get(model))
                    .cloned()
            })
    }

    pub(crate) fn take_readiness(
        &self,
        model: &str,
    ) -> Result<BedrockCatalogReadiness, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Amazon Bedrock model evidence cache is unavailable")?;
        if state.attempt == state.consumed_attempt {
            return Ok(BedrockCatalogReadiness::RefreshRequired);
        }
        state.consumed_attempt = state.attempt;
        if let Some(kind) = state.failure {
            return Ok(BedrockCatalogReadiness::Failed(kind));
        }
        Ok(state
            .rows
            .as_ref()
            .and_then(|rows| rows.get(model))
            .cloned()
            .map(Box::new)
            .map_or(
                BedrockCatalogReadiness::RefreshRequired,
                BedrockCatalogReadiness::Ready,
            ))
    }
}

impl BedrockCatalog {
    /// Build a source for one validated region's control plane.
    #[must_use]
    pub fn new(
        http: HttpService,
        authorizer: Arc<dyn BedrockRequestAuthorizer>,
        region: &AwsRegion,
    ) -> Self {
        Self {
            http,
            authorizer,
            region: AwsRegionResolution::Resolved {
                region: region.clone(),
                origin: heycode_authorization_aws::AwsRegionOrigin::Connection,
            },
            evidence: BedrockCatalogEvidence::default(),
        }
    }

    fn with_resolution(
        http: HttpService,
        authorizer: Arc<dyn BedrockRequestAuthorizer>,
        region: AwsRegionResolution,
    ) -> Self {
        Self {
            http,
            authorizer,
            region,
            evidence: BedrockCatalogEvidence::default(),
        }
    }

    pub(crate) fn evidence(&self) -> BedrockCatalogEvidence {
        self.evidence.clone()
    }

    /// Discover every foundation model the account can see in this region,
    /// retaining the lifecycle, modality, streaming and inference-type
    /// evidence the shared [`ModelDescriptor`] vocabulary cannot express.
    ///
    /// # Errors
    /// Returns a classified credential, network, availability or
    /// invalid-response failure. One malformed row rejects the whole
    /// generation; nothing partial is ever returned.
    pub async fn discover(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<BedrockFoundationModel>, CatalogFetchError> {
        let result = self.discover_inner(cancellation).await;
        self.evidence.record(&result)?;
        result
    }

    async fn discover_inner(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<BedrockFoundationModel>, CatalogFetchError> {
        let region = self.region.region().ok_or_else(|| {
            CatalogFetchError::new(
                CatalogFailureKind::Unavailable,
                unresolved_region_message(&self.region),
            )
        })?;
        self.discover_region(region, None, cancellation).await
    }

    async fn discover_region(
        &self,
        region: &AwsRegion,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<BedrockFoundationModel>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let request = HttpRequest::get(control_plane_url(region))
            .and_then(|request| request.header("accept", "application/json"))
            .map(|request| request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT))
            .map_err(|_| invalid("Amazon Bedrock model list request could not be constructed"))?;
        let request = match credential {
            Some(secret) => request
                .header("authorization", &format!("Bearer {}", secret.expose()))
                .map_err(|_| {
                    unauthorized("Amazon Bedrock credential is not a usable bearer token")
                })?,
            None => {
                self.authorizer
                    .authorize(request, cancellation.clone())
                    .await?
            }
        };
        let response = self
            .http
            .send(request, cancellation.clone())
            .await
            .map_err(map_transport_error)?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        classify_status(&response)?;
        require_bounded(&response)?;
        require_json(&response)?;
        normalize(&response.body)
    }
}

#[async_trait]
impl ModelCatalog for BedrockCatalog {
    fn supports_parameter_credentials(&self) -> bool {
        true
    }

    fn provider(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        Ok(self
            .discover(cancellation)
            .await?
            .into_iter()
            .map(BedrockFoundationModel::into_descriptor)
            .collect())
    }

    async fn fetch_parameters_with_credential(
        &self,
        parameters: &BTreeMap<String, String>,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let region = draft_region(parameters)?;
        Ok(self
            .discover_region(&region, credential, cancellation)
            .await?
            .into_iter()
            .map(BedrockFoundationModel::into_descriptor)
            .collect())
    }

    async fn fetch_parameters(
        &self,
        parameters: &BTreeMap<String, String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        self.fetch_parameters_with_credential(parameters, None, cancellation)
            .await
    }
}

fn draft_region(parameters: &BTreeMap<String, String>) -> Result<AwsRegion, CatalogFetchError> {
    if parameters.len() != 1 {
        return Err(invalid(
            "Amazon Bedrock setup requires exactly one AWS region",
        ));
    }
    let region = parameters
        .get("region")
        .ok_or_else(|| invalid("Amazon Bedrock setup requires exactly one AWS region"))?;
    AwsRegion::new(region.clone())
        .map_err(|_| invalid("Amazon Bedrock setup region is not a valid region id"))
}

/// Safe Amazon Bedrock provider identity.
#[must_use]
pub fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: BEDROCK_PROVIDER.to_owned(),
        display_name: BEDROCK_DISPLAY_NAME.to_owned(),
        protocols: vec![ProviderProtocol::BedrockConverse],
    }
}

/// Amazon Bedrock connection metadata for the generic product wizard.
#[must_use]
pub fn bedrock_connection_profile() -> heycode_llm::ConnectionProfile {
    heycode_llm::ConnectionProfile {
        registry_name: BEDROCK_PROVIDER.to_owned(),
        descriptor: provider_descriptor(),
        default_model: None,
        credential_reference: Some(
            heycode_authorization_aws::AWS_BEDROCK_API_KEY_REFERENCE.to_owned(),
        ),
        family: heycode_llm::ConnectionFamily::Cloud,
        default_endpoint: None,
        help: Some(
            "Create a region-bound Amazon Bedrock API key and ensure the account can list foundation models in that region."
                .to_owned(),
        ),
        selectable_models: None,
        parameters: vec![heycode_llm::ConnectionParameter {
            id: "region".to_owned(),
            label: "AWS region".to_owned(),
            description: "Region used for Bedrock model discovery and inference, for example us-east-1"
                .to_owned(),
        }],
        model_selection: heycode_llm::ConnectionModelSelection::Catalog,
    }
}

/// Configuration captured by the Bedrock catalog contribution plugin.
///
/// Only the credential is configured here. The region is not: PAWS01 already
/// owns region resolution, and re-deriving it would let a second answer drift
/// away from the one the rest of the AWS stack reports.
#[derive(Clone)]
pub struct BedrockCatalogConfig {
    credential: CredentialQuery,
}

impl BedrockCatalogConfig {
    /// Authorize discovery with the Amazon Bedrock API key behind `credential`.
    #[must_use]
    pub const fn api_key(credential: CredentialQuery) -> Self {
        Self { credential }
    }
}

/// Register Amazon Bedrock discovery into the shared model catalog registry.
#[must_use]
pub fn bedrock_catalog_plugin(config: BedrockCatalogConfig) -> Box<dyn Plugin> {
    struct BedrockCatalogPlugin(BedrockCatalogConfig);

    impl Plugin for BedrockCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-bedrock"
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
                BEDROCK_PROVIDER,
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
            let authorizer = Arc::new(BedrockApiKeyAuthorizer::new(
                credentials,
                self.0.credential.clone(),
            ));
            let source =
                BedrockCatalog::with_resolution(http.as_ref().clone(), authorizer, resolution);
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(BedrockCatalogPlugin(config))
}

/// Name why a region did not resolve, never echoing the rejected value.
///
/// Shared with the Mantle source so the two AWS catalogs explain an
/// unusable region identically.
pub(crate) fn unresolved_region_detail(resolution: &AwsRegionResolution) -> &'static str {
    match resolution {
        AwsRegionResolution::Malformed { .. } => {
            "the configured AWS region is not a valid region id"
        }
        AwsRegionResolution::Undetermined => "the AWS region could not be determined",
        // `Resolved` never reaches here, and the enum is non-exhaustive.
        _ => "no AWS region is configured",
    }
}

/// Explain a region that did not resolve, naming the state and never the
/// rejected value.
fn unresolved_region_message(resolution: &AwsRegionResolution) -> String {
    format!(
        "Amazon Bedrock catalog cannot address an endpoint: {}",
        unresolved_region_detail(resolution)
    )
}

/// Refuse an oversized body instead of normalizing a prefix of it.
///
/// The transport enforces the same cap and fails with
/// [`TransportError::ResponseTooLarge`]; this check keeps the guarantee for
/// any transport that does not.
fn require_bounded(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    if response.body.len() > CATALOG_RESPONSE_LIMIT {
        return Err(invalid("Amazon Bedrock model list response is too large"));
    }
    Ok(())
}

pub(crate) fn require_json(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    if response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    }) {
        Ok(())
    } else {
        Err(invalid("Amazon Bedrock model list response is not JSON"))
    }
}

/// Map an HTTP status onto the stable catalog failure classes.
///
/// The documented failures are `AccessDeniedException` (403),
/// `ValidationException` (400), `ThrottlingException` (429) and
/// `InternalServerException` (500); 401 is added for a rejected bearer token.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_ListFoundationModels.html>
pub(crate) fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized(
            "Amazon Bedrock credential is missing or unauthorized",
        )),
        429 | 500..=599 => Err(unavailable(
            "Amazon Bedrock model list is temporarily unavailable",
        )),
        _ => Err(invalid(
            "Amazon Bedrock model list returned an unexpected HTTP status",
        )),
    }
}

/// Map provider-owned catalog preparation into the body-free inference error
/// vocabulary used by the async Provider hook.
pub(crate) fn catalog_preparation_error(error: CatalogError) -> LlmError {
    let class = match error {
        CatalogError::Cancelled { .. }
        | CatalogError::Refresh {
            kind: CatalogFailureKind::Cancelled,
            ..
        } => ProviderErrorClass::Cancelled,
        CatalogError::Refresh {
            kind: CatalogFailureKind::Unauthorized,
            ..
        } => ProviderErrorClass::Authentication,
        CatalogError::Refresh {
            kind: CatalogFailureKind::Network,
            ..
        } => ProviderErrorClass::Network,
        CatalogError::Refresh {
            kind: CatalogFailureKind::Unavailable,
            ..
        } => ProviderErrorClass::Overloaded,
        CatalogError::Refresh {
            kind: CatalogFailureKind::InvalidResponse,
            ..
        }
        | CatalogError::InvalidCatalog { .. } => ProviderErrorClass::Protocol,
        CatalogError::DuplicateCatalog { .. }
        | CatalogError::DuplicatePersistence
        | CatalogError::UnknownCatalog { .. }
        | CatalogError::NoCachedCatalog { .. }
        | CatalogError::RegistryUnavailable
        | CatalogError::Persistence { .. } => ProviderErrorClass::Server,
    };
    LlmError::Provider(ProviderFailure::new(class, ProviderFailureOrigin::Local))
}

pub(crate) fn catalog_failure_preparation_error(kind: CatalogFailureKind) -> LlmError {
    let class = match kind {
        CatalogFailureKind::Cancelled => ProviderErrorClass::Cancelled,
        CatalogFailureKind::Unauthorized => ProviderErrorClass::Authentication,
        CatalogFailureKind::Network => ProviderErrorClass::Network,
        CatalogFailureKind::Unavailable => ProviderErrorClass::Overloaded,
        CatalogFailureKind::InvalidResponse => ProviderErrorClass::Protocol,
    };
    LlmError::Provider(ProviderFailure::new(class, ProviderFailureOrigin::Local))
}

pub(crate) fn map_transport_error(error: TransportError) -> CatalogFetchError {
    match error {
        TransportError::Cancelled => CatalogFetchError::cancelled(),
        TransportError::Http {
            status: 401 | 403, ..
        } => unauthorized("Amazon Bedrock credential is missing or unauthorized"),
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            unavailable("Amazon Bedrock model list is temporarily unavailable")
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "Amazon Bedrock model list network request failed",
        ),
        // An oversized body is a response we refuse, not a network fault.
        TransportError::ResponseTooLarge { .. } => {
            invalid("Amazon Bedrock model list response is too large")
        }
        _ => invalid("Amazon Bedrock model list transport response is invalid"),
    }
}

fn unauthorized(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::Unauthorized, message)
}

fn unavailable(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::Unavailable, message)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn every_validated_region_forms_a_documented_control_plane_url() {
        for region in [
            "us-east-1",
            "eu-central-1",
            "ap-northeast-1",
            "us-gov-west-1",
        ] {
            let region = AwsRegion::new(region).unwrap();
            let url = control_plane_url(&region);
            assert_eq!(
                url,
                format!(
                    "https://bedrock.{}.amazonaws.com/foundation-models",
                    region.as_str()
                )
            );
            assert!(
                HttpRequest::get(&url).is_ok(),
                "`{url}` must be a valid request URL"
            );
        }
    }

    #[test]
    fn an_oversized_transport_response_is_refused_rather_than_reported_as_network_loss() {
        let failure = map_transport_error(TransportError::ResponseTooLarge { max_bytes: 16 });
        assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
    }

    #[test]
    fn typed_http_timeout_preserves_the_catalog_network_class() {
        let failure = map_transport_error(TransportError::Timeout);
        assert_eq!(failure.kind(), CatalogFailureKind::Network);
    }

    #[test]
    fn an_unresolved_region_is_explained_without_echoing_the_rejected_value() {
        let malformed = AwsRegionResolution::Malformed {
            origin: heycode_authorization_aws::AwsRegionOrigin::Environment {
                variable: heycode_authorization_aws::AWS_REGION_VAR,
            },
        };
        let message = unresolved_region_message(&malformed);
        assert!(message.contains("not a valid region id"), "{message}");
        assert!(
            unresolved_region_message(&AwsRegionResolution::Unresolved)
                .contains("no AWS region is configured")
        );
        assert!(
            unresolved_region_message(&AwsRegionResolution::Undetermined)
                .contains("could not be determined")
        );
    }
}

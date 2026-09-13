//! Exact resource/deployment readiness through the Azure OpenAI v1 Models API.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ServiceKey,
};
use heycode_credentials::{
    CredentialQuery, CredentialSecret, CredentialsService, SERVICE_CREDENTIALS,
};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CatalogFailureKind, CatalogFetchError, CatalogRegistry, ModelCatalog, ModelDescriptor,
    ProviderDescriptor, SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{
    AZURE_OPENAI_PROVIDER, AzureDeploymentName, AzureResourceName, provider_descriptor,
    responses_base_url,
};

const RESPONSE_LIMIT: usize = 256 * 1024;
const PARAMETER_RESOURCE: &str = "resource";
const PARAMETER_DEPLOYMENT: &str = "deployment";

/// Catalog configuration with optional active connection coordinates.
#[derive(Clone)]
pub struct AzureOpenAiCatalogConfig {
    credential: CredentialQuery,
    connection: Option<(AzureResourceName, AzureDeploymentName)>,
}

impl AzureOpenAiCatalogConfig {
    /// Resolve an Azure OpenAI API key at each readiness operation.
    #[must_use]
    pub const fn api_key(credential: CredentialQuery) -> Self {
        Self {
            credential,
            connection: None,
        }
    }

    /// Bind ordinary refresh to the active saved resource and deployment.
    #[must_use]
    pub fn with_connection(
        mut self,
        resource: AzureResourceName,
        deployment: AzureDeploymentName,
    ) -> Self {
        self.connection = Some((resource, deployment));
        self
    }
}

/// Provider-owned Azure OpenAI deployment readiness source.
pub struct AzureOpenAiCatalog {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    credential: CredentialQuery,
    connection: Option<(AzureResourceName, AzureDeploymentName)>,
}

impl AzureOpenAiCatalog {
    /// Build a credential-aware source without performing I/O.
    ///
    /// # Errors
    /// A credential kind other than `api-key` is refused.
    pub fn new(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        config: AzureOpenAiCatalogConfig,
    ) -> Result<Self, CatalogFetchError> {
        if config.credential.kind.as_str() != "api-key" {
            return Err(invalid("Azure OpenAI requires an api-key credential query"));
        }
        Ok(Self {
            http,
            credentials,
            credential: config.credential,
            connection: config.connection,
        })
    }

    async fn discover(
        &self,
        resource: &AzureResourceName,
        deployment: &AzureDeploymentName,
        supplied: Option<&CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let resolved = if supplied.is_none() {
            self.credentials
                .resolve(&self.credential)
                .map_err(|_| unavailable("Azure OpenAI credential store is unavailable"))?
        } else {
            None
        };
        let secret = supplied.or(resolved.as_ref()).ok_or_else(|| {
            unauthorized("Azure OpenAI API key is not configured for this connection")
        })?;
        let request = HttpRequest::get(model_url(resource, deployment))
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header("api-key", secret.expose()))
            .map(|request| request.with_max_response_bytes(RESPONSE_LIMIT))
            .map_err(|_| invalid("Azure OpenAI model request could not be constructed"))?;
        let response = self
            .http
            .send(request, cancellation.clone())
            .await
            .map_err(map_transport_error)?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        classify_status(&response)?;
        let model = parse_model(&response, deployment)?;
        Ok(vec![model])
    }
}

#[async_trait]
impl ModelCatalog for AzureOpenAiCatalog {
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
        match self.connection.as_ref() {
            Some((resource, deployment)) => {
                self.discover(resource, deployment, None, cancellation)
                    .await
            }
            None if cancellation.is_cancelled() => Err(CatalogFetchError::cancelled()),
            None => Ok(Vec::new()),
        }
    }

    async fn fetch_parameters(
        &self,
        parameters: &BTreeMap<String, String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let (resource, deployment) = validate_parameters(parameters)?;
        self.discover(&resource, &deployment, None, cancellation)
            .await
    }

    async fn fetch_parameters_with_credential(
        &self,
        parameters: &BTreeMap<String, String>,
        credential: Option<&CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let (resource, deployment) = validate_parameters(parameters)?;
        self.discover(&resource, &deployment, credential, cancellation)
            .await
    }
}

/// Register Azure OpenAI draft and active deployment readiness.
#[must_use]
pub fn azure_openai_catalog_plugin(config: AzureOpenAiCatalogConfig) -> Box<dyn Plugin> {
    struct AzureCatalogPlugin(AzureOpenAiCatalogConfig);

    impl Plugin for AzureCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-azure-openai"
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
                AZURE_OPENAI_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            &[SERVICE_MODELS, SERVICE_HTTP, SERVICE_CREDENTIALS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            let source =
                AzureOpenAiCatalog::new(http.as_ref().clone(), credentials, self.0.clone())
                    .map_err(|error| CoreError::other(error.message()))?;
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(AzureCatalogPlugin(config))
}

fn validate_parameters(
    parameters: &BTreeMap<String, String>,
) -> Result<(AzureResourceName, AzureDeploymentName), CatalogFetchError> {
    if parameters.len() != 2
        || !parameters.contains_key(PARAMETER_RESOURCE)
        || !parameters.contains_key(PARAMETER_DEPLOYMENT)
    {
        return Err(invalid(
            "Azure OpenAI coordinates must contain exactly resource and deployment",
        ));
    }
    let resource = AzureResourceName::new(parameters[PARAMETER_RESOURCE].clone())
        .map_err(|_| invalid("Azure OpenAI resource name is invalid"))?;
    let deployment = AzureDeploymentName::new(parameters[PARAMETER_DEPLOYMENT].clone())
        .map_err(|_| invalid("Azure OpenAI deployment name is invalid"))?;
    Ok((resource, deployment))
}

fn model_url(resource: &AzureResourceName, deployment: &AzureDeploymentName) -> String {
    format!(
        "{}/models/{}",
        responses_base_url(resource),
        deployment.as_str()
    )
}

#[derive(Deserialize)]
struct AzureModel {
    id: String,
    object: String,
    created: i64,
    owned_by: String,
}

fn parse_model(
    response: &HttpResponse,
    expected: &AzureDeploymentName,
) -> Result<ModelDescriptor, CatalogFetchError> {
    let is_json = response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    });
    if !is_json {
        return Err(invalid("Azure OpenAI model response is not JSON"));
    }
    let row: AzureModel = serde_json::from_slice(&response.body)
        .map_err(|_| invalid("Azure OpenAI model response has an invalid JSON shape"))?;
    if row.id != expected.as_str()
        || row.object != "model"
        || row.created < 0
        || row.owned_by.is_empty()
        || row.owned_by.len() > 256
        || row.owned_by.chars().any(char::is_control)
    {
        return Err(invalid(
            "Azure OpenAI model response has an invalid JSON shape",
        ));
    }
    let mut model = ModelDescriptor::unknown(row.id);
    model.created_at_ms = u64::try_from(row.created)
        .ok()
        .and_then(|value| value.checked_mul(1_000))
        .filter(|value| *value > 0);
    Ok(model)
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200 => Ok(()),
        401 | 403 => Err(unauthorized("Azure OpenAI rejected the configured API key")),
        404 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "Azure OpenAI resource or deployment was not found",
        )),
        429 | 500..=599 => Err(unavailable(
            "Azure OpenAI deployment readiness is temporarily unavailable",
        )),
        _ => Err(invalid(
            "Azure OpenAI deployment readiness returned an unexpected HTTP status",
        )),
    }
}

fn map_transport_error(error: TransportError) -> CatalogFetchError {
    match error {
        TransportError::Cancelled => CatalogFetchError::cancelled(),
        TransportError::Http {
            status: 401 | 403, ..
        } => unauthorized("Azure OpenAI rejected the configured API key"),
        TransportError::Http { status: 404, .. } => CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "Azure OpenAI resource or deployment was not found",
        ),
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            unavailable("Azure OpenAI deployment readiness is temporarily unavailable")
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "Azure OpenAI deployment readiness network request failed",
        ),
        TransportError::ResponseTooLarge { .. } => {
            invalid("Azure OpenAI deployment readiness response is too large")
        }
        _ => invalid("Azure OpenAI deployment readiness transport response is invalid"),
    }
}

fn unauthorized(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::Unauthorized, message)
}

fn unavailable(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::Unavailable, message)
}

fn invalid(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

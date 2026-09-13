//! Bounded canonical `/models` discovery for one explicit custom server.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

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

use crate::{CUSTOM_OPENAI_PROVIDER, CustomOpenAiEndpoint, provider_descriptor};

const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const MAX_MODELS: usize = 4096;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

/// Catalog configuration with an optional active saved connection.
#[derive(Clone, Default)]
pub struct CustomOpenAiCatalogConfig {
    connection: Option<(CustomOpenAiEndpoint, Option<CredentialQuery>)>,
}

impl CustomOpenAiCatalogConfig {
    /// Build setup-safe draft discovery with no active address or credential.
    #[must_use]
    pub const fn discovery() -> Self {
        Self { connection: None }
    }

    /// Bind ordinary refresh to one saved endpoint and optional bearer credential.
    #[must_use]
    pub fn with_connection(
        mut self,
        endpoint: CustomOpenAiEndpoint,
        credential: Option<CredentialQuery>,
    ) -> Self {
        self.connection = Some((endpoint, credential));
        self
    }

    fn credential(&self) -> Option<&CredentialQuery> {
        self.connection
            .as_ref()
            .and_then(|(_, credential)| credential.as_ref())
    }
}

/// Provider-owned canonical model-list source.
pub struct CustomOpenAiCatalog {
    http: HttpService,
    credentials: Option<Arc<CredentialsService>>,
    connection: Option<(CustomOpenAiEndpoint, Option<CredentialQuery>)>,
}

impl CustomOpenAiCatalog {
    /// Bind HTTP and optional credential services without performing I/O.
    ///
    /// # Errors
    /// A non-api-key saved credential or a missing required credential service
    /// is refused before publication.
    pub fn new(
        http: HttpService,
        credentials: Option<Arc<CredentialsService>>,
        config: CustomOpenAiCatalogConfig,
    ) -> Result<Self, CatalogFetchError> {
        if config
            .credential()
            .is_some_and(|credential| credential.kind.as_str() != "api-key")
        {
            return Err(invalid(
                "custom server bearer credential must be an api-key",
            ));
        }
        if config.credential().is_some() && credentials.is_none() {
            return Err(unavailable(
                "custom server credential service is unavailable",
            ));
        }
        Ok(Self {
            http,
            credentials,
            connection: config.connection,
        })
    }

    async fn discover(
        &self,
        endpoint: &CustomOpenAiEndpoint,
        configured: Option<&CredentialQuery>,
        supplied: Option<&CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let resolved = match (supplied, configured) {
            (None, Some(query)) => self
                .credentials
                .as_ref()
                .ok_or_else(|| unavailable("custom server credential service is unavailable"))?
                .resolve(query)
                .map_err(|_| unavailable("custom server credential store is unavailable"))?,
            _ => None,
        };
        let credential = supplied.or(resolved.as_ref());
        let mut request = HttpRequest::get(endpoint.models_url())
            .and_then(|request| request.header("accept", "application/json"))
            .map(|request| request.with_max_response_bytes(RESPONSE_LIMIT))
            .map_err(|_| invalid("custom server model request could not be constructed"))?;
        if let Some(credential) = credential {
            request = request
                .header("authorization", &format!("Bearer {}", credential.expose()))
                .map_err(|_| invalid("custom server model request could not be constructed"))?;
        }
        let operation = cancellation.child_token();
        let _guard = operation.clone().drop_guard();
        let response = tokio::time::timeout(
            DISCOVERY_TIMEOUT,
            self.http.send(request, operation.clone()),
        )
        .await
        .map_err(|_| network("custom server model request timed out"))?
        .map_err(map_transport_error)?;
        if operation.is_cancelled() && cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        classify_status(&response)?;
        parse_models(&response)
    }
}

#[async_trait]
impl ModelCatalog for CustomOpenAiCatalog {
    fn supports_endpoint_credentials(&self) -> bool {
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
            Some((endpoint, credential)) => {
                self.discover(endpoint, credential.as_ref(), None, cancellation)
                    .await
            }
            None if cancellation.is_cancelled() => Err(CatalogFetchError::cancelled()),
            None => Ok(Vec::new()),
        }
    }

    async fn fetch_endpoint(
        &self,
        endpoint: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let endpoint = CustomOpenAiEndpoint::new(endpoint)
            .map_err(|_| invalid("custom server URL is invalid"))?;
        self.discover(&endpoint, None, None, cancellation).await
    }

    async fn fetch_endpoint_with_credential(
        &self,
        endpoint: &str,
        credential: Option<&CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let endpoint = CustomOpenAiEndpoint::new(endpoint)
            .map_err(|_| invalid("custom server URL is invalid"))?;
        self.discover(&endpoint, None, credential, cancellation)
            .await
    }
}

/// Register setup-safe draft discovery and optional active refresh.
#[must_use]
pub fn custom_openai_catalog_plugin(config: CustomOpenAiCatalogConfig) -> Box<dyn Plugin> {
    struct CustomCatalogPlugin(CustomOpenAiCatalogConfig);

    impl Plugin for CustomCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-custom-openai"
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
                CUSTOM_OPENAI_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            if self.0.credential().is_some() {
                &[SERVICE_MODELS, SERVICE_HTTP, SERVICE_CREDENTIALS]
            } else {
                &[SERVICE_MODELS, SERVICE_HTTP]
            }
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = if self.0.credential().is_some() {
                Some(
                    context
                        .get::<CredentialsService>(SERVICE_CREDENTIALS)
                        .ok_or_else(|| {
                            CoreError::MissingService(SERVICE_CREDENTIALS.to_string())
                        })?,
                )
            } else {
                None
            };
            let source =
                CustomOpenAiCatalog::new(http.as_ref().clone(), credentials, self.0.clone())
                    .map_err(|error| CoreError::other(error.message()))?;
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(CustomCatalogPlugin(config))
}

#[derive(Deserialize)]
struct ModelList {
    object: Option<String>,
    data: Vec<ModelRow>,
}

#[derive(Deserialize)]
struct ModelRow {
    id: String,
    object: Option<String>,
    created: Option<i64>,
    owned_by: Option<String>,
}

fn parse_models(response: &HttpResponse) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
    if !response
        .content_type
        .as_deref()
        .is_some_and(json_content_type)
    {
        return Err(invalid("custom server model response is not JSON"));
    }
    let envelope: ModelList = serde_json::from_slice(&response.body)
        .map_err(|_| invalid("custom server model response has an invalid JSON shape"))?;
    if envelope
        .object
        .as_deref()
        .is_some_and(|object| object != "list")
        || envelope.data.len() > MAX_MODELS
    {
        return Err(invalid(
            "custom server model response has an invalid JSON shape",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut models = Vec::with_capacity(envelope.data.len());
    for row in envelope.data {
        if row.id.is_empty()
            || row.id.len() > 256
            || row.id.trim() != row.id
            || row.id.chars().any(char::is_control)
            || !ids.insert(row.id.clone())
            || row
                .object
                .as_deref()
                .is_some_and(|object| object != "model")
            || row.created.is_some_and(|created| created < 0)
            || row.owned_by.as_deref().is_some_and(|owner| {
                owner.is_empty() || owner.len() > 256 || owner.chars().any(char::is_control)
            })
        {
            return Err(invalid(
                "custom server model response has an invalid JSON shape",
            ));
        }
        let mut model = ModelDescriptor::unknown(row.id);
        model.created_at_ms = row
            .created
            .and_then(|value| u64::try_from(value).ok())
            .and_then(|value| value.checked_mul(1_000))
            .filter(|value| *value > 0);
        models.push(model);
    }
    Ok(models)
}

fn json_content_type(value: &str) -> bool {
    value == "application/json"
        || value.starts_with("application/json;")
        || value
            .split_once(';')
            .map_or(value, |(kind, _)| kind)
            .ends_with("+json")
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200 => Ok(()),
        401 | 403 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unauthorized,
            "custom server rejected the bearer credential",
        )),
        404 | 405 => Err(unavailable(
            "custom server does not expose canonical model discovery",
        )),
        408 | 429 | 500..=599 => Err(unavailable(
            "custom server model discovery is temporarily unavailable",
        )),
        _ => Err(invalid(
            "custom server model discovery returned an unexpected HTTP status",
        )),
    }
}

fn map_transport_error(error: TransportError) -> CatalogFetchError {
    match error {
        TransportError::Cancelled => CatalogFetchError::cancelled(),
        _ => network("custom server model request failed"),
    }
}

fn invalid(message: &str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

fn unavailable(message: &str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::Unavailable, message)
}

fn network(message: &str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::Network, message)
}

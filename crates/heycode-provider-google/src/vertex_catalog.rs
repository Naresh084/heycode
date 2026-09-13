//! Credential-aware Vertex Gemini setup readiness over one non-generating request.
//!
//! The request is the documented bodyless
//! `projects.locations.publishers.models.fetchPublisherModelConfig` GET:
//! <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/reference/rest/v1beta1/projects.locations.publishers.models/fetchPublisherModelConfig>.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization_gcp::{
    GcpAccountHealth, GcpAuthService, GcpLocation, GcpLocationHealth, GcpLocationKind,
    GcpMetadataPolicy, GcpProfileRequest, GcpProjectHealth, GcpProjectId, SERVICE_GCP_AUTH,
};
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ServiceKey,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CatalogFailureKind, CatalogFetchError, CatalogRegistry, ModelCatalog, ModelDescriptor,
    ProviderDescriptor, SERVICE_MODELS,
};
use tokio_util::sync::CancellationToken;

use crate::{GOOGLE_GEMINI_3_7_FLASH, MaintainedVertexCatalog};

const RESPONSE_LIMIT: usize = 1024 * 1024;
const PARAMETER_PROJECT: &str = "project";
const PARAMETER_LOCATION: &str = "location";

/// Operation-time OAuth token lookup used by the Vertex readiness source.
#[derive(Clone)]
pub struct VertexGeminiCatalogConfig {
    credential: CredentialQuery,
}

impl VertexGeminiCatalogConfig {
    /// Present the OAuth token behind one exact credential query.
    #[must_use]
    pub const fn oauth_token(credential: CredentialQuery) -> Self {
        Self { credential }
    }
}

/// Maintained Vertex model metadata plus an account-specific setup probe.
///
/// Ordinary catalog refresh stays credential-blind. Draft coordinate probes
/// independently confirm ADC presence, token availability, and reachability
/// with `fetchPublisherModelConfig`, which sends neither a prompt nor model
/// content.
pub struct VertexGeminiCatalog {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    gcp: Arc<GcpAuthService>,
    credential: CredentialQuery,
}

impl VertexGeminiCatalog {
    /// Build a Vertex source over the composed account and transport services.
    ///
    /// # Errors
    /// A query whose semantic kind is not `oauth-token` is refused.
    pub fn oauth_token(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        gcp: Arc<GcpAuthService>,
        credential: CredentialQuery,
    ) -> Result<Self, CatalogFetchError> {
        if credential.kind.as_str() != "oauth-token" {
            return Err(invalid(
                "Vertex AI requires an oauth-token credential query",
            ));
        }
        Ok(Self {
            http,
            credentials,
            gcp,
            credential,
        })
    }

    async fn readiness(
        &self,
        parameters: &BTreeMap<String, String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let (project, location) = validate_parameters(parameters)?;
        let profile = self
            .gcp
            .resolve(
                GcpProfileRequest {
                    project: Some(project.as_str().to_owned()),
                    location: Some(location.as_str().to_owned()),
                    metadata: GcpMetadataPolicy::probe(),
                    ..GcpProfileRequest::default()
                },
                cancellation.clone(),
            )
            .await;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        require_account(profile.account())?;
        require_project(profile.project(), &project)?;
        require_location(profile.location(), &location)?;

        let secret = self
            .credentials
            .resolve(&self.credential)
            .map_err(|_| unavailable("Google Cloud credential store is unavailable"))?
            .ok_or_else(|| {
                unauthorized("Google Cloud cloud-platform OAuth token is not configured")
            })?;
        let request = HttpRequest::get(readiness_url(&project, &location))
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| {
                request.header("authorization", &format!("Bearer {}", secret.expose()))
            })
            .map(|request| request.with_max_response_bytes(RESPONSE_LIMIT))
            .map_err(|_| invalid("Vertex AI readiness request could not be constructed"))?;
        let response = self
            .http
            .send(request, cancellation.clone())
            .await
            .map_err(map_transport_error)?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        classify_status(&response)?;
        require_json_object(&response)?;
        MaintainedVertexCatalog::vertex_gemini()
            .fetch(cancellation)
            .await
    }
}

#[async_trait]
impl ModelCatalog for VertexGeminiCatalog {
    fn provider(&self) -> ProviderDescriptor {
        MaintainedVertexCatalog::vertex_gemini().provider()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        MaintainedVertexCatalog::vertex_gemini()
            .fetch(cancellation)
            .await
    }

    async fn fetch_parameters(
        &self,
        parameters: &BTreeMap<String, String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        self.readiness(parameters, cancellation).await
    }
}

/// Register credential-aware Vertex setup while retaining maintained refresh.
#[must_use]
pub fn vertex_gemini_catalog_plugin(config: VertexGeminiCatalogConfig) -> Box<dyn Plugin> {
    struct VertexGeminiCatalogPlugin(VertexGeminiCatalogConfig);

    impl Plugin for VertexGeminiCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-google-vertex"
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
                crate::GOOGLE_VERTEX_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            &[
                SERVICE_MODELS,
                SERVICE_HTTP,
                SERVICE_CREDENTIALS,
                SERVICE_GCP_AUTH,
            ]
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
            let gcp = context
                .get::<GcpAuthService>(SERVICE_GCP_AUTH)
                .ok_or_else(|| CoreError::MissingService(SERVICE_GCP_AUTH.to_string()))?;
            let source = VertexGeminiCatalog::oauth_token(
                http.as_ref().clone(),
                credentials,
                gcp,
                self.0.credential.clone(),
            )
            .map_err(|error| CoreError::other(error.message()))?;
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(VertexGeminiCatalogPlugin(config))
}

fn validate_parameters(
    parameters: &BTreeMap<String, String>,
) -> Result<(GcpProjectId, GcpLocation), CatalogFetchError> {
    if parameters.len() != 2
        || !parameters.contains_key(PARAMETER_PROJECT)
        || !parameters.contains_key(PARAMETER_LOCATION)
    {
        return Err(invalid(
            "Vertex AI coordinates must contain exactly project and location",
        ));
    }
    let project = GcpProjectId::new(parameters[PARAMETER_PROJECT].clone())
        .map_err(|_| invalid("Vertex AI project is not a valid project id or number"))?;
    let location = GcpLocation::new(parameters[PARAMETER_LOCATION].clone())
        .map_err(|_| invalid("Vertex AI location is not a valid region or global"))?;
    Ok((project, location))
}

fn require_account(account: &GcpAccountHealth) -> Result<(), CatalogFetchError> {
    match account {
        GcpAccountHealth::Configured { .. } => Ok(()),
        GcpAccountHealth::Absent | GcpAccountHealth::Faulted { .. } => Err(unauthorized(
            "Google Cloud Application Default Credentials are not configured or usable",
        )),
        GcpAccountHealth::Undetermined { .. } => Err(unavailable(
            "Google Cloud Application Default Credentials state could not be determined",
        )),
    }
}

fn require_project(
    health: &GcpProjectHealth,
    expected: &GcpProjectId,
) -> Result<(), CatalogFetchError> {
    match health {
        GcpProjectHealth::Confirmed { project, .. }
        | GcpProjectHealth::Unconfirmed { project, .. }
            if project == expected =>
        {
            Ok(())
        }
        GcpProjectHealth::Confirmed { .. } | GcpProjectHealth::Unconfirmed { .. } => Err(invalid(
            "Google Cloud resolved a different project than the selected coordinate",
        )),
        GcpProjectHealth::Unset | GcpProjectHealth::Malformed { .. } => Err(unauthorized(
            "Google Cloud project is not configured or usable",
        )),
        GcpProjectHealth::Undetermined { .. } => Err(unavailable(
            "Google Cloud project state could not be determined",
        )),
    }
}

fn require_location(
    health: &GcpLocationHealth,
    expected: &GcpLocation,
) -> Result<(), CatalogFetchError> {
    match health {
        GcpLocationHealth::Selected { location, .. } if location == expected => Ok(()),
        GcpLocationHealth::Selected { .. } => Err(invalid(
            "Google Cloud resolved a different location than the selected coordinate",
        )),
        GcpLocationHealth::Unset | GcpLocationHealth::Malformed { .. } => {
            Err(invalid("Google Cloud location is not configured or usable"))
        }
        GcpLocationHealth::Undetermined { .. } => Err(unavailable(
            "Google Cloud location state could not be determined",
        )),
    }
}

fn readiness_url(project: &GcpProjectId, location: &GcpLocation) -> String {
    let host = match location.kind() {
        GcpLocationKind::Global => "aiplatform.googleapis.com".to_owned(),
        GcpLocationKind::Region => format!("{}-aiplatform.googleapis.com", location.as_str()),
    };
    format!(
        "https://{host}/v1beta1/projects/{}/locations/{}/publishers/google/models/{GOOGLE_GEMINI_3_7_FLASH}:fetchPublisherModelConfig",
        project.as_str(),
        location.as_str(),
    )
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized(
            "Vertex AI rejected the configured Google Cloud authority",
        )),
        429 | 500..=599 => Err(unavailable(
            "Vertex AI model readiness is temporarily unavailable",
        )),
        _ => Err(invalid(
            "Vertex AI model readiness returned an unexpected HTTP status",
        )),
    }
}

fn require_json_object(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    let is_json = response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    });
    if !is_json {
        return Err(invalid("Vertex AI model readiness response is not JSON"));
    }
    let value: serde_json::Value = serde_json::from_slice(&response.body)
        .map_err(|_| invalid("Vertex AI model readiness has an invalid JSON shape"))?;
    if !value.is_object() {
        return Err(invalid(
            "Vertex AI model readiness has an invalid JSON shape",
        ));
    }
    Ok(())
}

fn map_transport_error(error: TransportError) -> CatalogFetchError {
    match error {
        TransportError::Cancelled => CatalogFetchError::cancelled(),
        TransportError::Http {
            status: 401 | 403, ..
        } => unauthorized("Vertex AI rejected the configured Google Cloud authority"),
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            unavailable("Vertex AI model readiness is temporarily unavailable")
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "Vertex AI model readiness network request failed",
        ),
        TransportError::ResponseTooLarge { .. } => {
            invalid("Vertex AI model readiness response is too large")
        }
        _ => invalid("Vertex AI model readiness transport response is invalid"),
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

//! Distinct Ollama sibling identity, native catalog and smoke prerequisites.
//!
//! Ollama's native `/api/*` identity/catalog and its OpenAI-compatible `/v1/*`
//! surface are separate evidence. Native-only rows never claim an inference
//! protocol; picker rows join product identity, native details/running state,
//! per-model capabilities and compatibility-list presence before carrying the
//! documented Chat protocol.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ProviderProtocol, ServiceKey,
};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, CatalogRegistry, ChatRequest,
    ChunkStream, InferenceAdapter, InferenceStream, LlmError, ModelCapabilities, ModelCatalog,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, OpenAiChatCompletionsAdapter,
    OpenAiChatCompletionsConfig, OpenAiCompatClient, OpenAiCompatConfig, Provider,
    ProviderDescriptor, ProviderInfo, ProviderProfile, RequestDraft, ResolveError, ResolvedCall,
    SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// Ollama's documented default local origin.
pub const OLLAMA_DEFAULT_BASE_URL: &str = "http://localhost:11434";
/// Distinct provider/catalog identity.
pub const OLLAMA_PROVIDER: &str = "ollama";
/// Human display name.
pub const OLLAMA_DISPLAY_NAME: &str = "Ollama";
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const OLLAMA_COMPATIBILITY_KEY: &str = "ollama";
const MAX_PICKER_MODELS: usize = 256;

/// Concrete joined catalog service published by [`ollama_plugin`].
pub const SERVICE_OLLAMA_CATALOG: ServiceKey = ServiceKey::new("ollama/catalog");
/// Explicit non-secret profile published by [`ollama_plugin`].
pub const SERVICE_OLLAMA_PROFILE: ServiceKey = ServiceKey::new("ollama/profile");
/// Constructed Chat provider published for the composition root bridge.
pub const SERVICE_OLLAMA_INFERENCE: ServiceKey = ServiceKey::new("ollama/inference");
/// Read-only identity/readiness inspector.
pub const SERVICE_OLLAMA_INSPECTOR: ServiceKey = ServiceKey::new("ollama");

/// Validated Ollama origin and its independent native/compatibility surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaEndpoint {
    base_url: String,
    version_url: String,
    tags_url: String,
    running_url: String,
    show_url: String,
    openai_base_url: String,
    openai_models_url: String,
}

impl OllamaEndpoint {
    /// Documented default local Ollama endpoint.
    #[must_use]
    pub fn local() -> Self {
        Self::derive(OLLAMA_DEFAULT_BASE_URL)
    }

    /// Validate an explicit origin.
    ///
    /// # Errors
    /// Non-HTTP(S), credential-bearing or hostless origins fail without
    /// retaining a partial endpoint.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, OllamaReadinessError> {
        let endpoint = Self::derive(base_url.as_ref());
        for url in [
            endpoint.version_url(),
            endpoint.tags_url(),
            endpoint.running_url(),
            endpoint.show_url(),
            endpoint.openai_models_url(),
        ] {
            HttpRequest::get(url).map_err(|_| OllamaReadinessError::InvalidEndpoint)?;
        }
        Ok(endpoint)
    }

    /// Normalized origin.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Native product-version endpoint.
    #[must_use]
    pub fn version_url(&self) -> &str {
        &self.version_url
    }

    /// Native downloaded-model catalog endpoint.
    #[must_use]
    pub fn tags_url(&self) -> &str {
        &self.tags_url
    }

    /// Native running-model endpoint.
    #[must_use]
    pub fn running_url(&self) -> &str {
        &self.running_url
    }

    /// Native read-only per-model metadata endpoint.
    #[must_use]
    pub fn show_url(&self) -> &str {
        &self.show_url
    }

    /// OpenAI-compatible version root used by the shared Chat adapter.
    #[must_use]
    pub fn openai_base_url(&self) -> &str {
        &self.openai_base_url
    }

    /// OpenAI-compatible model picker endpoint.
    #[must_use]
    pub fn openai_models_url(&self) -> &str {
        &self.openai_models_url
    }

    fn derive(base_url: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_owned();
        Self {
            version_url: format!("{base_url}/api/version"),
            tags_url: format!("{base_url}/api/tags"),
            running_url: format!("{base_url}/api/ps"),
            show_url: format!("{base_url}/api/show"),
            openai_base_url: format!("{base_url}/v1"),
            openai_models_url: format!("{base_url}/v1/models"),
            base_url,
        }
    }
}

/// Explicit user-selected Ollama profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaProfile {
    default_model: String,
}

impl OllamaProfile {
    /// Build a profile around one model already selected from the native and
    /// OpenAI-compatible catalogs.
    ///
    /// # Errors
    /// Blank, untrimmed, control-bearing or oversized ids are rejected.
    pub fn new(default_model: impl Into<String>) -> Result<Self, OllamaReadinessError> {
        let default_model = default_model.into();
        if !safe_text(&default_model, 256) {
            return Err(OllamaReadinessError::InvalidModel);
        }
        Ok(Self { default_model })
    }

    /// Documented OpenAI Chat profile, distinct from the native catalog's
    /// protocol-Unknown evidence.
    #[must_use]
    pub fn provider_profile(&self) -> ProviderProfile {
        ProviderProfile {
            registry_name: OLLAMA_PROVIDER.to_owned(),
            descriptor: inference_descriptor(),
            default_model: self.default_model.clone(),
            credential_reference: None,
        }
    }

    /// Explicit default selected from the joined picker catalog.
    #[must_use]
    pub fn default_model(&self) -> &str {
        &self.default_model
    }
}

/// Provider-owned Ollama Chat route over its documented OpenAI-compatible API.
///
/// Native Ollama identity/catalog evidence remains in [`OllamaCatalog`]. This
/// type claims only the separately documented Chat Completions protocol and
/// uses Ollama's published required-but-ignored SDK key value; it never asks
/// the credential registry for a secret.
#[derive(Clone)]
pub struct OllamaInference {
    legacy: OpenAiCompatClient,
    adapter: OpenAiChatCompletionsAdapter,
    default_model: String,
    credential_reference: Option<String>,
}

impl OllamaInference {
    /// Bind an explicit selected model to the shared OpenAI Chat adapter.
    ///
    /// # Errors
    /// Invalid model ids or an unusable compatibility endpoint fail before the
    /// provider can enter a registry.
    pub fn new(
        http: HttpService,
        endpoint: OllamaEndpoint,
        default_model: impl Into<String>,
    ) -> Result<Self, OllamaInferenceError> {
        Self::with_credential(
            http,
            endpoint,
            default_model,
            heycode_llm::RouteCredential::fixed(OLLAMA_COMPATIBILITY_KEY),
        )
    }

    /// Bind an explicit server credential to the selected Chat route.
    ///
    /// # Errors
    /// Invalid model or endpoint fails before publication.
    pub fn with_credential(
        http: HttpService,
        endpoint: OllamaEndpoint,
        default_model: impl Into<String>,
        credential: heycode_llm::RouteCredential,
    ) -> Result<Self, OllamaInferenceError> {
        let credential_reference = match credential.binding() {
            heycode_llm::AuthenticationBinding::Credential(reference) => {
                Some(reference.as_str().to_owned())
            }
            _ => None,
        };
        let profile = OllamaProfile::new(default_model).map_err(OllamaInferenceError::Profile)?;
        let base_url = endpoint.openai_base_url().to_owned();
        let legacy = OpenAiCompatClient::with_credential_and_transport(
            OpenAiCompatConfig {
                base_url: base_url.clone(),
                api_key_env: "OLLAMA_API_KEY",
                extra_headers: Vec::new(),
            },
            credential.clone(),
            http.clone(),
        )
        .map_err(OllamaInferenceError::Route)?;
        let adapter = OpenAiChatCompletionsAdapter::new(
            OpenAiChatCompletionsConfig::with_credential(
                inference_descriptor(),
                base_url,
                credential,
            ),
            http,
        )
        .map_err(OllamaInferenceError::Route)?;
        Ok(Self {
            legacy,
            adapter,
            default_model: profile.default_model,
            credential_reference,
        })
    }

    /// Profile projected from this live provider instance.
    #[must_use]
    pub fn provider_profile(&self) -> ProviderProfile {
        ProviderProfile {
            registry_name: OLLAMA_PROVIDER.to_owned(),
            descriptor: inference_descriptor(),
            default_model: self.default_model.clone(),
            credential_reference: self.credential_reference.clone(),
        }
    }
}

impl Provider for OllamaInference {
    fn credential_reference(&self) -> Option<&str> {
        self.credential_reference.as_deref()
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: OLLAMA_PROVIDER.to_owned(),
            default_model: self.default_model.clone(),
        }
    }

    fn descriptor(&self) -> ProviderDescriptor {
        inference_descriptor()
    }

    fn describe_model(&self, model: &str) -> ModelDescriptor {
        ModelDescriptor::unknown(model)
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.legacy.stream(&request.model, &request)
    }
}

impl InferenceAdapter for OllamaInference {
    fn descriptor(&self) -> ProviderDescriptor {
        inference_descriptor()
    }

    fn authentication_binding(&self) -> heycode_llm::AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        self.adapter.resolve(draft, model)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        InferenceAdapter::stream(&self.adapter, call)
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: CancellationToken,
    ) -> InferenceStream {
        InferenceAdapter::stream_cancellable(&self.adapter, call, cancellation)
    }
}

/// Stable Ollama inference-construction failure.
#[derive(Debug, thiserror::Error)]
pub enum OllamaInferenceError {
    /// Explicit default model was invalid.
    #[error("Ollama inference profile is invalid")]
    Profile(#[source] OllamaReadinessError),
    /// Shared Chat adapter rejected the route.
    #[error("Ollama inference route is invalid")]
    Route(#[source] LlmError),
}

/// Ollama-native model details from `/api/tags`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaModelDetails {
    /// Storage format such as `gguf`.
    pub format: Option<String>,
    /// Primary model family.
    pub family: Option<String>,
    /// Every published family.
    pub families: Vec<String>,
    /// Human parameter-size label.
    pub parameter_size: Option<String>,
    /// Quantization level.
    pub quantization_level: Option<String>,
}

/// One downloaded Ollama model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaModelRecord {
    /// Native model name used on the wire.
    pub name: String,
    /// Native model alias, retained separately when it differs from `name`.
    pub model: String,
    /// Last modification timestamp text.
    pub modified_at: String,
    /// On-disk bytes.
    pub size: u64,
    /// Content digest.
    pub digest: String,
    /// Native model details.
    pub details: OllamaModelDetails,
}

impl OllamaModelRecord {
    /// Conservative shared descriptor. Native catalog identity proves no
    /// inference capability by itself.
    #[must_use]
    pub fn descriptor(&self) -> ModelDescriptor {
        ModelDescriptor {
            id: self.name.clone(),
            display_name: self.name.clone(),
            aliases: Vec::new(),
            created_at_ms: None,
            context_window: None,
            max_output_tokens: None,
            lifecycle: ModelLifecycle::unknown(),
            capabilities: ModelCapabilities::unknown(),
            pricing: ModelPricing::unknown(),
            performance: ModelPerformance::unknown(),
            reasoning: None,
        }
    }
}

/// Exact loaded-state metadata joined from read-only `/api/ps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaRunningModel {
    model: String,
    digest: String,
    expires_at: String,
    size_vram: u64,
    context_length: u64,
}

impl OllamaRunningModel {
    /// Provider expiry timestamp for this loaded instance.
    #[must_use]
    pub fn expires_at(&self) -> &str {
        &self.expires_at
    }

    /// Bytes currently resident in VRAM.
    #[must_use]
    pub const fn size_vram(&self) -> u64 {
        self.size_vram
    }

    /// Active loaded-instance context length.
    #[must_use]
    pub const fn context_length(&self) -> u64 {
        self.context_length
    }
}

/// Picker-ready model joined across native identity/details/running state and
/// the OpenAI-compatible model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaPickerModel {
    record: OllamaModelRecord,
    capabilities: Vec<String>,
    context_window: Option<u64>,
    running: Option<OllamaRunningModel>,
}

impl OllamaPickerModel {
    /// Complete native downloaded-model record.
    #[must_use]
    pub const fn record(&self) -> &OllamaModelRecord {
        &self.record
    }

    /// Exact `/api/show` capability strings in provider order.
    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    /// Architecture-specific maximum context evidence from `model_info`.
    #[must_use]
    pub const fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    /// Current read-only loaded state, when `/api/ps` listed this model.
    #[must_use]
    pub const fn running(&self) -> Option<&OllamaRunningModel> {
        self.running.as_ref()
    }

    /// Shared picker descriptor derived only from the joined evidence.
    #[must_use]
    pub fn descriptor(&self) -> ModelDescriptor {
        let aliases = if self.record.model != self.record.name {
            vec![self.record.model.clone()]
        } else {
            Vec::new()
        };
        ModelDescriptor {
            id: self.record.name.clone(),
            display_name: format!(
                "{} ({})",
                self.record.name,
                if self.running.is_some() {
                    "running"
                } else {
                    "downloaded"
                }
            ),
            aliases,
            created_at_ms: None,
            context_window: self.context_window,
            max_output_tokens: None,
            lifecycle: ModelLifecycle::unknown(),
            capabilities: ModelCapabilities {
                tools: listed_support(&self.capabilities, "tools"),
                reasoning: listed_support(&self.capabilities, "thinking"),
                image_input: listed_support(&self.capabilities, "vision"),
                ..ModelCapabilities::unknown()
            },
            pricing: ModelPricing::unknown(),
            performance: ModelPerformance::unknown(),
            reasoning: None,
        }
    }
}

/// Reads Ollama's native downloaded-model catalog.
#[derive(Clone)]
pub struct OllamaCatalog {
    http: HttpService,
    endpoint: OllamaEndpoint,
    credential: Option<heycode_llm::RouteCredential>,
}

impl OllamaCatalog {
    /// Bind one native catalog reader.
    #[must_use]
    pub const fn new(http: HttpService, endpoint: OllamaEndpoint) -> Self {
        Self {
            http,
            endpoint,
            credential: None,
        }
    }

    /// Bind an explicitly selected server credential, resolved on each request.
    #[must_use]
    pub fn with_credential(mut self, credential: heycode_llm::RouteCredential) -> Self {
        self.credential = Some(credential);
        self
    }

    fn authenticated_http(&self) -> HttpService {
        match self.credential.as_ref() {
            None => self.http.clone(),
            Some(credential) => {
                catalog_http(self.http.clone(), self.endpoint.clone(), credential.clone())
            }
        }
    }

    /// Identity-only native catalog descriptor.
    #[must_use]
    pub fn provider_descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: OLLAMA_PROVIDER.to_owned(),
            display_name: OLLAMA_DISPLAY_NAME.to_owned(),
            protocols: vec![ProviderProtocol::Unknown],
        }
    }

    /// Joined picker descriptor after native identity and compatibility agree.
    #[must_use]
    pub fn picker_descriptor(&self) -> ProviderDescriptor {
        inference_descriptor()
    }

    /// Fetch the complete native downloaded-model list.
    ///
    /// # Errors
    /// Transport/status/content-type/body/identity failures reject the whole
    /// generation.
    pub async fn list_models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<OllamaModelRecord>, CatalogFetchError> {
        let response = send(
            &self.authenticated_http(),
            self.endpoint.tags_url(),
            cancellation,
            DEFAULT_TIMEOUT,
        )
        .await
        .map_err(readiness_to_catalog)?;
        parse_tags(&response).map_err(readiness_to_catalog)
    }

    /// Build picker-ready rows from all read-only product surfaces.
    ///
    /// # Errors
    /// Identity, native/OpenAI catalog, running-state or per-model detail
    /// mismatches reject the complete generation.
    pub async fn picker_models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<OllamaPickerModel>, CatalogFetchError> {
        inspect_snapshot(&self.authenticated_http(), &self.endpoint, cancellation)
            .await
            .map(|snapshot| snapshot.models)
            .map_err(readiness_to_catalog)
    }
}

#[async_trait]
impl ModelCatalog for OllamaCatalog {
    fn supports_endpoint_credentials(&self) -> bool {
        true
    }

    async fn fetch_endpoint_with_credential(
        &self,
        endpoint: &str,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let endpoint = OllamaEndpoint::new(endpoint).map_err(readiness_to_catalog)?;
        let mut source = Self::new(self.http.clone(), endpoint);
        if let Some(credential) = credential {
            source =
                source.with_credential(heycode_llm::RouteCredential::fixed(credential.expose()));
        }
        source.fetch(cancellation).await
    }
    async fn fetch_endpoint(
        &self,
        endpoint: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let endpoint = OllamaEndpoint::new(endpoint).map_err(readiness_to_catalog)?;
        Self::new(self.http.clone(), endpoint)
            .fetch(cancellation)
            .await
    }

    fn provider(&self) -> ProviderDescriptor {
        self.picker_descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        Ok(self
            .picker_models(cancellation)
            .await?
            .iter()
            .map(OllamaPickerModel::descriptor)
            .collect())
    }
}

fn catalog_http(
    http: HttpService,
    endpoint: OllamaEndpoint,
    credential: heycode_llm::RouteCredential,
) -> HttpService {
    HttpService::new(Arc::new(OllamaCatalogTransport {
        http,
        endpoint,
        credential,
    }))
}

struct OllamaCatalogTransport {
    http: HttpService,
    endpoint: OllamaEndpoint,
    credential: heycode_llm::RouteCredential,
}

impl heycode_http::HttpTransport for OllamaCatalogTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> heycode_http::BufferedResponseFuture {
        let http = self.http.clone();
        let endpoint = self.endpoint.clone();
        let credential = self.credential.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(TransportError::Cancelled);
            }
            let invalid = || TransportError::InvalidRequest {
                field: "authorization",
                message: "Ollama catalog credential request is invalid".into(),
            };
            if ![
                endpoint.version_url(),
                endpoint.tags_url(),
                endpoint.running_url(),
                endpoint.show_url(),
                endpoint.openai_models_url(),
            ]
            .contains(&request.url())
                || request
                    .headers()
                    .iter()
                    .any(|header| header.name().eq_ignore_ascii_case("authorization"))
            {
                return Err(invalid());
            }
            let secret = credential.acquire().map_err(|_| invalid())?;
            let request =
                request.header("authorization", &format!("Bearer {}", secret.expose()))?;
            http.send(request, cancellation).await
        })
    }
    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::SseEventStream {
        Box::pin(futures::stream::once(async {
            Err(TransportError::InvalidRequest {
                field: "transport",
                message: "Ollama catalog transport does not execute inference".into(),
            })
        }))
    }
}

/// Register the distinct Ollama native catalog into the shared model registry.
#[must_use]
pub fn ollama_catalog_plugin(endpoint: OllamaEndpoint) -> Box<dyn Plugin> {
    struct OllamaCatalogPlugin(OllamaEndpoint);

    impl Plugin for OllamaCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-ollama"
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
                OLLAMA_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MODELS, SERVICE_HTTP]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            models
                .register(
                    context,
                    Arc::new(OllamaCatalog::new(http.as_ref().clone(), self.0.clone())),
                )
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(OllamaCatalogPlugin(endpoint))
}

/// Publish the complete crate-local Ollama product boundary.
///
/// The shared provider registry has no late effect-registration API, so the
/// constructed inference provider is exposed under
/// [`SERVICE_OLLAMA_INFERENCE`] for the composition root bridge rather than
/// falsely claiming picker activation. Catalog registration and all four
/// services are context-owned and unwind on shutdown.
#[must_use]
pub fn ollama_plugin(endpoint: OllamaEndpoint, profile: OllamaProfile) -> Box<dyn Plugin> {
    ollama_plugin_with_credential(endpoint, profile, None)
}

/// Mount Ollama with an optional explicit credential for an authenticated server.
#[must_use]
pub fn ollama_plugin_with_credential(
    endpoint: OllamaEndpoint,
    profile: OllamaProfile,
    credential: Option<heycode_credentials::CredentialQuery>,
) -> Box<dyn Plugin> {
    struct OllamaPlugin {
        endpoint: OllamaEndpoint,
        profile: OllamaProfile,
        credential: Option<heycode_credentials::CredentialQuery>,
    }

    impl Plugin for OllamaPlugin {
        fn name(&self) -> &'static str {
            "provider-ollama"
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

        fn provides(&self) -> &'static [ServiceKey] {
            &[
                SERVICE_OLLAMA_CATALOG,
                SERVICE_OLLAMA_PROFILE,
                SERVICE_OLLAMA_INFERENCE,
                SERVICE_OLLAMA_INSPECTOR,
            ]
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            vec![PluginContributionSpec::new(
                ContributionKind::ModelCatalog,
                OLLAMA_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            if self.credential.is_some() {
                &[
                    SERVICE_MODELS,
                    SERVICE_HTTP,
                    heycode_credentials::SERVICE_CREDENTIALS,
                ]
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
            let http = http.as_ref().clone();
            let credential = self
                .credential
                .as_ref()
                .map(|query| {
                    context
                        .get::<heycode_credentials::CredentialsService>(
                            heycode_credentials::SERVICE_CREDENTIALS,
                        )
                        .map(|service| {
                            heycode_llm::RouteCredential::registry(
                                service.as_ref().clone(),
                                query.clone(),
                            )
                        })
                        .ok_or_else(|| {
                            CoreError::MissingService(
                                heycode_credentials::SERVICE_CREDENTIALS.to_string(),
                            )
                        })
                })
                .transpose()?;
            let mut catalog = OllamaCatalog::new(http.clone(), self.endpoint.clone());
            if let Some(credential) = credential.as_ref() {
                catalog = catalog.with_credential(credential.clone());
            }
            let inference = OllamaInference::with_credential(
                http.clone(),
                self.endpoint.clone(),
                self.profile.default_model.clone(),
                credential.clone().unwrap_or_else(|| {
                    heycode_llm::RouteCredential::fixed(OLLAMA_COMPATIBILITY_KEY)
                }),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let inspector =
                OllamaInspector::new(catalog.authenticated_http(), self.endpoint.clone());

            models
                .register(context, Arc::new(catalog.clone()))
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.provide(SERVICE_OLLAMA_CATALOG, self.name(), catalog)?;
            context.provide(SERVICE_OLLAMA_PROFILE, self.name(), self.profile.clone())?;
            context.provide(SERVICE_OLLAMA_INFERENCE, self.name(), inference)?;
            context.provide(SERVICE_OLLAMA_INSPECTOR, self.name(), inspector)
        }
    }

    Box::new(OllamaPlugin {
        endpoint,
        profile,
        credential,
    })
}

/// Read-only prerequisites for a later live Chat smoke.
#[derive(Clone)]
pub struct OllamaInspector {
    http: HttpService,
    endpoint: OllamaEndpoint,
}

impl OllamaInspector {
    /// Bind one read-only inspector.
    #[must_use]
    pub const fn new(http: HttpService, endpoint: OllamaEndpoint) -> Self {
        Self { http, endpoint }
    }

    /// Require Ollama identity plus native/OpenAI model agreement.
    ///
    /// This never invokes inference and therefore never claims a live smoke
    /// passed.
    ///
    /// # Errors
    /// Invalid model, cancellation, unavailable/unrecognized surfaces, or a
    /// model absent from either list.
    pub async fn inspect(
        &self,
        model: &str,
        cancellation: CancellationToken,
    ) -> Result<OllamaSmokeReadiness, OllamaReadinessError> {
        if !safe_text(model, 256) {
            return Err(OllamaReadinessError::InvalidModel);
        }
        let InspectionSnapshot {
            version,
            models,
            non_chat,
        } = inspect_snapshot(&self.http, &self.endpoint, cancellation).await?;
        let picker_model = models
            .into_iter()
            .find(|row| row.record.name == model)
            .ok_or_else(|| {
                if non_chat.contains(model) {
                    OllamaReadinessError::ModelNotChatCapable
                } else {
                    OllamaReadinessError::ModelMissing
                }
            })?;
        Ok(OllamaSmokeReadiness {
            version,
            model: model.to_owned(),
            picker_model,
        })
    }
}

/// Proven read-only prerequisites, not a live inference result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaSmokeReadiness {
    version: String,
    model: String,
    picker_model: OllamaPickerModel,
}

impl OllamaSmokeReadiness {
    /// Observed Ollama version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Model present in both catalogs.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Picker metadata proven by the same read-only inspection.
    #[must_use]
    pub const fn picker_model(&self) -> &OllamaPickerModel {
        &self.picker_model
    }

    /// Read-only prerequisites never equal a live generation.
    #[must_use]
    pub const fn live_smoke_passed(&self) -> bool {
        false
    }
}

/// Stable Ollama discovery/readiness failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OllamaReadinessError {
    /// The selected server rejected or requires a credential.
    #[error("Ollama server requires a valid API key")]
    Unauthorized,
    /// Configured endpoint was invalid.
    #[error("Ollama endpoint must be an absolute host-qualified HTTP(S) origin")]
    InvalidEndpoint,
    /// Requested model id was invalid.
    #[error("Ollama model id is invalid")]
    InvalidModel,
    /// Version surface did not carry Ollama's documented body.
    #[error("Ollama version surface is unrecognized")]
    UnrecognizedVersion,
    /// Native tags response was malformed.
    #[error("Ollama native model catalog is invalid")]
    InvalidNativeCatalog,
    /// OpenAI-compatible model list was malformed.
    #[error("Ollama OpenAI-compatible model catalog is invalid")]
    InvalidOpenAiCatalog,
    /// Selected model was absent from one catalog.
    #[error("Ollama selected model is not visible on both required surfaces")]
    ModelMissing,
    /// Selected model exists but `/api/show` does not list completion.
    #[error("Ollama selected model is not completion-capable")]
    ModelNotChatCapable,
    /// Local server/transport did not answer successfully.
    #[error("Ollama local server is unavailable")]
    Unavailable,
    /// Caller cancelled inspection.
    #[error("Ollama inspection was cancelled")]
    Cancelled,
}

#[derive(Deserialize)]
struct VersionEnvelope {
    version: String,
}

#[derive(Deserialize)]
struct TagsEnvelope {
    models: Vec<TagRow>,
}

#[derive(Deserialize)]
struct TagRow {
    name: String,
    model: String,
    modified_at: String,
    size: u64,
    digest: String,
    details: TagDetails,
}

#[derive(Deserialize)]
struct TagDetails {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    families: Vec<String>,
    #[serde(default)]
    parameter_size: Option<String>,
    #[serde(default)]
    quantization_level: Option<String>,
}

#[derive(Deserialize)]
struct RunningEnvelope {
    models: Vec<RunningRow>,
}

#[derive(Deserialize)]
struct RunningRow {
    name: String,
    model: String,
    size: u64,
    digest: String,
    expires_at: String,
    size_vram: u64,
    context_length: u64,
}

#[derive(Deserialize)]
struct ShowEnvelope {
    modified_at: String,
    capabilities: Vec<String>,
    model_info: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct OpenAiEnvelope {
    object: String,
    data: Vec<OpenAiModelRow>,
}

#[derive(Deserialize)]
struct OpenAiModelRow {
    id: String,
    object: String,
    #[serde(default)]
    created: Option<u64>,
    #[serde(default)]
    owned_by: Option<String>,
}

struct InspectionSnapshot {
    version: String,
    models: Vec<OllamaPickerModel>,
    non_chat: BTreeSet<String>,
}

async fn inspect_snapshot(
    http: &HttpService,
    endpoint: &OllamaEndpoint,
    cancellation: CancellationToken,
) -> Result<InspectionSnapshot, OllamaReadinessError> {
    tokio::time::timeout(
        DEFAULT_TIMEOUT,
        inspect_snapshot_within_budget(http, endpoint, cancellation),
    )
    .await
    .map_err(|_| OllamaReadinessError::Unavailable)?
}

async fn inspect_snapshot_within_budget(
    http: &HttpService,
    endpoint: &OllamaEndpoint,
    cancellation: CancellationToken,
) -> Result<InspectionSnapshot, OllamaReadinessError> {
    let version: VersionEnvelope = decode(
        &send(
            http,
            endpoint.version_url(),
            cancellation.child_token(),
            DEFAULT_TIMEOUT,
        )
        .await?,
    )
    .map_err(|_| OllamaReadinessError::UnrecognizedVersion)?;
    if !safe_text(&version.version, 64) {
        return Err(OllamaReadinessError::UnrecognizedVersion);
    }
    let native = parse_tags(
        &send(
            http,
            endpoint.tags_url(),
            cancellation.child_token(),
            DEFAULT_TIMEOUT,
        )
        .await?,
    )?;
    if native.len() > MAX_PICKER_MODELS {
        return Err(OllamaReadinessError::InvalidNativeCatalog);
    }
    let running = parse_running(
        &send(
            http,
            endpoint.running_url(),
            cancellation.child_token(),
            DEFAULT_TIMEOUT,
        )
        .await?,
    )?;
    let openai = parse_openai_models(
        &send(
            http,
            endpoint.openai_models_url(),
            cancellation.child_token(),
            DEFAULT_TIMEOUT,
        )
        .await?,
    )?;

    let mut models = Vec::new();
    let mut non_chat = BTreeSet::new();
    for record in native {
        if !openai.contains(&record.name) {
            continue;
        }
        let evidence = parse_show(
            &send_show(
                http,
                endpoint.show_url(),
                &record.name,
                cancellation.child_token(),
            )
            .await?,
            &record,
        )?;
        if !evidence
            .capabilities
            .iter()
            .any(|capability| capability == "completion")
        {
            non_chat.insert(record.name);
            continue;
        }
        let loaded = running.get(&record.name).cloned();
        if loaded
            .as_ref()
            .is_some_and(|running| running.model != record.model || running.digest != record.digest)
        {
            return Err(OllamaReadinessError::InvalidNativeCatalog);
        }
        models.push(OllamaPickerModel {
            record,
            capabilities: evidence.capabilities,
            context_window: evidence.context_window,
            running: loaded,
        });
    }
    if cancellation.is_cancelled() {
        return Err(OllamaReadinessError::Cancelled);
    }
    Ok(InspectionSnapshot {
        version: version.version,
        models,
        non_chat,
    })
}

struct ShowEvidence {
    capabilities: Vec<String>,
    context_window: Option<u64>,
}

async fn send_show(
    http: &HttpService,
    url: &str,
    model: &str,
    cancellation: CancellationToken,
) -> Result<HttpResponse, OllamaReadinessError> {
    let body = serde_json::to_vec(&serde_json::json!({"model":model,"verbose":false}))
        .map_err(|_| OllamaReadinessError::InvalidModel)?;
    let request = HttpRequest::post(url, body)
        .and_then(|request| request.header("accept", "application/json"))
        .and_then(|request| request.header("content-type", "application/json"))
        .map(|request| request.with_max_response_bytes(RESPONSE_LIMIT))
        .map_err(|_| OllamaReadinessError::InvalidEndpoint)?;
    send_request(http, request, cancellation, DEFAULT_TIMEOUT).await
}

async fn send(
    http: &HttpService,
    url: &str,
    cancellation: CancellationToken,
    timeout: Duration,
) -> Result<HttpResponse, OllamaReadinessError> {
    let request = HttpRequest::get(url)
        .and_then(|request| request.header("accept", "application/json"))
        .map(|request| request.with_max_response_bytes(RESPONSE_LIMIT))
        .map_err(|_| OllamaReadinessError::InvalidEndpoint)?;
    send_request(http, request, cancellation, timeout).await
}

async fn send_request(
    http: &HttpService,
    request: HttpRequest,
    cancellation: CancellationToken,
    timeout: Duration,
) -> Result<HttpResponse, OllamaReadinessError> {
    if cancellation.is_cancelled() {
        return Err(OllamaReadinessError::Cancelled);
    }
    let response = tokio::time::timeout(timeout, http.send(request, cancellation.clone()))
        .await
        .map_err(|_| OllamaReadinessError::Unavailable)?
        .map_err(map_transport)?;
    if cancellation.is_cancelled() {
        return Err(OllamaReadinessError::Cancelled);
    }
    if !(200..=299).contains(&response.status) {
        if matches!(response.status, 401 | 403) {
            return Err(OllamaReadinessError::Unauthorized);
        }
        return Err(OllamaReadinessError::Unavailable);
    }
    Ok(response)
}

fn decode<T: for<'de> Deserialize<'de>>(
    response: &HttpResponse,
) -> Result<T, OllamaReadinessError> {
    if !response
        .content_type
        .as_deref()
        .is_some_and(|value| value == "application/json" || value.ends_with("+json"))
    {
        return Err(OllamaReadinessError::InvalidNativeCatalog);
    }
    serde_json::from_slice(&response.body).map_err(|_| OllamaReadinessError::InvalidNativeCatalog)
}

fn parse_tags(response: &HttpResponse) -> Result<Vec<OllamaModelRecord>, OllamaReadinessError> {
    let envelope: TagsEnvelope =
        decode(response).map_err(|_| OllamaReadinessError::InvalidNativeCatalog)?;
    let mut seen = BTreeSet::new();
    let mut records = Vec::with_capacity(envelope.models.len());
    for row in envelope.models {
        if !safe_text(&row.name, 256)
            || !safe_text(&row.model, 256)
            || !valid_timestamp(&row.modified_at)
            || row.size == 0
            || row.digest.len() != 64
            || !row.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !seen.insert(row.name.clone())
            || !optional_safe(&row.details.format)
            || !optional_safe(&row.details.family)
            || !optional_safe(&row.details.parameter_size)
            || !optional_safe(&row.details.quantization_level)
            || row
                .details
                .families
                .iter()
                .any(|family| !safe_text(family, 128))
        {
            return Err(OllamaReadinessError::InvalidNativeCatalog);
        }
        records.push(OllamaModelRecord {
            name: row.name,
            model: row.model,
            modified_at: row.modified_at,
            size: row.size,
            digest: row.digest,
            details: OllamaModelDetails {
                format: row.details.format,
                family: row.details.family,
                families: row.details.families,
                parameter_size: row.details.parameter_size,
                quantization_level: row.details.quantization_level,
            },
        });
    }
    Ok(records)
}

fn parse_running(
    response: &HttpResponse,
) -> Result<BTreeMap<String, OllamaRunningModel>, OllamaReadinessError> {
    let envelope: RunningEnvelope =
        decode(response).map_err(|_| OllamaReadinessError::InvalidNativeCatalog)?;
    if envelope.models.len() > MAX_PICKER_MODELS {
        return Err(OllamaReadinessError::InvalidNativeCatalog);
    }
    let mut running = BTreeMap::new();
    for row in envelope.models {
        if !safe_text(&row.name, 256)
            || !safe_text(&row.model, 256)
            || row.size == 0
            || row.digest.len() != 64
            || !row.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !valid_timestamp(&row.expires_at)
            || row.size_vram == 0
            || row.context_length == 0
            || running.contains_key(&row.name)
        {
            return Err(OllamaReadinessError::InvalidNativeCatalog);
        }
        running.insert(
            row.name,
            OllamaRunningModel {
                model: row.model,
                digest: row.digest,
                expires_at: row.expires_at,
                size_vram: row.size_vram,
                context_length: row.context_length,
            },
        );
    }
    Ok(running)
}

fn parse_show(
    response: &HttpResponse,
    record: &OllamaModelRecord,
) -> Result<ShowEvidence, OllamaReadinessError> {
    let envelope: ShowEnvelope =
        decode(response).map_err(|_| OllamaReadinessError::InvalidNativeCatalog)?;
    if envelope.modified_at != record.modified_at
        || envelope.capabilities.is_empty()
        || envelope.capabilities.len() > 32
    {
        return Err(OllamaReadinessError::InvalidNativeCatalog);
    }
    let mut seen = BTreeSet::new();
    for capability in &envelope.capabilities {
        if !safe_text(capability, 64) || !seen.insert(capability.as_str()) {
            return Err(OllamaReadinessError::InvalidNativeCatalog);
        }
    }
    let architecture = envelope
        .model_info
        .get("general.architecture")
        .and_then(serde_json::Value::as_str)
        .filter(|value| safe_text(value, 128));
    let context_window = architecture
        .and_then(|architecture| {
            envelope
                .model_info
                .get(&format!("{architecture}.context_length"))
        })
        .and_then(serde_json::Value::as_u64);
    if context_window == Some(0) {
        return Err(OllamaReadinessError::InvalidNativeCatalog);
    }
    Ok(ShowEvidence {
        capabilities: envelope.capabilities,
        context_window,
    })
}

fn parse_openai_models(response: &HttpResponse) -> Result<BTreeSet<String>, OllamaReadinessError> {
    let envelope: OpenAiEnvelope =
        decode(response).map_err(|_| OllamaReadinessError::InvalidOpenAiCatalog)?;
    if envelope.object != "list" {
        return Err(OllamaReadinessError::InvalidOpenAiCatalog);
    }
    let mut ids = BTreeSet::new();
    for row in envelope.data {
        if !safe_text(&row.id, 256)
            || row.object != "model"
            || row.created == Some(0)
            || row
                .owned_by
                .as_ref()
                .is_some_and(|owner| !safe_text(owner, 128))
            || !ids.insert(row.id)
        {
            return Err(OllamaReadinessError::InvalidOpenAiCatalog);
        }
    }
    Ok(ids)
}

fn safe_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
}

fn optional_safe(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(|value| safe_text(value, 128))
}

fn valid_timestamp(value: &str) -> bool {
    value.len() <= 128 && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

fn listed_support(capabilities: &[String], expected: &str) -> CapabilitySupport {
    if capabilities.iter().any(|capability| capability == expected) {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unsupported
    }
}

fn map_transport(error: TransportError) -> OllamaReadinessError {
    match error {
        TransportError::Cancelled => OllamaReadinessError::Cancelled,
        _ => OllamaReadinessError::Unavailable,
    }
}

fn readiness_to_catalog(error: OllamaReadinessError) -> CatalogFetchError {
    match error {
        OllamaReadinessError::Unauthorized => CatalogFetchError::new(
            CatalogFailureKind::Unauthorized,
            "Ollama server requires a valid API key",
        ),
        OllamaReadinessError::Cancelled => CatalogFetchError::cancelled(),
        OllamaReadinessError::Unavailable => {
            CatalogFetchError::new(CatalogFailureKind::Network, "Ollama catalog is unavailable")
        }
        _ => CatalogFetchError::new(
            CatalogFailureKind::InvalidResponse,
            "Ollama catalog response is invalid",
        ),
    }
}

fn inference_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: OLLAMA_PROVIDER.to_owned(),
        display_name: OLLAMA_DISPLAY_NAME.to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

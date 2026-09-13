//! Native LM Studio model list and capability mapping.
//!
//! PLM02 reads the documented native `GET /api/v1/models` list. That endpoint
//! enumerates the models present in the local library, so every row is
//! *downloaded*; a row whose `loaded_instances` is non-empty is additionally
//! *loaded*. Neither state is availability: LM Studio loads a downloaded model
//! on demand, so a downloaded-but-not-loaded model is usable, never missing.
//!
//! Capability mapping keeps three distinct facts apart. LM Studio publishes
//! explicit negatives (`"vision": false`), which are real evidence of absence
//! and become `Unsupported`. An absent key is not a negative and stays
//! `Unknown`. And an absent `capabilities` object — which is what an embedding
//! model has — makes every capability `Unknown`. `Unknown` is never promoted to
//! `Supported`.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ProviderProtocol, ServiceKey,
};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, CatalogRegistry, ModelCapabilities,
    ModelCatalog, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing,
    ProviderDescriptor, SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::config::LmStudioConfig;
use crate::endpoint::{LM_STUDIO_DISPLAY_NAME, LM_STUDIO_PROVIDER, LmStudioAuth, LmStudioSurface};

/// Service key for the LM Studio local model list.
///
/// The shared `"models"` catalog carries routing-shaped [`ModelDescriptor`]s;
/// this key carries the LM Studio-owned records that additionally state local
/// load state, which the shared descriptor has no field for.
pub const SERVICE_LM_STUDIO_MODELS: ServiceKey = ServiceKey::new("lmstudio/models");

/// Response cap for one model list. A large local library with many loaded
/// instances is still metadata, not model weights.
const MODELS_RESPONSE_LIMIT: usize = 4 * 1024 * 1024;

/// Which kind of model one row describes.
///
/// The v1 list documents `"llm" | "embedding"`. The legacy v0 list also emitted
/// `"vlm"`, which v1 folds into an `"llm"` carrying `capabilities.vision`, so an
/// unrecognized value is retained and shown rather than dropped or treated as
/// chat-capable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LmStudioModelKind {
    /// A language model: `"llm"`.
    Llm,
    /// An embedding model: `"embedding"`.
    Embedding,
    /// A `type` this crate does not recognize, retained verbatim.
    Unrecognized(String),
}

impl LmStudioModelKind {
    fn parse(value: &str) -> Self {
        match value {
            "llm" => Self::Llm,
            "embedding" => Self::Embedding,
            other => Self::Unrecognized(other.to_owned()),
        }
    }
}

/// Local availability of one model in the LM Studio library.
///
/// Both states mean the weights are on disk. Neither means unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LmStudioModelState {
    /// In the local library with no loaded instance. LM Studio can load it on
    /// demand, so this is a ready model, not a missing one.
    Downloaded,
    /// At least one instance is loaded in memory.
    Loaded,
}

/// One loaded instance of a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioLoadedInstance {
    /// Instance identifier.
    pub id: String,
    /// Context length this instance was loaded with.
    ///
    /// This is the running configuration and is routinely smaller than the
    /// model's [`LmStudioModelRecord::max_context_length`] — the documented
    /// example loads a 262,144-token model at 4,096 — so the two are kept
    /// separate and neither is used in place of the other.
    pub context_length: Option<u64>,
}

/// Published quantization of one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioQuantization {
    /// Quantization name, such as `Q4_K_M`.
    pub name: Option<String>,
    /// Bits per weight.
    pub bits_per_weight: Option<u32>,
}

/// One model in the local LM Studio library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioModelRecord {
    /// Model identifier used on the wire.
    pub key: String,
    /// Human display name, falling back to [`Self::key`].
    pub display_name: String,
    /// Which kind of model this row describes.
    pub kind: LmStudioModelKind,
    /// Publisher, when published.
    pub publisher: Option<String>,
    /// Architecture, absent for embedding models.
    pub architecture: Option<String>,
    /// Quantization, when published.
    pub quantization: Option<LmStudioQuantization>,
    /// On-disk size in bytes, when published.
    pub size_bytes: Option<u64>,
    /// Parameter count string such as `26B-A4B`, when published.
    pub params_string: Option<String>,
    /// File format such as `gguf` or `mlx`, when published.
    pub format: Option<String>,
    /// Context length the model architecture supports, when published.
    pub max_context_length: Option<u64>,
    /// Every currently loaded instance. Empty means downloaded only.
    pub loaded_instances: Vec<LmStudioLoadedInstance>,
    /// Whether the model was trained for tool use.
    ///
    /// This is the field PLM03 uses to keep a chat-only model out of agent
    /// mode, so it is evidence-backed in both directions: an explicit
    /// `"trained_for_tool_use": false` is `Unsupported`, an absent key or an
    /// absent `capabilities` object is `Unknown`, and `Unknown` never becomes
    /// `Supported`.
    pub tool_trained: CapabilitySupport,
    /// Whether the model accepts image input.
    pub vision: CapabilitySupport,
    /// Whether the model exposes a reasoning control.
    pub reasoning: CapabilitySupport,
    /// Reasoning options the model accepts, when it publishes a control.
    pub reasoning_options: Vec<String>,
}

impl LmStudioModelRecord {
    /// Local availability, derived from [`Self::loaded_instances`].
    #[must_use]
    pub fn state(&self) -> LmStudioModelState {
        if self.loaded_instances.is_empty() {
            LmStudioModelState::Downloaded
        } else {
            LmStudioModelState::Loaded
        }
    }

    /// Whether at least one instance is loaded in memory.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.state() == LmStudioModelState::Loaded
    }

    /// Whether this row can be routed as a chat model.
    ///
    /// Only a recognized `"llm"` qualifies: an embedding model cannot serve a
    /// chat turn, and an unrecognized kind is not proven to.
    #[must_use]
    pub fn is_chat_model(&self) -> bool {
        self.kind == LmStudioModelKind::Llm
    }

    /// Tri-state capability snapshot for this model.
    ///
    /// LM Studio publishes evidence for exactly three capabilities. Every other
    /// component stays `Unknown` because the endpoint publishes nothing about
    /// it — protocol compatibility is not capability evidence (GOTCHAS #22).
    #[must_use]
    pub fn capabilities(&self) -> ModelCapabilities {
        let mut capabilities = ModelCapabilities::unknown();
        capabilities.tools = self.tool_trained;
        capabilities.image_input = self.vision;
        capabilities.reasoning = self.reasoning;
        capabilities
    }

    /// Routing-shaped descriptor for the shared catalog.
    ///
    /// `max_context_length` is the architecture's window, never a loaded
    /// instance's configured one. LM Studio publishes no output cap, no
    /// lifecycle, no price and no performance evidence, so those stay unknown
    /// rather than being invented — and unknown is not zero.
    #[must_use]
    pub fn descriptor(&self) -> ModelDescriptor {
        ModelDescriptor {
            id: self.key.clone(),
            display_name: self.display_name.clone(),
            aliases: Vec::new(),
            created_at_ms: None,
            context_window: self.max_context_length,
            max_output_tokens: None,
            lifecycle: ModelLifecycle::unknown(),
            capabilities: self.capabilities(),
            pricing: ModelPricing::unknown(),
            performance: ModelPerformance::unknown(),
            reasoning: None,
        }
    }
}

/// Reads the native LM Studio model list.
#[derive(Clone)]
pub struct LmStudioCatalog {
    http: HttpService,
    credentials: Option<Arc<CredentialsService>>,
    config: LmStudioConfig,
}

impl LmStudioCatalog {
    /// Build a catalog reader for one configured endpoint.
    #[must_use]
    pub const fn new(
        http: HttpService,
        credentials: Option<Arc<CredentialsService>>,
        config: LmStudioConfig,
    ) -> Self {
        Self {
            http,
            credentials,
            config,
        }
    }

    /// Configuration this reader uses.
    #[must_use]
    pub const fn config(&self) -> &LmStudioConfig {
        &self.config
    }

    /// Read the complete local model library.
    ///
    /// One all-or-nothing generation: a malformed envelope, a row without a
    /// usable identity, or a duplicate identity rejects the whole list rather
    /// than publishing a partial library.
    ///
    /// # Errors
    /// Returns a classified unauthorized/unavailable/network/decoding failure.
    /// Diagnostics never carry response bytes.
    pub async fn list_models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<LmStudioModelRecord>, CatalogFetchError> {
        self.list_models_authorized(self.authorization(), cancellation)
            .await
    }

    async fn list_models_authorized(
        &self,
        authorization: Option<String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<LmStudioModelRecord>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let url = self.config.endpoint().url(LmStudioSurface::NativeRestV1);
        let mut request =
            HttpRequest::get(url).and_then(|request| request.header("accept", "application/json"));
        if let Some(value) = authorization {
            request = request.and_then(|request| request.header("authorization", &value));
        }
        let request = request
            .map(|request| request.with_max_response_bytes(MODELS_RESPONSE_LIMIT))
            .map_err(|_| invalid_response("LM Studio model request could not be constructed"))?;
        // The registry supplies cancellation but no deadline, so the deadline is
        // this reader's own: a hung local server must not stall a refresh.
        let response = match tokio::time::timeout(
            self.config.catalog_timeout(),
            self.http.send(request, cancellation.clone()),
        )
        .await
        {
            Err(_elapsed) => {
                return Err(CatalogFetchError::new(
                    CatalogFailureKind::Network,
                    "LM Studio model list timed out",
                ));
            }
            Ok(result) => result.map_err(map_transport_error)?,
        };
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        classify_status(&response)?;
        parse_models(&response)
    }

    fn authorization(&self) -> Option<String> {
        let LmStudioAuth::BearerToken(query) = self.config.auth() else {
            return None;
        };
        let secret = self.credentials.as_ref()?.resolve(query).ok().flatten()?;
        Some(format!("Bearer {}", secret.expose()))
    }
}

impl std::fmt::Debug for LmStudioCatalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LmStudioCatalog")
            .field("base_url", &self.config.endpoint().base_url())
            .field(
                "authenticated",
                &matches!(self.config.auth(), LmStudioAuth::BearerToken(_)),
            )
            .finish()
    }
}

#[async_trait]
impl ModelCatalog for LmStudioCatalog {
    /// Identity only.
    ///
    /// The model list proves the native REST surface, not which inference
    /// protocol this server speaks, so protocols stay `Unknown` here. PLM01's
    /// `LmStudioDetector` is what proves a protocol, and a consumer joins the
    /// two rather than this source guessing.
    fn provider(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: LM_STUDIO_PROVIDER.to_owned(),
            display_name: LM_STUDIO_DISPLAY_NAME.to_owned(),
            protocols: vec![ProviderProtocol::Unknown],
        }
    }

    /// Routing-shaped generation.
    ///
    /// Only chat models reach the shared catalog: offering an embedding model
    /// as a chat route would be a real defect. The excluded rows stay visible
    /// through [`LmStudioCatalog::list_models`].
    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        Ok(self
            .list_models(cancellation)
            .await?
            .iter()
            .filter(|record| record.is_chat_model())
            .map(LmStudioModelRecord::descriptor)
            .collect())
    }
}

struct LoadedModelCatalog(LmStudioCatalog);

#[async_trait]
impl ModelCatalog for LoadedModelCatalog {
    fn supports_endpoint_credentials(&self) -> bool {
        true
    }
    async fn fetch_endpoint(
        &self,
        endpoint: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        self.fetch_endpoint_with_credential(endpoint, None, cancellation)
            .await
    }

    async fn fetch_endpoint_with_credential(
        &self,
        endpoint: &str,
        credential: Option<&heycode_credentials::CredentialSecret>,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let endpoint = crate::LmStudioEndpoint::new(endpoint).map_err(|_| {
            CatalogFetchError::new(
                CatalogFailureKind::InvalidResponse,
                "Enter an HTTP(S) LM Studio server origin",
            )
        })?;
        let source = Self(LmStudioCatalog::new(
            self.0.http.clone(),
            None,
            LmStudioConfig::local().with_endpoint(endpoint),
        ));
        let records = source
            .0
            .list_models_authorized(
                credential.map(|credential| format!("Bearer {}", credential.expose())),
                cancellation,
            )
            .await?;
        Ok(loaded_descriptors(&records))
    }

    fn provider(&self) -> ProviderDescriptor {
        self.0.provider()
    }
    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let records = self.0.list_models(cancellation).await?;
        Ok(loaded_descriptors(&records))
    }
}

fn loaded_descriptors(records: &[LmStudioModelRecord]) -> Vec<ModelDescriptor> {
    let mut models = Vec::new();
    for record in records.iter().filter(|record| record.is_chat_model()) {
        for instance in &record.loaded_instances {
            let mut model = record.descriptor();
            model.id = instance.id.clone();
            model.display_name = format!("{} (loaded)", record.display_name);
            model.context_window = instance.context_length;
            models.push(model);
        }
    }
    models
}

/// Register the LM Studio model list into the shared catalog registry and
/// publish the local-state records.
#[must_use]
pub fn lmstudio_catalog_plugin(config: LmStudioConfig) -> Box<dyn Plugin> {
    struct LmStudioCatalogPlugin(LmStudioConfig);

    impl Plugin for LmStudioCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-lmstudio"
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
            &[SERVICE_LM_STUDIO_MODELS]
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            vec![PluginContributionSpec::new(
                ContributionKind::ModelCatalog,
                LM_STUDIO_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            match self.0.auth() {
                LmStudioAuth::None => &[SERVICE_MODELS, SERVICE_HTTP],
                LmStudioAuth::BearerToken(_) => {
                    &[SERVICE_MODELS, SERVICE_HTTP, SERVICE_CREDENTIALS]
                }
            }
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = match self.0.auth() {
                LmStudioAuth::None => None,
                LmStudioAuth::BearerToken(_) => Some(
                    context
                        .get::<CredentialsService>(SERVICE_CREDENTIALS)
                        .ok_or_else(|| {
                            CoreError::MissingService(SERVICE_CREDENTIALS.to_string())
                        })?,
                ),
            };
            let catalog = LmStudioCatalog::new(http.as_ref().clone(), credentials, self.0.clone());
            // The registry is shared state this plugin did not create, so the
            // row is effect-owned and disappears with the plugin.
            models
                .register(context, Arc::new(LoadedModelCatalog(catalog.clone())))
                .map_err(|error| CoreError::other(error.to_string()))?;
            // The bare value, never an `Arc`: `get::<LmStudioCatalog>` would
            // answer `None` for `Arc<Arc<_>>` (GOTCHAS #27).
            context.provide(SERVICE_LM_STUDIO_MODELS, self.name(), catalog)
        }
    }

    Box::new(LmStudioCatalogPlugin(config))
}

#[derive(Deserialize)]
struct ModelsEnvelope {
    models: Vec<ModelRow>,
}

#[derive(Deserialize)]
struct ModelRow {
    #[serde(rename = "type")]
    kind: String,
    key: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    publisher: Option<String>,
    #[serde(default)]
    architecture: Option<String>,
    #[serde(default)]
    quantization: Option<QuantizationRow>,
    #[serde(default)]
    size_bytes: Option<u64>,
    #[serde(default)]
    params_string: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    max_context_length: Option<u64>,
    #[serde(default)]
    loaded_instances: Vec<InstanceRow>,
    #[serde(default)]
    capabilities: Option<CapabilitiesRow>,
}

#[derive(Deserialize)]
struct QuantizationRow {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    bits_per_weight: Option<u32>,
}

#[derive(Deserialize)]
struct InstanceRow {
    id: String,
    #[serde(default)]
    config: Option<InstanceConfigRow>,
}

#[derive(Deserialize)]
struct InstanceConfigRow {
    #[serde(default)]
    context_length: Option<u64>,
}

/// `capabilities` is absent for embedding models, and `reasoning` is an object
/// rather than a boolean.
#[derive(Deserialize)]
struct CapabilitiesRow {
    #[serde(default)]
    vision: Option<bool>,
    #[serde(default)]
    trained_for_tool_use: Option<bool>,
    #[serde(default)]
    reasoning: Option<ReasoningRow>,
}

#[derive(Deserialize)]
struct ReasoningRow {
    #[serde(default)]
    allowed_options: Vec<String>,
}

/// An explicit boolean is evidence in both directions; an absent one is not.
fn support(value: Option<bool>) -> CapabilitySupport {
    match value {
        Some(true) => CapabilitySupport::Supported,
        Some(false) => CapabilitySupport::Unsupported,
        None => CapabilitySupport::Unknown,
    }
}

fn parse_models(response: &HttpResponse) -> Result<Vec<LmStudioModelRecord>, CatalogFetchError> {
    if !response
        .content_type
        .as_deref()
        .is_some_and(|value| value == "application/json" || value.ends_with("+json"))
    {
        return Err(invalid_response("LM Studio model list is not JSON"));
    }
    let envelope: ModelsEnvelope = serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("LM Studio model list has an invalid JSON shape"))?;
    let mut seen = BTreeSet::new();
    let mut records = Vec::with_capacity(envelope.models.len());
    for row in envelope.models {
        let key = row.key.trim().to_owned();
        if key.is_empty() || !seen.insert(key.clone()) {
            return Err(invalid_response(
                "LM Studio model list contains a blank or duplicate model key",
            ));
        }
        let display_name = row
            .display_name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| key.clone());
        let capabilities = row.capabilities;
        let reasoning_options = capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.reasoning.as_ref())
            .map(|reasoning| reasoning.allowed_options.clone())
            .unwrap_or_default();
        records.push(LmStudioModelRecord {
            kind: LmStudioModelKind::parse(&row.kind),
            key,
            display_name,
            publisher: row.publisher,
            architecture: row.architecture,
            quantization: row.quantization.map(|quantization| LmStudioQuantization {
                name: quantization.name,
                bits_per_weight: quantization.bits_per_weight,
            }),
            size_bytes: row.size_bytes,
            params_string: row.params_string,
            format: row.format,
            max_context_length: row.max_context_length,
            loaded_instances: row
                .loaded_instances
                .into_iter()
                .map(|instance| LmStudioLoadedInstance {
                    id: instance.id,
                    context_length: instance.config.and_then(|config| config.context_length),
                })
                .collect(),
            tool_trained: support(
                capabilities
                    .as_ref()
                    .and_then(|capabilities| capabilities.trained_for_tool_use),
            ),
            vision: support(
                capabilities
                    .as_ref()
                    .and_then(|capabilities| capabilities.vision),
            ),
            // A published reasoning control is evidence of support. Its absence
            // is not documented as a denial, so it stays Unknown.
            reasoning: capabilities
                .as_ref()
                .and_then(|capabilities| capabilities.reasoning.as_ref())
                .map_or(CapabilitySupport::Unknown, |_| CapabilitySupport::Supported),
            reasoning_options,
        });
    }
    Ok(records)
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized()),
        429 | 500..=599 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "LM Studio model list is temporarily unavailable",
        )),
        _ => Err(invalid_response(
            "LM Studio model list returned an unexpected HTTP status",
        )),
    }
}

fn map_transport_error(error: TransportError) -> CatalogFetchError {
    match error {
        TransportError::Cancelled => CatalogFetchError::cancelled(),
        TransportError::Http {
            status: 401 | 403, ..
        } => unauthorized(),
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            CatalogFetchError::new(
                CatalogFailureKind::Unavailable,
                "LM Studio model list is temporarily unavailable",
            )
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "LM Studio model list request failed",
        ),
        _ => invalid_response("LM Studio model list transport response is invalid"),
    }
}

fn unauthorized() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        "LM Studio credential is missing or unauthorized",
    )
}

fn invalid_response(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

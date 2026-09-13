//! OpenRouter public model-catalog discovery and normalization.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate};
use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor, ProviderProtocol,
};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, CatalogRegistry, ModelCapabilities,
    ModelCatalog, ModelDescriptor, ModelLifecycle, ModelReasoningMetadata, ProviderDescriptor,
    SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// Current OpenRouter route for Z.ai GLM-5.3-Flash.
pub const OPENROUTER_GLM_5_3_FLASH: &str = "z-ai/glm-5.3-flash";

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const CATALOG_RESPONSE_LIMIT: usize = 8 * 1024 * 1024;

/// Configuration for the OpenRouter catalog contribution.
#[derive(Clone)]
pub struct OpenRouterCatalogConfig {
    base_url: String,
}

impl OpenRouterCatalogConfig {
    /// Use the official OpenRouter API origin.
    #[must_use]
    pub fn official() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Override the API base URL for a contract-test endpoint.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

/// Provider-owned OpenRouter model catalog source.
pub struct OpenRouterCatalog {
    http: HttpService,
    models_url: String,
    glm_url: String,
}

impl OpenRouterCatalog {
    /// Build against the official OpenRouter catalog endpoints.
    ///
    /// # Errors
    /// Invalid fixed endpoint construction fails before publication.
    pub fn new(http: HttpService) -> Result<Self, CatalogFetchError> {
        Self::with_base_url(http, DEFAULT_BASE_URL)
    }

    /// Build against an explicit OpenRouter-compatible base URL.
    ///
    /// # Errors
    /// Blank or invalid HTTP(S) URLs fail before publication.
    pub fn with_base_url(
        http: HttpService,
        base_url: impl AsRef<str>,
    ) -> Result<Self, CatalogFetchError> {
        let base_url = base_url.as_ref().trim_end_matches('/');
        let models_url = format!("{base_url}/models");
        let glm_url = format!("{base_url}/model/{OPENROUTER_GLM_5_3_FLASH}");
        for url in [&models_url, &glm_url] {
            HttpRequest::get(url)
                .map_err(|_| invalid_response("OpenRouter catalog base URL is invalid"))?;
        }
        Ok(Self {
            http,
            models_url,
            glm_url,
        })
    }

    async fn request(
        &self,
        url: &str,
        cancellation: CancellationToken,
    ) -> Result<HttpResponse, CatalogFetchError> {
        let request = HttpRequest::get(url)
            .and_then(|request| request.header("accept", "application/json"))
            .map(|request| request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT))
            .map_err(|_| invalid_response("OpenRouter catalog request could not be constructed"))?;
        let response = self
            .http
            .send(request, cancellation)
            .await
            .map_err(map_transport_error)?;
        classify_status(&response)?;
        Ok(response)
    }
}

#[async_trait]
impl ModelCatalog for OpenRouterCatalog {
    fn provider(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let full = self.request(&self.models_url, cancellation.clone()).await?;
        let mut rows = parse_full(full)?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let detail = self.request(&self.glm_url, cancellation.clone()).await?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }

        let detail = parse_single(detail)?;
        if detail.id != OPENROUTER_GLM_5_3_FLASH {
            return Err(invalid_response(
                "OpenRouter single-model response identity does not match the requested model",
            ));
        }
        let Some(list_row) = rows.get(OPENROUTER_GLM_5_3_FLASH) else {
            return Err(invalid_response(
                "OpenRouter full catalog omitted the verified default model",
            ));
        };
        if list_row != &detail {
            return Err(invalid_response(
                "OpenRouter full and single-model catalog rows disagree",
            ));
        }
        rows.insert(detail.id.clone(), detail);
        let provenance =
            heycode_llm::ModelMetadataProvenance::new("openrouter:models-api", capture_time_ms()?)
                .map_err(|_| invalid_response("OpenRouter catalog capture metadata is invalid"))?;
        rows.into_values()
            .map(|row| normalize_model(row, &provenance))
            .collect()
    }
}

fn capture_time_ms() -> Result<u64, CatalogFetchError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .filter(|captured| *captured != 0)
        .ok_or_else(|| invalid_response("OpenRouter catalog capture time is unavailable"))
}

/// Register OpenRouter discovery into the shared model registry.
#[must_use]
pub fn openrouter_catalog_plugin(config: OpenRouterCatalogConfig) -> Box<dyn Plugin> {
    struct OpenRouterCatalogPlugin(OpenRouterCatalogConfig);

    impl Plugin for OpenRouterCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-openrouter"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::ModelCatalog,
                "openrouter",
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
            let source = OpenRouterCatalog::with_base_url(http.as_ref().clone(), &self.0.base_url)
                .map_err(|error| CoreError::other(error.to_string()))?;
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(OpenRouterCatalogPlugin(config))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FullEnvelope {
    data: Vec<ModelRow>,
    total_count: usize,
    links: PageLinks,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct SingleEnvelope {
    data: ModelRow,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct PageLinks {
    next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ModelRow {
    id: String,
    canonical_slug: String,
    name: String,
    created: u64,
    description: String,
    context_length: u64,
    architecture: Architecture,
    pricing: Pricing,
    top_provider: TopProvider,
    supported_parameters: Vec<String>,
    expiration_date: Option<String>,
    #[serde(default)]
    reasoning: Option<ReasoningMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ReasoningMetadata {
    #[serde(default)]
    supported_efforts: Option<Vec<String>>,
    #[serde(default)]
    default_effort: Option<String>,
    #[serde(default)]
    default_enabled: Option<bool>,
    #[serde(default)]
    mandatory: Option<bool>,
}

impl ReasoningMetadata {
    /// Project the published block into the neutral descriptor vocabulary.
    /// `supported_efforts`, `default_effort`, `default_enabled` and
    /// `mandatory` are carried verbatim, in published order.
    fn retain(&self) -> Result<ModelReasoningMetadata, CatalogFetchError> {
        ModelReasoningMetadata::published(
            self.supported_efforts.clone().unwrap_or_default(),
            self.default_effort.clone(),
            self.default_enabled,
            self.mandatory,
        )
        .map_err(|_| invalid_response("OpenRouter catalog contains invalid reasoning metadata"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Architecture {
    input_modalities: Vec<String>,
    output_modalities: Vec<String>,
}

/// The token-price components OpenRouter publishes as decimal USD-per-token
/// strings. Media and search components (`image`, `audio`, `web_search`, ...)
/// are deliberately absent: they are not per-token and the neutral vocabulary
/// has no component for them, so inventing one would misreport cost.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Pricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
    #[serde(default)]
    input_cache_read: Option<String>,
    #[serde(default)]
    input_cache_write: Option<String>,
    #[serde(default)]
    internal_reasoning: Option<String>,
}

impl Pricing {
    /// Exact per-token components in stable order.
    fn components(&self) -> [(heycode_llm::PriceComponent, Option<&str>); 5] {
        use heycode_llm::PriceComponent as Component;
        [
            (Component::Input, self.prompt.as_deref()),
            (Component::Output, self.completion.as_deref()),
            (Component::CachedInput, self.input_cache_read.as_deref()),
            (Component::CacheWrite, self.input_cache_write.as_deref()),
            (Component::Reasoning, self.internal_reasoning.as_deref()),
        ]
    }

    /// Normalize published prices into the neutral vocabulary.
    ///
    /// Every value is USD per token. An absent component stays absent — a
    /// missing price is unknown, never free.
    fn parsed_components(
        &self,
    ) -> Result<Vec<(heycode_llm::PriceComponent, heycode_llm::TokenPrice)>, CatalogFetchError>
    {
        let mut components = Vec::new();
        for (component, raw) in self.components() {
            let Some(raw) = raw else { continue };
            // OpenRouter routing models publish -1 when pricing depends on the
            // selected downstream model. Preserve unknown cost, never zero.
            if raw == "-1" {
                continue;
            }
            let price = match heycode_llm::TokenPrice::parse_decimal(
                heycode_llm::PriceCurrency::Usd,
                heycode_llm::TokenPriceUnit::PerToken,
                raw,
            ) {
                Ok(price) => price,
                // Do not round a published price into invented exact cost.
                Err(heycode_llm::PricingError::ExcessivePrecision) => continue,
                Err(_) => {
                    return Err(invalid_response(
                        "OpenRouter catalog contains invalid pricing",
                    ));
                }
            };
            components.push((component, price));
        }
        Ok(components)
    }

    fn normalize(
        &self,
        provenance: &heycode_llm::ModelMetadataProvenance,
    ) -> Result<heycode_llm::ModelPricing, CatalogFetchError> {
        let mut components = self.parsed_components()?.into_iter();
        let Some((component, price)) = components.next() else {
            return Ok(heycode_llm::ModelPricing::unknown());
        };
        let mut pricing = heycode_llm::ModelPricing::captured(provenance.clone(), component, price);
        for (component, price) in components {
            pricing = pricing
                .with(component, price)
                .map_err(|_| invalid_response("OpenRouter catalog contains invalid pricing"))?;
        }
        Ok(pricing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct TopProvider {
    context_length: Option<u64>,
    max_completion_tokens: Option<u64>,
}

fn parse_full(response: HttpResponse) -> Result<BTreeMap<String, ModelRow>, CatalogFetchError> {
    require_json(&response)?;
    let envelope: FullEnvelope = serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("OpenRouter full catalog has an invalid JSON shape"))?;
    if envelope.data.is_empty()
        || envelope.total_count != envelope.data.len()
        || envelope.links.next.is_some()
    {
        return Err(invalid_response(
            "OpenRouter full catalog is empty, partial, or count-mismatched",
        ));
    }
    let mut rows = BTreeMap::new();
    for row in envelope.data {
        validate_row(&row)?;
        if rows.insert(row.id.clone(), row).is_some() {
            return Err(invalid_response(
                "OpenRouter full catalog contains duplicate model ids",
            ));
        }
    }
    Ok(rows)
}

fn parse_single(response: HttpResponse) -> Result<ModelRow, CatalogFetchError> {
    require_json(&response)?;
    let envelope: SingleEnvelope = serde_json::from_slice(&response.body).map_err(|_| {
        invalid_response("OpenRouter single-model catalog has an invalid JSON shape")
    })?;
    validate_row(&envelope.data)?;
    Ok(envelope.data)
}

fn require_json(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    if response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    }) {
        Ok(())
    } else {
        Err(invalid_response("OpenRouter catalog response is not JSON"))
    }
}

fn validate_row(row: &ModelRow) -> Result<(), CatalogFetchError> {
    if !safe_text(&row.id, 256)
        || !row.id.contains('/')
        || !safe_text(&row.canonical_slug, 256)
        || !row.canonical_slug.contains('/')
        || !safe_display_text(&row.name, 256)
        || row.description.len() > 32 * 1024
        || row
            .description
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        || row.created == 0
        || row.context_length == 0
    {
        return Err(invalid_response(
            "OpenRouter catalog contains an invalid model identity or limit",
        ));
    }
    if row
        .top_provider
        .context_length
        .is_some_and(|value| value == 0 || value > row.context_length)
        || row
            .top_provider
            .max_completion_tokens
            .is_some_and(|value| value == 0)
    {
        return Err(invalid_response(
            "OpenRouter catalog contains invalid top-provider limits",
        ));
    }
    validate_unique_values(&row.architecture.input_modalities, "input modality", false)?;
    validate_unique_values(
        &row.architecture.output_modalities,
        "output modality",
        false,
    )?;
    if !row
        .architecture
        .output_modalities
        .iter()
        .any(|value| value == "text")
    {
        return Err(invalid_response(
            "OpenRouter text catalog contains a non-text model row",
        ));
    }
    validate_unique_values(&row.supported_parameters, "supported parameter", true)?;
    // Validate every published component before the generation is accepted, so
    // one malformed price rejects the row rather than silently vanishing.
    let _pricing = row.pricing.parsed_components()?;
    if let Some(reasoning) = row.reasoning.as_ref() {
        validate_reasoning(reasoning)?;
    }
    if row.id == OPENROUTER_GLM_5_3_FLASH {
        validate_glm_contract(row)?;
    }
    if let Some(expiration) = row.expiration_date.as_deref() {
        parse_expiration(expiration)?;
    }
    Ok(())
}

fn validate_reasoning(reasoning: &ReasoningMetadata) -> Result<(), CatalogFetchError> {
    if let Some(efforts) = reasoning.supported_efforts.as_ref() {
        validate_unique_values(efforts, "reasoning effort", false)?;
    }
    if reasoning
        .default_effort
        .as_deref()
        .is_some_and(|effort| !safe_text(effort, 32))
    {
        return Err(invalid_response(
            "OpenRouter catalog contains invalid reasoning metadata",
        ));
    }
    if reasoning.default_effort.is_some()
        && !reasoning
            .supported_efforts
            .as_deref()
            .is_some_and(|efforts| {
                efforts
                    .iter()
                    .any(|effort| Some(effort.as_str()) == reasoning.default_effort.as_deref())
            })
    {
        return Err(invalid_response(
            "OpenRouter catalog reasoning default is not a supported effort",
        ));
    }
    // The retained projection is the exact evidence other crates read, so a
    // block that cannot be retained rejects the generation here.
    reasoning.retain().map(|_| ())
}

fn validate_glm_contract(row: &ModelRow) -> Result<(), CatalogFetchError> {
    let reasoning = row.reasoning.as_ref().ok_or_else(|| {
        invalid_response("OpenRouter GLM catalog omitted mandatory reasoning metadata")
    })?;
    let exact_efforts = reasoning
        .supported_efforts
        .as_deref()
        .is_some_and(|efforts| {
            efforts
                .iter()
                .map(String::as_str)
                .eq(["max", "high", "low"])
        });
    if !exact_efforts
        || reasoning.default_effort.as_deref() != Some("max")
        || reasoning.default_enabled != Some(true)
        || reasoning.mandatory != Some(true)
    {
        return Err(invalid_response(
            "OpenRouter GLM reasoning metadata drifted from the strict route",
        ));
    }

    let parameters = row
        .supported_parameters
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if !["reasoning", "tools", "tool_choice"]
        .into_iter()
        .all(|parameter| parameters.contains(parameter))
    {
        return Err(invalid_response(
            "OpenRouter GLM catalog omitted a strict route parameter",
        ));
    }
    Ok(())
}

fn validate_unique_values(
    values: &[String],
    field: &'static str,
    allow_empty: bool,
) -> Result<(), CatalogFetchError> {
    let mut seen = BTreeSet::new();
    if (!allow_empty && values.is_empty())
        || values
            .iter()
            .any(|value| !safe_text(value, 128) || !seen.insert(value.as_str()))
    {
        return Err(invalid_response(match field {
            "input modality" => "OpenRouter catalog contains invalid input modalities",
            "output modality" => "OpenRouter catalog contains invalid output modalities",
            "reasoning effort" => "OpenRouter catalog contains invalid reasoning metadata",
            _ => "OpenRouter catalog contains invalid supported parameters",
        }));
    }
    Ok(())
}

fn normalize_model(
    row: ModelRow,
    provenance: &heycode_llm::ModelMetadataProvenance,
) -> Result<ModelDescriptor, CatalogFetchError> {
    let parameters: BTreeSet<_> = row
        .supported_parameters
        .iter()
        .map(String::as_str)
        .collect();
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = support(parameters.contains("tools"));
    capabilities.reasoning = support(
        row.reasoning.is_some()
            || parameters.contains("reasoning")
            || parameters.contains("reasoning_effort"),
    );
    capabilities.image_input = support(
        row.architecture
            .input_modalities
            .iter()
            .any(|value| value == "image"),
    );
    capabilities.document_input = support(
        row.architecture
            .input_modalities
            .iter()
            .any(|value| value == "file"),
    );
    capabilities.structured_output = support(
        parameters.contains("structured_outputs") || parameters.contains("response_format"),
    );
    // The provider-scoped descriptor includes OpenRouter's documented
    // model-agnostic server-tool fallback, not only upstream-native search.
    capabilities.native_web = CapabilitySupport::Supported;
    if row.pricing.input_cache_read.is_some() {
        capabilities.prompt_cache = CapabilitySupport::Supported;
    }

    let reasoning = row
        .reasoning
        .as_ref()
        .map(ReasoningMetadata::retain)
        .transpose()?;
    let lifecycle = match row.expiration_date.as_deref() {
        Some(expiration) => {
            ModelLifecycle::deprecated(Some(parse_expiration(expiration)?), Vec::new())
        }
        None => ModelLifecycle::stable(),
    };
    let pricing = row.pricing.normalize(provenance)?;
    Ok(ModelDescriptor {
        id: row.id,
        display_name: row.name.trim().to_owned(),
        aliases: Vec::new(),
        created_at_ms: row.created.checked_mul(1_000).filter(|value| *value > 0),
        context_window: row.top_provider.context_length.or(Some(row.context_length)),
        max_output_tokens: row.top_provider.max_completion_tokens,
        lifecycle,
        capabilities,
        reasoning,
        pricing,
        // OpenRouter's catalog publishes no latency or throughput evidence.
        performance: heycode_llm::ModelPerformance::unknown(),
    })
}

const fn support(value: bool) -> CapabilitySupport {
    if value {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unsupported
    }
}

fn parse_expiration(value: &str) -> Result<u64, CatalogFetchError> {
    let millis = if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        date.and_hms_opt(0, 0, 0)
            .ok_or_else(|| invalid_response("OpenRouter expiration date is invalid"))?
            .and_utc()
            .timestamp_millis()
    } else {
        DateTime::parse_from_rfc3339(value)
            .map_err(|_| invalid_response("OpenRouter expiration date is invalid"))?
            .timestamp_millis()
    };
    u64::try_from(millis)
        .map_err(|_| invalid_response("OpenRouter expiration date is before Unix epoch"))
}

fn safe_text(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= max
        && !value.chars().any(char::is_control)
}

fn safe_display_text(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "openrouter".to_owned(),
        display_name: "OpenRouter".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unauthorized,
            "OpenRouter catalog request is unauthorized",
        )),
        429 | 500..=599 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "OpenRouter catalog is temporarily unavailable",
        )),
        _ => Err(invalid_response(
            "OpenRouter catalog returned an unexpected HTTP status",
        )),
    }
}

fn map_transport_error(error: TransportError) -> CatalogFetchError {
    match error {
        TransportError::Cancelled => CatalogFetchError::cancelled(),
        TransportError::Http {
            status: 401 | 403, ..
        } => CatalogFetchError::new(
            CatalogFailureKind::Unauthorized,
            "OpenRouter catalog request is unauthorized",
        ),
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            CatalogFetchError::new(
                CatalogFailureKind::Unavailable,
                "OpenRouter catalog is temporarily unavailable",
            )
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "OpenRouter catalog network request failed",
        ),
        _ => invalid_response("OpenRouter catalog transport response is invalid"),
    }
}

fn invalid_response(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

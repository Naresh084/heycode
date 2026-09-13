//! Authenticated Anthropic model-list discovery and normalization.
//!
//! The source resolves its credential only when a refresh runs, pages the
//! documented `GET /v1/models` cursor through the shared HTTP service, and
//! publishes one all-or-nothing catalog generation.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor, ProviderProtocol,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, CatalogRegistry, ModelCapabilities,
    ModelCatalog, ModelDescriptor, ModelLifecycle, ProviderDescriptor, SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// Provider-owned default Anthropic model id.
pub const ANTHROPIC_CLAUDE_OPUS_5: &str = "claude-opus-5";
/// Required Anthropic API version header value.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

pub(crate) const ANTHROPIC_PROVIDER: &str = "anthropic";
pub(crate) const ANTHROPIC_DISPLAY_NAME: &str = "Anthropic";
pub(crate) const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Documented maximum page size for `GET /v1/models`.
const PAGE_SIZE: u32 = 1000;
/// Pages one refresh may read before a never-ending cursor is a protocol fault.
const MAX_PAGES: usize = 16;
/// Rows one generation may contain.
const MAX_MODELS: usize = 4096;
const CATALOG_RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const MAX_ID_BYTES: usize = 256;
const MAX_DISPLAY_NAME_BYTES: usize = 256;

/// Configuration captured by the Anthropic catalog contribution plugin.
#[derive(Clone)]
pub struct AnthropicCatalogConfig {
    base_url: String,
    credential: CredentialQuery,
}

impl AnthropicCatalogConfig {
    /// Use the official Anthropic API origin and an explicit credential query.
    #[must_use]
    pub fn official(credential: CredentialQuery) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            credential,
        }
    }

    /// Override the API base URL for a compatible proxy or test endpoint.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

/// Provider-owned Anthropic model catalog source.
pub struct AnthropicCatalog {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    credential: CredentialQuery,
    base_url: String,
}

impl AnthropicCatalog {
    /// Build a source for the official Anthropic endpoint.
    ///
    /// # Errors
    /// Returns an invalid-response failure if the fixed endpoint cannot form a
    /// valid credential-free HTTP URL.
    pub fn new(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        credential: CredentialQuery,
    ) -> Result<Self, CatalogFetchError> {
        Self::with_base_url(http, credentials, credential, DEFAULT_BASE_URL)
    }

    /// Build a source for an explicit Anthropic-compatible API base URL.
    ///
    /// # Errors
    /// Blank or invalid HTTP(S) base URLs fail before the source is published.
    pub fn with_base_url(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        credential: CredentialQuery,
        base_url: impl AsRef<str>,
    ) -> Result<Self, CatalogFetchError> {
        let base_url = base_url.as_ref().trim_end_matches('/').to_owned();
        HttpRequest::get(page_url(&base_url, None))
            .map_err(|_| invalid_response("Anthropic catalog base URL is invalid"))?;
        Ok(Self {
            http,
            credentials,
            credential,
            base_url,
        })
    }

    async fn request(
        &self,
        url: &str,
        key: &str,
        cancellation: CancellationToken,
    ) -> Result<HttpResponse, CatalogFetchError> {
        let request = HttpRequest::get(url)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header("x-api-key", key))
            .and_then(|request| request.header("anthropic-version", ANTHROPIC_VERSION))
            .map(|request| request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT))
            .map_err(|_| invalid_response("Anthropic catalog request could not be constructed"))?;
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
impl ModelCatalog for AnthropicCatalog {
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
        let secret = self
            .credentials
            .resolve(&self.credential)
            .map_err(|_| unauthorized())?
            .ok_or_else(unauthorized)?;

        let mut rows: BTreeMap<String, ModelRow> = BTreeMap::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let url = page_url(&self.base_url, cursor.as_deref());
            let response = self
                .request(&url, secret.expose(), cancellation.clone())
                .await?;
            if cancellation.is_cancelled() {
                return Err(CatalogFetchError::cancelled());
            }
            let page = parse_page(&response)?;
            for row in page.data {
                validate_row(&row)?;
                if rows.insert(row.id.clone(), row).is_some() {
                    return Err(invalid_response(
                        "Anthropic model list contains duplicate model ids",
                    ));
                }
            }
            if rows.len() > MAX_MODELS {
                return Err(invalid_response("Anthropic model list is too large"));
            }
            if !page.has_more {
                return finish(rows);
            }
            // `last_id` is the cursor for the next page; a promised page with no
            // usable cursor is a protocol fault, not an empty result.
            cursor = Some(page.last_id.ok_or_else(|| {
                invalid_response("Anthropic model list promised a page without a cursor")
            })?);
        }
        Err(invalid_response(
            "Anthropic model list exceeded its page budget",
        ))
    }
}

/// Register Anthropic discovery into the shared model catalog registry.
#[must_use]
pub fn anthropic_catalog_plugin(config: AnthropicCatalogConfig) -> Box<dyn Plugin> {
    struct AnthropicCatalogPlugin(AnthropicCatalogConfig);

    impl Plugin for AnthropicCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-anthropic"
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
                ANTHROPIC_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MODELS, SERVICE_CREDENTIALS, SERVICE_HTTP]
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
            let source = AnthropicCatalog::with_base_url(
                http.as_ref().clone(),
                credentials,
                self.0.credential.clone(),
                &self.0.base_url,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(AnthropicCatalogPlugin(config))
}

fn page_url(base_url: &str, cursor: Option<&str>) -> String {
    match cursor {
        // Cursors are validated model ids restricted to a URL-safe charset, so
        // they need no escaping here.
        Some(cursor) => format!("{base_url}/v1/models?limit={PAGE_SIZE}&after_id={cursor}"),
        None => format!("{base_url}/v1/models?limit={PAGE_SIZE}"),
    }
}

fn finish(rows: BTreeMap<String, ModelRow>) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
    if !rows.contains_key(ANTHROPIC_CLAUDE_OPUS_5) {
        return Err(invalid_response(
            "Anthropic model list omitted the provider default model",
        ));
    }
    Ok(rows.into_values().map(normalize_model).collect())
}

#[derive(Debug, Deserialize)]
struct ModelsPage {
    data: Vec<ModelRow>,
    has_more: bool,
    first_id: Option<String>,
    last_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelRow {
    id: String,
    #[serde(rename = "type")]
    object: String,
    display_name: String,
    created_at: String,
    /// Documented as nullable. Absent and null both mean "no published limit";
    /// only a zero contradicts the field's meaning.
    #[serde(default)]
    max_input_tokens: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    capabilities: Option<Capabilities>,
}

/// Every capability is optional evidence: an absent field is Unknown, never a
/// rejected generation and never Supported. Identity and limits above stay
/// strict because request resolution depends on them.
#[derive(Debug, Deserialize)]
struct Capabilities {
    #[serde(default)]
    image_input: Option<Support>,
    #[serde(default)]
    pdf_input: Option<Support>,
    #[serde(default)]
    structured_outputs: Option<Support>,
    #[serde(default)]
    thinking: Option<Support>,
    #[serde(default)]
    context_management: Option<ContextManagement>,
}

#[derive(Debug, Deserialize)]
struct Support {
    supported: bool,
}

#[derive(Debug, Deserialize)]
struct ContextManagement {
    supported: bool,
    #[serde(default)]
    compact_20260112: Option<Support>,
}

fn parse_page(response: &HttpResponse) -> Result<ModelsPage, CatalogFetchError> {
    require_json(response)?;
    let page: ModelsPage = serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("Anthropic model list has an invalid JSON shape"))?;
    let (Some(first), Some(last)) = (page.data.first(), page.data.last()) else {
        return Err(invalid_response("Anthropic model list page is empty"));
    };
    // `first_id`/`last_id` are documented as the first and last ids of `data`;
    // a page whose cursors describe other rows is not a page we can follow.
    if page
        .first_id
        .as_deref()
        .is_some_and(|value| value != first.id)
        || page
            .last_id
            .as_deref()
            .is_some_and(|value| value != last.id)
    {
        return Err(invalid_response(
            "Anthropic model list cursors disagree with the returned rows",
        ));
    }
    Ok(page)
}

fn require_json(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    if response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    }) {
        Ok(())
    } else {
        Err(invalid_response(
            "Anthropic model list response is not JSON",
        ))
    }
}

fn validate_row(row: &ModelRow) -> Result<(), CatalogFetchError> {
    if row.object != "model" {
        return Err(invalid_response(
            "Anthropic model list contains a non-model row",
        ));
    }
    if !safe_model_id(&row.id) || !safe_display_text(&row.display_name) {
        return Err(invalid_response(
            "Anthropic model list contains an invalid model identity",
        ));
    }
    if DateTime::parse_from_rfc3339(&row.created_at).is_err() {
        return Err(invalid_response(
            "Anthropic model list contains an invalid release instant",
        ));
    }
    if row.max_input_tokens == Some(0) || row.max_tokens == Some(0) {
        return Err(invalid_response(
            "Anthropic model list contains a zero token limit",
        ));
    }
    Ok(())
}

fn normalize_model(row: ModelRow) -> ModelDescriptor {
    let mut capabilities = ModelCapabilities::unknown();
    // The current prompt-caching guide states that automatic and explicit
    // caching are supported on every active Claude model. A successful
    // account-scoped Models row is the active-model evidence; this is a
    // provider-wide capability join, not an inference from Messages syntax.
    capabilities.prompt_cache = CapabilitySupport::Supported;
    if let Some(evidence) = row.capabilities.as_ref() {
        capabilities.reasoning = support(evidence.thinking.as_ref());
        capabilities.image_input = support(evidence.image_input.as_ref());
        capabilities.document_input = support(evidence.pdf_input.as_ref());
        capabilities.structured_output = support(evidence.structured_outputs.as_ref());
        capabilities.native_compaction = compaction_support(evidence.context_management.as_ref());
    }
    // The Models API and provider-wide docs publish no equivalent tool-calling
    // or provider-hosted-web fact, so those remain Unknown.
    ModelDescriptor {
        id: row.id,
        display_name: row.display_name.trim().to_owned(),
        // The list endpoint publishes no alias field; `/v1/models/{alias}`
        // resolves them one at a time and is not a generation source.
        aliases: Vec::new(),
        created_at_ms: DateTime::parse_from_rfc3339(&row.created_at)
            .ok()
            .and_then(|value| u64::try_from(value.timestamp_millis()).ok())
            .filter(|value| *value > 0),
        context_window: row.max_input_tokens,
        max_output_tokens: row.max_tokens,
        // No deprecation or retirement evidence is published here.
        lifecycle: ModelLifecycle::unknown(),
        capabilities,
        // Anthropic publishes prices in documentation, not through any API
        // endpoint, so no price is transcribed as if it were API evidence.
        pricing: heycode_llm::ModelPricing::unknown(),
        performance: heycode_llm::ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn support(evidence: Option<&Support>) -> CapabilitySupport {
    match evidence {
        Some(Support { supported: true }) => CapabilitySupport::Supported,
        Some(Support { supported: false }) => CapabilitySupport::Unsupported,
        None => CapabilitySupport::Unknown,
    }
}

fn compaction_support(evidence: Option<&ContextManagement>) -> CapabilitySupport {
    match evidence {
        // The exact strategy row is the strongest evidence.
        Some(ContextManagement {
            compact_20260112: Some(strategy),
            ..
        }) => support(Some(strategy)),
        // No strategy row, but context management itself is explicitly absent.
        Some(ContextManagement {
            supported: false, ..
        }) => CapabilitySupport::Unsupported,
        _ => CapabilitySupport::Unknown,
    }
}

/// Model ids stay inside an unreserved URL charset so a pagination cursor or a
/// single-model path needs no escaping.
pub(crate) fn safe_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':' | b'@')
        })
}

fn safe_display_text(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && value.len() <= MAX_DISPLAY_NAME_BYTES
        && !value.chars().any(char::is_control)
}

pub(crate) fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: ANTHROPIC_PROVIDER.to_owned(),
        display_name: ANTHROPIC_DISPLAY_NAME.to_owned(),
        protocols: vec![ProviderProtocol::AnthropicMessages],
    }
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized()),
        429 | 500..=599 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "Anthropic model list is temporarily unavailable",
        )),
        _ => Err(invalid_response(
            "Anthropic model list returned an unexpected HTTP status",
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
                "Anthropic model list is temporarily unavailable",
            )
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "Anthropic model list network request failed",
        ),
        _ => invalid_response("Anthropic model list transport response is invalid"),
    }
}

fn unauthorized() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        "Anthropic credential is missing or unauthorized",
    )
}

fn invalid_response(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_http_timeout_preserves_the_catalog_network_class() {
        let failure = map_transport_error(TransportError::Timeout);
        assert_eq!(failure.kind(), CatalogFailureKind::Network);
        assert_eq!(
            failure.message(),
            "Anthropic model list network request failed"
        );
    }

    #[test]
    fn model_ids_outside_the_unreserved_charset_are_refused() {
        assert!(safe_model_id("claude-opus-5"));
        assert!(safe_model_id("claude-3-5-haiku-20241022"));
        assert!(safe_model_id("claude-2.1"));
        for rejected in ["", "claude opus", "claude/opus", "claude?limit=1", "a&b"] {
            assert!(!safe_model_id(rejected), "`{rejected}` must be refused");
        }
    }
}

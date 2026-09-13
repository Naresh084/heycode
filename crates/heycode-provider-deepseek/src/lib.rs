//! DeepSeek-owned model discovery, lifecycle metadata and protocol profiles.
//!
//! The catalog source resolves credentials only when a refresh runs, uses the
//! shared HTTP service, and publishes one all-or-nothing catalog generation.
//!
//! DeepSeek serves the same models over two dialects. The OpenAI-shaped one is
//! reached at [`DEEPSEEK_OPENAI_BASE_URL`]; the Anthropic-shaped one is
//! described by [`anthropic`], which transcribes DeepSeek's published
//! compatibility table and names every field that table leaves out.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
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

pub mod anthropic;
pub mod optional;

pub use anthropic::{
    DEEPSEEK_ANTHROPIC_BASE_URL, DEEPSEEK_ANTHROPIC_MESSAGES_BASE_URL,
    DEEPSEEK_IGNORED_THINKING_BUDGET_TOKENS, DEEPSEEK_V4_FLASH_VISION_EXP,
    DEEPSEEK_V4_MAX_OUTPUT_TOKENS, DEEPSEEK_V4_PRO_1M, DeepSeekAnthropicAdapter,
    DeepSeekAnthropicEffort, DeepSeekAnthropicError, DeepSeekAnthropicField,
    DeepSeekAnthropicModelRoute, DeepSeekAnthropicProfile, DeepSeekAnthropicSupport, admit_model,
    classify_model, temperature_with_thinking_enabled,
};
pub use optional::{
    DEEPSEEK_BETA_BASE_URL, DEEPSEEK_BETA_CHAT_COMPLETIONS_URL, DEEPSEEK_BETA_FIM_COMPLETIONS_URL,
    DEEPSEEK_CHAT_PREFIX_OPTION_KIND, DEEPSEEK_FIM_MAX_OUTPUT_TOKENS,
    DEEPSEEK_JSON_OUTPUT_OPTION_KIND, DEEPSEEK_STANDARD_CHAT_COMPLETIONS_URL,
    DEEPSEEK_STRICT_TOOLS_OPTION_KIND, DeepSeekChatPrefix, DeepSeekFimRequest, DeepSeekJsonOutput,
    DeepSeekOptionalCapability, DeepSeekOptionalError, DeepSeekOptionalProtocol,
    DeepSeekOptionalRoute, DeepSeekStrictTools,
};

/// Current low-latency DeepSeek V4 model id.
pub const DEEPSEEK_V4_FLASH: &str = "deepseek-v4-flash";
/// Current high-capability DeepSeek V4 model id.
pub const DEEPSEEK_V4_PRO: &str = "deepseek-v4-pro";
/// Exact published retirement instant for the legacy Chat/Reasoner aliases.
pub const DEEPSEEK_LEGACY_RETIREMENT_MS: u64 = 1_784_908_740_000;

/// Non-secret credential reference DeepSeek's own guides populate.
///
/// Source: <https://api-docs.deepseek.com/> sample code, which reads the key
/// from `DEEPSEEK_API_KEY`.
pub const DEEPSEEK_API_KEY_REFERENCE: &str = "DEEPSEEK_API_KEY";

/// Base URL for DeepSeek's OpenAI-format dialect.
///
/// Source: the "BASE URL (OpenAI Format)" row of
/// <https://api-docs.deepseek.com/quick_start/pricing>.
pub const DEEPSEEK_OPENAI_BASE_URL: &str = "https://api.deepseek.com";

const DEEPSEEK_PROVIDER: &str = "deepseek";
const DEEPSEEK_DISPLAY_NAME: &str = "DeepSeek";
const DEFAULT_BASE_URL: &str = DEEPSEEK_OPENAI_BASE_URL;
const CATALOG_RESPONSE_LIMIT: usize = 1024 * 1024;
const V4_CONTEXT_WINDOW: u64 = 1_048_576;
const V4_MAX_OUTPUT_TOKENS: u64 = DEEPSEEK_V4_MAX_OUTPUT_TOKENS;

/// Configuration captured by the DeepSeek catalog contribution plugin.
#[derive(Clone)]
pub struct DeepSeekCatalogConfig {
    base_url: String,
    credential: CredentialQuery,
}

impl DeepSeekCatalogConfig {
    /// Use the official DeepSeek API origin and an explicit credential query.
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

/// Provider-owned DeepSeek model catalog source.
pub struct DeepSeekCatalog {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    credential: CredentialQuery,
    models_url: String,
}

impl DeepSeekCatalog {
    /// Build a source for the official DeepSeek endpoint.
    ///
    /// # Errors
    /// Returns an invalid-response failure if the fixed endpoint cannot form
    /// a valid credential-free HTTP URL.
    pub fn new(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        credential: CredentialQuery,
    ) -> Result<Self, CatalogFetchError> {
        Self::with_base_url(http, credentials, credential, DEFAULT_BASE_URL)
    }

    /// Build a source for an explicit DeepSeek-compatible API base URL.
    ///
    /// # Errors
    /// Blank or invalid HTTP(S) base URLs fail before the source is published.
    pub fn with_base_url(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        credential: CredentialQuery,
        base_url: impl AsRef<str>,
    ) -> Result<Self, CatalogFetchError> {
        let base_url = base_url.as_ref().trim_end_matches('/');
        let models_url = format!("{base_url}/models");
        HttpRequest::get(&models_url).map_err(|_| {
            CatalogFetchError::new(
                CatalogFailureKind::InvalidResponse,
                "DeepSeek catalog base URL is invalid",
            )
        })?;
        Ok(Self {
            http,
            credentials,
            credential,
            models_url,
        })
    }
}

#[async_trait]
impl ModelCatalog for DeepSeekCatalog {
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
        let authorization = format!("Bearer {}", secret.expose());
        let request = HttpRequest::get(&self.models_url)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header("authorization", &authorization))
            .map(|request| request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT))
            .map_err(|_| invalid_response("DeepSeek catalog request could not be constructed"))?;
        let response = self
            .http
            .send(request, cancellation.clone())
            .await
            .map_err(map_transport_error)?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        classify_status(&response)?;
        parse_catalog(response)
    }
}

/// Register DeepSeek discovery into the shared model catalog registry.
#[must_use]
pub fn deepseek_catalog_plugin(config: DeepSeekCatalogConfig) -> Box<dyn Plugin> {
    struct DeepSeekCatalogPlugin(DeepSeekCatalogConfig);

    impl Plugin for DeepSeekCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-deepseek"
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
                DEEPSEEK_PROVIDER,
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
            let source = DeepSeekCatalog::with_base_url(
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

    Box::new(DeepSeekCatalogPlugin(config))
}

#[derive(Deserialize)]
struct ModelsEnvelope {
    object: String,
    data: Vec<ModelRow>,
}

#[derive(Deserialize)]
struct ModelRow {
    id: String,
    object: String,
    owned_by: String,
}

fn parse_catalog(response: HttpResponse) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
    if !response
        .content_type
        .as_deref()
        .is_some_and(|value| value == "application/json" || value.ends_with("+json"))
    {
        return Err(invalid_response("DeepSeek catalog response is not JSON"));
    }
    let envelope: ModelsEnvelope = serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("DeepSeek catalog response has an invalid JSON shape"))?;
    if envelope.object != "list" {
        return Err(invalid_response(
            "DeepSeek catalog response object is not `list`",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut models = BTreeMap::new();
    for row in envelope.data {
        if row.id.is_empty()
            || row.id.trim() != row.id
            || row.object != "model"
            || row.owned_by != DEEPSEEK_PROVIDER
            || !seen.insert(row.id.clone())
        {
            return Err(invalid_response(
                "DeepSeek catalog contains an invalid or duplicate model row",
            ));
        }
        models.insert(row.id.clone(), live_descriptor(row.id));
    }
    for legacy in ["deepseek-chat", "deepseek-reasoner"] {
        models.insert(legacy.to_owned(), retired_descriptor(legacy));
    }
    Ok(models.into_values().collect())
}

fn live_descriptor(id: String) -> ModelDescriptor {
    match id.as_str() {
        DEEPSEEK_V4_FLASH => v4_descriptor(id, "DeepSeek V4 Flash"),
        DEEPSEEK_V4_PRO => v4_descriptor(id, "DeepSeek V4 Pro"),
        _ => ModelDescriptor::unknown(id),
    }
}

fn v4_descriptor(id: String, display_name: &str) -> ModelDescriptor {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    capabilities.reasoning = CapabilitySupport::Supported;
    capabilities.prompt_cache = CapabilitySupport::Supported;
    ModelDescriptor {
        // DeepSeek's authenticated `/models` publishes no price or performance
        // evidence, so neither is invented here.
        pricing: heycode_llm::ModelPricing::unknown(),
        performance: heycode_llm::ModelPerformance::unknown(),
        id,
        display_name: display_name.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(V4_CONTEXT_WINDOW),
        max_output_tokens: Some(V4_MAX_OUTPUT_TOKENS),
        lifecycle: ModelLifecycle::preview(),
        capabilities,
        reasoning: None,
    }
}

fn retired_descriptor(id: &str) -> ModelDescriptor {
    ModelDescriptor {
        // DeepSeek's authenticated `/models` publishes no price or performance
        // evidence, so neither is invented here.
        pricing: heycode_llm::ModelPricing::unknown(),
        performance: heycode_llm::ModelPerformance::unknown(),
        id: id.to_owned(),
        display_name: id.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::retired(
            Some(DEEPSEEK_LEGACY_RETIREMENT_MS),
            vec![DEEPSEEK_V4_FLASH.to_owned()],
        ),
        capabilities: ModelCapabilities::unknown(),
        reasoning: None,
    }
}

fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: DEEPSEEK_PROVIDER.to_owned(),
        display_name: DEEPSEEK_DISPLAY_NAME.to_owned(),
        // Both dialects are published for every current model: the
        // "Anthropic API" row of
        // <https://api-docs.deepseek.com/quick_start/pricing> marks all three
        // `✓`, alongside the OpenAI-format base URL in the same table.
        protocols: vec![
            ProviderProtocol::OpenAiChatCompletions,
            ProviderProtocol::AnthropicMessages,
        ],
    }
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized()),
        429 | 500..=599 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "DeepSeek catalog is temporarily unavailable",
        )),
        _ => Err(invalid_response(
            "DeepSeek catalog returned an unexpected HTTP status",
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
                "DeepSeek catalog is temporarily unavailable",
            )
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "DeepSeek catalog network request failed",
        ),
        TransportError::Http { .. }
        | TransportError::InvalidRequest { .. }
        | TransportError::InvalidSse { .. }
        | TransportError::ResponseTooLarge { .. } => {
            invalid_response("DeepSeek catalog transport response is invalid")
        }
        _ => invalid_response("DeepSeek catalog transport response is invalid"),
    }
}

fn unauthorized() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        "DeepSeek credential is missing or unauthorized",
    )
}

fn invalid_response(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_http_timeout_preserves_the_existing_catalog_network_class() {
        let failure = map_transport_error(TransportError::Timeout);
        assert_eq!(failure.kind(), CatalogFailureKind::Network);
        assert_eq!(failure.message(), "DeepSeek catalog network request failed");
    }
}

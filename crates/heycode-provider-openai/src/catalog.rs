//! Authenticated OpenAI model-list discovery and normalization.
//!
//! The source resolves its credential only when a refresh runs and reads the
//! documented `GET /v1/models` endpoint through the shared HTTP service. That
//! endpoint accepts no query parameters and publishes no cursor, so one
//! refresh is exactly one request and one all-or-nothing generation.
//!
//! The list is account-scoped — it is what the presented key may use — so the
//! generation is the account-capable model set. It carries no capability,
//! window, price or display-name field. Exact maintained primary documentation
//! is joined only for the capabilities whose production admission requires an
//! affirmative model fact: native web search, native compaction and prompt
//! caching on `gpt-5.6-sol`. Every other field and model remain Unknown.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor, ProviderProtocol,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, CatalogRegistry, ModelCapabilities,
    ModelCatalog, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing,
    ProviderDescriptor, SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// Provider-owned default OpenAI model id.
///
/// `gpt-5.6-sol` is the frontier member of the GPT-5.6 family and an exact id
/// in the official `ModelIdsShared` enum; the bare `gpt-5.6` alias is not an
/// enumerated id, so the explicit variant is the request-facing default.
pub const OPENAI_GPT_5_6_SOL: &str = "gpt-5.6-sol";

pub(crate) const OPENAI_PROVIDER: &str = "openai";
pub(crate) const OPENAI_DISPLAY_NAME: &str = "OpenAI";
pub(crate) const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Rows one generation may contain.
const MAX_MODELS: usize = 4096;
const CATALOG_RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const MAX_ID_BYTES: usize = 256;
const MAX_OWNER_BYTES: usize = 256;
/// The documented `shutdown_date` format is a bare calendar date.
const SHUTDOWN_DATE_FORMAT: &str = "%Y-%m-%d";

/// Configuration captured by the OpenAI catalog contribution plugin.
#[derive(Clone)]
pub struct OpenAiCatalogConfig {
    base_url: String,
    credential: CredentialQuery,
}

impl OpenAiCatalogConfig {
    /// Use the official OpenAI API origin and an explicit credential query.
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

/// Provider-owned OpenAI model catalog source.
pub struct OpenAiCatalog {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    credential: CredentialQuery,
    base_url: String,
}

impl OpenAiCatalog {
    /// Build a source for the official OpenAI endpoint.
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

    /// Build a source for an explicit OpenAI-compatible API base URL.
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
        HttpRequest::get(models_url(&base_url))
            .map_err(|_| invalid_response("OpenAI catalog base URL is invalid"))?;
        Ok(Self {
            http,
            credentials,
            credential,
            base_url,
        })
    }

    async fn request(
        &self,
        key: &str,
        cancellation: CancellationToken,
    ) -> Result<HttpResponse, CatalogFetchError> {
        let request = HttpRequest::get(models_url(&self.base_url))
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header("authorization", &bearer(key)))
            .map(|request| request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT))
            .map_err(|_| invalid_response("OpenAI catalog request could not be constructed"))?;
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
impl ModelCatalog for OpenAiCatalog {
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

        let response = self.request(secret.expose(), cancellation.clone()).await?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let page = parse_list(&response)?;
        if page.data.len() > MAX_MODELS {
            return Err(invalid_response("OpenAI model list is too large"));
        }

        let mut rows: BTreeMap<String, ModelDescriptor> = BTreeMap::new();
        for row in page.data {
            let model = normalize_row(row)?;
            if rows.insert(model.id.clone(), model).is_some() {
                return Err(invalid_response(
                    "OpenAI model list contains duplicate model ids",
                ));
            }
        }
        // The account-scoped list never legitimately publishes zero models; an
        // empty generation would replace last-good state with nothing.
        if rows.is_empty() {
            return Err(invalid_response("OpenAI model list is empty"));
        }
        Ok(rows.into_values().collect())
    }
}

/// Register OpenAI discovery into the shared model catalog registry.
#[must_use]
pub fn openai_catalog_plugin(config: OpenAiCatalogConfig) -> Box<dyn Plugin> {
    struct OpenAiCatalogPlugin(OpenAiCatalogConfig);

    impl Plugin for OpenAiCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-openai"
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
                OPENAI_PROVIDER,
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
            let source = OpenAiCatalog::with_base_url(
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

    Box::new(OpenAiCatalogPlugin(config))
}

/// The documented list endpoint takes no query parameter and no cursor.
pub(crate) fn models_url(base_url: &str) -> String {
    format!("{base_url}/v1/models")
}

pub(crate) fn bearer(key: &str) -> String {
    format!("Bearer {key}")
}

#[derive(Debug, Deserialize)]
struct ModelList {
    #[serde(rename = "object")]
    envelope: String,
    data: Vec<ModelRow>,
}

/// The documented `Model` object. `id`, `object`, `created` and `owned_by` are
/// required; `shutdown_date` is the only optional field and is nullable.
#[derive(Debug, Deserialize)]
struct ModelRow {
    id: String,
    #[serde(rename = "object")]
    object: String,
    /// Required by the documented schema. Kept so a row missing it fails
    /// rather than normalizing into a descriptor from an unrecognized shape.
    created: u64,
    owned_by: String,
    /// Absent and null both mean "shutdown not announced".
    #[serde(default)]
    shutdown_date: Option<String>,
}

fn parse_list(response: &HttpResponse) -> Result<ModelList, CatalogFetchError> {
    require_json(response)?;
    let list: ModelList = serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("OpenAI model list has an invalid JSON shape"))?;
    if list.envelope != "list" {
        return Err(invalid_response("OpenAI model list is not a list envelope"));
    }
    Ok(list)
}

fn require_json(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    if response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    }) {
        Ok(())
    } else {
        Err(invalid_response("OpenAI model list response is not JSON"))
    }
}

/// Validate one documented row and normalize it in a single pass.
fn normalize_row(row: ModelRow) -> Result<ModelDescriptor, CatalogFetchError> {
    if row.object != "model" {
        return Err(invalid_response(
            "OpenAI model list contains a non-model row",
        ));
    }
    if !safe_model_id(&row.id) {
        return Err(invalid_response(
            "OpenAI model list contains an invalid model id",
        ));
    }
    // `owned_by` is a documented required string. It publishes nothing this
    // descriptor carries, so it is checked for shape only, never normalized
    // into display metadata it was not meant to be.
    if !safe_owner(&row.owned_by) {
        return Err(invalid_response(
            "OpenAI model list contains an invalid model owner",
        ));
    }
    let lifecycle = match row.shutdown_date.as_deref() {
        // An announced shutdown is real availability evidence: still
        // selectable, but a deadline exists. CAT03 resolves the effective
        // phase against an explicit instant, so the deadline must be exact.
        Some(date) => ModelLifecycle::deprecated(Some(retirement_at_ms(date)?), Vec::new()),
        // No announcement is no evidence at all — never a Stable claim.
        None => ModelLifecycle::unknown(),
    };
    let capabilities = maintained_capabilities(&row.id);
    Ok(ModelDescriptor {
        // The endpoint publishes no display name, so identity is the id.
        display_name: row.id.clone(),
        id: row.id,
        // It publishes no alias field; `GET /v1/models/{model}` resolves one
        // id at a time and is not a generation source.
        aliases: Vec::new(),
        created_at_ms: row.created.checked_mul(1_000).filter(|value| *value > 0),
        // It publishes no context window or output cap. Documented model
        // pages do, but a docs table is not API evidence (GOTCHAS #41).
        context_window: None,
        max_output_tokens: None,
        lifecycle,
        // The endpoint publishes no capability field. The helper joins only
        // exact maintained primary-documentation facts for one exact model;
        // protocol compatibility and family-name guesses never contribute.
        capabilities,
        // No OpenAI API endpoint publishes per-token prices, so nothing is
        // transcribed here as if it were API evidence.
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    })
}

fn maintained_capabilities(model: &str) -> ModelCapabilities {
    // The current compact reference uses the exact GPT_5_6_SOL model enum, and
    // the current model/Responses guides explicitly document prompt-cache and
    // web-search support. None supports promoting an arbitrary account-listed
    // id.
    if model == OPENAI_GPT_5_6_SOL {
        ModelCapabilities {
            native_compaction: CapabilitySupport::Supported,
            prompt_cache: CapabilitySupport::Supported,
            native_web: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        }
    } else {
        ModelCapabilities::unknown()
    }
}

/// Convert an announced shutdown date to its exact deadline instant.
///
/// The documented format is a bare calendar date with no time or zone, so the
/// deadline is the start of that day in UTC: the earliest instant the
/// announced date can begin, which never lets resolution dispatch past it.
fn retirement_at_ms(value: &str) -> Result<u64, CatalogFetchError> {
    let invalid = || invalid_response("OpenAI model list contains an invalid shutdown date");
    let date = NaiveDate::parse_from_str(value, SHUTDOWN_DATE_FORMAT).map_err(|_| invalid())?;
    let instant = date.and_hms_opt(0, 0, 0).ok_or_else(invalid)?;
    u64::try_from(instant.and_utc().timestamp_millis()).map_err(|_| invalid())
}

/// Model ids stay inside an unreserved URL charset so a single-model path
/// needs no escaping. Fine-tuned ids carry `:` separators and are included.
pub(crate) fn safe_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':' | b'@')
        })
}

fn safe_owner(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_OWNER_BYTES
        && !value.chars().any(char::is_control)
}

pub(crate) fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: OPENAI_PROVIDER.to_owned(),
        display_name: OPENAI_DISPLAY_NAME.to_owned(),
        protocols: vec![
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions,
        ],
    }
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized()),
        429 | 500..=599 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "OpenAI model list is temporarily unavailable",
        )),
        _ => Err(invalid_response(
            "OpenAI model list returned an unexpected HTTP status",
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
                "OpenAI model list is temporarily unavailable",
            )
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "OpenAI model list network request failed",
        ),
        _ => invalid_response("OpenAI model list transport response is invalid"),
    }
}

fn unauthorized() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        "OpenAI credential is missing or unauthorized",
    )
}

fn invalid_response(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TransportError::Http` carries an `HttpErrorBody` with no constructor
    /// outside `heycode-http`, so its two arms cannot be built here; the
    /// equivalent status classification is proven end to end through the fake
    /// transport in `status_and_transport_failures_classify_without_exposing_bodies`.
    #[test]
    fn constructible_transport_failures_map_to_the_catalog_taxonomy() {
        assert_eq!(
            map_transport_error(TransportError::Cancelled).kind(),
            CatalogFailureKind::Cancelled
        );
        let network = map_transport_error(TransportError::Network {
            message: "must-not-leak".to_owned(),
        });
        assert_eq!(network.kind(), CatalogFailureKind::Network);
        assert!(!network.message().contains("must-not-leak"));
        let invalid = map_transport_error(TransportError::InvalidRequest {
            field: "url",
            message: "must-not-leak".to_owned(),
        });
        assert_eq!(invalid.kind(), CatalogFailureKind::InvalidResponse);
        assert!(!invalid.message().contains("must-not-leak"));
    }

    #[test]
    fn typed_http_timeout_preserves_the_catalog_network_class() {
        let failure = map_transport_error(TransportError::Timeout);
        assert_eq!(failure.kind(), CatalogFailureKind::Network);
        assert_eq!(
            failure.message(),
            "OpenAI model list network request failed"
        );
    }

    #[test]
    fn model_ids_outside_the_unreserved_charset_are_refused() {
        assert!(safe_model_id("gpt-5.6-sol"));
        assert!(safe_model_id("gpt-4.1-2025-04-14"));
        assert!(safe_model_id("o3-mini"));
        // Fine-tuned ids are colon-separated and must remain publishable.
        assert!(safe_model_id("ft:gpt-4.1-2025-04-14:acme::9abcdefg"));
        for rejected in ["", "gpt 5", "gpt/5", "gpt-5?limit=1", "a&b", "../models"] {
            assert!(!safe_model_id(rejected), "`{rejected}` must be refused");
        }
    }

    #[test]
    fn only_the_documented_bare_date_form_yields_a_deadline() {
        assert!(matches!(
            retirement_at_ms("2026-10-23"),
            Ok(1_792_713_600_000)
        ));
        assert!(matches!(retirement_at_ms("1970-01-01"), Ok(0)));
        for rejected in [
            "soon",
            "2026-13-45",
            "2026-10-23T00:00:00Z",
            "2026-10-23 ",
            "23-10-2026",
            "1969-12-31",
        ] {
            assert!(
                retirement_at_ms(rejected).is_err(),
                "`{rejected}` must be refused"
            );
        }
    }
}

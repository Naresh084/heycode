//! Authenticated Gemini model discovery and capability normalization.
//!
//! The source resolves its credential — and, in the OAuth mode, its quota
//! project — only when a refresh runs, pages the documented `GET /v1/models`
//! endpoint through the shared HTTP service, and publishes one all-or-nothing
//! catalog generation.
//!
//! # Which surface this is
//!
//! Google publishes generative models through two different APIs, and only one
//! of them answers the question this catalog exists to answer.
//!
//! - The **Gemini Developer API** (`generativelanguage.googleapis.com`)
//!   documents `models.list` as "Lists the `Model`s available through the
//!   Gemini API" for the presented credential, and every row carries
//!   `inputTokenLimit`, `outputTokenLimit`, `supportedGenerationMethods` and
//!   `thinking`. That is accessibility plus capability evidence, so it is the
//!   generation source here.
//! - **Vertex AI** lists base models through
//!   `ModelGardenService.ListPublisherModels` under `publishers/google/models`.
//!   Its `PublisherModel` resource has no token-limit field, no capability
//!   field, no supported-method list and no per-project entitlement field, and
//!   the shipped Google Gen AI SDK reads none of those from it while reading
//!   all of them on the Developer API path. A Vertex-sourced generation could
//!   therefore normalize no capability at all, so this crate deliberately does
//!   not publish one rather than publishing a catalog that answers nothing.
//!
//! # Sources
//!
//! - Discovery document (authoritative field names, paths and parameters):
//!   <https://generativelanguage.googleapis.com/$discovery/rest?version=v1>
//! - `models.list` reference: <https://ai.google.dev/api/models#method:-models.list>
//! - API-key header: <https://ai.google.dev/gemini-api/docs/api-key>
//! - OAuth/ADC bearer plus quota project:
//!   <https://ai.google.dev/gemini-api/docs/oauth>
//! - Vertex `ListPublisherModels` shape (why it is not used):
//!   <https://aiplatform.googleapis.com/$discovery/rest?version=v1beta1>

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization_gcp::{
    GcpAuthService, GcpProfileRequest, GcpProjectHealth, SERVICE_GCP_AUTH,
};
use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor, ProviderProtocol,
    ServiceKey,
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

/// Provider-owned default Gemini model id.
///
/// The documented model list marks `gemini-3.7-flash` Stable and describes it
/// as "Our latest and most capable Flash model, built for complex coding,
/// agentic workflows, and reliable multi-step execution"; the Pro variant of
/// the same generation is Preview
/// (<https://ai.google.dev/gemini-api/docs/models>).
pub const GOOGLE_GEMINI_3_7_FLASH: &str = "gemini-3.7-flash";

pub(crate) const GOOGLE_PROVIDER: &str = "google";
pub(crate) const GOOGLE_DISPLAY_NAME: &str = "Google Gemini";
pub(crate) const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Stable API version. `v1` and `v1beta` publish an identical `Model` schema,
/// so the stable version is used; it is also the version the documented ADC
/// example calls (<https://ai.google.dev/gemini-api/docs/oauth>).
const API_VERSION: &str = "v1";
/// Header that carries an API key
/// (<https://ai.google.dev/gemini-api/docs/api-key>).
const API_KEY_HEADER: &str = "x-goog-api-key";
/// Header that names the billing/quota project for an OAuth call
/// (<https://ai.google.dev/gemini-api/docs/oauth>).
const QUOTA_PROJECT_HEADER: &str = "x-goog-user-project";
/// Documented `models/{model}` resource-name prefix.
const MODEL_NAME_PREFIX: &str = "models/";
/// The one generation method a heycode inference request uses. The field's
/// documented example values are `generateMessage` and `generateContent`.
const GENERATE_CONTENT_METHOD: &str = "generateContent";
/// Documented maximum page size: "This method returns at most 1000 models per
/// page, even if you pass a larger page_size."
const PAGE_SIZE: u32 = 1000;
/// Pages one refresh may read before a never-ending cursor is a protocol fault.
const MAX_PAGES: usize = 16;
/// Rows one generation may contain, counted before filtering.
const MAX_MODELS: usize = 4096;
const CATALOG_RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const MAX_ID_BYTES: usize = 256;
/// A display name is documented as "up to 128 characters" of arbitrary UTF-8,
/// so the byte bound is the widest encoding of that character bound.
const MAX_DISPLAY_NAME_BYTES: usize = 512;
const MAX_METADATA_BYTES: usize = 256;
const MAX_PAGE_TOKEN_BYTES: usize = 4096;

/// How one Gemini Developer API request proves who is calling.
///
/// The two modes are separately documented and carry different accessibility
/// scopes: an API key identifies its own project, while an OAuth token needs
/// the caller to name a quota project explicitly.
#[derive(Clone)]
enum Authentication {
    /// `x-goog-api-key: <key>`.
    ApiKey,
    /// `Authorization: Bearer <token>` plus `x-goog-user-project: <project>`,
    /// where the project is resolved by PGCP01 rather than re-derived here.
    OAuthQuotaProject {
        gcp: Arc<GcpAuthService>,
        profile: GcpProfileRequest,
    },
}

/// Which credential the Google catalog plugin presents.
#[derive(Clone)]
enum ConfiguredAuth {
    ApiKey,
    OAuthQuotaProject { profile: GcpProfileRequest },
}

/// Configuration captured by the Google catalog contribution plugin.
#[derive(Clone)]
pub struct GoogleCatalogConfig {
    base_url: String,
    credential: CredentialQuery,
    auth: ConfiguredAuth,
}

impl GoogleCatalogConfig {
    /// Present an API key from `credential` as `x-goog-api-key`.
    #[must_use]
    pub fn api_key(credential: CredentialQuery) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            credential,
            auth: ConfiguredAuth::ApiKey,
        }
    }

    /// Present an OAuth access token from `credential` as a bearer token, and
    /// name the quota project PGCP01 resolves for `profile`.
    ///
    /// This crate never mints a token: it presents one the credentials service
    /// already holds, exactly as it would an API key.
    #[must_use]
    pub fn oauth_quota_project(credential: CredentialQuery, profile: GcpProfileRequest) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            credential,
            auth: ConfiguredAuth::OAuthQuotaProject { profile },
        }
    }

    /// Override the API base URL for a compatible proxy or test endpoint.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

/// Provider-owned Gemini model catalog source.
pub struct GeminiCatalog {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    credential: CredentialQuery,
    base_url: String,
    auth: Authentication,
}

impl GeminiCatalog {
    /// Build an API-key source for the official Gemini Developer API origin.
    ///
    /// # Errors
    /// Returns an invalid-response failure if the fixed endpoint cannot form a
    /// valid credential-free HTTP URL.
    pub fn api_key(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        credential: CredentialQuery,
    ) -> Result<Self, CatalogFetchError> {
        Self::build(http, credentials, credential, Authentication::ApiKey)
    }

    /// Build an OAuth source that names the quota project PGCP01 resolves.
    ///
    /// # Errors
    /// Returns an invalid-response failure if the fixed endpoint cannot form a
    /// valid credential-free HTTP URL.
    pub fn oauth_quota_project(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        credential: CredentialQuery,
        gcp: Arc<GcpAuthService>,
        profile: GcpProfileRequest,
    ) -> Result<Self, CatalogFetchError> {
        Self::build(
            http,
            credentials,
            credential,
            Authentication::OAuthQuotaProject { gcp, profile },
        )
    }

    /// Retarget this source at an explicit Gemini-compatible base URL.
    ///
    /// # Errors
    /// Blank or invalid HTTP(S) base URLs fail before the source is published.
    pub fn with_base_url(mut self, base_url: impl AsRef<str>) -> Result<Self, CatalogFetchError> {
        let base_url = base_url.as_ref().trim_end_matches('/').to_owned();
        HttpRequest::get(page_url(&base_url, None))
            .map_err(|_| invalid_response("Gemini catalog base URL is invalid"))?;
        self.base_url = base_url;
        Ok(self)
    }

    fn build(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        credential: CredentialQuery,
        auth: Authentication,
    ) -> Result<Self, CatalogFetchError> {
        Self {
            http,
            credentials,
            credential,
            base_url: String::new(),
            auth,
        }
        .with_base_url(DEFAULT_BASE_URL)
    }

    /// Resolve the quota project this refresh must name, if any.
    ///
    /// PGCP01's three project states are kept distinct on purpose: a project
    /// that is unset or malformed is a determinate failure, while a project
    /// that could not be determined is not a negative finding and must not be
    /// reported as one.
    async fn quota_project(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, CatalogFetchError> {
        let Authentication::OAuthQuotaProject { gcp, profile } = &self.auth else {
            return Ok(None);
        };
        let resolved = gcp.resolve(profile.clone(), cancellation.clone()).await;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        match resolved.project() {
            GcpProjectHealth::Confirmed { project, .. }
            | GcpProjectHealth::Unconfirmed { project, .. } => {
                Ok(Some(project.as_str().to_owned()))
            }
            GcpProjectHealth::Unset | GcpProjectHealth::Malformed { .. } => {
                Err(CatalogFetchError::new(
                    CatalogFailureKind::Unauthorized,
                    "Google Cloud quota project is not set or is not a valid project identity",
                ))
            }
            GcpProjectHealth::Undetermined { .. } => Err(CatalogFetchError::new(
                CatalogFailureKind::Unavailable,
                "Google Cloud quota project could not be determined",
            )),
        }
    }

    async fn request(
        &self,
        url: &str,
        secret: &str,
        quota_project: Option<&str>,
        cancellation: CancellationToken,
    ) -> Result<HttpResponse, CatalogFetchError> {
        let unusable = || invalid_response("Gemini catalog request could not be constructed");
        let request = HttpRequest::get(url)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| match self.auth {
                Authentication::ApiKey => request.header(API_KEY_HEADER, secret),
                Authentication::OAuthQuotaProject { .. } => {
                    request.header("authorization", &format!("Bearer {secret}"))
                }
            })
            .map_err(|_| unusable())?;
        let request = match quota_project {
            Some(project) => request
                .header(QUOTA_PROJECT_HEADER, project)
                .map_err(|_| unusable())?,
            None => request,
        };
        let response = self
            .http
            .send(
                request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT),
                cancellation,
            )
            .await
            .map_err(map_transport_error)?;
        classify_status(&response)?;
        Ok(response)
    }
}

#[async_trait]
impl ModelCatalog for GeminiCatalog {
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
        // Both the credential and the quota project are resolved per refresh,
        // never at composition, so a rotated key or a changed project
        // selection is visible without recomposing the world.
        let secret = self
            .credentials
            .resolve(&self.credential)
            .map_err(|_| unauthorized())?
            .ok_or_else(unauthorized)?;
        let quota_project = self.quota_project(&cancellation).await?;

        let mut rows: BTreeMap<String, ModelDescriptor> = BTreeMap::new();
        let mut seen: usize = 0;
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let url = page_url(&self.base_url, page_token.as_deref());
            let response = self
                .request(
                    &url,
                    secret.expose(),
                    quota_project.as_deref(),
                    cancellation.clone(),
                )
                .await?;
            if cancellation.is_cancelled() {
                return Err(CatalogFetchError::cancelled());
            }
            let page = parse_page(&response)?;
            // The whole payload is bounded before any of it is normalized, so
            // an oversized response is refused rather than truncated into a
            // generation that looks complete.
            seen = seen.saturating_add(page.models.len());
            if seen > MAX_MODELS {
                return Err(invalid_response("Gemini model list is too large"));
            }
            for row in page.models {
                let Some(model) = normalize_row(row)? else {
                    continue;
                };
                if rows.insert(model.id.clone(), model).is_some() {
                    return Err(invalid_response(
                        "Gemini model list contains duplicate model ids",
                    ));
                }
            }
            let Some(next) = page.next_page_token else {
                return finish(rows);
            };
            page_token = Some(validate_page_token(next)?);
        }
        Err(invalid_response(
            "Gemini model list exceeded its page budget",
        ))
    }
}

/// Register Gemini discovery into the shared model catalog registry.
#[must_use]
pub fn google_catalog_plugin(config: GoogleCatalogConfig) -> Box<dyn Plugin> {
    struct GoogleCatalogPlugin(GoogleCatalogConfig);

    impl Plugin for GoogleCatalogPlugin {
        fn name(&self) -> &'static str {
            "catalog-google"
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
                GOOGLE_PROVIDER,
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            // The OAuth mode genuinely depends on PGCP01; the API-key mode does
            // not, and declaring a dependency it never uses would make the
            // service look required for every Google world.
            match self.0.auth {
                ConfiguredAuth::ApiKey => &[SERVICE_MODELS, SERVICE_CREDENTIALS, SERVICE_HTTP],
                ConfiguredAuth::OAuthQuotaProject { .. } => &[
                    SERVICE_MODELS,
                    SERVICE_CREDENTIALS,
                    SERVICE_HTTP,
                    SERVICE_GCP_AUTH,
                ],
            }
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
            let credential = self.0.credential.clone();
            let source = match &self.0.auth {
                ConfiguredAuth::ApiKey => {
                    GeminiCatalog::api_key(http.as_ref().clone(), credentials, credential)
                }
                ConfiguredAuth::OAuthQuotaProject { profile } => {
                    let gcp = context
                        .get::<GcpAuthService>(SERVICE_GCP_AUTH)
                        .ok_or_else(|| CoreError::MissingService(SERVICE_GCP_AUTH.to_string()))?;
                    GeminiCatalog::oauth_quota_project(
                        http.as_ref().clone(),
                        credentials,
                        credential,
                        gcp,
                        profile.clone(),
                    )
                }
            }
            .and_then(|source| source.with_base_url(&self.0.base_url))
            .map_err(|error| CoreError::other(error.to_string()))?;
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(GoogleCatalogPlugin(config))
}

/// The documented list path plus its two documented page parameters.
fn page_url(base_url: &str, page_token: Option<&str>) -> String {
    match page_token {
        // A page token is an opaque server string, so it is percent-encoded
        // rather than trusted: a raw `&` or `#` would silently rewrite the
        // request's own parameters.
        Some(token) => format!(
            "{base_url}/{API_VERSION}/models?pageSize={PAGE_SIZE}&pageToken={}",
            percent_encode(token)
        ),
        None => format!("{base_url}/{API_VERSION}/models?pageSize={PAGE_SIZE}"),
    }
}

/// Percent-encode every byte outside the RFC 3986 unreserved set.
fn percent_encode(value: &str) -> String {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn validate_page_token(token: String) -> Result<String, CatalogFetchError> {
    if token.is_empty() || token.len() > MAX_PAGE_TOKEN_BYTES {
        return Err(invalid_response(
            "Gemini model list returned an unusable page token",
        ));
    }
    Ok(token)
}

fn finish(
    rows: BTreeMap<String, ModelDescriptor>,
) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
    // A credential that can reach the API always has at least one model it may
    // call; publishing an empty generation would replace last-good state with
    // nothing. The provider default is deliberately NOT required to be
    // present: a key or project may legitimately lack access to the flagship,
    // and that is a real account rather than a malformed response.
    if rows.is_empty() {
        return Err(invalid_response(
            "Gemini model list published no model that supports generateContent",
        ));
    }
    Ok(rows.into_values().collect())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsPage {
    /// Google omits empty repeated fields, so an absent array is an empty page
    /// rather than a malformed body.
    #[serde(default)]
    models: Vec<ModelRow>,
    /// "If this field is omitted, there are no more pages."
    #[serde(default)]
    next_page_token: Option<String>,
}

/// The documented `Model` resource. `name`, `baseModelId` and `version` are
/// documented as required; every other field is optional evidence whose
/// absence is Unknown, never a rejected generation and never Supported.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelRow {
    name: String,
    base_model_id: String,
    version: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    input_token_limit: Option<i64>,
    #[serde(default)]
    output_token_limit: Option<i64>,
    #[serde(default)]
    supported_generation_methods: Option<Vec<String>>,
    #[serde(default)]
    thinking: Option<bool>,
}

fn parse_page(response: &HttpResponse) -> Result<ModelsPage, CatalogFetchError> {
    require_json(response)?;
    serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("Gemini model list has an invalid JSON shape"))
}

fn require_json(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    if response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    }) {
        Ok(())
    } else {
        Err(invalid_response("Gemini model list response is not JSON"))
    }
}

/// Validate one documented row and normalize it in a single pass.
///
/// Returns `Ok(None)` for a well-formed row that does not advertise
/// `generateContent`. Such a row is a real model this credential may hold —
/// an embedding, speech or tuning-only model — but it is not a model a heycode
/// inference request can call, so publishing it would offer the user a model
/// the account cannot actually use. A row missing the field entirely is
/// dropped for the same reason: no evidence of callability is never promoted
/// into evidence of it.
fn normalize_row(row: ModelRow) -> Result<Option<ModelDescriptor>, CatalogFetchError> {
    let Some(id) = row.name.strip_prefix(MODEL_NAME_PREFIX) else {
        return Err(invalid_response(
            "Gemini model list contains a row that is not a model resource",
        ));
    };
    if !safe_model_id(id) {
        return Err(invalid_response(
            "Gemini model list contains an invalid model id",
        ));
    }
    // `baseModelId` and `version` are documented as required and publish
    // nothing this descriptor carries, so they are checked for shape only,
    // never normalized into metadata they were not meant to be.
    if !safe_metadata(&row.base_model_id) || !safe_metadata(&row.version) {
        return Err(invalid_response(
            "Gemini model list contains an invalid model identity",
        ));
    }
    let display_name = match row.display_name.as_deref() {
        Some(value) if safe_display_text(value) => value.trim().to_owned(),
        Some(_) => {
            return Err(invalid_response(
                "Gemini model list contains an invalid display name",
            ));
        }
        None => id.to_owned(),
    };
    let context_window = positive_limit(row.input_token_limit)?;
    let max_output_tokens = positive_limit(row.output_token_limit)?;

    let Some(methods) = row.supported_generation_methods.as_ref() else {
        return Ok(None);
    };
    if !methods
        .iter()
        .any(|method| method == GENERATE_CONTENT_METHOD)
    {
        return Ok(None);
    }

    let mut capabilities = ModelCapabilities::unknown();
    // `thinking` is the only capability field the `Model` resource publishes.
    // Tools, images, documents, structured output, provider-hosted web,
    // native compaction and prompt caching have no field here, so they stay
    // Unknown: protocol compatibility is not model capability evidence.
    capabilities.reasoning = match row.thinking {
        Some(true) => CapabilitySupport::Supported,
        Some(false) => CapabilitySupport::Unsupported,
        None => CapabilitySupport::Unknown,
    };

    Ok(Some(ModelDescriptor {
        display_name,
        id: id.to_owned(),
        // `baseModelId` names a family several rows share, and one row's id
        // can equal another row's base id. Publishing it as an alias would
        // create exactly the alias/id collision that invalidates a whole
        // candidate generation, so no alias is published.
        aliases: Vec::new(),
        created_at_ms: None,
        context_window,
        max_output_tokens,
        // The endpoint publishes no lifecycle, deprecation or shutdown field.
        // The documentation marks preview variants in prose, and an id
        // substring is not provider evidence.
        lifecycle: ModelLifecycle::unknown(),
        capabilities,
        // No Gemini API endpoint publishes per-token prices, so nothing is
        // transcribed here as if it were API evidence.
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }))
}

/// A published token limit is optional; a zero or negative one contradicts the
/// field's meaning and rejects the generation rather than becoming "unknown".
fn positive_limit(value: Option<i64>) -> Result<Option<u64>, CatalogFetchError> {
    let Some(limit) = value else {
        return Ok(None);
    };
    u64::try_from(limit)
        .ok()
        .filter(|limit| *limit > 0)
        .map(Some)
        .ok_or_else(|| invalid_response("Gemini model list contains a non-positive token limit"))
}

/// Model ids stay inside an unreserved URL charset so a request path needs no
/// escaping. `:` is excluded deliberately: it separates the model from the
/// method in `models/{model}:generateContent`, so an id containing one could
/// redirect a call to a different method.
pub(crate) fn safe_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

fn safe_metadata(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_METADATA_BYTES
        && !value.chars().any(char::is_control)
}

fn safe_display_text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_DISPLAY_NAME_BYTES
        && !value.chars().any(char::is_control)
}

pub(crate) fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: GOOGLE_PROVIDER.to_owned(),
        display_name: GOOGLE_DISPLAY_NAME.to_owned(),
        protocols: vec![ProviderProtocol::GeminiGenerateContent],
    }
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized()),
        429 | 500..=599 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "Gemini model list is temporarily unavailable",
        )),
        _ => Err(invalid_response(
            "Gemini model list returned an unexpected HTTP status",
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
                "Gemini model list is temporarily unavailable",
            )
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "Gemini model list network request failed",
        ),
        _ => invalid_response("Gemini model list transport response is invalid"),
    }
}

fn unauthorized() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        "Google credential is missing or unauthorized",
    )
}

fn invalid_response(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn typed_http_timeout_preserves_the_catalog_network_class() {
        let failure = map_transport_error(TransportError::Timeout);
        assert_eq!(failure.kind(), CatalogFailureKind::Network);
        assert_eq!(
            failure.message(),
            "Gemini model list network request failed"
        );
    }

    #[test]
    fn model_ids_outside_the_unreserved_charset_are_refused() {
        assert!(safe_model_id("gemini-3.7-flash"));
        assert!(safe_model_id("gemini-1.5-flash-001"));
        for rejected in [
            "",
            "gemini flash",
            "gemini/flash",
            "gemini-3.7-flash?pageSize=1",
            "a&b",
            "../models",
            // A colon would separate a method in `models/{model}:method`.
            "gemini-3.7-flash:generateContent",
        ] {
            assert!(!safe_model_id(rejected), "`{rejected}` must be refused");
        }
    }

    #[test]
    fn an_opaque_page_token_is_percent_encoded_before_it_reaches_a_url() {
        assert_eq!(percent_encode("AbC-1._~9"), "AbC-1._~9");
        assert_eq!(
            percent_encode("a+b/c=d&pageSize=1#x"),
            "a%2Bb%2Fc%3Dd%26pageSize%3D1%23x"
        );
        assert_eq!(percent_encode(" "), "%20");
    }

    #[test]
    fn a_published_token_limit_must_be_positive_or_it_rejects_the_row() {
        assert_eq!(positive_limit(None).unwrap(), None);
        assert_eq!(positive_limit(Some(1_048_576)).unwrap(), Some(1_048_576));
        for rejected in [Some(0), Some(-1), Some(i64::MIN)] {
            assert!(
                positive_limit(rejected).is_err(),
                "`{rejected:?}` must be refused"
            );
        }
    }
}

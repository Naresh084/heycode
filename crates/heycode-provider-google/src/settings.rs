//! Restart-applied PGCP05/PGCP06 Google inference policy.

use heycode_authorization_gcp::GcpProfileRequest;
use heycode_core::{
    Context, ContributionKind, CoreError, CoreResult, Plugin, PluginContributionKind,
    PluginContributionSpec, PluginDescriptor, ServiceKey,
};
use heycode_credentials::CredentialQuery;
use heycode_settings::{
    SettingsApplies, SettingsDefinition, SettingsFieldPath, SettingsNamespace, SettingsSchema,
    SettingsService, SettingsSnapshot,
};
use serde::Deserialize;

use crate::{
    CodeExecutionRequest, ExternalApiAuth, ExternalApiKeyLocation, ExternalGroundingRequest,
    GeminiCacheRequest, GoogleGeminiModelEvidence, GoogleGeminiPolicy, GoogleInferencePluginConfig,
    GoogleInferencePluginError, GoogleSearchRequest, google_inference_plugin,
};

/// Settings namespace owned by the PGCP05/PGCP06 policy plugin.
pub const GOOGLE_INFERENCE_SETTINGS_NAMESPACE: &str = "google-inference";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SearchMode {
    Disabled,
    Web,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ToggleMode {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CacheModeWire {
    Disabled,
    Implicit,
    Explicit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureWire<M> {
    mode: M,
    models: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheWire {
    mode: CacheModeWire,
    resource: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeveloperWire {
    google_search: FeatureWire<SearchMode>,
    code_execution: FeatureWire<ToggleMode>,
    cache: CacheWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ExternalGroundingMode {
    Disabled,
    SimpleSearch,
    ElasticSearch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ExternalAuthMode {
    NoAuth,
    SecretManagerApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ExternalAuthLocationWire {
    Query,
    Header,
    Path,
    Body,
    Cookie,
}

impl ExternalAuthLocationWire {
    const fn into_policy(self) -> ExternalApiKeyLocation {
        match self {
            Self::Query => ExternalApiKeyLocation::Query,
            Self::Header => ExternalApiKeyLocation::Header,
            Self::Path => ExternalApiKeyLocation::Path,
            Self::Body => ExternalApiKeyLocation::Body,
            Self::Cookie => ExternalApiKeyLocation::Cookie,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalAuthWire {
    mode: ExternalAuthMode,
    secret_version: String,
    name: String,
    location: ExternalAuthLocationWire,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ElasticSearchWire {
    index: String,
    search_template: String,
    num_hits: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalGroundingWire {
    mode: ExternalGroundingMode,
    models: Vec<String>,
    endpoint: String,
    auth: ExternalAuthWire,
    elastic_search: ElasticSearchWire,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VertexWire {
    external_grounding: ExternalGroundingWire,
    cache: CacheWire,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsWire {
    developer: DeveloperWire,
    vertex: VertexWire,
}

#[derive(Clone, PartialEq, Eq)]
struct EvidencedRequest<T> {
    request: T,
    models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheProduct {
    Developer,
    Vertex,
}

/// One resolved PGCP05/PGCP06 Settings generation.
#[derive(Clone, PartialEq, Eq)]
pub struct GoogleInferenceSettings {
    developer_search: Option<EvidencedRequest<GoogleSearchRequest>>,
    developer_code: Option<EvidencedRequest<CodeExecutionRequest>>,
    developer_cache: Option<GeminiCacheRequest>,
    vertex_external: Option<EvidencedRequest<ExternalGroundingRequest>>,
    vertex_cache: Option<GeminiCacheRequest>,
}

impl std::fmt::Debug for GoogleInferenceSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleInferenceSettings")
            .field(
                "developer_search_models",
                &self
                    .developer_search
                    .as_ref()
                    .map(|feature| feature.models.len()),
            )
            .field(
                "developer_code_models",
                &self
                    .developer_code
                    .as_ref()
                    .map(|feature| feature.models.len()),
            )
            .field(
                "developer_cache_mode",
                &self.developer_cache.as_ref().map(GeminiCacheRequest::mode),
            )
            .field(
                "vertex_external_models",
                &self
                    .vertex_external
                    .as_ref()
                    .map(|feature| feature.models.len()),
            )
            .field(
                "vertex_cache_mode",
                &self.vertex_cache.as_ref().map(GeminiCacheRequest::mode),
            )
            .finish()
    }
}

impl GoogleInferenceSettings {
    /// Parse one complete resolved namespace value.
    ///
    /// # Errors
    /// Missing/unknown fields, cross-product cache resources, literal secret
    /// fields, or invalid feature configuration fail without echoing values.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, GoogleInferenceSettingsError> {
        let wire: SettingsWire = serde_json::from_value(value.clone())
            .map_err(|_| GoogleInferenceSettingsError::InvalidSettings)?;
        let developer_search = match wire.developer.google_search.mode {
            SearchMode::Disabled => {
                validate_models(&wire.developer.google_search.models, false)?;
                None
            }
            SearchMode::Web => Some(EvidencedRequest {
                request: GoogleSearchRequest::web(),
                models: validate_models(&wire.developer.google_search.models, true)?,
            }),
        };
        let developer_code = match wire.developer.code_execution.mode {
            ToggleMode::Disabled => {
                validate_models(&wire.developer.code_execution.models, false)?;
                None
            }
            ToggleMode::Enabled => Some(EvidencedRequest {
                request: CodeExecutionRequest::new(),
                models: validate_models(&wire.developer.code_execution.models, true)?,
            }),
        };
        let developer_cache = parse_cache(wire.developer.cache, CacheProduct::Developer)?;
        let vertex_external = parse_external_grounding(wire.vertex.external_grounding)?;
        let vertex_cache = parse_cache(wire.vertex.cache, CacheProduct::Vertex)?;
        Ok(Self {
            developer_search,
            developer_code,
            developer_cache,
            vertex_external,
            vertex_cache,
        })
    }

    /// Parse the exact registered restart-applied Google inference snapshot.
    ///
    /// # Errors
    /// Wrong namespace/application timing, unproved wire exposure, or invalid
    /// resolved data fails closed.
    pub fn from_snapshot(
        snapshot: &SettingsSnapshot,
    ) -> Result<Self, GoogleInferenceSettingsError> {
        if snapshot.namespace().as_str() != GOOGLE_INFERENCE_SETTINGS_NAMESPACE
            || snapshot.applies() != SettingsApplies::Restart
            || !snapshot.wire_exposed()
        {
            return Err(GoogleInferenceSettingsError::Unavailable);
        }
        Self::from_value(snapshot.resolved())
    }

    fn developer_policy(&self) -> Result<GoogleGeminiPolicy, GoogleInferenceSettingsError> {
        let mut policy = GoogleGeminiPolicy::none();
        if let Some(feature) = &self.developer_search {
            policy = policy
                .with_google_search(feature.request.clone(), feature.models.clone())
                .map_err(map_plugin_error)?;
        }
        if let Some(feature) = &self.developer_code {
            policy = policy
                .with_code_execution(feature.request, feature.models.clone())
                .map_err(map_plugin_error)?;
        }
        if let Some(cache) = &self.developer_cache {
            policy = policy.with_cache(cache.clone()).map_err(map_plugin_error)?;
        }
        Ok(policy)
    }

    fn vertex_policy(&self) -> Result<GoogleGeminiPolicy, GoogleInferenceSettingsError> {
        let mut policy = GoogleGeminiPolicy::none();
        if let Some(feature) = &self.vertex_external {
            policy = policy
                .with_external_grounding(feature.request.clone(), feature.models.clone())
                .map_err(map_plugin_error)?;
        }
        if let Some(cache) = &self.vertex_cache {
            policy = policy.with_cache(cache.clone()).map_err(map_plugin_error)?;
        }
        Ok(policy)
    }

    /// Build one exact Gemini Developer inference config.
    ///
    /// Capability support stays on the supplied model descriptors. Settings
    /// only selects a policy and cannot promote Unknown evidence.
    ///
    /// # Errors
    /// Credential, model-evidence, or provider-policy inconsistency.
    pub fn developer_config(
        &self,
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
    ) -> Result<GoogleInferencePluginConfig, GoogleInferenceSettingsError> {
        GoogleInferencePluginConfig::developer(credential, evidence, self.developer_policy()?)
            .map_err(map_plugin_error)
    }

    /// Build one explicit-endpoint Vertex Gemini inference config.
    ///
    /// # Errors
    /// Endpoint, credential, model-evidence, or provider-policy inconsistency.
    pub fn vertex_config(
        &self,
        base_url: impl Into<String>,
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
    ) -> Result<GoogleInferencePluginConfig, GoogleInferenceSettingsError> {
        GoogleInferencePluginConfig::vertex(base_url, credential, evidence, self.vertex_policy()?)
            .map_err(map_plugin_error)
    }

    /// Build one lazy GCP-profile-backed Vertex Gemini inference config.
    ///
    /// # Errors
    /// Explicit project/location, credential, maintained model evidence, or
    /// provider-policy inconsistency.
    pub fn lazy_vertex_config(
        &self,
        profile: GcpProfileRequest,
        credential: CredentialQuery,
    ) -> Result<GoogleInferencePluginConfig, GoogleInferenceSettingsError> {
        GoogleInferencePluginConfig::lazy_vertex(profile, credential, self.vertex_policy()?)
            .map_err(map_plugin_error)
    }
}

/// Closed Google inference Settings/configuration failure without values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GoogleInferenceSettingsError {
    /// Settings service or namespace is unavailable.
    Unavailable,
    /// Resolved Settings data is malformed or cross-product.
    InvalidSettings,
    /// Feature model ids are absent from exact product evidence.
    PolicyModelUnproven,
    /// The resulting provider config is inconsistent.
    InvalidInferenceConfig,
}

impl std::fmt::Display for GoogleInferenceSettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "Google inference settings are unavailable",
            Self::InvalidSettings => "Google inference settings are invalid",
            Self::PolicyModelUnproven => "Google inference policy model evidence is unproven",
            Self::InvalidInferenceConfig => "Google inference configuration is invalid",
        })
    }
}

impl std::error::Error for GoogleInferenceSettingsError {}

fn map_plugin_error(error: GoogleInferencePluginError) -> GoogleInferenceSettingsError {
    match error {
        GoogleInferencePluginError::PolicyModelNotEvidenced => {
            GoogleInferenceSettingsError::PolicyModelUnproven
        }
        _ => GoogleInferenceSettingsError::InvalidInferenceConfig,
    }
}

fn validate_models(
    models: &[String],
    required: bool,
) -> Result<Vec<String>, GoogleInferenceSettingsError> {
    if (required && models.is_empty())
        || models.len() > 64
        || models.iter().any(|model| !safe_model_id(model))
    {
        return Err(GoogleInferenceSettingsError::InvalidSettings);
    }
    let mut unique = std::collections::BTreeSet::new();
    for model in models {
        if !unique.insert(model.as_str()) {
            return Err(GoogleInferenceSettingsError::InvalidSettings);
        }
    }
    Ok(models.to_vec())
}

fn safe_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model.trim() == model
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn parse_cache(
    wire: CacheWire,
    product: CacheProduct,
) -> Result<Option<GeminiCacheRequest>, GoogleInferenceSettingsError> {
    match wire.mode {
        CacheModeWire::Disabled if wire.resource.is_empty() => Ok(None),
        CacheModeWire::Implicit if wire.resource.is_empty() => {
            Ok(Some(GeminiCacheRequest::implicit()))
        }
        CacheModeWire::Explicit => match product {
            CacheProduct::Developer => GeminiCacheRequest::explicit(wire.resource),
            CacheProduct::Vertex => GeminiCacheRequest::vertex_explicit(wire.resource),
        }
        .map(Some)
        .map_err(|_| GoogleInferenceSettingsError::InvalidSettings),
        CacheModeWire::Disabled | CacheModeWire::Implicit => {
            Err(GoogleInferenceSettingsError::InvalidSettings)
        }
    }
}

fn parse_external_grounding(
    wire: ExternalGroundingWire,
) -> Result<Option<EvidencedRequest<ExternalGroundingRequest>>, GoogleInferenceSettingsError> {
    if wire.mode == ExternalGroundingMode::Disabled {
        if !wire.models.is_empty()
            || !wire.endpoint.is_empty()
            || wire.auth.mode != ExternalAuthMode::NoAuth
            || !wire.auth.secret_version.is_empty()
            || !wire.auth.name.is_empty()
            || !wire.elastic_search.index.is_empty()
            || !wire.elastic_search.search_template.is_empty()
            || wire.elastic_search.num_hits.is_some()
        {
            return Err(GoogleInferenceSettingsError::InvalidSettings);
        }
        return Ok(None);
    }
    let models = validate_models(&wire.models, true)?;
    let auth = match wire.auth.mode {
        ExternalAuthMode::NoAuth
            if wire.auth.secret_version.is_empty() && wire.auth.name.is_empty() =>
        {
            ExternalApiAuth::no_auth()
        }
        ExternalAuthMode::SecretManagerApiKey => ExternalApiAuth::api_key_secret(
            wire.auth.secret_version,
            wire.auth.name,
            wire.auth.location.into_policy(),
        )
        .map_err(|_| GoogleInferenceSettingsError::InvalidSettings)?,
        ExternalAuthMode::NoAuth => return Err(GoogleInferenceSettingsError::InvalidSettings),
    };
    let request = match wire.mode {
        ExternalGroundingMode::SimpleSearch
            if wire.elastic_search.index.is_empty()
                && wire.elastic_search.search_template.is_empty()
                && wire.elastic_search.num_hits.is_none() =>
        {
            ExternalGroundingRequest::simple_search(wire.endpoint, auth)
        }
        ExternalGroundingMode::ElasticSearch => ExternalGroundingRequest::elastic_search(
            wire.endpoint,
            auth,
            wire.elastic_search.index,
            wire.elastic_search.search_template,
            wire.elastic_search.num_hits,
        ),
        ExternalGroundingMode::Disabled | ExternalGroundingMode::SimpleSearch => {
            return Err(GoogleInferenceSettingsError::InvalidSettings);
        }
    }
    .map_err(|_| GoogleInferenceSettingsError::InvalidSettings)?;
    Ok(Some(EvidencedRequest { request, models }))
}

/// Exact Settings namespace identity.
///
/// # Errors
/// Static namespace validation failure.
pub fn google_inference_settings_namespace()
-> Result<SettingsNamespace, heycode_settings::SettingsError> {
    SettingsNamespace::new(GOOGLE_INFERENCE_SETTINGS_NAMESPACE)
}

/// Build the restart-applied, wire-visible PGCP05/PGCP06 Settings contract.
///
/// Developer and Vertex policies are separate objects, so an explicit cache
/// resource or provider-native tool can never cross products. Defaults disable
/// every provider-executed feature.
///
/// # Errors
/// Static namespace/schema/default/path validation failure.
pub fn google_inference_settings_definition()
-> Result<SettingsDefinition, heycode_settings::SettingsError> {
    let defaults = serde_json::json!({
        "developer":{
            "google_search":{"mode":"disabled","models":[]},
            "code_execution":{"mode":"disabled","models":[]},
            "cache":{"mode":"disabled","resource":""}
        },
        "vertex":{
            "external_grounding":{
                "mode":"disabled",
                "models":[],
                "endpoint":"",
                "auth":{
                    "mode":"no-auth",
                    "secret_version":"",
                    "name":"",
                    "location":"header"
                },
                "elastic_search":{
                    "index":"",
                    "search_template":"",
                    "num_hits":null
                }
            },
            "cache":{"mode":"disabled","resource":""}
        }
    });
    let model_list = || {
        serde_json::json!({
            "type":"array",
            "maxItems":64,
            "uniqueItems":true,
            "items":{"type":"string","minLength":1,"maxLength":256}
        })
    };
    let cache = || {
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["mode","resource"],
            "properties":{
                "mode":{"type":"string","enum":["disabled","implicit","explicit"]},
                "resource":{"type":"string","maxLength":512}
            }
        })
    };
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["developer","vertex"],
            "properties":{
                "developer":{
                    "type":"object",
                    "additionalProperties":false,
                    "required":["google_search","code_execution","cache"],
                    "properties":{
                        "google_search":{
                            "type":"object","additionalProperties":false,
                            "required":["mode","models"],
                            "properties":{
                                "mode":{"type":"string","enum":["disabled","web"]},
                                "models":model_list()
                            }
                        },
                        "code_execution":{
                            "type":"object","additionalProperties":false,
                            "required":["mode","models"],
                            "properties":{
                                "mode":{"type":"string","enum":["disabled","enabled"]},
                                "models":model_list()
                            }
                        },
                        "cache":cache()
                    }
                },
                "vertex":{
                    "type":"object",
                    "additionalProperties":false,
                    "required":["external_grounding","cache"],
                    "properties":{
                        "external_grounding":{
                            "type":"object","additionalProperties":false,
                            "required":["mode","models","endpoint","auth","elastic_search"],
                            "properties":{
                                "mode":{"type":"string","enum":[
                                    "disabled","simple-search","elastic-search"
                                ]},
                                "models":model_list(),
                                "endpoint":{"type":"string","maxLength":4096},
                                "auth":{
                                    "type":"object","additionalProperties":false,
                                    "required":["mode","secret_version","name","location"],
                                    "properties":{
                                        "mode":{"type":"string","enum":[
                                            "no-auth","secret-manager-api-key"
                                        ]},
                                        "secret_version":{"type":"string","maxLength":512},
                                        "name":{"type":"string","maxLength":128},
                                        "location":{"type":"string","enum":[
                                            "query","header","path","body","cookie"
                                        ]}
                                    }
                                },
                                "elastic_search":{
                                    "type":"object","additionalProperties":false,
                                    "required":["index","search_template","num_hits"],
                                    "properties":{
                                        "index":{"type":"string","maxLength":8192},
                                        "search_template":{"type":"string","maxLength":8192},
                                        "num_hits":{"type":["integer","null"],"minimum":1,
                                            "maximum":2147483647}
                                    }
                                }
                            }
                        },
                        "cache":cache()
                    }
                }
            }
        }),
        defaults,
        |value| {
            GoogleInferenceSettings::from_value(value)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?
    .with_public_path(SettingsFieldPath::new("developer")?)
    .with_public_path(SettingsFieldPath::new("vertex")?)
    .with_wire_exposure();
    Ok(
        SettingsDefinition::new(google_inference_settings_namespace()?, schema)
            .with_applies(SettingsApplies::Restart),
    )
}

/// Resolve the registered PGCP05/PGCP06 policy.
///
/// # Errors
/// Missing/poisoned Settings state or invalid resolved policy fails closed.
pub fn resolve_google_inference_settings(
    settings: &SettingsService,
) -> Result<GoogleInferenceSettings, GoogleInferenceSettingsError> {
    let namespace = google_inference_settings_namespace()
        .map_err(|_| GoogleInferenceSettingsError::Unavailable)?;
    let snapshot = settings
        .get(&namespace)
        .map_err(|_| GoogleInferenceSettingsError::Unavailable)?
        .ok_or(GoogleInferenceSettingsError::Unavailable)?;
    GoogleInferenceSettings::from_snapshot(&snapshot)
}

/// Resolve Settings and construct one Gemini Developer config.
///
/// # Errors
/// Settings, credential, or exact model-policy validation failure.
pub fn google_developer_config_from_settings(
    settings: &SettingsService,
    credential: CredentialQuery,
    evidence: GoogleGeminiModelEvidence,
) -> Result<GoogleInferencePluginConfig, GoogleInferenceSettingsError> {
    resolve_google_inference_settings(settings)?.developer_config(credential, evidence)
}

/// Resolve Settings and construct one explicit-endpoint Vertex Gemini config.
///
/// # Errors
/// Settings, endpoint, credential, or exact model-policy validation failure.
pub fn google_vertex_config_from_settings(
    settings: &SettingsService,
    base_url: impl Into<String>,
    credential: CredentialQuery,
    evidence: GoogleGeminiModelEvidence,
) -> Result<GoogleInferencePluginConfig, GoogleInferenceSettingsError> {
    resolve_google_inference_settings(settings)?.vertex_config(base_url, credential, evidence)
}

/// Resolve Settings and construct one lazy GCP-profile-backed Vertex config.
///
/// # Errors
/// Settings, explicit profile, credential, or maintained policy validation
/// failure.
pub fn google_lazy_vertex_config_from_settings(
    settings: &SettingsService,
    profile: GcpProfileRequest,
    credential: CredentialQuery,
) -> Result<GoogleInferencePluginConfig, GoogleInferenceSettingsError> {
    resolve_google_inference_settings(settings)?.lazy_vertex_config(profile, credential)
}

/// Build a Gemini Developer plugin whose policy resolves from the composed
/// Settings service during activation.
///
/// # Errors
/// Credential, evidence, or baseline provider admission failure.
pub fn google_developer_settings_plugin(
    credential: CredentialQuery,
    evidence: GoogleGeminiModelEvidence,
) -> Result<Box<dyn Plugin>, GoogleInferenceSettingsError> {
    let prototype = google_inference_plugin(
        GoogleInferencePluginConfig::developer(
            credential.clone(),
            evidence.clone(),
            GoogleGeminiPolicy::none(),
        )
        .map_err(map_plugin_error)?,
    );
    Ok(Box::new(GoogleSettingsInferencePlugin {
        route: GoogleSettingsRoute::Developer {
            credential,
            evidence,
        },
        inventory: prototype.inventory(),
    }))
}

/// Build a lazy Vertex Gemini plugin whose provider tools/cache resolve from
/// the composed Settings service during activation.
///
/// # Errors
/// Profile, credential, maintained evidence, or baseline provider admission
/// failure.
pub fn google_lazy_vertex_settings_plugin(
    profile: GcpProfileRequest,
    credential: CredentialQuery,
) -> Result<Box<dyn Plugin>, GoogleInferenceSettingsError> {
    let prototype = google_inference_plugin(
        GoogleInferencePluginConfig::lazy_vertex(
            profile.clone(),
            credential.clone(),
            GoogleGeminiPolicy::none(),
        )
        .map_err(map_plugin_error)?,
    );
    Ok(Box::new(GoogleSettingsInferencePlugin {
        route: GoogleSettingsRoute::LazyVertex {
            profile,
            credential,
        },
        inventory: prototype.inventory(),
    }))
}

enum GoogleSettingsRoute {
    Developer {
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
    },
    LazyVertex {
        profile: GcpProfileRequest,
        credential: CredentialQuery,
    },
}

struct GoogleSettingsInferencePlugin {
    route: GoogleSettingsRoute,
    inventory: Vec<PluginContributionSpec>,
}

impl Plugin for GoogleSettingsInferencePlugin {
    fn name(&self) -> &'static str {
        match &self.route {
            GoogleSettingsRoute::Developer { .. } => "inference-google-gemini",
            GoogleSettingsRoute::LazyVertex { .. } => "inference-google-vertex",
        }
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            self.name(),
            env!("CARGO_PKG_VERSION"),
            &[
                PluginContributionKind::Provider,
                PluginContributionKind::Tool,
            ],
        )
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        self.inventory.clone()
    }

    fn inject(&self) -> &'static [ServiceKey] {
        match &self.route {
            GoogleSettingsRoute::Developer { .. } => &[
                heycode_settings::SERVICE_SETTINGS,
                heycode_llm::SERVICE_PROVIDERS,
                heycode_http::SERVICE_HTTP,
                heycode_credentials::SERVICE_CREDENTIALS,
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
            ],
            GoogleSettingsRoute::LazyVertex { .. } => &[
                heycode_settings::SERVICE_SETTINGS,
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_MODELS,
                heycode_http::SERVICE_HTTP,
                heycode_credentials::SERVICE_CREDENTIALS,
                heycode_authorization_gcp::SERVICE_GCP_AUTH,
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
            ],
        }
    }

    fn apply(&self, context: &mut Context) -> CoreResult<()> {
        let settings = context
            .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
            .ok_or_else(|| CoreError::other("Google inference settings service is unavailable"))?;
        let config = match &self.route {
            GoogleSettingsRoute::Developer {
                credential,
                evidence,
            } => google_developer_config_from_settings(
                settings.as_ref(),
                credential.clone(),
                evidence.clone(),
            ),
            GoogleSettingsRoute::LazyVertex {
                profile,
                credential,
            } => google_lazy_vertex_config_from_settings(
                settings.as_ref(),
                profile.clone(),
                credential.clone(),
            ),
        }
        .map_err(|error| CoreError::other(error.to_string()))?;
        let plugin = google_inference_plugin(config);
        for contribution in plugin.inventory() {
            if self.inventory.contains(&contribution) {
                continue;
            }
            if contribution.kind != ContributionKind::NativeTool {
                return Err(CoreError::other(
                    "Google inference Settings changed a static contribution",
                ));
            }
            context.contribute(contribution.kind, contribution.name)?;
        }
        plugin.apply(context)
    }
}

/// Register the PGCP05/PGCP06 namespace as a Context effect.
#[must_use]
pub fn google_inference_settings_plugin() -> Box<dyn Plugin> {
    struct GoogleInferenceSettingsPlugin;

    impl Plugin for GoogleInferenceSettingsPlugin {
        fn name(&self) -> &'static str {
            "settings-google-inference"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                GOOGLE_INFERENCE_SETTINGS_NAMESPACE,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = context
                .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service type mismatch"))?;
            settings
                .register(
                    context,
                    google_inference_settings_definition().map_err(|_| {
                        CoreError::other("Google inference settings schema is invalid")
                    })?,
                )
                .map(|_| ())
                .map_err(|_| CoreError::other("Google inference settings are invalid"))
        }
    }

    Box::new(GoogleInferenceSettingsPlugin)
}

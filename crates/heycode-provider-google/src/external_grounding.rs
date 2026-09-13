//! PGCP05 Vertex external-API grounding request boundary.
//!
//! Vertex can have Gemini retrieve grounding context from a caller-owned
//! search endpoint. The request contains endpoint and schema metadata plus an
//! optional authentication configuration. This module deliberately supports
//! Secret Manager references and no-auth only: an API-key value has no place in
//! [`heycode_core::ProviderRequestOption`], because that option is durable request
//! evidence. A future direct-value path needs a separate operation-time secret
//! injection seam and must never serialize that value into the request header.
//!
//! Sources:
//! - <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/grounding/grounding-with-your-search-api>
//! - <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/reference/rpc/google.cloud.aiplatform.v1#externalapi>
//! - <https://docs.cloud.google.com/secret-manager/docs/reference/rpc/google.cloud.secretmanager.v1>

use heycode_core::ProviderRequestOption;

/// Provider identity reserved for Gemini served by Vertex AI.
pub const GOOGLE_VERTEX_PROVIDER: &str = "vertex-google";

/// Provider request-option kind carrying one `retrieval.externalApi` tool.
pub const GOOGLE_EXTERNAL_GROUNDING_OPTION_KIND: &str = "google-external-grounding";

/// Logical server-tool capability used by normalized external grounding.
pub const GOOGLE_EXTERNAL_GROUNDING_LOGICAL: &str = "external_grounding";

/// Provider-native tool name retained on normalized call events.
pub const GOOGLE_EXTERNAL_GROUNDING_TOOL_NAME: &str = "external_api";

const MAX_ENDPOINT_BYTES: usize = 4_096;
const MAX_SECRET_ID_BYTES: usize = 255;
const MAX_SECRET_ALIAS_BYTES: usize = 63;
const MAX_AUTH_NAME_BYTES: usize = 128;
const MAX_ELASTIC_FIELD_BYTES: usize = 8 * 1024;

/// External search schema selected for one Vertex retrieval tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalGroundingSpec {
    /// `POST {"query": string}` returning `[{snippet,uri}]` rows.
    SimpleSearch,
    /// Elasticsearch index/template retrieval.
    ElasticSearch(ExternalElasticSearchSpec),
}

impl ExternalGroundingSpec {
    fn wire_value(&self) -> &'static str {
        match self {
            Self::SimpleSearch => "SIMPLE_SEARCH",
            Self::ElasticSearch(_) => "ELASTIC_SEARCH",
        }
    }

    fn safe_value(&self) -> &'static str {
        match self {
            Self::SimpleSearch => "simple_search",
            Self::ElasticSearch(_) => "elastic_search",
        }
    }
}

/// Validated Elasticsearch parameters for external grounding.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalElasticSearchSpec {
    index: String,
    search_template: String,
    num_hits: Option<u32>,
}

impl std::fmt::Debug for ExternalElasticSearchSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExternalElasticSearchSpec")
            .field("index", &"[REDACTED]")
            .field("search_template", &"[REDACTED]")
            .field("num_hits", &self.num_hits)
            .finish()
    }
}

impl ExternalElasticSearchSpec {
    /// Validated Elasticsearch index.
    #[must_use]
    pub fn index(&self) -> &str {
        &self.index
    }

    /// Validated Elasticsearch search template.
    #[must_use]
    pub fn search_template(&self) -> &str {
        &self.search_template
    }

    /// Optional requested hit count.
    #[must_use]
    pub const fn num_hits(&self) -> Option<u32> {
        self.num_hits
    }
}

/// Where Vertex inserts a Secret Manager-backed API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalApiKeyLocation {
    /// Query parameter.
    Query,
    /// HTTP header.
    Header,
    /// URL path element.
    Path,
    /// Request body field.
    Body,
    /// Cookie.
    Cookie,
}

impl ExternalApiKeyLocation {
    fn wire_value(self) -> &'static str {
        match self {
            Self::Query => "HTTP_IN_QUERY",
            Self::Header => "HTTP_IN_HEADER",
            Self::Path => "HTTP_IN_PATH",
            Self::Body => "HTTP_IN_BODY",
            Self::Cookie => "HTTP_IN_COOKIE",
        }
    }
}

/// Authentication metadata that contains no credential value.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalApiAuth {
    kind: ExternalApiAuthKind,
}

#[derive(Clone, PartialEq, Eq)]
enum ExternalApiAuthKind {
    NoAuth,
    ApiKeySecret {
        resource: String,
        name: String,
        location: ExternalApiKeyLocation,
    },
}

impl std::fmt::Debug for ExternalApiAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExternalApiAuth")
            .field("kind", &self.safe_kind())
            .field(
                "secret_reference",
                &matches!(&self.kind, ExternalApiAuthKind::ApiKeySecret { .. })
                    .then_some("[REDACTED]"),
            )
            .finish()
    }
}

impl ExternalApiAuth {
    /// Configure an endpoint that performs no authentication.
    #[must_use]
    pub const fn no_auth() -> Self {
        Self {
            kind: ExternalApiAuthKind::NoAuth,
        }
    }

    /// Configure an API key held by Google Secret Manager.
    ///
    /// This takes only the resource name. Secret bytes are neither accepted nor
    /// resolved by this crate; Vertex's service agent performs that access.
    ///
    /// # Errors
    /// Invalid SecretVersion resource names or unsafe parameter names fail
    /// without echoing either value.
    pub fn api_key_secret(
        resource: impl Into<String>,
        name: impl Into<String>,
        location: ExternalApiKeyLocation,
    ) -> Result<Self, ExternalGroundingError> {
        let resource = resource.into();
        validate_secret_version(&resource)?;
        let name = name.into();
        if !safe_auth_name(&name) {
            return Err(ExternalGroundingError::InvalidAuthName);
        }
        Ok(Self {
            kind: ExternalApiAuthKind::ApiKeySecret {
                resource,
                name,
                location,
            },
        })
    }

    fn safe_kind(&self) -> &'static str {
        match &self.kind {
            ExternalApiAuthKind::NoAuth => "none",
            ExternalApiAuthKind::ApiKeySecret { .. } => "api_key_secret",
        }
    }

    fn wire_value(&self) -> serde_json::Value {
        match &self.kind {
            ExternalApiAuthKind::NoAuth => serde_json::json!({ "authType": "NO_AUTH" }),
            ExternalApiAuthKind::ApiKeySecret {
                resource,
                name,
                location,
            } => serde_json::json!({
                "authType": "API_KEY_AUTH",
                "apiKeyConfig": {
                    "apiKeySecret": resource,
                    "name": name,
                    "httpElementLocation": location.wire_value()
                }
            }),
        }
    }
}

/// One safe Vertex external-grounding request.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalGroundingRequest {
    endpoint: String,
    auth: ExternalApiAuth,
    spec: ExternalGroundingSpec,
}

impl std::fmt::Debug for ExternalGroundingRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExternalGroundingRequest")
            .field("endpoint", &"[REDACTED]")
            .field("auth", &self.auth)
            .field("spec", &self.spec)
            .finish()
    }
}

impl ExternalGroundingRequest {
    /// Configure the documented simple-search schema.
    ///
    /// # Errors
    /// Endpoint validation rejects non-HTTPS, credential-bearing, query-bearing,
    /// fragmented, hostless, or oversized URLs.
    pub fn simple_search(
        endpoint: impl Into<String>,
        auth: ExternalApiAuth,
    ) -> Result<Self, ExternalGroundingError> {
        Self::new(endpoint.into(), auth, ExternalGroundingSpec::SimpleSearch)
    }

    /// Configure the documented Elasticsearch schema.
    ///
    /// # Errors
    /// Same endpoint errors as [`Self::simple_search`]. Empty, control-bearing,
    /// or oversized index/template fields and a zero hit count are refused.
    pub fn elastic_search(
        endpoint: impl Into<String>,
        auth: ExternalApiAuth,
        index: impl Into<String>,
        search_template: impl Into<String>,
        num_hits: Option<u32>,
    ) -> Result<Self, ExternalGroundingError> {
        let index = index.into();
        let search_template = search_template.into();
        if !safe_elastic_field(&index)
            || !safe_elastic_field(&search_template)
            || num_hits == Some(0)
            || num_hits.is_some_and(|value| i32::try_from(value).is_err())
        {
            return Err(ExternalGroundingError::InvalidElasticSearch);
        }
        Self::new(
            endpoint.into(),
            auth,
            ExternalGroundingSpec::ElasticSearch(ExternalElasticSearchSpec {
                index,
                search_template,
                num_hits,
            }),
        )
    }

    fn new(
        endpoint: String,
        auth: ExternalApiAuth,
        spec: ExternalGroundingSpec,
    ) -> Result<Self, ExternalGroundingError> {
        if endpoint.len() > MAX_ENDPOINT_BYTES {
            return Err(ExternalGroundingError::InvalidEndpoint);
        }
        let request = heycode_http::HttpRequest::post(&endpoint, Vec::new())
            .map_err(|_| ExternalGroundingError::InvalidEndpoint)?;
        let endpoint = request.url().to_owned();
        if !endpoint.starts_with("https://") || endpoint.contains('?') || endpoint.contains('#') {
            return Err(ExternalGroundingError::InvalidEndpoint);
        }
        Ok(Self {
            endpoint,
            auth,
            spec,
        })
    }

    /// Validated external API schema.
    #[must_use]
    pub const fn spec(&self) -> &ExternalGroundingSpec {
        &self.spec
    }

    /// Exact `tools[]` entry for the Vertex GenerateContent request.
    #[must_use]
    pub fn tool_entry(&self) -> serde_json::Value {
        let mut external = serde_json::Map::from_iter([
            (
                "apiSpec".to_owned(),
                serde_json::json!(self.spec.wire_value()),
            ),
            ("endpoint".to_owned(), serde_json::json!(self.endpoint)),
            ("authConfig".to_owned(), self.auth.wire_value()),
        ]);
        if let ExternalGroundingSpec::ElasticSearch(spec) = &self.spec {
            let mut parameters = serde_json::Map::from_iter([
                ("index".to_owned(), serde_json::json!(spec.index)),
                (
                    "searchTemplate".to_owned(),
                    serde_json::json!(spec.search_template),
                ),
            ]);
            if let Some(num_hits) = spec.num_hits {
                parameters.insert("numHits".to_owned(), serde_json::json!(num_hits));
            }
            external.insert(
                "elasticSearchParams".to_owned(),
                serde_json::Value::Object(parameters),
            );
        }
        serde_json::json!({
            "retrieval": {
                "externalApi": serde_json::Value::Object(external)
            }
        })
    }

    /// Durable provider option containing references, never API-key bytes.
    ///
    /// # Errors
    /// Fixed provider metadata that no longer satisfies the shared option
    /// contract fails before request admission.
    pub fn provider_option(&self) -> Result<ProviderRequestOption, ExternalGroundingError> {
        ProviderRequestOption::new(
            GOOGLE_VERTEX_PROVIDER,
            GOOGLE_EXTERNAL_GROUNDING_OPTION_KIND,
            serde_json::json!({ "tool": self.tool_entry() }),
        )
        .map_err(|_| ExternalGroundingError::InvalidOption)
    }

    /// Safe normalized call input; endpoint and secret names stay out of UI.
    #[must_use]
    pub fn call_input(&self) -> serde_json::Value {
        serde_json::json!({
            "api_spec": self.spec.safe_value(),
            "auth": self.auth.safe_kind()
        })
    }
}

fn safe_auth_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_AUTH_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn safe_elastic_field(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ELASTIC_FIELD_BYTES
        && !value.chars().any(char::is_control)
}

fn validate_secret_version(resource: &str) -> Result<(), ExternalGroundingError> {
    let segments = resource.split('/').collect::<Vec<_>>();
    let ["projects", project, "secrets", secret, "versions", version] = segments.as_slice() else {
        return Err(ExternalGroundingError::InvalidSecretReference);
    };
    if heycode_authorization_gcp::GcpProjectId::new((*project).to_owned()).is_err()
        || secret.is_empty()
        || secret.len() > MAX_SECRET_ID_BYTES
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !safe_secret_version(version)
    {
        return Err(ExternalGroundingError::InvalidSecretReference);
    }
    Ok(())
}

fn safe_secret_version(value: &str) -> bool {
    if value == "latest" {
        return true;
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return !value.is_empty()
            && !value.starts_with('0')
            && value.parse::<i64>().is_ok_and(|version| version > 0);
    }
    value.len() <= MAX_SECRET_ALIAS_BYTES
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && value != "NEW"
}

/// External-grounding request admission failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExternalGroundingError {
    /// Endpoint is unsafe or outside the supported HTTPS shape.
    #[error("Vertex external-grounding endpoint is invalid")]
    InvalidEndpoint,
    /// Secret Manager version resource is malformed.
    #[error("Vertex external-grounding secret reference is invalid")]
    InvalidSecretReference,
    /// API-key parameter name is unsafe.
    #[error("Vertex external-grounding auth parameter name is invalid")]
    InvalidAuthName,
    /// Elasticsearch settings are empty, oversized, or invalid.
    #[error("Vertex external-grounding Elasticsearch settings are invalid")]
    InvalidElasticSearch,
    /// Safe request metadata failed the shared provider-option contract.
    #[error("Vertex external-grounding provider option is invalid")]
    InvalidOption,
}

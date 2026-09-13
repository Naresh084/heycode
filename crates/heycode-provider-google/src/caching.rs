//! PGCP06 context-cache request provenance and usage normalization.
//!
//! GenerateContent supports explicit caching through the request field
//! `cachedContent`; implicit caching sends no field. Both modes report the same
//! optional `cachedContentTokenCount` and `cacheTokensDetails` response fields,
//! so the response alone cannot identify which mode produced a hit. This module
//! binds usage to the committed request mode instead of guessing from a count.
//! Developer and Vertex explicit resource names are deliberately distinct;
//! accepting the short Developer name on Vertex would move a product identity
//! error from request admission to a remote 400.
//!
//! Sources:
//! - <https://ai.google.dev/gemini-api/docs/caching>
//! - <https://ai.google.dev/gemini-api/docs/generate-content/tokens>
//! - <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/context-cache/context-cache-use>
//! - <https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta>

use heycode_core::ProviderRequestOption;

/// Provider request-option kind carrying an explicit cache resource.
pub const GOOGLE_CONTEXT_CACHE_OPTION_KIND: &str = "google-context-cache";

const CACHE_PREFIX: &str = "cachedContents/";
const MAX_CACHE_ID_BYTES: usize = 256;
const MAX_CACHE_RESOURCE_BYTES: usize = 512;
const MAX_CACHE_DETAILS: usize = 64;

/// Whether one request relies on implicit or explicit context caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiCacheMode {
    /// No cache resource is sent; the service may reuse an implicit prefix.
    Implicit,
    /// A product-specific cached-content resource is sent explicitly.
    Explicit,
}

/// One committed context-cache request.
#[derive(Clone, PartialEq, Eq)]
pub struct GeminiCacheRequest {
    resource: CacheResource,
}

#[derive(Clone, PartialEq, Eq)]
enum CacheResource {
    Implicit,
    Developer(String),
    Vertex(String),
}

impl std::fmt::Debug for GeminiCacheRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeminiCacheRequest")
            .field("mode", &self.mode())
            .field("product", &self.product().map(GeminiCacheProduct::as_str))
            .field("resource", &self.resource().map(|_| "[REDACTED]"))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeminiCacheProduct {
    Developer,
    Vertex,
}

impl GeminiCacheProduct {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Developer => "developer",
            Self::Vertex => "vertex",
        }
    }
}

impl GeminiCacheRequest {
    /// Use the API's implicit caching behavior without adding a wire field.
    #[must_use]
    pub const fn implicit() -> Self {
        Self {
            resource: CacheResource::Implicit,
        }
    }

    /// Use one explicit `cachedContents/{id}` resource.
    ///
    /// # Errors
    /// A resource outside the documented collection, with an empty id, a path
    /// separator, or bytes outside the URL-unreserved set is refused.
    pub fn explicit(resource: impl Into<String>) -> Result<Self, CacheMetadataError> {
        let resource = resource.into();
        let Some(id) = resource.strip_prefix(CACHE_PREFIX) else {
            return Err(CacheMetadataError::InvalidResource);
        };
        if !safe_cache_id(id) {
            return Err(CacheMetadataError::InvalidResource);
        }
        Ok(Self {
            resource: CacheResource::Developer(resource),
        })
    }

    /// Use one full Vertex cached-content resource.
    ///
    /// # Errors
    /// The exact resource must be
    /// `projects/{project}/locations/{location}/cachedContents/{id}` with a
    /// validated project, Vertex location, and one URL-unreserved cache id.
    pub fn vertex_explicit(resource: impl Into<String>) -> Result<Self, CacheMetadataError> {
        let resource = resource.into();
        if resource.len() > MAX_CACHE_RESOURCE_BYTES {
            return Err(CacheMetadataError::InvalidResource);
        }
        let segments = resource.split('/').collect::<Vec<_>>();
        let [
            "projects",
            project,
            "locations",
            location,
            "cachedContents",
            id,
        ] = segments.as_slice()
        else {
            return Err(CacheMetadataError::InvalidResource);
        };
        if heycode_authorization_gcp::GcpProjectId::new((*project).to_owned()).is_err()
            || heycode_authorization_gcp::GcpLocation::new((*location).to_owned()).is_err()
            || !safe_cache_id(id)
        {
            return Err(CacheMetadataError::InvalidResource);
        }
        Ok(Self {
            resource: CacheResource::Vertex(resource),
        })
    }

    /// Cache mode committed by this request.
    #[must_use]
    pub const fn mode(&self) -> GeminiCacheMode {
        match &self.resource {
            CacheResource::Implicit => GeminiCacheMode::Implicit,
            CacheResource::Developer(_) | CacheResource::Vertex(_) => GeminiCacheMode::Explicit,
        }
    }

    /// Explicit resource name, absent for implicit caching.
    #[must_use]
    pub fn resource(&self) -> Option<&str> {
        match &self.resource {
            CacheResource::Implicit => None,
            CacheResource::Developer(resource) | CacheResource::Vertex(resource) => Some(resource),
        }
    }

    pub(crate) const fn product(&self) -> Option<GeminiCacheProduct> {
        match &self.resource {
            CacheResource::Implicit => None,
            CacheResource::Developer(_) => Some(GeminiCacheProduct::Developer),
            CacheResource::Vertex(_) => Some(GeminiCacheProduct::Vertex),
        }
    }

    pub(crate) fn supports_provider(&self, provider: &str) -> bool {
        match self.product() {
            None => matches!(
                provider,
                crate::catalog::GOOGLE_PROVIDER | crate::GOOGLE_VERTEX_PROVIDER
            ),
            Some(GeminiCacheProduct::Developer) => provider == crate::catalog::GOOGLE_PROVIDER,
            Some(GeminiCacheProduct::Vertex) => provider == crate::GOOGLE_VERTEX_PROVIDER,
        }
    }

    /// Provider request option for the explicit wire field.
    ///
    /// Implicit caching returns `None`: recording a provider option would claim
    /// bytes were sent when the documented behavior sends nothing.
    ///
    /// # Errors
    /// Fixed provider metadata that no longer satisfies the shared option
    /// contract fails before request admission.
    pub fn provider_option(&self) -> Result<Option<ProviderRequestOption>, CacheMetadataError> {
        let (provider, resource) = match &self.resource {
            CacheResource::Implicit => return Ok(None),
            CacheResource::Developer(resource) => (crate::catalog::GOOGLE_PROVIDER, resource),
            CacheResource::Vertex(resource) => (crate::GOOGLE_VERTEX_PROVIDER, resource),
        };
        ProviderRequestOption::new(
            provider,
            GOOGLE_CONTEXT_CACHE_OPTION_KIND,
            serde_json::json!({ "cachedContent": resource }),
        )
        .map(Some)
        .map_err(|_| CacheMetadataError::InvalidOption)
    }
}

fn safe_cache_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CACHE_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
}

/// Modality names published by `ModalityTokenCount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiModality {
    /// Provider explicitly reported no modality.
    Unspecified,
    /// Plain text.
    Text,
    /// Image input.
    Image,
    /// Video input.
    Video,
    /// Audio input.
    Audio,
    /// Document input such as PDF.
    Document,
}

impl GeminiModality {
    fn parse(value: &str) -> Result<Self, CacheMetadataError> {
        match value {
            "MODALITY_UNSPECIFIED" => Ok(Self::Unspecified),
            "TEXT" => Ok(Self::Text),
            "IMAGE" => Ok(Self::Image),
            "VIDEO" => Ok(Self::Video),
            "AUDIO" => Ok(Self::Audio),
            "DOCUMENT" => Ok(Self::Document),
            _ => Err(CacheMetadataError::UnknownModality),
        }
    }
}

/// One cached-input modality count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheTokenDetail {
    modality: GeminiModality,
    tokens: u64,
}

impl CacheTokenDetail {
    /// Reported modality.
    #[must_use]
    pub const fn modality(&self) -> GeminiModality {
        self.modality
    }

    /// Reported token count.
    #[must_use]
    pub const fn tokens(&self) -> u64 {
        self.tokens
    }
}

/// Cache usage bound to the request mode that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiCacheUsage {
    mode: GeminiCacheMode,
    prompt_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    details: Option<Vec<CacheTokenDetail>>,
}

impl GeminiCacheUsage {
    /// Parse one `usageMetadata` object without inventing absent counters.
    ///
    /// `promptTokenCount` includes cached tokens. The details list is retained
    /// independently; the discovery schema publishes no arithmetic identity
    /// requiring its sum to equal `cachedContentTokenCount`.
    ///
    /// # Errors
    /// Wrong types, negative counters, unknown modalities, too many details,
    /// or a cached count larger than the effective prompt fail.
    pub fn parse(
        request: &GeminiCacheRequest,
        usage: &serde_json::Value,
    ) -> Result<Self, CacheMetadataError> {
        let usage = usage.as_object().ok_or(CacheMetadataError::Malformed(
            "usageMetadata must be an object",
        ))?;
        let prompt_tokens = optional_counter(usage, "promptTokenCount")?;
        let cached_tokens = optional_counter(usage, "cachedContentTokenCount")?;
        if matches!((prompt_tokens, cached_tokens), (Some(prompt), Some(cached)) if cached > prompt)
        {
            return Err(CacheMetadataError::CachedExceedsPrompt);
        }
        let details = match usage
            .get("cacheTokensDetails")
            .filter(|value| !value.is_null())
        {
            None => None,
            Some(value) => {
                let rows = value.as_array().ok_or(CacheMetadataError::Malformed(
                    "cacheTokensDetails must be an array",
                ))?;
                if rows.len() > MAX_CACHE_DETAILS {
                    return Err(CacheMetadataError::TooManyDetails);
                }
                let mut normalized = Vec::with_capacity(rows.len());
                for row in rows {
                    let row = row.as_object().ok_or(CacheMetadataError::Malformed(
                        "cache token detail must be an object",
                    ))?;
                    let modality = row
                        .get("modality")
                        .and_then(serde_json::Value::as_str)
                        .ok_or(CacheMetadataError::Malformed(
                            "cache token modality must be a string",
                        ))?;
                    let tokens = row
                        .get("tokenCount")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or(CacheMetadataError::Malformed(
                            "cache token count must be a non-negative integer",
                        ))?;
                    normalized.push(CacheTokenDetail {
                        modality: GeminiModality::parse(modality)?,
                        tokens,
                    });
                }
                Some(normalized)
            }
        };
        Ok(Self {
            mode: request.mode(),
            prompt_tokens,
            cached_tokens,
            details,
        })
    }

    /// Request cache mode; this is request provenance, not inferred response data.
    #[must_use]
    pub const fn mode(&self) -> GeminiCacheMode {
        self.mode
    }

    /// Total effective prompt tokens, including cached content when reported.
    #[must_use]
    pub const fn prompt_tokens(&self) -> Option<u64> {
        self.prompt_tokens
    }

    /// Tokens in the cached prompt portion, preserving absent versus zero.
    #[must_use]
    pub const fn cached_tokens(&self) -> Option<u64> {
        self.cached_tokens
    }

    /// Whether a reported cache count is nonzero; absent remains unknown.
    #[must_use]
    pub fn cache_hit(&self) -> Option<bool> {
        self.cached_tokens.map(|tokens| tokens > 0)
    }

    /// Effective prompt tokens outside the cached portion when both facts exist.
    #[must_use]
    pub fn uncached_prompt_tokens(&self) -> Option<u64> {
        self.prompt_tokens?.checked_sub(self.cached_tokens?)
    }

    /// Per-modality cached token details, preserving absent versus empty.
    #[must_use]
    pub fn cache_tokens_details(&self) -> Option<&[CacheTokenDetail]> {
        self.details.as_deref()
    }
}

fn optional_counter(
    usage: &serde_json::Map<String, serde_json::Value>,
    field: &'static str,
) -> Result<Option<u64>, CacheMetadataError> {
    match usage.get(field).filter(|value| !value.is_null()) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or(CacheMetadataError::Malformed(
                "cache usage counter must be a non-negative integer",
            )),
    }
}

/// Context-cache request or usage-metadata failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CacheMetadataError {
    /// Explicit cache resource is outside the documented collection or shape.
    #[error("Gemini explicit cache resource is invalid")]
    InvalidResource,
    /// Fixed provider option failed shared validation.
    #[error("Gemini context-cache provider option is invalid")]
    InvalidOption,
    /// Response shape violates the published schema.
    #[error("invalid Gemini cache usage metadata: {0}")]
    Malformed(&'static str),
    /// Cached tokens cannot exceed the effective prompt total.
    #[error("Gemini cached token count exceeds the prompt token count")]
    CachedExceedsPrompt,
    /// Detail list exceeded the provider-owned safety cap.
    #[error("Gemini cache usage returned too many modality details")]
    TooManyDetails,
    /// A newer modality cannot be normalized as a known one.
    #[error("Gemini cache usage returned an unknown modality")]
    UnknownModality,
}

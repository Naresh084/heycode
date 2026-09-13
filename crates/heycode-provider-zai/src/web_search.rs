//! PZA04 Z.AI web-search request, response metadata and durable projection.
//!
//! Z.AI returns native Chat search results outside the assistant message as a
//! top-level `web_search` array. Each row has seven documented fields: title,
//! summary, URL, site name, icon URL, reference id and publication date. This
//! module validates and retains all seven in [`ZaiWebSearchRecord`], while also
//! projecting the provider-neutral call/result/citation subset that core can
//! persist today.
//!
//! The shared Chat parser does not expose that top-level array, and core's
//! normalized source vocabulary has no site/icon/reference/date fields. The
//! record is therefore the provider-owned durable payload a future shared hook
//! must carry; silently flattening it into URL/title alone would not satisfy
//! PZA04's metadata clause.

use heycode_core::{
    CallId, NativeToolImplementationKind, NativeToolRoute, ServerToolCall, ServerToolResult,
    ServerToolSource, ServerToolWebMetadata, UrlCitation,
};
use heycode_http::{HttpRequest, HttpService, TransportError};
use heycode_llm::{InferenceEvent, RouteCredential};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Z.AI's documented standalone Web Search API endpoint.
pub const ZAI_WEB_SEARCH_ENDPOINT: &str = "https://api.z.ai/api/paas/v4/web_search";
/// Logical native capability id shared with portable/provider search routes.
pub const ZAI_WEB_SEARCH_LOGICAL: &str = "web_search";
/// Exact provider-native implementation id for N01 registration.
pub const ZAI_WEB_SEARCH_IMPLEMENTATION: &str = "zai:web_search";
/// Provider-native name retained on normalized server-tool events.
pub const ZAI_WEB_SEARCH_TOOL_NAME: &str = "web_search";

const ZAI_WEB_SEARCH_PROVIDER: &str = "zai";
const ZAI_WEB_SEARCH_ENGINE: &str = "search-prime";
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_RESULTS: usize = 50;
const MAX_SITE_BYTES: usize = 256;
const MAX_REFERENCE_BYTES: usize = 64;
const MAX_PUBLISHED_BYTES: usize = 128;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// One bounded standalone/native web-search request.
#[derive(Clone, PartialEq, Eq)]
pub struct ZaiWebSearchRequest {
    query: String,
    count: u8,
}

impl ZaiWebSearchRequest {
    /// Validate one query and exact result count.
    ///
    /// # Errors
    /// Blank/control-bearing/oversized queries and counts outside 1..=50 fail
    /// before any request exists.
    pub fn new(query: impl Into<String>, count: u8) -> Result<Self, ZaiWebSearchError> {
        let query = query.into();
        if query.is_empty()
            || query.trim() != query
            || query.len() > MAX_QUERY_BYTES
            || query.chars().any(char::is_control)
        {
            return Err(ZaiWebSearchError::InvalidRequest("search_query"));
        }
        if !(1..=50).contains(&count) {
            return Err(ZaiWebSearchError::InvalidRequest("count"));
        }
        Ok(Self { query, count })
    }

    /// Exact query text sent to Z.AI.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Maximum number of results requested.
    #[must_use]
    pub const fn count(&self) -> u8 {
        self.count
    }

    /// Documented standalone API body.
    #[must_use]
    pub fn api_body(&self) -> serde_json::Value {
        serde_json::json!({
            "search_engine": ZAI_WEB_SEARCH_ENGINE,
            "search_query": self.query,
            "count": self.count,
        })
    }

    fn call_input(&self) -> serde_json::Value {
        self.api_body()
    }
}

impl std::fmt::Debug for ZaiWebSearchRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ZaiWebSearchRequest")
            .field("query_bytes", &self.query.len())
            .field("count", &self.count)
            .finish()
    }
}

/// One validated Z.AI result with every documented metadata field retained.
#[derive(Clone, PartialEq, Eq)]
pub struct ZaiWebSearchResultMetadata {
    title: String,
    summary: String,
    url: String,
    site_name: String,
    icon_url: String,
    reference: String,
    published: String,
}

impl ZaiWebSearchResultMetadata {
    /// Result title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Provider summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Public result URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Website name (`media` on the wire).
    #[must_use]
    pub fn site_name(&self) -> &str {
        &self.site_name
    }

    /// Public website icon URL.
    #[must_use]
    pub fn icon_url(&self) -> &str {
        &self.icon_url
    }

    /// Provider result reference (`refer` on the wire).
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// Provider publication-date string.
    #[must_use]
    pub fn published(&self) -> &str {
        &self.published
    }

    fn source(&self) -> Result<ServerToolSource, ZaiWebSearchError> {
        let metadata = ServerToolWebMetadata::new(
            &self.site_name,
            &self.icon_url,
            &self.reference,
            &self.published,
        )
        .map_err(|_| ZaiWebSearchError::Projection)?;
        ServerToolSource::new(&self.url, Some(&self.title))
            .and_then(|source| source.with_web_metadata(metadata))
            .map_err(|_| ZaiWebSearchError::Projection)
    }

    fn citation(&self) -> Result<UrlCitation, ZaiWebSearchError> {
        UrlCitation::new(
            &self.url,
            Some(&self.title),
            (!self.summary.is_empty()).then_some(self.summary.as_str()),
            None,
            None,
        )
        .map_err(|_| ZaiWebSearchError::Projection)
    }
}

impl std::fmt::Debug for ZaiWebSearchResultMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ZaiWebSearchResultMetadata")
            .field("has_title", &!self.title.is_empty())
            .field("summary_bytes", &self.summary.len())
            .field("url", &"[REDACTED]")
            .field("has_site_name", &!self.site_name.is_empty())
            .field("icon_url", &"[REDACTED]")
            .field("reference", &self.reference)
            .field("has_published", &!self.published.is_empty())
            .finish()
    }
}

/// Complete Z.AI search metadata generation.
#[derive(Clone, PartialEq, Eq)]
pub struct ZaiWebSearchRecord {
    id: String,
    created: u64,
    results: Vec<ZaiWebSearchResultMetadata>,
}

impl ZaiWebSearchRecord {
    /// Validate a standalone response or the `id`/`created`/`web_search`
    /// projection of a Chat response.
    ///
    /// # Errors
    /// Missing/unsafe identities, timestamps, result arrays or any malformed
    /// row reject the complete generation.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, ZaiWebSearchError> {
        let object = value
            .as_object()
            .ok_or(ZaiWebSearchError::Malformed("response"))?;
        let id = required_string(object, "id")
            .map_err(|_| ZaiWebSearchError::Malformed("id"))?
            .to_owned();
        let created = object
            .get("created")
            .and_then(serde_json::Value::as_u64)
            .filter(|created| *created > 0)
            .ok_or(ZaiWebSearchError::Malformed("created"))?;
        let rows = object
            .get("search_result")
            .or_else(|| object.get("web_search"))
            .and_then(serde_json::Value::as_array)
            .ok_or(ZaiWebSearchError::Malformed("results"))?;
        if rows.len() > MAX_RESULTS {
            return Err(ZaiWebSearchError::TooManyResults);
        }
        let mut results = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            results.push(parse_result(row).map_err(|_| ZaiWebSearchError::UnsafeResult { index })?);
        }
        let record = Self {
            id,
            created,
            results,
        };
        record.validate_identity()?;
        Ok(record)
    }

    /// Decode and revalidate a durable provider-owned record.
    ///
    /// # Errors
    /// Malformed JSON or any invalid field rejects the complete record.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ZaiWebSearchError> {
        let wire: WireRecord =
            serde_json::from_slice(bytes).map_err(|_| ZaiWebSearchError::Malformed("json"))?;
        let value = serde_json::to_value(wire).map_err(|_| ZaiWebSearchError::Serialization)?;
        Self::from_value(&value)
    }

    /// Encode the complete seven-field result metadata.
    ///
    /// # Errors
    /// Serialization failure.
    pub fn to_json(&self) -> Result<Vec<u8>, ZaiWebSearchError> {
        let wire = WireRecord::from(self);
        serde_json::to_vec(&wire).map_err(|_| ZaiWebSearchError::Serialization)
    }

    /// Provider task id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Provider creation time in Unix seconds.
    #[must_use]
    pub const fn created(&self) -> u64 {
        self.created
    }

    /// Ordered result rows.
    #[must_use]
    pub fn results(&self) -> &[ZaiWebSearchResultMetadata] {
        &self.results
    }

    /// Project the neutral durable subset while retaining this complete record.
    ///
    /// # Errors
    /// A call id/source/citation that violates core bounds fails instead of
    /// publishing a partial attribution.
    pub fn project(
        &self,
        request: &ZaiWebSearchRequest,
        output_index: u32,
    ) -> Result<ProjectedZaiWebSearch, ZaiWebSearchError> {
        let call_id = CallId::from_raw(format!("{}/web-search", self.id));
        let call = ServerToolCall::new(
            call_id.clone(),
            ZAI_WEB_SEARCH_LOGICAL,
            ZAI_WEB_SEARCH_TOOL_NAME,
            request.call_input(),
        )
        .map_err(|_| ZaiWebSearchError::Projection)?;
        let sources = self
            .results
            .iter()
            .map(ZaiWebSearchResultMetadata::source)
            .collect::<Result<Vec<_>, _>>()?;
        let result = ServerToolResult::success(
            call_id,
            Some(u32::try_from(self.results.len()).unwrap_or(u32::MAX)),
            sources,
        )
        .map_err(|_| ZaiWebSearchError::Projection)?;
        let mut events = Vec::with_capacity(self.results.len().saturating_add(2));
        events.push(InferenceEvent::ServerToolCall { output_index, call });
        events.push(InferenceEvent::ServerToolResult {
            output_index,
            result,
        });
        for row in &self.results {
            events.push(InferenceEvent::Citation {
                output_index,
                citation: row.citation()?,
            });
        }
        Ok(ProjectedZaiWebSearch {
            metadata: self.clone(),
            events,
        })
    }

    fn validate_identity(&self) -> Result<(), ZaiWebSearchError> {
        ServerToolCall::new(
            CallId::from_raw(format!("{}/web-search", self.id)),
            ZAI_WEB_SEARCH_LOGICAL,
            ZAI_WEB_SEARCH_TOOL_NAME,
            serde_json::json!({}),
        )
        .map(|_| ())
        .map_err(|_| ZaiWebSearchError::Malformed("id"))
    }
}

impl std::fmt::Debug for ZaiWebSearchRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ZaiWebSearchRecord")
            .field("id", &self.id)
            .field("created", &self.created)
            .field("result_count", &self.results.len())
            .finish()
    }
}

/// Complete provider metadata plus currently representable normalized events.
#[derive(Clone, PartialEq)]
pub struct ProjectedZaiWebSearch {
    metadata: ZaiWebSearchRecord,
    events: Vec<InferenceEvent>,
}

impl ProjectedZaiWebSearch {
    /// Full seven-field provider metadata.
    #[must_use]
    pub const fn metadata(&self) -> &ZaiWebSearchRecord {
        &self.metadata
    }

    /// Normalized call/result/citation events.
    #[must_use]
    pub fn events(&self) -> &[InferenceEvent] {
        &self.events
    }

    /// Consume the projection for shared publication.
    #[must_use]
    pub fn into_parts(self) -> (ZaiWebSearchRecord, Vec<InferenceEvent>) {
        (self.metadata, self.events)
    }
}

impl std::fmt::Debug for ProjectedZaiWebSearch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectedZaiWebSearch")
            .field("metadata", &self.metadata)
            .field("event_count", &self.events.len())
            .finish()
    }
}

/// Buffered client for Z.AI's standalone native Web Search API.
pub struct ZaiWebSearchClient {
    http: HttpService,
    credential: RouteCredential,
}

impl ZaiWebSearchClient {
    /// Bind a literal API key for embedding/tests.
    #[must_use]
    pub fn with_key(http: HttpService, api_key: impl Into<String>) -> Self {
        Self {
            http,
            credential: RouteCredential::fixed(api_key),
        }
    }

    /// Bind an operation-time credential route.
    #[must_use]
    pub const fn with_credential(http: HttpService, credential: RouteCredential) -> Self {
        Self { http, credential }
    }

    /// Execute one bounded search and validate the complete result generation.
    ///
    /// # Errors
    /// Credential, request, transport, HTTP classification, content type or
    /// response validation failure. No provider body enters the error.
    pub async fn search(
        &self,
        request: ZaiWebSearchRequest,
        cancellation: CancellationToken,
    ) -> Result<ZaiWebSearchRecord, ZaiWebSearchError> {
        let credential = self
            .credential
            .acquire()
            .map_err(|_| ZaiWebSearchError::Credential)?;
        let body = serde_json::to_vec(&request.api_body())
            .map_err(|_| ZaiWebSearchError::Serialization)?;
        let request = HttpRequest::post(ZAI_WEB_SEARCH_ENDPOINT, body)
            .and_then(|request| {
                request.header("authorization", &format!("Bearer {}", credential.expose()))
            })
            .and_then(|request| request.header("content-type", "application/json"))
            .map_err(|_| ZaiWebSearchError::Request)?
            .with_max_response_bytes(MAX_RESPONSE_BYTES);
        let response = self
            .http
            .send(request, cancellation)
            .await
            .map_err(classify_transport)?;
        match response.status {
            200 => {}
            401 | 403 => return Err(ZaiWebSearchError::Unauthorized),
            429 | 500..=599 => return Err(ZaiWebSearchError::Unavailable),
            status => return Err(ZaiWebSearchError::Rejected { status }),
        }
        if response.content_type.as_deref() != Some("application/json") {
            return Err(ZaiWebSearchError::ContentType);
        }
        ZaiWebSearchRecord::from_json(&response.body)
    }
}

impl std::fmt::Debug for ZaiWebSearchClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ZaiWebSearchClient")
            .field("endpoint", &ZAI_WEB_SEARCH_ENDPOINT)
            .field("credential", &self.credential)
            .finish()
    }
}

/// Provider-owned N01 contribution data for the composition root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiWebSearchContribution {
    route: NativeToolRoute,
    priority: i16,
}

impl ZaiWebSearchContribution {
    /// Validated logical/provider route.
    #[must_use]
    pub const fn route(&self) -> &NativeToolRoute {
        &self.route
    }

    /// Deterministic provider-native selection priority.
    #[must_use]
    pub const fn priority(&self) -> i16 {
        self.priority
    }
}

/// Build the provider-owned N01 candidate. The root maps this into
/// `heycode-native-tools` without repeating identities.
///
/// # Errors
/// A compiled identity stopped satisfying the core route grammar.
pub fn zai_web_search_contribution() -> Result<ZaiWebSearchContribution, ZaiWebSearchError> {
    Ok(ZaiWebSearchContribution {
        route: NativeToolRoute::new(
            ZAI_WEB_SEARCH_LOGICAL,
            ZAI_WEB_SEARCH_IMPLEMENTATION,
            NativeToolImplementationKind::Provider,
            Some(ZAI_WEB_SEARCH_PROVIDER.to_owned()),
        )
        .map_err(|_| ZaiWebSearchError::Contribution)?,
        priority: 100,
    })
}

/// Stable body-free native search failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZaiWebSearchError {
    /// Invalid caller request field.
    InvalidRequest(&'static str),
    /// Credential could not be resolved.
    Credential,
    /// HTTP request could not be built.
    Request,
    /// Operation was cancelled.
    Cancelled,
    /// Network/timeout/size failure.
    Transport,
    /// Credential was rejected.
    Unauthorized,
    /// Rate limit or provider outage.
    Unavailable,
    /// Other non-success status.
    Rejected {
        /// HTTP status only; response body is excluded.
        status: u16,
    },
    /// Response media type was not JSON.
    ContentType,
    /// Stable structural field failed.
    Malformed(&'static str),
    /// Result count exceeded the documented request maximum.
    TooManyResults,
    /// One row was unsafe; the complete generation is rejected.
    UnsafeResult {
        /// Zero-based row index.
        index: usize,
    },
    /// JSON could not be encoded.
    Serialization,
    /// Normalized call/source/citation projection failed.
    Projection,
    /// Native-tool contribution identity failed core validation.
    Contribution,
}

impl std::fmt::Display for ZaiWebSearchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(field) => {
                write!(formatter, "invalid Z.AI web-search field `{field}`")
            }
            Self::Credential => formatter.write_str("Z.AI web-search credential is unavailable"),
            Self::Request => formatter.write_str("Z.AI web-search request is invalid"),
            Self::Cancelled => formatter.write_str("Z.AI web search was cancelled"),
            Self::Transport => formatter.write_str("Z.AI web-search transport failed"),
            Self::Unauthorized => formatter.write_str("Z.AI web-search credential was rejected"),
            Self::Unavailable => formatter.write_str("Z.AI web search is unavailable"),
            Self::Rejected { status } => {
                write!(formatter, "Z.AI web search returned HTTP {status}")
            }
            Self::ContentType => formatter.write_str("Z.AI web-search response is not JSON"),
            Self::Malformed(field) => write!(
                formatter,
                "Z.AI web-search response field `{field}` is malformed"
            ),
            Self::TooManyResults => {
                formatter.write_str("Z.AI web-search response has too many results")
            }
            Self::UnsafeResult { index } => {
                write!(formatter, "Z.AI web-search result {index} is unsafe")
            }
            Self::Serialization => formatter.write_str("Z.AI web-search JSON serialization failed"),
            Self::Projection => formatter.write_str("Z.AI web-search durable projection failed"),
            Self::Contribution => {
                formatter.write_str("Z.AI web-search native contribution is invalid")
            }
        }
    }
}

impl std::error::Error for ZaiWebSearchError {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    id: String,
    created: u64,
    search_result: Vec<WireResult>,
}

impl From<&ZaiWebSearchRecord> for WireRecord {
    fn from(record: &ZaiWebSearchRecord) -> Self {
        Self {
            id: record.id.clone(),
            created: record.created,
            search_result: record.results.iter().map(WireResult::from).collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResult {
    title: String,
    content: String,
    link: String,
    media: String,
    icon: String,
    refer: String,
    publish_date: String,
}

impl From<&ZaiWebSearchResultMetadata> for WireResult {
    fn from(result: &ZaiWebSearchResultMetadata) -> Self {
        Self {
            title: result.title.clone(),
            content: result.summary.clone(),
            link: result.url.clone(),
            media: result.site_name.clone(),
            icon: result.icon_url.clone(),
            refer: result.reference.clone(),
            publish_date: result.published.clone(),
        }
    }
}

fn parse_result(value: &serde_json::Value) -> Result<ZaiWebSearchResultMetadata, ()> {
    let object = value.as_object().ok_or(())?;
    const FIELDS: [&str; 7] = [
        "title",
        "content",
        "link",
        "media",
        "icon",
        "refer",
        "publish_date",
    ];
    if object.len() != FIELDS.len() || object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
        return Err(());
    }
    let title = required_string(object, "title")?.to_owned();
    let summary = required_string_allow_empty(object, "content")?.to_owned();
    let url = required_string(object, "link")?.to_owned();
    let site_name = required_string_allow_empty(object, "media")?.to_owned();
    let icon_url = required_string_allow_empty(object, "icon")?.to_owned();
    let reference = required_string(object, "refer")?.to_owned();
    let published = required_string_allow_empty(object, "publish_date")?.to_owned();

    ServerToolSource::new(&url, Some(&title)).map_err(|_| ())?;
    UrlCitation::new(
        &url,
        Some(&title),
        (!summary.is_empty()).then_some(summary.as_str()),
        None,
        None,
    )
    .map_err(|_| ())?;
    if !icon_url.is_empty() {
        ServerToolSource::new(&icon_url, None).map_err(|_| ())?;
    }
    validate_single_line(&site_name, MAX_SITE_BYTES)?;
    validate_single_line(&reference, MAX_REFERENCE_BYTES)?;
    validate_single_line(&published, MAX_PUBLISHED_BYTES)?;
    Ok(ZaiWebSearchResultMetadata {
        title,
        summary,
        url,
        site_name,
        icon_url,
        reference,
        published,
    })
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, ()> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(())
}

fn required_string_allow_empty<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, ()> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(())
}

fn validate_single_line(value: &str, maximum: usize) -> Result<(), ()> {
    if value.len() > maximum || value.chars().any(char::is_control) {
        Err(())
    } else {
        Ok(())
    }
}

fn classify_transport(error: TransportError) -> ZaiWebSearchError {
    match error {
        TransportError::Cancelled => ZaiWebSearchError::Cancelled,
        TransportError::Http {
            status: 401 | 403, ..
        } => ZaiWebSearchError::Unauthorized,
        TransportError::Http {
            status: 429 | 500..=599,
            ..
        } => ZaiWebSearchError::Unavailable,
        TransportError::Http { status, .. } => ZaiWebSearchError::Rejected { status },
        _ => ZaiWebSearchError::Transport,
    }
}

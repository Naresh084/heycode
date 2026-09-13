//! Provider-neutral, UI-safe server-tool event vocabulary.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::CallId;

const MAX_CALL_ID_BYTES: usize = 256;
const MAX_LOGICAL_BYTES: usize = 64;
const MAX_PROVIDER_NAME_BYTES: usize = 128;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_INPUT_DEPTH: usize = 16;
const MAX_INPUT_NODES: usize = 4_096;
const MAX_INPUT_STRING_BYTES: usize = 16 * 1024;
const MAX_SOURCES: usize = 128;
const MAX_URL_BYTES: usize = 4_096;
const MAX_TITLE_BYTES: usize = 512;
const MAX_SITE_NAME_BYTES: usize = 256;
const MAX_PROVIDER_REFERENCE_BYTES: usize = 256;
const MAX_PUBLISHED_BYTES: usize = 256;
const MAX_CITED_TEXT_BYTES: usize = 16 * 1024;
const MAX_ERROR_CODE_BYTES: usize = 64;

/// One provider-executed tool call after logical normalization.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerToolCall {
    id: CallId,
    logical: String,
    provider_name: String,
    input: serde_json::Value,
}

impl ServerToolCall {
    /// Construct one normalized server-tool call.
    ///
    /// # Errors
    /// Invalid identities, non-object input or oversized/deep input fail.
    pub fn new(
        id: CallId,
        logical: impl Into<String>,
        provider_name: impl Into<String>,
        input: serde_json::Value,
    ) -> Result<Self, ServerToolError> {
        let call = Self {
            id,
            logical: logical.into(),
            provider_name: provider_name.into(),
            input,
        };
        call.validate()?;
        Ok(call)
    }

    /// Revalidate a deserialized call.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), ServerToolError> {
        validate_call_id(&self.id)?;
        if !safe_logical(&self.logical) {
            return invalid("logical", "logical server-tool id is invalid");
        }
        if !safe_provider_name(&self.provider_name) {
            return invalid("provider_name", "provider tool name is invalid");
        }
        validate_input(&self.input)
    }

    /// Provider call correlation id.
    #[must_use]
    pub fn id(&self) -> &CallId {
        &self.id
    }

    /// Stable logical capability id.
    #[must_use]
    pub fn logical(&self) -> &str {
        &self.logical
    }

    /// Exact provider-native tool name.
    #[must_use]
    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }

    /// Validated object input. Treat values as untrusted provider content.
    #[must_use]
    pub fn input(&self) -> &serde_json::Value {
        &self.input
    }
}

impl fmt::Debug for ServerToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerToolCall")
            .field("id", &self.id)
            .field("logical", &self.logical)
            .field("provider_name", &self.provider_name)
            .field("input", &"[REDACTED]")
            .finish()
    }
}

/// Completion class for a provider-executed tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerToolOutcome {
    /// Provider reported a completed result.
    Success,
    /// Provider represented a tool-level failure inside a successful response.
    Error,
}

/// One bounded public source discovered by a server tool.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolSource {
    url: String,
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    web_metadata: Option<ServerToolWebMetadata>,
}

impl ServerToolSource {
    /// Construct a safe display source.
    ///
    /// # Errors
    /// Non-HTTP(S), credential-bearing or unsafe/bounded metadata fails.
    pub fn new(url: &str, title: Option<&str>) -> Result<Self, ServerToolError> {
        let source = Self {
            url: url.to_owned(),
            title: title.map(str::to_owned),
            web_metadata: None,
        };
        source.validate()?;
        Ok(source)
    }

    /// Revalidate a deserialized source.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), ServerToolError> {
        validate_public_url(&self.url)?;
        if let Some(title) = &self.title {
            validate_single_line(title, MAX_TITLE_BYTES, "title")?;
        }
        if let Some(metadata) = &self.web_metadata {
            metadata.validate()?;
        }
        Ok(())
    }

    /// Attach complete bounded web-result metadata retained by the provider.
    ///
    /// # Errors
    /// Unsafe site/icon/reference/publication fields leave the source
    /// unchanged.
    pub fn with_web_metadata(
        mut self,
        metadata: ServerToolWebMetadata,
    ) -> Result<Self, ServerToolError> {
        metadata.validate()?;
        self.web_metadata = Some(metadata);
        Ok(self)
    }

    /// Original public source URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Optional bounded display title.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Optional complete provider web-result metadata.
    #[must_use]
    pub const fn web_metadata(&self) -> Option<&ServerToolWebMetadata> {
        self.web_metadata.as_ref()
    }
}

impl fmt::Debug for ServerToolSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerToolSource")
            .field("url", &"[REDACTED]")
            .field("has_title", &self.title.is_some())
            .field("has_web_metadata", &self.web_metadata.is_some())
            .finish()
    }
}

/// Optional provider-neutral web metadata attached to a durable source.
///
/// Empty strings remain distinct from absence because some provider response
/// schemas require the field while permitting an unknown value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolWebMetadata {
    site_name: String,
    icon_url: String,
    provider_reference: String,
    published: String,
}

impl ServerToolWebMetadata {
    /// Construct one complete web-result metadata row.
    ///
    /// # Errors
    /// Oversized/control-bearing text or a non-public nonempty icon URL fails.
    pub fn new(
        site_name: impl Into<String>,
        icon_url: impl Into<String>,
        provider_reference: impl Into<String>,
        published: impl Into<String>,
    ) -> Result<Self, ServerToolError> {
        let metadata = Self {
            site_name: site_name.into(),
            icon_url: icon_url.into(),
            provider_reference: provider_reference.into(),
            published: published.into(),
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Revalidate deserialized metadata.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), ServerToolError> {
        validate_optional_single_line(&self.site_name, MAX_SITE_NAME_BYTES, "site_name")?;
        if !self.icon_url.is_empty() {
            validate_public_url_field(&self.icon_url, "icon_url")?;
        }
        validate_optional_single_line(
            &self.provider_reference,
            MAX_PROVIDER_REFERENCE_BYTES,
            "provider_reference",
        )?;
        validate_optional_single_line(&self.published, MAX_PUBLISHED_BYTES, "published")
    }

    /// Provider site/display name.
    #[must_use]
    pub fn site_name(&self) -> &str {
        &self.site_name
    }

    /// Public icon URL, or the provider's explicit empty value.
    #[must_use]
    pub fn icon_url(&self) -> &str {
        &self.icon_url
    }

    /// Provider-local result reference.
    #[must_use]
    pub fn provider_reference(&self) -> &str {
        &self.provider_reference
    }

    /// Provider publication-date string.
    #[must_use]
    pub fn published(&self) -> &str {
        &self.published
    }
}

impl fmt::Debug for ServerToolWebMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerToolWebMetadata")
            .field("has_site_name", &!self.site_name.is_empty())
            .field("has_icon_url", &!self.icon_url.is_empty())
            .field(
                "has_provider_reference",
                &!self.provider_reference.is_empty(),
            )
            .field("has_published", &!self.published.is_empty())
            .finish()
    }
}

/// One normalized provider server-tool result.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolResult {
    call_id: CallId,
    outcome: ServerToolOutcome,
    output_count: Option<u32>,
    error_code: Option<String>,
    sources: Vec<ServerToolSource>,
}

impl ServerToolResult {
    /// Construct one successful result with optional enumerable output count.
    ///
    /// # Errors
    /// Invalid call identity or source metadata fails.
    pub fn success(
        call_id: CallId,
        output_count: Option<u32>,
        sources: Vec<ServerToolSource>,
    ) -> Result<Self, ServerToolError> {
        let result = Self {
            call_id,
            outcome: ServerToolOutcome::Success,
            output_count,
            error_code: None,
            sources,
        };
        result.validate()?;
        Ok(result)
    }

    /// Construct one provider-reported tool-level error.
    ///
    /// # Errors
    /// Invalid call identity or unsafe error code fails.
    pub fn error(call_id: CallId, error_code: impl Into<String>) -> Result<Self, ServerToolError> {
        let result = Self {
            call_id,
            outcome: ServerToolOutcome::Error,
            output_count: None,
            error_code: Some(error_code.into()),
            sources: Vec::new(),
        };
        result.validate()?;
        Ok(result)
    }

    /// Revalidate a deserialized result.
    ///
    /// # Errors
    /// Invalid identity, inconsistent outcome fields or unsafe sources fail.
    pub fn validate(&self) -> Result<(), ServerToolError> {
        validate_call_id(&self.call_id)?;
        if self.sources.len() > MAX_SOURCES {
            return invalid("sources", "too many server-tool sources");
        }
        for source in &self.sources {
            source.validate()?;
        }
        match self.outcome {
            ServerToolOutcome::Success if self.error_code.is_none() => Ok(()),
            ServerToolOutcome::Error
                if self.output_count.is_none()
                    && self.sources.is_empty()
                    && self.error_code.as_deref().is_some_and(safe_error_code) =>
            {
                Ok(())
            }
            ServerToolOutcome::Success => invalid(
                "error_code",
                "successful result cannot contain an error code",
            ),
            ServerToolOutcome::Error => invalid(
                "outcome",
                "error result requires one safe code and no outputs/sources",
            ),
        }
    }

    /// Provider server-call correlation id.
    #[must_use]
    pub fn call_id(&self) -> &CallId {
        &self.call_id
    }

    /// Success/error classification.
    #[must_use]
    pub const fn outcome(&self) -> ServerToolOutcome {
        self.outcome
    }

    /// Provider-reported enumerable output count, when meaningful.
    #[must_use]
    pub const fn output_count(&self) -> Option<u32> {
        self.output_count
    }

    /// Stable tool-level error code without response body text.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }

    /// Bounded public result sources.
    #[must_use]
    pub fn sources(&self) -> &[ServerToolSource] {
        &self.sources
    }
}

impl fmt::Debug for ServerToolResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerToolResult")
            .field("call_id", &self.call_id)
            .field("outcome", &self.outcome)
            .field("output_count", &self.output_count)
            .field("error_code", &self.error_code)
            .field("source_count", &self.sources.len())
            .finish()
    }
}

/// Evidence behind a provider server-tool request count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerToolUsageEvidence {
    /// Provider reported only an aggregate request count, not individual calls.
    ProviderAggregate,
}

/// One exact published server-tool fee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolPublishedCost {
    currency: String,
    pico_units: u128,
}

impl ServerToolPublishedCost {
    /// Three-letter lowercase currency code.
    #[must_use]
    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// Exact non-zero total fee in pico-units.
    #[must_use]
    pub const fn pico_units(&self) -> u128 {
        self.pico_units
    }
}

/// Cost evidence attached to provider server-tool usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerToolUsageCost {
    /// Engine/provider/result-count pricing cannot be resolved exactly.
    Unknown,
    /// Published price multiplied by exact usage inputs.
    Published(ServerToolPublishedCost),
}

impl ServerToolUsageCost {
    /// Construct one exact published fee.
    ///
    /// # Errors
    /// Currency must be three lowercase ASCII letters and pico-units non-zero.
    pub fn published(currency: &str, pico_units: u128) -> Result<Self, ServerToolError> {
        if currency.len() != 3
            || !currency.bytes().all(|byte| byte.is_ascii_lowercase())
            || pico_units == 0
        {
            return invalid("cost", "server-tool published cost is invalid");
        }
        Ok(Self::Published(ServerToolPublishedCost {
            currency: currency.to_owned(),
            pico_units,
        }))
    }

    fn validate(&self) -> Result<(), ServerToolError> {
        match self {
            Self::Unknown => Ok(()),
            Self::Published(cost) => Self::published(&cost.currency, cost.pico_units).map(|_| ()),
        }
    }
}

/// Provider-reported aggregate server-tool usage without invented call ids.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolUsage {
    logical: String,
    requests: u32,
    evidence: ServerToolUsageEvidence,
    cost: ServerToolUsageCost,
}

impl ServerToolUsage {
    /// Construct and validate one aggregate usage row.
    ///
    /// # Errors
    /// Invalid logical id, zero requests or invalid cost evidence.
    pub fn new(
        logical: impl Into<String>,
        requests: u32,
        evidence: ServerToolUsageEvidence,
        cost: ServerToolUsageCost,
    ) -> Result<Self, ServerToolError> {
        let usage = Self {
            logical: logical.into(),
            requests,
            evidence,
            cost,
        };
        usage.validate()?;
        Ok(usage)
    }

    /// Revalidate a deserialized usage row.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), ServerToolError> {
        if !safe_logical(&self.logical) {
            return invalid("logical", "logical server-tool usage id is invalid");
        }
        if self.requests == 0 {
            return invalid("requests", "server-tool usage requests must be positive");
        }
        self.cost.validate()
    }

    /// Logical capability id.
    #[must_use]
    pub fn logical(&self) -> &str {
        &self.logical
    }

    /// Provider-reported request count.
    #[must_use]
    pub const fn requests(&self) -> u32 {
        self.requests
    }

    /// Count evidence class.
    #[must_use]
    pub const fn evidence(&self) -> ServerToolUsageEvidence {
        self.evidence
    }

    /// Cost evidence.
    #[must_use]
    pub const fn cost(&self) -> &ServerToolUsageCost {
        &self.cost
    }
}

impl fmt::Debug for ServerToolUsage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerToolUsage")
            .field("logical", &self.logical)
            .field("requests", &self.requests)
            .field("evidence", &self.evidence)
            .field("cost", &self.cost)
            .finish()
    }
}

/// One UI-safe URL citation attached to assistant output.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlCitation {
    url: String,
    title: Option<String>,
    cited_text: Option<String>,
    start_index: Option<u32>,
    end_index: Option<u32>,
}

impl UrlCitation {
    /// Construct one bounded URL citation.
    ///
    /// # Errors
    /// Unsafe URL/text or inconsistent output range fails.
    pub fn new(
        url: &str,
        title: Option<&str>,
        cited_text: Option<&str>,
        start_index: Option<u32>,
        end_index: Option<u32>,
    ) -> Result<Self, ServerToolError> {
        let citation = Self {
            url: url.to_owned(),
            title: title.map(str::to_owned),
            cited_text: cited_text.map(str::to_owned),
            start_index,
            end_index,
        };
        citation.validate()?;
        Ok(citation)
    }

    /// Revalidate a deserialized citation.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), ServerToolError> {
        validate_public_url(&self.url)?;
        if let Some(title) = &self.title {
            validate_single_line(title, MAX_TITLE_BYTES, "title")?;
        }
        if let Some(text) = &self.cited_text {
            validate_multiline(text, MAX_CITED_TEXT_BYTES, "cited_text")?;
        }
        match (self.start_index, self.end_index) {
            (None, None) => Ok(()),
            (Some(start), Some(end)) if start <= end => Ok(()),
            _ => invalid("range", "citation range must be paired and ordered"),
        }
    }

    /// Original cited URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Optional bounded source title.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Optional bounded cited excerpt.
    #[must_use]
    pub fn cited_text(&self) -> Option<&str> {
        self.cited_text.as_deref()
    }

    /// Optional inclusive output start index.
    #[must_use]
    pub const fn start_index(&self) -> Option<u32> {
        self.start_index
    }

    /// Optional exclusive output end index.
    #[must_use]
    pub const fn end_index(&self) -> Option<u32> {
        self.end_index
    }
}

impl fmt::Debug for UrlCitation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UrlCitation")
            .field("url", &"[REDACTED]")
            .field("has_title", &self.title.is_some())
            .field("has_cited_text", &self.cited_text.is_some())
            .field("start_index", &self.start_index)
            .field("end_index", &self.end_index)
            .finish()
    }
}

/// Normalized server-tool boundary failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerToolError {
    /// One stable structural field is invalid.
    #[error("invalid server tool field `{field}`: {message}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Safe structural detail without provider content.
        message: String,
    },
}

fn validate_call_id(id: &CallId) -> Result<(), ServerToolError> {
    let value = id.as_str();
    if value.is_empty()
        || value.len() > MAX_CALL_ID_BYTES
        || value.trim() != value
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return invalid("call_id", "provider call id is invalid");
    }
    Ok(())
}

fn safe_logical(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_LOGICAL_BYTES
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn safe_provider_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_PROVIDER_NAME_BYTES
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b':' | b'/' | b'.')
        })
}

fn safe_error_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ERROR_CODE_BYTES
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn validate_input(value: &serde_json::Value) -> Result<(), ServerToolError> {
    if !value.is_object() {
        return invalid("input", "server-tool input must be an object");
    }
    let bytes = serde_json::to_vec(value).map_err(|_| ServerToolError::InvalidField {
        field: "input",
        message: "server-tool input could not serialize".to_owned(),
    })?;
    if bytes.len() > MAX_INPUT_BYTES {
        return invalid("input", "server-tool input exceeds size bound");
    }
    let mut nodes = 0_usize;
    validate_json_node(value, 0, &mut nodes)
}

fn validate_json_node(
    value: &serde_json::Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), ServerToolError> {
    if depth > MAX_INPUT_DEPTH {
        return invalid("input", "server-tool input exceeds depth bound");
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_INPUT_NODES {
        return invalid("input", "server-tool input exceeds node bound");
    }
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
        serde_json::Value::String(value) => {
            if value.len() > MAX_INPUT_STRING_BYTES {
                return invalid("input", "server-tool input string exceeds size bound");
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_json_node(value, depth.saturating_add(1), nodes)?;
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
                    return invalid("input", "server-tool input object key is invalid");
                }
                validate_json_node(value, depth.saturating_add(1), nodes)?;
            }
        }
    }
    Ok(())
}

fn validate_public_url(value: &str) -> Result<(), ServerToolError> {
    validate_public_url_field(value, "url")
}

fn validate_public_url_field(value: &str, field: &'static str) -> Result<(), ServerToolError> {
    if value.is_empty()
        || value.len() > MAX_URL_BYTES
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
    {
        return invalid(field, "citation URL is invalid");
    }
    let parsed = url::Url::parse(value).map_err(|_| ServerToolError::InvalidField {
        field,
        message: "citation URL is invalid".to_owned(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return invalid(
            field,
            "citation URL must be public HTTP(S) without userinfo",
        );
    }
    Ok(())
}

fn validate_optional_single_line(
    value: &str,
    maximum: usize,
    field: &'static str,
) -> Result<(), ServerToolError> {
    if value.len() > maximum || value.trim() != value || value.chars().any(char::is_control) {
        return invalid(field, "display text is invalid");
    }
    Ok(())
}

fn validate_single_line(
    value: &str,
    maximum: usize,
    field: &'static str,
) -> Result<(), ServerToolError> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return invalid(field, "display text is invalid");
    }
    Ok(())
}

fn validate_multiline(
    value: &str,
    maximum: usize,
    field: &'static str,
) -> Result<(), ServerToolError> {
    if value.is_empty()
        || value.len() > maximum
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return invalid(field, "display text is invalid");
    }
    Ok(())
}

fn invalid<T>(field: &'static str, message: &'static str) -> Result<T, ServerToolError> {
    Err(ServerToolError::InvalidField {
        field,
        message: message.to_owned(),
    })
}

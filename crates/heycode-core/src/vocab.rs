//! Cross-crate vocabulary shared by tools and LLM adapters.
//!
//! Single home per AGENTS.md layering rules: both `heycode-tools` (schemas) and
//! `heycode-llm` (requests) import from here instead of declaring parallel types.

use serde::{Deserialize, Serialize};

const PROVIDER_REQUEST_OPTION_MAX_BYTES: usize = 64 * 1024;
const PROVIDER_REQUEST_OPTION_MAX_STRING_BYTES: usize = 4 * 1024;

/// JSON-Schema description of one model-callable tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool name the model calls by.
    pub name: String,
    /// Model-facing description: concrete, imperative, no UI jargon.
    pub description: String,
    /// JSON Schema object describing accepted arguments.
    pub parameters: serde_json::Value,
}

/// Canonicalize object-key order while preserving arrays and every value.
/// Used for request configuration only; conversation ordering is authoritative.
pub fn canonicalize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            let mut entries: Vec<_> = std::mem::take(object).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (key, mut value) in entries {
                canonicalize_json(&mut value);
                object.insert(key, value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                canonicalize_json(value);
            }
        }
        _ => {}
    }
}

/// Stable model-facing catalog independent of plugin registration order.
/// Native declarations remain declarations; no prose copies are introduced.
#[must_use]
pub fn canonical_tool_specs(mut tools: Vec<ToolSpec>) -> Vec<ToolSpec> {
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    for tool in &mut tools {
        canonicalize_json(&mut tool.parameters);
    }
    tools
}

/// Wire/runtime protocol family used by a provider route.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    /// Provider has not declared a trustworthy protocol family.
    Unknown,
    /// OpenAI Chat Completions.
    OpenAiChatCompletions,
    /// OpenAI Responses.
    OpenAiResponses,
    /// Anthropic Messages.
    AnthropicMessages,
    /// Gemini GenerateContent.
    GeminiGenerateContent,
    /// Amazon Bedrock Converse.
    BedrockConverse,
    /// Official delegated coding-agent runtime.
    DelegatedAgent,
}

/// Lossless provider-state category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStateKind {
    /// One complete Responses API output item for stateless replay.
    ResponseOutputItem,
    /// One complete Chat Completions assistant message for replay.
    ChatAssistantMessage,
    /// One complete Anthropic assistant block array plus optional continuation
    /// container metadata for exact replay.
    AnthropicMessage,
    /// One complete Gemini model `Content` — `role: "model"` plus its ordered
    /// `parts` — replayed verbatim.
    ///
    /// Verbatim is the contract, not an implementation convenience: Gemini
    /// requires a returned `thoughtSignature` "in the exact part where it was
    /// received", so a distilled signature field would satisfy a test and still
    /// fail the request.
    /// <https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures>
    GeminiModelContent,
    /// One complete Bedrock Converse assistant message with ordered content
    /// blocks, including opaque reasoning continuity material.
    BedrockConverseMessage,
}

/// Provider-state identity/shape failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderStateError {
    /// Route identity or JSON shape is invalid.
    #[error("invalid provider state field `{field}`: {message}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Safe detail.
        message: String,
    },
    /// State kind is impossible for its protocol.
    #[error("provider state kind {kind:?} is incompatible with protocol {protocol:?}")]
    ProtocolKindMismatch {
        /// Owning protocol.
        protocol: ProviderProtocol,
        /// State kind.
        kind: ProviderStateKind,
    },
}

/// Lossless schema-versioned provider continuation state.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderStateItem {
    provider: String,
    model: String,
    protocol: ProviderProtocol,
    kind: ProviderStateKind,
    schema_version: u32,
    data: serde_json::Value,
}

impl std::fmt::Debug for ProviderStateItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderStateItem")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            .field("kind", &self.kind)
            .field("schema_version", &self.schema_version)
            .field("data", &"<redacted>")
            .finish()
    }
}

/// Provider-owned, secret-free request option committed before dispatch.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRequestOption {
    provider: String,
    kind: String,
    schema_version: u32,
    data: serde_json::Value,
}

impl ProviderRequestOption {
    /// Construct one schema-v1 provider option object.
    ///
    /// # Errors
    /// Unsafe identity, non-object/oversized data, unsafe keys or oversized
    /// control-bearing strings fail before the option can become durable.
    pub fn new(
        provider: impl Into<String>,
        kind: impl Into<String>,
        data: serde_json::Value,
    ) -> Result<Self, ProviderRequestOptionError> {
        let option = Self {
            provider: provider.into(),
            kind: kind.into(),
            schema_version: 1,
            data,
        };
        option.validate()?;
        Ok(option)
    }

    /// Revalidate a deserialized option.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), ProviderRequestOptionError> {
        if !safe_option_id(&self.provider) {
            return invalid_provider_option("provider", "provider id must be lowercase kebab-case");
        }
        if !safe_option_id(&self.kind) {
            return invalid_provider_option("kind", "option kind must be lowercase kebab-case");
        }
        if self.schema_version != 1 {
            return invalid_provider_option(
                "schema_version",
                "only provider request option schema v1 is supported",
            );
        }
        if !self.data.is_object() {
            return invalid_provider_option(
                "data",
                "provider request option data must be an object",
            );
        }
        validate_provider_option_value(&self.data)?;
        let encoded = serde_json::to_vec(&self.data).map_err(|_| {
            ProviderRequestOptionError::InvalidField {
                field: "data",
                message: "provider request option data cannot be serialized".to_owned(),
            }
        })?;
        if encoded.len() > PROVIDER_REQUEST_OPTION_MAX_BYTES {
            return invalid_provider_option("data", "provider request option data is too large");
        }
        Ok(())
    }

    /// Owning provider id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Provider-owned option kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Option schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Validated provider-owned object data.
    #[must_use]
    pub const fn data(&self) -> &serde_json::Value {
        &self.data
    }
}

impl std::fmt::Debug for ProviderRequestOption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderRequestOption")
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("schema_version", &self.schema_version)
            .field("data", &"[REDACTED]")
            .finish()
    }
}

/// Provider request option validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderRequestOptionError {
    /// One stable field violated the option boundary.
    #[error("invalid provider request option field `{field}`: {message}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Safe structural detail.
        message: String,
    },
}

/// Runtime family selected for one logical native-tool capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeToolImplementationKind {
    /// Provider-hosted server tool.
    Provider,
    /// heycode client-side model tool.
    Client,
    /// MCP-backed implementation.
    Mcp,
}

/// Durable logical native-tool implementation selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeToolRoute {
    logical: String,
    implementation: String,
    kind: NativeToolImplementationKind,
    provider: Option<String>,
}

impl NativeToolRoute {
    /// Construct and validate one selected route.
    ///
    /// # Errors
    /// Unsafe ids or provider ownership inconsistent with `kind` fail.
    pub fn new(
        logical: impl Into<String>,
        implementation: impl Into<String>,
        kind: NativeToolImplementationKind,
        provider: Option<String>,
    ) -> Result<Self, NativeToolRouteError> {
        let route = Self {
            logical: logical.into(),
            implementation: implementation.into(),
            kind,
            provider,
        };
        route.validate()?;
        Ok(route)
    }

    /// Revalidate a deserialized route.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), NativeToolRouteError> {
        if !safe_logical_tool_id(&self.logical) {
            return invalid_native_route("logical", "logical tool id is invalid");
        }
        if !safe_implementation_id(&self.implementation) {
            return invalid_native_route("implementation", "implementation id is invalid");
        }
        match (self.kind, self.provider.as_deref()) {
            (NativeToolImplementationKind::Provider, Some(provider))
                if safe_option_id(provider) => {}
            (NativeToolImplementationKind::Provider, _) => {
                return invalid_native_route(
                    "provider",
                    "provider implementations require a provider owner",
                );
            }
            (NativeToolImplementationKind::Client | NativeToolImplementationKind::Mcp, None) => {}
            (NativeToolImplementationKind::Client | NativeToolImplementationKind::Mcp, Some(_)) => {
                return invalid_native_route(
                    "provider",
                    "client/MCP implementations cannot claim a provider owner",
                );
            }
        }
        Ok(())
    }

    /// Logical capability id.
    #[must_use]
    pub fn logical(&self) -> &str {
        &self.logical
    }

    /// Selected implementation id.
    #[must_use]
    pub fn implementation(&self) -> &str {
        &self.implementation
    }

    /// Implementation family.
    #[must_use]
    pub const fn kind(&self) -> NativeToolImplementationKind {
        self.kind
    }

    /// Provider owner for provider-hosted implementations.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }
}

/// Native-tool route validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeToolRouteError {
    /// One stable route field is invalid.
    #[error("invalid native tool route field `{field}`: {message}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Safe structural detail.
        message: String,
    },
}

fn safe_logical_tool_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        && bytes.len() <= 64
}

fn safe_implementation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b':' | b'/' | b'.')
        })
}

fn invalid_native_route<T>(
    field: &'static str,
    message: &'static str,
) -> Result<T, NativeToolRouteError> {
    Err(NativeToolRouteError::InvalidField {
        field,
        message: message.to_owned(),
    })
}

fn safe_option_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && bytes.len() <= 64
}

fn validate_provider_option_value(
    value: &serde_json::Value,
) -> Result<(), ProviderRequestOptionError> {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if key.is_empty()
                    || key.len() > 128
                    || key.trim() != key
                    || key.chars().any(char::is_control)
                {
                    return invalid_provider_option("data", "provider option object key is unsafe");
                }
                validate_provider_option_value(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_provider_option_value(value)?;
            }
        }
        serde_json::Value::String(value) => {
            if value.len() > PROVIDER_REQUEST_OPTION_MAX_STRING_BYTES
                || value.chars().any(char::is_control)
            {
                return invalid_provider_option("data", "provider option string is unsafe");
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn invalid_provider_option<T>(
    field: &'static str,
    message: &'static str,
) -> Result<T, ProviderRequestOptionError> {
    Err(ProviderRequestOptionError::InvalidField {
        field,
        message: message.to_owned(),
    })
}

impl ProviderStateItem {
    /// Validate one schema-v1 provider-owned JSON object.
    ///
    /// # Errors
    /// Blank identity, non-object/kind-specific data or protocol-kind mismatch.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        protocol: ProviderProtocol,
        kind: ProviderStateKind,
        data: serde_json::Value,
    ) -> Result<Self, ProviderStateError> {
        let item = Self {
            provider: provider.into(),
            model: model.into(),
            protocol,
            kind,
            schema_version: 1,
            data,
        };
        item.validate()?;
        Ok(item)
    }

    /// Revalidate deserialized state.
    ///
    /// # Errors
    /// Same contract as [`Self::new`].
    pub fn validate(&self) -> Result<(), ProviderStateError> {
        if self.provider.is_empty() || self.provider.trim() != self.provider {
            return invalid_state("provider", "provider id must be non-blank and trimmed");
        }
        if self.model.is_empty() || self.model.trim() != self.model {
            return invalid_state("model", "model id must be non-blank and trimmed");
        }
        if self.schema_version != 1 {
            return invalid_state(
                "schema_version",
                "only provider-state schema v1 is supported",
            );
        }
        let data = self
            .data
            .as_object()
            .ok_or_else(|| ProviderStateError::InvalidField {
                field: "data",
                message: "provider state data must be a JSON object".to_owned(),
            })?;
        let compatible = matches!(
            (self.protocol, self.kind),
            (
                ProviderProtocol::OpenAiResponses,
                ProviderStateKind::ResponseOutputItem
            ) | (
                ProviderProtocol::OpenAiChatCompletions,
                ProviderStateKind::ChatAssistantMessage
            ) | (
                ProviderProtocol::AnthropicMessages,
                ProviderStateKind::AnthropicMessage
            ) | (
                ProviderProtocol::GeminiGenerateContent,
                ProviderStateKind::GeminiModelContent
            ) | (
                ProviderProtocol::BedrockConverse,
                ProviderStateKind::BedrockConverseMessage
            )
        );
        if !compatible {
            return Err(ProviderStateError::ProtocolKindMismatch {
                protocol: self.protocol,
                kind: self.kind,
            });
        }
        match self.kind {
            ProviderStateKind::ResponseOutputItem => {
                if data
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return invalid_state(
                        "data",
                        "Responses output state requires a non-empty `type`",
                    );
                }
            }
            ProviderStateKind::ChatAssistantMessage => {
                if data.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
                    return invalid_state("data", "Chat assistant state requires role `assistant`");
                }
            }
            ProviderStateKind::AnthropicMessage => {
                if data.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
                    return invalid_state(
                        "data",
                        "Anthropic message state requires role `assistant`",
                    );
                }
                let content = data
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| ProviderStateError::InvalidField {
                        field: "data",
                        message: "Anthropic message state requires a `content` array".to_owned(),
                    })?;
                if data
                    .keys()
                    .any(|key| !matches!(key.as_str(), "role" | "content" | "container"))
                {
                    return invalid_state(
                        "data",
                        "Anthropic message state has unsupported top-level metadata",
                    );
                }
                if let Some(container) = data.get("container") {
                    let container =
                        container
                            .as_object()
                            .ok_or_else(|| ProviderStateError::InvalidField {
                                field: "data",
                                message: "Anthropic continuation container must be an object"
                                    .to_owned(),
                            })?;
                    if container
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|id| id.is_empty() || id.trim() != id)
                        || container
                            .get("expires_at")
                            .is_some_and(|value| !value.is_string())
                        || container
                            .get("skills")
                            .is_some_and(|value| !value.is_array())
                    {
                        return invalid_state(
                            "data",
                            "Anthropic continuation container has invalid id/expiry/skills",
                        );
                    }
                }
                for block in content {
                    let block =
                        block
                            .as_object()
                            .ok_or_else(|| ProviderStateError::InvalidField {
                                field: "data",
                                message: "Anthropic message content blocks must be objects"
                                    .to_owned(),
                            })?;
                    if block
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|value| value.is_empty() || value.trim() != value)
                    {
                        return invalid_state(
                            "data",
                            "Anthropic message content blocks require a non-empty `type`",
                        );
                    }
                }
            }
            ProviderStateKind::GeminiModelContent => {
                // <https://ai.google.dev/api/generate-content> — a `Content` is
                // a `role` plus an ordered `parts` array, and a model turn's
                // role is `model` rather than `assistant`.
                if data.get("role").and_then(serde_json::Value::as_str) != Some("model") {
                    return invalid_state("data", "Gemini model state requires role `model`");
                }
                let parts = data
                    .get("parts")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| ProviderStateError::InvalidField {
                        field: "data",
                        message: "Gemini model state requires a `parts` array".to_owned(),
                    })?;
                // A model turn always produced at least one part. An empty one
                // carries no signature and no output, so replaying it would put
                // a content block in the history that says nothing.
                if parts.is_empty() {
                    return invalid_state("data", "Gemini model state requires a nonempty `parts`");
                }
                if data
                    .keys()
                    .any(|key| !matches!(key.as_str(), "role" | "parts"))
                {
                    return invalid_state(
                        "data",
                        "Gemini model state has unsupported top-level metadata",
                    );
                }
                for part in parts {
                    if !part.is_object() {
                        return invalid_state("data", "Gemini model state parts must be objects");
                    }
                }
            }
            ProviderStateKind::BedrockConverseMessage => {
                if data.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
                    return invalid_state(
                        "data",
                        "Bedrock message state requires role `assistant`",
                    );
                }
                if data
                    .keys()
                    .any(|key| !matches!(key.as_str(), "role" | "content"))
                {
                    return invalid_state(
                        "data",
                        "Bedrock message state has unsupported top-level metadata",
                    );
                }
                let content = data
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .filter(|content| !content.is_empty())
                    .ok_or_else(|| ProviderStateError::InvalidField {
                        field: "data",
                        message: "Bedrock message state requires a nonempty `content` array"
                            .to_owned(),
                    })?;
                for block in content {
                    validate_bedrock_state_block(block)?;
                }
            }
        }
        Ok(())
    }

    /// Owning provider id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Owning canonical model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Owning protocol.
    #[must_use]
    pub const fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    /// State kind.
    #[must_use]
    pub const fn kind(&self) -> ProviderStateKind {
        self.kind
    }

    /// State schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Lossless provider-owned JSON.
    #[must_use]
    pub const fn data(&self) -> &serde_json::Value {
        &self.data
    }
}

fn validate_bedrock_state_block(block: &serde_json::Value) -> Result<(), ProviderStateError> {
    let block = block
        .as_object()
        .filter(|block| block.len() == 1)
        .ok_or_else(|| ProviderStateError::InvalidField {
            field: "data",
            message: "Bedrock content blocks must contain exactly one union member".to_owned(),
        })?;
    if let Some(text) = block.get("text") {
        if text
            .as_str()
            .is_none_or(|text| text.is_empty() || text.chars().any(char::is_control))
        {
            return invalid_state("data", "Bedrock text blocks require safe nonempty text");
        }
        return Ok(());
    }
    if let Some(reasoning) = block.get("reasoningContent") {
        let reasoning = reasoning
            .as_object()
            .filter(|reasoning| !reasoning.is_empty())
            .ok_or_else(|| ProviderStateError::InvalidField {
                field: "data",
                message: "Bedrock reasoningContent must be a nonempty object".to_owned(),
            })?;
        if reasoning
            .keys()
            .any(|key| !matches!(key.as_str(), "text" | "signature" | "redactedContent"))
            || reasoning.values().any(|value| {
                value
                    .as_str()
                    .is_none_or(|value| value.is_empty() || value.chars().any(char::is_control))
            })
        {
            return invalid_state("data", "Bedrock reasoningContent is invalid");
        }
        return Ok(());
    }
    if let Some(tool) = block.get("toolUse") {
        let tool = tool
            .as_object()
            .ok_or_else(|| ProviderStateError::InvalidField {
                field: "data",
                message: "Bedrock toolUse must be an object".to_owned(),
            })?;
        if tool
            .keys()
            .any(|key| !matches!(key.as_str(), "toolUseId" | "name" | "input"))
            || tool
                .get("toolUseId")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.is_empty() || value.chars().any(char::is_control))
            || tool
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.is_empty() || value.chars().any(char::is_control))
            || tool.get("input").is_none_or(|input| !input.is_object())
        {
            return invalid_state("data", "Bedrock toolUse block is invalid");
        }
        return Ok(());
    }
    invalid_state("data", "Bedrock content block union member is unsupported")
}

fn invalid_state<T>(
    field: &'static str,
    message: impl Into<String>,
) -> Result<T, ProviderStateError> {
    Err(ProviderStateError::InvalidField {
        field,
        message: message.into(),
    })
}

/// Token accounting for one provider response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Prompt tokens billed for the request.
    pub prompt_tokens: u64,
    /// Completion tokens generated by the response.
    pub completion_tokens: u64,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_message_state_requires_a_complete_typed_block_array() {
        let state = ProviderStateItem::new(
            "anthropic",
            "claude-test",
            ProviderProtocol::AnthropicMessages,
            ProviderStateKind::AnthropicMessage,
            serde_json::json!({
                "role":"assistant",
                "content":[
                    {"type":"thinking","thinking":"summary","signature":"opaque"},
                    {"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{}},
                    {"type":"future_block","opaque":{"kept":true}}
                ],
                "container":{"id":"container_1","expires_at":"2026-08-25T01:02:03Z"}
            }),
        )
        .unwrap();
        assert_eq!(state.kind(), ProviderStateKind::AnthropicMessage);

        for data in [
            serde_json::json!({"role":"user","content":[]}),
            serde_json::json!({"role":"assistant","content":"text"}),
            serde_json::json!({"role":"assistant","content":[{"type":""}]}),
            serde_json::json!({"role":"assistant","content":[{"type":" future "}]}),
            serde_json::json!({"role":"assistant","content":["text"]}),
            serde_json::json!({"role":"assistant","content":[],"container":"container_1"}),
            serde_json::json!({"role":"assistant","content":[],"container":{"id":" "}}),
            serde_json::json!({"role":"assistant","content":[],"future":true}),
        ] {
            assert!(
                ProviderStateItem::new(
                    "anthropic",
                    "claude-test",
                    ProviderProtocol::AnthropicMessages,
                    ProviderStateKind::AnthropicMessage,
                    data,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn anthropic_message_kind_is_rejected_on_other_protocols() {
        assert!(matches!(
            ProviderStateItem::new(
                "provider",
                "model",
                ProviderProtocol::OpenAiResponses,
                ProviderStateKind::AnthropicMessage,
                serde_json::json!({"role":"assistant","content":[]}),
            ),
            Err(ProviderStateError::ProtocolKindMismatch { .. })
        ));
    }

    #[test]
    fn gemini_model_state_requires_the_model_role_and_a_nonempty_part_array() {
        // Parts are kept opaque on purpose: `thoughtSignature` rides alongside
        // whichever content field a part set, and a newer part shape must
        // survive rather than be rejected by a stale allowlist.
        let state = ProviderStateItem::new(
            "google",
            "gemini-test",
            ProviderProtocol::GeminiGenerateContent,
            ProviderStateKind::GeminiModelContent,
            serde_json::json!({
                "role":"model",
                "parts":[
                    {"text":"answer","thoughtSignature":"opaque"},
                    {"functionCall":{"id":"fc-1","name":"read","args":{}}},
                    {"futurePart":{"kept":true}}
                ]
            }),
        )
        .unwrap();
        assert_eq!(state.kind(), ProviderStateKind::GeminiModelContent);

        for data in [
            // `assistant` is the Chat/Anthropic spelling, not Gemini's.
            serde_json::json!({"role":"assistant","parts":[{"text":"a"}]}),
            serde_json::json!({"role":"user","parts":[{"text":"a"}]}),
            serde_json::json!({"parts":[{"text":"a"}]}),
            serde_json::json!({"role":"model"}),
            serde_json::json!({"role":"model","parts":{}}),
            // An empty turn carries no signature and no output.
            serde_json::json!({"role":"model","parts":[]}),
            serde_json::json!({"role":"model","parts":["text"]}),
            serde_json::json!({"role":"model","parts":[{"text":"a"}],"future":true}),
        ] {
            assert!(
                ProviderStateItem::new(
                    "google",
                    "gemini-test",
                    ProviderProtocol::GeminiGenerateContent,
                    ProviderStateKind::GeminiModelContent,
                    data.clone(),
                )
                .is_err(),
                "{data} must be refused"
            );
        }
    }

    #[test]
    fn gemini_model_content_kind_is_rejected_on_other_protocols() {
        // The protocol/kind pair is what stops one route's model turn being
        // replayed onto another; a flag could not express it.
        for protocol in [
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions,
            ProviderProtocol::AnthropicMessages,
        ] {
            assert!(matches!(
                ProviderStateItem::new(
                    "provider",
                    "model",
                    protocol,
                    ProviderStateKind::GeminiModelContent,
                    serde_json::json!({"role":"model","parts":[{"text":"a"}]}),
                ),
                Err(ProviderStateError::ProtocolKindMismatch { .. })
            ));
        }
    }

    #[test]
    fn bedrock_message_state_preserves_ordered_reasoning_and_tool_blocks() {
        let state = ProviderStateItem::new(
            "bedrock",
            "anthropic.claude-test",
            ProviderProtocol::BedrockConverse,
            ProviderStateKind::BedrockConverseMessage,
            serde_json::json!({
                "role":"assistant",
                "content":[
                    {"reasoningContent":{"text":"summary","signature":"opaque"}},
                    {"text":"answer"},
                    {"toolUse":{"toolUseId":"tool_1","name":"read","input":{"path":"a"}}}
                ]
            }),
        )
        .unwrap();
        assert_eq!(state.kind(), ProviderStateKind::BedrockConverseMessage);
        assert_eq!(
            state.data()["content"][0]["reasoningContent"]["signature"],
            "opaque"
        );
        assert_eq!(state.data()["content"][2]["toolUse"]["toolUseId"], "tool_1");
        let debug = format!("{state:?}");
        assert!(!debug.contains("opaque"), "{debug}");
        assert!(!debug.contains("answer"), "{debug}");

        for data in [
            serde_json::json!({"role":"user","content":[{"text":"a"}]}),
            serde_json::json!({"role":"assistant","content":[]}),
            serde_json::json!({"role":"assistant","content":[{"text":""}]}),
            serde_json::json!({"role":"assistant","content":[{"text":"a","toolUse":{}}]}),
            serde_json::json!({"role":"assistant","content":[{"reasoningContent":{"signature":""}}]}),
            serde_json::json!({"role":"assistant","content":[{"toolUse":{"toolUseId":"tool_1","name":"read"}}]}),
            serde_json::json!({"role":"assistant","content":[{"future":{}}]}),
            serde_json::json!({"role":"assistant","content":[{"text":"a"}],"future":true}),
        ] {
            assert!(
                ProviderStateItem::new(
                    "bedrock",
                    "anthropic.claude-test",
                    ProviderProtocol::BedrockConverse,
                    ProviderStateKind::BedrockConverseMessage,
                    data,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn bedrock_message_kind_is_rejected_on_other_protocols() {
        for protocol in [
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions,
            ProviderProtocol::AnthropicMessages,
            ProviderProtocol::GeminiGenerateContent,
        ] {
            assert!(matches!(
                ProviderStateItem::new(
                    "bedrock",
                    "model",
                    protocol,
                    ProviderStateKind::BedrockConverseMessage,
                    serde_json::json!({"role":"assistant","content":[{"text":"a"}]}),
                ),
                Err(ProviderStateError::ProtocolKindMismatch { .. })
            ));
        }
    }

    #[test]
    fn the_gemini_protocol_rejects_every_other_state_kind() {
        for kind in [
            ProviderStateKind::ResponseOutputItem,
            ProviderStateKind::ChatAssistantMessage,
            ProviderStateKind::AnthropicMessage,
            ProviderStateKind::BedrockConverseMessage,
        ] {
            assert!(matches!(
                ProviderStateItem::new(
                    "google",
                    "gemini-test",
                    ProviderProtocol::GeminiGenerateContent,
                    kind,
                    serde_json::json!({"role":"model","parts":[{"text":"a"}]}),
                ),
                Err(ProviderStateError::ProtocolKindMismatch { .. })
            ));
        }
    }
}

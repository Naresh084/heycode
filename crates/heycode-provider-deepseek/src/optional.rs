//! Endpoint-specific DeepSeek optional capabilities.
//!
//! These surfaces do not become supported because the ordinary Chat route
//! exists. DeepSeek publishes a different request contract for each one:
//!
//! - strict function calling uses the beta Chat base and requires `strict:
//!   true` on every function;
//! - JSON Output uses the standard Chat base and a `json_object`
//!   `response_format` plus prompt-side requirements;
//! - FIM uses beta `/completions`, not Chat Completions; and
//! - chat prefix completion uses the beta Chat base and a final assistant
//!   message carrying `prefix: true`.
//!
//! The provider options in this module are durable, secret-free descriptions
//! of model-visible request changes. They intentionally stop before shared
//! adapter projection: the current Chat adapter cannot inject `strict` inside
//! every tool definition or `prefix` inside the final message, and it has no
//! FIM protocol. A composition owner must add those exact projections rather
//! than infer any of them from generic Chat support.

use heycode_core::{ProviderRequestOption, ToolSpec};
use heycode_llm::{CapabilitySupport, ChatMessage};
use thiserror::Error;

use crate::{
    DEEPSEEK_OPENAI_BASE_URL, DEEPSEEK_PROVIDER, DEEPSEEK_V4_FLASH, DEEPSEEK_V4_FLASH_VISION_EXP,
    DEEPSEEK_V4_PRO,
};

/// DeepSeek's documented base URL for beta API features.
///
/// Sources: the strict tool, chat prefix and FIM guides.
pub const DEEPSEEK_BETA_BASE_URL: &str = "https://api.deepseek.com/beta";
/// Standard Chat Completions endpoint used by JSON Output.
pub const DEEPSEEK_STANDARD_CHAT_COMPLETIONS_URL: &str =
    "https://api.deepseek.com/chat/completions";
/// Beta Chat Completions endpoint used by strict tools and chat prefix.
pub const DEEPSEEK_BETA_CHAT_COMPLETIONS_URL: &str =
    "https://api.deepseek.com/beta/chat/completions";
/// Beta FIM Completions endpoint.
pub const DEEPSEEK_BETA_FIM_COMPLETIONS_URL: &str = "https://api.deepseek.com/beta/completions";
/// Published FIM output ceiling (4K tokens).
pub const DEEPSEEK_FIM_MAX_OUTPUT_TOKENS: u16 = 4 * 1024;

/// Durable provider-option kind marking ordinary tools as strict.
pub const DEEPSEEK_STRICT_TOOLS_OPTION_KIND: &str = "strict-tools";
/// Durable provider-option kind carrying JSON Output's `response_format`.
pub const DEEPSEEK_JSON_OUTPUT_OPTION_KIND: &str = "json-output";
/// Durable provider-option kind marking the final assistant message as a
/// prefix.
pub const DEEPSEEK_CHAT_PREFIX_OPTION_KIND: &str = "chat-prefix";

const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
const FIM_COMPLETIONS_PATH: &str = "/completions";
const STRICT_TOOLS_EVIDENCE: &str = "https://api-docs.deepseek.com/guides/tool_calls/";
const JSON_OUTPUT_EVIDENCE: &str = "https://api-docs.deepseek.com/guides/json_mode/";
const FIM_EVIDENCE: &str = "https://api-docs.deepseek.com/api/create-completion/";
const CHAT_PREFIX_EVIDENCE: &str = "https://api-docs.deepseek.com/guides/chat_prefix_completion/";

/// One separately evidenced optional DeepSeek capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeepSeekOptionalCapability {
    /// Schema-constrained function calling on the beta Chat route.
    StrictTools,
    /// Valid-JSON string output on the standard Chat route.
    JsonOutput,
    /// Fill-in-the-middle completion on beta `/completions`.
    FimCompletion,
    /// Assistant-prefix completion on the beta Chat route.
    ChatPrefixCompletion,
}

impl DeepSeekOptionalCapability {
    /// Every PDS05 capability in tracker order.
    pub const ALL: [Self; 4] = [
        Self::StrictTools,
        Self::JsonOutput,
        Self::FimCompletion,
        Self::ChatPrefixCompletion,
    ];

    /// Exact endpoint contract for this capability.
    #[must_use]
    pub const fn route(self) -> DeepSeekOptionalRoute {
        match self {
            Self::StrictTools | Self::ChatPrefixCompletion => DeepSeekOptionalRoute {
                base_url: DEEPSEEK_BETA_BASE_URL,
                path: CHAT_COMPLETIONS_PATH,
                url: DEEPSEEK_BETA_CHAT_COMPLETIONS_URL,
                protocol: DeepSeekOptionalProtocol::ChatCompletions,
                beta: true,
            },
            Self::JsonOutput => DeepSeekOptionalRoute {
                base_url: DEEPSEEK_OPENAI_BASE_URL,
                path: CHAT_COMPLETIONS_PATH,
                url: DEEPSEEK_STANDARD_CHAT_COMPLETIONS_URL,
                protocol: DeepSeekOptionalProtocol::ChatCompletions,
                beta: false,
            },
            Self::FimCompletion => DeepSeekOptionalRoute {
                base_url: DEEPSEEK_BETA_BASE_URL,
                path: FIM_COMPLETIONS_PATH,
                url: DEEPSEEK_BETA_FIM_COMPLETIONS_URL,
                protocol: DeepSeekOptionalProtocol::FimCompletions,
                beta: true,
            },
        }
    }

    /// Current official primary source that names this capability's request
    /// route and wire shape.
    #[must_use]
    pub const fn evidence_url(self) -> &'static str {
        match self {
            Self::StrictTools => STRICT_TOOLS_EVIDENCE,
            Self::JsonOutput => JSON_OUTPUT_EVIDENCE,
            Self::FimCompletion => FIM_EVIDENCE,
            Self::ChatPrefixCompletion => CHAT_PREFIX_EVIDENCE,
        }
    }
}

/// Wire protocol family used by one optional endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeepSeekOptionalProtocol {
    /// OpenAI-shaped Chat Completions request/response.
    ChatCompletions,
    /// OpenAI-shaped legacy Completions request/response used for FIM.
    FimCompletions,
}

/// Fixed endpoint facts for one optional capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeepSeekOptionalRoute {
    base_url: &'static str,
    path: &'static str,
    url: &'static str,
    protocol: DeepSeekOptionalProtocol,
    beta: bool,
}

impl DeepSeekOptionalRoute {
    /// SDK base URL DeepSeek publishes for the feature.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        self.base_url
    }

    /// HTTP path beneath [`Self::base_url`].
    #[must_use]
    pub const fn path(self) -> &'static str {
        self.path
    }

    /// Complete HTTP endpoint URL.
    #[must_use]
    pub const fn url(self) -> &'static str {
        self.url
    }

    /// Request/response protocol family at this endpoint.
    #[must_use]
    pub const fn protocol(self) -> DeepSeekOptionalProtocol {
        self.protocol
    }

    /// Whether DeepSeek explicitly labels the route beta.
    #[must_use]
    pub const fn is_beta(self) -> bool {
        self.beta
    }
}

/// Provider-owned strict function-calling request boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeepSeekStrictTools;

impl DeepSeekStrictTools {
    /// Beta Chat endpoint strict functions require.
    #[must_use]
    pub const fn route() -> DeepSeekOptionalRoute {
        DeepSeekOptionalCapability::StrictTools.route()
    }

    /// Build the durable marker that makes every ordinary function strict.
    ///
    /// DeepSeek validates its evolving supported JSON Schema subset at the
    /// server. This boundary deliberately does not implement a second,
    /// inevitably drifting schema validator; it guarantees the two local
    /// facts heycode owns: the beta route and an explicit strict-mode decision.
    /// Tool definitions remain in the request's ordinary [`ToolSpec`] list so
    /// model-visible schemas are not duplicated inside provider options.
    ///
    /// # Errors
    /// An empty set fails instead of degenerating into ordinary Chat. Shared
    /// provider-option validation failures are reported without request data.
    pub fn request_option(
        tools: &[ToolSpec],
    ) -> Result<ProviderRequestOption, DeepSeekOptionalError> {
        if tools.is_empty() {
            return Err(DeepSeekOptionalError::EmptyStrictTools);
        }
        provider_option(
            DEEPSEEK_STRICT_TOOLS_OPTION_KIND,
            serde_json::json!({"enabled":true}),
        )
    }

    /// Project ordinary tool definitions into DeepSeek's exact strict wire
    /// shape.
    ///
    /// This projection guarantees `strict: true` on every function while
    /// retaining the schema value unchanged. Schema-subset acceptance remains
    /// DeepSeek's server-side boundary.
    ///
    /// # Errors
    /// An empty set fails instead of producing a request indistinguishable
    /// from ordinary Chat.
    pub fn wire_tools(tools: &[ToolSpec]) -> Result<Vec<serde_json::Value>, DeepSeekOptionalError> {
        if tools.is_empty() {
            return Err(DeepSeekOptionalError::EmptyStrictTools);
        }
        let tools = tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type":"function",
                    "function":{
                        "name":tool.name,
                        "description":tool.description,
                        "parameters":tool.parameters,
                        "strict":true
                    }
                })
            })
            .collect::<Vec<_>>();
        Ok(tools)
    }

    /// Thinking-mode support for strict function calling.
    ///
    /// DeepSeek explicitly documents strict mode as supported by both
    /// thinking and non-thinking mode.
    #[must_use]
    pub const fn thinking_support() -> CapabilitySupport {
        CapabilitySupport::Supported
    }
}

/// Provider-owned JSON Output request boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeepSeekJsonOutput;

impl DeepSeekJsonOutput {
    /// Standard Chat endpoint JSON Output uses.
    #[must_use]
    pub const fn route() -> DeepSeekOptionalRoute {
        DeepSeekOptionalCapability::JsonOutput.route()
    }

    /// Build JSON Output's exact durable `response_format` option.
    ///
    /// `effective_prompt` is the complete model-visible text projection. The
    /// documented `json` word is checked case-insensitively at word
    /// boundaries. DeepSeek also asks callers to provide an example of the
    /// desired shape; detecting whether arbitrary prose constitutes an
    /// example is not a fact this boundary can infer, so the product-facing
    /// caller must make that requirement visible.
    ///
    /// # Errors
    /// Fails if the prompt lacks the documented `json` word or if the durable
    /// provider option cannot pass shared validation.
    pub fn request_option(
        effective_prompt: &str,
    ) -> Result<ProviderRequestOption, DeepSeekOptionalError> {
        if !effective_prompt
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|word| word.eq_ignore_ascii_case("json"))
        {
            return Err(DeepSeekOptionalError::JsonPromptMissingKeyword);
        }
        provider_option(
            DEEPSEEK_JSON_OUTPUT_OPTION_KIND,
            serde_json::json!({"response_format":{"type":"json_object"}}),
        )
    }
}

/// Provider-owned beta chat-prefix request boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeepSeekChatPrefix;

impl DeepSeekChatPrefix {
    /// Beta Chat endpoint prefix completion requires.
    #[must_use]
    pub const fn route() -> DeepSeekOptionalRoute {
        DeepSeekOptionalCapability::ChatPrefixCompletion.route()
    }

    /// Build the exact final assistant message a prefix request must append.
    ///
    /// The model-visible prefix remains an ordinary [`ChatMessage`] so it is
    /// durably logged with the rest of the conversation. Empty prefixes are
    /// retained: DeepSeek requires the field and final assistant role but
    /// publishes no non-empty constraint.
    #[must_use]
    pub fn assistant_message(prefix: impl Into<String>) -> ChatMessage {
        ChatMessage::assistant(prefix)
    }

    /// Build the durable marker that makes the final assistant message a
    /// prefix.
    ///
    /// The prefix content is deliberately not duplicated inside this option.
    /// Shared provider options reject control-bearing strings, while code
    /// prefixes commonly contain newlines; the content belongs in
    /// [`Self::assistant_message`] and this marker supplies only the
    /// provider-specific `prefix: true` transform.
    ///
    /// # Errors
    /// Shared provider-option validation failures are reported without any
    /// model-visible content.
    pub fn request_option() -> Result<ProviderRequestOption, DeepSeekOptionalError> {
        provider_option(
            DEEPSEEK_CHAT_PREFIX_OPTION_KIND,
            serde_json::json!({"enabled":true}),
        )
    }
}

/// One provider-owned FIM request body.
///
/// Debug output omits the model-visible prefix and suffix.
#[derive(Clone, PartialEq, Eq)]
pub struct DeepSeekFimRequest {
    prompt: String,
    suffix: Option<String>,
    max_tokens: Option<u16>,
}

impl DeepSeekFimRequest {
    /// Construct the exact lower request for beta `/completions`.
    ///
    /// The current endpoint schema lists only `deepseek-v4-pro`, so the model
    /// is fixed rather than caller-selectable. Empty prompt/suffix strings are
    /// retained because the published schema requires strings but gives no
    /// non-empty constraint.
    ///
    /// # Errors
    /// A `max_tokens` value above DeepSeek's published 4K ceiling fails before
    /// dispatch.
    pub fn new(
        prompt: impl Into<String>,
        suffix: Option<String>,
        max_tokens: Option<u16>,
    ) -> Result<Self, DeepSeekOptionalError> {
        if let Some(requested) = max_tokens
            && requested > DEEPSEEK_FIM_MAX_OUTPUT_TOKENS
        {
            return Err(DeepSeekOptionalError::FimMaxTokensExceeded {
                requested,
                maximum: DEEPSEEK_FIM_MAX_OUTPUT_TOKENS,
            });
        }
        Ok(Self {
            prompt: prompt.into(),
            suffix,
            max_tokens,
        })
    }

    /// Beta FIM endpoint for this request.
    #[must_use]
    pub const fn route(&self) -> DeepSeekOptionalRoute {
        DeepSeekOptionalCapability::FimCompletion.route()
    }

    /// Exact model admitted by the current FIM endpoint schema.
    #[must_use]
    pub const fn model(&self) -> &'static str {
        DEEPSEEK_V4_PRO
    }

    /// Complete request JSON, omitting optional fields the caller did not set.
    #[must_use]
    pub fn body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model":self.model(),
            "prompt":self.prompt,
        });
        if let Some(suffix) = &self.suffix {
            body["suffix"] = serde_json::Value::String(suffix.clone());
        }
        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        body
    }

    /// Reconcile current model-table and endpoint-schema evidence for FIM.
    ///
    /// The model table marks V4 Flash and V4 Pro FIM-capable in non-thinking
    /// mode, but the FIM endpoint schema lists only V4 Pro. Their intersection
    /// proves Pro. Flash remains Unknown rather than being promoted from one
    /// side of a contradiction; the vision model is explicitly unsupported.
    #[must_use]
    pub fn model_support(model: &str) -> CapabilitySupport {
        match model {
            DEEPSEEK_V4_PRO => CapabilitySupport::Supported,
            DEEPSEEK_V4_FLASH => CapabilitySupport::Unknown,
            DEEPSEEK_V4_FLASH_VISION_EXP => CapabilitySupport::Unsupported,
            _ => CapabilitySupport::Unknown,
        }
    }

    /// Thinking support for the admitted Pro FIM request.
    ///
    /// DeepSeek's model table states that FIM is non-thinking only.
    #[must_use]
    pub const fn thinking_support() -> CapabilitySupport {
        CapabilitySupport::Unsupported
    }
}

impl std::fmt::Debug for DeepSeekFimRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeepSeekFimRequest")
            .field("model", &DEEPSEEK_V4_PRO)
            .field("prompt", &"[REDACTED]")
            .field("suffix", &self.suffix.as_ref().map(|_| "[REDACTED]"))
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

/// Failure admitting one endpoint-specific optional request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeepSeekOptionalError {
    /// Strict mode cannot exist without at least one strict function.
    #[error("DeepSeek strict tool mode requires at least one function")]
    EmptyStrictTools,
    /// DeepSeek requires the prompt to contain the word `json`.
    #[error("DeepSeek JSON Output requires the effective prompt to contain the word `json`")]
    JsonPromptMissingKeyword,
    /// FIM's published 4K output ceiling was exceeded.
    #[error("DeepSeek FIM max_tokens {requested} exceeds the published maximum {maximum}")]
    FimMaxTokensExceeded {
        /// Caller-supplied value.
        requested: u16,
        /// Published ceiling.
        maximum: u16,
    },
    /// The secret-free durable provider option failed shared validation.
    #[error("DeepSeek optional provider request data is invalid")]
    InvalidProviderOption,
}

fn provider_option(
    kind: &'static str,
    data: serde_json::Value,
) -> Result<ProviderRequestOption, DeepSeekOptionalError> {
    ProviderRequestOption::new(DEEPSEEK_PROVIDER, kind, data)
        .map_err(|_| DeepSeekOptionalError::InvalidProviderOption)
}

//! DeepSeek's Anthropic-format route, and exactly where its compatibility ends.
//!
//! DeepSeek serves the same V4 models over an Anthropic-Messages-shaped
//! endpoint alongside its OpenAI-shaped one and publishes a field-by-field
//! compatibility table for it
//! (<https://api-docs.deepseek.com/guides/anthropic_api>). This module
//! transcribes that table row by row rather than paraphrasing it, because the
//! interesting part of a vendor's "Anthropic-compatible" endpoint is never the
//! rows it lists — it is the rows it leaves out.
//!
//! Three absences are load-bearing:
//!
//! * **`thinking.signature`.** DeepSeek's message-field table gives sub-field
//!   rows for `text`, `tool_use` and `tool_result`, and gives the `thinking`
//!   block a bare "Supported" with no sub-fields at all. Anthropic thinking
//!   blocks carry an opaque `signature` that must be replayed unmodified, and
//!   heycode's own Messages parser refuses a `thinking` block that finishes
//!   without one. Whether DeepSeek emits, ignores or rejects that field is
//!   therefore the single fact this route most depends on, and DeepSeek
//!   publishes nothing about it. It stays
//!   [`heycode_llm::CapabilitySupport::Unknown`] and is never promoted from
//!   documentation alone.
//! * **The Messages path.** DeepSeek documents the Anthropic *base URL*
//!   `https://api.deepseek.com/anthropic`
//!   (<https://api-docs.deepseek.com/quick_start/pricing>, row "BASE URL
//!   (Anthropic Format)") but no path beneath it. The `/v1/messages` suffix in
//!   [`DEEPSEEK_ANTHROPIC_MESSAGES_BASE_URL`] is inherited from the Anthropic
//!   SDK that page tells you to point at that base, not published by DeepSeek.
//!   An unauthenticated probe cannot settle it either: DeepSeek's gateway
//!   authenticates before it routes, so an invented path and a real one both
//!   answer `401`.
//! * **Every response-side field.** The compatibility page covers headers,
//!   request fields, tools and message blocks. It has no table for
//!   `stop_reason`, usage/cache counters, streaming event names or the error
//!   envelope, so this module reports all of them as
//!   [`DeepSeekAnthropicSupport::Undocumented`].
//!
//! None of the three can be closed without a live credential, and PDS04 ran
//! with none.
//!
//! What the table *does* settle is worth having: the route's model field is a
//! silent-substitution hazard rather than a validated id
//! ([`classify_model`]), thinking is toggled with Anthropic's own `thinking`
//! block while effort travels in `output_config.effort`
//! (<https://api-docs.deepseek.com/guides/thinking_mode>), and prompt caching
//! is not addressable at all because every `cache_control` row reads
//! "Ignored".

use heycode_http::HttpService;
use heycode_llm::{
    AnthropicAuthWire, AnthropicMessagesAdapter, AnthropicMessagesConfig, AnthropicThinkingMode,
    AuthenticationBinding, CapabilitySupport, ChatRequest, ChunkStream, InferenceAdapter,
    InferenceStream, LlmError, Provider, ProviderDescriptor, ProviderInfo, ProviderProfile,
    ReasoningEffortId, RequestDraft, ResolveError, ResolvedCall, RouteCredential,
};
use thiserror::Error;

use crate::{DEEPSEEK_V4_FLASH, DEEPSEEK_V4_PRO, provider_descriptor};

fn inference_descriptor() -> ProviderDescriptor {
    let mut descriptor = provider_descriptor();
    descriptor.protocols = vec![heycode_core::ProviderProtocol::AnthropicMessages];
    descriptor
}

/// Base URL DeepSeek publishes for Anthropic-format clients.
///
/// Source: <https://api-docs.deepseek.com/guides/anthropic_api> ("the
/// `base_url` being `https://api.deepseek.com/anthropic`") and the "BASE URL
/// (Anthropic Format)" row of
/// <https://api-docs.deepseek.com/quick_start/pricing>.
pub const DEEPSEEK_ANTHROPIC_BASE_URL: &str = "https://api.deepseek.com/anthropic";

/// Base URL an Anthropic Messages client must be configured with so that
/// `POST {base}/messages` reaches DeepSeek.
///
/// DeepSeek publishes the SDK `base_url` in
/// [`DEEPSEEK_ANTHROPIC_BASE_URL`] and no path under it. The `/v1` segment is
/// the Anthropic SDK's own, exactly as Anthropic's native route is
/// `https://api.anthropic.com/v1/messages`. That makes this constant a
/// *composition* of one DeepSeek fact and one Anthropic fact, not a DeepSeek
/// fact — see [`DeepSeekAnthropicField::MessagesPath`], which reports it as
/// undocumented for that reason.
pub const DEEPSEEK_ANTHROPIC_MESSAGES_BASE_URL: &str = "https://api.deepseek.com/anthropic/v1";

/// Vision-preview V4 model id.
///
/// Source: the "MODEL" row of <https://api-docs.deepseek.com/quick_start/pricing>,
/// whose "Anthropic API" row marks this model `✓` alongside the other two.
pub const DEEPSEEK_V4_FLASH_VISION_EXP: &str = "deepseek-v4-flash-vision-exp";

/// Model id DeepSeek's Claude Code and coding-agent guides tell you to send.
///
/// Sources: <https://api-docs.deepseek.com/quick_start/agent_integrations/claude_code>
/// and <https://api-docs.deepseek.com/guides/coding_agents>, both of which set
/// `ANTHROPIC_MODEL=deepseek-v4-pro[1m]`. The model table on
/// <https://api-docs.deepseek.com/quick_start/pricing> does not list this id,
/// so what it selects — and how it relates to [`DEEPSEEK_V4_PRO`] — is
/// unpublished. heycode accepts it on this route because DeepSeek's own
/// instructions send it, and claims nothing further about it.
pub const DEEPSEEK_V4_PRO_1M: &str = "deepseek-v4-pro[1m]";

/// Published maximum output length shared by every current V4 model.
///
/// Source: the "MAX OUTPUT | MAXIMUM: 384K" row of
/// <https://api-docs.deepseek.com/quick_start/pricing>.
pub const DEEPSEEK_V4_MAX_OUTPUT_TOKENS: u64 = 384 * 1024;

/// Placeholder `thinking.budget_tokens` this route sends.
///
/// Anthropic requires `budget_tokens` whenever `thinking.type` is `enabled`,
/// and heycode's Messages adapter enforces a floor of 1024. DeepSeek documents
/// the field as ignored ("Supported (`budget_tokens` is ignored)"), so no
/// value carries meaning here and the adapter's own minimum is sent rather
/// than a number that would read as a chosen budget. Effort is carried by
/// `output_config.effort` instead — see [`DeepSeekAnthropicEffort`].
pub const DEEPSEEK_IGNORED_THINKING_BUDGET_TOKENS: u64 = 1_024;

/// What DeepSeek's compatibility table publishes about one Anthropic Messages
/// field.
///
/// The first four variants are DeepSeek's own vocabulary, transcribed. The
/// final two preserve different kinds of Unknown evidence: an official
/// integration recipe that implies a wire behavior without specifying it,
/// and a field the official pages do not address at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeepSeekAnthropicSupport {
    /// DeepSeek documents the field as behaving as Anthropic defines it
    /// ("Fully Supported" or "Supported").
    Supported,
    /// DeepSeek documents that it accepts the field and does nothing with it
    /// ("Ignored"). Accepting a field is not honouring it.
    Ignored,
    /// DeepSeek documents the field as refused ("Not Supported").
    NotSupported,
    /// DeepSeek documents that it substitutes its own value ("Use DeepSeek
    /// Model Instead"). The request does not fail; it is answered by something
    /// other than what was asked for.
    Remapped,
    /// An official DeepSeek integration recipe requires behavior that its
    /// compatibility table does not specify at the wire level.
    IntegrationInferred,
    /// DeepSeek's page says nothing about the field.
    Undocumented,
}

impl DeepSeekAnthropicSupport {
    /// Project onto the workspace capability tri-state.
    ///
    /// Only a documented `Supported` row becomes
    /// [`CapabilitySupport::Supported`]. Integration-inferred and undocumented
    /// fields become [`CapabilitySupport::Unknown`] and can never become
    /// anything else from this function — closing an unknown needs evidence,
    /// not a projection.
    #[must_use]
    pub const fn capability(self) -> CapabilitySupport {
        match self {
            Self::Supported => CapabilitySupport::Supported,
            Self::Ignored | Self::NotSupported | Self::Remapped => CapabilitySupport::Unsupported,
            Self::IntegrationInferred | Self::Undocumented => CapabilitySupport::Unknown,
        }
    }

    /// Stable lowercase identifier for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Ignored => "ignored",
            Self::NotSupported => "not-supported",
            Self::Remapped => "remapped",
            Self::IntegrationInferred => "integration-inferred",
            Self::Undocumented => "undocumented",
        }
    }
}

/// One row of DeepSeek's Anthropic API compatibility table, plus the
/// load-bearing fields it has no row for.
///
/// This is deliberately exhaustive rather than a summary: a partial
/// transcription hides exactly the rows a reader would want to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DeepSeekAnthropicField {
    // ---- HTTP Header -----------------------------------------------------
    /// `anthropic-beta` request header on `/messages`.
    AnthropicBetaHeader,
    /// `anthropic-version` request header.
    AnthropicVersionHeader,
    /// `x-api-key` request header.
    XApiKeyHeader,
    /// `Authorization: Bearer` request header.
    AuthorizationBearerHeader,

    // ---- Simple fields ---------------------------------------------------
    /// Top-level `model`.
    Model,
    /// Top-level `max_tokens`.
    MaxTokens,
    /// Top-level `container`.
    Container,
    /// Top-level `mcp_servers`.
    McpServers,
    /// `metadata.user_id`.
    MetadataUserId,
    /// Any `metadata` field other than `user_id`.
    MetadataOtherFields,
    /// Top-level `service_tier`.
    ServiceTier,
    /// Top-level `stop_sequences`.
    StopSequences,
    /// Top-level `stream`.
    Stream,
    /// Top-level `system`.
    System,
    /// Top-level `temperature`.
    Temperature,
    /// Top-level `thinking`.
    Thinking,
    /// `thinking.budget_tokens`.
    ThinkingBudgetTokens,
    /// `output_config.effort`.
    OutputConfigEffort,
    /// Any `output_config` field other than `effort`.
    OutputConfigOtherFields,
    /// Top-level `top_k`.
    TopK,
    /// Top-level `top_p`.
    TopP,

    // ---- Tool fields -----------------------------------------------------
    /// `tools[].name`.
    ToolName,
    /// `tools[].input_schema`.
    ToolInputSchema,
    /// `tools[].description`.
    ToolDescription,
    /// `tools[].cache_control`.
    ToolCacheControl,
    /// `tool_choice: {"type":"none"}`.
    ToolChoiceNone,
    /// `tool_choice: {"type":"auto"}`.
    ToolChoiceAuto,
    /// `tool_choice: {"type":"any"}`.
    ToolChoiceAny,
    /// `tool_choice: {"type":"tool"}`.
    ToolChoiceTool,
    /// `tool_choice.disable_parallel_tool_use`.
    DisableParallelToolUse,
    /// The versioned server-tool `type` string a web-search request must
    /// carry.
    WebSearchServerToolDefinition,

    // ---- Message fields --------------------------------------------------
    /// `content` given as a bare string.
    ContentString,
    /// `content[].type == "text"`, sub-field `text`.
    TextBlock,
    /// `text` block `cache_control`.
    TextCacheControl,
    /// `text` block `citations`.
    TextCitations,
    /// `content[].type == "image"` with a `base64` or `url` source.
    ImageBlock,
    /// `content[].type == "image"` with a `file` source.
    ImageFileSource,
    /// `content[].type == "document"`.
    DocumentBlock,
    /// `content[].type == "search_result"`.
    SearchResultBlock,
    /// `content[].type == "thinking"`.
    ThinkingBlock,
    /// The `signature` sub-field of a `thinking` block.
    ThinkingBlockSignature,
    /// `content[].type == "redacted_thinking"`.
    RedactedThinkingBlock,
    /// `tool_use` block `id`.
    ToolUseId,
    /// `tool_use` block `input`.
    ToolUseInput,
    /// `tool_use` block `name`.
    ToolUseName,
    /// `tool_use` block `cache_control`.
    ToolUseCacheControl,
    /// `tool_result` block `tool_use_id`.
    ToolResultToolUseId,
    /// `tool_result` block `content`.
    ToolResultContent,
    /// `tool_result` block `cache_control`.
    ToolResultCacheControl,
    /// `tool_result` block `is_error`.
    ToolResultIsError,
    /// `content[].type == "server_tool_use"`.
    ServerToolUseBlock,
    /// `content[].type == "web_search_tool_result"`.
    WebSearchToolResultBlock,
    /// `content[].type == "code_execution_tool_result"`.
    CodeExecutionToolResultBlock,
    /// `content[].type == "mcp_tool_use"`.
    McpToolUseBlock,
    /// `content[].type == "mcp_tool_result"`.
    McpToolResultBlock,
    /// `content[].type == "container_upload"`.
    ContainerUploadBlock,

    // ---- Fields with no row at all ---------------------------------------
    /// The path beneath the documented Anthropic base URL.
    MessagesPath,
    /// Response `stop_reason` values.
    ResponseStopReason,
    /// Response `usage` token counters.
    ResponseUsage,
    /// Response `usage` prompt-cache counters.
    ResponseUsageCacheCounters,
    /// The set and ordering of streaming event names.
    StreamingEventNames,
    /// The shape of an error body returned by the Messages route.
    ErrorEnvelope,
}

impl DeepSeekAnthropicField {
    /// Every field this module takes a position on, in declaration order.
    pub const ALL: [Self; 62] = [
        Self::AnthropicBetaHeader,
        Self::AnthropicVersionHeader,
        Self::XApiKeyHeader,
        Self::AuthorizationBearerHeader,
        Self::Model,
        Self::MaxTokens,
        Self::Container,
        Self::McpServers,
        Self::MetadataUserId,
        Self::MetadataOtherFields,
        Self::ServiceTier,
        Self::StopSequences,
        Self::Stream,
        Self::System,
        Self::Temperature,
        Self::Thinking,
        Self::ThinkingBudgetTokens,
        Self::OutputConfigEffort,
        Self::OutputConfigOtherFields,
        Self::TopK,
        Self::TopP,
        Self::ToolName,
        Self::ToolInputSchema,
        Self::ToolDescription,
        Self::ToolCacheControl,
        Self::ToolChoiceNone,
        Self::ToolChoiceAuto,
        Self::ToolChoiceAny,
        Self::ToolChoiceTool,
        Self::DisableParallelToolUse,
        Self::WebSearchServerToolDefinition,
        Self::ContentString,
        Self::TextBlock,
        Self::TextCacheControl,
        Self::TextCitations,
        Self::ImageBlock,
        Self::ImageFileSource,
        Self::DocumentBlock,
        Self::SearchResultBlock,
        Self::ThinkingBlock,
        Self::ThinkingBlockSignature,
        Self::RedactedThinkingBlock,
        Self::ToolUseId,
        Self::ToolUseInput,
        Self::ToolUseName,
        Self::ToolUseCacheControl,
        Self::ToolResultToolUseId,
        Self::ToolResultContent,
        Self::ToolResultCacheControl,
        Self::ToolResultIsError,
        Self::ServerToolUseBlock,
        Self::WebSearchToolResultBlock,
        Self::CodeExecutionToolResultBlock,
        Self::McpToolUseBlock,
        Self::McpToolResultBlock,
        Self::ContainerUploadBlock,
        Self::MessagesPath,
        Self::ResponseStopReason,
        Self::ResponseUsage,
        Self::ResponseUsageCacheCounters,
        Self::StreamingEventNames,
        Self::ErrorEnvelope,
    ];

    /// Evidence DeepSeek publishes or implies for this field.
    ///
    /// Every `Supported`, `Ignored`, `NotSupported` and `Remapped` arm is a
    /// transcription of one row of
    /// <https://api-docs.deepseek.com/guides/anthropic_api>, except where the
    /// arm's own comment cites a different page. `IntegrationInferred` names
    /// an official recipe without an explicit wire guarantee. Every
    /// `Undocumented` arm is an absence on those pages, not a guess about
    /// behaviour.
    #[must_use]
    pub const fn documented_support(self) -> DeepSeekAnthropicSupport {
        use DeepSeekAnthropicSupport as Support;
        match self {
            // "anthropic-beta | Ignored for /messages; required
            // (files-api-2025-04-14) for Files API endpoints".
            Self::AnthropicBetaHeader => Support::Ignored,
            // "anthropic-version | Ignored".
            Self::AnthropicVersionHeader => Support::Ignored,
            // "x-api-key | Fully Supported".
            Self::XApiKeyHeader => Support::Supported,
            // No header-table row. DeepSeek's Claude Code guide sets
            // `ANTHROPIC_AUTH_TOKEN`, and Claude Code currently projects that
            // as `Authorization: Bearer`, but the DeepSeek page does not state
            // the header scheme. That is official integration evidence, not a
            // published wire guarantee.
            Self::AuthorizationBearerHeader => Support::IntegrationInferred,

            // "model | Use DeepSeek Model Instead", plus the mapping rules and
            // the note that an unsupported model name "will automatically map
            // it to the deepseek-v4-flash model".
            Self::Model => Support::Remapped,
            // "max_tokens | Fully Supported".
            Self::MaxTokens => Support::Supported,
            // "container | Ignored".
            Self::Container => Support::Ignored,
            // "mcp_servers | Ignored".
            Self::McpServers => Support::Ignored,
            // "metadata | user_id is supported, others are ignored".
            Self::MetadataUserId => Support::Supported,
            Self::MetadataOtherFields => Support::Ignored,
            // "service_tier | Ignored".
            Self::ServiceTier => Support::Ignored,
            // "stop_sequences | Fully Supported".
            Self::StopSequences => Support::Supported,
            // "stream | Fully Supported".
            Self::Stream => Support::Supported,
            // "system | Fully Supported".
            Self::System => Support::Supported,
            // "temperature | Fully Supported (range [0.0 ~ 2.0])". This row is
            // in tension with
            // <https://api-docs.deepseek.com/guides/thinking_mode>, which says
            // thinking mode "does not support the temperature, top_p,
            // presence_penalty, or frequency_penalty parameters" and that
            // setting them "will not trigger an error but will also have no
            // effect". The table's claim is transcribed here; the interaction
            // is reported separately by
            // [`temperature_with_thinking_enabled`].
            Self::Temperature => Support::Supported,
            // "thinking | Supported (budget_tokens is ignored)".
            Self::Thinking => Support::Supported,
            Self::ThinkingBudgetTokens => Support::Ignored,
            // "output_config | Only effort is supported".
            Self::OutputConfigEffort => Support::Supported,
            Self::OutputConfigOtherFields => Support::Ignored,
            // "top_k | Ignored".
            Self::TopK => Support::Ignored,
            // "top_p | Fully Supported".
            Self::TopP => Support::Supported,

            // tools: "name | Fully Supported", "input_schema | Fully
            // Supported", "description | Fully Supported", "cache_control |
            // Ignored".
            Self::ToolName | Self::ToolInputSchema | Self::ToolDescription => Support::Supported,
            Self::ToolCacheControl => Support::Ignored,
            // tool_choice: "none | Fully Supported"; "auto"/"any"/"tool" each
            // "Supported (disable_parallel_tool_use is ignored)".
            Self::ToolChoiceNone
            | Self::ToolChoiceAuto
            | Self::ToolChoiceAny
            | Self::ToolChoiceTool => Support::Supported,
            Self::DisableParallelToolUse => Support::Ignored,
            // The message-field table marks the `server_tool_use` and
            // `web_search_tool_result` *response* blocks Supported, and the
            // Claude Code guide says "The DeepSeek API natively supports the
            // Web Search feature in Claude Code". Neither publishes the
            // versioned server-tool `type` a request must send to enable it,
            // so the request half stays undocumented and this route configures
            // no server tool.
            Self::WebSearchServerToolDefinition => Support::Undocumented,

            // "content | string | Fully Supported".
            Self::ContentString => Support::Supported,
            // 'array, type="text" | text | Fully Supported'.
            Self::TextBlock => Support::Supported,
            Self::TextCacheControl | Self::TextCitations => Support::Ignored,
            // 'array, type="image" | source | Supported. source.type can be
            // base64 ... url, or file'.
            Self::ImageBlock => Support::Supported,
            // The same row says the `file` variant "requires the header
            // anthropic-beta: files-api-2025-04-14", while the header table
            // says `anthropic-beta` is "Ignored for /messages". Both cannot
            // hold for a `/messages` request carrying a file image, so the
            // page does not, on net, publish a usable answer for this variant.
            Self::ImageFileSource => Support::Undocumented,
            // 'array, type = "document" | Not Supported'.
            Self::DocumentBlock => Support::NotSupported,
            // 'array, type = "search_result" | Not Supported'.
            Self::SearchResultBlock => Support::NotSupported,
            // 'array, type = "thinking" | Supported'. The row lists no
            // sub-fields, unlike the text/tool_use/tool_result rows.
            Self::ThinkingBlock => Support::Supported,
            // No row anywhere on the page mentions `signature`. Anthropic
            // thinking blocks carry one and require it back unmodified, and
            // heycode's Messages parser rejects a thinking block that finishes
            // without one, so this absence decides whether reasoning works at
            // all on this route.
            Self::ThinkingBlockSignature => Support::Undocumented,
            // 'array, type="redacted_thinking" | Not Supported'.
            Self::RedactedThinkingBlock => Support::NotSupported,
            // 'array, type = "tool_use" | id/input/name | Fully Supported',
            // 'cache_control | Ignored'.
            Self::ToolUseId | Self::ToolUseInput | Self::ToolUseName => Support::Supported,
            Self::ToolUseCacheControl => Support::Ignored,
            // 'array, type = "tool_result" | tool_use_id | Fully Supported',
            // 'content | Fully Supported', 'cache_control | Ignored',
            // 'is_error | Ignored'.
            Self::ToolResultToolUseId | Self::ToolResultContent => Support::Supported,
            Self::ToolResultCacheControl | Self::ToolResultIsError => Support::Ignored,
            // 'array, type = "server_tool_use" | Supported',
            // 'array, type = "web_search_tool_result" | Supported'.
            Self::ServerToolUseBlock | Self::WebSearchToolResultBlock => Support::Supported,
            // The remaining block rows all read "Not Supported".
            Self::CodeExecutionToolResultBlock
            | Self::McpToolUseBlock
            | Self::McpToolResultBlock
            | Self::ContainerUploadBlock => Support::NotSupported,

            // DeepSeek publishes the Anthropic base URL and no path under it,
            // and its gateway authenticates before it routes, so even an
            // unauthenticated probe cannot distinguish a real path from an
            // invented one — every path answers 401.
            Self::MessagesPath => Support::Undocumented,
            // The compatibility page has no response-side table at all.
            Self::ResponseStopReason
            | Self::ResponseUsage
            | Self::ResponseUsageCacheCounters
            | Self::StreamingEventNames
            | Self::ErrorEnvelope => Support::Undocumented,
        }
    }

    /// Workspace tri-state capability for this field.
    #[must_use]
    pub const fn capability(self) -> CapabilitySupport {
        self.documented_support().capability()
    }
}

/// What DeepSeek publishes about sending `temperature` with thinking enabled.
///
/// The Anthropic compatibility table calls `temperature` "Fully Supported",
/// while <https://api-docs.deepseek.com/guides/thinking_mode> says thinking
/// mode "does not support the temperature, top_p, presence_penalty, or
/// frequency_penalty parameters" and that setting them "will not trigger an
/// error but will also have no effect". The thinking-mode page is written
/// about the model rather than one dialect and never restates the exemption
/// per dialect, so which sentence governs an Anthropic-format request is not
/// settled by either page.
///
/// heycode does not depend on the answer. PDS02 removes `temperature` from
/// thinking requests on the OpenAI-format route, and `heycode-llm`'s Messages
/// adapter independently clears it during resolution whenever the selected
/// thinking mode is enabled — so a thinking request from this profile never
/// carries the field either way, and the conflict never reaches the wire. What
/// stays unknown is only what DeepSeek would do with a temperature it did
/// receive alongside enabled thinking; with thinking disabled the table's
/// "Fully Supported" governs and the field is sent.
#[must_use]
pub const fn temperature_with_thinking_enabled() -> CapabilitySupport {
    CapabilitySupport::Unknown
}

/// How DeepSeek's Anthropic-format endpoint resolves a requested `model`.
///
/// The endpoint never rejects a model name. Every classification below except
/// [`Self::UnmappedDefault`] describes a name whose destination DeepSeek
/// publishes; that last one describes a name DeepSeek answers with
/// `deepseek-v4-flash` without saying so in the response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepSeekAnthropicModelRoute {
    /// An id DeepSeek's model table lists as served over the Anthropic API.
    Published {
        /// The id itself.
        model: &'static str,
    },
    /// An id DeepSeek's integration guides send but its model table omits.
    IntegrationGuideOnly {
        /// The id itself.
        model: &'static str,
    },
    /// A Claude-family name prefix DeepSeek documents a mapping for.
    ClaudeAlias {
        /// The documented prefix that matched.
        prefix: &'static str,
        /// The DeepSeek model the prefix selects.
        model: &'static str,
    },
    /// Any other name. DeepSeek serves it with `deepseek-v4-flash` and reports
    /// no substitution.
    UnmappedDefault {
        /// The model DeepSeek silently substitutes.
        model: &'static str,
    },
}

impl DeepSeekAnthropicModelRoute {
    /// The DeepSeek model this route actually reaches.
    #[must_use]
    pub const fn model(&self) -> &'static str {
        match self {
            Self::Published { model }
            | Self::IntegrationGuideOnly { model }
            | Self::ClaudeAlias { model, .. }
            | Self::UnmappedDefault { model } => model,
        }
    }

    /// Whether DeepSeek reaches this model by substituting it for a name it
    /// did not recognize.
    #[must_use]
    pub const fn is_silent_fallback(&self) -> bool {
        matches!(self, Self::UnmappedDefault { .. })
    }
}

/// Classify a requested `model` against DeepSeek's published routing rules.
///
/// Sources: <https://api-docs.deepseek.com/guides/anthropic_api> — "Models
/// starting with `claude-opus` are mapped to `deepseek-v4-pro`", "Models
/// starting with `claude-haiku` or `claude-sonnet` are mapped to
/// `deepseek-v4-flash`", and "When you pass an unsupported model name to
/// DeepSeek's Anthropic API, the API backend will automatically map it to the
/// `deepseek-v4-flash` model".
///
/// Matching is exact and case-sensitive because that is how the prefixes are
/// written. A differently-cased name therefore classifies as
/// [`DeepSeekAnthropicModelRoute::UnmappedDefault`], which is the safe
/// direction: [`admit_model`] refuses it rather than assuming a
/// case-insensitive match DeepSeek never promised.
#[must_use]
pub fn classify_model(requested: &str) -> DeepSeekAnthropicModelRoute {
    for model in [
        DEEPSEEK_V4_FLASH,
        DEEPSEEK_V4_PRO,
        DEEPSEEK_V4_FLASH_VISION_EXP,
    ] {
        if requested == model {
            return DeepSeekAnthropicModelRoute::Published { model };
        }
    }
    if requested == DEEPSEEK_V4_PRO_1M {
        return DeepSeekAnthropicModelRoute::IntegrationGuideOnly {
            model: DEEPSEEK_V4_PRO_1M,
        };
    }
    for (prefix, model) in [
        ("claude-opus", DEEPSEEK_V4_PRO),
        ("claude-haiku", DEEPSEEK_V4_FLASH),
        ("claude-sonnet", DEEPSEEK_V4_FLASH),
    ] {
        if requested.starts_with(prefix) {
            return DeepSeekAnthropicModelRoute::ClaudeAlias { prefix, model };
        }
    }
    DeepSeekAnthropicModelRoute::UnmappedDefault {
        model: DEEPSEEK_V4_FLASH,
    }
}

/// Resolve a requested `model` to the DeepSeek model that will answer, or
/// refuse a name DeepSeek would silently substitute for.
///
/// This is the guard the route exists for. A typo, a retired id or a
/// still-supported id this build has not been taught all reach the endpoint as
/// an unrecognized name, and the endpoint answers every one of them with
/// `deepseek-v4-flash` at flash prices without marking the response. Failing
/// before dispatch is the only place that substitution is still visible.
///
/// # Errors
/// [`DeepSeekAnthropicError::SilentModelFallback`] for any name that
/// classifies as [`DeepSeekAnthropicModelRoute::UnmappedDefault`].
pub fn admit_model(requested: &str) -> Result<&'static str, DeepSeekAnthropicError> {
    let route = classify_model(requested);
    if route.is_silent_fallback() {
        return Err(DeepSeekAnthropicError::SilentModelFallback {
            requested: requested.to_owned(),
            served: route.model(),
        });
    }
    Ok(route.model())
}

/// A reasoning choice this route offers, and the exact wire value it sends.
///
/// Source: the "Thinking Mode Toggle" and "Thinking Effort Control" rows of
/// <https://api-docs.deepseek.com/guides/thinking_mode>. The toggle cell spans
/// the OpenAI and Anthropic columns as `{"thinking": {"type":
/// "enabled/disabled"}}`; the Anthropic effort cell is `{"output_config":
/// {"effort": "low/high/max"}}`.
///
/// DeepSeek's effort-mapping table also accepts `medium` and `xhigh`, both of
/// which it remaps to `high`. Neither is offered here: a choice that silently
/// becomes another choice is a worse interface than not having it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeepSeekAnthropicEffort {
    /// Thinking off — `thinking: {"type":"disabled"}`, no `output_config`.
    None,
    /// `output_config.effort = "low"`, which DeepSeek maps to actual `low`.
    Low,
    /// `output_config.effort = "high"`. DeepSeek's default effort.
    High,
    /// `output_config.effort = "max"`, which DeepSeek maps to actual `max`.
    Max,
}

impl DeepSeekAnthropicEffort {
    /// Every offered choice, weakest first.
    pub const ALL: [Self; 4] = [Self::None, Self::Low, Self::High, Self::Max];

    /// DeepSeek's own default effort.
    ///
    /// Source: footnote (1) of
    /// <https://api-docs.deepseek.com/guides/thinking_mode> — "Thinking mode
    /// is enabled by default, with the default effort being high".
    pub const DEFAULT: Self = Self::High;

    /// Canonical heycode reasoning id for this choice.
    #[must_use]
    pub const fn canonical_id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// Exact `output_config.effort` value, or `None` when the choice is
    /// carried by the `thinking` toggle alone.
    #[must_use]
    pub const fn wire_effort(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Low => Some("low"),
            Self::High => Some("high"),
            Self::Max => Some("max"),
        }
    }

    /// Validated canonical reasoning id.
    ///
    /// # Errors
    /// [`DeepSeekAnthropicError::Route`] if `heycode-llm` ever stops accepting
    /// one of these literals. The ids are compile-time constants, so this is a
    /// reported impossibility rather than a panic.
    pub fn reasoning_effort_id(self) -> Result<ReasoningEffortId, DeepSeekAnthropicError> {
        ReasoningEffortId::new(self.canonical_id()).map_err(|error| DeepSeekAnthropicError::Route {
            message: error.to_string(),
        })
    }

    /// Messages thinking wire mode for this choice.
    #[must_use]
    pub fn thinking_mode(self) -> AnthropicThinkingMode {
        match self.wire_effort() {
            // DeepSeek documents only `enabled` and `disabled`; the adaptive
            // dialect is Anthropic's and appears nowhere on DeepSeek's pages,
            // so it is never sent here.
            None => AnthropicThinkingMode::disabled(),
            Some(effort) => {
                AnthropicThinkingMode::enabled(DEEPSEEK_IGNORED_THINKING_BUDGET_TOKENS, None)
                    .with_wire_effort(effort)
            }
        }
    }
}

/// Failure building or using the Anthropic-format DeepSeek route.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeepSeekAnthropicError {
    /// The requested model is one DeepSeek would answer with a substitute.
    #[error(
        "DeepSeek's Anthropic route would silently serve `{served}` for model `{requested}`; \
         no response field reports the substitution"
    )]
    SilentModelFallback {
        /// Name the caller asked for.
        requested: String,
        /// Model DeepSeek would answer with.
        served: &'static str,
    },
    /// The route could not be constructed from its own constants.
    #[error("DeepSeek Anthropic route could not be built: {message}")]
    Route {
        /// Already-safe text from the underlying validation.
        message: String,
    },
}

/// DeepSeek's Anthropic-format route, paired with the auth header it sends.
///
/// The base URL is fixed. There is deliberately no override: a profile whose
/// endpoint can be redirected is a profile whose citations no longer describe
/// where a request goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeepSeekAnthropicProfile {
    auth_wire: AnthropicAuthWire,
    max_output_tokens: u64,
}

impl Default for DeepSeekAnthropicProfile {
    fn default() -> Self {
        Self::api_key()
    }
}

impl DeepSeekAnthropicProfile {
    /// Route authenticating with `x-api-key`.
    ///
    /// This is the header DeepSeek's compatibility table marks "Fully
    /// Supported", and the one the Anthropic SDK sends for
    /// `ANTHROPIC_API_KEY`.
    #[must_use]
    pub const fn api_key() -> Self {
        Self {
            auth_wire: AnthropicAuthWire::XApiKey,
            max_output_tokens: DEEPSEEK_V4_MAX_OUTPUT_TOKENS,
        }
    }

    /// Route projecting the Claude Code recipe as `Authorization: Bearer`.
    ///
    /// DeepSeek's Claude Code guide configures `ANTHROPIC_AUTH_TOKEN`, while
    /// the DeepSeek compatibility header table names only `x-api-key`.
    /// Current Claude Code behavior makes bearer the operational projection,
    /// but [`DeepSeekAnthropicField::AuthorizationBearerHeader`] remains
    /// [`DeepSeekAnthropicSupport::IntegrationInferred`] and therefore
    /// [`CapabilitySupport::Unknown`] until DeepSeek documents or a gated live
    /// check observes that exact wire behavior.
    #[must_use]
    pub const fn auth_token() -> Self {
        Self {
            auth_wire: AnthropicAuthWire::Bearer,
            max_output_tokens: DEEPSEEK_V4_MAX_OUTPUT_TOKENS,
        }
    }

    /// Override the adapter-owned `max_tokens` default.
    ///
    /// The default is DeepSeek's published per-model maximum, so a caller who
    /// sets nothing is capped by the model rather than by a number heycode
    /// invented.
    #[must_use]
    pub const fn with_max_output_tokens(mut self, max_output_tokens: u64) -> Self {
        self.max_output_tokens = max_output_tokens;
        self
    }

    /// Auth header dialect this route sends.
    #[must_use]
    pub const fn auth_wire(&self) -> AnthropicAuthWire {
        self.auth_wire
    }

    /// Adapter-owned `max_tokens` default.
    #[must_use]
    pub const fn max_output_tokens(&self) -> u64 {
        self.max_output_tokens
    }

    /// Base URL the Messages adapter is configured with.
    #[must_use]
    pub const fn base_url(&self) -> &'static str {
        DEEPSEEK_ANTHROPIC_MESSAGES_BASE_URL
    }

    /// Provider identity for this route, declaring Anthropic Messages.
    #[must_use]
    pub fn provider_descriptor(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    /// Route-exact identity used by the strict Messages adapter.
    ///
    /// The provider/catalog descriptor remains multi-dialect because DeepSeek
    /// publishes both Chat Completions and Anthropic Messages. One selected
    /// [`InferenceAdapter`] must advertise exactly the protocol it resolves.
    #[must_use]
    pub fn inference_descriptor(&self) -> ProviderDescriptor {
        inference_descriptor()
    }

    /// Safe identity/default metadata for setup and routing discovery.
    #[must_use]
    pub fn provider_profile(&self) -> ProviderProfile {
        ProviderProfile {
            registry_name: crate::DEEPSEEK_PROVIDER.to_owned(),
            descriptor: self.provider_descriptor(),
            default_model: DEEPSEEK_V4_FLASH.to_owned(),
            credential_reference: Some(crate::DEEPSEEK_API_KEY_REFERENCE.to_owned()),
        }
    }

    /// Messages adapter configuration for this route.
    ///
    /// What this deliberately does *not* configure is as much of the profile
    /// as what it does:
    ///
    /// * **No server tools.** DeepSeek marks the `server_tool_use` and
    ///   `web_search_tool_result` response blocks Supported but publishes no
    ///   versioned request `type` for them, so none is invented.
    /// * **No `anthropic-beta` header.** The header table marks it "Ignored
    ///   for /messages".
    /// * **`anthropic-version` is left at the adapter's `2023-06-01`.**
    ///   DeepSeek ignores it, so sending it costs nothing and keeps the route
    ///   correct if that ever changes.
    /// * **No `cache_control` anywhere.** Every `cache_control` row reads
    ///   "Ignored", so prompt caching is not addressable on this route.
    ///
    /// # Errors
    /// [`DeepSeekAnthropicError::Route`] if a canonical reasoning id fails
    /// validation.
    ///
    /// Production composition should prefer
    /// [`Self::messages_config_with_credential`] so key rotation reaches the
    /// next operation without rebuilding the adapter.
    pub fn messages_config(
        &self,
        api_key: impl Into<String>,
    ) -> Result<AnthropicMessagesConfig, DeepSeekAnthropicError> {
        self.messages_config_with_credential(RouteCredential::fixed(api_key))
    }

    /// Messages adapter configuration with an operation-time credential.
    ///
    /// This is the production-shaped counterpart to [`Self::messages_config`]:
    /// the caller supplies a route-bound credential handle and the shared
    /// adapter acquires its value once when each operation starts.
    ///
    /// # Errors
    /// [`DeepSeekAnthropicError::Route`] if a canonical reasoning id fails
    /// validation.
    pub fn messages_config_with_credential(
        &self,
        credential: RouteCredential,
    ) -> Result<AnthropicMessagesConfig, DeepSeekAnthropicError> {
        let mut choices = Vec::with_capacity(DeepSeekAnthropicEffort::ALL.len());
        for effort in DeepSeekAnthropicEffort::ALL {
            choices.push((effort.reasoning_effort_id()?, effort.thinking_mode()));
        }
        Ok(AnthropicMessagesConfig::with_credential(
            self.inference_descriptor(),
            self.base_url(),
            credential,
        )
        .with_auth_wire(self.auth_wire)
        .with_thinking(
            choices,
            Some(DeepSeekAnthropicEffort::DEFAULT.reasoning_effort_id()?),
        )
        .with_default_max_output_tokens(Some(self.max_output_tokens)))
    }
}

/// Provider-owned guard around the shared Anthropic Messages protocol.
///
/// DeepSeek answers unrecognized model ids with `deepseek-v4-flash` instead
/// of rejecting them, and documented Claude aliases also select a different
/// canonical id. The shared Messages adapter cannot know that provider rule.
/// This wrapper refuses both cases before producing a [`ResolvedCall`], so the
/// exact durable route always names the model DeepSeek was asked to serve.
pub struct DeepSeekAnthropicAdapter {
    inner: AnthropicMessagesAdapter,
}

impl DeepSeekAnthropicAdapter {
    /// Build the guarded route with a fixed key for embedding or tests.
    ///
    /// # Errors
    /// Invalid provider-owned profile or shared Messages configuration.
    pub fn with_key(
        profile: DeepSeekAnthropicProfile,
        api_key: impl Into<String>,
        http: HttpService,
    ) -> Result<Self, DeepSeekAnthropicError> {
        Self::with_credential(profile, RouteCredential::fixed(api_key), http)
    }

    /// Build the guarded route with an operation-time credential handle.
    ///
    /// # Errors
    /// Invalid provider-owned profile or shared Messages configuration.
    pub fn with_credential(
        profile: DeepSeekAnthropicProfile,
        credential: RouteCredential,
        http: HttpService,
    ) -> Result<Self, DeepSeekAnthropicError> {
        let config = profile.messages_config_with_credential(credential)?;
        let inner = AnthropicMessagesAdapter::new(config, http).map_err(|error| {
            DeepSeekAnthropicError::Route {
                message: error.to_string(),
            }
        })?;
        Ok(Self { inner })
    }
}

impl Provider for DeepSeekAnthropicAdapter {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: crate::DEEPSEEK_PROVIDER.to_owned(),
            default_model: DEEPSEEK_V4_FLASH.to_owned(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(crate::DEEPSEEK_API_KEY_REFERENCE)
    }

    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(LlmError::InvalidResponse(
                "DeepSeek Anthropic route dispatches through its exact inference adapter; the legacy chat path is unavailable"
                    .to_owned(),
            ))
        }))
    }
}

impl InferenceAdapter for DeepSeekAnthropicAdapter {
    fn descriptor(&self) -> ProviderDescriptor {
        self.inner.descriptor()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.inner.authentication_binding()
    }

    fn reasoning_effort_options(
        &self,
        model: &heycode_llm::ModelDescriptor,
    ) -> Result<Option<heycode_llm::ReasoningEffortOptions>, ResolveError> {
        self.inner.reasoning_effort_options(model)
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &heycode_llm::ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        let served = admit_model(&draft.model).map_err(|_| ResolveError::InvalidRequest {
            field: "model",
            message: "unrecognized DeepSeek Anthropic model would be silently replaced".to_owned(),
        })?;
        if served != draft.model {
            return Err(ResolveError::InvalidRequest {
                field: "model",
                message: "mapped model aliases must be canonicalized before exact dispatch"
                    .to_owned(),
            });
        }
        self.inner.resolve(draft, model)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.inner.stream(call)
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        self.inner.stream_cancellable(call, cancellation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undocumented_field_projects_to_unknown_and_never_to_supported() {
        for field in DeepSeekAnthropicField::ALL {
            if matches!(
                field.documented_support(),
                DeepSeekAnthropicSupport::IntegrationInferred
                    | DeepSeekAnthropicSupport::Undocumented
            ) {
                assert_eq!(field.capability(), CapabilitySupport::Unknown);
            } else {
                assert_ne!(field.capability(), CapabilitySupport::Unknown);
            }
        }
    }

    #[test]
    fn an_ignored_field_is_reported_unsupported_rather_than_supported() {
        assert_eq!(
            DeepSeekAnthropicField::ToolCacheControl.capability(),
            CapabilitySupport::Unsupported
        );
        assert_eq!(
            DeepSeekAnthropicField::DisableParallelToolUse.capability(),
            CapabilitySupport::Unsupported
        );
    }
}

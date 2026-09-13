//! Provider-owned OpenAI prompt-cache controls and detailed usage facts.
//!
//! Ordinary normalized usage keeps total prompt/completion counters, while the
//! shared neutral response-metadata plane now durably carries the detailed
//! cache/reasoning facts parsed here. The provider-owned Settings policy below
//! decides whether cache controls are added to a request; its default is off.
//!
//! Primary sources:
//! <https://developers.openai.com/api/reference/cli/resources/responses/methods/create>
//! and <https://developers.openai.com/api/docs/guides/latest-model>.

use heycode_core::ProviderRequestOption;
use heycode_llm::CapabilitySupport;
use heycode_settings::{
    SettingsApplies, SettingsDefinition, SettingsError, SettingsFieldPath, SettingsNamespace,
    SettingsService, SettingsSnapshot,
};

use crate::OPENAI_GPT_5_6_SOL;
use crate::inference::OpenAiProvider;

const PROMPT_CACHE_OPTION_KIND: &str = "prompt-cache";
const PROMPT_CACHE_SETTINGS_NAMESPACE: &str = "openai-prompt-cache";

/// Prompt-cache selection mode for the current Responses dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiPromptCacheMode {
    /// Keep OpenAI's one automatically selected breakpoint.
    Implicit,
    /// Disable the implicit breakpoint and use only explicit content markers.
    Explicit,
}

impl OpenAiPromptCacheMode {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Implicit => "implicit",
            Self::Explicit => "explicit",
        }
    }

    fn from_wire_name(value: &str) -> Result<Self, OpenAiPromptCachePolicyFault> {
        match value {
            "implicit" => Ok(Self::Implicit),
            "explicit" => Ok(Self::Explicit),
            _ => Err(OpenAiPromptCachePolicyFault::InvalidMode),
        }
    }
}

/// Exact capability evidence for current prompt-cache controls.
#[must_use]
pub fn openai_prompt_cache_support(model: &str) -> CapabilitySupport {
    if model == OPENAI_GPT_5_6_SOL {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unknown
    }
}

/// Validated top-level Responses prompt-cache fields.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiPromptCacheControl {
    mode: OpenAiPromptCacheMode,
    wire: serde_json::Value,
}

impl OpenAiPromptCacheControl {
    /// Build a current 30-minute prompt-cache policy for one stable key.
    ///
    /// # Errors
    /// Empty, oversized or non-identifier keys are refused without echoing.
    pub fn new(
        prompt_cache_key: impl AsRef<str>,
        mode: OpenAiPromptCacheMode,
    ) -> Result<Self, OpenAiPromptCacheFault> {
        let key = prompt_cache_key.as_ref();
        if !safe_cache_key(key) {
            return Err(OpenAiPromptCacheFault::InvalidConfiguration);
        }
        Ok(Self {
            mode,
            wire: serde_json::json!({
                "prompt_cache_key":key,
                "prompt_cache_options":{"mode":mode.wire_name(),"ttl":"30m"}
            }),
        })
    }

    /// Configured implicit/explicit mode.
    #[must_use]
    pub const fn mode(&self) -> OpenAiPromptCacheMode {
        self.mode
    }

    /// Exact top-level request fields after model capability admission.
    ///
    /// # Errors
    /// Unsupported and unproven capability are refused distinctly.
    pub fn wire_for(&self, model: &str) -> Result<&serde_json::Value, OpenAiPromptCacheFault> {
        match openai_prompt_cache_support(model) {
            CapabilitySupport::Supported => Ok(&self.wire),
            CapabilitySupport::Unsupported => Err(OpenAiPromptCacheFault::UnsupportedCapability),
            CapabilitySupport::Unknown => Err(OpenAiPromptCacheFault::UnprovenCapability),
        }
    }

    /// Project the exact control into the durable provider-option plane.
    ///
    /// The option remains secret-free but its data is redacted from `Debug`.
    /// A Responses wire Consumer must map both top-level members together;
    /// silently dropping either member is not permitted.
    ///
    /// # Errors
    /// Unsupported/unproven model evidence or provider metadata that no
    /// longer satisfies the shared option contract fails closed.
    pub fn provider_option(
        &self,
        model: &str,
    ) -> Result<ProviderRequestOption, OpenAiPromptCacheFault> {
        self.wire_for(model)?;
        ProviderRequestOption::new("openai", PROMPT_CACHE_OPTION_KIND, self.wire.clone())
            .map_err(|_| OpenAiPromptCacheFault::InvalidConfiguration)
    }
}

impl std::fmt::Debug for OpenAiPromptCacheControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiPromptCacheControl")
            .field("mode", &self.mode)
            .field("prompt_cache_key", &"[REDACTED]")
            .finish()
    }
}

/// Whether one response read and/or wrote prompt-cache tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiCacheActivity {
    /// No cache read or write was reported.
    None,
    /// Existing cached input was read.
    Read,
    /// New cached input was written.
    Write,
    /// The response both read and wrote cache regions.
    ReadAndWrite,
}

/// Lossless current Responses token counters with cache components separated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiCacheUsage {
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
}

impl OpenAiCacheUsage {
    /// Parse one exact Responses `usage` object.
    ///
    /// # Errors
    /// Missing/non-integer components, impossible subtotals or overflow are
    /// refused without retaining provider text.
    pub fn from_usage(usage: &serde_json::Value) -> Result<Self, OpenAiPromptCacheFault> {
        let usage = usage
            .as_object()
            .ok_or(OpenAiPromptCacheFault::InvalidUsage)?;
        let input_tokens = required_u64(usage, "input_tokens")?;
        let input_details = usage
            .get("input_tokens_details")
            .and_then(serde_json::Value::as_object)
            .ok_or(OpenAiPromptCacheFault::InvalidUsage)?;
        let cache_read_tokens = required_u64(input_details, "cached_tokens")?;
        let cache_write_tokens = required_u64(input_details, "cache_write_tokens")?;
        let output_tokens = required_u64(usage, "output_tokens")?;
        let output_details = usage
            .get("output_tokens_details")
            .and_then(serde_json::Value::as_object)
            .ok_or(OpenAiPromptCacheFault::InvalidUsage)?;
        let reasoning_tokens = required_u64(output_details, "reasoning_tokens")?;
        let total_tokens = required_u64(usage, "total_tokens")?;
        if input_tokens.checked_add(output_tokens) != Some(total_tokens)
            || cache_read_tokens > input_tokens
            || cache_write_tokens > input_tokens
            || reasoning_tokens > output_tokens
        {
            return Err(OpenAiPromptCacheFault::InvalidUsage);
        }
        Ok(Self {
            input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            output_tokens,
            reasoning_tokens,
            total_tokens,
        })
    }

    /// All provider-reported input tokens.
    #[must_use]
    pub const fn input_tokens(self) -> u64 {
        self.input_tokens
    }

    /// Input tokens read from an existing cache entry.
    #[must_use]
    pub const fn cache_read_tokens(self) -> u64 {
        self.cache_read_tokens
    }

    /// Input tokens written into the cache.
    #[must_use]
    pub const fn cache_write_tokens(self) -> u64 {
        self.cache_write_tokens
    }

    /// Generated output tokens.
    #[must_use]
    pub const fn output_tokens(self) -> u64 {
        self.output_tokens
    }

    /// Reasoning subset of output tokens.
    #[must_use]
    pub const fn reasoning_tokens(self) -> u64 {
        self.reasoning_tokens
    }

    /// Provider-reported input plus output total.
    #[must_use]
    pub const fn total_tokens(self) -> u64 {
        self.total_tokens
    }

    /// Visible cache activity classification.
    #[must_use]
    pub const fn activity(self) -> OpenAiCacheActivity {
        match (self.cache_read_tokens > 0, self.cache_write_tokens > 0) {
            (false, false) => OpenAiCacheActivity::None,
            (true, false) => OpenAiCacheActivity::Read,
            (false, true) => OpenAiCacheActivity::Write,
            (true, true) => OpenAiCacheActivity::ReadAndWrite,
        }
    }
}

/// Closed prompt-cache refusal without cache keys or provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenAiPromptCacheFault {
    /// Cache key/options are invalid.
    InvalidConfiguration,
    /// Exact evidence says the selected model lacks the capability.
    UnsupportedCapability,
    /// Exact capability evidence is absent.
    UnprovenCapability,
    /// Provider usage counters are missing or inconsistent.
    InvalidUsage,
}

impl std::fmt::Display for OpenAiPromptCacheFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "OpenAI prompt-cache configuration is invalid",
            Self::UnsupportedCapability => "OpenAI prompt caching is unsupported by this model",
            Self::UnprovenCapability => "OpenAI prompt-cache capability is unproven",
            Self::InvalidUsage => "OpenAI prompt-cache usage is invalid",
        })
    }
}

impl std::error::Error for OpenAiPromptCacheFault {}

/// Restart-resolved OpenAI prompt-cache policy.
///
/// Disabled carries no cache control, so applying the default cannot change a
/// request. Enabled owns one already-validated control whose key remains
/// redacted from diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiPromptCachePolicy {
    /// Do not add prompt-cache request fields.
    Disabled,
    /// Add one exact implicit/explicit cache control to normal Responses calls.
    Enabled(OpenAiPromptCacheControl),
}

impl OpenAiPromptCachePolicy {
    fn from_value(value: &serde_json::Value) -> Result<Self, OpenAiPromptCachePolicyFault> {
        let object = value
            .as_object()
            .ok_or(OpenAiPromptCachePolicyFault::InvalidShape)?;
        if object.len() != 3
            || !object.contains_key("enabled")
            || !object.contains_key("prompt_cache_key")
            || !object.contains_key("mode")
        {
            return Err(OpenAiPromptCachePolicyFault::InvalidShape);
        }
        let enabled = object
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .ok_or(OpenAiPromptCachePolicyFault::InvalidEnabled)?;
        let key = object
            .get("prompt_cache_key")
            .and_then(serde_json::Value::as_str)
            .ok_or(OpenAiPromptCachePolicyFault::InvalidCacheKey)?;
        let mode = OpenAiPromptCacheMode::from_wire_name(
            object
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .ok_or(OpenAiPromptCachePolicyFault::InvalidMode)?,
        )?;
        if (!key.is_empty() && !safe_cache_key(key))
            || heycode_settings::screen_text_for_credentials(key).is_err()
            || (enabled && key.is_empty())
        {
            return Err(OpenAiPromptCachePolicyFault::InvalidCacheKey);
        }
        if enabled {
            OpenAiPromptCacheControl::new(key, mode)
                .map(Self::Enabled)
                .map_err(|_| OpenAiPromptCachePolicyFault::InvalidCacheKey)
        } else {
            Ok(Self::Disabled)
        }
    }

    /// Parse one snapshot owned by the restart-applied OpenAI namespace.
    ///
    /// # Errors
    /// Wrong namespace/application timing or an invalid resolved value fails
    /// without exposing the cache key.
    pub fn from_snapshot(
        snapshot: &SettingsSnapshot,
    ) -> Result<Self, OpenAiPromptCachePolicyFault> {
        if snapshot.namespace().as_str() != PROMPT_CACHE_SETTINGS_NAMESPACE
            || snapshot.applies() != SettingsApplies::Restart
            || !snapshot.wire_exposed()
        {
            return Err(OpenAiPromptCachePolicyFault::InvalidSnapshot);
        }
        Self::from_value(snapshot.resolved())
    }

    /// Apply this resolved restart policy to one constructed OpenAI provider.
    ///
    /// # Errors
    /// An enabled policy for a model without exact prompt-cache evidence, or
    /// provider-option construction failure, prevents provider publication.
    pub fn apply_to(
        self,
        provider: OpenAiProvider,
    ) -> Result<OpenAiProvider, heycode_llm::LlmError> {
        match self {
            Self::Disabled => Ok(provider),
            Self::Enabled(control) => provider.with_prompt_caching(control),
        }
    }
}

/// Closed prompt-cache Settings failure without key or document values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenAiPromptCachePolicyFault {
    /// Top-level object fields are missing, extra, or not an object.
    InvalidShape,
    /// Enabled state is not boolean.
    InvalidEnabled,
    /// Cache mode is not `implicit` or `explicit`.
    InvalidMode,
    /// Cache key is absent, unsafe, credential-shaped, or missing while enabled.
    InvalidCacheKey,
    /// Snapshot does not belong to the restart-applied OpenAI namespace.
    InvalidSnapshot,
    /// The Settings registry/namespace is unavailable.
    SettingsUnavailable,
}

impl std::fmt::Display for OpenAiPromptCachePolicyFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidShape => "OpenAI prompt-cache settings shape is invalid",
            Self::InvalidEnabled => "OpenAI prompt-cache enabled state is invalid",
            Self::InvalidMode => "OpenAI prompt-cache mode is invalid",
            Self::InvalidCacheKey => "OpenAI prompt-cache key is invalid",
            Self::InvalidSnapshot => "OpenAI prompt-cache settings snapshot is invalid",
            Self::SettingsUnavailable => "OpenAI prompt-cache settings are unavailable",
        })
    }
}

impl std::error::Error for OpenAiPromptCachePolicyFault {}

/// Provider-owned Settings namespace.
///
/// # Errors
/// Static namespace validation failure.
pub fn openai_prompt_cache_settings_namespace() -> Result<SettingsNamespace, SettingsError> {
    SettingsNamespace::new(PROMPT_CACHE_SETTINGS_NAMESPACE)
}

/// Restart-applied, wire-exposed prompt-cache Settings definition.
///
/// # Errors
/// Static schema, field-path, defaults, or namespace validation failure.
pub fn openai_prompt_cache_settings_definition() -> Result<SettingsDefinition, SettingsError> {
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["enabled","prompt_cache_key","mode"],
            "properties":{
                "enabled":{"type":"boolean"},
                "prompt_cache_key":{
                    "type":"string",
                    "maxLength":64,
                    "pattern":"^[A-Za-z0-9_.:/-]*$"
                },
                "mode":{"type":"string","enum":["implicit","explicit"]}
            }
        }),
        serde_json::json!({
            "enabled":false,
            "prompt_cache_key":"",
            "mode":"implicit"
        }),
        |value| {
            OpenAiPromptCachePolicy::from_value(value)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?
    .with_public_path(SettingsFieldPath::new("prompt_cache_key")?)
    .with_wire_exposure();
    Ok(
        SettingsDefinition::new(openai_prompt_cache_settings_namespace()?, schema)
            .with_applies(SettingsApplies::Restart),
    )
}

/// Resolve the registered restart policy for root provider construction.
///
/// # Errors
/// Missing/unavailable Settings state or invalid resolved policy fails closed.
pub fn resolve_openai_prompt_cache_policy(
    settings: &SettingsService,
) -> Result<OpenAiPromptCachePolicy, OpenAiPromptCachePolicyFault> {
    let namespace = openai_prompt_cache_settings_namespace()
        .map_err(|_| OpenAiPromptCachePolicyFault::SettingsUnavailable)?;
    let snapshot = settings
        .get(&namespace)
        .map_err(|_| OpenAiPromptCachePolicyFault::SettingsUnavailable)?
        .ok_or(OpenAiPromptCachePolicyFault::SettingsUnavailable)?;
    OpenAiPromptCachePolicy::from_snapshot(&snapshot)
}

fn safe_cache_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes().iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u64, OpenAiPromptCacheFault> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or(OpenAiPromptCacheFault::InvalidUsage)
}

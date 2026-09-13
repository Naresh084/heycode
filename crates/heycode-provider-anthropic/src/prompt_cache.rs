//! Provider-owned Anthropic automatic prompt-cache policy and usage facts.
//!
//! The request policy is secret-free and becomes a durable provider option.
//! Detailed cache counters remain separate from the shared prompt/completion
//! total so a cache read, write, and uncached suffix never collapse together.
//!
//! Primary source:
//! <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>.

use heycode_core::ProviderRequestOption;

/// Durable provider-option kind for automatic prompt caching.
pub const ANTHROPIC_PROMPT_CACHE_OPTION_KIND: &str = "prompt-cache";

/// Automatic cache breakpoint lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicPromptCacheTtl {
    /// Five-minute cache entry, the provider default.
    FiveMinutes,
    /// One-hour cache entry.
    OneHour,
}

impl AnthropicPromptCacheTtl {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::OneHour => "1h",
        }
    }
}

/// Exact top-level automatic prompt-cache policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicPromptCachePolicy {
    ttl: AnthropicPromptCacheTtl,
    request_fields: serde_json::Value,
}

impl AnthropicPromptCachePolicy {
    /// Cache through the last eligible prompt block using one explicit TTL.
    #[must_use]
    pub fn automatic(ttl: AnthropicPromptCacheTtl) -> Self {
        Self {
            ttl,
            request_fields: serde_json::json!({
                "cache_control":{"type":"ephemeral","ttl":ttl.wire_name()}
            }),
        }
    }

    /// Selected cache lifetime.
    #[must_use]
    pub const fn ttl(&self) -> AnthropicPromptCacheTtl {
        self.ttl
    }

    /// Exact top-level request fields.
    #[must_use]
    pub const fn request_fields(&self) -> &serde_json::Value {
        &self.request_fields
    }

    /// Project the policy into the durable provider-option plane.
    ///
    /// # Errors
    /// The fixed provider/kind and bounded policy should remain valid; a
    /// shared provider-option contract change is returned as a closed fault.
    pub fn provider_option(&self) -> Result<ProviderRequestOption, AnthropicPromptCacheFault> {
        ProviderRequestOption::new(
            "anthropic",
            ANTHROPIC_PROMPT_CACHE_OPTION_KIND,
            self.request_fields.clone(),
        )
        .map_err(|_| AnthropicPromptCacheFault::InvalidConfiguration)
    }
}

/// Provider-observed cache activity for one successful response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicCacheActivity {
    /// No cache read or write was reported.
    None,
    /// A new prefix was written.
    Write,
    /// An existing prefix was read.
    Read,
    /// The request both read an old prefix and wrote a new suffix.
    ReadAndWrite,
}

/// Detailed current Messages cache counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnthropicCacheUsage {
    uncached_input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_5m_input_tokens: Option<u64>,
    cache_creation_1h_input_tokens: Option<u64>,
    total_input_tokens: u64,
    output_tokens: u64,
}

impl AnthropicCacheUsage {
    /// Parse one exact response `usage` object.
    ///
    /// # Errors
    /// Missing/non-integer components, inconsistent cache-write subtotals, or
    /// arithmetic overflow are refused without retaining provider text.
    pub fn from_usage(usage: &serde_json::Value) -> Result<Self, AnthropicPromptCacheFault> {
        let usage = usage
            .as_object()
            .ok_or(AnthropicPromptCacheFault::InvalidUsage)?;
        let uncached_input_tokens = required_u64(usage, "input_tokens")?;
        let cache_creation_input_tokens = required_u64(usage, "cache_creation_input_tokens")?;
        let cache_read_input_tokens = required_u64(usage, "cache_read_input_tokens")?;
        let output_tokens = required_u64(usage, "output_tokens")?;
        let total_input_tokens = uncached_input_tokens
            .checked_add(cache_creation_input_tokens)
            .and_then(|total| total.checked_add(cache_read_input_tokens))
            .ok_or(AnthropicPromptCacheFault::InvalidUsage)?;
        let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
            match usage.get("cache_creation") {
                Some(value) => {
                    let details = value
                        .as_object()
                        .ok_or(AnthropicPromptCacheFault::InvalidUsage)?;
                    let five = required_u64(details, "ephemeral_5m_input_tokens")?;
                    let hour = required_u64(details, "ephemeral_1h_input_tokens")?;
                    if five.checked_add(hour) != Some(cache_creation_input_tokens) {
                        return Err(AnthropicPromptCacheFault::InvalidUsage);
                    }
                    (Some(five), Some(hour))
                }
                None => (None, None),
            };
        Ok(Self {
            uncached_input_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
            total_input_tokens,
            output_tokens,
        })
    }

    /// Input tokens after the final cache breakpoint.
    #[must_use]
    pub const fn uncached_input_tokens(self) -> u64 {
        self.uncached_input_tokens
    }

    /// Tokens written into a cache entry.
    #[must_use]
    pub const fn cache_creation_input_tokens(self) -> u64 {
        self.cache_creation_input_tokens
    }

    /// Tokens read from an existing cache entry.
    #[must_use]
    pub const fn cache_read_input_tokens(self) -> u64 {
        self.cache_read_input_tokens
    }

    /// Five-minute subset of cache-write tokens when the provider reports it.
    #[must_use]
    pub const fn cache_creation_5m_input_tokens(self) -> Option<u64> {
        self.cache_creation_5m_input_tokens
    }

    /// One-hour subset of cache-write tokens when the provider reports it.
    #[must_use]
    pub const fn cache_creation_1h_input_tokens(self) -> Option<u64> {
        self.cache_creation_1h_input_tokens
    }

    /// Checked total input processed across uncached, write, and read regions.
    #[must_use]
    pub const fn total_input_tokens(self) -> u64 {
        self.total_input_tokens
    }

    /// Generated output tokens.
    #[must_use]
    pub const fn output_tokens(self) -> u64 {
        self.output_tokens
    }

    /// Visible cache activity.
    #[must_use]
    pub const fn activity(self) -> AnthropicCacheActivity {
        match (
            self.cache_creation_input_tokens > 0,
            self.cache_read_input_tokens > 0,
        ) {
            (false, false) => AnthropicCacheActivity::None,
            (true, false) => AnthropicCacheActivity::Write,
            (false, true) => AnthropicCacheActivity::Read,
            (true, true) => AnthropicCacheActivity::ReadAndWrite,
        }
    }
}

/// Closed prompt-cache refusal without request or provider content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnthropicPromptCacheFault {
    /// Request policy cannot enter the durable/wire plane.
    InvalidConfiguration,
    /// Response usage counters are missing or inconsistent.
    InvalidUsage,
}

impl std::fmt::Display for AnthropicPromptCacheFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "Anthropic prompt-cache configuration is invalid",
            Self::InvalidUsage => "Anthropic prompt-cache usage is invalid",
        })
    }
}

impl std::error::Error for AnthropicPromptCacheFault {}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u64, AnthropicPromptCacheFault> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or(AnthropicPromptCacheFault::InvalidUsage)
}

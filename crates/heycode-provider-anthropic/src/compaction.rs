//! Provider-owned Anthropic server-compaction request and checkpoint facts.
//!
//! A compaction block is continuation state, not a summary for heycode to parse.
//! This module keeps the complete assistant item unchanged and exposes only its
//! location. Provider content never enters diagnostics.
//!
//! Primary source:
//! <https://platform.claude.com/docs/en/build-with-claude/compaction>.

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind};
use heycode_llm::CapabilitySupport;

use crate::ANTHROPIC_CLAUDE_OPUS_5;

/// Beta value required for server-side compaction.
pub const ANTHROPIC_COMPACTION_BETA: &str = "compact-2026-01-12";

/// Current official per-model server-compaction evidence.
#[must_use]
pub fn anthropic_compaction_support(model: &str) -> CapabilitySupport {
    if matches!(
        model,
        ANTHROPIC_CLAUDE_OPUS_5
            | "claude-fable-5"
            | "claude-mythos-5"
            | "claude-mythos-preview"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
    ) {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unknown
    }
}

/// Exact `compact_20260112` request definition.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicCompactionDefinition {
    trigger_tokens: Option<u64>,
    pause_after_compaction: bool,
}

impl AnthropicCompactionDefinition {
    /// Build a compaction edit.
    ///
    /// `None` uses Anthropic's current 150,000-token default. An explicit
    /// trigger must meet the documented 50,000-token minimum.
    ///
    /// # Errors
    /// A trigger below 50,000 is refused.
    pub fn new(
        trigger_tokens: Option<u64>,
        pause_after_compaction: bool,
    ) -> Result<Self, AnthropicCompactionFault> {
        if trigger_tokens.is_some_and(|tokens| tokens < 50_000) {
            return Err(AnthropicCompactionFault::InvalidConfiguration);
        }
        Ok(Self {
            trigger_tokens,
            pause_after_compaction,
        })
    }

    /// Required beta values, without the header name.
    #[must_use]
    pub const fn beta_headers(&self) -> &[&'static str] {
        &[ANTHROPIC_COMPACTION_BETA]
    }

    /// Whether the provider should stop immediately after emitting the
    /// compaction checkpoint.
    #[must_use]
    pub const fn pause_after_compaction(&self) -> bool {
        self.pause_after_compaction
    }

    /// Exact top-level request extension after capability admission.
    ///
    /// # Errors
    /// Unsupported and unknown model evidence fail distinctly.
    pub fn request_fields_for(
        &self,
        model: &str,
    ) -> Result<serde_json::Value, AnthropicCompactionFault> {
        match anthropic_compaction_support(model) {
            CapabilitySupport::Supported => {}
            CapabilitySupport::Unsupported => {
                return Err(AnthropicCompactionFault::UnsupportedCapability);
            }
            CapabilitySupport::Unknown => {
                return Err(AnthropicCompactionFault::UnprovenCapability);
            }
        }
        let mut edit = serde_json::json!({
            "type":"compact_20260112",
            "pause_after_compaction":self.pause_after_compaction
        });
        if let Some(tokens) = self.trigger_tokens {
            edit["trigger"] = serde_json::json!({"type":"input_tokens","value":tokens});
        }
        Ok(serde_json::json!({"context_management":{"edits":[edit]}}))
    }
}

impl std::fmt::Debug for AnthropicCompactionDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicCompactionDefinition")
            .field("trigger_tokens", &self.trigger_tokens)
            .field("pause_after_compaction", &self.pause_after_compaction)
            .finish()
    }
}

/// Validated complete assistant state containing one opaque compaction block.
#[derive(Clone, PartialEq)]
pub struct AnthropicCompactionCheckpoint {
    state: ProviderStateItem,
    compaction_index: usize,
}

impl AnthropicCompactionCheckpoint {
    /// Admit exact Messages provider state for durable continuation.
    ///
    /// # Errors
    /// Capability, route, block count or empty compaction content failures are
    /// returned as closed safe faults.
    pub fn from_state(
        model: &str,
        state: ProviderStateItem,
    ) -> Result<Self, AnthropicCompactionFault> {
        match anthropic_compaction_support(model) {
            CapabilitySupport::Supported => {}
            CapabilitySupport::Unsupported => {
                return Err(AnthropicCompactionFault::UnsupportedCapability);
            }
            CapabilitySupport::Unknown => {
                return Err(AnthropicCompactionFault::UnprovenCapability);
            }
        }
        state
            .validate()
            .map_err(|_| AnthropicCompactionFault::InvalidState)?;
        if state.provider() != "anthropic"
            || state.model() != model
            || state.protocol() != ProviderProtocol::AnthropicMessages
            || state.kind() != ProviderStateKind::AnthropicMessage
        {
            return Err(AnthropicCompactionFault::WrongRoute);
        }
        let content = state
            .data()
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or(AnthropicCompactionFault::InvalidState)?;
        let mut indices = content.iter().enumerate().filter_map(|(index, block)| {
            (block.get("type").and_then(serde_json::Value::as_str) == Some("compaction"))
                .then_some(index)
        });
        let compaction_index = indices
            .next()
            .ok_or(AnthropicCompactionFault::InvalidState)?;
        if indices.next().is_some()
            || content[compaction_index]
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(AnthropicCompactionFault::InvalidState);
        }
        Ok(Self {
            state,
            compaction_index,
        })
    }

    /// Complete unchanged assistant provider state.
    #[must_use]
    pub const fn state(&self) -> &ProviderStateItem {
        &self.state
    }

    /// Exact opaque compaction block inside [`Self::state`].
    #[must_use]
    pub fn compaction(&self) -> &serde_json::Value {
        &self.state.data()["content"][self.compaction_index]
    }
}

impl std::fmt::Debug for AnthropicCompactionCheckpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicCompactionCheckpoint")
            .field("compaction_index", &self.compaction_index)
            .finish_non_exhaustive()
    }
}

/// Closed server-compaction fault without request or provider content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnthropicCompactionFault {
    /// Trigger or definition is invalid.
    InvalidConfiguration,
    /// Exact documentation excludes the selected model.
    UnsupportedCapability,
    /// Exact capability evidence is absent.
    UnprovenCapability,
    /// Provider/model/protocol/state-kind identity is wrong.
    WrongRoute,
    /// Compaction state is missing, ambiguous or malformed.
    InvalidState,
}

impl std::fmt::Display for AnthropicCompactionFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "Anthropic compaction configuration is invalid",
            Self::UnsupportedCapability => "Anthropic compaction is unsupported by this model",
            Self::UnprovenCapability => "Anthropic compaction capability is unproven",
            Self::WrongRoute => "Anthropic compaction state route is invalid",
            Self::InvalidState => "Anthropic compaction state is invalid",
        })
    }
}

impl std::error::Error for AnthropicCompactionFault {}

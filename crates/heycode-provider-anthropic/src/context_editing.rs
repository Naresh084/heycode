//! Provider-owned Anthropic context-editing definitions and applied facts.
//!
//! Context editing is applied server-side while the client retains its full
//! unmodified history. Request policy and response metadata are therefore
//! separate values: this module never rewrites a [`heycode_core::ProviderStateItem`].
//!
//! Primary source:
//! <https://platform.claude.com/docs/en/build-with-claude/context-editing>.

use std::collections::BTreeSet;

use heycode_core::ProviderRequestOption;
use heycode_llm::CapabilitySupport;

use crate::ANTHROPIC_CLAUDE_OPUS_5;

/// Beta value required for server-side context editing.
pub const ANTHROPIC_CONTEXT_EDITING_BETA: &str = "context-management-2025-06-27";
/// Durable provider-option kind for context-editing policy.
pub const ANTHROPIC_CONTEXT_EDITING_OPTION_KIND: &str = "context-editing";

/// Conservative current model evidence for the Claude API context-editing
/// route.
///
/// The primary page says all supported Claude models and directly examples
/// Opus 5. Maintained current ids are admitted; an unlisted id remains Unknown
/// rather than inheriting support from a name prefix.
#[must_use]
pub fn anthropic_context_editing_support(model: &str) -> CapabilitySupport {
    if matches!(
        model,
        ANTHROPIC_CLAUDE_OPUS_5
            | "claude-fable-5"
            | "claude-mythos-5"
            | "claude-mythos-preview"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-opus-4-5"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
            | "claude-sonnet-4-5"
            | "claude-haiku-4-5"
    ) {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unknown
    }
}

/// Thinking history retained after a thinking-clearing edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicThinkingKeep {
    /// Preserve all thinking and maximize prompt-cache continuity.
    All,
    /// Preserve thinking from the last positive number of assistant turns.
    Turns(u64),
}

/// Exact `clear_thinking_20251015` edit.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicThinkingClear {
    keep: AnthropicThinkingKeep,
    wire: serde_json::Value,
}

impl AnthropicThinkingClear {
    /// Build an explicit thinking-retention policy.
    ///
    /// # Errors
    /// Keeping zero turns is invalid.
    pub fn new(keep: AnthropicThinkingKeep) -> Result<Self, AnthropicContextEditingFault> {
        let keep_wire = match keep {
            AnthropicThinkingKeep::All => serde_json::json!("all"),
            AnthropicThinkingKeep::Turns(0) => {
                return Err(AnthropicContextEditingFault::InvalidConfiguration);
            }
            AnthropicThinkingKeep::Turns(turns) => {
                serde_json::json!({"type":"thinking_turns","value":turns})
            }
        };
        Ok(Self {
            keep,
            wire: serde_json::json!({
                "type":"clear_thinking_20251015",
                "keep":keep_wire
            }),
        })
    }
}

impl std::fmt::Debug for AnthropicThinkingClear {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicThinkingClear")
            .field("keep", &self.keep)
            .finish()
    }
}

/// Documented trigger dialect for `clear_tool_uses_20250919`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicToolClearTrigger {
    /// Trigger when input context reaches this positive token count.
    InputTokens(u64),
    /// Trigger after this positive number of tool uses.
    ToolUses(u64),
}

impl AnthropicToolClearTrigger {
    fn wire(self) -> Result<serde_json::Value, AnthropicContextEditingFault> {
        match self {
            Self::InputTokens(0) | Self::ToolUses(0) => {
                Err(AnthropicContextEditingFault::InvalidConfiguration)
            }
            Self::InputTokens(value) => {
                Ok(serde_json::json!({"type":"input_tokens","value":value}))
            }
            Self::ToolUses(value) => Ok(serde_json::json!({"type":"tool_uses","value":value})),
        }
    }
}

/// Exact `clear_tool_uses_20250919` edit.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicToolClear {
    trigger: Option<AnthropicToolClearTrigger>,
    keep_tool_uses: Option<u64>,
    clear_at_least_tokens: Option<u64>,
    clear_tool_inputs: bool,
    excluded_tool_count: usize,
    wire: serde_json::Value,
}

impl AnthropicToolClear {
    /// Build a bounded tool-result clearing policy.
    ///
    /// Omitted numeric controls retain Anthropic's defaults. This compatibility
    /// constructor selects the input-token trigger dialect when present; use
    /// [`Self::with_trigger`] for the complete input-token/tool-use choice.
    ///
    /// # Errors
    /// Zero numeric values, duplicate/unsafe tool names or more than 64
    /// exclusions are refused.
    pub fn new(
        trigger_tokens: Option<u64>,
        keep_tool_uses: Option<u64>,
        clear_at_least_tokens: Option<u64>,
        clear_tool_inputs: bool,
        exclude_tools: Vec<String>,
    ) -> Result<Self, AnthropicContextEditingFault> {
        Self::with_trigger(
            trigger_tokens.map(AnthropicToolClearTrigger::InputTokens),
            keep_tool_uses,
            clear_at_least_tokens,
            clear_tool_inputs,
            exclude_tools,
        )
    }

    /// Build a bounded tool-result clearing policy with either documented
    /// trigger dialect.
    ///
    /// # Errors
    /// Zero numeric values, duplicate/unsafe tool names or more than 64
    /// exclusions are refused.
    pub fn with_trigger(
        trigger: Option<AnthropicToolClearTrigger>,
        keep_tool_uses: Option<u64>,
        clear_at_least_tokens: Option<u64>,
        clear_tool_inputs: bool,
        exclude_tools: Vec<String>,
    ) -> Result<Self, AnthropicContextEditingFault> {
        if keep_tool_uses == Some(0)
            || clear_at_least_tokens == Some(0)
            || exclude_tools.len() > 64
            || exclude_tools.iter().any(|name| !safe_tool_name(name))
            || has_duplicates(&exclude_tools)
        {
            return Err(AnthropicContextEditingFault::InvalidConfiguration);
        }
        let mut wire = serde_json::json!({"type":"clear_tool_uses_20250919"});
        if let Some(trigger) = trigger {
            wire["trigger"] = trigger.wire()?;
        }
        if let Some(tool_uses) = keep_tool_uses {
            wire["keep"] = serde_json::json!({"type":"tool_uses","value":tool_uses});
        }
        if let Some(tokens) = clear_at_least_tokens {
            wire["clear_at_least"] = serde_json::json!({"type":"input_tokens","value":tokens});
        }
        if !exclude_tools.is_empty() {
            wire["exclude_tools"] = serde_json::json!(exclude_tools);
        }
        if clear_tool_inputs {
            wire["clear_tool_inputs"] = serde_json::json!(true);
        }
        Ok(Self {
            trigger,
            keep_tool_uses,
            clear_at_least_tokens,
            clear_tool_inputs,
            excluded_tool_count: wire
                .get("exclude_tools")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len),
            wire,
        })
    }
}

impl std::fmt::Debug for AnthropicToolClear {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicToolClear")
            .field("trigger", &self.trigger)
            .field("keep_tool_uses", &self.keep_tool_uses)
            .field("clear_at_least_tokens", &self.clear_at_least_tokens)
            .field("clear_tool_inputs", &self.clear_tool_inputs)
            .field("excluded_tool_count", &self.excluded_tool_count)
            .finish()
    }
}

/// Ordered context-editing request policy.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicContextEditingPolicy {
    has_thinking: bool,
    has_tools: bool,
    request_fields: serde_json::Value,
}

impl AnthropicContextEditingPolicy {
    /// Combine thinking and/or tool clearing in the required provider order.
    ///
    /// # Errors
    /// A policy containing no edit is invalid.
    pub fn new(
        thinking: Option<AnthropicThinkingClear>,
        tools: Option<AnthropicToolClear>,
    ) -> Result<Self, AnthropicContextEditingFault> {
        if thinking.is_none() && tools.is_none() {
            return Err(AnthropicContextEditingFault::InvalidConfiguration);
        }
        let has_thinking = thinking.is_some();
        let has_tools = tools.is_some();
        // The docs require clear_thinking before clear_tool_uses. Taking the
        // two strategies as named slots makes the invalid order unrepresentable.
        let edits = thinking
            .into_iter()
            .map(|definition| definition.wire)
            .chain(tools.into_iter().map(|definition| definition.wire))
            .collect::<Vec<_>>();
        Ok(Self {
            has_thinking,
            has_tools,
            request_fields: serde_json::json!({"context_management":{"edits":edits}}),
        })
    }

    /// Required beta values, without the header name.
    #[must_use]
    pub const fn beta_headers(&self) -> &[&'static str] {
        &[ANTHROPIC_CONTEXT_EDITING_BETA]
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
    pub fn provider_option(&self) -> Result<ProviderRequestOption, AnthropicContextEditingFault> {
        ProviderRequestOption::new(
            "anthropic",
            ANTHROPIC_CONTEXT_EDITING_OPTION_KIND,
            self.request_fields.clone(),
        )
        .map_err(|_| AnthropicContextEditingFault::InvalidConfiguration)
    }
}

impl std::fmt::Debug for AnthropicContextEditingPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicContextEditingPolicy")
            .field("has_thinking", &self.has_thinking)
            .field("has_tools", &self.has_tools)
            .finish()
    }
}

/// Applied server-side edit family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnthropicContextEditKind {
    /// Thinking turns were cleared.
    ClearThinking,
    /// Tool-use/result pairs were cleared.
    ClearToolUses,
}

/// Exact applied-edit object plus safe normalized counters.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicAppliedContextEdit {
    kind: AnthropicContextEditKind,
    cleared_units: u64,
    cleared_input_tokens: u64,
    raw: serde_json::Value,
}

impl AnthropicAppliedContextEdit {
    /// Applied edit family.
    #[must_use]
    pub const fn kind(&self) -> AnthropicContextEditKind {
        self.kind
    }

    /// Cleared thinking turns or tool uses, according to [`Self::kind`].
    #[must_use]
    pub const fn cleared_units(&self) -> u64 {
        self.cleared_units
    }

    /// Provider-reported input tokens cleared by this edit.
    #[must_use]
    pub const fn cleared_input_tokens(&self) -> u64 {
        self.cleared_input_tokens
    }

    /// Complete unchanged applied-edit metadata for durable recording.
    #[must_use]
    pub const fn raw(&self) -> &serde_json::Value {
        &self.raw
    }
}

impl std::fmt::Debug for AnthropicAppliedContextEdit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicAppliedContextEdit")
            .field("kind", &self.kind)
            .field("cleared_units", &self.cleared_units)
            .field("cleared_input_tokens", &self.cleared_input_tokens)
            .finish()
    }
}

/// Observed prompt-cache consequence of a terminal context-edit report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicCacheImpact {
    /// No edit was applied, so this response reports no invalidation point.
    Preserved,
    /// Clearing occurred and invalidated the cached prefix at the edit point.
    InvalidatedAtEdit,
}

/// Validated terminal context-management metadata.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicContextEditReport {
    edits: Vec<AnthropicAppliedContextEdit>,
    total_cleared_input_tokens: u64,
}

impl AnthropicContextEditReport {
    /// Parse `context_management.applied_edits` from a response or terminal
    /// streaming event without retaining other provider fields.
    ///
    /// # Errors
    /// Missing/malformed metadata, unknown/duplicate edit kinds or counter
    /// overflow returns a closed safe fault.
    pub fn from_terminal(
        terminal: &serde_json::Value,
    ) -> Result<Self, AnthropicContextEditingFault> {
        let applied = terminal
            .get("context_management")
            .and_then(|value| value.get("applied_edits"))
            .and_then(serde_json::Value::as_array)
            .ok_or(AnthropicContextEditingFault::InvalidMetadata)?;
        if applied.len() > 2 {
            return Err(AnthropicContextEditingFault::InvalidMetadata);
        }
        let mut seen = BTreeSet::new();
        let mut edits = Vec::with_capacity(applied.len());
        let mut total = 0_u64;
        for raw in applied {
            let object = raw
                .as_object()
                .ok_or(AnthropicContextEditingFault::InvalidMetadata)?;
            let (kind, units_field) = match object.get("type").and_then(serde_json::Value::as_str) {
                Some("clear_thinking_20251015") => (
                    AnthropicContextEditKind::ClearThinking,
                    "cleared_thinking_turns",
                ),
                Some("clear_tool_uses_20250919") => {
                    (AnthropicContextEditKind::ClearToolUses, "cleared_tool_uses")
                }
                _ => return Err(AnthropicContextEditingFault::InvalidMetadata),
            };
            if !seen.insert(kind) {
                return Err(AnthropicContextEditingFault::InvalidMetadata);
            }
            let cleared_units = positive_u64(object, units_field)?;
            let cleared_input_tokens = positive_u64(object, "cleared_input_tokens")?;
            total = total
                .checked_add(cleared_input_tokens)
                .ok_or(AnthropicContextEditingFault::InvalidMetadata)?;
            edits.push(AnthropicAppliedContextEdit {
                kind,
                cleared_units,
                cleared_input_tokens,
                raw: raw.clone(),
            });
        }
        Ok(Self {
            edits,
            total_cleared_input_tokens: total,
        })
    }

    /// Applied edits in provider order.
    #[must_use]
    pub fn edits(&self) -> &[AnthropicAppliedContextEdit] {
        &self.edits
    }

    /// Checked sum of tokens cleared across applied edits.
    #[must_use]
    pub const fn total_cleared_input_tokens(&self) -> u64 {
        self.total_cleared_input_tokens
    }

    /// Observed cache consequence.
    #[must_use]
    pub const fn cache_impact(&self) -> AnthropicCacheImpact {
        if self.edits.is_empty() {
            AnthropicCacheImpact::Preserved
        } else {
            AnthropicCacheImpact::InvalidatedAtEdit
        }
    }
}

impl std::fmt::Debug for AnthropicContextEditReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicContextEditReport")
            .field("edits", &self.edits)
            .field(
                "total_cleared_input_tokens",
                &self.total_cleared_input_tokens,
            )
            .field("cache_impact", &self.cache_impact())
            .finish()
    }
}

/// Closed context-editing refusal without tool names or provider content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnthropicContextEditingFault {
    /// Request policy is invalid.
    InvalidConfiguration,
    /// Terminal applied-edit metadata is invalid.
    InvalidMetadata,
}

impl std::fmt::Display for AnthropicContextEditingFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "Anthropic context-editing configuration is invalid",
            Self::InvalidMetadata => "Anthropic context-editing metadata is invalid",
        })
    }
}

impl std::error::Error for AnthropicContextEditingFault {}

fn safe_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value.as_bytes().iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

fn has_duplicates(values: &[String]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}

fn positive_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u64, AnthropicContextEditingFault> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or(AnthropicContextEditingFault::InvalidMetadata)
}

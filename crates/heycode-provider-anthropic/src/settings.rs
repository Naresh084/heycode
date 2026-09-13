//! Restart-applied opt-in Anthropic cache and context-editing policy.

use heycode_settings::{
    SettingsApplies, SettingsDefinition, SettingsNamespace, SettingsSchema, SettingsService,
    SettingsSnapshot,
};
use serde::Deserialize;

use crate::{
    AnthropicContextEditingPolicy, AnthropicPromptCachePolicy, AnthropicPromptCacheTtl,
    AnthropicProvider, AnthropicThinkingClear, AnthropicThinkingKeep, AnthropicTokenCounterConfig,
    AnthropicToolClear, AnthropicToolClearTrigger,
};

/// Settings namespace owned by `provider-anthropic`.
pub const ANTHROPIC_SETTINGS_NAMESPACE: &str = "anthropic";

const MAX_SETTINGS_QUANTITY: u64 = 9_007_199_254_740_991;
const DEFAULT_THINKING_TURNS: u64 = 1;
const DEFAULT_TOOL_TRIGGER: u64 = 100_000;
const DEFAULT_TOOL_KEEP: u64 = 3;
const DEFAULT_CLEAR_AT_LEAST: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PromptCacheMode {
    Disabled,
    #[serde(rename = "automatic-5m")]
    Automatic5m,
    #[serde(rename = "automatic-1h")]
    Automatic1h,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ThinkingMode {
    Disabled,
    KeepAll,
    KeepTurns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ToolMode {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum TriggerMode {
    ProviderDefault,
    InputTokens,
    ToolUses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KeepMode {
    ProviderDefault,
    ToolUses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ClearAtLeastMode {
    Disabled,
    InputTokens,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptCacheWire {
    mode: PromptCacheMode,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThinkingWire {
    mode: ThinkingMode,
    keep_turns: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerWire {
    mode: TriggerMode,
    value: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeepWire {
    mode: KeepMode,
    value: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearAtLeastWire {
    mode: ClearAtLeastMode,
    value: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsWire {
    mode: ToolMode,
    trigger: TriggerWire,
    keep: KeepWire,
    clear_at_least: ClearAtLeastWire,
    clear_tool_inputs: bool,
    exclude_tools: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextEditingWire {
    thinking: ThinkingWire,
    tools: ToolsWire,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsWire {
    prompt_cache: PromptCacheWire,
    context_editing: ContextEditingWire,
}

/// One resolved Settings generation applied identically to inference and
/// token counting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicSettingsPolicies {
    prompt_cache: Option<AnthropicPromptCachePolicy>,
    context_editing: Option<AnthropicContextEditingPolicy>,
}

impl AnthropicSettingsPolicies {
    /// Parse one complete resolved namespace value.
    ///
    /// # Errors
    /// Missing/unknown fields, invalid modes, zero/oversized quantities or
    /// invalid tool exclusions are refused.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, AnthropicSettingsError> {
        let wire: SettingsWire = serde_json::from_value(value.clone())
            .map_err(|_| AnthropicSettingsError::InvalidSettings)?;
        validate_quantity(wire.context_editing.thinking.keep_turns)?;
        validate_quantity(wire.context_editing.tools.trigger.value)?;
        validate_quantity(wire.context_editing.tools.keep.value)?;
        validate_quantity(wire.context_editing.tools.clear_at_least.value)?;

        let prompt_cache = match wire.prompt_cache.mode {
            PromptCacheMode::Disabled => None,
            PromptCacheMode::Automatic5m => Some(AnthropicPromptCachePolicy::automatic(
                AnthropicPromptCacheTtl::FiveMinutes,
            )),
            PromptCacheMode::Automatic1h => Some(AnthropicPromptCachePolicy::automatic(
                AnthropicPromptCacheTtl::OneHour,
            )),
        };
        let thinking = match wire.context_editing.thinking.mode {
            ThinkingMode::Disabled => None,
            ThinkingMode::KeepAll => Some(
                AnthropicThinkingClear::new(AnthropicThinkingKeep::All)
                    .map_err(|_| AnthropicSettingsError::InvalidSettings)?,
            ),
            ThinkingMode::KeepTurns => Some(
                AnthropicThinkingClear::new(AnthropicThinkingKeep::Turns(
                    wire.context_editing.thinking.keep_turns,
                ))
                .map_err(|_| AnthropicSettingsError::InvalidSettings)?,
            ),
        };
        let trigger = match wire.context_editing.tools.trigger.mode {
            TriggerMode::ProviderDefault => None,
            TriggerMode::InputTokens => Some(AnthropicToolClearTrigger::InputTokens(
                wire.context_editing.tools.trigger.value,
            )),
            TriggerMode::ToolUses => Some(AnthropicToolClearTrigger::ToolUses(
                wire.context_editing.tools.trigger.value,
            )),
        };
        let keep = match wire.context_editing.tools.keep.mode {
            KeepMode::ProviderDefault => None,
            KeepMode::ToolUses => Some(wire.context_editing.tools.keep.value),
        };
        let clear_at_least = match wire.context_editing.tools.clear_at_least.mode {
            ClearAtLeastMode::Disabled => None,
            ClearAtLeastMode::InputTokens => Some(wire.context_editing.tools.clear_at_least.value),
        };
        let tool_policy = AnthropicToolClear::with_trigger(
            trigger,
            keep,
            clear_at_least,
            wire.context_editing.tools.clear_tool_inputs,
            wire.context_editing.tools.exclude_tools,
        )
        .map_err(|_| AnthropicSettingsError::InvalidSettings)?;
        let tools = (wire.context_editing.tools.mode == ToolMode::Enabled).then_some(tool_policy);
        let context_editing = if thinking.is_none() && tools.is_none() {
            None
        } else {
            Some(
                AnthropicContextEditingPolicy::new(thinking, tools)
                    .map_err(|_| AnthropicSettingsError::InvalidSettings)?,
            )
        };
        Ok(Self {
            prompt_cache,
            context_editing,
        })
    }

    /// Parse the exact registered Anthropic snapshot.
    ///
    /// # Errors
    /// A different namespace or invalid resolved value is refused.
    pub fn from_snapshot(snapshot: &SettingsSnapshot) -> Result<Self, AnthropicSettingsError> {
        if snapshot.namespace().as_str() != ANTHROPIC_SETTINGS_NAMESPACE {
            return Err(AnthropicSettingsError::Unavailable);
        }
        Self::from_value(snapshot.resolved())
    }

    /// Resolve the registered namespace from the composed Settings service.
    ///
    /// # Errors
    /// Missing/poisoned Settings state or an invalid resolved value is
    /// refused with no fallback to enabled behavior.
    pub fn resolve(settings: &SettingsService) -> Result<Self, AnthropicSettingsError> {
        let namespace =
            anthropic_settings_namespace().map_err(|_| AnthropicSettingsError::Unavailable)?;
        let snapshot = settings
            .get(&namespace)
            .map_err(|_| AnthropicSettingsError::Unavailable)?
            .ok_or(AnthropicSettingsError::Unavailable)?;
        Self::from_snapshot(&snapshot)
    }

    /// Optional automatic prompt-cache policy.
    #[must_use]
    pub const fn prompt_cache(&self) -> Option<&AnthropicPromptCachePolicy> {
        self.prompt_cache.as_ref()
    }

    /// Optional ordered context-editing policy.
    #[must_use]
    pub const fn context_editing(&self) -> Option<&AnthropicContextEditingPolicy> {
        self.context_editing.as_ref()
    }

    /// Apply this generation to one newly constructed provider.
    ///
    /// # Errors
    /// Durable provider-option construction failure.
    pub fn apply_provider(
        &self,
        mut provider: AnthropicProvider,
    ) -> Result<AnthropicProvider, heycode_llm::LlmError> {
        if let Some(policy) = &self.context_editing {
            provider = provider.with_context_editing(policy.clone())?;
        }
        if let Some(policy) = &self.prompt_cache {
            provider = provider.with_prompt_caching(policy.clone())?;
        }
        Ok(provider)
    }

    /// Apply this same generation to token-count request construction.
    #[must_use]
    pub fn apply_token_counter_config(
        &self,
        mut config: AnthropicTokenCounterConfig,
    ) -> AnthropicTokenCounterConfig {
        if let Some(policy) = &self.context_editing {
            config = config.with_context_editing(policy.clone());
        }
        if let Some(policy) = &self.prompt_cache {
            config = config.with_prompt_caching(policy.clone());
        }
        config
    }
}

/// Closed settings refusal without rejected values or tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnthropicSettingsError {
    /// The registered namespace is absent or unavailable.
    Unavailable,
    /// The resolved settings value is malformed or inconsistent.
    InvalidSettings,
}

impl std::fmt::Display for AnthropicSettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "Anthropic settings are unavailable",
            Self::InvalidSettings => "Anthropic settings are invalid",
        })
    }
}

impl std::error::Error for AnthropicSettingsError {}

/// Exact Settings namespace identity.
///
/// # Errors
/// Static namespace validation failure.
pub fn anthropic_settings_namespace() -> Result<SettingsNamespace, heycode_settings::SettingsError>
{
    SettingsNamespace::new(ANTHROPIC_SETTINGS_NAMESPACE)
}

/// Build the restart-applied, wire-visible Settings contract.
///
/// Every quantity has an explicit mode and a positive visible value. Disabled
/// or provider-default modes never use zero as an omission sentinel.
///
/// # Errors
/// Static namespace/schema/default validation failure.
pub fn anthropic_settings_definition() -> Result<SettingsDefinition, heycode_settings::SettingsError>
{
    let defaults = serde_json::json!({
        "prompt_cache":{"mode":"disabled"},
        "context_editing":{
            "thinking":{"mode":"disabled","keep_turns":DEFAULT_THINKING_TURNS},
            "tools":{
                "mode":"disabled",
                "trigger":{"mode":"provider-default","value":DEFAULT_TOOL_TRIGGER},
                "keep":{"mode":"provider-default","value":DEFAULT_TOOL_KEEP},
                "clear_at_least":{"mode":"disabled","value":DEFAULT_CLEAR_AT_LEAST},
                "clear_tool_inputs":false,
                "exclude_tools":[]
            }
        }
    });
    let quantity = || {
        serde_json::json!({
            "type":"integer",
            "minimum":1,
            "maximum":MAX_SETTINGS_QUANTITY
        })
    };
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["prompt_cache","context_editing"],
            "properties":{
                "prompt_cache":{
                    "type":"object","additionalProperties":false,"required":["mode"],
                    "properties":{"mode":{"type":"string","enum":[
                        "disabled","automatic-5m","automatic-1h"
                    ]}}
                },
                "context_editing":{
                    "type":"object","additionalProperties":false,
                    "required":["thinking","tools"],
                    "properties":{
                        "thinking":{
                            "type":"object","additionalProperties":false,
                            "required":["mode","keep_turns"],
                            "properties":{
                                "mode":{"type":"string","enum":[
                                    "disabled","keep-all","keep-turns"
                                ]},
                                "keep_turns":quantity()
                            }
                        },
                        "tools":{
                            "type":"object","additionalProperties":false,
                            "required":["mode","trigger","keep","clear_at_least",
                                "clear_tool_inputs","exclude_tools"],
                            "properties":{
                                "mode":{"type":"string","enum":["disabled","enabled"]},
                                "trigger":{
                                    "type":"object","additionalProperties":false,
                                    "required":["mode","value"],
                                    "properties":{
                                        "mode":{"type":"string","enum":[
                                            "provider-default","input-tokens","tool-uses"
                                        ]},
                                        "value":quantity()
                                    }
                                },
                                "keep":{
                                    "type":"object","additionalProperties":false,
                                    "required":["mode","value"],
                                    "properties":{
                                        "mode":{"type":"string","enum":[
                                            "provider-default","tool-uses"
                                        ]},
                                        "value":quantity()
                                    }
                                },
                                "clear_at_least":{
                                    "type":"object","additionalProperties":false,
                                    "required":["mode","value"],
                                    "properties":{
                                        "mode":{"type":"string","enum":[
                                            "disabled","input-tokens"
                                        ]},
                                        "value":quantity()
                                    }
                                },
                                "clear_tool_inputs":{"type":"boolean"},
                                "exclude_tools":{
                                    "type":"array","maxItems":64,"uniqueItems":true,
                                    "items":{"type":"string","minLength":1,"maxLength":128}
                                }
                            }
                        }
                    }
                }
            }
        }),
        defaults,
        |value| {
            AnthropicSettingsPolicies::from_value(value)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?
    .with_wire_exposure();
    Ok(
        SettingsDefinition::new(anthropic_settings_namespace()?, schema)
            .with_applies(SettingsApplies::Restart),
    )
}

fn validate_quantity(value: u64) -> Result<(), AnthropicSettingsError> {
    if value == 0 || value > MAX_SETTINGS_QUANTITY {
        Err(AnthropicSettingsError::InvalidSettings)
    } else {
        Ok(())
    }
}

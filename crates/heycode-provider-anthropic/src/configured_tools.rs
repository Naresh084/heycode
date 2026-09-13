//! Restart-applied configuration for deployment-specific Anthropic server tools.

use std::collections::BTreeSet;

use heycode_core::{
    Context, CoreError, CoreResult, NativeToolImplementationKind, Plugin, PluginContributionKind,
    PluginDescriptor,
};
use heycode_llm::{CapabilitySupport, LlmError};
use heycode_settings::{
    SettingsApplies, SettingsDefinition, SettingsNamespace, SettingsSchema, SettingsService,
    SettingsSnapshot,
};
use serde::Deserialize;

use crate::catalog::ANTHROPIC_PROVIDER;
use crate::{
    AnthropicMaintainedDefaultServerToolPolicy, AnthropicProvider, AnthropicServerToolDefinition,
    AnthropicServerToolFault, AnthropicServerToolKind, AnthropicServerToolPlan,
    server_tool_support,
};

/// Restart-applied Settings namespace for configuration-dependent server tools.
pub const ANTHROPIC_SERVER_TOOLS_SETTINGS_NAMESPACE: &str = "anthropic-server-tools";

/// The exact maintained-default family excluded by affirmative Unsupported
/// evidence.
pub const ANTHROPIC_EXCLUDED_DEFAULT_SERVER_TOOL_KINDS: [AnthropicServerToolKind; 1] =
    [AnthropicServerToolKind::WebFetch];

const MAX_MCP_SERVERS: usize = 20;
const MAX_DEFERRED_TOOL_NAMES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FeatureMode {
    Disabled,
    Enabled,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdvisorWire {
    mode: FeatureMode,
    model: String,
    max_uses: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolSearchWire {
    mode: FeatureMode,
    deferred_tool_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpServerWire {
    name: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpWire {
    mode: FeatureMode,
    servers: Vec<McpServerWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsWire {
    advisor: AdvisorWire,
    tool_search: ToolSearchWire,
    mcp: McpWire,
}

/// One exact Settings-derived Anthropic server-tool plan.
///
/// The plan is absent when the executor model proves no server-tool family and
/// no family was explicitly configured. Absent is not zero support advertised:
/// it publishes no N01 candidate and leaves the provider unconfigured.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicConfiguredServerToolPolicy {
    plan: Option<AnthropicServerToolPlan>,
}

impl AnthropicConfiguredServerToolPolicy {
    /// Build and model-admit one complete plan from a Settings value.
    ///
    /// The maintained-default baseline is admitted per family against the
    /// executor: a family the executor has no capability evidence for is
    /// absent from the plan, so an executor outside the maintained default
    /// still yields a valid policy instead of refusing to compose. Explicitly
    /// configured advisor, tool-search and MCP families are never downgraded
    /// that way — they name what the operator asked for, so unproven or
    /// affirmatively unsupported evidence refuses.
    ///
    /// # Errors
    /// Partial, duplicate, unsafe configuration, or an explicitly configured
    /// family the executor does not prove, is refused before provider or N01
    /// publication.
    pub fn from_value(
        value: &serde_json::Value,
        executor_model: &str,
    ) -> Result<Self, AnthropicServerToolSettingsFault> {
        let wire: SettingsWire = serde_json::from_value(value.clone())
            .map_err(|_| AnthropicServerToolSettingsFault::InvalidSettings)?;
        if !credential_free_metadata(&wire.advisor.model)
            || wire
                .tool_search
                .deferred_tool_names
                .iter()
                .any(|value| !credential_free_metadata(value))
            || wire.mcp.servers.iter().any(|server| {
                !credential_free_metadata(&server.name) || !credential_free_metadata(&server.url)
            })
        {
            return Err(AnthropicServerToolSettingsFault::InvalidSettings);
        }
        if wire.advisor.max_uses == 0 {
            return Err(AnthropicServerToolSettingsFault::InvalidSettings);
        }

        let advisor = match wire.advisor.mode {
            FeatureMode::Disabled => {
                if !wire.advisor.model.is_empty() || wire.advisor.max_uses != 1 {
                    return Err(AnthropicServerToolSettingsFault::InvalidSettings);
                }
                None
            }
            FeatureMode::Enabled => Some(
                AnthropicServerToolDefinition::advisor(wire.advisor.model, wire.advisor.max_uses)
                    .map_err(map_server_tool_fault)?,
            ),
        };

        let deferred_tool_names = match wire.tool_search.mode {
            FeatureMode::Disabled => {
                if !wire.tool_search.deferred_tool_names.is_empty() {
                    return Err(AnthropicServerToolSettingsFault::InvalidSettings);
                }
                None
            }
            FeatureMode::Enabled => {
                if wire.tool_search.deferred_tool_names.is_empty()
                    || wire.tool_search.deferred_tool_names.len() > MAX_DEFERRED_TOOL_NAMES
                    || wire
                        .tool_search
                        .deferred_tool_names
                        .iter()
                        .any(|name| !safe_deferred_tool_name(name))
                    || has_duplicate_strings(&wire.tool_search.deferred_tool_names)
                {
                    return Err(AnthropicServerToolSettingsFault::InvalidSettings);
                }
                Some(wire.tool_search.deferred_tool_names)
            }
        };

        let mcp_definitions = match wire.mcp.mode {
            FeatureMode::Disabled => {
                if !wire.mcp.servers.is_empty() {
                    return Err(AnthropicServerToolSettingsFault::InvalidSettings);
                }
                Vec::new()
            }
            FeatureMode::Enabled => {
                if wire.mcp.servers.is_empty() || wire.mcp.servers.len() > MAX_MCP_SERVERS {
                    return Err(AnthropicServerToolSettingsFault::InvalidSettings);
                }
                let mut names = BTreeSet::new();
                let mut urls = BTreeSet::new();
                let mut definitions = Vec::with_capacity(wire.mcp.servers.len());
                for server in wire.mcp.servers {
                    if !names.insert(server.name.clone()) || !urls.insert(server.url.clone()) {
                        return Err(AnthropicServerToolSettingsFault::InvalidSettings);
                    }
                    definitions.push(
                        AnthropicServerToolDefinition::mcp_connector(server.name, server.url)
                            .map_err(map_server_tool_fault)?,
                    );
                }
                definitions
            }
        };

        let baseline = AnthropicMaintainedDefaultServerToolPolicy::atomic_zero_configuration()
            .map_err(map_server_tool_fault)?
            .into_plan();
        let mut definitions = baseline
            .definitions()
            .iter()
            .filter(|definition| {
                server_tool_support(executor_model, definition.kind())
                    == CapabilitySupport::Supported
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(definition) = advisor {
            definitions.push(definition);
        }
        if deferred_tool_names.is_some() {
            definitions.push(AnthropicServerToolDefinition::tool_search_regex());
        }
        definitions.extend(mcp_definitions);
        if definitions.is_empty() {
            return Ok(Self { plan: None });
        }
        let mut plan = AnthropicServerToolPlan::new(definitions).map_err(map_server_tool_fault)?;
        if let Some(names) = deferred_tool_names {
            plan = plan
                .with_deferred_tools(names)
                .map_err(map_server_tool_fault)?;
        }
        plan.tools_for(executor_model)
            .map_err(map_server_tool_fault)?;
        Ok(Self { plan: Some(plan) })
    }

    /// Parse the exact restart-applied, wire-verified namespace snapshot.
    ///
    /// # Errors
    /// Snapshot identity/timing/exposure or plan admission failure is refused.
    pub fn from_snapshot(
        snapshot: &SettingsSnapshot,
        executor_model: &str,
    ) -> Result<Self, AnthropicServerToolSettingsFault> {
        if snapshot.namespace().as_str() != ANTHROPIC_SERVER_TOOLS_SETTINGS_NAMESPACE
            || snapshot.applies() != SettingsApplies::Restart
            || !snapshot.wire_exposed()
        {
            return Err(AnthropicServerToolSettingsFault::Unavailable);
        }
        Self::from_value(snapshot.resolved(), executor_model)
    }

    /// Resolve the registered namespace for one explicit executor model.
    ///
    /// # Errors
    /// Missing/poisoned Settings state or invalid capability/configuration
    /// fails without hidden fallback.
    pub fn resolve(
        settings: &SettingsService,
        executor_model: &str,
    ) -> Result<Self, AnthropicServerToolSettingsFault> {
        let namespace = anthropic_server_tools_settings_namespace()
            .map_err(|_| AnthropicServerToolSettingsFault::Unavailable)?;
        let snapshot = settings
            .get(&namespace)
            .map_err(|_| AnthropicServerToolSettingsFault::Unavailable)?
            .ok_or(AnthropicServerToolSettingsFault::Unavailable)?;
        Self::from_snapshot(&snapshot, executor_model)
    }

    /// Exact executable logical kinds, deduplicating multiple MCP servers.
    ///
    /// An executor with no proven family and no explicit configuration yields
    /// no kinds at all.
    #[must_use]
    pub fn kinds(&self) -> Vec<AnthropicServerToolKind> {
        let mut kinds = Vec::new();
        let Some(plan) = &self.plan else {
            return kinds;
        };
        for definition in plan.definitions() {
            if !kinds.contains(&definition.kind()) {
                kinds.push(definition.kind());
            }
        }
        kinds
    }

    /// Exact provider plan shared by provider configuration and candidate
    /// generation, absent when no family is admitted.
    #[must_use]
    pub const fn plan(&self) -> Option<&AnthropicServerToolPlan> {
        self.plan.as_ref()
    }

    /// Exact maintained-default exclusions.
    #[must_use]
    pub const fn excluded_default_kinds(&self) -> &'static [AnthropicServerToolKind] {
        &ANTHROPIC_EXCLUDED_DEFAULT_SERVER_TOOL_KINDS
    }

    /// Configure one newly constructed provider with this exact plan.
    ///
    /// An absent plan leaves the provider unchanged, so the feature is absent
    /// rather than advertised.
    ///
    /// # Errors
    /// Shared adapter/provider-option admission failure prevents publication.
    pub fn configure(self, provider: AnthropicProvider) -> Result<AnthropicProvider, LlmError> {
        match self.plan {
            Some(plan) => provider.with_server_tools(plan),
            None => Ok(provider),
        }
    }
}

impl std::fmt::Debug for AnthropicConfiguredServerToolPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicConfiguredServerToolPolicy")
            .field("kinds", &self.kinds())
            .field(
                "excluded_default_kinds",
                &ANTHROPIC_EXCLUDED_DEFAULT_SERVER_TOOL_KINDS,
            )
            .finish()
    }
}

/// Closed Settings/configuration refusal without model/tool/server values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnthropicServerToolSettingsFault {
    /// The namespace is absent or is not the required restart/exposed shape.
    Unavailable,
    /// The configuration is malformed, partial, duplicated, or unsafe.
    InvalidSettings,
    /// Exact evidence says the executor/pair does not support the selection.
    UnsupportedCapability,
    /// Exact capability evidence for the executor/pair is absent.
    UnprovenCapability,
}

impl std::fmt::Display for AnthropicServerToolSettingsFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "Anthropic server-tool settings are unavailable",
            Self::InvalidSettings => "Anthropic server-tool settings are invalid",
            Self::UnsupportedCapability => {
                "Anthropic server-tool settings select an unsupported capability"
            }
            Self::UnprovenCapability => {
                "Anthropic server-tool settings select an unproven capability"
            }
        })
    }
}

impl std::error::Error for AnthropicServerToolSettingsFault {}

/// Exact Settings namespace identity.
///
/// # Errors
/// Static namespace validation failure.
pub fn anthropic_server_tools_settings_namespace()
-> Result<SettingsNamespace, heycode_settings::SettingsError> {
    SettingsNamespace::new(ANTHROPIC_SERVER_TOOLS_SETTINGS_NAMESPACE)
}

/// Build the restart-applied, wire-visible definition for one explicit
/// executor model.
///
/// The defaults keep every configuration family off, so they validate for any
/// executor; an executor without server-tool evidence yields an empty admitted
/// plan instead of an unregistrable namespace.
///
/// # Errors
/// Static schema/default/namespace failure is returned before registration.
pub fn anthropic_server_tools_settings_definition(
    executor_model: impl Into<String>,
) -> Result<SettingsDefinition, heycode_settings::SettingsError> {
    let executor_model = executor_model.into();
    let validator_model = executor_model.clone();
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["advisor","tool_search","mcp"],
            "properties":{
                "advisor":{
                    "type":"object","additionalProperties":false,
                    "required":["mode","model","max_uses"],
                    "properties":{
                        "mode":{"type":"string","enum":["disabled","enabled"]},
                        "model":{
                            "type":"string","maxLength":128,
                            "pattern":"^$|^[A-Za-z0-9][A-Za-z0-9_.:/-]*$"
                        },
                        "max_uses":{"type":"integer","minimum":1,"maximum":4294967295_u64}
                    }
                },
                "tool_search":{
                    "type":"object","additionalProperties":false,
                    "required":["mode","deferred_tool_names"],
                    "properties":{
                        "mode":{"type":"string","enum":["disabled","enabled"]},
                        "deferred_tool_names":{
                            "type":"array","maxItems":MAX_DEFERRED_TOOL_NAMES,
                            "uniqueItems":true,
                            "items":{
                                "type":"string","minLength":1,"maxLength":64,
                                "pattern":"^[A-Za-z0-9_-]+$"
                            }
                        }
                    }
                },
                "mcp":{
                    "type":"object","additionalProperties":false,
                    "required":["mode","servers"],
                    "properties":{
                        "mode":{"type":"string","enum":["disabled","enabled"]},
                        "servers":{
                            "type":"array","maxItems":MAX_MCP_SERVERS,
                            "items":{
                                "type":"object","additionalProperties":false,
                                "required":["name","url"],
                                "properties":{
                                    "name":{
                                        "type":"string","minLength":1,"maxLength":128,
                                        "pattern":"^[A-Za-z0-9][A-Za-z0-9_.:/-]*$"
                                    },
                                    "url":{"type":"string","minLength":1,"maxLength":2048}
                                }
                            }
                        }
                    }
                }
            }
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"disabled","servers":[]}
        }),
        move |value| {
            AnthropicConfiguredServerToolPolicy::from_value(value, &validator_model)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?
    .with_wire_exposure();
    Ok(
        SettingsDefinition::new(anthropic_server_tools_settings_namespace()?, schema)
            .with_applies(SettingsApplies::Restart),
    )
}

/// Register the Settings-derived N01 candidates for one explicit executor.
///
/// The plugin intentionally retains the existing `native-anthropic` id so the
/// root can replace the zero-configuration factory without a persisted profile
/// migration. The two implementations must not be composed together.
#[must_use]
pub fn anthropic_configured_native_tools_plugin(
    executor_model: impl Into<String>,
) -> Box<dyn Plugin> {
    struct AnthropicConfiguredNativeToolsPlugin {
        executor_model: String,
    }

    impl Plugin for AnthropicConfiguredNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-anthropic"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Service,
                    PluginContributionKind::Tool,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                ANTHROPIC_SERVER_TOOLS_SETTINGS_NAMESPACE,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_settings::SERVICE_SETTINGS,
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = context
                .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service type mismatch"))?;
            let registry = context
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| CoreError::other("native-tools service type mismatch"))?;
            let snapshot = settings
                .register(
                    context,
                    anthropic_server_tools_settings_definition(self.executor_model.clone())
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let policy =
                AnthropicConfiguredServerToolPolicy::from_snapshot(&snapshot, &self.executor_model)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            for kind in policy.kinds() {
                context.contribute(
                    heycode_core::ContributionKind::NativeTool,
                    kind.implementation_id(),
                )?;
                let implementation = heycode_native_tools::NativeToolImplementation::new(
                    kind.as_str(),
                    kind.implementation_id(),
                    NativeToolImplementationKind::Provider,
                    Some(ANTHROPIC_PROVIDER.to_owned()),
                    100,
                )
                .and_then(|implementation| {
                    implementation.with_models(vec![self.executor_model.clone()])
                })
                .map_err(|error| CoreError::other(error.to_string()))?;
                registry
                    .register(context, implementation)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            Ok(())
        }
    }

    Box::new(AnthropicConfiguredNativeToolsPlugin {
        executor_model: executor_model.into(),
    })
}

fn safe_deferred_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'))
}

fn has_duplicate_strings(values: &[String]) -> bool {
    let mut unique = BTreeSet::new();
    values.iter().any(|value| !unique.insert(value.as_str()))
}

fn map_server_tool_fault(fault: AnthropicServerToolFault) -> AnthropicServerToolSettingsFault {
    match fault {
        AnthropicServerToolFault::UnsupportedCapability => {
            AnthropicServerToolSettingsFault::UnsupportedCapability
        }
        AnthropicServerToolFault::UnprovenCapability => {
            AnthropicServerToolSettingsFault::UnprovenCapability
        }
        AnthropicServerToolFault::InvalidConfiguration
        | AnthropicServerToolFault::WrongRoute
        | AnthropicServerToolFault::InvalidState => {
            AnthropicServerToolSettingsFault::InvalidSettings
        }
    }
}

fn credential_free_metadata(value: &str) -> bool {
    heycode_settings::screen_text_for_credentials(value).is_ok()
        && value
            .split(['/', ':', '?', '#', '&', '='])
            .all(|part| heycode_settings::screen_text_for_credentials(part).is_ok())
}

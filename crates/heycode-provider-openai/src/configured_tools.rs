//! Restart-applied configuration for deployment-specific OpenAI hosted tools.
//!
//! File search can join the complete Responses/N02 bridge today. Remote MCP
//! metadata is admitted credential-free with mandatory approval, but remains
//! outside the executable plan until an approval-response owner exists.

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

use crate::catalog::OPENAI_PROVIDER;
use crate::{
    OpenAiHostedToolDefinition, OpenAiHostedToolFault, OpenAiHostedToolKind,
    OpenAiHostedToolProductPolicy, OpenAiHostedTools, OpenAiProvider, hosted_tool_support,
};

/// Restart-applied Settings namespace for deployment-specific hosted tools.
pub const OPENAI_HOSTED_TOOLS_SETTINGS_NAMESPACE: &str = "openai-hosted-tools";

/// Hosted families whose action, media, or approval loop is not owned by the
/// current shared bridge.
pub const OPENAI_UNOWNED_UPPER_LOOP_KINDS: [OpenAiHostedToolKind; 3] = [
    OpenAiHostedToolKind::ComputerUse,
    OpenAiHostedToolKind::ImageGeneration,
    OpenAiHostedToolKind::RemoteMcp,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FileSearchMode {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RemoteMcpMode {
    Disabled,
    Configured,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSearchWire {
    mode: FileSearchMode,
    vector_store_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteMcpWire {
    mode: RemoteMcpMode,
    server_label: String,
    server_url: String,
    require_approval: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsWire {
    file_search: FileSearchWire,
    remote_mcp: RemoteMcpWire,
}

/// One exact Settings-derived OpenAI hosted-tool policy.
///
/// The executable definition list always contains the named bridge-complete
/// baseline and may add file search. A configured remote MCP definition is
/// retained separately so no caller can mistake admitted metadata for an
/// executable approval loop.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiConfiguredHostedToolPolicy {
    definitions: Vec<OpenAiHostedToolDefinition>,
    configured_remote_mcp: Option<OpenAiHostedToolDefinition>,
}

impl OpenAiConfiguredHostedToolPolicy {
    /// Build one exact model-admitted policy from a complete namespace value.
    ///
    /// The zero-configuration baseline is admitted per family against the
    /// selected model: a family the model has no capability evidence for is
    /// simply absent from the executable plan, so a model outside the
    /// maintained default still yields a valid policy with no hosted tools
    /// rather than refusing to compose. Explicitly enabled configuration —
    /// file search or a remote MCP server — is never downgraded that way; it
    /// names a family the operator asked for, so unproven or affirmatively
    /// unsupported evidence refuses.
    ///
    /// # Errors
    /// Partial, duplicate, unsafe configuration, or an explicitly enabled
    /// family the selected model does not prove, is refused before a provider
    /// or N01 candidate can publish.
    pub fn from_value(
        value: &serde_json::Value,
        model: &str,
    ) -> Result<Self, OpenAiHostedToolSettingsFault> {
        let wire: SettingsWire = serde_json::from_value(value.clone())
            .map_err(|_| OpenAiHostedToolSettingsFault::InvalidSettings)?;
        if wire
            .file_search
            .vector_store_ids
            .iter()
            .any(|value| !credential_free_metadata(value))
            || !credential_free_metadata(&wire.remote_mcp.server_label)
            || !credential_free_metadata(&wire.remote_mcp.server_url)
            || wire.remote_mcp.require_approval != "always"
        {
            return Err(OpenAiHostedToolSettingsFault::InvalidSettings);
        }

        let file_search = match wire.file_search.mode {
            FileSearchMode::Disabled => {
                if !wire.file_search.vector_store_ids.is_empty() {
                    return Err(OpenAiHostedToolSettingsFault::InvalidSettings);
                }
                None
            }
            FileSearchMode::Enabled => Some(
                OpenAiHostedToolDefinition::file_search(wire.file_search.vector_store_ids)
                    .map_err(map_hosted_tool_fault)?,
            ),
        };
        let configured_remote_mcp = match wire.remote_mcp.mode {
            RemoteMcpMode::Disabled => {
                if !wire.remote_mcp.server_label.is_empty()
                    || !wire.remote_mcp.server_url.is_empty()
                {
                    return Err(OpenAiHostedToolSettingsFault::InvalidSettings);
                }
                None
            }
            RemoteMcpMode::Configured => Some(
                OpenAiHostedToolDefinition::remote_mcp(
                    wire.remote_mcp.server_label,
                    wire.remote_mcp.server_url,
                )
                .map_err(map_hosted_tool_fault)?,
            ),
        };
        let mut definitions = OpenAiHostedToolProductPolicy::bridge_complete_zero_configuration()
            .into_definitions()
            .into_iter()
            .filter(|definition| {
                hosted_tool_support(model, definition.kind()) == CapabilitySupport::Supported
            })
            .collect::<Vec<_>>();
        if let Some(definition) = file_search {
            require_supported(model, definition.kind())?;
            let after_web_search = definitions
                .iter()
                .position(|prior| prior.kind() == OpenAiHostedToolKind::WebSearch)
                .map_or(0, |index| index + 1);
            definitions.insert(after_web_search, definition);
        }

        if !definitions.is_empty() {
            OpenAiHostedTools::new(model, definitions.clone()).map_err(map_hosted_tool_fault)?;
        }
        if let Some(definition) = &configured_remote_mcp {
            require_supported(model, definition.kind())?;
            definition.wire_for(model).map_err(map_hosted_tool_fault)?;
        }
        Ok(Self {
            definitions,
            configured_remote_mcp,
        })
    }

    /// Parse the exact restart-applied, wire-verified namespace snapshot.
    ///
    /// # Errors
    /// Snapshot identity/timing/exposure or configuration failure is refused.
    pub fn from_snapshot(
        snapshot: &SettingsSnapshot,
        model: &str,
    ) -> Result<Self, OpenAiHostedToolSettingsFault> {
        if snapshot.namespace().as_str() != OPENAI_HOSTED_TOOLS_SETTINGS_NAMESPACE
            || snapshot.applies() != SettingsApplies::Restart
            || !snapshot.wire_exposed()
        {
            return Err(OpenAiHostedToolSettingsFault::Unavailable);
        }
        Self::from_value(snapshot.resolved(), model)
    }

    /// Resolve the registered namespace for one explicit selected model.
    ///
    /// # Errors
    /// Missing/poisoned state or any invalid/unproven configuration fails with
    /// no fallback to a hidden tool set.
    pub fn resolve(
        settings: &SettingsService,
        model: &str,
    ) -> Result<Self, OpenAiHostedToolSettingsFault> {
        let namespace = openai_hosted_tools_settings_namespace()
            .map_err(|_| OpenAiHostedToolSettingsFault::Unavailable)?;
        let snapshot = settings
            .get(&namespace)
            .map_err(|_| OpenAiHostedToolSettingsFault::Unavailable)?
            .ok_or(OpenAiHostedToolSettingsFault::Unavailable)?;
        Self::from_snapshot(&snapshot, model)
    }

    /// Exact executable hosted kinds in wire-definition order.
    #[must_use]
    pub fn kinds(&self) -> Vec<OpenAiHostedToolKind> {
        self.definitions
            .iter()
            .map(OpenAiHostedToolDefinition::kind)
            .collect()
    }

    /// Credential-free remote MCP metadata awaiting an approval loop.
    #[must_use]
    pub const fn configured_remote_mcp(&self) -> Option<&OpenAiHostedToolDefinition> {
        self.configured_remote_mcp.as_ref()
    }

    /// Exact families that remain unavailable at the current upper boundary.
    #[must_use]
    pub const fn unowned_upper_loop_kinds(&self) -> &'static [OpenAiHostedToolKind] {
        &OPENAI_UNOWNED_UPPER_LOOP_KINDS
    }

    /// Configure one newly constructed provider with this exact executable
    /// plan.
    ///
    /// An empty plan — the selected model proves no hosted family — leaves the
    /// provider unchanged, so the feature is absent rather than advertised.
    ///
    /// # Errors
    /// Shared adapter/provider admission failure prevents publication.
    pub fn configure(self, provider: OpenAiProvider) -> Result<OpenAiProvider, LlmError> {
        if self.definitions.is_empty() {
            return Ok(provider);
        }
        provider.with_hosted_tools(self.definitions)
    }
}

impl std::fmt::Debug for OpenAiConfiguredHostedToolPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiConfiguredHostedToolPolicy")
            .field("kinds", &self.kinds())
            .field(
                "remote_mcp_configured",
                &self.configured_remote_mcp.is_some(),
            )
            .field("unowned_upper_loop_kinds", &OPENAI_UNOWNED_UPPER_LOOP_KINDS)
            .finish()
    }
}

/// Closed Settings/configuration refusal with no provider metadata values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenAiHostedToolSettingsFault {
    /// The namespace is absent or is not the required restart/exposed shape.
    Unavailable,
    /// The complete configuration is malformed, partial, duplicated, or unsafe.
    InvalidSettings,
    /// Exact evidence says the selected model lacks a configured family.
    UnsupportedCapability,
    /// Exact capability evidence for the selected model/family is absent.
    UnprovenCapability,
}

impl std::fmt::Display for OpenAiHostedToolSettingsFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "OpenAI hosted-tool settings are unavailable",
            Self::InvalidSettings => "OpenAI hosted-tool settings are invalid",
            Self::UnsupportedCapability => {
                "OpenAI hosted-tool settings select an unsupported capability"
            }
            Self::UnprovenCapability => "OpenAI hosted-tool settings select an unproven capability",
        })
    }
}

impl std::error::Error for OpenAiHostedToolSettingsFault {}

/// Exact Settings namespace identity.
///
/// # Errors
/// Static namespace validation failure.
pub fn openai_hosted_tools_settings_namespace()
-> Result<SettingsNamespace, heycode_settings::SettingsError> {
    SettingsNamespace::new(OPENAI_HOSTED_TOOLS_SETTINGS_NAMESPACE)
}

/// Build the restart-applied, wire-visible Settings definition for one
/// explicit selected model.
///
/// The defaults keep every configuration family off, so they validate for any
/// selected model; a model without hosted-tool evidence yields an empty
/// executable plan instead of an unregistrable namespace.
///
/// # Errors
/// Static schema/default/namespace failure is returned without publishing the
/// namespace.
pub fn openai_hosted_tools_settings_definition(
    model: impl Into<String>,
) -> Result<SettingsDefinition, heycode_settings::SettingsError> {
    let model = model.into();
    let validator_model = model.clone();
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["file_search","remote_mcp"],
            "properties":{
                "file_search":{
                    "type":"object",
                    "additionalProperties":false,
                    "required":["mode","vector_store_ids"],
                    "properties":{
                        "mode":{"type":"string","enum":["disabled","enabled"]},
                        "vector_store_ids":{
                            "type":"array","maxItems":16,"uniqueItems":true,
                            "items":{
                                "type":"string","minLength":1,"maxLength":128,
                                "pattern":"^[A-Za-z0-9][A-Za-z0-9_.:/-]*$"
                            }
                        }
                    }
                },
                "remote_mcp":{
                    "type":"object",
                    "additionalProperties":false,
                    "required":["mode","server_label","server_url","require_approval"],
                    "properties":{
                        "mode":{"type":"string","enum":["disabled","configured"]},
                        "server_label":{
                            "type":"string","maxLength":128,
                            "pattern":"^$|^[A-Za-z0-9][A-Za-z0-9_.:/-]*$"
                        },
                        "server_url":{"type":"string","maxLength":512},
                        "require_approval":{"type":"string","enum":["always"]}
                    }
                }
            }
        }),
        serde_json::json!({
            "file_search":{"mode":"disabled","vector_store_ids":[]},
            "remote_mcp":{
                "mode":"disabled",
                "server_label":"",
                "server_url":"",
                "require_approval":"always"
            }
        }),
        move |value| {
            OpenAiConfiguredHostedToolPolicy::from_value(value, &validator_model)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?
    .with_wire_exposure();
    Ok(
        SettingsDefinition::new(openai_hosted_tools_settings_namespace()?, schema)
            .with_applies(SettingsApplies::Restart),
    )
}

/// Register the Settings-derived N01 candidates for one explicit model.
///
/// The plugin intentionally retains the existing `native-openai` id so the
/// composition root can replace the zero-configuration factory without a
/// profile identity migration. It must not be composed beside that old
/// implementation.
#[must_use]
pub fn openai_configured_native_tools_plugin(model: impl Into<String>) -> Box<dyn Plugin> {
    struct OpenAiConfiguredNativeToolsPlugin {
        model: String,
    }

    impl Plugin for OpenAiConfiguredNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-openai"
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
                OPENAI_HOSTED_TOOLS_SETTINGS_NAMESPACE,
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
                    openai_hosted_tools_settings_definition(self.model.clone())
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let policy = OpenAiConfiguredHostedToolPolicy::from_snapshot(&snapshot, &self.model)
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
                    Some(OPENAI_PROVIDER.to_owned()),
                    100,
                )
                .and_then(|implementation| implementation.with_models(vec![self.model.clone()]))
                .map_err(|error| CoreError::other(error.to_string()))?;
                registry
                    .register(context, implementation)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            Ok(())
        }
    }

    Box::new(OpenAiConfiguredNativeToolsPlugin {
        model: model.into(),
    })
}

fn require_supported(
    model: &str,
    kind: OpenAiHostedToolKind,
) -> Result<(), OpenAiHostedToolSettingsFault> {
    match hosted_tool_support(model, kind) {
        CapabilitySupport::Supported => Ok(()),
        CapabilitySupport::Unsupported => Err(OpenAiHostedToolSettingsFault::UnsupportedCapability),
        CapabilitySupport::Unknown => Err(OpenAiHostedToolSettingsFault::UnprovenCapability),
    }
}

fn map_hosted_tool_fault(fault: OpenAiHostedToolFault) -> OpenAiHostedToolSettingsFault {
    match fault {
        OpenAiHostedToolFault::UnprovenCapability => {
            OpenAiHostedToolSettingsFault::UnprovenCapability
        }
        OpenAiHostedToolFault::InvalidConfiguration
        | OpenAiHostedToolFault::MissingSharedBridge
        | OpenAiHostedToolFault::WrongRoute
        | OpenAiHostedToolFault::WrongItemType
        | OpenAiHostedToolFault::InvalidItem => OpenAiHostedToolSettingsFault::InvalidSettings,
    }
}

fn credential_free_metadata(value: &str) -> bool {
    heycode_settings::screen_text_for_credentials(value).is_ok()
        && value
            .split(['/', ':', '?', '#', '&', '='])
            .all(|part| heycode_settings::screen_text_for_credentials(part).is_ok())
}

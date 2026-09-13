//! Exact named plugin contribution inventory shared by diagnostics and UI.

use std::sync::{Arc, Mutex};

use crate::{
    AppliedPlugin, CoreError, PluginContributionKind, PluginDescriptor, PluginScope, PluginSource,
};

/// Exact contribution namespace. Unlike descriptor families, these variants
/// distinguish registries that can legitimately reuse the same logical name.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContributionKind {
    /// Type-erased context service key.
    Service,
    /// Native inference provider adapter.
    InferenceProvider,
    /// Native or delegated coding-agent runtime.
    AgentRuntime,
    /// Live/maintained model catalog source.
    ModelCatalog,
    /// Durable catalog persistence provider.
    CatalogPersistence,
    /// Token-counting implementation, exact or estimated.
    TokenCounter,
    /// Credential provider implementation.
    CredentialProvider,
    /// Human authorization flow.
    AuthorizationFlow,
    /// Settings persistence/provider implementation.
    SettingsProvider,
    /// Settings schema namespace.
    SettingsNamespace,
    /// Public web search/fetch provider implementation.
    WebProvider,
    /// Public web source-content processor implementation.
    WebProcessor,
    /// Model-callable tool.
    Tool,
    /// Logical native-tool implementation candidate.
    NativeTool,
    /// Provider/request transformation implementation.
    RequestTransform,
    /// Human-only command.
    Command,
    /// Deterministic system-prompt section.
    PromptSection,
    /// Declarative skill exposed through the live skill registry.
    Skill,
    /// Declarative subagent preset.
    AgentPreset,
    /// Default preset in the fallback registry. A regular `AgentPreset` with
    /// the same name shadows it; neither registration owns the other's row.
    AgentPresetFallback,
    /// Native or delegated subagent execution provider.
    SubagentProvider,
    /// Typed lifecycle hook.
    Hook,
    /// Semantic terminal theme.
    Theme,
    /// Plugin-bundled MCP server definition.
    McpServer,
    /// Named interception/waterfall seam.
    InterceptionSeam,
    /// Layer attached to an interception seam.
    InterceptionLayer,
    /// Complete user-interface implementation.
    UserInterface,
    /// Named UI slot/panel contribution.
    UiSlot,
    /// Managed external process.
    ExternalProcess,
    /// Redacted doctor/health check.
    DoctorCheck,
    /// Closed content-free telemetry metric contribution.
    TelemetryMetric,
    /// Stable method on the heycode-owned local app-server protocol.
    AppServerMethod,
}

impl ContributionKind {
    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::InferenceProvider => "inference_provider",
            Self::AgentRuntime => "agent_runtime",
            Self::ModelCatalog => "model_catalog",
            Self::TokenCounter => "token_counter",
            Self::CatalogPersistence => "catalog_persistence",
            Self::CredentialProvider => "credential_provider",
            Self::AuthorizationFlow => "authorization_flow",
            Self::SettingsProvider => "settings_provider",
            Self::SettingsNamespace => "settings_namespace",
            Self::WebProvider => "web_provider",
            Self::WebProcessor => "web_processor",
            Self::Tool => "tool",
            Self::NativeTool => "native_tool",
            Self::RequestTransform => "request_transform",
            Self::Command => "command",
            Self::PromptSection => "prompt_section",
            Self::Skill => "skill",
            Self::AgentPreset => "agent_preset",
            Self::AgentPresetFallback => "agent_preset_fallback",
            Self::SubagentProvider => "subagent_provider",
            Self::Hook => "hook",
            Self::Theme => "theme",
            Self::McpServer => "mcp_server",
            Self::InterceptionSeam => "interception_seam",
            Self::InterceptionLayer => "interception_layer",
            Self::UserInterface => "user_interface",
            Self::UiSlot => "ui_slot",
            Self::ExternalProcess => "external_process",
            Self::DoctorCheck => "doctor_check",
            Self::TelemetryMetric => "telemetry_metric",
            Self::AppServerMethod => "app_server_method",
        }
    }

    pub(crate) const fn descriptor_family(self) -> PluginContributionKind {
        match self {
            Self::Service | Self::SettingsNamespace => PluginContributionKind::Service,
            Self::InferenceProvider
            | Self::AgentRuntime
            | Self::ModelCatalog
            | Self::TokenCounter
            | Self::CatalogPersistence
            | Self::CredentialProvider
            | Self::AuthorizationFlow
            | Self::SettingsProvider
            | Self::WebProvider
            | Self::WebProcessor
            | Self::RequestTransform => PluginContributionKind::Provider,
            Self::Tool | Self::NativeTool => PluginContributionKind::Tool,
            Self::Command => PluginContributionKind::Command,
            Self::PromptSection | Self::Skill => PluginContributionKind::PromptSection,
            Self::AgentPreset | Self::AgentPresetFallback | Self::SubagentProvider => {
                PluginContributionKind::Provider
            }
            Self::Hook | Self::InterceptionSeam | Self::InterceptionLayer => {
                PluginContributionKind::Waterfall
            }
            Self::Theme | Self::UserInterface | Self::UiSlot => {
                PluginContributionKind::UserInterface
            }
            Self::McpServer | Self::ExternalProcess => PluginContributionKind::ExternalProcess,
            Self::DoctorCheck | Self::TelemetryMetric => PluginContributionKind::Diagnostic,
            Self::AppServerMethod => PluginContributionKind::Service,
        }
    }
}

impl std::fmt::Display for ContributionKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One exact row declared by a plugin before apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginContributionSpec {
    /// Owning exact registry namespace.
    pub kind: ContributionKind,
    /// Stable name within that registry.
    pub name: String,
}

impl PluginContributionSpec {
    /// Construct a row; composition validates the name and collisions.
    #[must_use]
    pub fn new(kind: ContributionKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
        }
    }
}

/// One committed exact contribution attributed to its plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginContribution {
    /// Runtime plugin id.
    pub plugin: &'static str,
    /// Exact registry namespace.
    pub kind: ContributionKind,
    /// Stable row name.
    pub name: String,
}

/// Immutable inventory projection in composition/declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInventorySnapshot {
    /// Successfully applied plugin descriptors.
    pub plugins: Vec<AppliedPlugin>,
    /// Exact committed rows.
    pub contributions: Vec<PluginContribution>,
}

#[derive(Default)]
struct InventoryState {
    plugins: Vec<AppliedPlugin>,
    contributions: Vec<PluginContribution>,
}

/// Shared live inventory handle used by diagnostics and `/plugins`.
#[derive(Clone, Default)]
pub struct PluginInventory(Arc<Mutex<InventoryState>>);

impl PluginInventory {
    /// Snapshot all successfully applied plugins and their named rows.
    ///
    /// # Errors
    /// Inventory lock poisoning fails loud.
    pub fn snapshot(&self) -> Result<PluginInventorySnapshot, CoreError> {
        let state = self.0.lock().map_err(|_| CoreError::InventoryUnavailable)?;
        Ok(PluginInventorySnapshot {
            plugins: state.plugins.clone(),
            contributions: state.contributions.clone(),
        })
    }

    pub(crate) fn contribute(
        &self,
        plugin: &'static str,
        spec: PluginContributionSpec,
    ) -> Result<(), CoreError> {
        let name = spec.name;
        if name.is_empty()
            || name.trim() != name
            || name.len() > 256
            || name.chars().any(char::is_control)
        {
            return Err(CoreError::InvalidContribution {
                plugin: plugin.to_owned(),
                kind: spec.kind.to_string(),
                name,
            });
        }
        let mut state = self.0.lock().map_err(|_| CoreError::InventoryUnavailable)?;
        if let Some(existing) = state
            .contributions
            .iter()
            .find(|row| row.kind == spec.kind && row.name == name)
        {
            return Err(CoreError::DuplicateContribution {
                kind: spec.kind.to_string(),
                name,
                existing: existing.plugin.to_owned(),
                claimant: plugin.to_owned(),
            });
        }
        state.contributions.push(PluginContribution {
            plugin,
            kind: spec.kind,
            name,
        });
        Ok(())
    }

    /// Drop every exact row recorded past `contributions`. Returns `false`
    /// when the shared state could not be reached, which the caller must treat
    /// as unverified residue.
    ///
    /// Applied plugins are deliberately not unwound here: `record_plugin` is
    /// the commit point and nothing can fail after it, so a recorded plugin
    /// belongs to a committed activation. If that ever stops holding, the
    /// activation fingerprint reports the survivor rather than hiding it.
    pub(crate) fn rollback(&self, contributions: usize) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        state.contributions.truncate(contributions);
        true
    }

    pub(crate) fn record_plugin(
        &self,
        descriptor: PluginDescriptor,
        scope: PluginScope,
    ) -> Result<(), CoreError> {
        let mut state = self.0.lock().map_err(|_| CoreError::InventoryUnavailable)?;
        if descriptor.source != PluginSource::Unclassified {
            for row in state
                .contributions
                .iter()
                .filter(|row| row.plugin == descriptor.id)
            {
                let family = row.kind.descriptor_family();
                if !descriptor.contributions.contains(&family) {
                    return Err(CoreError::ContributionFamilyMismatch {
                        plugin: descriptor.id.to_owned(),
                        kind: row.kind.to_string(),
                        family: format!("{family:?}"),
                    });
                }
            }
        }
        state.plugins.push(AppliedPlugin { descriptor, scope });
        Ok(())
    }
}

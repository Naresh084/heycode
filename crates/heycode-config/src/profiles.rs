//! Strict profile schema and source-aware effective tree projection.

use std::path::PathBuf;

use heycode_core::PluginScope;
use serde::Deserialize;

use crate::ManagedCodeAuthorityConfig;
use crate::scopes::validate_plugin_id;
use crate::{
    ConfigError, EffectivePluginSelection, PluginDirective, PluginScopeLayer,
    resolve_scoped_plugins,
};

/// Current standalone profile document schema.
pub const PROFILE_SCHEMA_VERSION: u32 = 3;
const PROFILE_SCHEMA_V1: u32 = 1;
const PROFILE_SCHEMA_V2: u32 = 2;

/// One strict versioned profile document. Source/scope metadata is attached by
/// [`ProfileLayer`] rather than trusted from file bytes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocument {
    /// Exact supported profile schema.
    pub schema_version: u32,
    /// Optional human/stable profile label.
    #[serde(default)]
    pub name: Option<String>,
    /// Ordered enable/disable rows.
    #[serde(default)]
    pub plugins: Vec<ProfilePluginRow>,
    /// Administrator-only implementation/capability admission rules.
    #[serde(default)]
    pub constraints: Option<ManagedProfileConstraints>,
    /// Administrator-only exact installed-code authority generation.
    #[serde(default)]
    pub code_authority: Option<ManagedCodeAuthorityConfig>,
}

/// One plugin row in a profile document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePluginRow {
    /// Stable plugin/factory id.
    pub id: String,
    /// Enabled by default when omitted.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
}

/// Implementation sources a managed profile may admit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedPluginSource {
    /// Plugin implementation compiled into the heycode binary.
    BuiltIn,
    /// Compatibility/test implementation without classified provenance.
    Unclassified,
}

impl ManagedPluginSource {
    fn admits(self, source: heycode_core::PluginSource) -> bool {
        matches!(
            (self, source),
            (Self::BuiltIn, heycode_core::PluginSource::BuiltIn)
                | (Self::Unclassified, heycode_core::PluginSource::Unclassified)
        )
    }
}

/// Broad plugin capability a managed profile may forbid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedPluginCapability {
    /// Context service or settings namespace.
    Service,
    /// Provider/catalog/runtime implementation.
    Provider,
    /// Model-callable tool.
    Tool,
    /// Human-only command.
    Command,
    /// Deterministic prompt contribution.
    PromptSection,
    /// Interception seam or layer.
    Waterfall,
    /// User-interface surface.
    UserInterface,
    /// Managed external child process.
    ExternalProcess,
    /// Redacted diagnostic contribution.
    Diagnostic,
}

impl ManagedPluginCapability {
    fn denies(self, capability: heycode_core::PluginContributionKind) -> bool {
        matches!(
            (self, capability),
            (Self::Service, heycode_core::PluginContributionKind::Service)
                | (
                    Self::Provider,
                    heycode_core::PluginContributionKind::Provider
                )
                | (Self::Tool, heycode_core::PluginContributionKind::Tool)
                | (Self::Command, heycode_core::PluginContributionKind::Command)
                | (
                    Self::PromptSection,
                    heycode_core::PluginContributionKind::PromptSection
                )
                | (
                    Self::Waterfall,
                    heycode_core::PluginContributionKind::Waterfall
                )
                | (
                    Self::UserInterface,
                    heycode_core::PluginContributionKind::UserInterface
                )
                | (
                    Self::ExternalProcess,
                    heycode_core::PluginContributionKind::ExternalProcess
                )
                | (
                    Self::Diagnostic,
                    heycode_core::PluginContributionKind::Diagnostic
                )
        )
    }
}

/// Final administrator admission policy applied before plugin activation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProfileConstraints {
    /// Implementation sources allowed to activate. Empty means unrestricted.
    #[serde(default)]
    pub allowed_sources: Vec<ManagedPluginSource>,
    /// Broad capabilities forbidden even from an admitted source.
    #[serde(default)]
    pub denied_capabilities: Vec<ManagedPluginCapability>,
}

impl ManagedProfileConstraints {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.allowed_sources.is_empty() && self.denied_capabilities.is_empty() {
            return profile_error("managed profile constraints must contain at least one rule");
        }
        let sources = self
            .allowed_sources
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let capabilities = self
            .denied_capabilities
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if sources.len() != self.allowed_sources.len()
            || capabilities.len() != self.denied_capabilities.len()
        {
            return profile_error("managed profile constraints contain duplicate rules");
        }
        Ok(())
    }

    fn admit(&self, plugin: &dyn heycode_core::Plugin) -> Result<(), ConfigError> {
        let descriptor = plugin.descriptor();
        if !self.allowed_sources.is_empty()
            && !self
                .allowed_sources
                .iter()
                .any(|allowed| allowed.admits(descriptor.source))
        {
            return constraint_error(format!(
                "plugin `{}` source `{}` is forbidden by managed policy",
                plugin.name(),
                descriptor.source.as_str()
            ));
        }
        if let Some(capability) = descriptor.contributions.iter().find(|capability| {
            self.denied_capabilities
                .iter()
                .any(|denied| denied.denies(**capability))
        }) {
            return constraint_error(format!(
                "plugin `{}` capability `{}` is forbidden by managed policy",
                plugin.name(),
                capability.as_str()
            ));
        }
        Ok(())
    }
}

const fn enabled_default() -> bool {
    true
}

impl ProfileDocument {
    /// Parse and validate one standalone profile TOML document.
    ///
    /// # Errors
    /// Malformed/unknown fields, unsupported version/name, invalid or
    /// duplicate plugin ids.
    pub fn from_toml(raw: &str) -> Result<Self, ConfigError> {
        let document: Self = toml::from_str(raw).map_err(|error| ConfigError::Parse {
            path: "<profile>".to_owned(),
            message: error.message().to_owned(),
        })?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !matches!(
            self.schema_version,
            PROFILE_SCHEMA_V1 | PROFILE_SCHEMA_V2 | PROFILE_SCHEMA_VERSION
        ) {
            return profile_error(format!(
                "unsupported profile schema {}; supported: {PROFILE_SCHEMA_V1}..={PROFILE_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.schema_version == PROFILE_SCHEMA_V1 && self.constraints.is_some() {
            return profile_error("managed profile constraints require profile schema 2");
        }
        if self.schema_version != PROFILE_SCHEMA_VERSION && self.code_authority.is_some() {
            return profile_error("managed code authority requires profile schema 3");
        }
        if let Some(constraints) = &self.constraints {
            constraints.validate()?;
        }
        if let Some(authority) = &self.code_authority {
            authority.validate()?;
        }
        if let Some(name) = &self.name {
            validate_plugin_id(name).map_err(|_| ConfigError::Parse {
                path: "<profile>".to_owned(),
                message: format!("invalid profile name `{name}`; expected lowercase kebab-case"),
            })?;
        }
        let mut seen = std::collections::BTreeSet::new();
        for row in &self.plugins {
            validate_plugin_id(&row.id)?;
            if !seen.insert(row.id.as_str()) {
                return profile_error(format!(
                    "plugin `{}` appears more than once in one profile document",
                    row.id
                ));
            }
        }
        Ok(())
    }

    fn directives(&self) -> Vec<PluginDirective> {
        self.plugins
            .iter()
            .map(|row| {
                if row.enabled {
                    PluginDirective::enable(&row.id)
                } else {
                    PluginDirective::disable(&row.id)
                }
            })
            .collect()
    }
}

/// Trusted metadata describing where a profile layer came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileSource {
    /// Compiled base profile.
    BuiltIn,
    /// User profile file.
    UserFile(PathBuf),
    /// Explicitly selected named profile in the user profile store.
    NamedProfile(PathBuf),
    /// Trusted project profile file.
    ProjectFile(PathBuf),
    /// Machine-local project profile file.
    LocalProjectFile(PathBuf),
    /// Process/session override label.
    Session(String),
    /// Administrator-managed policy label.
    Managed(String),
}

impl ProfileSource {
    /// User-file source.
    #[must_use]
    pub fn user(path: impl Into<PathBuf>) -> Self {
        Self::UserFile(path.into())
    }

    /// Named-profile-file source.
    #[must_use]
    pub fn named(path: impl Into<PathBuf>) -> Self {
        Self::NamedProfile(path.into())
    }

    /// Trusted project-file source.
    #[must_use]
    pub fn project(path: impl Into<PathBuf>) -> Self {
        Self::ProjectFile(path.into())
    }

    /// Local-project-file source.
    #[must_use]
    pub fn local_project(path: impl Into<PathBuf>) -> Self {
        Self::LocalProjectFile(path.into())
    }

    /// Session override source.
    #[must_use]
    pub fn session(label: impl Into<String>) -> Self {
        Self::Session(label.into())
    }

    /// Managed-policy source.
    #[must_use]
    pub fn managed(label: impl Into<String>) -> Self {
        Self::Managed(label.into())
    }

    /// Stable source kind without exposing path/label contents.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::BuiltIn => "built_in",
            Self::UserFile(_) => "user_file",
            Self::NamedProfile(_) => "named_profile",
            Self::ProjectFile(_) => "project_file",
            Self::LocalProjectFile(_) => "local_project_file",
            Self::Session(_) => "session_override",
            Self::Managed(_) => "managed_policy",
        }
    }

    const fn scope(&self) -> PluginScope {
        match self {
            Self::BuiltIn => PluginScope::BuiltIn,
            Self::UserFile(_) | Self::NamedProfile(_) => PluginScope::User,
            Self::ProjectFile(_) => PluginScope::Project,
            Self::LocalProjectFile(_) => PluginScope::LocalProject,
            Self::Session(_) => PluginScope::Session,
            Self::Managed(_) => PluginScope::Managed,
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::BuiltIn => Ok(()),
            Self::UserFile(path)
            | Self::NamedProfile(path)
            | Self::ProjectFile(path)
            | Self::LocalProjectFile(path)
                if path.as_os_str().is_empty() =>
            {
                profile_error(format!("{} profile path must not be empty", self.kind()))
            }
            Self::Session(label) | Self::Managed(label)
                if label.is_empty()
                    || label.trim() != label
                    || label.len() > 256
                    || label.chars().any(char::is_control) =>
            {
                profile_error(format!("{} label is invalid", self.kind()))
            }
            _ => Ok(()),
        }
    }
}

/// One validated document bound to trusted source/scope metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLayer {
    /// Activation scope.
    pub scope: PluginScope,
    /// Trusted source metadata.
    pub source: ProfileSource,
    /// Strict profile bytes projected into typed rows.
    pub document: ProfileDocument,
}

impl ProfileLayer {
    /// Bind a parsed document to its trusted discovery metadata.
    ///
    /// # Errors
    /// Source kind and requested scope mismatch.
    pub fn new(
        scope: PluginScope,
        source: ProfileSource,
        document: ProfileDocument,
    ) -> Result<Self, ConfigError> {
        source.validate()?;
        if (document.constraints.is_some() || document.code_authority.is_some())
            && scope != PluginScope::Managed
        {
            return profile_error(
                "profile constraints and code authority require managed policy authority",
            );
        }
        if source.scope() != scope || scope == PluginScope::BuiltIn {
            return profile_error(format!(
                "profile scope `{}` does not match source `{}`",
                scope.as_str(),
                source.kind()
            ));
        }
        Ok(Self {
            scope,
            source,
            document,
        })
    }
}

/// One source-aware layer in the effective tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProfileLayer {
    /// Layer scope.
    pub scope: PluginScope,
    /// Trusted config source.
    pub source: ProfileSource,
    /// Parsed schema version.
    pub schema_version: u32,
    /// Optional profile label.
    pub name: Option<String>,
}

/// One decision affecting a plugin id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDecision {
    /// Decision scope.
    pub scope: PluginScope,
    /// Exact source metadata.
    pub source: ProfileSource,
    /// Enable versus disable.
    pub enabled: bool,
}

/// One plugin id with complete decision history and final state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProfilePlugin {
    /// Stable plugin id.
    pub id: String,
    /// Final activation state.
    pub enabled: bool,
    /// Last decision scope.
    pub scope: PluginScope,
    /// Last decision source.
    pub source: ProfileSource,
    /// All decisions in precedence order.
    pub decisions: Vec<ProfileDecision>,
}

/// Enabled plugin row with its winning source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPluginSelection {
    /// Stable plugin id.
    pub id: String,
    /// Winning activation scope.
    pub scope: PluginScope,
    /// Winning source metadata.
    pub source: ProfileSource,
}

/// Inspectable source-aware profile resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProfileTree {
    /// Effective tree schema.
    pub schema_version: u32,
    /// Built-in and overlay layers in precedence order.
    pub layers: Vec<EffectiveProfileLayer>,
    /// Every mentioned plugin, including disabled rows.
    pub plugins: Vec<EffectiveProfilePlugin>,
    /// Enabled rows in exact composition order.
    pub enabled: Vec<SourcedPluginSelection>,
    /// Final managed admission rules, when configured.
    pub constraints: Option<ManagedProfileConstraints>,
    /// Final managed installed-code authority generation, when configured.
    pub code_authority: Option<ManagedCodeAuthorityConfig>,
}

impl EffectiveProfileTree {
    /// Admit fully constructed plugin implementations before any activation.
    ///
    /// # Errors
    /// A source or contribution family forbidden by managed policy fails with
    /// the exact plugin and safe policy identifier.
    pub fn admit_plugins(&self, plugins: &[heycode_core::ScopedPlugin]) -> Result<(), ConfigError> {
        let Some(constraints) = &self.constraints else {
            return Ok(());
        };
        for plugin in plugins {
            constraints.admit(plugin.plugin())?;
        }
        Ok(())
    }
}

/// Resolve strict profile layers into an inspectable source-aware tree.
///
/// # Errors
/// K05 scope/id conflicts or inconsistent layer metadata.
pub fn resolve_profile_tree(
    built_in: &[&str],
    layers: &[ProfileLayer],
) -> Result<EffectiveProfileTree, ConfigError> {
    let scope_layers: Vec<_> = layers
        .iter()
        .map(|layer| PluginScopeLayer::new(layer.scope, layer.document.directives()))
        .collect();
    let selected = resolve_scoped_plugins(built_in, &scope_layers)?;

    let mut ordered: Vec<&ProfileLayer> = layers.iter().collect();
    ordered.sort_by_key(|layer| layer.scope.precedence());
    let mut tree_layers = vec![EffectiveProfileLayer {
        scope: PluginScope::BuiltIn,
        source: ProfileSource::BuiltIn,
        schema_version: PROFILE_SCHEMA_VERSION,
        name: Some("built-in".to_owned()),
    }];
    tree_layers.extend(ordered.iter().map(|layer| EffectiveProfileLayer {
        scope: layer.scope,
        source: layer.source.clone(),
        schema_version: layer.document.schema_version,
        name: layer.document.name.clone(),
    }));

    let mut plugins = Vec::new();
    for id in built_in {
        plugins.push(EffectiveProfilePlugin {
            id: (*id).to_owned(),
            enabled: true,
            scope: PluginScope::BuiltIn,
            source: ProfileSource::BuiltIn,
            decisions: vec![ProfileDecision {
                scope: PluginScope::BuiltIn,
                source: ProfileSource::BuiltIn,
                enabled: true,
            }],
        });
    }
    for layer in &ordered {
        for row in &layer.document.plugins {
            let decision = ProfileDecision {
                scope: layer.scope,
                source: layer.source.clone(),
                enabled: row.enabled,
            };
            if let Some(plugin) = plugins.iter_mut().find(|plugin| plugin.id == row.id) {
                plugin.enabled = row.enabled;
                plugin.scope = layer.scope;
                plugin.source = layer.source.clone();
                plugin.decisions.push(decision);
            } else {
                plugins.push(EffectiveProfilePlugin {
                    id: row.id.clone(),
                    enabled: row.enabled,
                    scope: layer.scope,
                    source: layer.source.clone(),
                    decisions: vec![decision],
                });
            }
        }
    }

    let enabled = selected
        .into_iter()
        .map(|selection| sourced_selection(selection, &ordered))
        .collect::<Result<Vec<_>, _>>()?;
    let constraints = ordered
        .iter()
        .find(|layer| layer.scope == PluginScope::Managed)
        .and_then(|layer| layer.document.constraints.clone());
    let code_authority = ordered
        .iter()
        .find(|layer| layer.scope == PluginScope::Managed)
        .and_then(|layer| layer.document.code_authority.clone());
    Ok(EffectiveProfileTree {
        schema_version: PROFILE_SCHEMA_VERSION,
        layers: tree_layers,
        plugins,
        enabled,
        constraints,
        code_authority,
    })
}

fn sourced_selection(
    selection: EffectivePluginSelection,
    layers: &[&ProfileLayer],
) -> Result<SourcedPluginSelection, ConfigError> {
    let source = if selection.scope == PluginScope::BuiltIn {
        ProfileSource::BuiltIn
    } else {
        layers
            .iter()
            .find(|layer| layer.scope == selection.scope)
            .map(|layer| layer.source.clone())
            .ok_or_else(|| ConfigError::Parse {
                path: "<profile>".to_owned(),
                message: format!(
                    "effective plugin `{}` has no source for scope `{}`",
                    selection.id,
                    selection.scope.as_str()
                ),
            })?
    };
    Ok(SourcedPluginSelection {
        id: selection.id,
        scope: selection.scope,
        source,
    })
}

fn profile_error<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Parse {
        path: "<profile>".to_owned(),
        message: message.into(),
    })
}

fn constraint_error<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Parse {
        path: "<profile-constraints>".to_owned(),
        message: message.into(),
    })
}

//! Stable plugin identity and broad contribution metadata.

/// Where a plugin implementation came from.
///
/// Additional package/process/WASI sources arrive with the external plugin
/// loader; `Unclassified` exists only for the compatibility default while
/// built-ins migrate from the original name-only trait.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    /// Compiled into the heycode binary.
    BuiltIn,
    /// Legacy/test implementation that has not declared a descriptor yet.
    Unclassified,
}

impl PluginSource {
    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built_in",
            Self::Unclassified => "unclassified",
        }
    }
}

/// Activation scope of one plugin instance. Implementation provenance and
/// activation scope are independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginScope {
    /// Compiled default profile.
    BuiltIn,
    /// User-wide overlay.
    User,
    /// Trusted project overlay.
    Project,
    /// Machine-local project overlay.
    LocalProject,
    /// Process/session override.
    Session,
    /// Administrator-managed final constraint.
    Managed,
}

impl PluginScope {
    /// Stable low-to-high precedence rank.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::BuiltIn => 0,
            Self::User => 1,
            Self::Project => 2,
            Self::LocalProject => 3,
            Self::Session => 4,
            Self::Managed => 5,
        }
    }

    /// Stable diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built_in",
            Self::User => "user",
            Self::Project => "project",
            Self::LocalProject => "local_project",
            Self::Session => "session",
            Self::Managed => "managed",
        }
    }
}

/// Broad contribution families declared by a plugin descriptor.
///
/// These categories answer “what kind of capability can this plugin add?”
/// Exact named ownership (individual services/tools/commands) belongs to the
/// contribution inventory layered on top of descriptors.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginContributionKind {
    /// One or more context services.
    Service,
    /// One or more inference provider adapters.
    Provider,
    /// One or more model-callable tools.
    Tool,
    /// One or more human-only commands.
    Command,
    /// One or more deterministic prompt sections.
    PromptSection,
    /// One or more waterfall/interception contributions.
    Waterfall,
    /// A user-interface surface or renderer.
    UserInterface,
    /// A managed external child process.
    ExternalProcess,
    /// One or more redacted health/diagnostic checks.
    Diagnostic,
}

impl PluginContributionKind {
    /// Stable policy/configuration identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Provider => "provider",
            Self::Tool => "tool",
            Self::Command => "command",
            Self::PromptSection => "prompt_section",
            Self::Waterfall => "waterfall",
            Self::UserInterface => "user_interface",
            Self::ExternalProcess => "external_process",
            Self::Diagnostic => "diagnostic",
        }
    }
}

/// Stable metadata for one plugin implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginDescriptor {
    /// Stable kebab-case identity. During migration this must equal `name()`.
    pub id: &'static str,
    /// Plugin implementation version in semantic-version form.
    pub version: &'static str,
    /// Implementation provenance.
    pub source: PluginSource,
    /// Broad contribution families, in deterministic declaration order.
    pub contributions: &'static [PluginContributionKind],
}

/// Successfully applied implementation plus its winning activation scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedPlugin {
    /// Implementation metadata.
    pub descriptor: PluginDescriptor,
    /// Effective activation scope.
    pub scope: PluginScope,
}

impl PluginDescriptor {
    /// Descriptor for a plugin compiled into the binary.
    #[must_use]
    pub const fn built_in(
        id: &'static str,
        version: &'static str,
        contributions: &'static [PluginContributionKind],
    ) -> Self {
        Self {
            id,
            version,
            source: PluginSource::BuiltIn,
            contributions,
        }
    }

    /// Compatibility descriptor for legacy/test implementations.
    #[must_use]
    pub const fn unclassified(id: &'static str) -> Self {
        Self {
            id,
            version: "0.0.0-unclassified",
            source: PluginSource::Unclassified,
            contributions: &[],
        }
    }
}

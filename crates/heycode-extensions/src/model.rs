//! Validated, serializable plugin-manifest vocabulary.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::invalid;
use crate::{ApiVersion, ManifestError, PluginVersion};

/// Operating systems admitted by plugin manifest schema v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    /// Apple macOS.
    Macos,
    /// Linux distributions supported by the host.
    Linux,
    /// Microsoft Windows.
    Windows,
    /// FreeBSD.
    Freebsd,
}

impl OperatingSystem {
    /// Stable manifest identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Freebsd => "freebsd",
        }
    }
}

impl fmt::Display for OperatingSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// CPU architectures admitted by plugin manifest schema v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    /// 64-bit Arm.
    Aarch64,
    /// 64-bit x86.
    X86_64,
}

impl Architecture {
    /// Stable manifest identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }
}

impl fmt::Display for Architecture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One exact supported host target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformTarget {
    os: OperatingSystem,
    architecture: Architecture,
}

impl PlatformTarget {
    /// Construct an exact target from closed validated values.
    #[must_use]
    pub const fn new(os: OperatingSystem, architecture: Architecture) -> Self {
        Self { os, architecture }
    }

    /// Operating system.
    #[must_use]
    pub const fn os(&self) -> OperatingSystem {
        self.os
    }

    /// CPU architecture.
    #[must_use]
    pub const fn architecture(&self) -> Architecture {
        self.architecture
    }
}

/// Stable marketplace-namespaced plugin identity (`marketplace/plugin`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PluginId(String);

impl PluginId {
    /// Validate an external plugin id.
    ///
    /// # Errors
    /// The id must contain exactly two lowercase kebab-case segments separated
    /// by `/` and fit the stable length budget.
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        if !valid_plugin_id(&value) {
            return Err(invalid(
                "id",
                "must be marketplace/plugin with lowercase kebab-case segments",
            ));
        }
        Ok(Self(value))
    }

    /// Stable string identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Exact declarative contribution registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionKind {
    /// Lazy-loaded SKILL.md package.
    Skill,
    /// Human-only slash command declaration.
    Command,
    /// Agent preset.
    Agent,
    /// Typed lifecycle hook.
    Hook,
    /// Semantic terminal theme.
    Theme,
    /// Declarative inference provider route.
    Provider,
    /// Bundled MCP server definition.
    Mcp,
}

impl ContributionKind {
    /// Stable registry identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Agent => "agent",
            Self::Hook => "hook",
            Self::Theme => "theme",
            Self::Provider => "provider",
            Self::Mcp => "mcp",
        }
    }

    pub(crate) const fn directory(self) -> &'static str {
        match self {
            Self::Skill => "skills",
            Self::Command => "commands",
            Self::Agent => "agents",
            Self::Hook => "hooks",
            Self::Theme => "themes",
            Self::Provider => "providers",
            Self::Mcp => "mcp",
        }
    }
}

impl fmt::Display for ContributionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One public registry key used for collision and override policy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ContributionKey {
    kind: ContributionKind,
    name: String,
}

impl ContributionKey {
    /// Validate a registry key.
    ///
    /// # Errors
    /// Empty, untrimmed, control-bearing, or overlong names are rejected.
    pub fn new(kind: ContributionKind, name: impl Into<String>) -> Result<Self, ManifestError> {
        let name = name.into();
        if !valid_registry_name(&name) {
            return Err(invalid(
                "contribution.name",
                "must be trimmed, control-free, and at most 256 bytes",
            ));
        }
        Ok(Self { kind, name })
    }

    /// Exact registry kind.
    #[must_use]
    pub const fn kind(&self) -> ContributionKind {
        self.kind
    }

    /// Validated public name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Install/runtime grants requested by a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermission {
    /// Read files admitted by host policy.
    FilesystemRead,
    /// Write files admitted by host policy.
    FilesystemWrite,
    /// Make network requests admitted by host policy.
    NetworkAccess,
    /// Spawn an external process through the subprocess service.
    ProcessSpawn,
    /// Resolve declared credential references at operation time.
    CredentialUse,
    /// Connect to a declared MCP endpoint.
    McpConnect,
    /// Register typed event hooks.
    HookRegistration,
    /// Request an unnamespaced registry override.
    ContributionOverride,
}

impl PluginPermission {
    /// Stable manifest identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemRead => "filesystem_read",
            Self::FilesystemWrite => "filesystem_write",
            Self::NetworkAccess => "network_access",
            Self::ProcessSpawn => "process_spawn",
            Self::CredentialUse => "credential_use",
            Self::McpConnect => "mcp_connect",
            Self::HookRegistration => "hook_registration",
            Self::ContributionOverride => "contribution_override",
        }
    }
}

impl fmt::Display for PluginPermission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Host implementation selected for one manifest-declared code artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCodeRuntime {
    /// Native executable launched through the composed exact-image process owner.
    NativeProcess,
    /// WebAssembly Component Model artifact implementing the pinned WIT-v1 world.
    WasiComponentV1,
}

impl PluginCodeRuntime {
    /// Stable manifest identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeProcess => "native_process",
            Self::WasiComponentV1 => "wasi_component_v1",
        }
    }
}

impl fmt::Display for PluginCodeRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Strict manifest-owned runtime and artifact entrypoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginCode {
    runtime: PluginCodeRuntime,
    entrypoint: PluginPath,
}

impl PluginCode {
    pub(crate) const fn new(runtime: PluginCodeRuntime, entrypoint: PluginPath) -> Self {
        Self {
            runtime,
            entrypoint,
        }
    }

    /// Exact host runtime selected by the package manifest.
    #[must_use]
    pub const fn runtime(&self) -> PluginCodeRuntime {
        self.runtime
    }

    /// Exact package-relative artifact path selected by the manifest.
    #[must_use]
    pub const fn entrypoint(&self) -> &PluginPath {
        &self.entrypoint
    }
}

/// Inclusive host API compatibility range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PluginApiRange {
    /// Minimum compatible host API.
    pub minimum: ApiVersion,
    /// Maximum compatible host API.
    pub maximum: ApiVersion,
}

/// Validated relative path within an installed plugin root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PluginPath(String);

impl PluginPath {
    /// Parse one portable package-relative path for a host policy row.
    ///
    /// # Errors
    /// Empty, absolute, traversing, platform-specific or otherwise
    /// non-portable paths are rejected.
    pub fn parse(value: impl Into<String>) -> Result<Self, ManifestError> {
        Self::new("path", value.into())
    }

    pub(crate) fn new(field: &'static str, value: String) -> Result<Self, ManifestError> {
        if !valid_relative_path(&value) {
            return Err(invalid(
                field,
                "must be a normalized relative plugin path without traversal",
            ));
        }
        Ok(Self(value))
    }

    /// Slash-separated path relative to the installed plugin root.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Declared dependency version bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginDependency {
    /// Namespaced dependency id.
    pub id: PluginId,
    /// Inclusive lower version bound.
    pub minimum_version: PluginVersion,
    /// Optional exclusive upper bound.
    pub maximum_version_exclusive: Option<PluginVersion>,
    /// Whether activation may proceed without the dependency.
    pub optional: bool,
}

/// Authentication policy for this package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationPolicy {
    /// The plugin declares no authentication records.
    None,
    /// The plugin remains useful without its declared records.
    Optional,
    /// Activation requires all declared records.
    Required,
}

/// One non-secret credential binding required by a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialRequirement {
    /// Stable non-secret reference resolved by the credentials service.
    pub reference: String,
    /// Semantic credential kind, such as `api-key` or `oauth-token`.
    pub kind: String,
}

/// Secret-free authentication declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginAuthentication {
    /// Required/optional/none behavior.
    pub policy: AuthenticationPolicy,
    /// Non-secret references only; the manifest cannot represent values.
    pub credential_references: Vec<CredentialRequirement>,
}

/// Origin kind asserted by the plugin package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSourceKind {
    /// Relative local directory source.
    Local,
    /// Git repository source.
    Git,
    /// HTTPS archive source.
    Https,
    /// Supported package registry locator.
    Registry,
    /// Marketplace-local catalog locator.
    Marketplace,
}

/// Public detached signature algorithm supported by manifest v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    /// Ed25519 detached signature.
    Ed25519,
}

/// Validated public detached signature metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginSignature {
    /// Signature algorithm.
    pub algorithm: SignatureAlgorithm,
    /// Public verification-key identifier.
    pub key_id: String,
    /// Base64-encoded public signature bytes.
    pub value: String,
}

/// Update behavior requested by the package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Stable releases.
    Stable,
    /// Preview releases.
    Preview,
    /// Never advance beyond the declared revision/checksum.
    Pinned,
}

/// Secret-free source/provenance declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginSource {
    /// Source kind.
    pub kind: PluginSourceKind,
    /// Validated source locator with URL credentials and query strings denied.
    pub locator: String,
    /// Optional immutable or named source revision.
    pub revision: Option<String>,
    /// Optional lowercase SHA-256 digest (`sha256:<64 hex>`).
    pub checksum: Option<String>,
    /// Optional detached public signature.
    pub signature: Option<PluginSignature>,
    /// Requested update channel.
    pub update_channel: UpdateChannel,
}

/// Public-name exposure policy for a contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ContributionExposure {
    /// Public name is deterministically `<plugin-id>::<local-id>`.
    Namespaced,
    /// Public name intentionally targets a host-approved registry row.
    Override {
        /// Requested unnamespaced name.
        name: String,
    },
}

/// One validated declarative plugin contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginContribution {
    kind: ContributionKind,
    local_id: String,
    path: PluginPath,
    exposure: ContributionExposure,
    public_name: String,
}

impl PluginContribution {
    pub(crate) fn new(
        kind: ContributionKind,
        local_id: String,
        path: PluginPath,
        exposure: ContributionExposure,
        public_name: String,
    ) -> Self {
        Self {
            kind,
            local_id,
            path,
            exposure,
            public_name,
        }
    }

    /// Exact registry kind.
    #[must_use]
    pub const fn kind(&self) -> ContributionKind {
        self.kind
    }

    /// Plugin-local kebab-case id.
    #[must_use]
    pub fn local_id(&self) -> &str {
        &self.local_id
    }

    /// Validated relative contribution path.
    #[must_use]
    pub const fn path(&self) -> &PluginPath {
        &self.path
    }

    /// Explicit namespaced or override declaration.
    #[must_use]
    pub const fn exposure(&self) -> &ContributionExposure {
        &self.exposure
    }

    /// Effective public registry name.
    #[must_use]
    pub fn public_name(&self) -> &str {
        &self.public_name
    }

    pub(crate) fn key(&self) -> ContributionKey {
        ContributionKey {
            kind: self.kind,
            name: self.public_name.clone(),
        }
    }
}

/// Fully validated plugin manifest v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginManifest {
    schema_version: u32,
    id: PluginId,
    name: String,
    version: PluginVersion,
    description: String,
    license: String,
    api: PluginApiRange,
    contributions: Vec<PluginContribution>,
    configuration_schema: Option<PluginPath>,
    requested_permissions: Vec<PluginPermission>,
    default_enabled: bool,
    platforms: Vec<PlatformTarget>,
    source: PluginSource,
    dependencies: Vec<PluginDependency>,
    conflicts: Vec<PluginId>,
    authentication: PluginAuthentication,
    code: Option<PluginCode>,
}

impl PluginManifest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        schema_version: u32,
        id: PluginId,
        name: String,
        version: PluginVersion,
        description: String,
        license: String,
        api: PluginApiRange,
        contributions: Vec<PluginContribution>,
        configuration_schema: Option<PluginPath>,
        requested_permissions: Vec<PluginPermission>,
        default_enabled: bool,
        platforms: Vec<PlatformTarget>,
        source: PluginSource,
        dependencies: Vec<PluginDependency>,
        conflicts: Vec<PluginId>,
        authentication: PluginAuthentication,
        code: Option<PluginCode>,
    ) -> Self {
        Self {
            schema_version,
            id,
            name,
            version,
            description,
            license,
            api,
            contributions,
            configuration_schema,
            requested_permissions,
            default_enabled,
            platforms,
            source,
            dependencies,
            conflicts,
            authentication,
            code,
        }
    }

    /// Exact manifest schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Stable marketplace-namespaced plugin id.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// Human-readable package name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Semantic package version.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        &self.version
    }

    /// Human-readable package description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Validated SPDX-shaped license expression.
    #[must_use]
    pub fn license(&self) -> &str {
        &self.license
    }

    /// Host API compatibility range.
    #[must_use]
    pub const fn api(&self) -> PluginApiRange {
        self.api
    }

    /// Ordered exact contribution declarations.
    #[must_use]
    pub fn contributions(&self) -> &[PluginContribution] {
        &self.contributions
    }

    /// Optional relative JSON configuration schema.
    #[must_use]
    pub fn configuration_schema(&self) -> Option<&PluginPath> {
        self.configuration_schema.as_ref()
    }

    /// Ordered unique requested permissions.
    #[must_use]
    pub fn permissions(&self) -> &[PluginPermission] {
        &self.requested_permissions
    }

    /// Whether a successful install requests activation by default.
    #[must_use]
    pub const fn default_enabled(&self) -> bool {
        self.default_enabled
    }

    /// Explicit supported host targets.
    #[must_use]
    pub fn platforms(&self) -> &[PlatformTarget] {
        &self.platforms
    }

    /// Secret-free source/provenance metadata.
    #[must_use]
    pub const fn source(&self) -> &PluginSource {
        &self.source
    }

    /// Ordered unique dependency bounds.
    #[must_use]
    pub fn dependencies(&self) -> &[PluginDependency] {
        &self.dependencies
    }

    /// Ordered unique conflicting package ids.
    #[must_use]
    pub fn conflicts(&self) -> &[PluginId] {
        &self.conflicts
    }

    /// Secret-free authentication policy and references.
    #[must_use]
    pub const fn authentication(&self) -> &PluginAuthentication {
        &self.authentication
    }

    /// Optional authoritative runtime and entrypoint for installed code.
    #[must_use]
    pub const fn code(&self) -> Option<&PluginCode> {
        self.code.as_ref()
    }
}

pub(crate) fn valid_kebab(value: &str) -> bool {
    value.len() <= 64
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.contains("--")
}

pub(crate) fn valid_plugin_id(value: &str) -> bool {
    if value.len() > 129 {
        return false;
    }
    let mut segments = value.split('/');
    matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some(namespace), Some(package), None) if valid_kebab(namespace) && valid_kebab(package)
    )
}

pub(crate) fn valid_registry_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

pub(crate) fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.trim() == value
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && value.split('/').all(valid_portable_path_component)
}

fn valid_portable_path_component(component: &str) -> bool {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.len() > 255
        || component.ends_with([' ', '.'])
        || component
            .chars()
            .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return false;
    }

    let device_stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_uppercase();
    !matches!(device_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !is_windows_numbered_device(&device_stem)
}

fn is_windows_numbered_device(stem: &str) -> bool {
    let Some(suffix) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut characters = suffix.chars();
    characters.next().is_some_and(|digit| {
        characters.next().is_none()
            && matches!(
                digit,
                '0'..='9' | '⁰' | '¹' | '²' | '³' | '⁴' | '⁵' | '⁶' | '⁷' | '⁸' | '⁹'
            )
    })
}

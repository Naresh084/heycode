//! Provider-owned Token Plan MCP installation and policy facts.
//!
//! MiniMax's current Token Plan MCP guide documents both `web_search` and
//! `understand_image`. The bundle exposes both under its all-documented policy
//! and supports an explicit web-only least-privilege selection. Both tools
//! return untrusted MCP data and remain approval-gated.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heycode_core::{ToolSpec, UntrustedContentBoundary};
use heycode_credentials::CredentialQuery;

use crate::{MiniMaxProfile, MiniMaxRegion, TokenPlan};

const SERVER_ID: &str = "minimax-token-plan";
const DISPLAY_NAME: &str = "MiniMax Token Plan MCP";
const COMMAND: &str = "uvx";
const ARGUMENTS: &[&str] = &["minimax-coding-plan-mcp", "-y"];

/// Effect-owned aggregate bundle plugin id.
pub const MINIMAX_TOKEN_PLAN_MCP_PLUGIN_ID: &str = "mcp-minimax-token-plan";

/// Which currently documented Token Plan MCP tools may be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniMaxImageUnderstandingPolicy {
    /// Install both currently documented tools.
    AllDocumented,
    /// Install only web search as an explicit least-privilege restriction.
    WebSearchOnly,
}

/// Documentary strength behind one bundled tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniMaxMcpFeatureEvidence {
    /// Present in MiniMax's current indexed Token Plan MCP guide.
    Current,
}

/// Approval policy a concrete MCP definition must apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniMaxMcpApproval {
    /// Ask at call time before sending user data to the external server.
    Prompt,
    /// Refuse the tool regardless of server annotations.
    Deny,
}

/// Provider-owned installation scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniMaxMcpInstallScope {
    /// MiniMax's documented Claude command installs the server at user scope.
    User,
}

/// Non-tool MCP surfaces exposed by this bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MiniMaxMcpExposurePolicy {
    /// Whether resources may be consumed.
    pub resources: bool,
    /// Whether prompts may be consumed.
    pub prompts: bool,
    /// Whether server instructions may enter a prompt.
    pub instructions: bool,
}

impl MiniMaxMcpExposurePolicy {
    /// Disable every non-tool surface.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            resources: false,
            prompts: false,
            instructions: false,
        }
    }
}

/// Resource delivery mode passed to the MiniMax MCP child.
#[derive(Clone, PartialEq, Eq)]
pub enum MiniMaxMcpResourceMode {
    /// Server returns URLs; MiniMax documents this as the default.
    Url,
    /// Server may write resources beneath one explicitly selected directory.
    Local {
        /// Canonical existing directory; omitted from `Debug`.
        root: PathBuf,
    },
}

impl MiniMaxMcpResourceMode {
    /// Validate and canonicalize one local output directory.
    ///
    /// # Errors
    /// Relative, missing, non-directory, non-UTF-8 or read-only roots fail
    /// without exposing the path through the error.
    pub fn local(root: impl AsRef<Path>) -> Result<Self, MiniMaxMcpBundleError> {
        let root = root.as_ref();
        if !root.is_absolute() {
            return Err(MiniMaxMcpBundleError::InvalidLocalRoot);
        }
        let canonical =
            std::fs::canonicalize(root).map_err(|_| MiniMaxMcpBundleError::InvalidLocalRoot)?;
        let metadata =
            std::fs::metadata(&canonical).map_err(|_| MiniMaxMcpBundleError::InvalidLocalRoot)?;
        if !metadata.is_dir() || metadata.permissions().readonly() || canonical.to_str().is_none() {
            return Err(MiniMaxMcpBundleError::InvalidLocalRoot);
        }
        Ok(Self::Local { root: canonical })
    }

    /// Canonical local root, when local delivery was explicitly selected.
    #[must_use]
    pub fn local_root(&self) -> Option<&Path> {
        match self {
            Self::Url => None,
            Self::Local { root } => Some(root),
        }
    }
}

impl fmt::Debug for MiniMaxMcpResourceMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url => formatter.write_str("Url"),
            Self::Local { .. } => formatter.write_str("Local { root: [REDACTED] }"),
        }
    }
}

/// Exact environment source without a credential value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiniMaxMcpEnvironmentValue {
    /// Reviewed non-secret literal.
    Literal(String),
    /// Credential lookup performed only by the launch owner.
    Credential(CredentialQuery),
}

/// One exact bundled tool plus provider policy.
#[derive(Debug, Clone, PartialEq)]
pub struct MiniMaxMcpTool {
    spec: ToolSpec,
    evidence: MiniMaxMcpFeatureEvidence,
    approval: MiniMaxMcpApproval,
    untrusted_content: UntrustedContentBoundary,
}

impl MiniMaxMcpTool {
    /// Model-facing tool schema documented by MiniMax.
    #[must_use]
    pub const fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Evidence behind this tool's inclusion.
    #[must_use]
    pub const fn evidence(&self) -> MiniMaxMcpFeatureEvidence {
        self.evidence
    }

    /// Required approval policy.
    #[must_use]
    pub const fn approval(&self) -> MiniMaxMcpApproval {
        self.approval
    }

    /// External-data boundary applied after durable MCP result admission.
    #[must_use]
    pub const fn untrusted_content(&self) -> &UntrustedContentBoundary {
        &self.untrusted_content
    }
}

/// Provider-owned Token Plan MCP installation facts.
///
/// This type accepts only [`TokenPlan`]. A pay-as-you-go profile cannot be
/// passed accidentally:
///
/// ```compile_fail
/// use heycode_provider_minimax::{
///     MiniMaxImageUnderstandingPolicy, MiniMaxProfile,
///     MiniMaxTokenPlanMcpBundle, PayAsYouGo,
/// };
/// let _ = MiniMaxTokenPlanMcpBundle::new(
///     MiniMaxProfile::<PayAsYouGo>::international(),
///     MiniMaxImageUnderstandingPolicy::AllDocumented,
/// );
/// ```
#[derive(Clone, PartialEq)]
pub struct MiniMaxTokenPlanMcpBundle {
    profile: MiniMaxProfile<TokenPlan>,
    credential: CredentialQuery,
    resource_mode: MiniMaxMcpResourceMode,
    tools: Vec<MiniMaxMcpTool>,
}

/// Exact local launch after the host resolved executable and cwd authority.
#[derive(Clone, PartialEq)]
pub struct MiniMaxMcpLaunch {
    executable: PathBuf,
    cwd: PathBuf,
    environment: BTreeMap<String, MiniMaxMcpEnvironmentValue>,
}

impl MiniMaxMcpLaunch {
    /// Canonical executable path; values are never included in `Debug`.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Canonical process working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Exact documented arguments.
    #[must_use]
    pub const fn arguments(&self) -> &'static [&'static str] {
        ARGUMENTS
    }

    /// Literal/reference-only environment.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, MiniMaxMcpEnvironmentValue> {
        &self.environment
    }
}

impl fmt::Debug for MiniMaxMcpLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiniMaxMcpLaunch")
            .field("executable", &"[REDACTED]")
            .field("cwd", &"[REDACTED]")
            .field("arguments", &ARGUMENTS.len())
            .field("environment_variables", &self.environment.len())
            .finish()
    }
}

impl MiniMaxTokenPlanMcpBundle {
    /// Build the documented international Token Plan MCP bundle.
    ///
    /// # Errors
    /// MiniMax currently documents no mainland MCP host, and a malformed
    /// built-in credential identity fails closed.
    pub fn new(
        profile: MiniMaxProfile<TokenPlan>,
        image_policy: MiniMaxImageUnderstandingPolicy,
    ) -> Result<Self, MiniMaxMcpBundleError> {
        if profile.region() != MiniMaxRegion::International {
            return Err(MiniMaxMcpBundleError::UnsupportedRegion);
        }
        let credential = profile
            .credential_query()
            .map_err(|_| MiniMaxMcpBundleError::InvalidCredentialIdentity)?;
        let mut tools = vec![web_search_tool()];
        if image_policy == MiniMaxImageUnderstandingPolicy::AllDocumented {
            tools.push(understand_image_tool());
        }
        Ok(Self {
            profile,
            credential,
            resource_mode: MiniMaxMcpResourceMode::Url,
            tools,
        })
    }

    /// Replace URL delivery with one already-validated resource mode.
    #[must_use]
    pub fn with_resource_mode(mut self, resource_mode: MiniMaxMcpResourceMode) -> Self {
        self.resource_mode = resource_mode;
        self
    }

    /// Stable server id for a concrete MCP definition.
    #[must_use]
    pub const fn server_id(&self) -> &'static str {
        SERVER_ID
    }

    /// Human display name.
    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    /// Documented user installation scope.
    #[must_use]
    pub const fn scope(&self) -> MiniMaxMcpInstallScope {
        MiniMaxMcpInstallScope::User
    }

    /// Command resolved by the concrete process host at activation time.
    #[must_use]
    pub const fn command(&self) -> &'static str {
        COMMAND
    }

    /// Exact package/uvx arguments.
    #[must_use]
    pub const fn arguments(&self) -> &'static [&'static str] {
        ARGUMENTS
    }

    /// A missing optional bundle must not block the entire profile.
    #[must_use]
    pub const fn required(&self) -> bool {
        false
    }

    /// Non-tool surfaces remain disabled.
    #[must_use]
    pub const fn exposure(&self) -> MiniMaxMcpExposurePolicy {
        MiniMaxMcpExposurePolicy::none()
    }

    /// Explicit resource delivery policy.
    #[must_use]
    pub const fn resource_mode(&self) -> &MiniMaxMcpResourceMode {
        &self.resource_mode
    }

    /// Exact allowlisted tool rows.
    #[must_use]
    pub fn tools(&self) -> &[MiniMaxMcpTool] {
        &self.tools
    }

    /// Approval for a name after applying the exact allowlist.
    #[must_use]
    pub fn approval_for(&self, name: &str) -> MiniMaxMcpApproval {
        self.tools
            .iter()
            .find(|tool| tool.spec.name == name)
            .map_or(MiniMaxMcpApproval::Deny, MiniMaxMcpTool::approval)
    }

    /// Exact environment sources for the concrete stdio definition.
    #[must_use]
    pub fn environment(&self) -> BTreeMap<String, MiniMaxMcpEnvironmentValue> {
        let mut environment = BTreeMap::from([
            (
                "MINIMAX_API_HOST".to_owned(),
                MiniMaxMcpEnvironmentValue::Literal("https://api.minimax.io".to_owned()),
            ),
            (
                "MINIMAX_API_KEY".to_owned(),
                MiniMaxMcpEnvironmentValue::Credential(self.credential.clone()),
            ),
        ]);
        match &self.resource_mode {
            MiniMaxMcpResourceMode::Url => {
                environment.insert(
                    "MINIMAX_API_RESOURCE_MODE".to_owned(),
                    MiniMaxMcpEnvironmentValue::Literal("url".to_owned()),
                );
            }
            MiniMaxMcpResourceMode::Local { root } => {
                environment.insert(
                    "MINIMAX_API_RESOURCE_MODE".to_owned(),
                    MiniMaxMcpEnvironmentValue::Literal("local".to_owned()),
                );
                environment.insert(
                    "MINIMAX_MCP_BASE_PATH".to_owned(),
                    MiniMaxMcpEnvironmentValue::Literal(root.to_string_lossy().into_owned()),
                );
            }
        }
        environment
    }

    /// Resolve executable and cwd authority into one exact launch description.
    ///
    /// No credential is resolved: the environment still carries the original
    /// [`CredentialQuery`] and the ordinary MCP connection owner resolves it
    /// only when the child is launched.
    ///
    /// # Errors
    /// The executable must resolve to an executable regular file and cwd to a
    /// canonical directory.
    pub fn resolve_launch(
        &self,
        executable: impl AsRef<Path>,
        cwd: impl AsRef<Path>,
    ) -> Result<MiniMaxMcpLaunch, MiniMaxMcpBundleError> {
        Ok(MiniMaxMcpLaunch {
            executable: canonical_executable(executable.as_ref())?,
            cwd: canonical_directory(cwd.as_ref())?,
            environment: self.environment(),
        })
    }
}

impl fmt::Debug for MiniMaxTokenPlanMcpBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiniMaxTokenPlanMcpBundle")
            .field("plan", &self.profile.plan())
            .field("region", &self.profile.region())
            .field("resource_mode", &self.resource_mode)
            .field("tools", &self.tools.len())
            .finish()
    }
}

/// Safe bundle-construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MiniMaxMcpBundleError {
    /// The official bundle is not documented for this region.
    #[error("MiniMax does not document the Token Plan MCP server for this region")]
    UnsupportedRegion,
    /// Local resource delivery did not name one usable directory.
    #[error("MiniMax MCP local resource delivery requires an existing absolute writable directory")]
    InvalidLocalRoot,
    /// A built-in credential reference/kind failed its shared validator.
    #[error("MiniMax Token Plan MCP credential identity is invalid")]
    InvalidCredentialIdentity,
    /// Launch executable was not an absolute executable regular file.
    #[error("MiniMax MCP launch executable is invalid")]
    InvalidExecutable,
    /// Launch cwd was not an absolute directory.
    #[error("MiniMax MCP launch working directory is invalid")]
    InvalidWorkingDirectory,
}

fn canonical_executable(path: &Path) -> Result<PathBuf, MiniMaxMcpBundleError> {
    if !path.is_absolute() {
        return Err(MiniMaxMcpBundleError::InvalidExecutable);
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| MiniMaxMcpBundleError::InvalidExecutable)?;
    let metadata =
        std::fs::metadata(&canonical).map_err(|_| MiniMaxMcpBundleError::InvalidExecutable)?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(MiniMaxMcpBundleError::InvalidExecutable);
    }
    Ok(canonical)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, MiniMaxMcpBundleError> {
    if !path.is_absolute() {
        return Err(MiniMaxMcpBundleError::InvalidWorkingDirectory);
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| MiniMaxMcpBundleError::InvalidWorkingDirectory)?;
    if !std::fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(MiniMaxMcpBundleError::InvalidWorkingDirectory);
    }
    Ok(canonical)
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Host adapter implemented by the product composition layer that can see the
/// shared MCP connection owner without introducing an upward crate dependency.
pub trait MiniMaxTokenPlanMcpHost: Send + Sync {
    /// Services the adapter resolves during activation.
    fn required_services(&self) -> &'static [heycode_core::ServiceKey];

    /// Broad descriptor families covering [`Self::inventory`] rows.
    fn descriptor_families(&self) -> &'static [heycode_core::PluginContributionKind];

    /// Exact inventory rows contributed by this bundle.
    fn inventory(
        &self,
        _bundle: &MiniMaxTokenPlanMcpBundle,
    ) -> Vec<heycode_core::PluginContributionSpec> {
        Vec::new()
    }

    /// Convert and effect-register the bundle through the ordinary MCP owner.
    ///
    /// # Errors
    /// Duplicate, unavailable or invalid shared product state.
    fn register(
        &self,
        context: &heycode_core::Context,
        bundle: &MiniMaxTokenPlanMcpBundle,
    ) -> Result<(), MiniMaxMcpHostFailure>;
}

/// Build the Token Plan MCP aggregate plugin.
#[must_use]
pub fn minimax_token_plan_mcp_bundle_plugin(
    bundle: MiniMaxTokenPlanMcpBundle,
    host: Arc<dyn MiniMaxTokenPlanMcpHost>,
) -> Box<dyn heycode_core::Plugin> {
    Box::new(MiniMaxTokenPlanMcpPlugin { bundle, host })
}

struct MiniMaxTokenPlanMcpPlugin {
    bundle: MiniMaxTokenPlanMcpBundle,
    host: Arc<dyn MiniMaxTokenPlanMcpHost>,
}

impl heycode_core::Plugin for MiniMaxTokenPlanMcpPlugin {
    fn name(&self) -> &'static str {
        MINIMAX_TOKEN_PLAN_MCP_PLUGIN_ID
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            self.name(),
            env!("CARGO_PKG_VERSION"),
            self.host.descriptor_families(),
        )
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        self.host.required_services()
    }

    fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
        self.host.inventory(&self.bundle)
    }

    fn apply(&self, context: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
        self.host.register(context, &self.bundle).map_err(|error| {
            heycode_core::CoreError::other(format!("MiniMax Token Plan MCP: {error}"))
        })
    }
}

/// Closed body-free host registration failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniMaxMcpHostFailure {
    /// Server id is already live.
    Duplicate,
    /// Required registry, executable or policy is unavailable.
    Unavailable,
    /// Root conversion rejected the provider-owned specification.
    Invalid,
}

impl std::fmt::Display for MiniMaxMcpHostFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Duplicate => "server is already registered",
            Self::Unavailable => "MCP activation dependency is unavailable",
            Self::Invalid => "bundle specification is invalid",
        })
    }
}

impl std::error::Error for MiniMaxMcpHostFailure {}

fn web_search_tool() -> MiniMaxMcpTool {
    MiniMaxMcpTool {
        spec: ToolSpec {
            name: "web_search".to_owned(),
            description: "Search the public web for the supplied query.".to_owned(),
            parameters: serde_json::json!({
                "type":"object",
                "properties":{"query":{"type":"string"}},
                "required":["query"],
                "additionalProperties":false
            }),
        },
        evidence: MiniMaxMcpFeatureEvidence::Current,
        approval: MiniMaxMcpApproval::Prompt,
        untrusted_content: UntrustedContentBoundary::mcp(),
    }
}

fn understand_image_tool() -> MiniMaxMcpTool {
    MiniMaxMcpTool {
        spec: ToolSpec {
            name: "understand_image".to_owned(),
            description: "Analyze an image from a URL or explicitly permitted local path."
                .to_owned(),
            parameters: serde_json::json!({
                "type":"object",
                "properties":{
                    "prompt":{"type":"string"},
                    "image_url":{"type":"string"}
                },
                "required":["prompt","image_url"],
                "additionalProperties":false
            }),
        },
        evidence: MiniMaxMcpFeatureEvidence::Current,
        approval: MiniMaxMcpApproval::Prompt,
        untrusted_content: UntrustedContentBoundary::mcp(),
    }
}

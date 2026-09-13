//! PZA05 host-neutral GLM Coding Plan MCP bundle.
//!
//! Three servers are documented Streamable HTTP endpoints authenticated with
//! the Coding Plan key. Vision is a local stdio package launched through
//! `npx`, requiring Node.js 22+, `@z_ai/mcp-server` 0.1.2 or newer,
//! `Z_AI_API_KEY` and `Z_AI_MODE=ZAI`. This module keeps those facts as
//! secret-free provider-owned specifications and dispatches them through one
//! effect-owned host bridge.
//!
//! It deliberately does not depend on `heycode-mcp`: AGENTS' dependency table
//! keeps this provider below the shared registry. The composition root, which
//! may know both crates, maps [`ZaiMcpServerSpec`] into exact MCP definitions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginContributionSpec, PluginDescriptor,
    ServiceKey,
};
use heycode_credentials::CredentialQuery;

use crate::{Coding, ZAI_CODING_API_KEY_REFERENCE, ZaiCredential};

/// Coding Plan Web Search Streamable HTTP endpoint.
pub const ZAI_MCP_SEARCH_ENDPOINT: &str = "https://api.z.ai/api/mcp/web_search_prime/mcp";
/// Coding Plan Web Reader Streamable HTTP endpoint.
pub const ZAI_MCP_READER_ENDPOINT: &str = "https://api.z.ai/api/mcp/web_reader/mcp";
/// Coding Plan Zread Streamable HTTP endpoint.
pub const ZAI_MCP_ZREAD_ENDPOINT: &str = "https://api.z.ai/api/mcp/zread/mcp";
/// Local vision MCP package.
pub const ZAI_MCP_VISION_PACKAGE: &str = "@z_ai/mcp-server";
/// Minimum package version Z.AI documents for current vision capability.
pub const ZAI_MCP_VISION_MINIMUM_VERSION: &str = "0.1.2";
/// Minimum Node.js major version documented by Z.AI.
pub const ZAI_MCP_VISION_MINIMUM_NODE_MAJOR: u16 = 22;
/// Static aggregate bridge plugin id.
pub const ZAI_CODING_MCP_PLUGIN_ID: &str = "mcp-zai-coding";

const VISION_TOOLS: [&str; 8] = [
    "ui_to_artifact",
    "extract_text_from_screenshot",
    "diagnose_error_screenshot",
    "understand_technical_diagram",
    "analyze_data_visualization",
    "ui_diff_check",
    "image_analysis",
    "video_analysis",
];

/// Action-time policy a root adapter must preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiMcpApproval {
    /// Ask before sending arguments to an external server.
    Prompt,
}

/// Non-tool surfaces exposed by one Coding Plan bundle server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZaiMcpExposurePolicy {
    /// Resource access.
    pub resources: bool,
    /// Prompt access.
    pub prompts: bool,
    /// Server instructions entering model context.
    pub instructions: bool,
}

impl ZaiMcpExposurePolicy {
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

/// Secret-free bearer-header binding for one remote server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiMcpAuthorization {
    credential_query: CredentialQuery,
}

impl ZaiMcpAuthorization {
    /// Non-secret Coding Plan credential reference.
    #[must_use]
    pub const fn credential_reference(&self) -> &'static str {
        ZAI_CODING_API_KEY_REFERENCE
    }

    /// Exact safe query resolved only by the connection operation.
    #[must_use]
    pub const fn credential_query(&self) -> &CredentialQuery {
        &self.credential_query
    }

    /// Documented HTTP authentication scheme.
    #[must_use]
    pub const fn scheme(&self) -> &'static str {
        "Bearer"
    }

    /// Header name the root adapter binds by reference.
    #[must_use]
    pub const fn header_name(&self) -> &'static str {
        "authorization"
    }
}

/// Secret-free environment credential binding for the local vision server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiMcpCredentialEnvironment {
    name: &'static str,
    credential_query: CredentialQuery,
}

impl ZaiMcpCredentialEnvironment {
    /// Child environment variable name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Non-secret Coding Plan credential reference.
    #[must_use]
    pub const fn credential_reference(&self) -> &'static str {
        ZAI_CODING_API_KEY_REFERENCE
    }

    /// Exact safe query resolved only at process launch.
    #[must_use]
    pub const fn credential_query(&self) -> &CredentialQuery {
        &self.credential_query
    }
}

/// Provider-owned transport specification awaiting root conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZaiMcpTransportSpec {
    /// Remote Streamable HTTP MCP server.
    StreamableHttp {
        /// Exact documented endpoint.
        endpoint: &'static str,
        /// Bearer credential reference, never a value.
        authorization: ZaiMcpAuthorization,
    },
    /// Local stdio server distributed as an npm package and launched by npx.
    Npx {
        /// Exact package name without an unpinned `latest` suffix.
        package: &'static str,
        /// Minimum version Z.AI requires for current tools.
        minimum_package_version: &'static str,
        /// Minimum Node.js major version.
        minimum_node_major: u16,
        /// Credential reference mapped into the child environment.
        credential_environment: ZaiMcpCredentialEnvironment,
        /// Public non-secret fixed environment.
        static_environment: &'static [(&'static str, &'static str)],
    },
}

/// One official Coding Plan MCP server definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiMcpServerSpec {
    id: &'static str,
    display_name: &'static str,
    transport: ZaiMcpTransportSpec,
    tools: &'static [&'static str],
    required: bool,
    default_approval: ZaiMcpApproval,
    exposure: ZaiMcpExposurePolicy,
}

/// Exact vision-server launch after host executable/version resolution.
#[derive(Clone, PartialEq, Eq)]
pub struct ZaiVisionMcpLaunch {
    executable: PathBuf,
    cwd: PathBuf,
    arguments: Vec<String>,
    credential_environment: ZaiMcpCredentialEnvironment,
    static_environment: &'static [(&'static str, &'static str)],
}

impl ZaiVisionMcpLaunch {
    /// Canonical `npx` executable.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Canonical process cwd.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Exact arguments with the observed acceptable package version pinned.
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Operation-time credential environment binding.
    #[must_use]
    pub const fn credential_environment(&self) -> &ZaiMcpCredentialEnvironment {
        &self.credential_environment
    }

    /// Reviewed public environment.
    #[must_use]
    pub const fn static_environment(&self) -> &'static [(&'static str, &'static str)] {
        self.static_environment
    }
}

impl std::fmt::Debug for ZaiVisionMcpLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ZaiVisionMcpLaunch")
            .field("executable", &"[REDACTED]")
            .field("cwd", &"[REDACTED]")
            .field("arguments", &self.arguments)
            .field("credential_environment", &self.credential_environment)
            .field("static_environment", &self.static_environment)
            .finish()
    }
}

impl ZaiMcpServerSpec {
    /// Stable server id.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Human label.
    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        self.display_name
    }

    /// Exact documented transport facts.
    #[must_use]
    pub const fn transport(&self) -> &ZaiMcpTransportSpec {
        &self.transport
    }

    /// Documented current tool names.
    #[must_use]
    pub const fn tools(&self) -> &'static [&'static str] {
        self.tools
    }

    /// Whether one unavailable bundle server blocks unrelated product startup.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }

    /// Action-time approval for every exact allowlisted tool.
    #[must_use]
    pub const fn default_approval(&self) -> ZaiMcpApproval {
        self.default_approval
    }

    /// Non-tool capability exposure.
    #[must_use]
    pub const fn exposure(&self) -> ZaiMcpExposurePolicy {
        self.exposure
    }

    /// Resolve the vision server's executable and runtime evidence.
    ///
    /// The observed package version is pinned into argv; this never executes
    /// unpinned `@latest`. Credential bytes remain absent and resolve only when
    /// the ordinary MCP connection launches the child.
    ///
    /// # Errors
    /// Non-vision servers, unsafe paths, Node below 22, or package versions
    /// below 0.1.2 fail before producing a launch.
    pub fn resolve_vision_launch(
        &self,
        executable: impl AsRef<Path>,
        cwd: impl AsRef<Path>,
        node_major: u16,
        package_version: &str,
    ) -> Result<ZaiVisionMcpLaunch, ZaiMcpBundleError> {
        let ZaiMcpTransportSpec::Npx {
            package,
            minimum_package_version,
            minimum_node_major,
            credential_environment,
            static_environment,
        } = &self.transport
        else {
            return Err(ZaiMcpBundleError::NotVisionServer);
        };
        if node_major < *minimum_node_major {
            return Err(ZaiMcpBundleError::RuntimeTooOld);
        }
        let observed = parse_version(package_version).ok_or(ZaiMcpBundleError::PackageTooOld)?;
        let minimum =
            parse_version(minimum_package_version).ok_or(ZaiMcpBundleError::PackageTooOld)?;
        if observed < minimum {
            return Err(ZaiMcpBundleError::PackageTooOld);
        }
        Ok(ZaiVisionMcpLaunch {
            executable: canonical_executable(executable.as_ref())?,
            cwd: canonical_directory(cwd.as_ref())?,
            arguments: vec!["-y".to_owned(), format!("{package}@{package_version}")],
            credential_environment: credential_environment.clone(),
            static_environment,
        })
    }
}

/// Complete official Coding Plan MCP generation in stable registration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiCodingMcpBundle {
    servers: Vec<ZaiMcpServerSpec>,
}

impl ZaiCodingMcpBundle {
    /// Build and validate the four official server specifications.
    ///
    /// # Errors
    /// A compiled endpoint or identity no longer satisfies the lower boundary.
    pub fn official() -> Result<Self, ZaiMcpBundleError> {
        let credential_query = ZaiCredential::<Coding>::default_reference()
            .map_err(|_| ZaiMcpBundleError::Invalid)?
            .query()
            .clone();
        let authorization = ZaiMcpAuthorization {
            credential_query: credential_query.clone(),
        };
        let remote = |id, display_name, endpoint, tools| ZaiMcpServerSpec {
            id,
            display_name,
            transport: ZaiMcpTransportSpec::StreamableHttp {
                endpoint,
                authorization: authorization.clone(),
            },
            tools,
            required: false,
            default_approval: ZaiMcpApproval::Prompt,
            exposure: ZaiMcpExposurePolicy::none(),
        };
        let servers = vec![
            remote(
                "web-search-prime",
                "Z.AI Web Search",
                ZAI_MCP_SEARCH_ENDPOINT,
                &["webSearchPrime"],
            ),
            remote(
                "web-reader",
                "Z.AI Web Reader",
                ZAI_MCP_READER_ENDPOINT,
                &["webReader"],
            ),
            ZaiMcpServerSpec {
                id: "zai-vision",
                display_name: "Z.AI Vision",
                transport: ZaiMcpTransportSpec::Npx {
                    package: ZAI_MCP_VISION_PACKAGE,
                    minimum_package_version: ZAI_MCP_VISION_MINIMUM_VERSION,
                    minimum_node_major: ZAI_MCP_VISION_MINIMUM_NODE_MAJOR,
                    credential_environment: ZaiMcpCredentialEnvironment {
                        name: "Z_AI_API_KEY",
                        credential_query,
                    },
                    static_environment: &[("Z_AI_MODE", "ZAI")],
                },
                tools: &VISION_TOOLS,
                required: false,
                default_approval: ZaiMcpApproval::Prompt,
                exposure: ZaiMcpExposurePolicy::none(),
            },
            remote(
                "zread",
                "Z.AI Zread",
                ZAI_MCP_ZREAD_ENDPOINT,
                &["search_doc", "get_repo_structure", "read_file"],
            ),
        ];
        validate_servers(&servers)?;
        Ok(Self { servers })
    }

    /// Stable server generation.
    #[must_use]
    pub fn servers(&self) -> &[ZaiMcpServerSpec] {
        &self.servers
    }

    /// Secret-free JSON inspection projection.
    #[must_use]
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.servers
                .iter()
                .map(|server| {
                    let transport = match &server.transport {
                        ZaiMcpTransportSpec::StreamableHttp {
                            endpoint,
                            authorization,
                        } => serde_json::json!({
                            "kind":"streamable_http",
                            "endpoint":endpoint,
                            "authorization":{
                                "scheme":authorization.scheme(),
                                "credential_reference":authorization.credential_reference()
                            }
                        }),
                        ZaiMcpTransportSpec::Npx {
                            package,
                            minimum_package_version,
                            minimum_node_major,
                            credential_environment,
                            static_environment,
                        } => serde_json::json!({
                            "kind":"npx",
                            "package":package,
                            "minimum_package_version":minimum_package_version,
                            "minimum_node_major":minimum_node_major,
                            "credential_environment":{
                                "name":credential_environment.name(),
                                "credential_reference":credential_environment.credential_reference()
                            },
                            "static_environment":static_environment,
                        }),
                    };
                    serde_json::json!({
                        "id":server.id,
                        "display_name":server.display_name,
                        "transport":transport,
                        "tools":server.tools,
                        "required":server.required,
                        "default_approval":"prompt",
                        "exposure":{
                            "resources":server.exposure.resources,
                            "prompts":server.exposure.prompts,
                            "instructions":server.exposure.instructions,
                        },
                    })
                })
                .collect(),
        )
    }
}

/// Host adapter implemented by the composition layer that can see heycode-mcp.
pub trait ZaiCodingMcpHost: Send + Sync {
    /// Services the adapter resolves during registration.
    fn required_services(&self) -> &'static [ServiceKey];

    /// Broad families covering [`Self::inventory`] rows.
    fn descriptor_families(&self) -> &'static [PluginContributionKind];

    /// Exact inventory rows contributed by one server when an honest core
    /// namespace exists. The default refuses to mislabel an MCP definition.
    fn inventory(&self, _server: &ZaiMcpServerSpec) -> Vec<PluginContributionSpec> {
        Vec::new()
    }

    /// Convert and effect-register one server.
    ///
    /// # Errors
    /// Duplicate/unavailable/invalid shared registry state.
    fn register(
        &self,
        context: &Context,
        server: &ZaiMcpServerSpec,
    ) -> Result<(), ZaiMcpHostFailure>;
}

/// Build the aggregate effect-owned Coding Plan MCP plugin.
///
/// # Errors
/// Static bundle validation failure.
pub fn zai_coding_mcp_bundle_plugin(
    host: Arc<dyn ZaiCodingMcpHost>,
) -> Result<Box<dyn Plugin>, ZaiMcpBundleError> {
    Ok(Box::new(ZaiCodingMcpPlugin {
        bundle: ZaiCodingMcpBundle::official()?,
        host,
    }))
}

struct ZaiCodingMcpPlugin {
    bundle: ZaiCodingMcpBundle,
    host: Arc<dyn ZaiCodingMcpHost>,
}

impl Plugin for ZaiCodingMcpPlugin {
    fn name(&self) -> &'static str {
        ZAI_CODING_MCP_PLUGIN_ID
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            self.name(),
            env!("CARGO_PKG_VERSION"),
            self.host.descriptor_families(),
        )
    }

    fn inject(&self) -> &'static [ServiceKey] {
        self.host.required_services()
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        self.bundle
            .servers
            .iter()
            .flat_map(|server| self.host.inventory(server))
            .collect()
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        for server in &self.bundle.servers {
            self.host
                .register(context, server)
                .map_err(|error| CoreError::other(format!("Z.AI MCP `{}`: {error}", server.id)))?;
        }
        Ok(())
    }
}

/// Closed body-free host registration failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiMcpHostFailure {
    /// Server id is already live.
    Duplicate,
    /// Required registry or policy is unavailable.
    Unavailable,
    /// Root conversion rejected the provider-owned specification.
    Invalid,
}

impl std::fmt::Display for ZaiMcpHostFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Duplicate => "server is already registered",
            Self::Unavailable => "MCP registry is unavailable",
            Self::Invalid => "server specification is invalid",
        })
    }
}

impl std::error::Error for ZaiMcpHostFailure {}

/// Static Coding Plan bundle validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiMcpBundleError {
    /// One compiled server identity, endpoint or tool list is invalid.
    Invalid,
    /// Attempted to resolve a remote server as local vision.
    NotVisionServer,
    /// Resolved executable was not an absolute executable regular file.
    InvalidExecutable,
    /// Resolved cwd was not an absolute directory.
    InvalidWorkingDirectory,
    /// Observed Node.js major is below the provider minimum.
    RuntimeTooOld,
    /// Observed package version is malformed or below the provider minimum.
    PackageTooOld,
}

impl std::fmt::Display for ZaiMcpBundleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "Z.AI Coding Plan MCP bundle is invalid",
            Self::NotVisionServer => "Z.AI MCP server is not the local vision server",
            Self::InvalidExecutable => "Z.AI MCP executable is invalid",
            Self::InvalidWorkingDirectory => "Z.AI MCP working directory is invalid",
            Self::RuntimeTooOld => "Z.AI MCP Node.js runtime is too old",
            Self::PackageTooOld => "Z.AI MCP package version is invalid or too old",
        })
    }
}

impl std::error::Error for ZaiMcpBundleError {}

fn parse_version(value: &str) -> Option<(u32, u32, u32)> {
    if value.is_empty()
        || value.len() > 32
        || value.trim() != value
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let mut parts = value.split('.');
    let major = parse_version_component(parts.next()?)?;
    let minor = parse_version_component(parts.next()?)?;
    let patch = parse_version_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn parse_version_component(value: &str) -> Option<u32> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return None;
    }
    value.parse().ok()
}

fn canonical_executable(path: &Path) -> Result<PathBuf, ZaiMcpBundleError> {
    if !path.is_absolute() {
        return Err(ZaiMcpBundleError::InvalidExecutable);
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| ZaiMcpBundleError::InvalidExecutable)?;
    let metadata =
        std::fs::metadata(&canonical).map_err(|_| ZaiMcpBundleError::InvalidExecutable)?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(ZaiMcpBundleError::InvalidExecutable);
    }
    Ok(canonical)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ZaiMcpBundleError> {
    if !path.is_absolute() {
        return Err(ZaiMcpBundleError::InvalidWorkingDirectory);
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| ZaiMcpBundleError::InvalidWorkingDirectory)?;
    if !std::fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(ZaiMcpBundleError::InvalidWorkingDirectory);
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

fn validate_servers(servers: &[ZaiMcpServerSpec]) -> Result<(), ZaiMcpBundleError> {
    let mut ids = std::collections::BTreeSet::new();
    for server in servers {
        if server.id.is_empty()
            || !ids.insert(server.id)
            || server.tools.is_empty()
            || server.tools.iter().any(|tool| tool.is_empty())
        {
            return Err(ZaiMcpBundleError::Invalid);
        }
        match &server.transport {
            ZaiMcpTransportSpec::StreamableHttp { endpoint, .. } => {
                heycode_http::HttpRequest::post(endpoint, Vec::new())
                    .map_err(|_| ZaiMcpBundleError::Invalid)?;
            }
            ZaiMcpTransportSpec::Npx {
                package,
                minimum_package_version,
                minimum_node_major,
                credential_environment,
                static_environment,
            } => {
                if package.is_empty()
                    || minimum_package_version.is_empty()
                    || *minimum_node_major < 22
                    || credential_environment.name.is_empty()
                    || credential_environment.credential_reference().is_empty()
                    || static_environment.is_empty()
                {
                    return Err(ZaiMcpBundleError::Invalid);
                }
            }
        }
    }
    Ok(())
}

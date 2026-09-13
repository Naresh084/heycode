//! PL10 host-neutral WASI Component Model and WIT-v1 boundary.
//!
//! The package loader identifies the Component Model layer and freezes the
//! exact bytes. A caller-supplied engine remains responsible for full binary
//! validation and Canonical-ABI typechecking against the selected WIT world.
//! Capability plans are empty by default and never inherit ambient process
//! authority.

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    CodePluginCancellationToken, CodePluginCapabilityGrants, CodePluginError,
    CodePluginExecutableDigest, CodePluginLaunchSpec, CodePluginLauncher, CodePluginPackage,
    CodePluginProcess, CodePluginSessionId, CodePluginTransportFault, PackageContentHash,
    PackageProvenance, PluginCodeRuntime, PluginId, PluginInstallCache, PluginPermission,
    PluginVersion, PreparedCodePlugin,
};

/// Exact heycode code-plugin WIT package and worlds for ABI revision 1.
pub const WASI_CODE_PLUGIN_WIT_V1: &str = include_str!("../wit/heycode-code-plugin-v1.wit");

/// Stable custom WIT ABI revision.
pub const WASI_CODE_PLUGIN_ABI_VERSION: u32 = 1;

/// Canonical exported interface required from every component world.
pub const WASI_CODE_PLUGIN_INTERFACE_V1: &str = "dshx:code-plugin/plugin@1.0.0";

const COMPONENT_PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];
const MAX_ABI_NAMES: usize = 64;
const MAX_ABI_NAME_BYTES: usize = 256;
const MAX_GUEST_PATH_BYTES: usize = 256;

const FILESYSTEM_IMPORTS: &[&str] = &[
    "wasi:clocks/system-clock@0.3.1",
    "wasi:clocks/types@0.3.1",
    "wasi:filesystem/preopens@0.3.1",
    "wasi:filesystem/types@0.3.1",
];

const NETWORK_IMPORTS: &[&str] = &[
    "wasi:clocks/types@0.3.1",
    "wasi:sockets/ip-name-lookup@0.3.1",
    "wasi:sockets/types@0.3.1",
];

/// SHA-256 identity of the exact Component Model bytes supplied to the engine.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WasiComponentDigest(String);

impl WasiComponentDigest {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(render_sha256(bytes))
    }

    /// Algorithm-qualified lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WasiComponentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WasiComponentDigest([SHA256])")
    }
}

/// SHA-256 identity of the exact checked-in WIT-v1 document.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WasiWitDigest(String);

impl WasiWitDigest {
    fn v1() -> Self {
        Self(render_sha256(WASI_CODE_PLUGIN_WIT_V1.as_bytes()))
    }

    /// Algorithm-qualified lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WasiWitDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WasiWitDigest([SHA256])")
    }
}

/// One exact WIT world selected by the effective capability plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WasiComponentWorld {
    /// No filesystem or network interfaces.
    Pure,
    /// Capability-scoped filesystem interfaces only.
    Filesystem,
    /// Endpoint-scoped network interfaces only.
    Network,
    /// Both scoped filesystem and network interfaces.
    FilesystemNetwork,
}

impl WasiComponentWorld {
    /// Exact WIT world name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pure => "code-plugin",
            Self::Filesystem => "code-plugin-filesystem",
            Self::Network => "code-plugin-network",
            Self::FilesystemNetwork => "code-plugin-filesystem-network",
        }
    }

    fn expected_imports(self) -> Vec<String> {
        let rows: &[&str] = match self {
            Self::Pure => &[],
            Self::Filesystem => FILESYSTEM_IMPORTS,
            Self::Network => NETWORK_IMPORTS,
            Self::FilesystemNetwork => {
                let mut values = FILESYSTEM_IMPORTS.to_vec();
                values.extend_from_slice(NETWORK_IMPORTS);
                values.sort_unstable();
                values.dedup();
                return values.into_iter().map(str::to_owned).collect();
            }
        };
        rows.iter().map(|row| (*row).to_owned()).collect()
    }
}

/// Effective access attached to one WASI filesystem preopen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WasiPreopenAccess {
    /// Read but do not mutate.
    ReadOnly,
    /// Mutate but do not read file contents.
    WriteOnly,
    /// Read and mutate.
    ReadWrite,
}

impl WasiPreopenAccess {
    const fn can_read(self) -> bool {
        matches!(self, Self::ReadOnly | Self::ReadWrite)
    }

    const fn can_write(self) -> bool {
        matches!(self, Self::WriteOnly | Self::ReadWrite)
    }
}

/// One absolute host directory exposed under one portable guest path.
#[derive(Clone, PartialEq, Eq)]
pub struct WasiPreopen {
    host_path: PathBuf,
    guest_path: String,
    access: WasiPreopenAccess,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl WasiPreopen {
    /// Validate one scoped preopen descriptor.
    ///
    /// # Errors
    /// Relative host paths and non-portable guest paths are refused.
    pub fn new(
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        access: WasiPreopenAccess,
    ) -> Result<Self, WasiComponentError> {
        let host_path = host_path.into();
        let guest_path = guest_path.into();
        if !host_path.is_absolute() || !valid_guest_path(&guest_path) {
            return Err(WasiComponentError::InvalidPreopen);
        }
        let host_path =
            std::fs::canonicalize(host_path).map_err(|_| WasiComponentError::InvalidPreopen)?;
        let directory =
            std::fs::File::open(&host_path).map_err(|_| WasiComponentError::InvalidPreopen)?;
        let metadata = directory
            .metadata()
            .map_err(|_| WasiComponentError::InvalidPreopen)?;
        if !metadata.is_dir() {
            return Err(WasiComponentError::InvalidPreopen);
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Ok(Self {
            host_path,
            guest_path,
            access,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }

    /// Exact host path for the engine's capability-opening boundary.
    #[must_use]
    pub fn host_path(&self) -> &Path {
        &self.host_path
    }

    /// Portable path visible inside the component.
    #[must_use]
    pub fn guest_path(&self) -> &str {
        &self.guest_path
    }

    /// Effective read/write ceiling.
    #[must_use]
    pub const fn access(&self) -> WasiPreopenAccess {
        self.access
    }

    /// Whether an already-open Unix directory is the exact object minted by
    /// this policy row.
    ///
    /// Path-based Component engines use this before converting the held file
    /// descriptor into their own preopen handle.
    #[cfg(unix)]
    #[must_use]
    pub fn matches_open_directory(&self, directory: &std::fs::File) -> bool {
        use std::os::unix::fs::MetadataExt as _;

        directory.metadata().is_ok_and(|metadata| {
            metadata.is_dir() && metadata.dev() == self.device && metadata.ino() == self.inode
        })
    }
}

impl fmt::Debug for WasiPreopen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WasiPreopen")
            .field("host_path", &"[REDACTED]")
            .field("guest_path", &self.guest_path)
            .field("access", &self.access)
            .finish()
    }
}

/// One exact outbound hostname or IP address and port.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WasiNetworkEndpoint {
    host: String,
    port: u16,
}

impl WasiNetworkEndpoint {
    /// Validate one endpoint-scoped network grant.
    ///
    /// # Errors
    /// Invalid host syntax or port zero is refused.
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, WasiComponentError> {
        let host = host.into();
        if port == 0 || !valid_network_host(&host) {
            return Err(WasiComponentError::InvalidNetworkEndpoint);
        }
        let host = if host.parse::<IpAddr>().is_ok() {
            host
        } else {
            host.to_ascii_lowercase()
        };
        Ok(Self { host, port })
    }

    /// Exact host admitted by the network policy.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Exact admitted port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Debug for WasiNetworkEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WasiNetworkEndpoint")
            .field("host", &"[REDACTED]")
            .field("port", &self.port)
            .finish()
    }
}

/// Package-bound effective WASI capability plan.
#[derive(Clone)]
pub struct WasiComponentCapabilityPolicy {
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    component_digest: CodePluginExecutableDigest,
    granted: Vec<PluginPermission>,
    world: WasiComponentWorld,
    preopens: Vec<WasiPreopen>,
    network_endpoints: Vec<WasiNetworkEndpoint>,
}

impl WasiComponentCapabilityPolicy {
    /// Construct an empty policy with no ambient authority.
    #[must_use]
    pub fn deny_all(package: &WasiComponentPackage) -> Self {
        Self {
            id: package.code.id().clone(),
            version: package.code.version().clone(),
            package_digest: package.code.package_digest().clone(),
            component_digest: package.code.executable_digest().clone(),
            granted: Vec::new(),
            world: WasiComponentWorld::Pure,
            preopens: Vec::new(),
            network_endpoints: Vec::new(),
        }
    }

    /// Bind explicit manifest grants to scoped filesystem and network facts.
    ///
    /// Process spawning, credential resolution and MCP connections have no
    /// WIT-v1 host interface and are refused. Hook registration and registry
    /// override remain activation-time authority and expose no WASI import.
    ///
    /// # Errors
    /// A foreign grant, unrequested resource, missing scope, duplicate scope
    /// or unsupported runtime capability is refused.
    pub fn new(
        package: &WasiComponentPackage,
        grants: &CodePluginCapabilityGrants,
        preopens: impl IntoIterator<Item = WasiPreopen>,
        network_endpoints: impl IntoIterator<Item = WasiNetworkEndpoint>,
    ) -> Result<Self, WasiComponentError> {
        grants
            .verify(&package.code)
            .map_err(|_| WasiComponentError::GrantMismatch)?;
        let granted = grants.granted().to_vec();
        for capability in &granted {
            if matches!(
                capability,
                PluginPermission::ProcessSpawn
                    | PluginPermission::CredentialUse
                    | PluginPermission::McpConnect
            ) {
                return Err(WasiComponentError::UnsupportedCapability(*capability));
            }
        }

        let mut preopens = preopens.into_iter().collect::<Vec<_>>();
        preopens.sort_by(|left, right| left.guest_path.cmp(&right.guest_path));
        let mut guest_paths = BTreeSet::new();
        let mut host_paths = BTreeSet::new();
        for preopen in &preopens {
            if !guest_paths.insert(preopen.guest_path.clone())
                || !host_paths.insert(preopen.host_path.clone())
            {
                return Err(WasiComponentError::DuplicatePreopen);
            }
            if preopen.access.can_read() && !granted.contains(&PluginPermission::FilesystemRead) {
                return Err(WasiComponentError::UnrequestedCapability(
                    PluginPermission::FilesystemRead,
                ));
            }
            if preopen.access.can_write() && !granted.contains(&PluginPermission::FilesystemWrite) {
                return Err(WasiComponentError::UnrequestedCapability(
                    PluginPermission::FilesystemWrite,
                ));
            }
        }
        if granted.contains(&PluginPermission::FilesystemRead)
            && !preopens.iter().any(|preopen| preopen.access.can_read())
        {
            return Err(WasiComponentError::MissingCapabilityResource(
                PluginPermission::FilesystemRead,
            ));
        }
        if granted.contains(&PluginPermission::FilesystemWrite)
            && !preopens.iter().any(|preopen| preopen.access.can_write())
        {
            return Err(WasiComponentError::MissingCapabilityResource(
                PluginPermission::FilesystemWrite,
            ));
        }

        let mut network_endpoints = network_endpoints.into_iter().collect::<Vec<_>>();
        network_endpoints.sort();
        if network_endpoints.windows(2).any(|rows| rows[0] == rows[1]) {
            return Err(WasiComponentError::DuplicateNetworkEndpoint);
        }
        if !network_endpoints.is_empty() && !granted.contains(&PluginPermission::NetworkAccess) {
            return Err(WasiComponentError::UnrequestedCapability(
                PluginPermission::NetworkAccess,
            ));
        }
        if granted.contains(&PluginPermission::NetworkAccess) && network_endpoints.is_empty() {
            return Err(WasiComponentError::MissingCapabilityResource(
                PluginPermission::NetworkAccess,
            ));
        }

        let filesystem = !preopens.is_empty();
        let network = !network_endpoints.is_empty();
        let world = match (filesystem, network) {
            (false, false) => WasiComponentWorld::Pure,
            (true, false) => WasiComponentWorld::Filesystem,
            (false, true) => WasiComponentWorld::Network,
            (true, true) => WasiComponentWorld::FilesystemNetwork,
        };
        Ok(Self {
            id: package.code.id().clone(),
            version: package.code.version().clone(),
            package_digest: package.code.package_digest().clone(),
            component_digest: package.code.executable_digest().clone(),
            granted,
            world,
            preopens,
            network_endpoints,
        })
    }

    /// Exact WIT world selected by the effective resources.
    #[must_use]
    pub const fn world(&self) -> WasiComponentWorld {
        self.world
    }

    fn verify(
        &self,
        package: &WasiComponentPackage,
        grants: &CodePluginCapabilityGrants,
    ) -> Result<(), WasiComponentError> {
        grants
            .verify(&package.code)
            .map_err(|_| WasiComponentError::GrantMismatch)?;
        if self.id != *package.code.id()
            || self.version != *package.code.version()
            || self.package_digest != *package.code.package_digest()
            || self.component_digest != *package.code.executable_digest()
            || self.granted != grants.granted()
        {
            return Err(WasiComponentError::GrantMismatch);
        }
        Ok(())
    }
}

impl fmt::Debug for WasiComponentCapabilityPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WasiComponentCapabilityPolicy")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("world", &self.world)
            .field("granted_count", &self.granted.len())
            .field("preopen_count", &self.preopens.len())
            .field("network_endpoint_count", &self.network_endpoints.len())
            .finish()
    }
}

/// Reverified component package plus frozen exact component bytes.
pub struct WasiComponentPackage {
    code: CodePluginPackage,
    component_bytes: Arc<[u8]>,
    component_digest: WasiComponentDigest,
}

impl WasiComponentPackage {
    /// Load one non-executable Component Model artifact from the immutable cache.
    ///
    /// This checks only the distinct component-layer preamble. The selected
    /// [`WasiComponentEngine`] must perform full binary and WIT type validation
    /// before returning an instance.
    ///
    /// # Errors
    /// Cache/provenance/identity failure or a core-module/non-Wasm artifact is
    /// refused before engine invocation.
    pub fn load(
        cache: &PluginInstallCache,
        id: &PluginId,
        version: &PluginVersion,
        provenance: &PackageProvenance,
        component: impl Into<String>,
    ) -> Result<Self, WasiComponentError> {
        let code = CodePluginPackage::load_component(cache, id, version, provenance, component)?;
        Self::from_code(code)
    }

    /// Load only the strict manifest-declared WASI Component Model artifact.
    ///
    /// # Errors
    /// Missing or non-WASI code metadata, cache/provenance failure, or an
    /// invalid Component Model preamble is refused before engine invocation.
    pub fn load_declared(
        cache: &PluginInstallCache,
        id: &PluginId,
        version: &PluginVersion,
        provenance: &PackageProvenance,
    ) -> Result<Self, WasiComponentError> {
        let code = CodePluginPackage::load_declared(cache, id, version, provenance)?;
        if code.runtime() != PluginCodeRuntime::WasiComponentV1 {
            return Err(WasiComponentError::RuntimeMismatch);
        }
        Self::from_code(code)
    }

    fn from_code(code: CodePluginPackage) -> Result<Self, WasiComponentError> {
        let component_bytes = code.entrypoint_bytes();
        if component_bytes.get(..COMPONENT_PREAMBLE.len()) != Some(COMPONENT_PREAMBLE.as_slice()) {
            return Err(WasiComponentError::InvalidComponent);
        }
        let component_digest = WasiComponentDigest::from_bytes(&component_bytes);
        Ok(Self {
            code,
            component_bytes,
            component_digest,
        })
    }

    /// Underlying PL09 package identity used for explicit grant construction.
    #[must_use]
    pub const fn code_package(&self) -> &CodePluginPackage {
        &self.code
    }

    /// Digest of the exact bytes supplied to the component engine.
    #[must_use]
    pub const fn component_digest(&self) -> &WasiComponentDigest {
        &self.component_digest
    }

    /// Instantiate, ABI-check and complete the ordinary PL09 handshake.
    ///
    /// # Errors
    /// Policy mismatch, engine refusal, ABI mismatch, cancellation or the
    /// closed PL09 handshake failures are returned with no component output.
    pub fn launch(
        self,
        grants: CodePluginCapabilityGrants,
        policy: WasiComponentCapabilityPolicy,
        session_id: CodePluginSessionId,
        engine: Arc<dyn WasiComponentEngine>,
        cancellation: &CodePluginCancellationToken,
    ) -> Result<PreparedCodePlugin, WasiComponentError> {
        policy.verify(&self, &grants)?;
        let instantiation = WasiComponentInstantiation {
            id: self.code.id().clone(),
            version: self.code.version().clone(),
            package_digest: self.code.package_digest().clone(),
            executable_digest: self.code.executable_digest().clone(),
            component_digest: self.component_digest,
            component_bytes: self.component_bytes,
            wit_digest: WasiWitDigest::v1(),
            granted: policy.granted.clone(),
            world: policy.world,
            preopens: policy.preopens,
            network_endpoints: policy.network_endpoints,
        };
        let launcher = Arc::new(WasiComponentLauncher {
            engine,
            instantiation,
        });
        self.code
            .launch(grants, session_id, launcher, cancellation)
            .map_err(map_code_error)
    }
}

impl fmt::Debug for WasiComponentPackage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WasiComponentPackage")
            .field("code", &self.code)
            .field("component_digest", &self.component_digest)
            .field("component_bytes", &self.component_bytes.len())
            .finish()
    }
}

/// Exact ABI observation returned by a Component Model engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasiComponentAbiReport {
    world: String,
    wit_digest: String,
    imports: Vec<String>,
    exports: Vec<String>,
}

impl WasiComponentAbiReport {
    /// Validate one engine-produced ABI report.
    ///
    /// # Errors
    /// Oversized, duplicate or control-bearing names and malformed digests are
    /// refused.
    pub fn new<I, S, E, T>(
        world: impl Into<String>,
        wit_digest: impl Into<String>,
        imports: I,
        exports: E,
    ) -> Result<Self, WasiComponentError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        E: IntoIterator<Item = T>,
        T: Into<String>,
    {
        let world = world.into();
        let wit_digest = wit_digest.into();
        let imports = validate_abi_names(imports)?;
        let exports = validate_abi_names(exports)?;
        if !valid_abi_name(&world) || !valid_sha256(&wit_digest) {
            return Err(WasiComponentError::InvalidAbiReport);
        }
        Ok(Self {
            world,
            wit_digest,
            imports,
            exports,
        })
    }

    /// Exact expected report for one checked-in WIT-v1 world.
    #[must_use]
    pub fn exact_v1(world: WasiComponentWorld) -> Self {
        Self {
            world: world.as_str().to_owned(),
            wit_digest: WasiWitDigest::v1().0,
            imports: world.expected_imports(),
            exports: vec![WASI_CODE_PLUGIN_INTERFACE_V1.to_owned()],
        }
    }

    /// Exact selected WIT world name.
    #[must_use]
    pub fn world(&self) -> &str {
        &self.world
    }

    /// Exact checked-in WIT document identity.
    #[must_use]
    pub fn wit_digest(&self) -> &str {
        &self.wit_digest
    }

    /// Sorted exact Component imports admitted for the selected world.
    #[must_use]
    pub fn imports(&self) -> &[String] {
        &self.imports
    }

    /// Sorted exact Component exports required by ABI v1.
    #[must_use]
    pub fn exports(&self) -> &[String] {
        &self.exports
    }

    fn matches_v1(&self, world: WasiComponentWorld, wit_digest: &WasiWitDigest) -> bool {
        let expected_imports = world.expected_imports();
        self.world == world.as_str()
            && self.wit_digest == wit_digest.as_str()
            && self
                .imports
                .iter()
                .all(|actual| expected_imports.binary_search(actual).is_ok())
            && self.exports == [WASI_CODE_PLUGIN_INTERFACE_V1]
    }
}

/// Complete engine input with no implicit ambient capabilities.
pub struct WasiComponentInstantiation {
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    executable_digest: CodePluginExecutableDigest,
    component_digest: WasiComponentDigest,
    component_bytes: Arc<[u8]>,
    wit_digest: WasiWitDigest,
    granted: Vec<PluginPermission>,
    world: WasiComponentWorld,
    preopens: Vec<WasiPreopen>,
    network_endpoints: Vec<WasiNetworkEndpoint>,
}

impl WasiComponentInstantiation {
    /// Exact frozen Component Model bytes.
    #[must_use]
    pub fn component_bytes(&self) -> &[u8] {
        &self.component_bytes
    }

    /// Exact component byte identity.
    #[must_use]
    pub const fn component_digest(&self) -> &WasiComponentDigest {
        &self.component_digest
    }

    /// Exact WIT document identity.
    #[must_use]
    pub const fn wit_digest(&self) -> &WasiWitDigest {
        &self.wit_digest
    }

    /// Exact selected world.
    #[must_use]
    pub const fn world(&self) -> WasiComponentWorld {
        self.world
    }

    /// Explicit filesystem preopens; empty means no filesystem authority.
    #[must_use]
    pub fn preopens(&self) -> &[WasiPreopen] {
        &self.preopens
    }

    /// Explicit outbound endpoints; empty means no network authority.
    #[must_use]
    pub fn network_endpoints(&self) -> &[WasiNetworkEndpoint] {
        &self.network_endpoints
    }

    /// Environment inheritance is forbidden by ABI v1.
    #[must_use]
    pub const fn inherits_environment(&self) -> bool {
        false
    }

    /// Argument inheritance is forbidden by ABI v1.
    #[must_use]
    pub const fn inherits_arguments(&self) -> bool {
        false
    }

    /// Stdio inheritance is forbidden; calls use typed component exports.
    #[must_use]
    pub const fn inherits_stdio(&self) -> bool {
        false
    }
}

impl fmt::Debug for WasiComponentInstantiation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WasiComponentInstantiation")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("package_digest", &self.package_digest)
            .field("component_digest", &self.component_digest)
            .field("wit_digest", &self.wit_digest)
            .field("component_bytes", &self.component_bytes.len())
            .field("granted_count", &self.granted.len())
            .field("world", &self.world)
            .field("preopen_count", &self.preopens.len())
            .field("network_endpoint_count", &self.network_endpoints.len())
            .field("inherits_environment", &false)
            .field("inherits_arguments", &false)
            .field("inherits_stdio", &false)
            .finish()
    }
}

/// Typechecked component instance plus its PL09 invocation adapter.
pub struct WasiComponentInstance {
    abi: WasiComponentAbiReport,
    process: Arc<dyn CodePluginProcess>,
}

impl WasiComponentInstance {
    /// Bind an engine ABI report to the process adapter it describes.
    #[must_use]
    pub fn new(abi: WasiComponentAbiReport, process: Arc<dyn CodePluginProcess>) -> Self {
        Self { abi, process }
    }
}

impl fmt::Debug for WasiComponentInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WasiComponentInstance")
            .field("abi", &self.abi)
            .finish_non_exhaustive()
    }
}

/// Host runtime that fully validates, links and instantiates one component.
pub trait WasiComponentEngine: Send + Sync {
    /// Instantiate the exact bytes with only the supplied capability plan.
    ///
    /// The implementation must typecheck the selected world through the
    /// Component Model Canonical ABI, construct an empty WASI context, then add
    /// only the listed preopens/endpoints. It must not inherit environment,
    /// arguments or stdio.
    ///
    /// # Errors
    /// A closed body-free engine fault.
    fn instantiate(
        &self,
        request: &WasiComponentInstantiation,
        cancellation: &CodePluginCancellationToken,
    ) -> Result<WasiComponentInstance, WasiComponentEngineFault>;
}

/// Closed body-free Component Model engine failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WasiComponentEngineFault {
    /// Component decoding or Canonical-ABI typechecking failed.
    #[error("WASI component is invalid")]
    InvalidComponent,
    /// Runtime instantiation failed.
    #[error("WASI component could not start")]
    Start,
    /// Component runtime is not available.
    #[error("WASI component runtime is unavailable")]
    Unavailable,
    /// Caller cancelled instantiation.
    #[error("WASI component instantiation was cancelled")]
    Cancelled,
}

struct WasiComponentLauncher {
    engine: Arc<dyn WasiComponentEngine>,
    instantiation: WasiComponentInstantiation,
}

impl CodePluginLauncher for WasiComponentLauncher {
    fn launch(
        &self,
        spec: &CodePluginLaunchSpec,
        cancellation: &CodePluginCancellationToken,
    ) -> Result<Arc<dyn CodePluginProcess>, CodePluginTransportFault> {
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        if spec.id() != &self.instantiation.id
            || spec.version() != &self.instantiation.version
            || spec.package_digest() != &self.instantiation.package_digest
            || spec.executable_digest() != &self.instantiation.executable_digest
            || spec.granted_capabilities() != self.instantiation.granted
        {
            return Err(CodePluginTransportFault::Protocol);
        }
        let instance = self
            .engine
            .instantiate(&self.instantiation, cancellation)
            .map_err(map_engine_fault)?;
        if cancellation.is_cancelled() {
            instance.process.shutdown();
            return Err(CodePluginTransportFault::Cancelled);
        }
        if !instance
            .abi
            .matches_v1(self.instantiation.world, &self.instantiation.wit_digest)
        {
            instance.process.shutdown();
            return Err(CodePluginTransportFault::Protocol);
        }
        Ok(instance.process)
    }
}

/// Stable Component Model preparation and policy failures.
#[derive(Debug, Error)]
pub enum WasiComponentError {
    /// PL09 package identity/admission failed.
    #[error(transparent)]
    Code(#[from] CodePluginError),
    /// Artifact is not a Component Model layer-1 binary.
    #[error("WASI artifact is not a Component Model binary")]
    InvalidComponent,
    /// Manifest selected another code runtime.
    #[error("WASI package runtime does not match the manifest")]
    RuntimeMismatch,
    /// Capability policy belongs to another package/component/grant set.
    #[error("WASI capability policy does not match the component generation")]
    GrantMismatch,
    /// A resource would expose a capability absent from the explicit grant.
    #[error("WASI resource exposes unrequested capability {0}")]
    UnrequestedCapability(PluginPermission),
    /// A granted capability has no scoped resource and would be ambiguous.
    #[error("WASI capability {0} has no scoped resource")]
    MissingCapabilityResource(PluginPermission),
    /// WIT v1 has no isolated implementation for this capability.
    #[error("WASI WIT v1 does not support capability {0}")]
    UnsupportedCapability(PluginPermission),
    /// Filesystem preopen is invalid.
    #[error("WASI filesystem preopen is invalid")]
    InvalidPreopen,
    /// Guest or host preopen aliases another row.
    #[error("WASI filesystem preopen is duplicated")]
    DuplicatePreopen,
    /// Network endpoint is invalid.
    #[error("WASI network endpoint is invalid")]
    InvalidNetworkEndpoint,
    /// Network endpoint appears twice.
    #[error("WASI network endpoint is duplicated")]
    DuplicateNetworkEndpoint,
    /// Engine ABI report is malformed.
    #[error("WASI component ABI report is invalid")]
    InvalidAbiReport,
    /// Engine/PL09 transport failed with a closed class.
    #[error(transparent)]
    Transport(CodePluginTransportFault),
}

fn map_code_error(error: CodePluginError) -> WasiComponentError {
    match error {
        CodePluginError::Transport(fault) => WasiComponentError::Transport(fault),
        other => WasiComponentError::Code(other),
    }
}

const fn map_engine_fault(fault: WasiComponentEngineFault) -> CodePluginTransportFault {
    match fault {
        WasiComponentEngineFault::InvalidComponent => CodePluginTransportFault::Protocol,
        WasiComponentEngineFault::Start => CodePluginTransportFault::Spawn,
        WasiComponentEngineFault::Unavailable => CodePluginTransportFault::Unavailable,
        WasiComponentEngineFault::Cancelled => CodePluginTransportFault::Cancelled,
    }
}

fn validate_abi_names<I, S>(values: I) -> Result<Vec<String>, WasiComponentError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut values = values.into_iter().map(Into::into).collect::<Vec<_>>();
    if values.len() > MAX_ABI_NAMES || values.iter().any(|value| !valid_abi_name(value)) {
        return Err(WasiComponentError::InvalidAbiReport);
    }
    values.sort();
    if values.windows(2).any(|rows| rows[0] == rows[1]) {
        return Err(WasiComponentError::InvalidAbiReport);
    }
    Ok(values)
}

fn valid_abi_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ABI_NAME_BYTES
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'\\' && byte != b'"')
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_guest_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_GUEST_PATH_BYTES
        || !value.starts_with('/')
        || (value.len() > 1 && value.ends_with('/'))
        || value.contains("//")
        || value.contains('\\')
        || value.contains(':')
        || value.chars().any(char::is_control)
    {
        return false;
    }
    value == "/"
        || value
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn valid_network_host(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 253
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return false;
    }
    if value.parse::<IpAddr>().is_ok() {
        return true;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn render_sha256(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut rendered = String::with_capacity(71);
    rendered.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

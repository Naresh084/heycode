//! PL09 host-neutral out-of-process code-plugin protocol.
//!
//! This boundary proves package provenance, executable and package digests,
//! exact identity, explicit capability grants, closed JSON protocol frames and
//! one generation-wide contribution lease. It deliberately does not spawn a
//! process itself: a higher owner adapts CodePluginLauncher to the composed
//! subprocess and sandbox services.

use std::collections::BTreeSet;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginContributionSpec, PluginDescriptor,
    ServiceKey,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::cache_fs::read_cache_file;
use crate::model::valid_kebab;
use crate::{
    ContributionKind, DeclarativePackage, DeclarativePluginError, HostActivationFailure,
    MarketplaceError, PackageCacheError, PackageContentHash, PackageProvenance, PluginCodeRuntime,
    PluginId, PluginInstallCache, PluginPath, PluginPermission, PluginVersion,
};

/// Stable JSON protocol version for code-plugin processes.
pub const CODE_PLUGIN_PROTOCOL_VERSION: u32 = 1;

/// Static core bridge id for the complete active code-plugin generation.
pub const CODE_PLUGIN_ACTIVATION_PLUGIN_ID: &str = "code-extensions";

const MAX_EXECUTABLE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum encoded request or response frame accepted by protocol v1.
pub const CODE_PLUGIN_MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_NODES: usize = 65_536;

const CODE_PLUGIN_FAMILIES: &[PluginContributionKind] = &[
    PluginContributionKind::ExternalProcess,
    PluginContributionKind::Provider,
    PluginContributionKind::Tool,
    PluginContributionKind::Command,
    PluginContributionKind::PromptSection,
    PluginContributionKind::Waterfall,
    PluginContributionKind::UserInterface,
];

/// SHA-256 identity of the exact executable bytes admitted for launch.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CodePluginExecutableDigest(String);

impl CodePluginExecutableDigest {
    fn from_bytes(bytes: &[u8]) -> Self {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        let mut rendered = String::with_capacity(71);
        rendered.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(rendered, "{byte:02x}");
        }
        Self(rendered)
    }

    /// Algorithm-qualified lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CodePluginExecutableDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CodePluginExecutableDigest([SHA256])")
    }
}

/// Host-minted identity for one process generation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CodePluginSessionId(String);

impl CodePluginSessionId {
    /// Construct a fixed-width lowercase identity from host entropy.
    #[must_use]
    pub fn from_u128(value: u128) -> Self {
        Self(format!("{value:032x}"))
    }

    /// Stable wire identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CodePluginSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CodePluginSessionId([REDACTED])")
    }
}

/// Exact manifest-declared contribution a code process may implement.
#[derive(Clone, PartialEq, Eq)]
pub struct CodePluginContribution {
    package_id: PluginId,
    package_version: PluginVersion,
    kind: ContributionKind,
    local_id: String,
    public_name: String,
    path: PluginPath,
    document: Arc<str>,
}

impl fmt::Debug for CodePluginContribution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodePluginContribution")
            .field("package_id", &self.package_id)
            .field("package_version", &self.package_version)
            .field("kind", &self.kind)
            .field("local_id", &self.local_id)
            .field("public_name", &self.public_name)
            .field("path", &self.path)
            .field("document_bytes", &self.document.len())
            .finish()
    }
}

impl CodePluginContribution {
    fn from_declarative(contribution: &crate::DeclarativeContribution) -> Self {
        Self {
            package_id: contribution.package_id().clone(),
            package_version: contribution.package_version().clone(),
            kind: contribution.kind(),
            local_id: contribution.local_id().to_owned(),
            public_name: contribution.public_name().to_owned(),
            path: contribution.path().clone(),
            document: Arc::from(contribution.document()),
        }
    }

    /// Package that supplied this exact immutable document.
    #[must_use]
    pub const fn package_id(&self) -> &PluginId {
        &self.package_id
    }

    /// Exact installed package version.
    #[must_use]
    pub const fn package_version(&self) -> &PluginVersion {
        &self.package_version
    }

    /// Exact PL03 registry kind.
    #[must_use]
    pub const fn kind(&self) -> ContributionKind {
        self.kind
    }

    /// Package-local manifest id.
    #[must_use]
    pub fn local_id(&self) -> &str {
        &self.local_id
    }

    /// Exact validated public registry name.
    #[must_use]
    pub fn public_name(&self) -> &str {
        &self.public_name
    }

    /// Manifest-owned primary document path.
    #[must_use]
    pub const fn path(&self) -> &PluginPath {
        &self.path
    }

    /// Complete bounded immutable primary document for the product adapter.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }

    fn wire(&self) -> WireContribution {
        WireContribution {
            kind: self.kind,
            name: self.public_name.clone(),
        }
    }
}

/// Reverified installed package plus one explicit executable entrypoint.
pub struct CodePluginPackage {
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    entrypoint: PluginPath,
    entrypoint_path: PathBuf,
    entrypoint_executable: bool,
    entrypoint_bytes: Arc<[u8]>,
    executable_digest: CodePluginExecutableDigest,
    runtime: PluginCodeRuntime,
    requested_capabilities: BTreeSet<PluginPermission>,
    contributions: Vec<CodePluginContribution>,
}

impl CodePluginPackage {
    /// Reverify one installed package and admit an executable entrypoint.
    ///
    /// The package must have host-established provenance. Cache resolution
    /// rehashes the whole object and provenance supplies the independent digest
    /// authority PL05 established.
    ///
    /// # Errors
    /// Cache or provenance substitution, unknown origin, an unsafe entrypoint
    /// or a package with no PL03 code contribution fails before launch.
    pub fn load(
        cache: &PluginInstallCache,
        id: &PluginId,
        version: &PluginVersion,
        provenance: &PackageProvenance,
        entrypoint: impl Into<String>,
    ) -> Result<Self, CodePluginError> {
        Self::load_artifact(
            cache,
            id,
            version,
            provenance,
            entrypoint,
            PluginCodeRuntime::NativeProcess,
            true,
        )
    }

    /// Reverify one installed package and use only its strict manifest-owned
    /// runtime and entrypoint.
    ///
    /// # Errors
    /// Missing code metadata, cache/provenance substitution, unsafe artifact,
    /// or contribution-document admission fails before launch.
    pub fn load_declared(
        cache: &PluginInstallCache,
        id: &PluginId,
        version: &PluginVersion,
        provenance: &PackageProvenance,
    ) -> Result<Self, CodePluginError> {
        let installed = cache.resolve(id, version)?;
        let code = installed
            .manifest()
            .code()
            .ok_or(CodePluginError::MissingCodeDeclaration)?;
        let executable = code.runtime() == PluginCodeRuntime::NativeProcess;
        Self::load_artifact(
            cache,
            id,
            version,
            provenance,
            code.entrypoint().as_str(),
            code.runtime(),
            executable,
        )
    }

    pub(crate) fn load_component(
        cache: &PluginInstallCache,
        id: &PluginId,
        version: &PluginVersion,
        provenance: &PackageProvenance,
        entrypoint: impl Into<String>,
    ) -> Result<Self, CodePluginError> {
        Self::load_artifact(
            cache,
            id,
            version,
            provenance,
            entrypoint,
            PluginCodeRuntime::WasiComponentV1,
            false,
        )
    }

    fn load_artifact(
        cache: &PluginInstallCache,
        id: &PluginId,
        version: &PluginVersion,
        provenance: &PackageProvenance,
        entrypoint: impl Into<String>,
        runtime: PluginCodeRuntime,
        executable: bool,
    ) -> Result<Self, CodePluginError> {
        let installed = cache.resolve(id, version)?;
        if !provenance.is_established() {
            return Err(CodePluginError::UnknownProvenance);
        }
        provenance.reverify(&installed)?;
        let entrypoint = PluginPath::new("code.entrypoint", entrypoint.into())
            .map_err(|_| CodePluginError::InvalidEntrypoint)?;
        if let Some(declared) = installed.manifest().code()
            && (declared.runtime() != runtime || declared.entrypoint() != &entrypoint)
        {
            return Err(CodePluginError::CodeDeclarationMismatch);
        }
        let entrypoint_path = installed.package_root().join(entrypoint.as_str());
        let artifact = read_cache_file(&entrypoint_path, MAX_EXECUTABLE_BYTES, Some(executable))
            .map_err(|_| CodePluginError::UnsafeEntrypoint)?;
        let declarative = DeclarativePackage::load(&installed)?;
        let contributions = declarative
            .contributions()
            .iter()
            .map(CodePluginContribution::from_declarative)
            .collect::<Vec<_>>();
        if contributions.is_empty() {
            return Err(CodePluginError::NoContributions);
        }
        let executable_digest = CodePluginExecutableDigest::from_bytes(&artifact);
        Ok(Self {
            id: installed.manifest().id().clone(),
            version: installed.manifest().version().clone(),
            package_digest: installed.content_hash().clone(),
            entrypoint,
            entrypoint_path,
            entrypoint_executable: executable,
            entrypoint_bytes: Arc::from(artifact),
            executable_digest,
            runtime,
            requested_capabilities: installed.manifest().permissions().iter().copied().collect(),
            contributions,
        })
    }

    /// Package identity.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// Exact package version.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        &self.version
    }

    /// Reverified canonical package tree digest.
    #[must_use]
    pub const fn package_digest(&self) -> &PackageContentHash {
        &self.package_digest
    }

    /// Reverified exact executable digest.
    #[must_use]
    pub const fn executable_digest(&self) -> &CodePluginExecutableDigest {
        &self.executable_digest
    }

    /// Exact manifest-selected execution runtime.
    #[must_use]
    pub const fn runtime(&self) -> PluginCodeRuntime {
        self.runtime
    }

    /// Manifest-declared PL03 contribution generation.
    #[must_use]
    pub fn contributions(&self) -> &[CodePluginContribution] {
        &self.contributions
    }

    fn reverify_entrypoint(&self) -> Result<(), CodePluginError> {
        let artifact = read_cache_file(
            &self.entrypoint_path,
            MAX_EXECUTABLE_BYTES,
            Some(self.entrypoint_executable),
        )
        .map_err(|_| CodePluginError::UnsafeEntrypoint)?;
        if artifact.as_slice() != self.entrypoint_bytes.as_ref()
            || CodePluginExecutableDigest::from_bytes(&artifact) != self.executable_digest
        {
            return Err(CodePluginError::UnsafeEntrypoint);
        }
        Ok(())
    }

    pub(crate) fn entrypoint_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.entrypoint_bytes)
    }

    /// Launch and complete the v1 identity, grant and contribution handshake.
    ///
    /// # Errors
    /// Entry-point drift, grant mismatch, launch or handshake failure,
    /// cancellation, or a mismatched ready frame. Every failure shuts the
    /// process down before returning.
    pub fn launch(
        self,
        grants: CodePluginCapabilityGrants,
        session_id: CodePluginSessionId,
        launcher: Arc<dyn CodePluginLauncher>,
        cancellation: &CancellationToken,
    ) -> Result<PreparedCodePlugin, CodePluginError> {
        grants.verify(&self)?;
        if cancellation.is_cancelled() {
            return Err(CodePluginError::Transport(
                CodePluginTransportFault::Cancelled,
            ));
        }
        self.reverify_entrypoint()?;
        let spec = CodePluginLaunchSpec {
            id: self.id.clone(),
            version: self.version.clone(),
            package_digest: self.package_digest.clone(),
            executable_digest: self.executable_digest.clone(),
            executable_bytes: Arc::clone(&self.entrypoint_bytes),
            entrypoint: self.entrypoint_path.clone(),
            granted_capabilities: grants.granted.clone(),
        };
        let process = launcher.launch(&spec, cancellation)?;
        let inner = Arc::new(RuntimeInner::new(
            process,
            session_id,
            self.id.clone(),
            self.version.clone(),
            self.package_digest.clone(),
            self.executable_digest.clone(),
            grants.granted.clone(),
            self.contributions.clone(),
        ));
        let weak = Arc::downgrade(&inner);
        if let Err(fault) = inner.process.set_exit_listener(Arc::new(move |exit| {
            if let Some(inner) = weak.upgrade() {
                inner.retire(exit);
            }
        })) {
            inner.retire(CodePluginExit::Crashed);
            return Err(CodePluginError::Transport(fault));
        }
        let request = inner.initialize_frame()?;
        let response = match inner.process.exchange(&request, cancellation) {
            Ok(response) => response,
            Err(fault) => {
                inner.retire(CodePluginExit::Crashed);
                return Err(CodePluginError::Transport(fault));
            }
        };
        if cancellation.is_cancelled() {
            inner.retire(CodePluginExit::Cancelled);
            return Err(CodePluginError::Transport(
                CodePluginTransportFault::Cancelled,
            ));
        }
        if let Err(fault) = inner.admit_ready(&response) {
            inner.retire(CodePluginExit::Crashed);
            return Err(CodePluginError::Protocol(fault));
        }
        Ok(PreparedCodePlugin {
            id: self.id,
            version: self.version,
            package_digest: self.package_digest,
            executable_digest: self.executable_digest,
            contributions: self.contributions,
            inner,
        })
    }
}

impl fmt::Debug for CodePluginPackage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodePluginPackage")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("package_digest", &self.package_digest)
            .field("executable_digest", &self.executable_digest)
            .field("runtime", &self.runtime)
            .field("entrypoint", &self.entrypoint)
            .field(
                "requested_capability_count",
                &self.requested_capabilities.len(),
            )
            .field("contribution_count", &self.contributions.len())
            .finish()
    }
}

/// Explicit package-bound capability allowlist.
#[derive(Clone)]
pub struct CodePluginCapabilityGrants {
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    executable_digest: CodePluginExecutableDigest,
    granted: Vec<PluginPermission>,
}

impl CodePluginCapabilityGrants {
    /// Construct an explicit empty allowlist.
    #[must_use]
    pub fn deny_all(package: &CodePluginPackage) -> Self {
        Self {
            id: package.id.clone(),
            version: package.version.clone(),
            package_digest: package.package_digest.clone(),
            executable_digest: package.executable_digest.clone(),
            granted: Vec::new(),
        }
    }

    /// Grant an exact subset of permissions requested by the manifest.
    ///
    /// # Errors
    /// Duplicate or unrequested capabilities are denied.
    pub fn new(
        package: &CodePluginPackage,
        capabilities: impl IntoIterator<Item = PluginPermission>,
    ) -> Result<Self, CodePluginError> {
        let mut granted = BTreeSet::new();
        for capability in capabilities {
            if !package.requested_capabilities.contains(&capability) {
                return Err(CodePluginError::UnrequestedCapability(capability));
            }
            if !granted.insert(capability) {
                return Err(CodePluginError::DuplicateCapability(capability));
            }
        }
        Ok(Self {
            id: package.id.clone(),
            version: package.version.clone(),
            package_digest: package.package_digest.clone(),
            executable_digest: package.executable_digest.clone(),
            granted: granted.into_iter().collect(),
        })
    }

    /// Sorted explicit allowlist.
    #[must_use]
    pub fn as_slice(&self) -> &[PluginPermission] {
        &self.granted
    }

    pub(crate) fn verify(&self, package: &CodePluginPackage) -> Result<(), CodePluginError> {
        if self.id != package.id
            || self.version != package.version
            || self.package_digest != package.package_digest
            || self.executable_digest != package.executable_digest
        {
            return Err(CodePluginError::GrantMismatch);
        }
        Ok(())
    }

    pub(crate) fn granted(&self) -> &[PluginPermission] {
        &self.granted
    }
}

impl fmt::Debug for CodePluginCapabilityGrants {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodePluginCapabilityGrants")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("granted_count", &self.granted.len())
            .finish()
    }
}

/// Exact verified process launch input.
pub struct CodePluginLaunchSpec {
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    executable_digest: CodePluginExecutableDigest,
    executable_bytes: Arc<[u8]>,
    entrypoint: PathBuf,
    granted_capabilities: Vec<PluginPermission>,
}

impl CodePluginLaunchSpec {
    /// Package identity.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// Exact version.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        &self.version
    }

    /// Canonical package tree digest.
    #[must_use]
    pub const fn package_digest(&self) -> &PackageContentHash {
        &self.package_digest
    }

    /// Exact executable digest.
    #[must_use]
    pub const fn executable_digest(&self) -> &CodePluginExecutableDigest {
        &self.executable_digest
    }

    /// Exact reverified executable bytes the execution owner must bind at
    /// spawn, rather than reopening the ambient entrypoint path.
    #[must_use]
    pub fn executable_bytes(&self) -> &[u8] {
        &self.executable_bytes
    }

    /// Absolute executable path inside the verified cache object.
    #[must_use]
    pub fn entrypoint(&self) -> &Path {
        &self.entrypoint
    }

    /// Sorted capabilities the process sandbox may expose.
    #[must_use]
    pub fn granted_capabilities(&self) -> &[PluginPermission] {
        &self.granted_capabilities
    }
}

impl fmt::Debug for CodePluginLaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodePluginLaunchSpec")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("package_digest", &self.package_digest)
            .field("executable_digest", &self.executable_digest)
            .field("executable_bytes", &self.executable_bytes.len())
            .field("granted_capability_count", &self.granted_capabilities.len())
            .finish()
    }
}

/// Closed reason a code-plugin process became terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePluginExit {
    /// Process exited without a reported crash; contributions still retire.
    Exited,
    /// Process or protocol driver crashed.
    Crashed,
    /// Host cancelled or disposed the generation.
    Cancelled,
}

/// Closed body-free launch or exchange failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodePluginTransportFault {
    /// Process could not start.
    #[error("code plugin process could not start")]
    Spawn,
    /// Process became terminal.
    #[error("code plugin process terminated")]
    Crashed,
    /// Caller cancelled the operation.
    #[error("code plugin operation was cancelled")]
    Cancelled,
    /// Raw framing or driver protocol failed.
    #[error("code plugin transport protocol failed")]
    Protocol,
    /// Transport is unavailable.
    #[error("code plugin transport is unavailable")]
    Unavailable,
}

/// Process launcher supplied by the execution-world owner.
pub trait CodePluginLauncher: Send + Sync {
    /// Launch the exact executable with only the supplied capability grants.
    ///
    /// The implementation must use the composed process and sandbox path,
    /// clear ambient environment authority and settle spawn before returning.
    ///
    /// # Errors
    /// A closed body-free transport fault.
    fn launch(
        &self,
        spec: &CodePluginLaunchSpec,
        cancellation: &CancellationToken,
    ) -> Result<Arc<dyn CodePluginProcess>, CodePluginTransportFault>;
}

/// One quiescently-owned out-of-process protocol connection.
pub trait CodePluginProcess: Send + Sync {
    /// Exchange one bounded UTF-8 JSON request and response.
    ///
    /// # Errors
    /// A closed transport failure. Response bytes never enter the fault.
    fn exchange(
        &self,
        request: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, CodePluginTransportFault>;

    /// Install the terminal listener before initialization.
    ///
    /// An already-terminal process invokes the listener before returning.
    ///
    /// # Errors
    /// The listener or protocol driver could not be installed.
    fn set_exit_listener(
        &self,
        listener: Arc<dyn Fn(CodePluginExit) + Send + Sync>,
    ) -> Result<(), CodePluginTransportFault>;

    /// Cancel and synchronously reap the complete owned process tree.
    fn shutdown(&self);
}

/// One atomically published host contribution generation.
///
/// The host registers the complete slice in one operation. Withdrawal must be
/// idempotent and make the complete slice unavailable under one host-owned
/// generation commit; per-row publication is not a valid implementation.
pub trait CodePluginContributionGeneration: Send + Sync {
    /// Publish the complete prepared generation at the runtime commit point.
    ///
    /// The runtime calls this exactly once while process retirement is
    /// excluded. Implementations must make every proxy available through one
    /// generation gate; individual registry rows may already exist but must
    /// remain inactive before this call. Commit must not invoke the process or
    /// re-enter the runtime lifecycle lock.
    fn commit(&self);

    /// Withdraw the complete contribution generation.
    fn withdraw(&self);
}

/// Adapter from code-plugin claims to concrete product registries.
pub trait CodePluginContributionHost: Send + Sync {
    /// Services activation reads from the Context.
    fn required_services(&self) -> &'static [ServiceKey];

    /// Exact inventory rows derived only from the manifest-owned contribution.
    ///
    /// Process response annotations must never influence this mapping.
    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec>;

    /// Atomically activate one process's complete exact contribution set.
    ///
    /// # Errors
    /// A closed host failure. The host must publish nothing on failure.
    fn activate_generation(
        &self,
        context: &Context,
        client: CodePluginClient,
        contributions: &[CodePluginContribution],
    ) -> Result<Arc<dyn CodePluginContributionGeneration>, HostActivationFailure>;
}

/// Process whose handshake completed but whose host rows are not yet live.
pub struct PreparedCodePlugin {
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    executable_digest: CodePluginExecutableDigest,
    contributions: Vec<CodePluginContribution>,
    inner: Arc<RuntimeInner>,
}

impl PreparedCodePlugin {
    /// Package identity.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// Exact package version.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        &self.version
    }

    /// Exact manifest-owned contributions acknowledged by the process.
    #[must_use]
    pub fn contributions(&self) -> &[CodePluginContribution] {
        &self.contributions
    }
}

impl fmt::Debug for PreparedCodePlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCodePlugin")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("package_digest", &self.package_digest)
            .field("executable_digest", &self.executable_digest)
            .field("contribution_count", &self.contributions.len())
            .finish()
    }
}

/// Weak invocation handle installed into concrete contribution proxies.
#[derive(Clone)]
pub struct CodePluginClient {
    inner: Weak<RuntimeInner>,
}

impl CodePluginClient {
    /// Exact sorted capability set admitted for this process generation.
    ///
    /// Product adapters use this only while preparing their inactive rows;
    /// process annotations cannot add to it.
    ///
    /// # Errors
    /// A retired or dropped generation is closed.
    pub fn granted_capabilities(&self) -> Result<Vec<PluginPermission>, CodePluginInvocationError> {
        let inner = self
            .inner
            .upgrade()
            .ok_or(CodePluginInvocationError::Closed)?;
        if inner.lifecycle.is_cancelled() {
            return Err(CodePluginInvocationError::Closed);
        }
        Ok(inner.capabilities.clone())
    }

    /// Invoke one exact active contribution operation.
    ///
    /// Input and output are bounded JSON values. A transport or protocol
    /// failure retires the whole process generation because its correlation
    /// state is no longer trustworthy. A closed remote denial is an ordinary
    /// result and leaves the generation active.
    ///
    /// # Errors
    /// Closed/cancelled generation, unknown contribution, malformed operation
    /// or payload, closed remote denial, transport failure or protocol drift.
    pub fn invoke(
        &self,
        kind: ContributionKind,
        public_name: &str,
        operation: &str,
        input: Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, CodePluginInvocationError> {
        if cancellation.is_cancelled() {
            return Err(CodePluginInvocationError::Cancelled);
        }
        if !valid_kebab(operation) {
            return Err(CodePluginInvocationError::InvalidOperation);
        }
        validate_json(&input).map_err(|_| CodePluginInvocationError::InvalidPayload)?;
        let inner = self
            .inner
            .upgrade()
            .ok_or(CodePluginInvocationError::Closed)?;
        if !inner.is_active() {
            return Err(CodePluginInvocationError::Closed);
        }
        let contribution = WireContribution {
            kind,
            name: public_name.to_owned(),
        };
        if !inner
            .contributions
            .iter()
            .any(|known| known == &contribution)
        {
            return Err(CodePluginInvocationError::UnknownContribution);
        }
        let request_id = inner
            .next_request_id
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| CodePluginInvocationError::RequestIdsExhausted)?;
        let request = InvokeWire {
            protocol_version: CODE_PLUGIN_PROTOCOL_VERSION,
            kind: HostFrameKind::Invoke,
            session_id: inner.session_id.as_str(),
            request_id,
            contribution,
            operation,
            input,
        };
        let request =
            serde_json::to_vec(&request).map_err(|_| CodePluginInvocationError::InvalidPayload)?;
        if request.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
            return Err(CodePluginInvocationError::InvalidPayload);
        }
        let response = match inner.process.exchange(&request, cancellation) {
            Ok(response) => response,
            Err(fault) => {
                inner.retire(CodePluginExit::Crashed);
                return Err(CodePluginInvocationError::Transport(fault));
            }
        };
        if cancellation.is_cancelled() {
            inner.retire(CodePluginExit::Cancelled);
            return Err(CodePluginInvocationError::Cancelled);
        }
        if !inner.is_active() {
            return Err(CodePluginInvocationError::Closed);
        }
        match inner.admit_response(&response, request_id) {
            Ok(PluginResponseWire::Result { output, .. }) => Ok(output),
            Ok(PluginResponseWire::Error { code, .. }) => {
                Err(CodePluginInvocationError::Remote(code))
            }
            Err(fault) => {
                inner.retire(CodePluginExit::Crashed);
                Err(CodePluginInvocationError::Protocol(fault))
            }
        }
    }

    /// Cancel, withdraw and reap the complete process generation.
    pub fn cancel_generation(&self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.retire(CodePluginExit::Cancelled);
        }
    }
}

impl fmt::Debug for CodePluginClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodePluginClient")
            .field("reachable", &self.inner.strong_count().ne(&0))
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RuntimePhase {
    Starting,
    Ready,
    Active,
    Retired,
}

struct RuntimeState {
    phase: RuntimePhase,
    generation: Option<Arc<dyn CodePluginContributionGeneration>>,
}

struct RuntimeInner {
    process: Arc<dyn CodePluginProcess>,
    session_id: CodePluginSessionId,
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    executable_digest: CodePluginExecutableDigest,
    capabilities: Vec<PluginPermission>,
    contributions: Vec<WireContribution>,
    lifecycle: CancellationToken,
    next_request_id: AtomicU64,
    state: Mutex<RuntimeState>,
}

impl RuntimeInner {
    #[allow(clippy::too_many_arguments)]
    fn new(
        process: Arc<dyn CodePluginProcess>,
        session_id: CodePluginSessionId,
        id: PluginId,
        version: PluginVersion,
        package_digest: PackageContentHash,
        executable_digest: CodePluginExecutableDigest,
        capabilities: Vec<PluginPermission>,
        contributions: Vec<CodePluginContribution>,
    ) -> Self {
        Self {
            process,
            session_id,
            id,
            version,
            package_digest,
            executable_digest,
            capabilities,
            contributions: contributions
                .iter()
                .map(CodePluginContribution::wire)
                .collect(),
            lifecycle: CancellationToken::new(),
            next_request_id: AtomicU64::new(1),
            state: Mutex::new(RuntimeState {
                phase: RuntimePhase::Starting,
                generation: None,
            }),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, RuntimeState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn initialize_frame(&self) -> Result<Vec<u8>, CodePluginError> {
        let frame = InitializeWire {
            protocol_version: CODE_PLUGIN_PROTOCOL_VERSION,
            kind: HostFrameKind::Initialize,
            session_id: self.session_id.as_str(),
            package_id: self.id.as_str(),
            package_version: self.version.to_string(),
            package_digest: self.package_digest.as_str(),
            executable_digest: self.executable_digest.as_str(),
            granted_capabilities: &self.capabilities,
            contributions: &self.contributions,
        };
        let bytes = serde_json::to_vec(&frame)
            .map_err(|_| CodePluginError::Protocol(CodePluginProtocolFault::InvalidFrame))?;
        if bytes.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
            return Err(CodePluginError::Protocol(
                CodePluginProtocolFault::FrameTooLarge,
            ));
        }
        Ok(bytes)
    }

    fn admit_ready(&self, response: &[u8]) -> Result<(), CodePluginProtocolFault> {
        if response.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
            return Err(CodePluginProtocolFault::FrameTooLarge);
        }
        let ready: ReadyWire =
            serde_json::from_slice(response).map_err(|_| CodePluginProtocolFault::InvalidFrame)?;
        if ready.protocol_version != CODE_PLUGIN_PROTOCOL_VERSION
            || ready.kind != PluginFrameKind::Ready
            || ready.session_id != self.session_id.as_str()
            || ready.package_id != self.id.as_str()
            || ready.package_version != self.version.to_string()
            || ready.package_digest != self.package_digest.as_str()
            || ready.executable_digest != self.executable_digest.as_str()
        {
            return Err(CodePluginProtocolFault::IdentityMismatch);
        }
        if ready.accepted_capabilities != self.capabilities {
            return Err(CodePluginProtocolFault::CapabilityMismatch);
        }
        if ready.contributions != self.contributions {
            return Err(CodePluginProtocolFault::ContributionMismatch);
        }
        let mut state = self.lock_state();
        if state.phase != RuntimePhase::Starting {
            return Err(CodePluginProtocolFault::InvalidState);
        }
        state.phase = RuntimePhase::Ready;
        Ok(())
    }

    fn admit_response(
        &self,
        response: &[u8],
        request_id: u64,
    ) -> Result<PluginResponseWire, CodePluginProtocolFault> {
        if response.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
            return Err(CodePluginProtocolFault::FrameTooLarge);
        }
        let response: PluginResponseWire =
            serde_json::from_slice(response).map_err(|_| CodePluginProtocolFault::InvalidFrame)?;
        let (protocol_version, session_id, actual_request_id, output) = match &response {
            PluginResponseWire::Result {
                protocol_version,
                session_id,
                request_id,
                output,
            } => (
                *protocol_version,
                session_id.as_str(),
                *request_id,
                Some(output),
            ),
            PluginResponseWire::Error {
                protocol_version,
                session_id,
                request_id,
                ..
            } => (*protocol_version, session_id.as_str(), *request_id, None),
        };
        if protocol_version != CODE_PLUGIN_PROTOCOL_VERSION
            || session_id != self.session_id.as_str()
            || actual_request_id != request_id
        {
            return Err(CodePluginProtocolFault::CorrelationMismatch);
        }
        if let Some(output) = output {
            validate_json(output)?;
        }
        Ok(response)
    }

    fn is_active(&self) -> bool {
        self.lock_state().phase == RuntimePhase::Active && !self.lifecycle.is_cancelled()
    }

    fn attach(
        &self,
        generation: Arc<dyn CodePluginContributionGeneration>,
    ) -> Result<(), CodePluginError> {
        let mut state = self.lock_state();
        if state.phase != RuntimePhase::Ready || self.lifecycle.is_cancelled() {
            drop(state);
            let _ = catch_unwind(AssertUnwindSafe(|| generation.withdraw()));
            return Err(CodePluginError::Closed);
        }
        state.generation = Some(Arc::clone(&generation));
        state.phase = RuntimePhase::Active;
        if catch_unwind(AssertUnwindSafe(|| generation.commit())).is_err() {
            state.phase = RuntimePhase::Ready;
            state.generation = None;
            drop(state);
            let _ = catch_unwind(AssertUnwindSafe(|| generation.withdraw()));
            return Err(CodePluginError::Closed);
        }
        Ok(())
    }

    fn client(inner: &Arc<Self>) -> CodePluginClient {
        CodePluginClient {
            inner: Arc::downgrade(inner),
        }
    }

    fn retire(&self, _exit: CodePluginExit) {
        let generation = {
            let mut state = self.lock_state();
            if state.phase == RuntimePhase::Retired {
                return;
            }
            state.phase = RuntimePhase::Retired;
            state.generation.take()
        };
        self.lifecycle.cancel();
        if let Some(generation) = generation {
            let _ = catch_unwind(AssertUnwindSafe(|| generation.withdraw()));
        }
        let _ = catch_unwind(AssertUnwindSafe(|| self.process.shutdown()));
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.retire(CodePluginExit::Cancelled);
    }
}

/// Build one aggregate core plugin for a complete prepared process generation.
///
/// Package ids and exact PL03 claims are collision-checked before composition.
/// Each process registers one Context disposer before its atomic host
/// generation publishes. A host refusal, later plugin failure, process exit,
/// protocol failure, cancellation or Context shutdown therefore converges on
/// the same withdrawal and quiescent process cleanup path.
///
/// # Errors
/// Duplicate packages or contribution claims are refused before composition.
pub fn code_plugin_activation_plugin(
    plugins: Vec<PreparedCodePlugin>,
    host: Arc<dyn CodePluginContributionHost>,
) -> Result<Box<dyn Plugin>, CodePluginError> {
    let mut ids = BTreeSet::new();
    let mut claims = BTreeSet::new();
    for plugin in &plugins {
        if !ids.insert(plugin.id.clone()) {
            return Err(CodePluginError::DuplicatePackage(plugin.id.clone()));
        }
        for contribution in &plugin.contributions {
            if !claims.insert((contribution.kind, contribution.public_name.clone())) {
                return Err(CodePluginError::DuplicateContribution {
                    kind: contribution.kind,
                    name: contribution.public_name.clone(),
                });
            }
        }
    }
    Ok(Box::new(CodePluginActivationPlugin { plugins, host }))
}

struct CodePluginActivationPlugin {
    plugins: Vec<PreparedCodePlugin>,
    host: Arc<dyn CodePluginContributionHost>,
}

impl Plugin for CodePluginActivationPlugin {
    fn name(&self) -> &'static str {
        CODE_PLUGIN_ACTIVATION_PLUGIN_ID
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            CODE_PLUGIN_ACTIVATION_PLUGIN_ID,
            env!("CARGO_PKG_VERSION"),
            CODE_PLUGIN_FAMILIES,
        )
    }

    fn inject(&self) -> &'static [ServiceKey] {
        self.host.required_services()
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        let mut rows = Vec::new();
        for plugin in &self.plugins {
            rows.push(PluginContributionSpec::new(
                heycode_core::ContributionKind::ExternalProcess,
                plugin.id.as_str(),
            ));
            for contribution in &plugin.contributions {
                rows.extend(self.host.inventory(contribution));
            }
        }
        rows
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        for plugin in &self.plugins {
            {
                let state = plugin.inner.lock_state();
                if state.phase != RuntimePhase::Ready {
                    return Err(CoreError::other(CodePluginError::Closed.to_string()));
                }
            }
            let inner = Arc::clone(&plugin.inner);
            context.effect(move || inner.retire(CodePluginExit::Cancelled));
            let generation = self
                .host
                .activate_generation(
                    context,
                    RuntimeInner::client(&plugin.inner),
                    &plugin.contributions,
                )
                .map_err(|failure| {
                    CoreError::other(
                        CodePluginError::Host {
                            package: plugin.id.clone(),
                            failure,
                        }
                        .to_string(),
                    )
                })?;
            plugin
                .inner
                .attach(generation)
                .map_err(|error| CoreError::other(error.to_string()))?;
        }
        Ok(())
    }
}

/// Stable code-plugin preparation and activation failures.
#[derive(Debug, Error)]
pub enum CodePluginError {
    /// PL02 cache resolution or validation failed.
    #[error(transparent)]
    Cache(#[from] PackageCacheError),
    /// PL05 provenance re-verification failed.
    #[error(transparent)]
    Marketplace(#[from] MarketplaceError),
    /// Bounded immutable contribution-document admission failed.
    #[error(transparent)]
    Declarative(#[from] DeclarativePluginError),
    /// Code execution requires host-established package provenance.
    #[error("code plugin package provenance is unknown")]
    UnknownProvenance,
    /// Entrypoint text is not a portable package-relative path.
    #[error("code plugin entrypoint is invalid")]
    InvalidEntrypoint,
    /// Product activation requires an explicit manifest runtime/entrypoint.
    #[error("code plugin manifest has no code runtime declaration")]
    MissingCodeDeclaration,
    /// A caller attempted to override manifest-owned runtime/entrypoint facts.
    #[error("code plugin runtime or entrypoint differs from its manifest")]
    CodeDeclarationMismatch,
    /// Entrypoint is absent, non-executable, unsafe or changed.
    #[error("code plugin entrypoint is unsafe or changed")]
    UnsafeEntrypoint,
    /// A process with no PL03 contribution would publish no product capability.
    #[error("code plugin package has no activatable contribution")]
    NoContributions,
    /// Capability was not requested by the exact manifest.
    #[error("code plugin capability was not requested by the manifest")]
    UnrequestedCapability(PluginPermission),
    /// Capability appeared twice in the explicit grant.
    #[error("code plugin capability grant contains a duplicate")]
    DuplicateCapability(PluginPermission),
    /// Grant was constructed for another package or executable generation.
    #[error("code plugin capability grant does not match the package generation")]
    GrantMismatch,
    /// Launch or raw process exchange failed.
    #[error(transparent)]
    Transport(#[from] CodePluginTransportFault),
    /// Closed protocol validation failed.
    #[error(transparent)]
    Protocol(CodePluginProtocolFault),
    /// Active generation contains the same package twice.
    #[error("code plugin generation contains package {0} more than once")]
    DuplicatePackage(PluginId),
    /// Active generation contains the same exact PL03 claim twice.
    #[error("code plugin generation contains duplicate {kind} contribution {name}")]
    DuplicateContribution {
        /// Exact contribution registry.
        kind: ContributionKind,
        /// Validated public name.
        name: String,
    },
    /// Concrete host refused the atomic contribution generation.
    #[error("code plugin {package} contribution generation failed: {failure}")]
    Host {
        /// Validated package id.
        package: PluginId,
        /// Closed body-free host failure.
        failure: HostActivationFailure,
    },
    /// Process exited before activation committed.
    #[error("code plugin process generation is closed")]
    Closed,
}

/// Closed protocol validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodePluginProtocolFault {
    /// JSON frame exceeded the fixed byte ceiling.
    #[error("code plugin protocol frame is too large")]
    FrameTooLarge,
    /// JSON frame, discriminator or field set is invalid.
    #[error("code plugin protocol frame is invalid")]
    InvalidFrame,
    /// Version, session, package identity or digest differs.
    #[error("code plugin protocol identity does not match")]
    IdentityMismatch,
    /// Process acknowledged a different capability set.
    #[error("code plugin protocol capability acknowledgement does not match")]
    CapabilityMismatch,
    /// Process contribution claims differ from the manifest.
    #[error("code plugin protocol contribution acknowledgement does not match")]
    ContributionMismatch,
    /// Response request or session correlation differs.
    #[error("code plugin protocol response correlation does not match")]
    CorrelationMismatch,
    /// JSON value exceeded the depth or node ceiling.
    #[error("code plugin protocol JSON value is invalid")]
    InvalidPayload,
    /// Process lifecycle transition is invalid.
    #[error("code plugin protocol lifecycle state is invalid")]
    InvalidState,
}

/// Closed process-reported invocation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodePluginRemoteErrorCode {
    /// Operation or input is invalid.
    InvalidRequest,
    /// Process policy denied the operation.
    Denied,
    /// Required capability or dependency is unavailable.
    Unavailable,
    /// Process failed without a safe detail.
    Failed,
}

impl fmt::Display for CodePluginRemoteErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::InvalidRequest => "invalid_request",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        };
        formatter.write_str(value)
    }
}

/// Stable invocation failure with no request or response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodePluginInvocationError {
    /// Process generation is no longer active.
    #[error("code plugin process generation is closed")]
    Closed,
    /// Caller cancelled before request admission.
    #[error("code plugin invocation was cancelled")]
    Cancelled,
    /// Contribution was not part of the exact ready generation.
    #[error("code plugin contribution is not active")]
    UnknownContribution,
    /// Operation id is not valid lowercase kebab-case.
    #[error("code plugin operation id is invalid")]
    InvalidOperation,
    /// Input or output exceeded JSON bounds.
    #[error("code plugin invocation payload is invalid")]
    InvalidPayload,
    /// Request id space was exhausted.
    #[error("code plugin request id space is exhausted")]
    RequestIdsExhausted,
    /// Transport failed and the generation was retired.
    #[error(transparent)]
    Transport(CodePluginTransportFault),
    /// Protocol failed and the generation was retired.
    #[error(transparent)]
    Protocol(CodePluginProtocolFault),
    /// Process returned one closed safe error code.
    #[error("code plugin invocation failed: {0}")]
    Remote(CodePluginRemoteErrorCode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HostFrameKind {
    Initialize,
    Invoke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PluginFrameKind {
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireContribution {
    kind: ContributionKind,
    name: String,
}

#[derive(Serialize)]
struct InitializeWire<'a> {
    protocol_version: u32,
    kind: HostFrameKind,
    session_id: &'a str,
    package_id: &'a str,
    package_version: String,
    package_digest: &'a str,
    executable_digest: &'a str,
    granted_capabilities: &'a [PluginPermission],
    contributions: &'a [WireContribution],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyWire {
    protocol_version: u32,
    kind: PluginFrameKind,
    session_id: String,
    package_id: String,
    package_version: String,
    package_digest: String,
    executable_digest: String,
    accepted_capabilities: Vec<PluginPermission>,
    contributions: Vec<WireContribution>,
}

#[derive(Serialize)]
struct InvokeWire<'a> {
    protocol_version: u32,
    kind: HostFrameKind,
    session_id: &'a str,
    request_id: u64,
    contribution: WireContribution,
    operation: &'a str,
    input: Value,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PluginResponseWire {
    Result {
        protocol_version: u32,
        session_id: String,
        request_id: u64,
        output: Value,
    },
    Error {
        protocol_version: u32,
        session_id: String,
        request_id: u64,
        code: CodePluginRemoteErrorCode,
    },
}

fn validate_json(value: &Value) -> Result<(), CodePluginProtocolFault> {
    let mut stack = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes
            .checked_add(1)
            .ok_or(CodePluginProtocolFault::InvalidPayload)?;
        if nodes > MAX_JSON_NODES || depth > MAX_JSON_DEPTH {
            return Err(CodePluginProtocolFault::InvalidPayload);
        }
        match value {
            Value::Array(values) => {
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                stack.extend(values.values().map(|value| (value, depth + 1)));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

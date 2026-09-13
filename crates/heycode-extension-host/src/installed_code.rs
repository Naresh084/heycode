//! Explicit installed-code authority and combined product activation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heycode_core::{
    Context, CoreError, Plugin, PluginContributionSpec, PluginDescriptor, ServiceKey,
};
use heycode_extensions::lifecycle::PluginState;
use heycode_extensions::{
    CodePluginCancellationToken, CodePluginCapabilityGrants, CodePluginPackage,
    CodePluginSessionId, DeclarativePackage, ManagedDeclarativePackage,
    ManagedPluginAdmissionGeneration, ManagedPluginAdmissionGenerationId, ManifestValidator,
    PackageContentHash, PackageProvenance, PluginCodeRuntime, PluginId, PluginInstallCache,
    PluginPath, PluginPermission, PluginVersion, PreparedCodePlugin, WasiComponentCapabilityPolicy,
    WasiComponentEngine, WasiComponentPackage, WasiNetworkEndpoint, WasiPreopen, WasiPreopenAccess,
    code_plugin_activation_plugin,
};
use thiserror::Error;

use crate::code_plugin_host::ProductCodePluginHost;
use crate::code_plugin_process::HeycodeExecCodePluginLauncher;
use crate::{
    INSTALLED_PRODUCT_SERVICES, PRODUCT_EXTENSIONS_PLUGIN_ID, PRODUCT_FAMILIES,
    ProductExtensionError, ProductExtensionsPlugin, activate_bundled_mcp, preflight_mcp_claims,
};

const MAX_MANAGED_CODE_AUTHORITY_ROWS: usize = 128;
const MAX_MANAGED_WASI_RESOURCES: usize = 64;

/// Session issuance policy authorized by a managed profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedCodePluginSessionPolicy {
    /// Mint a fresh unpredictable id for every activation attempt.
    UniquePerActivation,
}

/// One deferred WASI preopen; object identity is resolved only during apply.
#[derive(Clone, PartialEq, Eq)]
pub struct ManagedWasiPreopenSpec {
    host_path: PathBuf,
    guest_path: String,
    access: WasiPreopenAccess,
}

impl ManagedWasiPreopenSpec {
    /// Validate one zero-I/O preopen request.
    ///
    /// The host directory is canonicalized and object-bound later, inside
    /// plugin apply after PL08 package re-admission.
    ///
    /// # Errors
    /// Relative/lossy host paths or non-portable guest paths are refused.
    pub fn new(
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        access: WasiPreopenAccess,
    ) -> Result<Self, InstalledCodePluginError> {
        let host_path = host_path.into();
        let guest_path = guest_path.into();
        if !valid_host_path(&host_path) || !valid_guest_path(&guest_path) {
            return Err(InstalledCodePluginError::InvalidAuthorityGeneration);
        }
        Ok(Self {
            host_path,
            guest_path,
            access,
        })
    }
}

impl fmt::Debug for ManagedWasiPreopenSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedWasiPreopenSpec")
            .field("host_path", &"[REDACTED]")
            .field("guest_path", &self.guest_path)
            .field("access", &self.access)
            .finish()
    }
}

/// One deferred exact outbound IP endpoint.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ManagedWasiNetworkEndpointSpec {
    address: IpAddr,
    port: u16,
}

impl ManagedWasiNetworkEndpointSpec {
    /// Validate one exact TCP endpoint without DNS authority.
    ///
    /// # Errors
    /// Port zero is refused.
    pub const fn new(address: IpAddr, port: u16) -> Result<Self, InstalledCodePluginError> {
        if port == 0 {
            return Err(InstalledCodePluginError::InvalidAuthorityGeneration);
        }
        Ok(Self { address, port })
    }
}

impl fmt::Debug for ManagedWasiNetworkEndpointSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedWasiNetworkEndpointSpec")
            .field("address", &"[REDACTED]")
            .field("port", &self.port)
            .finish()
    }
}

/// Runtime-specific resource requests from one trusted managed rule.
#[derive(Clone)]
pub enum ManagedCodePluginResourceSpec {
    /// Native execution receives no WASI resource rows.
    NativeProcess,
    /// WASI receives only these deferred exact resources.
    WasiComponentV1 {
        /// Exact filesystem preopens.
        preopens: Vec<ManagedWasiPreopenSpec>,
        /// Exact outbound IP endpoints.
        network_endpoints: Vec<ManagedWasiNetworkEndpointSpec>,
    },
}

impl ManagedCodePluginResourceSpec {
    const fn runtime(&self) -> PluginCodeRuntime {
        match self {
            Self::NativeProcess => PluginCodeRuntime::NativeProcess,
            Self::WasiComponentV1 { .. } => PluginCodeRuntime::WasiComponentV1,
        }
    }

    fn validate(
        &self,
        id: &PluginId,
        grants: &BTreeSet<PluginPermission>,
    ) -> Result<(), InstalledCodePluginError> {
        let Self::WasiComponentV1 {
            preopens,
            network_endpoints,
        } = self
        else {
            return Ok(());
        };
        if preopens.len() > MAX_MANAGED_WASI_RESOURCES
            || network_endpoints.len() > MAX_MANAGED_WASI_RESOURCES
            || grants.iter().any(|grant| {
                matches!(
                    grant,
                    PluginPermission::ProcessSpawn
                        | PluginPermission::CredentialUse
                        | PluginPermission::McpConnect
                )
            })
        {
            return Err(InstalledCodePluginError::ResourceMismatch(id.clone()));
        }
        let mut host_paths = BTreeSet::new();
        let mut guest_paths = BTreeSet::new();
        for preopen in preopens {
            if preopen.access == WasiPreopenAccess::WriteOnly {
                return Err(InstalledCodePluginError::ResourceMismatch(id.clone()));
            }
            if !host_paths.insert(preopen.host_path.as_path())
                || !guest_paths.insert(preopen.guest_path.as_str())
            {
                return Err(InstalledCodePluginError::ResourceMismatch(id.clone()));
            }
            if can_read(preopen.access) && !grants.contains(&PluginPermission::FilesystemRead) {
                return Err(InstalledCodePluginError::ResourceMismatch(id.clone()));
            }
            if can_write(preopen.access) && !grants.contains(&PluginPermission::FilesystemWrite) {
                return Err(InstalledCodePluginError::ResourceMismatch(id.clone()));
            }
        }
        let readable = preopens.iter().any(|row| can_read(row.access));
        let writable = preopens.iter().any(|row| can_write(row.access));
        if grants.contains(&PluginPermission::FilesystemRead) != readable
            || grants.contains(&PluginPermission::FilesystemWrite) != writable
            || grants.contains(&PluginPermission::NetworkAccess) != !network_endpoints.is_empty()
        {
            return Err(InstalledCodePluginError::ResourceMismatch(id.clone()));
        }
        let unique = network_endpoints.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != network_endpoints.len() {
            return Err(InstalledCodePluginError::ResourceMismatch(id.clone()));
        }
        Ok(())
    }

    fn materialize(
        &self,
        id: &PluginId,
    ) -> Result<InstalledCodePluginResources, InstalledCodePluginError> {
        match self {
            Self::NativeProcess => Ok(InstalledCodePluginResources::NativeProcess),
            Self::WasiComponentV1 {
                preopens,
                network_endpoints,
            } => {
                let preopens = preopens
                    .iter()
                    .map(|row| {
                        WasiPreopen::new(&row.host_path, &row.guest_path, row.access)
                            .map_err(|_| InstalledCodePluginError::ResourceMismatch(id.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let network_endpoints = network_endpoints
                    .iter()
                    .map(|row| {
                        WasiNetworkEndpoint::new(row.address.to_string(), row.port)
                            .map_err(|_| InstalledCodePluginError::ResourceMismatch(id.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(InstalledCodePluginResources::WasiComponentV1 {
                    preopens,
                    network_endpoints,
                })
            }
        }
    }
}

impl fmt::Debug for ManagedCodePluginResourceSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NativeProcess => formatter.write_str("NativeProcess"),
            Self::WasiComponentV1 {
                preopens,
                network_endpoints,
            } => formatter
                .debug_struct("WasiComponentV1")
                .field("preopen_count", &preopens.len())
                .field("network_endpoint_count", &network_endpoints.len())
                .finish(),
        }
    }
}

/// Exact trusted managed rule for one installed package generation.
#[derive(Clone)]
pub struct ManagedCodePluginAuthorityRule {
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    runtime: PluginCodeRuntime,
    entrypoint: PluginPath,
    granted_capabilities: Vec<PluginPermission>,
    session_policy: ManagedCodePluginSessionPolicy,
    resources: ManagedCodePluginResourceSpec,
}

impl ManagedCodePluginAuthorityRule {
    /// Validate one exact managed code-authority row without filesystem I/O.
    ///
    /// # Errors
    /// Duplicate grants, runtime/resource disagreement or a widening WASI
    /// resource shape is refused.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: PluginId,
        version: PluginVersion,
        package_digest: PackageContentHash,
        runtime: PluginCodeRuntime,
        entrypoint: PluginPath,
        granted_capabilities: impl IntoIterator<Item = PluginPermission>,
        session_policy: ManagedCodePluginSessionPolicy,
        resources: ManagedCodePluginResourceSpec,
    ) -> Result<Self, InstalledCodePluginError> {
        if runtime != resources.runtime() {
            return Err(InstalledCodePluginError::RuntimeMismatch(id));
        }
        let mut grants = BTreeSet::new();
        for grant in granted_capabilities {
            if !grants.insert(grant) {
                return Err(InstalledCodePluginError::DuplicateGrant(id));
            }
        }
        resources.validate(&id, &grants)?;
        Ok(Self {
            id,
            version,
            package_digest,
            runtime,
            entrypoint,
            granted_capabilities: grants.into_iter().collect(),
            session_policy,
            resources,
        })
    }
}

impl fmt::Debug for ManagedCodePluginAuthorityRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedCodePluginAuthorityRule")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("package_digest", &"[SHA256]")
            .field("runtime", &self.runtime)
            .field("entrypoint", &self.entrypoint)
            .field("grant_count", &self.granted_capabilities.len())
            .field("session_policy", &self.session_policy)
            .field("resources", &self.resources)
            .finish()
    }
}

/// Complete exact code-authority generation from one managed profile layer.
#[derive(Clone)]
pub struct ManagedCodePluginAuthorityGeneration {
    pl08_generation: ManagedPluginAdmissionGenerationId,
    rules: BTreeMap<PluginId, ManagedCodePluginAuthorityRule>,
}

impl ManagedCodePluginAuthorityGeneration {
    /// Construct one complete deterministic generation.
    ///
    /// # Errors
    /// Duplicate package ids or more than the stable row cap are refused.
    pub fn new(
        pl08_generation: ManagedPluginAdmissionGenerationId,
        rules: impl IntoIterator<Item = ManagedCodePluginAuthorityRule>,
    ) -> Result<Self, InstalledCodePluginError> {
        let mut by_id = BTreeMap::new();
        for rule in rules {
            let id = rule.id.clone();
            if by_id.insert(id.clone(), rule).is_some() {
                return Err(InstalledCodePluginError::DuplicateAuthority(id));
            }
            if by_id.len() > MAX_MANAGED_CODE_AUTHORITY_ROWS {
                return Err(InstalledCodePluginError::InvalidAuthorityGeneration);
            }
        }
        Ok(Self {
            pl08_generation,
            rules: by_id,
        })
    }
}

impl fmt::Debug for ManagedCodePluginAuthorityGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedCodePluginAuthorityGeneration")
            .field("pl08_generation", &self.pl08_generation)
            .field("rule_count", &self.rules.len())
            .finish()
    }
}

/// Production provider joining trusted code rules to current PL08 evidence.
#[derive(Clone)]
pub struct ManagedInstalledCodePluginAuthorityProvider {
    admission: ManagedPluginAdmissionGeneration,
    authority: ManagedCodePluginAuthorityGeneration,
}

impl ManagedInstalledCodePluginAuthorityProvider {
    /// Bind matching PL08 and code-authority generations without filesystem I/O.
    ///
    /// # Errors
    /// A stale profile fingerprint is refused immediately.
    pub fn new(
        admission: ManagedPluginAdmissionGeneration,
        authority: ManagedCodePluginAuthorityGeneration,
    ) -> Result<Self, InstalledCodePluginError> {
        if admission.id() != &authority.pl08_generation {
            return Err(InstalledCodePluginError::StaleManagedGeneration);
        }
        Ok(Self {
            admission,
            authority,
        })
    }

    fn issue_session(policy: ManagedCodePluginSessionPolicy) -> CodePluginSessionId {
        match policy {
            ManagedCodePluginSessionPolicy::UniquePerActivation => {
                CodePluginSessionId::from_u128(uuid::Uuid::new_v4().as_u128())
            }
        }
    }
}

impl fmt::Debug for ManagedInstalledCodePluginAuthorityProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedInstalledCodePluginAuthorityProvider")
            .field("admission", &self.admission)
            .field("authority", &self.authority)
            .finish()
    }
}

/// Runtime-specific resources explicitly minted by the host.
#[derive(Clone)]
pub enum InstalledCodePluginResources {
    /// Native process receives only the current composed sandbox policy under
    /// its exact permission ceiling; no additional path/endpoint is inferred.
    NativeProcess,
    /// WASI receives only these exact preopens and outbound endpoints.
    WasiComponentV1 {
        /// Exact filesystem preopens.
        preopens: Vec<WasiPreopen>,
        /// Exact outbound network endpoints.
        network_endpoints: Vec<WasiNetworkEndpoint>,
    },
}

impl InstalledCodePluginResources {
    const fn runtime(&self) -> PluginCodeRuntime {
        match self {
            Self::NativeProcess => PluginCodeRuntime::NativeProcess,
            Self::WasiComponentV1 { .. } => PluginCodeRuntime::WasiComponentV1,
        }
    }
}

impl fmt::Debug for InstalledCodePluginResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NativeProcess => formatter.write_str("NativeProcess"),
            Self::WasiComponentV1 {
                preopens,
                network_endpoints,
            } => formatter
                .debug_struct("WasiComponentV1")
                .field("preopen_count", &preopens.len())
                .field("network_endpoint_count", &network_endpoints.len())
                .finish(),
        }
    }
}

/// Exact host authority for one installed code package generation.
#[derive(Clone)]
pub struct InstalledCodePluginAuthority {
    provenance: PackageProvenance,
    session_id: CodePluginSessionId,
    granted_capabilities: Vec<PluginPermission>,
    resources: InstalledCodePluginResources,
}

impl InstalledCodePluginAuthority {
    /// Bind authority only from a package re-admitted by the current PL08
    /// managed-policy generation.
    ///
    /// This is the production constructor. [`Self::new`] remains available to
    /// an explicit operator-path embedding boundary, but the default product
    /// root should use this method so policy approval and activation bytes are
    /// joined by the package digest.
    ///
    /// # Errors
    /// A non-code package, non-Allowed policy generation, or invalid explicit
    /// grant set is refused.
    pub fn from_managed(
        package: &ManagedDeclarativePackage,
        session_id: CodePluginSessionId,
        granted_capabilities: impl IntoIterator<Item = PluginPermission>,
        resources: InstalledCodePluginResources,
    ) -> Result<Self, InstalledCodePluginError> {
        if package.manifest().code().is_none() || !package.evaluation().is_allowed() {
            return Err(InstalledCodePluginError::ManagedAuthorityRequired(
                package.manifest().id().clone(),
            ));
        }
        Self::new(
            package.provenance().clone(),
            session_id,
            granted_capabilities,
            resources,
        )
    }

    /// Bind established provenance, a host-minted session, an explicit grant
    /// set and runtime-specific resources.
    ///
    /// No permission or resource is defaulted from the manifest. The manifest
    /// only states what may be requested; this object states what the host
    /// actually admitted for this activation.
    ///
    /// # Errors
    /// Unknown provenance or a duplicate grant is refused before composition.
    pub fn new(
        provenance: PackageProvenance,
        session_id: CodePluginSessionId,
        granted_capabilities: impl IntoIterator<Item = PluginPermission>,
        resources: InstalledCodePluginResources,
    ) -> Result<Self, InstalledCodePluginError> {
        if !provenance.is_established() {
            return Err(InstalledCodePluginError::UnknownProvenance(
                provenance.id().clone(),
            ));
        }
        let mut unique = BTreeSet::new();
        for capability in granted_capabilities {
            if !unique.insert(capability) {
                return Err(InstalledCodePluginError::DuplicateGrant(
                    provenance.id().clone(),
                ));
            }
        }
        Ok(Self {
            provenance,
            session_id,
            granted_capabilities: unique.into_iter().collect(),
            resources,
        })
    }

    /// Exact package identity.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        self.provenance.id()
    }

    /// Exact package version.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        self.provenance.version()
    }

    /// Host-minted process/component session identity.
    #[must_use]
    pub const fn session_id(&self) -> &CodePluginSessionId {
        &self.session_id
    }

    /// Exact sorted host-admitted capabilities.
    #[must_use]
    pub fn granted_capabilities(&self) -> &[PluginPermission] {
        &self.granted_capabilities
    }

    /// Exact runtime-specific resource set.
    #[must_use]
    pub const fn resources(&self) -> &InstalledCodePluginResources {
        &self.resources
    }
}

impl fmt::Debug for InstalledCodePluginAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstalledCodePluginAuthority")
            .field("id", self.id())
            .field("version", self.version())
            .field("session_id", &self.session_id)
            .field("granted_count", &self.granted_capabilities.len())
            .field("resources", &self.resources)
            .finish()
    }
}

/// Closed installed-code activation failure.
#[derive(Debug, Error)]
pub enum InstalledCodePluginError {
    /// Managed code rules did not bind the current PL08 generation.
    #[error("installed code plugin authority uses a stale managed generation")]
    StaleManagedGeneration,
    /// Managed code authority exceeded a bound or contained an invalid shape.
    #[error("installed code plugin authority generation is invalid")]
    InvalidAuthorityGeneration,
    /// Provenance has no host-held origin evidence.
    #[error("installed code plugin {0} has no established provenance")]
    UnknownProvenance(PluginId),
    /// Production authority did not come from a current all-Allowed PL08 row.
    #[error("installed code plugin {0} requires current managed admission")]
    ManagedAuthorityRequired(PluginId),
    /// One explicit permission appeared twice.
    #[error("installed code plugin {0} has a duplicate capability grant")]
    DuplicateGrant(PluginId),
    /// One package has multiple authority rows.
    #[error("installed code plugin {0} has duplicate activation authority")]
    DuplicateAuthority(PluginId),
    /// Two package generations reused one process/component session id.
    #[error("installed code plugin activation reused a session identity")]
    DuplicateSession,
    /// A code package is enabled but has no explicit authority row.
    #[error("installed code plugin {0} has no activation authority")]
    MissingAuthority(PluginId),
    /// Authority named a package that is not enabled at the same version.
    #[error("installed code plugin {0} authority does not match lifecycle state")]
    LifecycleMismatch(PluginId),
    /// Manifest runtime and host resource plan disagree.
    #[error("installed code plugin {0} runtime does not match its resource plan")]
    RuntimeMismatch(PluginId),
    /// Managed package digest differs from freshly re-admitted PL08 bytes.
    #[error("installed code plugin {0} package digest does not match managed authority")]
    PackageDigestMismatch(PluginId),
    /// Managed entrypoint differs from the freshly re-admitted manifest.
    #[error("installed code plugin {0} entrypoint does not match managed authority")]
    EntrypointMismatch(PluginId),
    /// A managed grant was not requested by the freshly admitted manifest.
    #[error("installed code plugin {0} capability grant does not match managed authority")]
    CapabilityMismatch(PluginId),
    /// WASI resources and exact grants disagree or could not be object-bound.
    #[error("installed code plugin {0} runtime resources do not match managed authority")]
    ResourceMismatch(PluginId),
    /// Cache/provenance/document/grant loading failed.
    #[error("installed code plugin {0} package admission failed")]
    PackageAdmission(PluginId),
    /// Native process or component launch/handshake failed.
    #[error("installed code plugin {0} runtime activation failed")]
    RuntimeActivation(PluginId),
    /// Lifecycle or product registries are unavailable.
    #[error("installed code plugin host is unavailable")]
    HostUnavailable,
    /// The pinned WASI engine could not be constructed.
    #[error("installed code plugin WASI runtime is unavailable")]
    WasiRuntimeUnavailable,
}

/// Deferred source of exact installed-code authority rows.
///
/// Resolution runs inside `product-extensions` apply after the lazy PL02 cache
/// has opened and lifecycle state is available. A production implementation
/// can therefore call `prepare_managed_declarative` against its current PL08
/// source/catalog/policy generation without performing cache I/O during factory
/// inspection.
pub trait InstalledCodePluginAuthorityProvider: Send + Sync {
    /// Resolve the complete authority generation for the enabled lifecycle
    /// snapshot.
    ///
    /// Returning no row for an enabled code package fails later as
    /// [`InstalledCodePluginError::MissingAuthority`]; extra, duplicate or
    /// version-mismatched rows also fail before registry publication.
    ///
    /// # Errors
    /// Only closed [`InstalledCodePluginError`] values may cross this boundary.
    fn authorities(
        &self,
        cache: &PluginInstallCache,
        enabled: &[PluginState],
    ) -> Result<Vec<InstalledCodePluginAuthority>, InstalledCodePluginError>;
}

impl<F> InstalledCodePluginAuthorityProvider for F
where
    F: Fn(
            &PluginInstallCache,
            &[PluginState],
        ) -> Result<Vec<InstalledCodePluginAuthority>, InstalledCodePluginError>
        + Send
        + Sync,
{
    fn authorities(
        &self,
        cache: &PluginInstallCache,
        enabled: &[PluginState],
    ) -> Result<Vec<InstalledCodePluginAuthority>, InstalledCodePluginError> {
        self(cache, enabled)
    }
}

impl InstalledCodePluginAuthorityProvider for ManagedInstalledCodePluginAuthorityProvider {
    fn authorities(
        &self,
        cache: &PluginInstallCache,
        enabled: &[PluginState],
    ) -> Result<Vec<InstalledCodePluginAuthority>, InstalledCodePluginError> {
        let mut enabled_ids = BTreeSet::new();
        let mut consumed = BTreeSet::new();
        let mut authorities = Vec::new();
        for state in enabled {
            if !enabled_ids.insert(state.id.clone()) {
                return Err(InstalledCodePluginError::LifecycleMismatch(
                    state.id.clone(),
                ));
            }
            let installed = cache
                .resolve(&state.id, &state.active)
                .map_err(|_| InstalledCodePluginError::PackageAdmission(state.id.clone()))?;
            let Some(code) = installed.manifest().code() else {
                if self.authority.rules.contains_key(&state.id) {
                    return Err(InstalledCodePluginError::LifecycleMismatch(
                        state.id.clone(),
                    ));
                }
                continue;
            };
            let rule = self
                .authority
                .rules
                .get(&state.id)
                .ok_or_else(|| InstalledCodePluginError::MissingAuthority(state.id.clone()))?;
            if rule.version != state.active {
                return Err(InstalledCodePluginError::LifecycleMismatch(
                    state.id.clone(),
                ));
            }
            let package = self
                .admission
                .prepare_managed_declarative(cache, &state.id, &state.active)
                .map_err(|_| InstalledCodePluginError::PackageAdmission(state.id.clone()))?;
            let admitted_code = package
                .manifest()
                .code()
                .ok_or_else(|| InstalledCodePluginError::PackageAdmission(state.id.clone()))?;
            if package.provenance().content() != &rule.package_digest {
                return Err(InstalledCodePluginError::PackageDigestMismatch(
                    state.id.clone(),
                ));
            }
            if code.runtime() != rule.runtime || admitted_code.runtime() != rule.runtime {
                return Err(InstalledCodePluginError::RuntimeMismatch(state.id.clone()));
            }
            if code.entrypoint() != &rule.entrypoint
                || admitted_code.entrypoint() != &rule.entrypoint
            {
                return Err(InstalledCodePluginError::EntrypointMismatch(
                    state.id.clone(),
                ));
            }
            if !rule
                .granted_capabilities
                .iter()
                .all(|grant| package.manifest().permissions().contains(grant))
            {
                return Err(InstalledCodePluginError::CapabilityMismatch(
                    state.id.clone(),
                ));
            }
            let resources = rule.resources.materialize(&state.id)?;
            let authority = InstalledCodePluginAuthority::from_managed(
                &package,
                Self::issue_session(rule.session_policy),
                rule.granted_capabilities.iter().copied(),
                resources,
            )?;
            consumed.insert(state.id.clone());
            authorities.push(authority);
        }
        if let Some(id) = self
            .authority
            .rules
            .keys()
            .find(|id| !consumed.contains(*id))
        {
            return Err(InstalledCodePluginError::LifecycleMismatch(id.clone()));
        }
        Ok(authorities)
    }
}

struct StaticAuthorityProvider(Vec<InstalledCodePluginAuthority>);

impl InstalledCodePluginAuthorityProvider for StaticAuthorityProvider {
    fn authorities(
        &self,
        _cache: &PluginInstallCache,
        _enabled: &[PluginState],
    ) -> Result<Vec<InstalledCodePluginAuthority>, InstalledCodePluginError> {
        Ok(self.0.clone())
    }
}

/// Build the existing `product-extensions` plugin with an explicit installed
/// code authority generation.
///
/// Enabled declarative packages retain PL03/PL04 behavior. Enabled packages
/// carrying `[code]` are excluded from declarative activation and must match
/// exactly one authority row before their process/component and six real
/// registry adapters can publish.
///
/// # Errors
/// Duplicate authority/session rows fail before a plugin is returned.
pub fn installed_product_extensions_plugin_with_code(
    cache: PluginInstallCache,
    authorities: Vec<InstalledCodePluginAuthority>,
) -> Result<Box<dyn Plugin>, InstalledCodePluginError> {
    preflight_authorities(&authorities)?;
    Ok(Box::new(CombinedInstalledExtensionsPlugin::new(
        CacheSource::Open(cache),
        Arc::new(StaticAuthorityProvider(authorities)),
    )))
}

/// Lazily open the PL02 cache during `product-extensions` apply while using an
/// explicit installed-code authority generation.
///
/// # Errors
/// Duplicate authority/session rows fail before a plugin is returned.
pub fn installed_product_extensions_plugin_from_root_with_code(
    root: PathBuf,
    validator: ManifestValidator,
    authorities: Vec<InstalledCodePluginAuthority>,
) -> Result<Box<dyn Plugin>, InstalledCodePluginError> {
    preflight_authorities(&authorities)?;
    Ok(Box::new(CombinedInstalledExtensionsPlugin::new(
        CacheSource::Lazy { root, validator },
        Arc::new(StaticAuthorityProvider(authorities)),
    )))
}

/// Lazily open the PL02 cache and resolve code authority inside plugin apply.
///
/// This is the production constructor when current PL08 admission must be
/// re-evaluated from the exact cache generation. The provider receives the
/// enabled lifecycle snapshot and can construct rows with
/// [`InstalledCodePluginAuthority::from_managed`] without factory-time I/O.
#[must_use]
pub fn installed_product_extensions_plugin_from_root_with_code_provider(
    root: PathBuf,
    validator: ManifestValidator,
    provider: Arc<dyn InstalledCodePluginAuthorityProvider>,
) -> Box<dyn Plugin> {
    Box::new(CombinedInstalledExtensionsPlugin::new(
        CacheSource::Lazy { root, validator },
        provider,
    ))
}

/// [`installed_product_extensions_plugin_from_root_with_code_provider`] that
/// also loads the user's file-authored hooks and subagent presets.
///
/// The composition root decides the roots from workspace trust; see
/// [`crate::user_declarations`].
#[must_use]
pub fn installed_product_extensions_plugin_with_user_declarations(
    root: PathBuf,
    validator: ManifestValidator,
    provider: Arc<dyn InstalledCodePluginAuthorityProvider>,
    user: crate::user_declarations::UserDeclarationRoots,
) -> Box<dyn Plugin> {
    installed_product_extensions_plugin_with_imported_agents(
        root,
        validator,
        provider,
        user,
        Vec::new(),
    )
}

/// Mount native declarations plus one pinned imported-agent generation. Native
/// declarations retain precedence within their own scope during later reloads.
#[must_use]
pub fn installed_product_extensions_plugin_with_imported_agents(
    root: PathBuf,
    validator: ManifestValidator,
    provider: Arc<dyn InstalledCodePluginAuthorityProvider>,
    user: crate::user_declarations::UserDeclarationRoots,
    imported: Vec<heycode_agent::SubagentPreset>,
) -> Box<dyn Plugin> {
    let mut plugin =
        CombinedInstalledExtensionsPlugin::new(CacheSource::Lazy { root, validator }, provider);
    plugin.user = Some(user);
    plugin.imported = imported;
    Box::new(plugin)
}

enum CacheSource {
    Open(PluginInstallCache),
    Lazy {
        root: PathBuf,
        validator: ManifestValidator,
    },
}

struct CombinedInstalledExtensionsPlugin {
    cache: CacheSource,
    authority_provider: Arc<dyn InstalledCodePluginAuthorityProvider>,
    user: Option<crate::user_declarations::UserDeclarationRoots>,
    imported: Vec<heycode_agent::SubagentPreset>,
}

impl CombinedInstalledExtensionsPlugin {
    fn new(
        cache: CacheSource,
        authority_provider: Arc<dyn InstalledCodePluginAuthorityProvider>,
    ) -> Self {
        Self {
            cache,
            authority_provider,
            user: None,
            imported: Vec::new(),
        }
    }

    /// Register the user's file-authored hooks and presets as context effects.
    ///
    /// Each loaded file is a dynamic inventory row so `doctor --composition`
    /// and `/plugins verbose` name it; a skipped file is not an error.
    fn apply_user_declarations(&self, context: &mut Context) -> Result<(), CoreError> {
        let Some(roots) = &self.user else {
            return Ok(());
        };
        let mut loaded =
            crate::user_declarations::load_user_declarations_with_imports(roots, &self.imported);
        if !loaded.hooks.is_empty() {
            let hooks = context
                .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
                .ok_or_else(|| CoreError::other("hooks service missing for user hooks"))?;
            for hook in loaded.hooks.drain(..) {
                context.contribute(heycode_core::ContributionKind::Hook, hook.owner.clone())?;
                let registration = hooks.register_owned(hook);
                context.effect(move || drop(registration));
            }
        }
        let registry = context
            .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
            .ok_or_else(|| CoreError::other("subagent registry missing for user agents"))?;
        crate::agent_management::install(
            context,
            roots.clone(),
            registry,
            loaded,
            self.imported.clone(),
        )?;
        Ok(())
    }

    fn apply_cache(
        &self,
        context: &mut Context,
        cache: &PluginInstallCache,
    ) -> Result<(), CoreError> {
        self.activate(context, cache)
            .map_err(|error| CoreError::other(error.to_string()))
    }

    fn activate(
        &self,
        context: &mut Context,
        cache: &PluginInstallCache,
    ) -> Result<(), InstalledCodePluginError> {
        let lifecycle = context
            .get::<heycode_extensions::lifecycle::PluginLifecycle>(
                heycode_extensions::lifecycle::SERVICE_PLUGIN_LIFECYCLE,
            )
            .ok_or(InstalledCodePluginError::HostUnavailable)?;
        let states = lifecycle
            .list()
            .map_err(|_| InstalledCodePluginError::HostUnavailable)?;
        let enabled = states
            .iter()
            .filter(|state| state.enabled)
            .cloned()
            .collect::<Vec<_>>();
        let authority_rows = self.authority_provider.authorities(cache, &enabled)?;
        preflight_authorities(&authority_rows)?;
        let wasi_engine = if authority_rows.iter().any(|authority| {
            matches!(
                &authority.resources,
                InstalledCodePluginResources::WasiComponentV1 { .. }
            )
        }) {
            Some(Arc::new(
                crate::WasmtimeWasiComponentEngine::new()
                    .map_err(|_| InstalledCodePluginError::WasiRuntimeUnavailable)?,
            ) as Arc<dyn WasiComponentEngine>)
        } else {
            None
        };
        let authorities = authority_rows
            .iter()
            .map(|authority| (authority.id().clone(), authority))
            .collect::<BTreeMap<_, _>>();
        let mut consumed = BTreeSet::new();
        let mut declarative_packages = Vec::new();
        let mut mcp_packages = Vec::new();
        let mut code_packages = Vec::new();

        let subprocess = context
            .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
            .ok_or(InstalledCodePluginError::HostUnavailable)?;
        let cancellation = CodePluginCancellationToken::new();
        let shutdown = cancellation.clone();
        context.effect(move || shutdown.cancel());

        for state in enabled {
            let installed = cache
                .resolve(&state.id, &state.active)
                .map_err(|_| InstalledCodePluginError::PackageAdmission(state.id.clone()))?;
            let package = DeclarativePackage::load(&installed)
                .map_err(|_| InstalledCodePluginError::PackageAdmission(state.id.clone()))?;
            if installed.manifest().code().is_none() {
                declarative_packages.push(package.clone());
                mcp_packages.push(package);
                continue;
            }
            let authority = authorities
                .get(&state.id)
                .copied()
                .ok_or_else(|| InstalledCodePluginError::MissingAuthority(state.id.clone()))?;
            if authority.version() != &state.active {
                return Err(InstalledCodePluginError::LifecycleMismatch(state.id));
            }
            consumed.insert(state.id.clone());
            mcp_packages.push(package);
            code_packages.push(prepare_code_package(
                cache,
                authority,
                &subprocess,
                wasi_engine.as_ref(),
                &cancellation,
            )?);
        }

        for authority in &authority_rows {
            if !consumed.contains(authority.id()) {
                return Err(InstalledCodePluginError::LifecycleMismatch(
                    authority.id().clone(),
                ));
            }
        }

        preflight_mcp_claims(&mcp_packages)
            .map_err(|_| InstalledCodePluginError::HostUnavailable)?;
        let declarative = ProductExtensionsPlugin::new(declarative_packages, true)
            .map_err(|_| InstalledCodePluginError::HostUnavailable)?;
        let code_host = ProductCodePluginHost::production()
            .map_err(|_| InstalledCodePluginError::HostUnavailable)?;
        let code = code_plugin_activation_plugin(code_packages, Arc::new(code_host))
            .map_err(|_| InstalledCodePluginError::HostUnavailable)?;

        let mut rows = declarative.declarative_inventory();
        rows.extend(mcp_inventory(&mcp_packages));
        rows.extend(code.inventory());
        contribute_rows(context, rows).map_err(|_| InstalledCodePluginError::HostUnavailable)?;

        declarative
            .apply_declarative(context)
            .map_err(|_| InstalledCodePluginError::HostUnavailable)?;
        activate_bundled_mcp(context, &mcp_packages)
            .map_err(|_| InstalledCodePluginError::HostUnavailable)?;
        code.apply(context)
            .map_err(|_| InstalledCodePluginError::HostUnavailable)
    }
}

impl Plugin for CombinedInstalledExtensionsPlugin {
    fn name(&self) -> &'static str {
        PRODUCT_EXTENSIONS_PLUGIN_ID
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            PRODUCT_EXTENSIONS_PLUGIN_ID,
            env!("CARGO_PKG_VERSION"),
            PRODUCT_FAMILIES,
        )
    }

    fn provides(&self) -> &'static [ServiceKey] {
        if self.user.is_some() {
            &[crate::SERVICE_AGENT_DECLARATIONS]
        } else {
            &[]
        }
    }

    fn inject(&self) -> &'static [ServiceKey] {
        INSTALLED_PRODUCT_SERVICES
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        match &self.cache {
            CacheSource::Open(cache) => self.apply_cache(context, cache)?,
            CacheSource::Lazy { root, validator } => {
                let cache = PluginInstallCache::open(root, validator.clone()).map_err(|_| {
                    CoreError::other(ProductExtensionError::PackageUnavailable.to_string())
                })?;
                self.apply_cache(context, &cache)?;
            }
        }
        self.apply_user_declarations(context)
    }
}

fn preflight_authorities(
    authorities: &[InstalledCodePluginAuthority],
) -> Result<(), InstalledCodePluginError> {
    let mut packages = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    for authority in authorities {
        if !packages.insert(authority.id().clone()) {
            return Err(InstalledCodePluginError::DuplicateAuthority(
                authority.id().clone(),
            ));
        }
        if !sessions.insert(authority.session_id.clone()) {
            return Err(InstalledCodePluginError::DuplicateSession);
        }
    }
    Ok(())
}

fn prepare_code_package(
    cache: &PluginInstallCache,
    authority: &InstalledCodePluginAuthority,
    subprocess: &heycode_exec::SubprocessService,
    wasi_engine: Option<&Arc<dyn WasiComponentEngine>>,
    cancellation: &CodePluginCancellationToken,
) -> Result<PreparedCodePlugin, InstalledCodePluginError> {
    let installed = cache
        .resolve(authority.id(), authority.version())
        .map_err(|_| InstalledCodePluginError::PackageAdmission(authority.id().clone()))?;
    let runtime = installed
        .manifest()
        .code()
        .map(|code| code.runtime())
        .ok_or_else(|| InstalledCodePluginError::PackageAdmission(authority.id().clone()))?;
    if runtime != authority.resources.runtime() {
        return Err(InstalledCodePluginError::RuntimeMismatch(
            authority.id().clone(),
        ));
    }
    match &authority.resources {
        InstalledCodePluginResources::NativeProcess => {
            let package = CodePluginPackage::load_declared(
                cache,
                authority.id(),
                authority.version(),
                &authority.provenance,
            )
            .map_err(|_| InstalledCodePluginError::PackageAdmission(authority.id().clone()))?;
            let grants = CodePluginCapabilityGrants::new(
                &package,
                authority.granted_capabilities.iter().copied(),
            )
            .map_err(|_| InstalledCodePluginError::PackageAdmission(authority.id().clone()))?;
            package
                .launch(
                    grants,
                    authority.session_id.clone(),
                    Arc::new(HeycodeExecCodePluginLauncher::new(subprocess.clone())),
                    cancellation,
                )
                .map_err(|_| InstalledCodePluginError::RuntimeActivation(authority.id().clone()))
        }
        InstalledCodePluginResources::WasiComponentV1 {
            preopens,
            network_endpoints,
        } => {
            let package = WasiComponentPackage::load_declared(
                cache,
                authority.id(),
                authority.version(),
                &authority.provenance,
            )
            .map_err(|_| InstalledCodePluginError::PackageAdmission(authority.id().clone()))?;
            let grants = CodePluginCapabilityGrants::new(
                package.code_package(),
                authority.granted_capabilities.iter().copied(),
            )
            .map_err(|_| InstalledCodePluginError::PackageAdmission(authority.id().clone()))?;
            let policy = WasiComponentCapabilityPolicy::new(
                &package,
                &grants,
                preopens.clone(),
                network_endpoints.clone(),
            )
            .map_err(|_| InstalledCodePluginError::PackageAdmission(authority.id().clone()))?;
            let engine = wasi_engine.ok_or(InstalledCodePluginError::WasiRuntimeUnavailable)?;
            package
                .launch(
                    grants,
                    policy,
                    authority.session_id.clone(),
                    Arc::clone(engine),
                    cancellation,
                )
                .map_err(|_| InstalledCodePluginError::RuntimeActivation(authority.id().clone()))
        }
    }
}

fn mcp_inventory(packages: &[DeclarativePackage]) -> Vec<PluginContributionSpec> {
    packages
        .iter()
        .flat_map(DeclarativePackage::mcp_contributions)
        .map(|contribution| {
            PluginContributionSpec::new(
                heycode_core::ContributionKind::McpServer,
                crate::product_id(contribution.public_name()),
            )
        })
        .collect()
}

fn contribute_rows(context: &Context, rows: Vec<PluginContributionSpec>) -> Result<(), CoreError> {
    for row in rows {
        context.contribute(row.kind, row.name)?;
    }
    Ok(())
}

fn valid_host_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .to_str()
            .is_some_and(|value| value.len() <= 4096 && !value.chars().any(char::is_control))
}

fn valid_guest_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value.starts_with('/')
        && (value.len() == 1 || !value.ends_with('/'))
        && !value.contains("//")
        && !value.contains('\u{5c}')
        && !value.contains(':')
        && !value.chars().any(char::is_control)
        && (value == "/"
            || value
                .split('/')
                .skip(1)
                .all(|component| !component.is_empty() && !matches!(component, "." | "..")))
}

const fn can_read(access: WasiPreopenAccess) -> bool {
    matches!(
        access,
        WasiPreopenAccess::ReadOnly | WasiPreopenAccess::ReadWrite
    )
}

const fn can_write(access: WasiPreopenAccess) -> bool {
    matches!(
        access,
        WasiPreopenAccess::WriteOnly | WasiPreopenAccess::ReadWrite
    )
}

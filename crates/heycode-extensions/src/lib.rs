//! Stable external plugin package manifest contract.
//!
//! This crate owns untrusted package metadata parsing and validation, immutable
//! local installation, deterministic dependency/conflict/platform resolution,
//! and host-neutral declarative/code activation bridges, the WIT-v1 Component
//! Model boundary, and a curated value-minimized inspection projection. Domain
//! registries still own each contribution's schema and behavior; bridges load
//! bounded immutable input and require a host adapter to activate and
//! effect-dispose complete generations explicitly.

mod activation;
mod cache_error;
mod cache_fs;
mod cache_model;
mod cache_source;
mod cache_store;
mod code_plugin;
mod error;
mod inspector;
pub mod lifecycle;
mod managed_policy;
mod marketplace_catalog;
mod marketplace_error;
mod marketplace_model;
mod marketplace_provenance;
mod model;
mod resolver;
mod validator;
mod version;
mod wasi_component;
mod wire;

pub use activation::{
    DECLARATIVE_ACTIVATION_PLUGIN_ID, DeclarativeContribution, DeclarativeContributionHost,
    DeclarativeContributionRegistration, DeclarativeDocumentFault, DeclarativePackage,
    DeclarativePluginError, HostActivationFailure, declarative_activation_plugin,
};
pub use error::ManifestError;
pub use inspector::{
    CuratedPluginInspector, CuratedPluginReport, CuratedPluginRow, MAX_CURATED_PLUGIN_REPORT_BYTES,
    MAX_CURATED_PLUGIN_ROWS, PluginContributionCount, PluginExecutionKind, PluginGenerationState,
    PluginInspectionPackage, PluginInspectorError,
};
pub use model::{
    Architecture, AuthenticationPolicy, ContributionExposure, ContributionKey, ContributionKind,
    CredentialRequirement, OperatingSystem, PlatformTarget, PluginApiRange, PluginAuthentication,
    PluginCode, PluginCodeRuntime, PluginContribution, PluginDependency, PluginId, PluginManifest,
    PluginPath, PluginPermission, PluginSignature, PluginSource, PluginSourceKind,
    SignatureAlgorithm, UpdateChannel,
};
pub use validator::ManifestValidator;
pub use version::{ApiVersion, PluginVersion};
pub use wasi_component::{
    WASI_CODE_PLUGIN_ABI_VERSION, WASI_CODE_PLUGIN_INTERFACE_V1, WASI_CODE_PLUGIN_WIT_V1,
    WasiComponentAbiReport, WasiComponentCapabilityPolicy, WasiComponentDigest,
    WasiComponentEngine, WasiComponentEngineFault, WasiComponentError, WasiComponentInstance,
    WasiComponentInstantiation, WasiComponentPackage, WasiComponentWorld, WasiNetworkEndpoint,
    WasiPreopen, WasiPreopenAccess, WasiWitDigest,
};

/// Exact external plugin manifest schema understood by this crate.
pub const PLUGIN_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Exact durable inspection/reference layout of the plugin install cache.
pub const PLUGIN_CACHE_SCHEMA_VERSION: u32 = 1;

/// Exact marketplace catalog document schema understood by this crate.
pub const MARKETPLACE_CATALOG_SCHEMA_VERSION: u32 = 1;

/// First stable heycode external plugin host API.
pub const PLUGIN_API_VERSION: u32 = 1;
pub use cache_error::{CacheCorruption, PackageCacheError, PackageLimit, PackageSourceIssue};
pub use cache_model::{
    CacheCleanupReport, CachedPluginSummary, InstallDisposition, InstalledPlugin,
    PackageContentHash, PluginCacheSnapshot,
};
pub use cache_store::PluginInstallCache;
pub use code_plugin::{
    CODE_PLUGIN_ACTIVATION_PLUGIN_ID, CODE_PLUGIN_MAX_FRAME_BYTES, CODE_PLUGIN_PROTOCOL_VERSION,
    CodePluginCapabilityGrants, CodePluginClient, CodePluginContribution,
    CodePluginContributionGeneration, CodePluginContributionHost, CodePluginError,
    CodePluginExecutableDigest, CodePluginExit, CodePluginInvocationError, CodePluginLaunchSpec,
    CodePluginLauncher, CodePluginPackage, CodePluginProcess, CodePluginProtocolFault,
    CodePluginRemoteErrorCode, CodePluginSessionId, CodePluginTransportFault, PreparedCodePlugin,
    code_plugin_activation_plugin,
};
pub use managed_policy::{
    ManagedCapabilityPolicy, ManagedChecksumRequirement, ManagedDeclarativePackage,
    ManagedInstalledPlugin, ManagedLifecycleAdmission, ManagedPluginAdmissionGeneration,
    ManagedPluginAdmissionGenerationId, ManagedPluginError, ManagedPluginPolicy,
    ManagedPluginPolicyRule, ManagedPolicyAxis, ManagedPolicyConfigurationError,
    ManagedPolicyEvaluation, ManagedPolicyRejection, ManagedPolicyVerdict,
    ManagedSignatureRequirement, managed_declarative_activation_plugin,
    resolved_managed_declarative_activation_plugin,
};
pub use marketplace_error::{MarketplaceError, Substitution};
pub use marketplace_model::{
    CatalogDigest, CatalogEntry, MarketplaceCatalog, MarketplaceId, MarketplaceSource,
    MarketplaceSourceKind, SignatureState,
};
pub use marketplace_provenance::{PackageOrigin, PackageProvenance};
pub use resolver::{
    DuplicatePluginDiagnostic, IncompatibleDependencyVersionDiagnostic, PluginGraphResolver,
    PluginResolutionError, ResolvedPluginActivationError, ResolvedPluginGraph,
    ResolvedPluginInstallError, UnsupportedPlatformDiagnostic,
    resolved_declarative_activation_plugin,
};
/// Cancellation token used by code-plugin launch and invocation operations.
pub use tokio_util::sync::CancellationToken as CodePluginCancellationToken;

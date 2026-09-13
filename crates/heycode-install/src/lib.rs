//! Q14–Q16 release admission, install/rollback policy, and evidence matrix.
//!
//! Authenticated manifest/artifact proofs precede recoverable publication;
//! existing installs additionally require stable/preview/pinned and enabled-
//! plugin API approval. Rollback re-verifies retained proofs and is directional
//! because configuration migration is not symmetric. Fresh-machine results
//! retain local/hosted/definition/cross-compile provenance, and workflow YAML
//! never becomes observed native evidence by inference.
//!
//! # Signature ownership
//!
//! The crate owns no production signing key or network client. A host supplies
//! [`ReleaseSignatureVerifier`] for GitHub/Sigstore bundles and receives only
//! closed body-free failures. A non-cloneable [`VerifiedReleaseArtifact`] owns
//! the exact bytes permitted to reach [`InstallRoot`].
//!
//! # Why rollback is directional
//!
//! PL06 rolls a *plugin* back by swapping active and previous, so a rollback
//! can itself be rolled back. Cached plugin versions are inert data and
//! activating either one is a pure choice. A binary is not: the newer version
//! already migrated the configuration, so moving forward is a migration while
//! moving back is a refusal plus a restore. Modelling them as one symmetric
//! swap would claim an equivalence the code does not have. A rollback here
//! therefore clears the rollback target rather than pointing it at the version
//! just escaped — which is also the version an operator rolled back *because*
//! it was broken. The escaped version stays retained on disk, so re-installing
//! it remains possible; it is simply an install, not a rollback.

mod channel;
mod error;
mod gate;
mod gh;
mod install;
mod manager;
mod manifest;
mod model;
mod onboarding;
mod plugin;
mod signature;

pub use channel::{
    ApprovedRelease, PluginApiCompatibility, PluginCompatibilitySet, ReleaseChannel, ReleasePolicy,
    ReleasePolicyRefusal, ReleasePolicyVerdict,
};
pub use error::{ArtifactMismatch, UpdateError};
pub use gate::{RollbackRefusal, RollbackVerdict, rollback_config_verdict};
pub use gh::{GhAttestationVerifier, GhAttestationVerifierError};
pub use install::{InstallDisposition, InstallOutcome, InstallRoot, InstallState, RollbackOutcome};
pub use manager::{ReleaseApplyOutcome, ReleaseBundle, ReleaseManager, ReleaseManagerError};
pub use model::{
    ArtifactAttestation, ArtifactDigest, ReleaseArtifact, ReleaseManifest, ReleasePlatform,
    ReleaseVersion, SignatureScheme,
};
pub use onboarding::{
    EvidenceSource, FreshMachineCheck, FreshMachineMatrix, FreshMachineMatrixError,
    FreshMachineObservation, OnboardingPlatform, PlatformOnboardingEvidence, TurnEvidence,
    parse_fresh_machine_evidence,
};
pub use plugin::{GhReleaseManagerConfig, SERVICE_RELEASE_MANAGER, gh_release_manager_plugin};
pub use signature::{
    AttestedReleaseManifest, ReleaseSignatureVerifier, ReleaseTrustPolicy, SignatureSubject,
    SignatureVerificationFault, SignatureVerificationRequest, VerifiedReleaseArtifact,
    VerifiedSignature,
};

/// Exact release manifest schema understood by this crate.
pub const RELEASE_MANIFEST_SCHEMA_VERSION: u32 = 2;

/// Exact durable layout of an install root.
pub const INSTALL_ROOT_SCHEMA_VERSION: u32 = 2;

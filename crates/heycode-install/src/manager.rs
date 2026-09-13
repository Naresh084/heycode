//! Product release operation owner above the authenticated install boundary.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use heycode_config::ConfigVersionState;
use thiserror::Error;

use crate::manifest::MAX_MANIFEST_BYTES;
use crate::signature::{MAX_RELEASE_ARTIFACT_BYTES, MAX_SIGNATURE_BUNDLE_BYTES};
use crate::{
    InstallOutcome, InstallRoot, InstallState, PluginCompatibilitySet, ReleaseChannel,
    ReleaseManifest, ReleasePlatform, ReleasePolicy, ReleasePolicyRefusal, ReleasePolicyVerdict,
    ReleaseSignatureVerifier, ReleaseTrustPolicy, ReleaseVersion, RollbackOutcome, UpdateError,
};

/// Exact owned files that form one release candidate.
///
/// This type deliberately has no `Debug`: its fields contain remotely supplied
/// manifest/bundle bytes and an executable.
pub struct ReleaseBundle {
    manifest: Vec<u8>,
    manifest_bundle: Vec<u8>,
    artifact: Vec<u8>,
    artifact_bundle: Vec<u8>,
}

impl ReleaseBundle {
    /// Bind the four exact byte sequences verified and installed together.
    #[must_use]
    pub fn new(
        manifest: Vec<u8>,
        manifest_bundle: Vec<u8>,
        artifact: Vec<u8>,
        artifact_bundle: Vec<u8>,
    ) -> Self {
        Self {
            manifest,
            manifest_bundle,
            artifact,
            artifact_bundle,
        }
    }

    /// Read four explicit local files into one immutable operation bundle.
    ///
    /// Each path must identify a regular non-symlink file and is bounded before
    /// allocation. The bytes are then owned, so verification and installation
    /// cannot observe a later path replacement.
    ///
    /// # Errors
    /// Missing/unsafe files, size violations, and read failures are reported
    /// without exposing paths or file contents.
    pub fn read_local(
        manifest: &Path,
        manifest_bundle: &Path,
        artifact: &Path,
        artifact_bundle: &Path,
    ) -> Result<Self, ReleaseManagerError> {
        Ok(Self::new(
            read_bounded_file(manifest, MAX_MANIFEST_BYTES, LocalFileKind::Manifest)?,
            read_bounded_file(
                manifest_bundle,
                MAX_SIGNATURE_BUNDLE_BYTES,
                LocalFileKind::SignatureBundle,
            )?,
            read_bounded_file(
                artifact,
                MAX_RELEASE_ARTIFACT_BYTES,
                LocalFileKind::Artifact,
            )?,
            read_bounded_file(
                artifact_bundle,
                MAX_SIGNATURE_BUNDLE_BYTES,
                LocalFileKind::SignatureBundle,
            )?,
        ))
    }
}

/// Result of applying one candidate bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReleaseApplyOutcome {
    /// A fresh install or update committed.
    Installed(InstallOutcome),
    /// The authenticated candidate is already current and no artifact changed.
    Current(ReleaseVersion),
}

/// Product release-manager failure without fetched bytes, paths, or verifier output.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReleaseManagerError {
    /// The owning plugin Context has shut down.
    #[error("release manager is closed")]
    Closed,
    /// Stable/preview/pinned or enabled-plugin API policy refused the candidate.
    #[error("release policy refused the candidate")]
    Policy(ReleasePolicyRefusal),
    /// One explicit local bundle component was missing, unsafe, or unreadable.
    #[error("local release bundle component `{component}` is unavailable")]
    LocalBundleUnavailable {
        /// Compile-time component label; never a path.
        component: &'static str,
    },
    /// Authenticated release parsing, verification, mutation, or rollback failed.
    #[error(transparent)]
    Update(#[from] UpdateError),
}

/// Effect-ownable release operation service.
pub struct ReleaseManager {
    install_root: std::path::PathBuf,
    platform: ReleasePlatform,
    trust: ReleaseTrustPolicy,
    verifier: Arc<dyn ReleaseSignatureVerifier>,
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for ReleaseManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReleaseManager")
            .field("platform", &self.platform)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl ReleaseManager {
    /// Open a durable installation and bind its exact trust/platform policy.
    ///
    /// # Errors
    /// Invalid or unsafe installation state is refused.
    pub fn new(
        install_root: impl AsRef<Path>,
        platform: ReleasePlatform,
        trust: ReleaseTrustPolicy,
        verifier: Arc<dyn ReleaseSignatureVerifier>,
    ) -> Result<Self, ReleaseManagerError> {
        let install_root = install_root.as_ref();
        if !install_root.is_absolute() {
            return Err(UpdateError::InvalidRoot.into());
        }
        if let Ok(metadata) = std::fs::symlink_metadata(install_root)
            && (metadata.file_type().is_symlink() || !metadata.is_dir())
        {
            return Err(UpdateError::InvalidRoot.into());
        }
        Ok(Self {
            install_root: install_root.to_path_buf(),
            platform,
            trust,
            verifier,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Read current/rollback state from the durable installation.
    ///
    /// # Errors
    /// A closed manager or malformed installation fails safely.
    pub fn state(&self) -> Result<InstallState, ReleaseManagerError> {
        self.ensure_open()?;
        match self.open_existing()? {
            Some(install) => install.state().map_err(Into::into),
            None => Ok(InstallState {
                current: None,
                previous: None,
            }),
        }
    }

    /// Authenticate, admit, and atomically apply one release bundle.
    ///
    /// Channel and enabled-plugin compatibility are snapshotted by the caller
    /// at this operation boundary. Policy runs after manifest authentication
    /// and before artifact signature verification or filesystem mutation.
    ///
    /// # Errors
    /// Closed service, signature/checksum failure, policy refusal, stale update
    /// approval, or durable install failure.
    pub fn apply(
        &self,
        bundle: ReleaseBundle,
        channel: ReleaseChannel,
        plugins: PluginCompatibilitySet,
    ) -> Result<ReleaseApplyOutcome, ReleaseManagerError> {
        self.ensure_open()?;
        let manifest = ReleaseManifest::parse_attested(
            &bundle.manifest,
            &bundle.manifest_bundle,
            &self.trust,
            self.verifier.as_ref(),
        )?;
        let state = self.state()?;
        let policy = ReleasePolicy::new(channel, plugins);
        let artifact = match state.current {
            Some(current) => match policy.evaluate(&current, &manifest) {
                ReleasePolicyVerdict::Approved(approved) => approved.verify_artifact(
                    &self.platform,
                    bundle.artifact,
                    &bundle.artifact_bundle,
                    self.verifier.as_ref(),
                )?,
                ReleasePolicyVerdict::Current => {
                    return Ok(ReleaseApplyOutcome::Current(current));
                }
                ReleasePolicyVerdict::Refused(refusal) => {
                    return Err(ReleaseManagerError::Policy(refusal));
                }
            },
            None => {
                policy
                    .evaluate_fresh(&manifest)
                    .map_err(ReleaseManagerError::Policy)?;
                manifest.verify_artifact(
                    &self.platform,
                    bundle.artifact,
                    &bundle.artifact_bundle,
                    self.verifier.as_ref(),
                )?
            }
        };
        self.ensure_open()?;
        InstallRoot::open(&self.install_root)?
            .install(artifact)
            .map(ReleaseApplyOutcome::Installed)
            .map_err(Into::into)
    }

    /// Re-verify and directionally restore the retained prior version.
    ///
    /// # Errors
    /// Closed service, corrupt/missing retained proof, signature failure, or
    /// durable rollback failure.
    pub fn rollback(
        &self,
        config: ConfigVersionState,
        plugins: PluginCompatibilitySet,
    ) -> Result<RollbackOutcome, ReleaseManagerError> {
        self.ensure_open()?;
        let install = self.open_existing()?.ok_or(UpdateError::InvalidRoot)?;
        install
            .rollback(config, &plugins, &self.trust, self.verifier.as_ref())
            .map_err(Into::into)
    }

    /// Make every held handle terminal. Idempotent.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(crate) fn close_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.closed)
    }

    fn ensure_open(&self) -> Result<(), ReleaseManagerError> {
        if self.closed.load(Ordering::Acquire) {
            Err(ReleaseManagerError::Closed)
        } else {
            Ok(())
        }
    }

    fn open_existing(&self) -> Result<Option<InstallRoot>, ReleaseManagerError> {
        match std::fs::symlink_metadata(&self.install_root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(UpdateError::InvalidRoot.into()),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(UpdateError::InvalidRoot.into())
            }
            Ok(_) => InstallRoot::open(&self.install_root)
                .map(Some)
                .map_err(Into::into),
        }
    }
}

#[derive(Clone, Copy)]
enum LocalFileKind {
    Manifest,
    SignatureBundle,
    Artifact,
}

fn read_bounded_file(
    path: &Path,
    maximum: usize,
    kind: LocalFileKind,
) -> Result<Vec<u8>, ReleaseManagerError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| local_file_unavailable(kind))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(local_file_unavailable(kind));
    }
    let length =
        usize::try_from(metadata.len()).map_err(|_| local_file_size_error(kind, maximum))?;
    if length > maximum {
        return Err(local_file_size_error(kind, maximum));
    }
    let bytes = std::fs::read(path).map_err(|_| local_file_unavailable(kind))?;
    if bytes.len() != length || bytes.len() > maximum {
        return Err(local_file_unavailable(kind));
    }
    Ok(bytes)
}

fn local_file_size_error(kind: LocalFileKind, maximum: usize) -> ReleaseManagerError {
    ReleaseManagerError::Update(match kind {
        LocalFileKind::Manifest => UpdateError::ManifestTooLarge { limit: maximum },
        LocalFileKind::SignatureBundle => UpdateError::SignatureBundleSize { limit: maximum },
        LocalFileKind::Artifact => UpdateError::ArtifactTooLarge { limit: maximum },
    })
}

const fn local_file_unavailable(kind: LocalFileKind) -> ReleaseManagerError {
    ReleaseManagerError::LocalBundleUnavailable {
        component: match kind {
            LocalFileKind::Manifest => "manifest",
            LocalFileKind::SignatureBundle => "signature_bundle",
            LocalFileKind::Artifact => "artifact",
        },
    }
}

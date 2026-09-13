//! The install root and the two transitions the row names.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use heycode_config::ConfigVersionState;
use serde::{Deserialize, Serialize};

use crate::error::{io, mismatch};
use crate::signature::VerifiedArtifactParts;
use crate::{
    ArtifactDigest, ArtifactMismatch, INSTALL_ROOT_SCHEMA_VERSION, PluginCompatibilitySet,
    ReleaseManifest, ReleasePlatform, ReleaseSignatureVerifier, ReleaseTrustPolicy, ReleaseVersion,
    RollbackRefusal, RollbackVerdict, UpdateError, VerifiedReleaseArtifact,
    rollback_config_verdict,
};

const RECORD_FILE: &str = ".heycode-install";
const VERSIONS_DIRECTORY: &str = "versions";
const BIN_DIRECTORY: &str = "bin";
#[cfg(not(windows))]
const BINARY_NAME: &str = "heycode";
#[cfg(windows)]
const BINARY_NAME: &str = "heycode.exe";
const RELEASE_FILE: &str = "release.json";
const RELEASE_MANIFEST_FILE: &str = "release-manifest.json";
const RELEASE_MANIFEST_BUNDLE_FILE: &str = "release-manifest.sigstore.json";
const ARTIFACT_BUNDLE_FILE: &str = "artifact.sigstore.json";
const TRANSITION_FILE: &str = ".transition";
const MAX_RECORD_BYTES: u64 = 4 * 1024;
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Whether an install created the first version or replaced one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallDisposition {
    /// The root held no current version; this is the acceptance's fresh
    /// install.
    FreshInstall,
    /// A current version was replaced and retained as the rollback target.
    Update,
}

/// What an install committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallOutcome {
    /// The version now current.
    pub version: ReleaseVersion,
    /// The version this install displaced, retained as the rollback target.
    pub previous: Option<ReleaseVersion>,
    /// Whether this was a fresh install or an update.
    pub disposition: InstallDisposition,
}

/// What a rollback did, or declined to do.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RollbackOutcome {
    /// The current version is now `to`.
    RolledBack {
        /// The version rolled back to.
        to: ReleaseVersion,
    },
    /// Nothing was changed.
    Refused(RollbackRefusal),
}

/// What the root believes about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallState {
    /// The version `bin/heycode` was placed from, when one is installed.
    pub current: Option<ReleaseVersion>,
    /// The version a rollback would return to, when there is one.
    ///
    /// Exactly one step, and cleared by a rollback rather than swapped — see
    /// the crate documentation for why a binary rollback is directional where
    /// PL06's plugin rollback is not.
    pub previous: Option<ReleaseVersion>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    schema_version: u32,
    current: Option<String>,
    previous: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRetained {
    schema_version: u32,
    version: String,
    digest: String,
    config_schema_version: u32,
    plugin_api_version: u32,
    platform: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTransition {
    schema_version: u32,
    before: WireRecord,
    after: WireRecord,
    target_digest: String,
}

/// One heycode installation on disk.
pub struct InstallRoot {
    root: PathBuf,
}

impl InstallRoot {
    /// Open or create one absolute install root.
    ///
    /// # Errors
    /// A relative root, a root whose expected entries are not directories, a
    /// malformed record, or a filesystem failure.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, UpdateError> {
        let root = root.as_ref();
        if !root.is_absolute() {
            return Err(UpdateError::InvalidRoot);
        }
        ensure_directory(root, "create_install_root")?;
        let root = std::fs::canonicalize(root).map_err(|_| io("canonicalize_install_root"))?;
        ensure_directory(&root.join(VERSIONS_DIRECTORY), "create_versions_directory")?;
        ensure_directory(&root.join(BIN_DIRECTORY), "create_bin_directory")?;
        let opened = Self { root };
        if !opened.record_path().exists() {
            opened.write_state(&InstallState {
                current: None,
                previous: None,
            })?;
        }
        opened.recover_transition()?;
        let _validated = opened.state()?;
        Ok(opened)
    }

    /// Canonical install root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The stable path a PATH entry points at.
    #[must_use]
    pub fn current_binary(&self) -> PathBuf {
        self.root.join(BIN_DIRECTORY).join(BINARY_NAME)
    }

    /// Current and prior version, read from the durable record.
    ///
    /// # Errors
    /// A malformed record, or a filesystem failure.
    pub fn state(&self) -> Result<InstallState, UpdateError> {
        let bytes = read_bounded(&self.record_path(), MAX_RECORD_BYTES)?
            .ok_or(UpdateError::CorruptRecord)?;
        let wire =
            serde_json::from_slice::<WireRecord>(&bytes).map_err(|_| UpdateError::CorruptRecord)?;
        if wire.schema_version != INSTALL_ROOT_SCHEMA_VERSION {
            return Err(UpdateError::CorruptRecord);
        }
        let parse = |value: Option<String>| {
            value
                .map(ReleaseVersion::parse)
                .transpose()
                .map_err(|_| UpdateError::CorruptRecord)
        };
        Ok(InstallState {
            current: parse(wire.current)?,
            previous: parse(wire.previous)?,
        })
    }

    /// Versions this root still holds, derived from the filesystem.
    ///
    /// Deliberately derived rather than recorded. A retention list in the
    /// record could disagree with the disk, and the disagreement would surface
    /// as a rollback into an empty directory. Reading the disk means a pruned
    /// version is detected by its absence.
    ///
    /// # Errors
    /// A filesystem failure. Entries that are not admissible retained versions
    /// are skipped rather than failing: an unrelated directory is not this
    /// crate's to reject, but it is also not a version it will roll back to.
    pub fn retained_versions(&self) -> Result<Vec<ReleaseVersion>, UpdateError> {
        let mut retained = Vec::new();
        let entries = std::fs::read_dir(self.root.join(VERSIONS_DIRECTORY))
            .map_err(|_| io("read_versions_directory"))?;
        for entry in entries {
            let entry = entry.map_err(|_| io("read_versions_directory"))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(version) = ReleaseVersion::parse(name) else {
                continue;
            };
            if self.read_retained(&version)?.is_some() {
                retained.push(version);
            }
        }
        retained.sort();
        Ok(retained)
    }

    /// Install one signature- and checksum-verified artifact and make it current.
    ///
    /// [`VerifiedReleaseArtifact`] owns the exact bytes authenticated through
    /// an attested manifest. Publication uses an atomic binary replacement and
    /// a recovery journal, so reopening deterministically completes or clears
    /// an interrupted transition.
    ///
    /// # Errors
    /// [`UpdateError::AlreadyCurrent`] or a filesystem failure. Signature and
    /// checksum failures occur before this method can receive its argument.
    pub fn install(
        &self,
        artifact: VerifiedReleaseArtifact,
    ) -> Result<InstallOutcome, UpdateError> {
        self.recover_transition()?;
        let state = self.state()?;
        if state.current.as_ref() == Some(artifact.version()) {
            return Err(UpdateError::AlreadyCurrent {
                version: artifact.version().clone(),
            });
        }
        if let Some(current) = &state.current
            && artifact
                .update_approval
                .as_ref()
                .is_none_or(|approval| &approval.from != current)
        {
            return Err(UpdateError::UpdatePolicyRequired {
                current: current.clone(),
                candidate: artifact.version().clone(),
            });
        }
        let parts = artifact.into_parts();
        match self.read_retained(&parts.version)? {
            Some(retained) if retained.digest == parts.digest => {}
            Some(retained) => {
                return Err(mismatch(ArtifactMismatch::Retained {
                    version: parts.version,
                    recorded: retained.digest,
                    found: parts.digest,
                }));
            }
            None => self.write_retained(&parts)?,
        }
        let previous = state.current.clone();
        let next = InstallState {
            current: Some(parts.version.clone()),
            previous: previous.clone(),
        };
        self.commit_transition(&state, &next, &parts.digest, &parts.bytes)?;
        Ok(InstallOutcome {
            disposition: if previous.is_none() {
                InstallDisposition::FreshInstall
            } else {
                InstallDisposition::Update
            },
            version: parts.version,
            previous,
        })
    }

    /// Roll back to the retained prior version.
    ///
    /// `configuration` is the classification of the configuration document as
    /// it stands on disk; the B03 gate compares it against what the *target*
    /// understands. Every refusal is decided before the binary is swapped.
    ///
    /// # Errors
    /// [`UpdateError::Mismatch`] when the retained copy no longer matches what
    /// was recorded for it, or a filesystem failure. An absent prior version,
    /// a pruned target and a too-new configuration are refusals rather than
    /// errors.
    pub fn rollback(
        &self,
        configuration: ConfigVersionState,
        plugins: &PluginCompatibilitySet,
        trust: &ReleaseTrustPolicy,
        verifier: &dyn ReleaseSignatureVerifier,
    ) -> Result<RollbackOutcome, UpdateError> {
        self.recover_transition()?;
        let state = self.state()?;
        let Some(target) = state.previous.clone() else {
            return Ok(RollbackOutcome::Refused(
                RollbackRefusal::NothingToRollBackTo,
            ));
        };
        let Some(retained) = self.read_retained(&target)? else {
            return Ok(RollbackOutcome::Refused(
                RollbackRefusal::TargetNotRetained { version: target },
            ));
        };
        let Some(bytes) = read_bounded(&self.retained_binary(&target), MAX_BINARY_BYTES)? else {
            return Ok(RollbackOutcome::Refused(
                RollbackRefusal::TargetNotRetained { version: target },
            ));
        };
        let found = ArtifactDigest::of_bytes(&bytes);
        if found != retained.digest {
            return Err(mismatch(ArtifactMismatch::Retained {
                version: target,
                recorded: retained.digest,
                found,
            }));
        }
        let directory = self.retained_directory(&target);
        let Some(manifest_raw) = read_bounded(&directory.join(RELEASE_MANIFEST_FILE), 256 * 1024)?
        else {
            return Ok(RollbackOutcome::Refused(
                RollbackRefusal::TargetNotRetained { version: target },
            ));
        };
        let Some(manifest_bundle) = read_bounded(
            &directory.join(RELEASE_MANIFEST_BUNDLE_FILE),
            4 * 1024 * 1024,
        )?
        else {
            return Ok(RollbackOutcome::Refused(
                RollbackRefusal::TargetNotRetained { version: target },
            ));
        };
        let Some(artifact_bundle) =
            read_bounded(&directory.join(ARTIFACT_BUNDLE_FILE), 4 * 1024 * 1024)?
        else {
            return Ok(RollbackOutcome::Refused(
                RollbackRefusal::TargetNotRetained { version: target },
            ));
        };
        let manifest =
            ReleaseManifest::parse_attested(&manifest_raw, &manifest_bundle, trust, verifier)?;
        if manifest.manifest().version() != &target
            || manifest.manifest().config_schema_version() != retained.config_schema_version
            || manifest.manifest().plugin_api_version() != retained.plugin_api_version
        {
            return Err(UpdateError::CorruptRecord);
        }
        let verified =
            manifest.verify_artifact(&retained.platform, bytes, &artifact_bundle, verifier)?;
        if let Some(plugin) = plugins.first_incompatible(retained.plugin_api_version) {
            return Ok(RollbackOutcome::Refused(
                RollbackRefusal::PluginApiIncompatible {
                    plugin: plugin.plugin().to_owned(),
                    target_api: retained.plugin_api_version,
                    minimum: plugin.minimum(),
                    maximum: plugin.maximum(),
                },
            ));
        }
        if let RollbackVerdict::Refused(refusal) =
            rollback_config_verdict(configuration, retained.config_schema_version)
        {
            return Ok(RollbackOutcome::Refused(refusal));
        }

        // Directional: the escaped version stays retained on disk but is not
        // armed as the next rollback target. See the crate documentation.
        let next = InstallState {
            current: Some(target.clone()),
            previous: None,
        };
        self.commit_transition(&state, &next, verified.digest(), verified.bytes())?;
        Ok(RollbackOutcome::RolledBack { to: target })
    }

    fn record_path(&self) -> PathBuf {
        self.root.join(RECORD_FILE)
    }

    fn transition_path(&self) -> PathBuf {
        self.root.join(BIN_DIRECTORY).join(TRANSITION_FILE)
    }

    fn retained_directory(&self, version: &ReleaseVersion) -> PathBuf {
        self.root.join(VERSIONS_DIRECTORY).join(version.as_str())
    }

    fn retained_binary(&self, version: &ReleaseVersion) -> PathBuf {
        self.retained_directory(version).join(BINARY_NAME)
    }

    fn read_retained(&self, version: &ReleaseVersion) -> Result<Option<Retained>, UpdateError> {
        let path = self.retained_directory(version).join(RELEASE_FILE);
        let Some(bytes) = read_bounded(&path, MAX_RECORD_BYTES)? else {
            return Ok(None);
        };
        let wire = serde_json::from_slice::<WireRetained>(&bytes)
            .map_err(|_| UpdateError::CorruptRecord)?;
        if wire.schema_version != INSTALL_ROOT_SCHEMA_VERSION || wire.version != version.as_str() {
            return Err(UpdateError::CorruptRecord);
        }
        for (file, maximum) in [
            (BINARY_NAME, MAX_BINARY_BYTES),
            (ARTIFACT_BUNDLE_FILE, 4 * 1024 * 1024),
            (RELEASE_MANIFEST_FILE, 256 * 1024),
            (RELEASE_MANIFEST_BUNDLE_FILE, 4 * 1024 * 1024),
        ] {
            if !regular_bounded_exists(&self.retained_directory(version).join(file), maximum)? {
                return Ok(None);
            }
        }
        Ok(Some(Retained {
            digest: ArtifactDigest::parse(&wire.digest).map_err(|_| UpdateError::CorruptRecord)?,
            config_schema_version: wire.config_schema_version,
            plugin_api_version: wire.plugin_api_version,
            platform: ReleasePlatform::new(wire.platform)
                .map_err(|_| UpdateError::CorruptRecord)?,
        }))
    }

    fn write_retained(&self, artifact: &VerifiedArtifactParts) -> Result<(), UpdateError> {
        let directory = self.retained_directory(&artifact.version);
        ensure_directory(&directory, "create_retained_directory")?;
        write_atomic(&directory.join(BINARY_NAME), &artifact.bytes, true)?;
        write_atomic(
            &directory.join(ARTIFACT_BUNDLE_FILE),
            &artifact.artifact_bundle,
            false,
        )?;
        write_atomic(
            &directory.join(RELEASE_MANIFEST_FILE),
            &artifact.manifest_raw,
            false,
        )?;
        write_atomic(
            &directory.join(RELEASE_MANIFEST_BUNDLE_FILE),
            &artifact.manifest_bundle,
            false,
        )?;
        let record = serde_json::to_vec(&WireRetained {
            schema_version: INSTALL_ROOT_SCHEMA_VERSION,
            version: artifact.version.as_str().to_owned(),
            digest: artifact.digest.as_str().to_owned(),
            config_schema_version: artifact.config_schema_version,
            plugin_api_version: artifact.plugin_api_version,
            platform: artifact.platform.as_str().to_owned(),
        })
        .map_err(|_| io("render_retained_record"))?;
        write_atomic(&directory.join(RELEASE_FILE), &record, false)
    }

    fn place_current(&self, bytes: &[u8]) -> Result<(), UpdateError> {
        write_atomic(&self.current_binary(), bytes, true)
    }

    fn commit_transition(
        &self,
        before: &InstallState,
        after: &InstallState,
        target_digest: &ArtifactDigest,
        bytes: &[u8],
    ) -> Result<(), UpdateError> {
        let journal = serde_json::to_vec(&WireTransition {
            schema_version: INSTALL_ROOT_SCHEMA_VERSION,
            before: wire_record(before),
            after: wire_record(after),
            target_digest: target_digest.as_str().to_owned(),
        })
        .map_err(|_| io("render_install_transition"))?;
        write_atomic(&self.transition_path(), &journal, false)?;
        self.place_current(bytes)?;
        self.write_state(after)?;
        remove_transition(&self.transition_path());
        Ok(())
    }

    fn recover_transition(&self) -> Result<(), UpdateError> {
        let Some(bytes) = read_bounded(&self.transition_path(), MAX_RECORD_BYTES)? else {
            return Ok(());
        };
        let wire = serde_json::from_slice::<WireTransition>(&bytes)
            .map_err(|_| UpdateError::CorruptRecord)?;
        if wire.schema_version != INSTALL_ROOT_SCHEMA_VERSION {
            return Err(UpdateError::CorruptRecord);
        }
        let before = state_from_wire(wire.before)?;
        let after = state_from_wire(wire.after)?;
        let target_digest =
            ArtifactDigest::parse(&wire.target_digest).map_err(|_| UpdateError::CorruptRecord)?;
        let current = read_bounded(&self.current_binary(), MAX_BINARY_BYTES)?;
        if current.as_deref().map(ArtifactDigest::of_bytes).as_ref() == Some(&target_digest) {
            self.write_state(&after)?;
            remove_transition(&self.transition_path());
            return Ok(());
        }
        let current_matches_before = match (&before.current, current.as_deref()) {
            (None, None) => true,
            (Some(version), Some(current)) => self
                .read_retained(version)?
                .is_some_and(|retained| ArtifactDigest::of_bytes(current) == retained.digest),
            _ => false,
        };
        if current_matches_before && self.state()? == before {
            remove_transition(&self.transition_path());
            return Ok(());
        }
        Err(UpdateError::CorruptRecord)
    }

    fn write_state(&self, state: &InstallState) -> Result<(), UpdateError> {
        let bytes =
            serde_json::to_vec(&wire_record(state)).map_err(|_| io("render_install_record"))?;
        write_atomic(&self.record_path(), &bytes, false)
    }
}

struct Retained {
    digest: ArtifactDigest,
    config_schema_version: u32,
    plugin_api_version: u32,
    platform: ReleasePlatform,
}

fn wire_record(state: &InstallState) -> WireRecord {
    WireRecord {
        schema_version: INSTALL_ROOT_SCHEMA_VERSION,
        current: state
            .current
            .as_ref()
            .map(|version| version.as_str().to_owned()),
        previous: state
            .previous
            .as_ref()
            .map(|version| version.as_str().to_owned()),
    }
}

fn state_from_wire(wire: WireRecord) -> Result<InstallState, UpdateError> {
    if wire.schema_version != INSTALL_ROOT_SCHEMA_VERSION {
        return Err(UpdateError::CorruptRecord);
    }
    let parse = |value: Option<String>| {
        value
            .map(ReleaseVersion::parse)
            .transpose()
            .map_err(|_| UpdateError::CorruptRecord)
    };
    Ok(InstallState {
        current: parse(wire.current)?,
        previous: parse(wire.previous)?,
    })
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, UpdateError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(io("inspect_install_file")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(io("inspect_install_file"));
    }
    std::fs::read(path)
        .map(Some)
        .map_err(|_| io("read_install_file"))
}

fn regular_bounded_exists(path: &Path, maximum: u64) -> Result<bool, UpdateError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum
            {
                return Err(io("inspect_install_file"));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(io("inspect_install_file")),
    }
}

fn ensure_directory(path: &Path, operation: &'static str) -> Result<(), UpdateError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(UpdateError::InvalidRoot),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path).map_err(|_| io(operation))?;
            let metadata = std::fs::symlink_metadata(path).map_err(|_| io(operation))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(UpdateError::InvalidRoot)
            }
        }
        Err(_) => Err(io(operation)),
    }
}

fn remove_transition(path: &Path) {
    if std::fs::remove_file(path).is_ok() {
        sync_parent(path);
    }
}

/// Write through a sibling temporary and rename, so a reader never observes a
/// half-written binary and a failed write leaves the previous one in place.
fn write_atomic(path: &Path, bytes: &[u8], executable: bool) -> Result<(), UpdateError> {
    use std::io::Write as _;

    let parent = path.parent().ok_or(UpdateError::InvalidRoot)?;
    let file_name = path.file_name().ok_or(UpdateError::InvalidRoot)?;
    let mut temporary = parent.join(file_name);
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    temporary
        .as_mut_os_string()
        .push(format!(".{}.{}.tmp", std::process::id(), sequence));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| io("create_install_temporary"))?;
    if file.write_all(bytes).is_err() {
        drop(file);
        remove_temporary(&temporary);
        return Err(io("write_install_temporary"));
    }
    if executable && let Err(error) = set_executable(&temporary) {
        drop(file);
        remove_temporary(&temporary);
        return Err(error);
    }
    if file.sync_all().is_err() {
        drop(file);
        remove_temporary(&temporary);
        return Err(io("sync_install_temporary"));
    }
    drop(file);
    match std::fs::rename(&temporary, path) {
        Ok(()) => {
            sync_parent(path);
            Ok(())
        }
        Err(_) => {
            remove_temporary(&temporary);
            Err(io("commit_install_file"))
        }
    }
}

fn remove_temporary(path: &Path) {
    drop(std::fs::remove_file(path));
}

#[cfg(unix)]
fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = std::fs::File::open(parent)
    {
        drop(directory.sync_all());
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|_| io("set_install_mode"))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), UpdateError> {
    Ok(())
}

//! Atomic object/ref publication and deterministic cache inspection.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use sha2::{Digest as _, Sha256};

use crate::cache_error::{corrupt, io};
use crate::cache_fs::{
    ensure_directory, is_stale, open_existing_lock_file, open_lock_file, open_root,
    read_cache_file, sync_directory, try_create_directory, validate_directory, write_new_file,
};
use crate::cache_source::{PackageEntryData, PreparedPackage, read_package_directory};
use crate::model::{valid_kebab, valid_relative_path};
use crate::{
    CacheCleanupReport, CacheCorruption, CachedPluginSummary, InstallDisposition, InstalledPlugin,
    ManifestValidator, PLUGIN_CACHE_SCHEMA_VERSION, PackageCacheError, PackageContentHash,
    PluginCacheSnapshot, PluginId, PluginManifest, PluginVersion,
};

const OBJECTS_DIRECTORY: &str = ".objects";
const REFS_DIRECTORY: &str = ".refs";
const STAGING_DIRECTORY: &str = ".staging";
const LOCKS_DIRECTORY: &str = ".locks";
const SCHEMA_FILE: &str = ".cache-schema";
const INIT_LOCK_FILE: &str = ".cache-init.lock";
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(5);
const MAX_REFERENCE_BYTES: u64 = 384;
const REFERENCE_READ_WAIT: Duration = Duration::from_secs(1);
static WORK_ID: AtomicU64 = AtomicU64::new(1);
static PROCESS_LOCKS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

/// Owner-only versioned content-addressed plugin install cache.
pub struct PluginInstallCache {
    root: PathBuf,
    root_capability: Arc<Dir>,
    validator: ManifestValidator,
}

impl PluginInstallCache {
    /// Open or create one absolute owner-only cache root.
    ///
    /// # Errors
    /// Relative/unsafe roots, non-owner-only state, unsupported host security,
    /// or filesystem failures fail before a cache handle is returned.
    pub fn open(
        root: impl AsRef<Path>,
        validator: ManifestValidator,
    ) -> Result<Self, PackageCacheError> {
        let root = open_root(root.as_ref())?;
        initialize_schema(&root)?;
        for relative in [
            OBJECTS_DIRECTORY,
            ".objects/sha256",
            REFS_DIRECTORY,
            STAGING_DIRECTORY,
            LOCKS_DIRECTORY,
            ".locks/version",
            ".locks/object",
        ] {
            ensure_directory(&root.join(relative))?;
        }
        let root_capability = Arc::new(
            Dir::open_ambient_dir(&root, ambient_authority())
                .map_err(|_| io("open_cache_capability"))?,
        );
        Ok(Self {
            root,
            root_capability,
            validator,
        })
    }

    /// Canonical cache root. This path is local-only and is never included in
    /// [`PluginCacheSnapshot`].
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validate and atomically install one complete local package directory.
    ///
    /// # Errors
    /// Unsafe source/cache filesystem state, manifest/package mismatch,
    /// resource limits, id/version substitution, lock timeout, or durable
    /// commit failure. A failure never changes an existing version reference.
    pub fn install_directory(
        &self,
        source: impl AsRef<Path>,
    ) -> Result<InstalledPlugin, PackageCacheError> {
        let prepared = self.prepare_directory(source.as_ref())?;
        self.commit_prepared(prepared)
    }

    /// Read and validate a complete source tree without mutating the cache.
    ///
    /// PL08 uses this read-only phase to join the exact manifest/tree digest to
    /// administrator policy before an install lock, object, or reference can
    /// be created. The returned package owns the bytes that later commit, so a
    /// source swap cannot change what was authorized between the two phases.
    pub(crate) fn prepare_directory(
        &self,
        source: &Path,
    ) -> Result<PreparedPackage, PackageCacheError> {
        self.validate_layout()?;
        if !source.is_absolute() {
            return Err(PackageCacheError::UnsafeSource {
                issue: crate::PackageSourceIssue::NonPortablePath,
            });
        }
        let source_metadata =
            std::fs::symlink_metadata(source).map_err(|_| io("inspect_source_root"))?;
        if source_metadata.file_type().is_symlink() {
            return Err(PackageCacheError::UnsafeSource {
                issue: crate::PackageSourceIssue::SymbolicLink,
            });
        }
        let canonical_source =
            std::fs::canonicalize(source).map_err(|_| io("canonicalize_source_root"))?;
        if canonical_source.starts_with(&self.root) || self.root.starts_with(&canonical_source) {
            return Err(PackageCacheError::UnsafeSource {
                issue: crate::PackageSourceIssue::CacheOverlap,
            });
        }
        read_package_directory(&canonical_source, &self.validator)
    }

    /// Commit bytes already frozen by [`Self::prepare_directory`].
    ///
    /// No source path is reopened here. This preserves PL02's immutable
    /// publication law while giving PL08 one exact pre-mutation policy point.
    pub(crate) fn commit_prepared(
        &self,
        prepared: PreparedPackage,
    ) -> Result<InstalledPlugin, PackageCacheError> {
        let version_lock_path = self.version_lock_path(&prepared.manifest)?;
        let _version_lock = self.acquire_lock(version_lock_path)?;

        if let Some(existing) = self.read_reference(&prepared.manifest)? {
            if existing != prepared.content_hash {
                return Err(PackageCacheError::VersionConflict {
                    id: prepared.manifest.id().clone(),
                    version: prepared.manifest.version().clone(),
                });
            }
            let verified = self.load_object(
                prepared.manifest.id(),
                prepared.manifest.version(),
                &existing,
            )?;
            return Ok(self.receipt(verified, InstallDisposition::AlreadyPresent));
        }

        let object_lock_path = self.object_lock_path(&prepared.content_hash);
        let _object_lock = self.acquire_lock(object_lock_path)?;
        let object_created = self.ensure_object(&prepared)?;
        let reference_result = self.write_reference(&prepared.manifest, &prepared.content_hash);
        if let Err(error) = reference_result {
            if object_created && !self.hash_is_referenced(&prepared.content_hash)? {
                self.remove_tree(&self.object_path(&prepared.content_hash))?;
                sync_directory(&self.root.join(".objects/sha256"))?;
            }
            return Err(error);
        }
        Ok(self.receipt(prepared, InstallDisposition::Installed))
    }

    /// Resolve and fully verify one installed id/version.
    ///
    /// # Errors
    /// Missing/malformed refs, unsafe object state, manifest mismatch, or hash
    /// corruption fail instead of returning an activation path.
    pub fn resolve(
        &self,
        id: &PluginId,
        version: &PluginVersion,
    ) -> Result<InstalledPlugin, PackageCacheError> {
        let prepared = self.resolve_prepared(id, version)?;
        Ok(self.receipt(prepared, InstallDisposition::AlreadyPresent))
    }

    /// Resolve one reference into the exact rehashed package bytes without
    /// exposing an ambient activation path.
    pub(crate) fn resolve_prepared(
        &self,
        id: &PluginId,
        version: &PluginVersion,
    ) -> Result<PreparedPackage, PackageCacheError> {
        self.validate_layout()?;
        let manifest_ref = ManifestRef { id, version };
        let hash = self
            .read_reference_for(manifest_ref)?
            .ok_or_else(|| corrupt(CacheCorruption::InvalidReference))?;
        self.load_object(id, version, &hash)
    }

    /// Verify every committed ref/object and return a deterministic path-free
    /// inspection snapshot.
    ///
    /// # Errors
    /// Any unsafe, malformed, missing, or content-mismatched row rejects the
    /// complete snapshot.
    pub fn inspect(&self) -> Result<PluginCacheSnapshot, PackageCacheError> {
        self.validate_layout()?;
        let records = self.reference_records()?;
        let mut packages = Vec::with_capacity(records.len());
        for record in records {
            let package = self.load_object(&record.id, &record.version, &record.hash)?;
            packages.push(CachedPluginSummary {
                id: package.manifest.id().clone(),
                version: package.manifest.version().clone(),
                content_hash: package.content_hash,
                contribution_count: package.manifest.contributions().len(),
                default_enabled: package.manifest.default_enabled(),
            });
        }
        packages.sort_by(|left, right| {
            left.id.cmp(&right.id).then_with(|| {
                left.version
                    .precedence_cmp(&right.version)
                    .then_with(|| left.version.as_str().cmp(right.version.as_str()))
            })
        });
        Ok(PluginCacheSnapshot {
            schema_version: PLUGIN_CACHE_SCHEMA_VERSION,
            packages,
        })
    }

    /// Remove abandoned staging/lock state and complete unreferenced objects at
    /// least `minimum_age` old. Committed refs and their objects are never
    /// cleanup targets.
    ///
    /// # Errors
    /// Unsafe transient entries or filesystem failures stop cleanup without
    /// widening the deletion scope.
    pub fn cleanup_stale(
        &self,
        minimum_age: Duration,
    ) -> Result<CacheCleanupReport, PackageCacheError> {
        self.validate_layout()?;
        let mut report = CacheCleanupReport {
            staging_directories_removed: 0,
            lock_files_removed: 0,
            orphan_objects_removed: 0,
        };
        let staging = self.root.join(STAGING_DIRECTORY);
        self.cleanup_staging(minimum_age, &mut report)?;
        self.cleanup_locks(&self.root.join(LOCKS_DIRECTORY), minimum_age, &mut report)?;
        self.cleanup_orphan_objects(minimum_age, &mut report)?;
        sync_directory(&staging)?;
        Ok(report)
    }

    fn validate_layout(&self) -> Result<(), PackageCacheError> {
        validate_directory(&self.root)?;
        validate_schema(&self.root)?;
        let expected = [
            SCHEMA_FILE,
            OBJECTS_DIRECTORY,
            REFS_DIRECTORY,
            STAGING_DIRECTORY,
            LOCKS_DIRECTORY,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let actual = read_entries(&self.root)?
            .iter()
            .map(entry_name)
            .collect::<Result<BTreeSet<_>, _>>()?;
        if actual.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected {
            return Err(corrupt(CacheCorruption::UnexpectedEntry));
        }
        for relative in [
            OBJECTS_DIRECTORY,
            ".objects/sha256",
            REFS_DIRECTORY,
            STAGING_DIRECTORY,
            LOCKS_DIRECTORY,
            ".locks/version",
            ".locks/object",
        ] {
            validate_directory(&self.root.join(relative))?;
        }
        Ok(())
    }

    fn ensure_object(&self, prepared: &PreparedPackage) -> Result<bool, PackageCacheError> {
        let object_path = self.object_path(&prepared.content_hash);
        match std::fs::symlink_metadata(&object_path) {
            Ok(_) => {
                let _verified = self.load_object(
                    prepared.manifest.id(),
                    prepared.manifest.version(),
                    &prepared.content_hash,
                )?;
                return Ok(false);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(io("inspect_object")),
        }

        let mut stage = self.create_stage("object")?;
        for entry in &prepared.entries {
            let target = stage.path().join(&entry.path);
            match &entry.data {
                PackageEntryData::Directory => ensure_directory(&target)?,
                PackageEntryData::File { bytes, executable } => {
                    write_new_file(&target, bytes, *executable)?;
                }
            }
        }
        sync_package_directories(stage.path(), &prepared.entries)?;
        let staged = self.read_cached_package(stage.path())?;
        self.verify_prepared_identity(
            &staged,
            prepared.manifest.id(),
            prepared.manifest.version(),
            &prepared.content_hash,
        )?;
        self.validate_object_modes(stage.path(), &staged)?;
        match std::fs::rename(stage.path(), &object_path) {
            Ok(()) => {
                stage.disarm();
                let objects = self.root.join(".objects/sha256");
                if let Err(error) = sync_directory(&objects) {
                    self.remove_tree(&object_path)?;
                    let _ = sync_directory(&objects);
                    return Err(error);
                }
                Ok(true)
            }
            Err(_) if object_path.exists() => {
                let _verified = self.load_object(
                    prepared.manifest.id(),
                    prepared.manifest.version(),
                    &prepared.content_hash,
                )?;
                Ok(false)
            }
            Err(_) => Err(io("commit_object")),
        }
    }

    fn load_object(
        &self,
        expected_id: &PluginId,
        expected_version: &PluginVersion,
        expected_hash: &PackageContentHash,
    ) -> Result<PreparedPackage, PackageCacheError> {
        let object_path = self.object_path(expected_hash);
        match std::fs::symlink_metadata(&object_path) {
            Ok(_) => validate_directory(&object_path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(corrupt(CacheCorruption::MissingObject));
            }
            Err(_) => return Err(io("inspect_object")),
        }
        let prepared = self.read_cached_package(&object_path)?;
        self.verify_prepared_identity(&prepared, expected_id, expected_version, expected_hash)?;
        self.validate_object_modes(&object_path, &prepared)?;
        Ok(prepared)
    }

    fn verify_prepared_identity(
        &self,
        prepared: &PreparedPackage,
        expected_id: &PluginId,
        expected_version: &PluginVersion,
        expected_hash: &PackageContentHash,
    ) -> Result<(), PackageCacheError> {
        if &prepared.content_hash != expected_hash {
            return Err(corrupt(CacheCorruption::ContentHashMismatch));
        }
        if prepared.manifest.id() != expected_id || prepared.manifest.version() != expected_version
        {
            return Err(corrupt(CacheCorruption::ManifestIdentityMismatch));
        }
        Ok(())
    }

    fn validate_object_modes(
        &self,
        root: &Path,
        prepared: &PreparedPackage,
    ) -> Result<(), PackageCacheError> {
        validate_directory(root)?;
        for entry in &prepared.entries {
            let path = root.join(&entry.path);
            match &entry.data {
                PackageEntryData::Directory => validate_directory(&path)?,
                PackageEntryData::File { bytes, executable } => {
                    let maximum = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                    let actual = read_cache_file(&path, maximum, Some(*executable))?;
                    if &actual != bytes {
                        return Err(corrupt(CacheCorruption::ContentHashMismatch));
                    }
                }
            }
        }
        Ok(())
    }

    fn write_reference(
        &self,
        manifest: &PluginManifest,
        hash: &PackageContentHash,
    ) -> Result<(), PackageCacheError> {
        let reference = self.reference_path(manifest.id(), manifest.version())?;
        let parent = reference.parent().ok_or(PackageCacheError::InvalidRoot)?;
        self.ensure_reference_parent(manifest.id())?;
        let mut stage = self.create_stage("ref")?;
        let staged_reference = stage.path().join("content");
        let bytes = render_reference(manifest.version(), hash);
        write_new_file(&staged_reference, bytes.as_bytes(), false)?;
        match std::fs::hard_link(&staged_reference, &reference) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = self
                    .read_reference_for(ManifestRef {
                        id: manifest.id(),
                        version: manifest.version(),
                    })?
                    .ok_or_else(|| corrupt(CacheCorruption::InvalidReference))?;
                if &existing == hash {
                    return Ok(());
                }
                return Err(PackageCacheError::VersionConflict {
                    id: manifest.id().clone(),
                    version: manifest.version().clone(),
                });
            }
            Err(_) => return Err(io("commit_reference")),
        }
        std::fs::remove_file(&staged_reference).map_err(|_| io("settle_reference"))?;
        sync_directory(stage.path())?;
        if let Err(error) = sync_directory(parent) {
            std::fs::remove_file(&reference).map_err(|_| io("rollback_reference"))?;
            let _ = sync_directory(parent);
            return Err(error);
        }
        stage.finish();
        Ok(())
    }

    fn read_reference(
        &self,
        manifest: &PluginManifest,
    ) -> Result<Option<PackageContentHash>, PackageCacheError> {
        self.read_reference_for(ManifestRef {
            id: manifest.id(),
            version: manifest.version(),
        })
    }

    fn read_cached_package(&self, root: &Path) -> Result<PreparedPackage, PackageCacheError> {
        read_package_directory(root, &self.validator).map_err(|error| match error {
            PackageCacheError::UnsafeSource {
                issue: crate::PackageSourceIssue::HardLink,
            } => corrupt(CacheCorruption::HardLink),
            PackageCacheError::UnsafeSource {
                issue:
                    crate::PackageSourceIssue::SymbolicLink
                    | crate::PackageSourceIssue::UnsupportedFileType
                    | crate::PackageSourceIssue::ChangedDuringRead,
            } => corrupt(CacheCorruption::UnsafeFileType),
            PackageCacheError::UnsafeSource { .. }
            | PackageCacheError::PackageLimit { .. }
            | PackageCacheError::MissingManifest
            | PackageCacheError::ManifestNotUtf8
            | PackageCacheError::MissingDeclaredPath
            | PackageCacheError::Manifest(_) => corrupt(CacheCorruption::UnexpectedEntry),
            other => other,
        })
    }

    fn read_reference_for(
        &self,
        manifest: ManifestRef<'_>,
    ) -> Result<Option<PackageContentHash>, PackageCacheError> {
        let path = self.reference_path(manifest.id, manifest.version)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(io("inspect_reference")),
        }
        let (version, hash) = read_reference_file(&path)?;
        if &version != manifest.version {
            return Err(corrupt(CacheCorruption::InvalidReference));
        }
        Ok(Some(hash))
    }

    fn reference_records(&self) -> Result<Vec<ReferenceRecord>, PackageCacheError> {
        let mut records = Vec::new();
        let refs = self.root.join(REFS_DIRECTORY);
        for namespace_entry in read_entries(&refs)? {
            let namespace = entry_name(&namespace_entry)?;
            if !valid_kebab(&namespace) {
                return Err(corrupt(CacheCorruption::UnexpectedEntry));
            }
            validate_directory(&namespace_entry.path())?;
            for package_entry in read_entries(&namespace_entry.path())? {
                let package = entry_name(&package_entry)?;
                if !valid_kebab(&package) {
                    return Err(corrupt(CacheCorruption::UnexpectedEntry));
                }
                validate_directory(&package_entry.path())?;
                let id = PluginId::new(format!("{namespace}/{package}"))
                    .map_err(|_| corrupt(CacheCorruption::UnexpectedEntry))?;
                for reference_entry in read_entries(&package_entry.path())? {
                    let file_name = entry_name(&reference_entry)?;
                    let version_key_on_disk = file_name
                        .strip_suffix(".ref")
                        .ok_or_else(|| corrupt(CacheCorruption::UnexpectedEntry))?;
                    let (version, hash) = read_reference_file(&reference_entry.path())?;
                    if version_key(&version) != version_key_on_disk {
                        return Err(corrupt(CacheCorruption::InvalidReference));
                    }
                    records.push(ReferenceRecord {
                        id: id.clone(),
                        version,
                        hash,
                    });
                }
            }
        }
        records.sort_by(|left, right| {
            left.id.cmp(&right.id).then_with(|| {
                left.version
                    .precedence_cmp(&right.version)
                    .then_with(|| left.version.as_str().cmp(right.version.as_str()))
            })
        });
        Ok(records)
    }

    fn hash_is_referenced(&self, expected: &PackageContentHash) -> Result<bool, PackageCacheError> {
        Ok(self
            .reference_records()?
            .iter()
            .any(|record| &record.hash == expected))
    }

    fn receipt(
        &self,
        prepared: PreparedPackage,
        disposition: InstallDisposition,
    ) -> InstalledPlugin {
        let package_root = self.object_path(&prepared.content_hash);
        InstalledPlugin {
            manifest: prepared.manifest,
            content_hash: prepared.content_hash,
            package_root,
            disposition,
        }
    }

    fn object_path(&self, hash: &PackageContentHash) -> PathBuf {
        self.root.join(".objects/sha256").join(hash.hex())
    }

    fn reference_path(
        &self,
        id: &PluginId,
        version: &PluginVersion,
    ) -> Result<PathBuf, PackageCacheError> {
        let (namespace, package) = id_parts(id)?;
        Ok(self
            .root
            .join(REFS_DIRECTORY)
            .join(namespace)
            .join(package)
            .join(format!("{}.ref", version_key(version))))
    }

    fn ensure_reference_parent(&self, id: &PluginId) -> Result<(), PackageCacheError> {
        let (namespace, package) = id_parts(id)?;
        let namespace_path = self.root.join(REFS_DIRECTORY).join(namespace);
        ensure_directory(&namespace_path)?;
        ensure_directory(&namespace_path.join(package))
    }

    fn version_lock_path(&self, manifest: &PluginManifest) -> Result<PathBuf, PackageCacheError> {
        let (namespace, package) = id_parts(manifest.id())?;
        let namespace_path = self.root.join(".locks/version").join(namespace);
        ensure_directory(&namespace_path)?;
        let package_path = namespace_path.join(package);
        ensure_directory(&package_path)?;
        Ok(package_path.join(format!("{}.lock", version_key(manifest.version()))))
    }

    fn object_lock_path(&self, hash: &PackageContentHash) -> PathBuf {
        self.root
            .join(".locks/object")
            .join(format!("{}.lock", hash.hex()))
    }

    fn acquire_lock(&self, path: PathBuf) -> Result<FileLock, PackageCacheError> {
        acquire_file_lock_with_wait(path)
    }

    fn create_stage(&self, label: &str) -> Result<StageDirectory, PackageCacheError> {
        let staging = self.root.join(STAGING_DIRECTORY);
        for _ in 0..1_024 {
            let sequence = WORK_ID.fetch_add(1, AtomicOrdering::Relaxed);
            let name = format!("{label}-{}-{sequence}", std::process::id());
            let path = staging.join(&name);
            let lease_path = staging.join(format!("{name}.lease"));
            let Some(lease) = try_acquire_file_lock(lease_path)? else {
                continue;
            };
            if try_create_directory(&path)? {
                return Ok(StageDirectory {
                    path,
                    relative: PathBuf::from(STAGING_DIRECTORY).join(&name),
                    root_capability: Arc::clone(&self.root_capability),
                    lease: Some(lease),
                    armed: true,
                });
            }
            let _ = lease.remove_if_owned();
        }
        Err(PackageCacheError::Busy)
    }

    fn cleanup_staging(
        &self,
        minimum_age: Duration,
        report: &mut CacheCleanupReport,
    ) -> Result<(), PackageCacheError> {
        let staging = self.root.join(STAGING_DIRECTORY);
        let entries = read_entries(&staging)?;
        for entry in &entries {
            let name = entry_name(entry)?;
            if name.ends_with(".lease") {
                continue;
            }
            if !valid_work_name(&name) {
                return Err(corrupt(CacheCorruption::UnexpectedEntry));
            }
            validate_directory(&entry.path())?;
            if !is_stale(&entry.path(), minimum_age)? {
                continue;
            }
            let lease_path = staging.join(format!("{name}.lease"));
            let (lease, may_remove) = match std::fs::symlink_metadata(&lease_path) {
                Ok(_) => match try_acquire_existing_file_lock(lease_path)? {
                    Some(lease) => (Some(lease), true),
                    None => (None, false),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, true),
                Err(_) => return Err(io("inspect_stage_lease")),
            };
            if may_remove && is_stale(&entry.path(), minimum_age)? {
                self.remove_tree(&entry.path())?;
                if let Some(lease) = lease {
                    let _removed = lease.remove_if_owned()?;
                }
                report.staging_directories_removed += 1;
            }
        }
        for entry in entries {
            let name = entry_name(&entry)?;
            let Some(stage_name) = name.strip_suffix(".lease") else {
                continue;
            };
            if !valid_work_name(stage_name) || !entry.path().exists() {
                continue;
            }
            if staging.join(stage_name).exists() || !lock_file_is_stale(&entry.path(), minimum_age)?
            {
                continue;
            }
            if let Some(lease) = try_acquire_existing_file_lock(entry.path())? {
                let _removed = lease.remove_if_owned()?;
            }
        }
        Ok(())
    }

    fn cleanup_locks(
        &self,
        directory: &Path,
        minimum_age: Duration,
        report: &mut CacheCleanupReport,
    ) -> Result<(), PackageCacheError> {
        validate_directory(directory)?;
        for entry in read_entries(directory)? {
            let name = entry_name(&entry)?;
            if name.ends_with(".lock") {
                let path = entry.path();
                if lock_file_is_stale(&path, minimum_age)?
                    && let Some(lock) = try_acquire_existing_file_lock(path.clone())?
                    && lock_file_is_stale(&path, minimum_age)?
                    && lock.remove_if_owned()?
                {
                    report.lock_files_removed += 1;
                }
            } else {
                validate_directory(&entry.path())?;
                if !valid_work_name(&name) {
                    return Err(corrupt(CacheCorruption::UnexpectedEntry));
                }
                self.cleanup_locks(&entry.path(), minimum_age, report)?;
            }
        }
        Ok(())
    }

    fn cleanup_orphan_objects(
        &self,
        minimum_age: Duration,
        report: &mut CacheCleanupReport,
    ) -> Result<(), PackageCacheError> {
        let objects = self.root.join(".objects/sha256");
        for entry in read_entries(&objects)? {
            let digest = entry_name(&entry)?;
            let hash = PackageContentHash::parse(&format!("sha256:{digest}"))?;
            validate_directory(&entry.path())?;
            if !is_stale(&entry.path(), minimum_age)? {
                continue;
            }
            let _object_lock = self.acquire_lock(self.object_lock_path(&hash))?;
            match std::fs::symlink_metadata(entry.path()) {
                Ok(_) => validate_directory(&entry.path())?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(io("inspect_orphan_object")),
            }
            if is_stale(&entry.path(), minimum_age)? && !self.hash_is_referenced(&hash)? {
                self.remove_tree(&entry.path())?;
                report.orphan_objects_removed += 1;
            }
        }
        sync_directory(&objects)
    }

    fn remove_tree(&self, path: &Path) -> Result<(), PackageCacheError> {
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| corrupt(CacheCorruption::UnsafeFileType))?;
        remove_tree_beneath(&self.root_capability, relative)
    }
}

struct ManifestRef<'a> {
    id: &'a PluginId,
    version: &'a PluginVersion,
}

struct ReferenceRecord {
    id: PluginId,
    version: PluginVersion,
    hash: PackageContentHash,
}

#[cfg(unix)]
struct FileLock {
    file: std::fs::File,
    path: PathBuf,
    _process_lock: ProcessLock,
}

#[cfg(unix)]
impl FileLock {
    fn remove_if_owned(self) -> Result<bool, PackageCacheError> {
        use std::os::unix::fs::MetadataExt as _;

        let opened = self.file.metadata().map_err(|_| io("inspect_lock_file"))?;
        let current = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(io("inspect_lock_file")),
        };
        if current.file_type().is_symlink() || !current.is_file() {
            return Err(corrupt(CacheCorruption::UnsafeFileType));
        }
        if current.nlink() != 1 {
            return Err(corrupt(CacheCorruption::HardLink));
        }
        if opened.dev() != current.dev() || opened.ino() != current.ino() {
            return Ok(false);
        }
        std::fs::remove_file(&self.path).map_err(|_| io("remove_stale_lock"))?;
        if let Some(parent) = self.path.parent() {
            sync_directory(parent)?;
        }
        Ok(true)
    }
}

#[cfg(unix)]
impl Drop for FileLock {
    fn drop(&mut self) {
        unlock_file(&self.file);
    }
}

#[cfg(not(unix))]
struct FileLock;

#[cfg(not(unix))]
impl FileLock {
    fn remove_if_owned(self) -> Result<bool, PackageCacheError> {
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

#[cfg(unix)]
#[allow(deprecated)]
fn try_acquire_file_lock(path: PathBuf) -> Result<Option<FileLock>, PackageCacheError> {
    let Some(process_lock) = try_claim_process_lock(path.clone())? else {
        return Ok(None);
    };
    let file = open_lock_file(&path)?;
    try_lock_open_file(file, path, process_lock)
}

#[cfg(unix)]
fn try_acquire_existing_file_lock(path: PathBuf) -> Result<Option<FileLock>, PackageCacheError> {
    let Some(process_lock) = try_claim_process_lock(path.clone())? else {
        return Ok(None);
    };
    let Some(file) = open_existing_lock_file(&path)? else {
        return Ok(None);
    };
    try_lock_open_file(file, path, process_lock)
}

#[cfg(unix)]
#[allow(deprecated)]
fn try_lock_open_file(
    file: std::fs::File,
    path: PathBuf,
    process_lock: ProcessLock,
) -> Result<Option<FileLock>, PackageCacheError> {
    use std::os::fd::AsRawFd as _;

    use nix::errno::Errno;
    use nix::fcntl::{FlockArg, flock};

    match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
        Ok(()) if locked_file_matches_path(&file, &path)? => Ok(Some(FileLock {
            file,
            path,
            _process_lock: process_lock,
        })),
        Ok(()) => {
            unlock_file(&file);
            Ok(None)
        }
        Err(error) if error == Errno::EWOULDBLOCK => Ok(None),
        Err(_) => Err(io("lock_cache_file")),
    }
}

#[cfg(unix)]
fn locked_file_matches_path(file: &std::fs::File, path: &Path) -> Result<bool, PackageCacheError> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = file.metadata().map_err(|_| io("inspect_lock_file"))?;
    if opened.nlink() == 0 {
        return Ok(false);
    }
    if opened.nlink() != 1 {
        return Err(corrupt(CacheCorruption::HardLink));
    }
    let current = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(io("inspect_lock_file")),
    };
    if current.file_type().is_symlink() || !current.is_file() {
        return Err(corrupt(CacheCorruption::UnsafeFileType));
    }
    if current.nlink() != 1 {
        return Err(corrupt(CacheCorruption::HardLink));
    }
    Ok(opened.dev() == current.dev() && opened.ino() == current.ino())
}

#[cfg(not(unix))]
fn try_acquire_file_lock(_path: PathBuf) -> Result<Option<FileLock>, PackageCacheError> {
    Err(PackageCacheError::UnsupportedSecurity)
}

#[cfg(not(unix))]
fn try_acquire_existing_file_lock(_path: PathBuf) -> Result<Option<FileLock>, PackageCacheError> {
    Err(PackageCacheError::UnsupportedSecurity)
}

#[cfg(unix)]
#[allow(deprecated)]
fn unlock_file(file: &std::fs::File) {
    use std::os::fd::AsRawFd as _;

    let _ = nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::Unlock);
}

fn lock_file_is_stale(path: &Path, minimum_age: Duration) -> Result<bool, PackageCacheError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(io("inspect_stale_lock")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(corrupt(CacheCorruption::UnsafeFileType));
    }
    let modified = metadata.modified().map_err(|_| io("inspect_stale_lock"))?;
    Ok(std::time::SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= minimum_age))
}

struct ProcessLock {
    path: PathBuf,
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        if let Ok(mut locks) = process_locks().lock() {
            locks.remove(&self.path);
        }
    }
}

fn try_claim_process_lock(path: PathBuf) -> Result<Option<ProcessLock>, PackageCacheError> {
    let mut locks = process_locks()
        .lock()
        .map_err(|_| io("lock_process_state"))?;
    if !locks.insert(path.clone()) {
        return Ok(None);
    }
    Ok(Some(ProcessLock { path }))
}

fn process_locks() -> &'static Mutex<BTreeSet<PathBuf>> {
    PROCESS_LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn acquire_file_lock_with_wait(path: PathBuf) -> Result<FileLock, PackageCacheError> {
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        if let Some(lock) = try_acquire_file_lock(path.clone())? {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err(PackageCacheError::Busy);
        }
        std::thread::sleep(LOCK_RETRY);
    }
}

struct StageDirectory {
    path: PathBuf,
    relative: PathBuf,
    root_capability: Arc<Dir>,
    lease: Option<FileLock>,
    armed: bool,
}

impl StageDirectory {
    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
        self.release_lease();
    }

    fn finish(&mut self) {
        if std::fs::remove_dir(&self.path).is_ok() {
            self.armed = false;
            self.release_lease();
        }
    }

    fn release_lease(&mut self) {
        if let Some(lease) = self.lease.take() {
            let _ = lease.remove_if_owned();
        }
    }
}

impl Drop for StageDirectory {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_tree_beneath(&self.root_capability, &self.relative);
        }
        self.release_lease();
    }
}

fn remove_tree_beneath(root: &Dir, relative: &Path) -> Result<(), PackageCacheError> {
    let relative_text = relative
        .to_str()
        .filter(|value| valid_relative_path(value))
        .ok_or_else(|| corrupt(CacheCorruption::UnsafeFileType))?;
    match root.remove_dir_all(relative_text) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(io("remove_cache_tree")),
    }
}

fn id_parts(id: &PluginId) -> Result<(&str, &str), PackageCacheError> {
    id.as_str()
        .split_once('/')
        .ok_or_else(|| corrupt(CacheCorruption::ManifestIdentityMismatch))
}

fn sync_package_directories(
    root: &Path,
    entries: &[crate::cache_source::PackageEntry],
) -> Result<(), PackageCacheError> {
    let mut directories = entries
        .iter()
        .filter_map(|entry| match entry.data {
            PackageEntryData::Directory => Some(root.join(&entry.path)),
            PackageEntryData::File { .. } => None,
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sync_directory(&directory)?;
    }
    sync_directory(root)
}

fn read_entries(path: &Path) -> Result<Vec<std::fs::DirEntry>, PackageCacheError> {
    let mut entries = std::fs::read_dir(path)
        .map_err(|_| io("read_cache_directory"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io("read_cache_directory"))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    Ok(entries)
}

fn entry_name(entry: &std::fs::DirEntry) -> Result<String, PackageCacheError> {
    entry
        .file_name()
        .into_string()
        .map_err(|_| corrupt(CacheCorruption::UnexpectedEntry))
}

fn valid_work_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 160
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn version_key(version: &PluginVersion) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"dshx-plugin-version-key\0\x01");
    hasher.update(version.as_str().as_bytes());
    let digest = hasher.finalize();
    let mut rendered = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

fn render_reference(version: &PluginVersion, hash: &PackageContentHash) -> String {
    format!(
        "schema_version = {}\nversion = {}\ncontent = {}\n",
        PLUGIN_CACHE_SCHEMA_VERSION,
        version.as_str(),
        hash.as_str()
    )
}

fn read_reference_file(
    path: &Path,
) -> Result<(PluginVersion, PackageContentHash), PackageCacheError> {
    let deadline = Instant::now() + REFERENCE_READ_WAIT;
    let bytes = loop {
        match read_cache_file(path, MAX_REFERENCE_BYTES, Some(false)) {
            Err(PackageCacheError::CorruptCache {
                reason: CacheCorruption::HardLink,
            }) if Instant::now() < deadline => std::thread::sleep(LOCK_RETRY),
            outcome => break outcome?,
        }
    };
    let raw =
        std::str::from_utf8(&bytes).map_err(|_| corrupt(CacheCorruption::InvalidReference))?;
    let mut lines = raw.lines();
    let schema = lines
        .next()
        .and_then(|line| line.strip_prefix("schema_version = "))
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| corrupt(CacheCorruption::InvalidReference))?;
    if schema != PLUGIN_CACHE_SCHEMA_VERSION {
        return Err(PackageCacheError::UnsupportedCacheSchema {
            found: schema,
            supported: PLUGIN_CACHE_SCHEMA_VERSION,
        });
    }
    let version = lines
        .next()
        .and_then(|line| line.strip_prefix("version = "))
        .ok_or_else(|| corrupt(CacheCorruption::InvalidReference))
        .and_then(|value| {
            PluginVersion::parse(value).map_err(|_| corrupt(CacheCorruption::InvalidReference))
        })?;
    let hash = lines
        .next()
        .and_then(|line| line.strip_prefix("content = "))
        .ok_or_else(|| corrupt(CacheCorruption::InvalidReference))
        .and_then(PackageContentHash::parse)?;
    if lines.next().is_some() || !raw.ends_with('\n') {
        return Err(corrupt(CacheCorruption::InvalidReference));
    }
    Ok((version, hash))
}

fn initialize_schema(root: &Path) -> Result<(), PackageCacheError> {
    let marker = root.join(SCHEMA_FILE);
    let initialization_lock_path = root.join(INIT_LOCK_FILE);
    let initialization_lock = acquire_file_lock_with_wait(initialization_lock_path)?;
    match std::fs::symlink_metadata(&marker) {
        Ok(_) => validate_schema(root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let names = read_entries(root)?
                .iter()
                .map(entry_name)
                .collect::<Result<BTreeSet<_>, _>>()?;
            if names != BTreeSet::from([INIT_LOCK_FILE.to_owned()]) {
                return Err(corrupt(CacheCorruption::UnexpectedEntry));
            }
            let bytes = format!("schema_version = {}\n", PLUGIN_CACHE_SCHEMA_VERSION);
            write_new_file(&marker, bytes.as_bytes(), false)?;
            sync_directory(root)?;
        }
        Err(_) => return Err(io("inspect_cache_schema")),
    }
    if !initialization_lock.remove_if_owned()? {
        return Err(io("settle_cache_schema_lock"));
    }
    sync_directory(root)
}

fn validate_schema(root: &Path) -> Result<(), PackageCacheError> {
    let bytes = read_cache_file(&root.join(SCHEMA_FILE), 64, Some(false))?;
    let raw =
        std::str::from_utf8(&bytes).map_err(|_| corrupt(CacheCorruption::InvalidReference))?;
    let found = raw
        .strip_prefix("schema_version = ")
        .and_then(|value| value.strip_suffix('\n'))
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| corrupt(CacheCorruption::InvalidReference))?;
    if found != PLUGIN_CACHE_SCHEMA_VERSION {
        return Err(PackageCacheError::UnsupportedCacheSchema {
            found,
            supported: PLUGIN_CACHE_SCHEMA_VERSION,
        });
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::{ApiVersion, Architecture, OperatingSystem, PlatformTarget};

    use super::*;

    #[test]
    fn reference_publication_never_replaces_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let validator = ManifestValidator::new(
            ApiVersion::new(1).unwrap(),
            PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
        );
        let cache = PluginInstallCache::open(temp.path().join("cache"), validator.clone()).unwrap();
        let raw = r#"schema_version = 1
id = "local/no-clobber"
name = "No clobber"
version = "1.0.0"
description = "Reference publication fixture."
license = "MIT"
default_enabled = false
requested_permissions = []
platforms = [{ os = "macos", architecture = "aarch64" }]
dependencies = []
conflicts = []
contributions = [{ kind = "skill", id = "review", path = "skills/review/SKILL.md", exposure = { mode = "namespaced" } }]

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixtures/no-clobber"
revision = "v1"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#;
        let manifest = validator.validate_toml(raw).unwrap();
        let existing = PackageContentHash::parse(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let candidate = PackageContentHash::parse(
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        )
        .unwrap();
        cache.ensure_reference_parent(manifest.id()).unwrap();
        let destination = cache
            .reference_path(manifest.id(), manifest.version())
            .unwrap();
        write_new_file(
            &destination,
            render_reference(manifest.version(), &existing).as_bytes(),
            false,
        )
        .unwrap();
        let before = read_cache_file(&destination, MAX_REFERENCE_BYTES, Some(false)).unwrap();

        assert!(matches!(
            cache.write_reference(&manifest, &candidate),
            Err(PackageCacheError::VersionConflict { .. })
        ));
        let after = read_cache_file(&destination, MAX_REFERENCE_BYTES, Some(false)).unwrap();
        assert_eq!(after, before);
        let (_, committed) = read_reference_file(&destination).unwrap();
        assert_eq!(committed, existing);
    }

    #[test]
    fn process_lock_registry_serializes_same_process_file_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        let validator = ManifestValidator::new(
            ApiVersion::new(1).unwrap(),
            PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
        );
        let cache = PluginInstallCache::open(temp.path().join("cache"), validator).unwrap();
        let path = cache.root().join(".locks/object/process-registry.lock");
        let first = cache.acquire_lock(path.clone()).unwrap();
        assert!(try_acquire_file_lock(path.clone()).unwrap().is_none());
        drop(first);
        assert!(try_acquire_file_lock(path).unwrap().is_some());
    }
}

#[cfg(all(test, not(unix)))]
mod non_unix_tests {
    #![allow(clippy::unwrap_used)]

    use crate::{
        ApiVersion, Architecture, ManifestValidator, OperatingSystem, PackageCacheError,
        PlatformTarget,
    };

    use super::PluginInstallCache;

    #[test]
    fn cache_fails_closed_without_an_owner_only_platform_backend() {
        let temp = tempfile::tempdir().unwrap();
        let validator = ManifestValidator::new(
            ApiVersion::new(1).unwrap(),
            PlatformTarget::new(OperatingSystem::Windows, Architecture::X86_64),
        );
        assert!(matches!(
            PluginInstallCache::open(temp.path().join("cache"), validator),
            Err(PackageCacheError::UnsupportedSecurity)
        ));
    }
}

//! Owner-only schema-v1 trust-store persistence.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{
    TRUST_FILE_SCHEMA_VERSION, WorkspaceId, WorkspaceIdentity, WorkspaceTrustDecision,
    WorkspaceTrustError,
};

#[cfg(unix)]
const MAX_STORE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    root: String,
    decision: WorkspaceTrustDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireDocument {
    schema_version: u32,
    generation: u64,
    #[serde(default)]
    workspaces: BTreeMap<String, WireRecord>,
}

impl Default for WireDocument {
    fn default() -> Self {
        Self {
            schema_version: TRUST_FILE_SCHEMA_VERSION,
            generation: 0,
            workspaces: BTreeMap::new(),
        }
    }
}

pub(crate) struct LoadedTrust {
    pub decision: Option<WorkspaceTrustDecision>,
    pub generation: u64,
}

#[derive(Clone)]
pub(crate) enum TrustStore {
    Memory(Arc<Mutex<WireDocument>>),
    #[cfg(unix)]
    File(Arc<FileStore>),
}

#[cfg(unix)]
pub(crate) struct FileStore {
    directory: cap_std::fs::Dir,
    name: OsString,
    owner_uid: u32,
    lock_file: std::fs::File,
    writer: Mutex<()>,
}

#[cfg(unix)]
struct StoreFileLock {
    file: std::fs::File,
}

#[cfg(unix)]
impl Drop for StoreFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl TrustStore {
    pub fn memory() -> Self {
        Self::Memory(Arc::new(Mutex::new(WireDocument::default())))
    }

    pub fn file(path: impl Into<PathBuf>) -> Result<Self, WorkspaceTrustError> {
        let path = path.into();
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(WorkspaceTrustError::InvalidStore);
        }
        #[cfg(unix)]
        {
            FileStore::open(&path).map(|store| Self::File(Arc::new(store)))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(WorkspaceTrustError::UnsupportedSecurity)
        }
    }

    pub fn load(&self, identity: &WorkspaceIdentity) -> Result<LoadedTrust, WorkspaceTrustError> {
        match self {
            Self::Memory(document) => {
                let document = document
                    .lock()
                    .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
                loaded_from_document(&document, identity)
            }
            #[cfg(unix)]
            Self::File(store) => {
                let _guard = store
                    .writer
                    .lock()
                    .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
                let _file_lock = store.lock()?;
                let document = read_document(store)?;
                loaded_from_document(&document, identity)
            }
        }
    }

    pub fn commit(
        &self,
        identity: &WorkspaceIdentity,
        decision: Option<WorkspaceTrustDecision>,
        expected_generation: u64,
    ) -> Result<u64, WorkspaceTrustError> {
        match self {
            Self::Memory(slot) => {
                let mut document = slot
                    .lock()
                    .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
                commit_document(&mut document, identity, decision, expected_generation)
            }
            #[cfg(unix)]
            Self::File(store) => {
                let _guard = store
                    .writer
                    .lock()
                    .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
                let _file_lock = store.lock()?;
                let mut document = read_document(store)?;
                let generation =
                    commit_document(&mut document, identity, decision, expected_generation)?;
                write_document(store, &document)?;
                Ok(generation)
            }
        }
    }
}

fn loaded_from_document(
    document: &WireDocument,
    identity: &WorkspaceIdentity,
) -> Result<LoadedTrust, WorkspaceTrustError> {
    validate_document(document)?;
    let decision = document
        .workspaces
        .get(identity.id().as_str())
        .map(|record| {
            if record.root != identity.normalized_root() || !record.decision.is_explicit() {
                Err(WorkspaceTrustError::InvalidStore)
            } else {
                Ok(record.decision)
            }
        })
        .transpose()?;
    Ok(LoadedTrust {
        decision,
        generation: document.generation,
    })
}

fn commit_document(
    document: &mut WireDocument,
    identity: &WorkspaceIdentity,
    decision: Option<WorkspaceTrustDecision>,
    expected_generation: u64,
) -> Result<u64, WorkspaceTrustError> {
    validate_document(document)?;
    if document.generation != expected_generation {
        return Err(WorkspaceTrustError::StoreChanged);
    }
    let generation = document
        .generation
        .checked_add(1)
        .ok_or(WorkspaceTrustError::RevisionExhausted)?;
    match decision {
        Some(decision) if decision.is_explicit() => {
            document.workspaces.insert(
                identity.id().as_str().to_owned(),
                WireRecord {
                    root: identity.normalized_root().to_owned(),
                    decision,
                },
            );
        }
        Some(_) => return Err(WorkspaceTrustError::InvalidDecision),
        None => {
            document.workspaces.remove(identity.id().as_str());
        }
    }
    document.generation = generation;
    Ok(generation)
}

fn validate_document(document: &WireDocument) -> Result<(), WorkspaceTrustError> {
    if document.schema_version != TRUST_FILE_SCHEMA_VERSION {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    for (id, record) in &document.workspaces {
        WorkspaceId::validate_stored(id)?;
        if !record.decision.is_explicit()
            || WorkspaceId::from_normalized_root(&record.root)?.as_str() != id
        {
            return Err(WorkspaceTrustError::InvalidStore);
        }
    }
    Ok(())
}

#[cfg(unix)]
impl FileStore {
    fn open(path: &std::path::Path) -> Result<Self, WorkspaceTrustError> {
        let parent = path.parent().ok_or(WorkspaceTrustError::InvalidStore)?;
        if parent.parent().is_none() {
            return Err(WorkspaceTrustError::InvalidStore);
        }
        std::fs::create_dir_all(parent).map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
        let before =
            std::fs::symlink_metadata(parent).map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
        if before.file_type().is_symlink() || !before.is_dir() {
            return Err(WorkspaceTrustError::InvalidStore);
        }
        let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())
            .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
        validate_directory_binding(parent, &before, &directory)?;
        let owner_uid = discover_owner_uid(&directory)?;
        secure_directory(&directory, owner_uid)?;
        let name = path
            .file_name()
            .ok_or(WorkspaceTrustError::InvalidStore)?
            .to_os_string();
        let lock_name = lock_name(&name);
        let lock_file =
            open_or_create_owned_file(&directory, &lock_name, owner_uid, Some(0), true)?;
        Ok(Self {
            directory,
            name,
            owner_uid,
            lock_file,
            writer: Mutex::new(()),
        })
    }

    fn lock(&self) -> Result<StoreFileLock, WorkspaceTrustError> {
        let file = self
            .lock_file
            .try_clone()
            .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
        file.lock()
            .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
        Ok(StoreFileLock { file })
    }
}

#[cfg(unix)]
fn read_document(store: &FileStore) -> Result<WireDocument, WorkspaceTrustError> {
    let Some(input) = open_existing_owned_file(
        &store.directory,
        &store.name,
        store.owner_uid,
        Some(MAX_STORE_BYTES),
        false,
    )?
    else {
        return Ok(WireDocument::default());
    };
    let mut raw = String::new();
    input
        .take(MAX_STORE_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_STORE_BYTES {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    let document =
        toml::from_str::<WireDocument>(&raw).map_err(|_| WorkspaceTrustError::InvalidStore)?;
    validate_document(&document)?;
    Ok(document)
}

#[cfg(unix)]
fn write_document(store: &FileStore, document: &WireDocument) -> Result<(), WorkspaceTrustError> {
    validate_document(document)?;
    let raw = toml::to_string_pretty(document).map_err(|_| WorkspaceTrustError::InvalidStore)?;
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_STORE_BYTES {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    validate_destination(store)?;
    let (temporary_name, mut output) = create_temporary_file(store)?;
    let mut cleanup = TemporaryEntry {
        directory: &store.directory,
        name: temporary_name.clone(),
        armed: true,
    };
    output
        .write_all(raw.as_bytes())
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    output
        .sync_all()
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    let output_metadata = output
        .metadata()
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    validate_std_file_metadata(&output_metadata, store.owner_uid, Some(MAX_STORE_BYTES))?;
    validate_destination(store)?;
    store
        .directory
        .rename(&temporary_name, &store.directory, &store.name)
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    cleanup.armed = false;
    let committed = store
        .directory
        .symlink_metadata(&store.name)
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    validate_cap_file_metadata(&committed, store.owner_uid, Some(MAX_STORE_BYTES))?;
    if !same_std_cap_file(&output_metadata, &committed) {
        return Err(WorkspaceTrustError::StoreUnavailable);
    }
    sync_directory(&store.directory)
}

#[cfg(unix)]
struct TemporaryEntry<'a> {
    directory: &'a cap_std::fs::Dir,
    name: OsString,
    armed: bool,
}

#[cfg(unix)]
impl Drop for TemporaryEntry<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.directory.remove_file(&self.name);
        }
    }
}

#[cfg(unix)]
fn validate_directory_binding(
    path: &std::path::Path,
    before: &std::fs::Metadata,
    directory: &cap_std::fs::Dir,
) -> Result<(), WorkspaceTrustError> {
    let opened = directory
        .metadata(".")
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    let after =
        std::fs::symlink_metadata(path).map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    if after.file_type().is_symlink()
        || !after.is_dir()
        || !same_std_file(before, &after)
        || !same_std_cap_file(before, &opened)
    {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    Ok(())
}

#[cfg(unix)]
fn discover_owner_uid(directory: &cap_std::fs::Dir) -> Result<u32, WorkspaceTrustError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
    use cap_std::fs::{MetadataExt as _, OpenOptionsExt as _};

    for _ in 0..128 {
        let name = unique_name(".heycode-trust-owner");
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No);
        match directory.open_with(&name, &options) {
            Ok(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
                let uid = metadata.uid();
                validate_cap_file_metadata(&metadata, uid, Some(0))?;
                directory
                    .remove_file(&name)
                    .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
                return Ok(uid);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(WorkspaceTrustError::StoreUnavailable),
        }
    }
    Err(WorkspaceTrustError::StoreUnavailable)
}

#[cfg(unix)]
fn secure_directory(
    directory: &cap_std::fs::Dir,
    owner_uid: u32,
) -> Result<(), WorkspaceTrustError> {
    use cap_std::fs::{MetadataExt as _, PermissionsExt as _};

    let metadata = directory
        .metadata(".")
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    if metadata.uid() != owner_uid {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    directory
        .set_permissions(".", cap_std::fs::Permissions::from_mode(0o700))
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    let secured = directory
        .metadata(".")
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    if secured.uid() != owner_uid || secured.mode() & 0o777 != 0o700 {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    Ok(())
}

#[cfg(unix)]
fn open_or_create_owned_file(
    directory: &cap_std::fs::Dir,
    name: &OsStr,
    owner_uid: u32,
    max_bytes: Option<u64>,
    writable: bool,
) -> Result<std::fs::File, WorkspaceTrustError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
    use cap_std::fs::OpenOptionsExt as _;

    let mut create = cap_std::fs::OpenOptions::new();
    create
        .read(true)
        .write(writable)
        .create_new(true)
        .mode(0o600)
        .follow(FollowSymlinks::No);
    match directory.open_with(name, &create) {
        Ok(file) => validate_opened_file(directory, name, file, owner_uid, max_bytes),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_existing_owned_file(directory, name, owner_uid, max_bytes, writable)?
                .ok_or(WorkspaceTrustError::StoreUnavailable)
        }
        Err(_) => Err(WorkspaceTrustError::StoreUnavailable),
    }
}

#[cfg(unix)]
fn open_existing_owned_file(
    directory: &cap_std::fs::Dir,
    name: &OsStr,
    owner_uid: u32,
    max_bytes: Option<u64>,
    writable: bool,
) -> Result<Option<std::fs::File>, WorkspaceTrustError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};

    let before = match directory.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(WorkspaceTrustError::StoreUnavailable),
    };
    validate_cap_file_metadata(&before, owner_uid, max_bytes)?;
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .read(true)
        .write(writable)
        .follow(FollowSymlinks::No);
    let file = directory
        .open_with(name, &options)
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    validate_opened_file(directory, name, file, owner_uid, max_bytes).map(Some)
}

#[cfg(unix)]
fn validate_opened_file(
    directory: &cap_std::fs::Dir,
    name: &OsStr,
    file: cap_std::fs::File,
    owner_uid: u32,
    max_bytes: Option<u64>,
) -> Result<std::fs::File, WorkspaceTrustError> {
    use cap_std::fs::PermissionsExt as _;

    let opened = file
        .metadata()
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    let current = directory
        .symlink_metadata(name)
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    validate_cap_file_metadata(&opened, owner_uid, max_bytes)?;
    validate_cap_file_metadata(&current, owner_uid, max_bytes)?;
    if !same_cap_file(&opened, &current) {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    file.set_permissions(cap_std::fs::Permissions::from_mode(0o600))
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    let secured = file
        .metadata()
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)?;
    validate_cap_file_metadata(&secured, owner_uid, max_bytes)?;
    use cap_std::fs::MetadataExt as _;
    if secured.mode() & 0o777 != 0o600 {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    Ok(file.into_std())
}

#[cfg(unix)]
fn validate_destination(store: &FileStore) -> Result<(), WorkspaceTrustError> {
    match store.directory.symlink_metadata(&store.name) {
        Ok(metadata) => {
            validate_cap_file_metadata(&metadata, store.owner_uid, Some(MAX_STORE_BYTES))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(WorkspaceTrustError::StoreUnavailable),
    }
}

#[cfg(unix)]
fn create_temporary_file(
    store: &FileStore,
) -> Result<(OsString, std::fs::File), WorkspaceTrustError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
    use cap_std::fs::OpenOptionsExt as _;

    for _ in 0..128 {
        let name = unique_name(".heycode-trust-write");
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No);
        match store.directory.open_with(&name, &options) {
            Ok(file) => {
                let file = validate_opened_file(
                    &store.directory,
                    &name,
                    file,
                    store.owner_uid,
                    Some(MAX_STORE_BYTES),
                )?;
                return Ok((name, file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(WorkspaceTrustError::StoreUnavailable),
        }
    }
    Err(WorkspaceTrustError::StoreUnavailable)
}

#[cfg(unix)]
fn validate_cap_file_metadata(
    metadata: &cap_std::fs::Metadata,
    owner_uid: u32,
    max_bytes: Option<u64>,
) -> Result<(), WorkspaceTrustError> {
    use cap_std::fs::MetadataExt as _;

    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != owner_uid
        || max_bytes.is_some_and(|max| metadata.len() > max)
    {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_std_file_metadata(
    metadata: &std::fs::Metadata,
    owner_uid: u32,
    max_bytes: Option<u64>,
) -> Result<(), WorkspaceTrustError> {
    use std::os::unix::fs::MetadataExt as _;

    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != owner_uid
        || max_bytes.is_some_and(|max| metadata.len() > max)
    {
        return Err(WorkspaceTrustError::InvalidStore);
    }
    Ok(())
}

#[cfg(unix)]
fn same_std_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(unix)]
fn same_cap_file(left: &cap_std::fs::Metadata, right: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt as _;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(unix)]
fn same_std_cap_file(left: &std::fs::Metadata, right: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt as _;
    use std::os::unix::fs::MetadataExt as _;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(unix)]
fn sync_directory(directory: &cap_std::fs::Dir) -> Result<(), WorkspaceTrustError> {
    directory
        .try_clone()
        .and_then(|directory| directory.into_std_file().sync_all())
        .map_err(|_| WorkspaceTrustError::StoreUnavailable)
}

#[cfg(unix)]
fn lock_name(name: &OsStr) -> OsString {
    let mut lock = name.to_os_string();
    lock.push(".lock");
    lock
}

#[cfg(unix)]
fn unique_name(prefix: &str) -> OsString {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}.{}.{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
    .into()
}

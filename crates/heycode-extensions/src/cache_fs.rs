//! Owner-only cache filesystem primitives and exact-tree cleanup.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cache_error::{corrupt, io};
use crate::{CacheCorruption, PackageCacheError};

pub(crate) fn open_root(root: &Path) -> Result<PathBuf, PackageCacheError> {
    #[cfg(unix)]
    {
        unix::open_root(root)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn ensure_directory(path: &Path) -> Result<(), PackageCacheError> {
    #[cfg(unix)]
    {
        unix::ensure_directory(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn try_create_directory(path: &Path) -> Result<bool, PackageCacheError> {
    #[cfg(unix)]
    {
        unix::try_create_directory(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn validate_directory(path: &Path) -> Result<(), PackageCacheError> {
    #[cfg(unix)]
    {
        unix::validate_directory(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn write_new_file(
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> Result<(), PackageCacheError> {
    #[cfg(unix)]
    {
        unix::write_new_file(path, bytes, executable)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, bytes, executable);
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn read_cache_file(
    path: &Path,
    maximum: u64,
    executable: Option<bool>,
) -> Result<Vec<u8>, PackageCacheError> {
    #[cfg(unix)]
    {
        unix::read_cache_file(path, maximum, executable)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, maximum, executable);
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn open_lock_file(path: &Path) -> Result<std::fs::File, PackageCacheError> {
    #[cfg(unix)]
    {
        unix::open_lock_file(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn open_existing_lock_file(
    path: &Path,
) -> Result<Option<std::fs::File>, PackageCacheError> {
    #[cfg(unix)]
    {
        unix::open_existing_lock_file(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), PackageCacheError> {
    let directory = std::fs::File::open(path).map_err(|_| io("sync_directory"))?;
    directory.sync_all().map_err(|_| io("sync_directory"))
}

pub(crate) fn is_stale(path: &Path, minimum_age: Duration) -> Result<bool, PackageCacheError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| io("inspect_stale_entry"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(corrupt(CacheCorruption::UnsafeFileType));
    }
    let modified = metadata.modified().map_err(|_| io("inspect_stale_entry"))?;
    Ok(SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= minimum_age))
}

pub(crate) fn validate_single_link(metadata: &std::fs::Metadata) -> Result<(), PackageCacheError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(corrupt(CacheCorruption::HardLink));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

#[cfg(unix)]
mod unix {
    use std::fs::{DirBuilder, File, Metadata, OpenOptions};
    use std::os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };

    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct FileIdentity {
        device: u64,
        inode: u64,
        length: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
        links: u64,
        mode: u32,
    }

    impl FileIdentity {
        fn from_metadata(metadata: &Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
                links: metadata.nlink(),
                mode: metadata.permissions().mode() & 0o777,
            }
        }
    }

    pub(super) fn open_root(root: &Path) -> Result<PathBuf, PackageCacheError> {
        if !root.is_absolute() {
            return Err(PackageCacheError::InvalidRoot);
        }
        match std::fs::symlink_metadata(root) {
            Ok(_) => validate_directory(root)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = root.parent().ok_or(PackageCacheError::InvalidRoot)?;
                let parent_metadata = std::fs::symlink_metadata(parent)
                    .map_err(|_| PackageCacheError::InvalidRoot)?;
                if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
                    return Err(PackageCacheError::InvalidRoot);
                }
                create_directory(root)?;
            }
            Err(_) => return Err(io("inspect_cache_root")),
        }
        let canonical = std::fs::canonicalize(root).map_err(|_| io("canonicalize_cache_root"))?;
        validate_directory(&canonical)?;
        Ok(canonical)
    }

    pub(super) fn ensure_directory(path: &Path) -> Result<(), PackageCacheError> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => validate_directory(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_directory(path),
            Err(_) => Err(io("inspect_cache_directory")),
        }
    }

    pub(super) fn try_create_directory(path: &Path) -> Result<bool, PackageCacheError> {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => {
                set_directory_mode(path)?;
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match std::fs::symlink_metadata(path) {
                    Ok(metadata) => {
                        validate_directory_metadata(&metadata)?;
                        Ok(false)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                    Err(_) => Err(io("inspect_cache_directory")),
                }
            }
            Err(_) => Err(io("create_cache_directory")),
        }
    }

    fn create_directory(path: &Path) -> Result<(), PackageCacheError> {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => {
                set_directory_mode(path)?;
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_directory(path)
            }
            Err(_) => Err(io("create_cache_directory")),
        }
    }

    fn set_directory_mode(path: &Path) -> Result<(), PackageCacheError> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| io("secure_cache_directory"))?;
        validate_directory(path)
    }

    pub(super) fn validate_directory(path: &Path) -> Result<(), PackageCacheError> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| corrupt(CacheCorruption::UnsafeFileType))?;
        validate_directory_metadata(&metadata)
    }

    fn validate_directory_metadata(metadata: &Metadata) -> Result<(), PackageCacheError> {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(corrupt(CacheCorruption::UnsafeFileType));
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(corrupt(CacheCorruption::Permissions));
        }
        Ok(())
    }

    pub(super) fn write_new_file(
        path: &Path,
        bytes: &[u8],
        executable: bool,
    ) -> Result<(), PackageCacheError> {
        let mode = if executable { 0o700 } else { 0o600 };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .map_err(|_| io("create_cache_file"))?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|_| io("secure_cache_file"))?;
        file.write_all(bytes).map_err(|_| io("write_cache_file"))?;
        file.sync_all().map_err(|_| io("sync_cache_file"))?;
        let metadata = file.metadata().map_err(|_| io("inspect_cache_file"))?;
        validate_file_metadata(&metadata, Some(executable))
    }

    pub(super) fn read_cache_file(
        path: &Path,
        maximum: u64,
        executable: Option<bool>,
    ) -> Result<Vec<u8>, PackageCacheError> {
        let before_metadata = std::fs::symlink_metadata(path)
            .map_err(|_| corrupt(CacheCorruption::UnsafeFileType))?;
        validate_file_metadata(&before_metadata, executable)?;
        if before_metadata.len() > maximum {
            return Err(corrupt(CacheCorruption::InvalidReference));
        }
        let before = FileIdentity::from_metadata(&before_metadata);
        let mut file = File::open(path).map_err(|_| io("read_cache_file"))?;
        let opened_metadata = file.metadata().map_err(|_| io("inspect_cache_file"))?;
        validate_file_metadata(&opened_metadata, executable)?;
        if FileIdentity::from_metadata(&opened_metadata) != before {
            return Err(corrupt(CacheCorruption::UnsafeFileType));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(before.length).unwrap_or(0));
        std::io::Read::by_ref(&mut file)
            .take(maximum + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| io("read_cache_file"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
            return Err(corrupt(CacheCorruption::InvalidReference));
        }
        let after_open = file.metadata().map_err(|_| io("inspect_cache_file"))?;
        let after_path = std::fs::symlink_metadata(path)
            .map_err(|_| corrupt(CacheCorruption::UnsafeFileType))?;
        validate_file_metadata(&after_open, executable)?;
        validate_file_metadata(&after_path, executable)?;
        if FileIdentity::from_metadata(&after_open) != before
            || FileIdentity::from_metadata(&after_path) != before
            || u64::try_from(bytes.len()).ok() != Some(before.length)
        {
            return Err(corrupt(CacheCorruption::UnsafeFileType));
        }
        Ok(bytes)
    }

    pub(super) fn open_lock_file(path: &Path) -> Result<File, PackageCacheError> {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => {
                file.sync_all().map_err(|_| io("sync_lock_file"))?;
                let metadata = file.metadata().map_err(|_| io("inspect_lock_file"))?;
                if metadata.nlink() != 0 {
                    validate_file_metadata(&metadata, Some(false))?;
                }
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                }
                Ok(file)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                open_existing_lock_file(path)?.ok_or_else(|| io("open_lock_file"))
            }
            Err(_) => Err(io("create_lock_file")),
        }
    }

    pub(super) fn open_existing_lock_file(path: &Path) -> Result<Option<File>, PackageCacheError> {
        let before = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(io("inspect_lock_file")),
        };
        validate_file_metadata(&before, Some(false))?;
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(io("open_lock_file")),
        };
        let opened = file.metadata().map_err(|_| io("inspect_lock_file"))?;
        if opened.nlink() == 0 {
            return Ok(None);
        }
        validate_file_metadata(&opened, Some(false))?;
        let after_path = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(io("inspect_lock_file")),
        };
        validate_file_metadata(&after_path, Some(false))?;
        if opened.dev() != before.dev()
            || opened.ino() != before.ino()
            || after_path.dev() != before.dev()
            || after_path.ino() != before.ino()
        {
            return Err(corrupt(CacheCorruption::UnsafeFileType));
        }
        Ok(Some(file))
    }

    fn validate_file_metadata(
        metadata: &Metadata,
        executable: Option<bool>,
    ) -> Result<(), PackageCacheError> {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(corrupt(CacheCorruption::UnsafeFileType));
        }
        validate_single_link(metadata)?;
        let actual = metadata.permissions().mode() & 0o777;
        let valid_mode = match executable {
            Some(true) => actual == 0o700,
            Some(false) => actual == 0o600,
            None => matches!(actual, 0o600 | 0o700),
        };
        if !valid_mode {
            return Err(corrupt(CacheCorruption::Permissions));
        }
        Ok(())
    }
}

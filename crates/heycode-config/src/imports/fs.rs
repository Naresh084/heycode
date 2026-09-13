//! Bounded no-follow source reads and owner-relative store operations.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{DirExt as _, FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest as _, Sha256};

use super::{ImportError, MAX_FILE_BYTES};

#[derive(Clone)]
pub(super) struct Anchor {
    directory: Arc<Dir>,
    canonical: PathBuf,
    suffix: PathBuf,
    identity: (u64, u64),
}

impl Anchor {
    pub(super) fn bind(path: &Path, may_be_missing: bool) -> Result<Self, ImportError> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            || std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(ImportError::UnsafePath);
        }
        let mut existing = path.to_path_buf();
        let mut missing = Vec::new();
        let canonical = loop {
            match std::fs::canonicalize(&existing) {
                Ok(path) => break path,
                Err(error) if may_be_missing && error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(
                        existing
                            .file_name()
                            .ok_or(ImportError::UnsafePath)?
                            .to_os_string(),
                    );
                    existing = existing
                        .parent()
                        .ok_or(ImportError::UnsafePath)?
                        .to_path_buf();
                }
                Err(_) => return Err(ImportError::Unavailable),
            }
        };
        let directory = open_absolute(&canonical)?;
        let identity = identity(&directory)?;
        if std::fs::canonicalize(&existing).map_err(|_| ImportError::Stale)? != canonical
            || identity != self::identity(&open_absolute(&canonical)?)?
        {
            return Err(ImportError::Stale);
        }
        let mut suffix = PathBuf::new();
        for part in missing.into_iter().rev() {
            suffix.push(part);
        }
        Ok(Self {
            directory: Arc::new(directory),
            canonical,
            suffix,
            identity,
        })
    }

    pub(super) fn path(&self) -> PathBuf {
        self.canonical.join(&self.suffix)
    }

    pub(super) fn root_identity(&self) -> (u64, u64) {
        self.identity
    }

    pub(super) fn recheck(&self) -> Result<(), ImportError> {
        if identity(&open_absolute(&self.canonical)?)? != self.identity {
            return Err(ImportError::Stale);
        }
        Ok(())
    }

    pub(super) fn open(&self, relative: &Path, create: bool) -> Result<Option<Dir>, ImportError> {
        self.recheck()?;
        let path = self.suffix.join(relative);
        let mut directory = self
            .directory
            .try_clone()
            .map_err(|_| ImportError::Unavailable)?;
        for part in path.components() {
            let Component::Normal(part) = part else {
                return Err(ImportError::UnsafePath);
            };
            match directory.open_dir_nofollow(part) {
                Ok(next) => directory = next,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !create {
                        return Ok(None);
                    }
                    match directory.create_dir(part) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(_) => return Err(ImportError::Unavailable),
                    }
                    let next = directory
                        .open_dir_nofollow(part)
                        .map_err(|_| ImportError::UnsafePath)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        next.set_permissions(
                            ".",
                            cap_std::fs::Permissions::from_std(std::fs::Permissions::from_mode(
                                0o700,
                            )),
                        )
                        .map_err(|_| ImportError::Unavailable)?;
                    }
                    directory = next;
                }
                Err(_) => return Err(ImportError::UnsafePath),
            }
        }
        Ok(Some(directory))
    }
}

fn open_absolute(path: &Path) -> Result<Dir, ImportError> {
    let mut volume = PathBuf::new();
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Prefix(_) | Component::RootDir => volume.push(part.as_os_str()),
            Component::Normal(_) => normal.push(part.as_os_str()),
            _ => return Err(ImportError::UnsafePath),
        }
    }
    if volume.as_os_str().is_empty() {
        return Err(ImportError::UnsafePath);
    }
    let mut directory = Dir::open_ambient_dir(volume, cap_std::ambient_authority())
        .map_err(|_| ImportError::UnsafePath)?;
    for part in normal.components() {
        directory = directory
            .open_dir_nofollow(part.as_os_str())
            .map_err(|_| ImportError::UnsafePath)?;
    }
    Ok(directory)
}

pub(super) fn identity(directory: &Dir) -> Result<(u64, u64), ImportError> {
    let meta = directory
        .dir_metadata()
        .map_err(|_| ImportError::Unavailable)?;
    Ok((meta.dev(), meta.ino()))
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn read_optional(
    directory: &Dir,
    name: &Path,
    maximum: usize,
) -> Result<Option<Vec<u8>>, ImportError> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = match directory.open_with(name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ImportError::UnsafePath),
    };
    let before = file.metadata().map_err(|_| ImportError::Unavailable)?;
    if !before.is_file() || before.nlink() != 1 || before.len() > maximum as u64 {
        return Err(ImportError::UnsafePath);
    }
    let mut bytes = Vec::new();
    file.try_clone()
        .map_err(|_| ImportError::Unavailable)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ImportError::Unavailable)?;
    let after = file.metadata().map_err(|_| ImportError::Unavailable)?;
    if bytes.len() > maximum
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || after.nlink() != 1
    {
        return Err(ImportError::Stale);
    }
    let reopened = directory
        .open_with(name, &options)
        .map_err(|_| ImportError::Stale)?;
    let confirmed = reopened.metadata().map_err(|_| ImportError::Stale)?;
    if after.dev() != confirmed.dev() || after.ino() != confirmed.ino() {
        return Err(ImportError::Stale);
    }
    Ok(Some(bytes))
}

/// A fixed source directory. No environment expansion, traversal, or ambient discovery.
#[derive(Clone)]
pub struct ImportSource {
    anchor: Anchor,
}

impl std::fmt::Debug for ImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImportSource(<private>)")
    }
}

/// Frozen private bytes and identity for one allowlisted source file.
#[derive(Clone)]
pub struct ImportSourceFile {
    source: ImportSource,
    relative: PathBuf,
    bytes: Vec<u8>,
    identity: (u64, u64),
}

impl std::fmt::Debug for ImportSourceFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImportSourceFile(<private>)")
    }
}

impl ImportSource {
    /// Open exactly the caller-selected existing directory without creating it.
    pub fn open(root: &Path) -> Result<Self, ImportError> {
        Ok(Self {
            anchor: Anchor::bind(root, false)?,
        })
    }

    /// Canonical path for trusted scope verification, never for generic previews.
    pub fn canonical_root(&self) -> PathBuf {
        self.anchor.path()
    }

    /// Read one normal relative path, returning absence without widening discovery.
    pub fn read(&self, relative: &Path) -> Result<Option<ImportSourceFile>, ImportError> {
        validate_relative(relative)?;
        let Some(directory) = self
            .anchor
            .open(relative.parent().unwrap_or_else(|| Path::new("")), false)?
        else {
            return Ok(None);
        };
        let name = Path::new(relative.file_name().ok_or(ImportError::UnsafePath)?);
        let Some(bytes) = read_optional(&directory, name, MAX_FILE_BYTES)? else {
            return Ok(None);
        };
        let meta = directory
            .symlink_metadata(name)
            .map_err(|_| ImportError::Stale)?;
        Ok(Some(ImportSourceFile {
            source: self.clone(),
            relative: relative.to_owned(),
            bytes,
            identity: (meta.dev(), meta.ino()),
        }))
    }

    /// List one allowlisted directory. Entry names are private input, not safe UI text.
    pub fn entries(&self, relative: &Path) -> Result<Vec<PathBuf>, ImportError> {
        if !relative.as_os_str().is_empty() {
            validate_relative(relative)?;
        }
        let Some(directory) = self.anchor.open(relative, false)? else {
            return Ok(Vec::new());
        };
        let mut names = Vec::new();
        for entry in directory.entries().map_err(|_| ImportError::Unavailable)? {
            if names.len() >= super::MAX_RESOURCES {
                return Err(ImportError::Limit);
            }
            names.push(relative.join(entry.map_err(|_| ImportError::Unavailable)?.file_name()));
        }
        names.sort();
        Ok(names)
    }
}

impl ImportSourceFile {
    /// Opaque private fingerprint for host CAS checks. Never render or log it.
    pub fn fingerprint_for_host(&self) -> String {
        format!(
            "{}:{}:{}",
            self.identity.0,
            self.identity.1,
            digest(&self.bytes)
        )
    }
    /// Private parser input. Never include this value in errors, logs, or previews.
    pub fn text_for_parser(&self) -> Result<&str, ImportError> {
        std::str::from_utf8(&self.bytes).map_err(|_| ImportError::InvalidDocument)
    }
    /// Byte count, suitable for bounded inventory accounting.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    /// Whether the frozen file is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    /// Recheck the exact original source bytes and file identity.
    pub fn recheck(&self) -> Result<(), ImportError> {
        let current = self
            .source
            .read(&self.relative)?
            .ok_or(ImportError::Stale)?;
        if self.identity != current.identity || self.bytes != current.bytes {
            return Err(ImportError::Stale);
        }
        Ok(())
    }
}

fn validate_relative(path: &Path) -> Result<(), ImportError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ImportError::UnsafePath);
    }
    Ok(())
}

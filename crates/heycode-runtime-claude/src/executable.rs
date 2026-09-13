//! Canonical executable binding and replacement detection.

use std::path::{Path, PathBuf};

use heycode_runtime::{RuntimeError, RuntimeErrorCode};

pub(crate) struct ResolvedExecutable {
    path: PathBuf,
    identity: ExecutableIdentity,
}

impl ResolvedExecutable {
    pub(crate) fn resolve(path: PathBuf) -> Result<Self, RuntimeError> {
        let path = std::fs::canonicalize(path).map_err(|_| unavailable_error())?;
        let metadata = std::fs::metadata(&path).map_err(|_| unavailable_error())?;
        if !metadata.is_file() {
            return Err(unavailable_error());
        }
        Ok(Self {
            path,
            identity: ExecutableIdentity::from_metadata(&metadata),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn verify(&self) -> Result<(), RuntimeError> {
        let metadata = std::fs::metadata(&self.path).map_err(|_| changed_error())?;
        if metadata.is_file() && ExecutableIdentity::from_metadata(&metadata) == self.identity {
            Ok(())
        } else {
            Err(changed_error())
        }
    }
}

impl std::fmt::Debug for ResolvedExecutable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedExecutable")
            .field("path", &"<redacted>")
            .field("identity", &self.identity)
            .finish()
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(unix)]
impl ExecutableIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;

        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableIdentity {
    length: u64,
    created: u64,
    modified: u64,
    attributes: u32,
}

#[cfg(windows)]
impl ExecutableIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::windows::fs::MetadataExt as _;

        Self {
            length: metadata.file_size(),
            created: metadata.creation_time(),
            modified: metadata.last_write_time(),
            attributes: metadata.file_attributes(),
        }
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableIdentity {
    length: u64,
    modified: Option<std::time::SystemTime>,
    readonly: bool,
}

#[cfg(not(any(unix, windows)))]
impl ExecutableIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            readonly: metadata.permissions().readonly(),
        }
    }
}

fn safe_error(code: RuntimeErrorCode, message: &'static str) -> RuntimeError {
    match RuntimeError::try_new(code, message) {
        Ok(error) => error,
        Err(_) => RuntimeError::internal("invalid static Claude executable error"),
    }
}

fn unavailable_error() -> RuntimeError {
    safe_error(
        RuntimeErrorCode::Unavailable,
        "Claude Code executable is unavailable",
    )
}

fn changed_error() -> RuntimeError {
    safe_error(
        RuntimeErrorCode::Unavailable,
        "Claude Code executable changed during the runtime operation",
    )
}

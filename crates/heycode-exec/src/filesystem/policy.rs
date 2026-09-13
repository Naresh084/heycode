//! Provider-neutral filesystem root authority.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use super::{FileSystemError, FileSystemErrorCode};

const MAX_ROOTS: usize = 64;

/// Access granted by one explicit filesystem root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileSystemRootAccess {
    /// Reads, metadata, and searches are allowed; mutations are denied.
    ReadOnly,
    /// Reads and mutations are allowed.
    ReadWrite,
}

impl FileSystemRootAccess {
    /// Whether this grant permits mutation.
    #[must_use]
    pub const fn permits_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// One explicit absolute logical root and its grant.
#[derive(Clone, PartialEq, Eq)]
pub struct FileSystemRoot {
    path: PathBuf,
    access: FileSystemRootAccess,
}

impl FileSystemRoot {
    /// Validate one provider-neutral root declaration.
    ///
    /// # Errors
    /// The path must be absolute, NUL-free, and contain no parent traversal.
    /// Existence and canonical identity are resolved by the Provider.
    pub fn new(
        path: impl Into<PathBuf>,
        access: FileSystemRootAccess,
    ) -> Result<Self, FileSystemError> {
        let path = path.into();
        if !path.is_absolute()
            || path.as_os_str().is_empty()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            || path.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self {
            path: remove_current_components(&path),
            access,
        })
    }

    /// Absolute logical root declared by the policy owner.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Access grant for this root.
    #[must_use]
    pub const fn access(&self) -> FileSystemRootAccess {
        self.access
    }
}

impl std::fmt::Debug for FileSystemRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileSystemRoot")
            .field("path", &"<redacted>")
            .field("access", &self.access)
            .finish()
    }
}

/// Complete ordered filesystem authority generation.
#[derive(Clone, PartialEq, Eq)]
pub struct FileSystemPolicy {
    roots: Vec<FileSystemRoot>,
}

impl FileSystemPolicy {
    /// Validate one complete root generation.
    ///
    /// # Errors
    /// A policy must declare one to 64 unique logical roots. Provider-level
    /// canonical aliases are rejected when the local Provider opens them.
    pub fn new(roots: impl IntoIterator<Item = FileSystemRoot>) -> Result<Self, FileSystemError> {
        let roots: Vec<FileSystemRoot> = roots.into_iter().collect();
        let unique: BTreeSet<&Path> = roots.iter().map(FileSystemRoot::path).collect();
        if roots.is_empty() || roots.len() > MAX_ROOTS || unique.len() != roots.len() {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self { roots })
    }

    /// Derive file-tool authority from the effective process sandbox root.
    ///
    /// Model-facing file tools remain workspace-rooted even when process
    /// sandbox mode is full access. Read-only process policy also removes file
    /// mutation authority; workspace/full modes grant workspace mutation.
    ///
    /// # Errors
    /// The sandbox workspace root must satisfy the filesystem root contract.
    pub fn from_sandbox(policy: &crate::SandboxPolicy) -> Result<Self, FileSystemError> {
        let access = if matches!(policy.mode, crate::SandboxMode::ReadOnly) {
            FileSystemRootAccess::ReadOnly
        } else {
            FileSystemRootAccess::ReadWrite
        };
        Self::new([FileSystemRoot::new(&policy.workspace_root, access)?])
    }

    /// Ordered root declarations.
    #[must_use]
    pub fn roots(&self) -> &[FileSystemRoot] {
        &self.roots
    }
}

impl std::fmt::Debug for FileSystemPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileSystemPolicy")
            .field("root_count", &self.roots.len())
            .field(
                "grants",
                &self
                    .roots
                    .iter()
                    .map(FileSystemRoot::access)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

pub(crate) fn remove_current_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if !matches!(component, Component::CurDir) {
            normalized.push(component.as_os_str());
        }
    }
    normalized
}

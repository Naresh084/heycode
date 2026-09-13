//! Bounded, private, atomically replaced session workspace journal.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use heycode_exec::{FileSystemPolicy, FileSystemRoot, FileSystemRootAccess};
use serde::{Deserialize, Serialize};

use super::{Result, WorkspaceTransitionError};

const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// A canonical directory identity. Mutable timestamps deliberately do not count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created: u128,
}

impl DirectoryIdentity {
    pub(super) fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            WorkspaceTransitionError::new(format!(
                "Directory is unavailable: {} ({error})",
                path.display()
            ))
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(WorkspaceTransitionError::new(format!(
                "Directory must have a stable canonical identity: {}",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let created = metadata
                .created()
                .map_err(|_| {
                    WorkspaceTransitionError::new(
                        "Directory identity is unavailable on this filesystem",
                    )
                })?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| WorkspaceTransitionError::new("Invalid directory creation time"))?
                .as_nanos();
            Ok(Self { created })
        }
    }

    fn validate(&self, path: &Path) -> Result<()> {
        if !path.is_absolute() || canonical_directory(path)? != path || &Self::read(path)? != self {
            return Err(WorkspaceTransitionError::new(format!(
                "Saved directory identity changed; workspace recovery is required: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

/// An explicitly granted file-tool root. This does not imply project trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRoot {
    /// Canonical root path.
    pub path: PathBuf,
    /// Whether file mutations are denied.
    pub read_only: bool,
    identity: DirectoryIdentity,
}
impl WorkspaceRoot {
    pub(super) fn new(path: PathBuf, read_only: bool) -> Result<Self> {
        let identity = DirectoryIdentity::read(&path)?;
        Ok(Self {
            path,
            read_only,
            identity,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceReturnScope {
    pub cwd: PathBuf,
    pub cwd_identity: DirectoryIdentity,
    pub roots: Vec<WorkspaceRoot>,
}

/// A host-created worktree and the exact scope to restore on exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWorktree {
    /// Opaque managed lease id.
    pub id: String,
    /// Exact initial Git commit.
    pub base: String,
    /// Canonical checkout path.
    pub path: PathBuf,
    /// Owning Git manager journal directory.
    pub manager_root: PathBuf,
    pub(super) previous: WorkspaceReturnScope,
}

/// Uncommitted setup that requires acknowledgement; authority remains unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecovery {
    /// Inspect this attempt's Git journal and any checkout before acknowledgement.
    pub manager_root: PathBuf,
    /// Human-facing explanation.
    pub detail: String,
}

/// Durable current-session workspace state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
    /// Increases only when the effective scope/recovery acknowledgement commits.
    pub revision: u64,
    /// Actual default cwd for file tools, shell launches and agent requests.
    pub cwd: PathBuf,
    /// Exact current file authority.
    pub roots: Vec<WorkspaceRoot>,
    /// Active managed worktree, if any.
    pub worktree: Option<WorkspaceWorktree>,
    /// Interrupted setup; no new authority was published.
    pub pending_recovery: Option<WorkspaceRecovery>,
    /// Prior checkout/attempt locations deliberately kept on disk.
    pub retained_worktrees: Vec<PathBuf>,
    pub(super) cwd_identity: DirectoryIdentity,
}

impl WorkspaceSnapshot {
    pub(super) fn initial(cwd: PathBuf, root: PathBuf, read_only: bool) -> Result<Self> {
        Ok(Self {
            cwd_identity: DirectoryIdentity::read(&cwd)?,
            cwd,
            revision: 0,
            roots: vec![WorkspaceRoot::new(root, read_only)?],
            worktree: None,
            pending_recovery: None,
            retained_worktrees: Vec::new(),
        })
    }

    pub(super) fn return_scope(&self) -> WorkspaceReturnScope {
        WorkspaceReturnScope {
            cwd: self.cwd.clone(),
            cwd_identity: self.cwd_identity.clone(),
            roots: self.roots.clone(),
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        validate_scope(&self.cwd, &self.cwd_identity, &self.roots)?;
        if self.retained_worktrees.len() > 4096
            || self
                .retained_worktrees
                .iter()
                .any(|path| !path.is_absolute())
        {
            return Err(WorkspaceTransitionError::new(
                "Workspace retention journal is invalid or full",
            ));
        }
        if let Some(worktree) = &self.worktree {
            if worktree.id.len() != 36
                || !worktree
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
                || crate::GitCommitId::new(&worktree.base).is_err()
                || !worktree.manager_root.is_absolute()
                || worktree.path.parent() != Some(worktree.manager_root.as_path())
                || worktree.path.file_name().and_then(|name| name.to_str())
                    != Some(worktree.id.as_str())
                || self.roots.len() != 1
                || self.roots[0].path != worktree.path
                || !self.cwd.starts_with(&worktree.path)
            {
                return Err(WorkspaceTransitionError::new(
                    "Saved managed worktree scope is invalid",
                ));
            }
            // A removed return directory must not prevent opening the *current*
            // worktree. Exit revalidates the saved return scope before commit.
            if worktree.previous.roots.is_empty()
                || worktree.previous.roots.len() > 64
                || !worktree.previous.cwd.is_absolute()
            {
                return Err(WorkspaceTransitionError::new(
                    "Saved worktree return scope is invalid",
                ));
            }
        }
        if let Some(pending) = &self.pending_recovery
            && (!pending.manager_root.is_absolute()
                || pending.detail.len() > 4096
                || self.worktree.is_some())
        {
            return Err(WorkspaceTransitionError::new(
                "Workspace recovery journal is invalid",
            ));
        }
        Ok(())
    }

    pub(super) fn filesystem_policy(&self) -> Result<FileSystemPolicy> {
        FileSystemPolicy::new(
            self.roots
                .iter()
                .map(|root| {
                    FileSystemRoot::new(
                        root.path.clone(),
                        if root.read_only {
                            FileSystemRootAccess::ReadOnly
                        } else {
                            FileSystemRootAccess::ReadWrite
                        },
                    )
                })
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| WorkspaceTransitionError::new(error.to_string()))?,
        )
        .map_err(|error| WorkspaceTransitionError::new(error.to_string()))
    }
}

fn validate_scope(cwd: &Path, identity: &DirectoryIdentity, roots: &[WorkspaceRoot]) -> Result<()> {
    if roots.is_empty() || roots.len() > 64 || !roots.iter().any(|root| cwd.starts_with(&root.path))
    {
        return Err(WorkspaceTransitionError::new(
            "Saved cwd must be inside a declared workspace root",
        ));
    }
    identity.validate(cwd)?;
    let mut seen = std::collections::BTreeSet::new();
    for root in roots {
        root.identity.validate(&root.path)?;
        if !seen.insert(root.path.clone()) {
            return Err(WorkspaceTransitionError::new(
                "Saved workspace contains duplicate roots",
            ));
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceJournal {
    pub version: u8,
    pub original_root: PathBuf,
    pub sandbox_mode: String,
    pub snapshot: WorkspaceSnapshot,
}
impl WorkspaceJournal {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(WorkspaceTransitionError::new(format!(
                    "Workspace journal cannot be read: {error}"
                )));
            }
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_STATE_BYTES
        {
            return Err(WorkspaceTransitionError::new(
                "Workspace journal is oversized or has an unsafe file type",
            ));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|error| {
                WorkspaceTransitionError::new(format!("Workspace journal cannot be read: {error}"))
            })?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(WorkspaceTransitionError::new(
                "Workspace journal exceeds 1 MiB",
            ));
        }
        let journal: Self = serde_json::from_slice(&bytes).map_err(|_| {
            WorkspaceTransitionError::new(
                "Workspace journal is corrupt; preserve it for explicit recovery",
            )
        })?;
        if journal.version != 1 {
            return Err(WorkspaceTransitionError::new(
                "Unsupported workspace journal version; use a compatible heycode version",
            ));
        }
        journal.snapshot.validate()?;
        Ok(Some(journal))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(WorkspaceTransitionError::new(
                "Workspace journal has an unsafe file type",
            ));
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| WorkspaceTransitionError::new("Workspace state could not be encoded"))?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(WorkspaceTransitionError::new(
                "Workspace journal exceeds 1 MiB",
            ));
        }
        let mut options = atomic_write_file::AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(false).mode(0o600);
        }
        let mut file = options.open(path).map_err(|error| {
            WorkspaceTransitionError::new(format!("Workspace journal cannot be staged: {error}"))
        })?;
        file.write_all(&bytes)
            .and_then(|()| file.commit())
            .map_err(|error| {
                WorkspaceTransitionError::new(format!("Workspace journal commit failed: {error}"))
            })?;
        Ok(())
    }
}

pub(super) fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).map_err(|error| {
        WorkspaceTransitionError::new(format!(
            "Directory is unavailable: {} ({error})",
            path.display()
        ))
    })?;
    DirectoryIdentity::read(&path)?;
    Ok(path)
}

pub(super) fn resolve_directory(cwd: &Path, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(WorkspaceTransitionError::new(
            "A nonempty directory path is required",
        ));
    }
    canonical_directory(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    })
}

pub(super) fn validate_storage(
    state_path: &Path,
    worktrees_root: &Path,
    original_root: &Path,
) -> Result<()> {
    if !state_path.is_absolute()
        || !worktrees_root.is_absolute()
        || state_path.file_name().is_none()
        || worktrees_root.file_name().is_none()
    {
        return Err(WorkspaceTransitionError::new(
            "Workspace journal and worktree storage must have explicit absolute paths",
        ));
    }
    let parent = state_path
        .parent()
        .ok_or_else(|| WorkspaceTransitionError::new("Workspace journal parent missing"))?;
    if canonical_directory(parent)? != parent {
        return Err(WorkspaceTransitionError::new(
            "Workspace journal parent must be canonical",
        ));
    }
    let worktree_parent = worktrees_root
        .parent()
        .ok_or_else(|| WorkspaceTransitionError::new("Worktree storage parent missing"))?;
    if canonical_directory(worktree_parent)? != worktree_parent
        || worktrees_root.starts_with(original_root)
        || original_root.starts_with(worktrees_root)
    {
        return Err(WorkspaceTransitionError::new(
            "Worktree storage must be canonical and disjoint from the source workspace",
        ));
    }
    if worktrees_root.exists() && canonical_directory(worktrees_root)? != worktrees_root {
        return Err(WorkspaceTransitionError::new(
            "Worktree storage must not be a symlink",
        ));
    }
    Ok(())
}

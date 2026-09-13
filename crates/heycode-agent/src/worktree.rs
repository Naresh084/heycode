//! O06 exact-base Git worktree lifecycle over the composed subprocess owner.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_exec::{OutputOverflowPolicy, ProcessSpec, SubprocessService};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const JOURNAL_FILE: &str = "worktrees.json";
const JOURNAL_VERSION: u8 = 1;
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 1024 * 1024;

/// Exact lower-case Git commit object id (SHA-1 or SHA-256 repository form).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GitCommitId(String);

impl GitCommitId {
    /// Validate an exact full Git object id.
    ///
    /// # Errors
    /// Abbreviations, upper-case or non-hex input is refused.
    pub fn new(value: impl Into<String>) -> Result<Self, WorktreeError> {
        let value = value.into();
        let valid = matches!(value.len(), 40 | 64)
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidBase,
                "worktree base must be an exact Git commit id",
            ));
        }
        Ok(Self(value))
    }

    /// Exact object id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GitCommitId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Opaque identity of one managed worktree lease.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreeId(String);

impl WorktreeId {
    fn generate() -> Self {
        Self(heycode_core::SessionId::generate().to_string())
    }

    fn validate(value: String) -> Result<Self, WorktreeError> {
        if value.len() == 36
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            Ok(Self(value))
        } else {
            Err(WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal contains an invalid identity",
            ))
        }
    }

    /// Exact opaque text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorktreeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Cleanup policy applied when a lease reports failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeRetention {
    /// Remove clean checkouts after every terminal outcome; preserve results.
    RemoveAlways,
    /// Preserve an explicitly failed checkout until an explicit cleanup.
    RetainOnFailure,
}

/// Terminal result reported by a lease owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeOutcome {
    /// Child work completed successfully.
    Success,
    /// Child work failed and retention policy applies.
    Failure,
    /// Caller/lifecycle cancellation.
    Cancelled,
}

/// Stable worktree failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeErrorCode {
    /// Repository/storage configuration is unsafe.
    InvalidConfig,
    /// Configured Git executable is unavailable.
    GitUnavailable,
    /// Base is not an exact existing commit.
    InvalidBase,
    /// Owner journal is malformed or unsafe.
    StateCorrupt,
    /// Git create/apply/status/cleanup failed.
    OperationFailed,
    /// Caller or owner lifecycle cancelled.
    Cancelled,
    /// Worktree id is not owned by this manager.
    Unknown,
}

/// Body-free managed-worktree failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorktreeError {
    code: WorktreeErrorCode,
    message: &'static str,
}

impl WorktreeError {
    const fn new(code: WorktreeErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    /// Stable failure class.
    #[must_use]
    pub const fn code(&self) -> WorktreeErrorCode {
        self.code
    }
}

/// Recovery accounting for one manager pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorktreeRecovery {
    removed: u32,
    retained: u32,
}

impl WorktreeRecovery {
    /// Stale creating/ready rows removed.
    #[must_use]
    pub const fn removed(&self) -> u32 {
        self.removed
    }

    /// Explicit retained-failure rows left untouched.
    #[must_use]
    pub const fn retained(&self) -> u32 {
        self.retained
    }
}

/// Exact opaque status bytes used to detect reviewer mutation.
#[derive(Clone, PartialEq, Eq)]
pub struct WorktreeSnapshot(Vec<u8>);

impl std::fmt::Debug for WorktreeSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorktreeSnapshot")
            .field("bytes", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Creating,
    Ready,
    Retained,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalEntry {
    id: String,
    base: GitCommitId,
    state: JournalState,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WorktreeJournal {
    version: u8,
    entries: BTreeMap<String, JournalEntry>,
}

/// Effect-compatible manager for exact-base isolated Git worktrees.
pub struct GitWorktreeManager {
    subprocess: SubprocessService,
    git_program: PathBuf,
    repository: PathBuf,
    root: PathBuf,
    retention: WorktreeRetention,
    operation: tokio::sync::Mutex<()>,
    live: Mutex<BTreeSet<WorktreeId>>,
    shutdown: CancellationToken,
}

impl std::fmt::Debug for GitWorktreeManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitWorktreeManager")
            .field("retention", &self.retention)
            .field(
                "live",
                &self.live.lock().map(|live| live.len()).unwrap_or_default(),
            )
            .field("shutdown", &self.shutdown.is_cancelled())
            .finish()
    }
}

impl GitWorktreeManager {
    /// Bind one repository/storage root to the composed subprocess service.
    /// Construction resolves Git but creates no directory and starts no process.
    ///
    /// # Errors
    /// Repository/root must be absolute, disjoint, and the repository must be
    /// a canonical directory. Git must resolve to an exact executable.
    pub fn new(
        subprocess: SubprocessService,
        repository: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        retention: WorktreeRetention,
    ) -> Result<Self, WorktreeError> {
        let repository = repository.into();
        let root = root.into();
        if !repository.is_absolute() || !root.is_absolute() {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree repository and storage root must be absolute",
            ));
        }
        let repository = fs::canonicalize(repository).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree repository is unavailable",
            )
        })?;
        let root_name = root.file_name().ok_or_else(|| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage root is invalid",
            )
        })?;
        let root_parent = root.parent().ok_or_else(|| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage parent is invalid",
            )
        })?;
        let root_parent = fs::canonicalize(root_parent).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage parent is unavailable",
            )
        })?;
        let root = root_parent.join(root_name);
        if !fs::metadata(&repository)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree repository is not an available directory",
            ));
        }
        if root.to_str().is_none() {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage root is not valid Unicode",
            ));
        }
        if root.starts_with(&repository) {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage root is inside the repository",
            ));
        }
        if repository.starts_with(&root) {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree repository is inside the storage root",
            ));
        }
        let git_program = subprocess
            .resolve_program(std::ffi::OsStr::new("git"))
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::GitUnavailable,
                    "Git executable is unavailable",
                )
            })?;
        Ok(Self {
            subprocess,
            git_program,
            repository,
            root,
            retention,
            operation: tokio::sync::Mutex::new(()),
            live: Mutex::new(BTreeSet::new()),
            shutdown: CancellationToken::new(),
        })
    }

    /// Canonical source repository.
    #[must_use]
    pub fn repository(&self) -> &Path {
        &self.repository
    }

    /// Managed worktree storage root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Cancel future/in-flight owner-aware operations. Synchronous and idempotent.
    pub fn dispose(&self) {
        self.shutdown.cancel();
    }

    /// Recover stale creating/ready journal rows while preserving explicit
    /// retained failures.
    ///
    /// # Errors
    /// Unsafe journal state, cancellation or failed Git cleanup fails loud and
    /// leaves the owning row for a later retry.
    pub async fn recover(
        &self,
        cancellation: CancellationToken,
    ) -> Result<WorktreeRecovery, WorktreeError> {
        let _guard = self.operation.lock().await;
        self.check_cancelled(&cancellation)?;
        self.ensure_root()?;
        let mut journal = self.load_journal()?;
        let report = self.recover_locked(&mut journal, &cancellation).await?;
        self.save_journal(&journal)?;
        Ok(report)
    }

    /// Create one detached exact-base worktree and publish its lease only after
    /// Git checkout and the ready journal commit both succeed.
    ///
    /// # Errors
    /// Invalid base, cancellation, journal or Git failure.
    pub async fn create(
        self: &Arc<Self>,
        base: GitCommitId,
        cancellation: CancellationToken,
    ) -> Result<GitWorktreeLease, WorktreeError> {
        let _guard = self.operation.lock().await;
        self.check_cancelled(&cancellation)?;
        self.ensure_root()?;
        let mut journal = self.load_journal()?;
        let _recovery = self.recover_locked(&mut journal, &cancellation).await?;
        self.verify_base(&base, &cancellation).await?;
        let id = WorktreeId::generate();
        let path = self.path_for(&id);
        journal.entries.insert(
            id.as_str().to_owned(),
            JournalEntry {
                id: id.as_str().to_owned(),
                base: base.clone(),
                state: JournalState::Creating,
            },
        );
        self.save_journal(&journal)?;
        let args = vec![
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            path.as_os_str().to_os_string(),
            OsString::from(base.as_str()),
        ];
        let create = self.run_git(&self.repository, args, &cancellation).await;
        if create.is_err() {
            let _cleanup = self.cleanup_path(&path, &cancellation).await;
            if !path.exists() {
                journal.entries.remove(id.as_str());
                let _saved = self.save_journal(&journal);
            }
            return Err(WorktreeError::new(
                WorktreeErrorCode::OperationFailed,
                "Git worktree creation failed",
            ));
        }
        let Some(entry) = journal.entries.get_mut(id.as_str()) else {
            return Err(WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal lost the creating row",
            ));
        };
        entry.state = JournalState::Ready;
        self.save_journal(&journal)?;
        self.live
            .lock()
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::StateCorrupt,
                    "worktree live registry is unavailable",
                )
            })?
            .insert(id.clone());
        Ok(GitWorktreeLease {
            id,
            base,
            path,
            manager: self.clone(),
            active: true,
        })
    }

    /// Resolve repository HEAD to a full exact commit id.
    ///
    /// # Errors
    /// Detached/unborn/unavailable HEAD, malformed output or cancellation fails.
    pub async fn resolve_head(
        &self,
        cancellation: CancellationToken,
    ) -> Result<GitCommitId, WorktreeError> {
        let _guard = self.operation.lock().await;
        self.check_cancelled(&cancellation)?;
        self.ensure_root()?;
        let output = self
            .run_git(
                &self.repository,
                vec![
                    OsString::from("rev-parse"),
                    OsString::from("--verify"),
                    OsString::from("HEAD^{commit}"),
                ],
                &cancellation,
            )
            .await?;
        let value = std::str::from_utf8(&output)
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::OperationFailed,
                    "Git returned an invalid commit identity",
                )
            })?
            .trim();
        GitCommitId::new(value)
    }

    /// Capture the exact tracked Git patch from `base` to the current index and
    /// working tree. Untracked files are deliberately not inferred.
    ///
    /// # Errors
    /// Invalid base, oversized/non-UTF-8 output, Git failure or cancellation.
    pub async fn tracked_patch(
        &self,
        base: &GitCommitId,
        cancellation: CancellationToken,
    ) -> Result<String, WorktreeError> {
        let _guard = self.operation.lock().await;
        self.check_cancelled(&cancellation)?;
        self.ensure_root()?;
        self.verify_base(base, &cancellation).await?;
        let output = self
            .run_git(
                &self.repository,
                vec![
                    OsString::from("diff"),
                    OsString::from("--binary"),
                    OsString::from("--no-ext-diff"),
                    OsString::from("--no-textconv"),
                    OsString::from(base.as_str()),
                    OsString::from("--"),
                ],
                &cancellation,
            )
            .await?;
        String::from_utf8(output).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::OperationFailed,
                "Git patch is not valid UTF-8",
            )
        })
    }

    /// Remove one explicitly retained failure.
    ///
    /// # Errors
    /// Unknown/non-retained id, cancellation, unsafe state or cleanup failure.
    pub async fn cleanup_retained(
        &self,
        id: &WorktreeId,
        cancellation: CancellationToken,
    ) -> Result<(), WorktreeError> {
        let _guard = self.operation.lock().await;
        self.check_cancelled(&cancellation)?;
        self.ensure_root()?;
        let mut journal = self.load_journal()?;
        let Some(entry) = journal.entries.get(id.as_str()) else {
            return Err(WorktreeError::new(
                WorktreeErrorCode::Unknown,
                "retained worktree is unknown",
            ));
        };
        if entry.state != JournalState::Retained {
            return Err(WorktreeError::new(
                WorktreeErrorCode::Unknown,
                "worktree is not retained",
            ));
        }
        self.cleanup_path(&self.path_for(id), &cancellation).await?;
        journal.entries.remove(id.as_str());
        self.save_journal(&journal)
    }

    async fn finish(
        &self,
        id: &WorktreeId,
        outcome: WorktreeOutcome,
        cancellation: CancellationToken,
    ) -> Result<(), WorktreeError> {
        let _guard = self.operation.lock().await;
        self.ensure_root()?;
        let mut journal = self.load_journal()?;
        let Some(entry) = journal.entries.get_mut(id.as_str()) else {
            self.remove_live(id)?;
            return Err(WorktreeError::new(
                WorktreeErrorCode::Unknown,
                "worktree lease is unknown",
            ));
        };
        if self.has_results(id, &entry.base, &cancellation).await?
            || (outcome == WorktreeOutcome::Failure
                && self.retention == WorktreeRetention::RetainOnFailure)
        {
            entry.state = JournalState::Retained;
            self.save_journal(&journal)?;
            self.remove_live(id)?;
            return Ok(());
        }
        let cleanup = self.cleanup_path(&self.path_for(id), &cancellation).await;
        self.remove_live(id)?;
        cleanup?;
        journal.entries.remove(id.as_str());
        self.save_journal(&journal)
    }

    async fn apply_patch(
        &self,
        id: &WorktreeId,
        patch: &str,
        cancellation: CancellationToken,
    ) -> Result<(), WorktreeError> {
        if patch.is_empty() {
            return Ok(());
        }
        if patch.len() > MAX_GIT_OUTPUT_BYTES || patch.contains('\0') {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "review patch exceeds the managed bound",
            ));
        }
        let _guard = self.operation.lock().await;
        self.check_live(id)?;
        let patch_path = self.root.join(format!(".patch-{}", id.as_str()));
        write_private(&patch_path, patch.as_bytes())?;
        let result = self
            .run_git(
                &self.path_for(id),
                vec![
                    OsString::from("apply"),
                    OsString::from("--index"),
                    OsString::from("--binary"),
                    OsString::from("--whitespace=nowarn"),
                    patch_path.as_os_str().to_os_string(),
                ],
                &cancellation,
            )
            .await;
        let _removed = fs::remove_file(&patch_path);
        result.map(|_| ())
    }

    async fn status(
        &self,
        id: &WorktreeId,
        cancellation: CancellationToken,
    ) -> Result<WorktreeSnapshot, WorktreeError> {
        let _guard = self.operation.lock().await;
        self.check_live(id)?;
        self.run_git(
            &self.path_for(id),
            vec![
                OsString::from("status"),
                OsString::from("--porcelain=v2"),
                OsString::from("-z"),
                OsString::from("--untracked-files=all"),
            ],
            &cancellation,
        )
        .await
        .map(WorktreeSnapshot)
    }

    // A detached HEAD can contain commits as well as index/worktree edits.
    // Include ignored files: generated artifacts may be the child's only result.
    // Any inspection failure refuses cleanup, leaving the durable journal intact.
    async fn has_results(
        &self,
        id: &WorktreeId,
        base: &GitCommitId,
        cancellation: &CancellationToken,
    ) -> Result<bool, WorktreeError> {
        let path = self.path_for(id);
        let head = self
            .run_git(&path, vec!["rev-parse".into(), "HEAD".into()], cancellation)
            .await?;
        if String::from_utf8_lossy(&head).trim() != base.as_str() {
            return Ok(true);
        }
        let status = self
            .run_git(
                &path,
                vec![
                    "status".into(),
                    "--porcelain=v1".into(),
                    "-z".into(),
                    "--untracked-files=all".into(),
                    "--ignored".into(),
                ],
                cancellation,
            )
            .await?;
        Ok(!status.is_empty())
    }

    async fn recover_locked(
        &self,
        journal: &mut WorktreeJournal,
        cancellation: &CancellationToken,
    ) -> Result<WorktreeRecovery, WorktreeError> {
        let live = self
            .live
            .lock()
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::StateCorrupt,
                    "worktree live registry is unavailable",
                )
            })?
            .clone();
        let mut report = WorktreeRecovery::default();
        let entries = journal.entries.values().cloned().collect::<Vec<_>>();
        for entry in entries {
            let id = WorktreeId::validate(entry.id.clone())?;
            if live.contains(&id) {
                continue;
            }
            if entry.state == JournalState::Retained
                || (entry.state == JournalState::Ready
                    && self.has_results(&id, &entry.base, cancellation).await?)
            {
                if let Some(row) = journal.entries.get_mut(id.as_str()) {
                    row.state = JournalState::Retained;
                }
                report.retained = report.retained.saturating_add(1);
                continue;
            }
            self.cleanup_path(&self.path_for(&id), cancellation).await?;
            journal.entries.remove(id.as_str());
            report.removed = report.removed.saturating_add(1);
        }
        Ok(report)
    }

    async fn verify_base(
        &self,
        base: &GitCommitId,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeError> {
        self.run_git(
            &self.repository,
            vec![
                OsString::from("cat-file"),
                OsString::from("-e"),
                OsString::from(format!("{}^{{commit}}", base.as_str())),
            ],
            cancellation,
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            if error.code() == WorktreeErrorCode::Cancelled {
                error
            } else {
                WorktreeError::new(
                    WorktreeErrorCode::InvalidBase,
                    "worktree base is not an existing commit",
                )
            }
        })
    }

    async fn cleanup_path(
        &self,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeError> {
        self.check_managed_path(path)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(WorktreeError::new(
                    WorktreeErrorCode::StateCorrupt,
                    "managed worktree path has an unsafe type",
                ));
            }
            Ok(_) => {
                self.run_git(
                    &self.repository,
                    vec![
                        OsString::from("worktree"),
                        OsString::from("remove"),
                        OsString::from("--force"),
                        path.as_os_str().to_os_string(),
                    ],
                    cancellation,
                )
                .await?;
                if path.exists() {
                    return Err(WorktreeError::new(
                        WorktreeErrorCode::OperationFailed,
                        "Git did not remove the managed worktree",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(WorktreeError::new(
                    WorktreeErrorCode::OperationFailed,
                    "managed worktree could not be inspected",
                ));
            }
        }
        let _pruned = self
            .run_git(
                &self.repository,
                vec![
                    OsString::from("worktree"),
                    OsString::from("prune"),
                    OsString::from("--expire=now"),
                ],
                cancellation,
            )
            .await?;
        Ok(())
    }

    async fn run_git(
        &self,
        cwd: &Path,
        args: Vec<OsString>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, WorktreeError> {
        if cancellation.is_cancelled() {
            return Err(WorktreeError::new(
                WorktreeErrorCode::Cancelled,
                "worktree operation was cancelled",
            ));
        }
        let mut hardened_args = vec![
            OsString::from("-c"),
            OsString::from(format!(
                "core.hooksPath={}",
                self.root.join(".disabled-hooks").display()
            )),
            OsString::from("-c"),
            OsString::from("core.fsmonitor=false"),
            OsString::from("-c"),
            OsString::from("credential.helper="),
        ];
        hardened_args.extend(args);
        let spec = ProcessSpec::new(&self.git_program, cwd)
            .and_then(|spec| spec.with_args(hardened_args))
            .and_then(|spec| {
                spec.with_environment([
                    ("GIT_CONFIG_NOSYSTEM", "1"),
                    ("GIT_TERMINAL_PROMPT", "0"),
                    ("LC_ALL", "C"),
                ])
            })
            .and_then(|spec| spec.with_timeout(Some(Duration::from_secs(60))))
            .and_then(|spec| spec.with_output_limit_bytes(MAX_GIT_OUTPUT_BYTES))
            .map(|spec| spec.with_output_overflow_policy(OutputOverflowPolicy::Error))
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::InvalidConfig,
                    "Git process specification is invalid",
                )
            })?;
        let output = self
            .subprocess
            .output(spec, cancellation.clone())
            .await
            .map_err(|error| {
                if cancellation.is_cancelled() || self.shutdown.is_cancelled() {
                    WorktreeError::new(
                        WorktreeErrorCode::Cancelled,
                        "worktree operation was cancelled",
                    )
                } else {
                    let _ = error;
                    WorktreeError::new(WorktreeErrorCode::OperationFailed, "Git operation failed")
                }
            })?;
        if !output.exit().is_success() || output.truncated() {
            return Err(WorktreeError::new(
                WorktreeErrorCode::OperationFailed,
                "Git operation failed",
            ));
        }
        Ok(output.stdout().to_vec())
    }

    fn ensure_root(&self) -> Result<(), WorktreeError> {
        fs::create_dir_all(&self.root).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage root could not be created",
            )
        })?;
        let metadata = fs::symlink_metadata(&self.root).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage root could not be inspected",
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "worktree storage root has an unsafe type",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700)).map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::InvalidConfig,
                    "worktree storage permissions could not be applied",
                )
            })?;
        }
        let disabled_hooks = self.root.join(".disabled-hooks");
        fs::create_dir_all(&disabled_hooks).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "disabled hook directory could not be created",
            )
        })?;
        Ok(())
    }

    fn load_journal(&self) -> Result<WorktreeJournal, WorktreeError> {
        let path = self.root.join(JOURNAL_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(WorktreeJournal {
                    version: JOURNAL_VERSION,
                    entries: BTreeMap::new(),
                });
            }
            Err(_) => {
                return Err(WorktreeError::new(
                    WorktreeErrorCode::StateCorrupt,
                    "worktree journal could not be inspected",
                ));
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_JOURNAL_BYTES
        {
            return Err(WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal has an unsafe type or size",
            ));
        }
        let bytes = fs::read(path).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal could not be read",
            )
        })?;
        let journal: WorktreeJournal = serde_json::from_slice(&bytes).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal is malformed",
            )
        })?;
        if journal.version != JOURNAL_VERSION
            || journal.entries.iter().any(|(key, entry)| {
                key != &entry.id
                    || WorktreeId::validate(entry.id.clone()).is_err()
                    || GitCommitId::new(entry.base.as_str()).is_err()
            })
        {
            return Err(WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal version or identity is invalid",
            ));
        }
        Ok(journal)
    }

    fn save_journal(&self, journal: &WorktreeJournal) -> Result<(), WorktreeError> {
        let bytes = serde_json::to_vec(journal).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal could not be serialized",
            )
        })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_JOURNAL_BYTES {
            return Err(WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal exceeds its bound",
            ));
        }
        let temporary = self.root.join(format!(
            ".worktrees-{}.tmp",
            heycode_core::SessionId::generate().as_str()
        ));
        write_private(&temporary, &bytes)?;
        let target = self.root.join(JOURNAL_FILE);
        if let Err(_error) = fs::rename(&temporary, &target) {
            let _removed = fs::remove_file(&temporary);
            return Err(WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree journal could not be committed",
            ));
        }
        sync_directory(&self.root)
    }

    fn path_for(&self, id: &WorktreeId) -> PathBuf {
        self.root.join(id.as_str())
    }

    fn check_managed_path(&self, path: &Path) -> Result<(), WorktreeError> {
        if path.parent() == Some(self.root.as_path())
            && path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| WorktreeId::validate(name.to_owned()).is_ok())
        {
            Ok(())
        } else {
            Err(WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "worktree path is outside the managed generation",
            ))
        }
    }

    fn check_live(&self, id: &WorktreeId) -> Result<(), WorktreeError> {
        if self
            .live
            .lock()
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::StateCorrupt,
                    "worktree live registry is unavailable",
                )
            })?
            .contains(id)
        {
            Ok(())
        } else {
            Err(WorktreeError::new(
                WorktreeErrorCode::Unknown,
                "worktree lease is not live",
            ))
        }
    }

    fn remove_live(&self, id: &WorktreeId) -> Result<(), WorktreeError> {
        self.live
            .lock()
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::StateCorrupt,
                    "worktree live registry is unavailable",
                )
            })?
            .remove(id);
        Ok(())
    }

    fn check_cancelled(&self, cancellation: &CancellationToken) -> Result<(), WorktreeError> {
        if cancellation.is_cancelled() || self.shutdown.is_cancelled() {
            Err(WorktreeError::new(
                WorktreeErrorCode::Cancelled,
                "worktree operation was cancelled",
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }
}

/// One live managed checkout. Dropping without `finish` deliberately leaves
/// its ready journal row for the next recovery pass; no cleanup task detaches.
pub struct GitWorktreeLease {
    id: WorktreeId,
    base: GitCommitId,
    path: PathBuf,
    manager: Arc<GitWorktreeManager>,
    active: bool,
}

impl std::fmt::Debug for GitWorktreeLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitWorktreeLease")
            .field("id", &self.id)
            .field("base", &self.base)
            .field("active", &self.active)
            .finish()
    }
}

impl GitWorktreeLease {
    /// Preserve this checkout explicitly, including when it is clean. A session
    /// workspace may publish the path only after this durable retention commit.
    /// No child failure is fabricated and no asynchronous cleanup is detached.
    pub async fn retain(mut self) -> Result<(), WorktreeError> {
        let _guard = self.manager.operation.lock().await;
        self.manager.ensure_root()?;
        let mut journal = self.manager.load_journal()?;
        let entry = journal.entries.get_mut(self.id.as_str()).ok_or_else(|| {
            WorktreeError::new(WorktreeErrorCode::Unknown, "worktree lease is unknown")
        })?;
        if entry.state != JournalState::Ready {
            return Err(WorktreeError::new(
                WorktreeErrorCode::Unknown,
                "worktree is not ready for retention",
            ));
        }
        entry.state = JournalState::Retained;
        self.manager.save_journal(&journal)?;
        self.manager.remove_live(&self.id)?;
        self.active = false;
        Ok(())
    }

    /// Stable lease id.
    #[must_use]
    pub const fn id(&self) -> &WorktreeId {
        &self.id
    }

    /// Exact checked-out base.
    #[must_use]
    pub const fn base(&self) -> &GitCommitId {
        &self.base
    }

    /// Absolute isolated workspace path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Apply one exact bounded Git patch from a managed owner-only file.
    ///
    /// # Errors
    /// Invalid patch, cancellation or Git apply failure.
    pub async fn apply_patch(
        &self,
        patch: &str,
        cancellation: CancellationToken,
    ) -> Result<(), WorktreeError> {
        self.manager
            .apply_patch(&self.id, patch, cancellation)
            .await
    }

    /// Capture exact Git status bytes for later mutation comparison.
    ///
    /// # Errors
    /// Cancellation, unknown lease or Git status failure.
    pub async fn snapshot(
        &self,
        cancellation: CancellationToken,
    ) -> Result<WorktreeSnapshot, WorktreeError> {
        self.manager.status(&self.id, cancellation).await
    }

    /// Report terminal outcome and synchronously own Git cleanup/retention to
    /// completion before returning.
    ///
    /// # Errors
    /// Cancellation, journal or Git cleanup failure.
    pub async fn finish(
        mut self,
        outcome: WorktreeOutcome,
        cancellation: CancellationToken,
    ) -> Result<(), WorktreeError> {
        let result = self.manager.finish(&self.id, outcome, cancellation).await;
        self.active = false;
        result
    }
}

impl Drop for GitWorktreeLease {
    fn drop(&mut self) {
        if self.active {
            // The durable ready row is the recovery handoff. Async cleanup from
            // Drop would detach a task and race process shutdown.
            let _removed = self.manager.remove_live(&self.id);
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), WorktreeError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| {
        WorktreeError::new(
            WorktreeErrorCode::StateCorrupt,
            "managed worktree state could not be created",
        )
    })?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_data())
        .map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::StateCorrupt,
                "managed worktree state could not be written",
            )
        })
}

fn sync_directory(path: &Path) -> Result<(), WorktreeError> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| {
                WorktreeError::new(
                    WorktreeErrorCode::StateCorrupt,
                    "managed worktree directory could not be synchronized",
                )
            })?;
    }
    let _ = path;
    Ok(())
}

impl GitWorktreeManager {
    /// Resolve the Git top-level directory for the configured source cwd.
    pub async fn source_root(
        &self,
        cancellation: CancellationToken,
    ) -> Result<PathBuf, WorktreeError> {
        let output = self
            .run_git(
                &self.repository,
                vec!["rev-parse".into(), "--show-toplevel".into()],
                &cancellation,
            )
            .await?;
        let text = std::str::from_utf8(&output).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "repository root is not Unicode",
            )
        })?;
        let path = PathBuf::from(text.trim_end_matches(['\n', '\r']));
        fs::canonicalize(path).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::InvalidConfig,
                "repository root unavailable",
            )
        })
    }

    async fn untracked_manifest(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, WorktreeError> {
        self.run_git(
            &self.repository,
            vec![
                "ls-files".into(),
                "--others".into(),
                "--exclude-standard".into(),
                "--full-name".into(),
                "-z".into(),
            ],
            cancellation,
        )
        .await
    }

    /// Isolate the current tracked edits and untracked files without changing the source index.
    /// Refuses oversized snapshots and source changes during capture. Failure leaves its lease
    /// journaled for ordinary result-preserving recovery.
    pub async fn create_from_current(
        self: &Arc<Self>,
        cancellation: CancellationToken,
    ) -> Result<GitWorktreeLease, WorktreeError> {
        let base = self.resolve_head(cancellation.clone()).await?;
        let patch = self.tracked_patch(&base, cancellation.clone()).await?;
        let untracked = self.untracked_manifest(&cancellation).await?;
        let source = self.source_root(cancellation.clone()).await?;
        let captured = crate::worktree_snapshot::capture(&source, &untracked).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::OperationFailed,
                "untracked snapshot is unsafe, changed, or exceeds 64 MiB / 10000 files",
            )
        })?;
        self.check_cancelled(&cancellation)?;
        let lease = self.create(base.clone(), cancellation.clone()).await?;
        if !patch.is_empty() {
            lease.apply_patch(&patch, cancellation.clone()).await?;
        }
        crate::worktree_snapshot::write(lease.path(), &captured).map_err(|_| {
            WorktreeError::new(
                WorktreeErrorCode::OperationFailed,
                "untracked destination is unsafe or unavailable",
            )
        })?;
        if self.resolve_head(cancellation.clone()).await? != base
            || self.tracked_patch(&base, cancellation.clone()).await? != patch
            || self.untracked_manifest(&cancellation).await? != untracked
            || crate::worktree_snapshot::capture(&source, &untracked)
                .ok()
                .as_ref()
                != Some(&captured)
        {
            return Err(WorktreeError::new(
                WorktreeErrorCode::OperationFailed,
                "source changed during worktree capture; preserve the attempt and retry",
            ));
        }
        Ok(lease)
    }
}

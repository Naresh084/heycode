//! Session workspace authority: durable scope changes and stable file/shell handles.
//!
//! A transition needs both the composition's admission permit and an empty local
//! operation set. The permit must exclude jobs, terminals, delegated runtimes,
//! resumable children and other consumers whose scope the host cannot rebind.
//! No process-global `chdir`, checkout switch or implicit worktree deletion occurs.

mod adapters;
mod state;
mod tools;

pub use adapters::{workspace_filesystem_plugin, workspace_shell_plugin};
pub use state::{WorkspaceRecovery, WorkspaceRoot, WorkspaceSnapshot, WorkspaceWorktree};
pub use tools::{EnterWorktreeTool, ExitWorktreeTool};

/// Session workspace authority service key.
pub const SERVICE_WORKSPACE_TRANSITION: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("workspace-transition");
/// Cloneable Context handle to the one workspace authority owner.
pub struct WorkspaceTransitionHandle(pub std::sync::Arc<WorkspaceTransitionService>);

/// A frontend-owned operation that must settle before workspace authority moves.
pub struct WorkspaceActivityLease {
    _operation: WorkspaceOperation,
}

/// Host teardown fence that refuses new local operations until released.
pub struct WorkspaceQuiescenceLease {
    _fence: WorkspaceFence,
}

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_exec::{
    FileSystemService, SandboxMode, SandboxService, ShellService, SubprocessService,
};
use tokio_util::sync::CancellationToken;

use crate::{GitWorktreeManager, WorktreeRetention};
use state::{DirectoryIdentity, WorkspaceJournal};

/// Which existing admission owner is requesting the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTransitionOrigin {
    /// An idle human command; an active foreground turn is a conflict.
    HumanCommand,
    /// A foreground native tool barrier; only that exact turn may remain active.
    ModelTool,
}

/// Owned host admission fence. Dropping it releases admission.
pub trait WorkspaceTransitionPermit: Send + Sync {}
impl<T: Send + Sync> WorkspaceTransitionPermit for T {}

/// Host integration must acquire a real admission fence, then inspect all owners.
/// A racy `is_idle` check alone does not meet this contract.
#[async_trait]
pub trait WorkspaceTransitionGuard: Send + Sync {
    /// Refuse unsafe ownership and hold future admissions until the result is dropped.
    async fn acquire(
        &self,
        origin: WorkspaceTransitionOrigin,
    ) -> Result<Box<dyn WorkspaceTransitionPermit>>;
}

/// Actionable failure without silently granting a different scope.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct WorkspaceTransitionError {
    /// Human-facing explanation and recovery context.
    pub message: String,
}

impl WorkspaceTransitionError {
    /// Construct a bounded host refusal or validation failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

type Result<T> = std::result::Result<T, WorkspaceTransitionError>;

struct WorkspaceGeneration {
    snapshot: WorkspaceSnapshot,
    filesystem: FileSystemService,
    shell: ShellService,
}

struct WorkspaceLive {
    closed: bool,
    generation: Arc<WorkspaceGeneration>,
    active_operations: usize,
    transitioning: bool,
}

/// Read-only directory selection bound to one session revision and filesystem
/// identity. Only confirmation under the normal admission guard grants access.
pub struct DirectoryGrantCandidate {
    owner: PathBuf,
    revision: u64,
    path: PathBuf,
    identity: DirectoryIdentity,
}

impl DirectoryGrantCandidate {
    /// Canonical path to display before asking for the session-scoped grant.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One session's authority owner. Publish its forwarding services before consumers.
pub struct WorkspaceTransitionService {
    state_path: PathBuf,
    original_root: PathBuf,
    sandbox: SandboxService,
    resolver: ShellService,
    git_subprocess: SubprocessService,
    worktrees_root: PathBuf,
    live: Arc<Mutex<WorkspaceLive>>,
    guard: Mutex<Option<Arc<dyn WorkspaceTransitionGuard>>>,
    transition: tokio::sync::Mutex<()>,
}

impl WorkspaceTransitionService {
    /// Pin current authority for a host-owned auxiliary process (for example
    /// dictation) that does not run through the shell tool or job registry.
    pub fn pin_activity(&self) -> Result<WorkspaceActivityLease> {
        Ok(WorkspaceActivityLease {
            _operation: self.operation()?,
        })
    }

    /// Acquire only after the host has fenced turns, jobs, children and protocol
    /// admission. This completes a full-composition teardown boundary.
    pub fn pause_activity(&self) -> Result<WorkspaceQuiescenceLease> {
        let mut live = self.live.lock().map_err(|_| unavailable())?;
        if live.active_operations != 0 || live.transitioning || live.closed {
            return Err(WorkspaceTransitionError::new(
                "Workspace operations must settle before recomposition",
            ));
        }
        live.transitioning = true;
        Ok(WorkspaceQuiescenceLease {
            _fence: WorkspaceFence(self.live.clone()),
        })
    }

    /// Permanently close this generation's operation admission for teardown.
    pub fn dispose(&self) {
        if let Ok(mut live) = self.live.lock() {
            live.closed = true;
        }
    }

    /// The immutable trust/profile anchor used when this session was composed.
    pub fn original_root(&self) -> &Path {
        &self.original_root
    }
    /// Reopen the last atomic commit, revalidating canonical directory identities.
    /// Interrupted worktree setup is reported in `pending_recovery`; it never
    /// changes authority or triggers automatic Git cleanup. The parent of
    /// `state_path` and `worktrees_root` must be host-owned existing directories.
    pub fn open(
        state_path: PathBuf,
        initial_cwd: PathBuf,
        sandbox: SandboxService,
        shell: ShellService,
        git_subprocess: SubprocessService,
        worktrees_root: PathBuf,
    ) -> Result<Arc<Self>> {
        let original_root = state::canonical_directory(&sandbox.policy().workspace_root)?;
        let initial_cwd = state::canonical_directory(&initial_cwd)?;
        if !initial_cwd.starts_with(&original_root) {
            return Err(WorkspaceTransitionError::new(
                "Initial cwd is outside the composed sandbox root",
            ));
        }
        state::validate_storage(&state_path, &worktrees_root, &original_root)?;
        let journal = WorkspaceJournal::load(&state_path)?;
        let snapshot = match journal {
            Some(journal) => {
                if journal.original_root != original_root
                    || journal.sandbox_mode != sandbox.policy().mode.as_str()
                {
                    return Err(WorkspaceTransitionError::new(
                        "Saved workspace authority does not match this session's composition; reopen with its original root and sandbox mode",
                    ));
                }
                journal.snapshot
            }
            None => WorkspaceSnapshot::initial(
                initial_cwd,
                original_root.clone(),
                sandbox.policy().mode == SandboxMode::ReadOnly,
            )?,
        };
        let generation = Self::build_generation(&snapshot, &sandbox, &shell)?;
        Ok(Arc::new(Self {
            state_path,
            original_root,
            sandbox,
            resolver: shell,
            git_subprocess,
            worktrees_root,
            live: Arc::new(Mutex::new(WorkspaceLive {
                closed: false,
                generation: Arc::new(generation),
                active_operations: 0,
                transitioning: false,
            })),
            guard: Mutex::new(None),
            transition: tokio::sync::Mutex::new(()),
        }))
    }

    /// Install once, after the composition has all admission owners.
    pub fn install_guard(&self, guard: Arc<dyn WorkspaceTransitionGuard>) -> Result<()> {
        let mut slot = self.guard.lock().map_err(|_| unavailable())?;
        if slot.is_some() {
            return Err(WorkspaceTransitionError::new(
                "Workspace admission guard is already installed",
            ));
        }
        *slot = Some(guard);
        Ok(())
    }

    /// Last durable scope; paths here are operational authority, not UI labels.
    pub fn snapshot(&self) -> Result<WorkspaceSnapshot> {
        Ok(self
            .live
            .lock()
            .map_err(|_| unavailable())?
            .generation
            .snapshot
            .clone())
    }

    /// Exact-generation filesystem for a host that pins a child's authority.
    pub fn pinned_filesystem(&self) -> Result<FileSystemService> {
        Ok(self
            .live
            .lock()
            .map_err(|_| unavailable())?
            .generation
            .filesystem
            .clone())
    }

    /// Exact-generation shell for a host that pins a child's authority.
    pub fn pinned_shell(&self) -> Result<ShellService> {
        Ok(self
            .live
            .lock()
            .map_err(|_| unavailable())?
            .generation
            .shell
            .clone())
    }

    /// Stable filesystem handle; old tool objects immediately see committed roots.
    pub fn filesystem(self: &Arc<Self>) -> FileSystemService {
        FileSystemService::new(Arc::new(adapters::WorkspaceFilesystem(self.clone())))
    }

    /// Stable shell handle; preserves shell configuration and current confinement.
    pub fn shell(self: &Arc<Self>) -> ShellService {
        ShellService::new(Arc::new(adapters::WorkspaceShell(self.clone())))
    }

    /// Project trust is not widened by `/add-dir`. Profiles/plugins remain bound
    /// to the original composition. Only host-created worktrees inherit the
    /// source directory's trust for instruction files.
    pub fn instruction_sources(
        &self,
        user_home: Option<PathBuf>,
        initial_project_trusted: bool,
    ) -> Result<heycode_prompt::instructions::InstructionSources> {
        let snapshot = self.snapshot()?;
        let trusted = snapshot.cwd.starts_with(&self.original_root)
            || snapshot.worktree.as_ref().is_some_and(|worktree| {
                worktree.previous.cwd.starts_with(&self.original_root)
                    && snapshot.cwd.starts_with(&worktree.path)
            });
        Ok(heycode_prompt::instructions::InstructionSources {
            user_home,
            workspace: (initial_project_trusted && trusted).then_some(snapshot.cwd),
        })
    }

    /// Add a canonical file-tool root without implicitly changing cwd or trust.
    /// Single-root restrictive sandboxes explicitly refuse outside-root grants.
    pub async fn add_directory(
        &self,
        path: &Path,
        origin: WorkspaceTransitionOrigin,
        cancellation: CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        if origin != WorkspaceTransitionOrigin::HumanCommand {
            return Err(WorkspaceTransitionError::new(
                "Only a human /add-dir command can grant a new directory",
            ));
        }
        let (_serial, _permit, _fence) = self.begin(origin, &cancellation).await?;
        let snapshot = self.ready_snapshot()?;
        let path = state::resolve_directory(&snapshot.cwd, path)?;
        self.commit_directory_grant(snapshot, path, &cancellation)
    }

    /// Resolve a human-entered path for a confirmation dialog without writing
    /// the workspace journal or granting filesystem authority.
    pub fn prepare_directory_grant(&self, path: &Path) -> Result<DirectoryGrantCandidate> {
        let operation = self.operation()?;
        let snapshot = &operation.generation.snapshot;
        if snapshot.pending_recovery.is_some() {
            return Err(WorkspaceTransitionError::new(
                "Workspace recovery is pending; inspect it before granting a directory",
            ));
        }
        let path = state::resolve_directory(&snapshot.cwd, path)?;
        self.validate_directory_grant(snapshot, &path)?;
        Ok(DirectoryGrantCandidate {
            owner: self.state_path.clone(),
            revision: snapshot.revision,
            identity: DirectoryIdentity::read(&path)?,
            path,
        })
    }

    /// Confirm the exact displayed directory. Stale scope or directory identity
    /// requires a new selection; all ordinary native admission checks apply.
    pub async fn confirm_directory_grant(
        &self,
        candidate: DirectoryGrantCandidate,
        cancellation: CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        let (_serial, _permit, _fence) = self
            .begin(WorkspaceTransitionOrigin::HumanCommand, &cancellation)
            .await?;
        let snapshot = self.ready_snapshot()?;
        if candidate.owner != self.state_path || candidate.revision != snapshot.revision {
            return Err(WorkspaceTransitionError::new(
                "Workspace changed since this directory was selected; choose the directory again",
            ));
        }
        if state::canonical_directory(&candidate.path)? != candidate.path
            || DirectoryIdentity::read(&candidate.path)? != candidate.identity
        {
            return Err(WorkspaceTransitionError::new(
                "Directory changed since it was selected; choose the directory again",
            ));
        }
        self.commit_directory_grant(snapshot, candidate.path, &cancellation)
    }

    fn validate_directory_grant(&self, snapshot: &WorkspaceSnapshot, path: &Path) -> Result<()> {
        if snapshot
            .roots
            .iter()
            .any(|root| path.starts_with(&root.path))
        {
            return Ok(());
        }
        if snapshot.worktree.is_some() {
            return Err(WorkspaceTransitionError::new(
                "Exit the current worktree before adding an outside directory",
            ));
        }
        if self.sandbox.policy().mode != SandboxMode::Off {
            return Err(WorkspaceTransitionError::new(
                "The active process sandbox supports one root; it cannot safely grant an additional directory. Recompose with an explicit supported workspace scope",
            ));
        }
        if snapshot.roots.len() >= 64 {
            return Err(WorkspaceTransitionError::new(
                "A workspace supports at most 64 directory roots",
            ));
        }
        Ok(())
    }

    fn commit_directory_grant(
        &self,
        mut snapshot: WorkspaceSnapshot,
        path: PathBuf,
        cancellation: &CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        self.validate_directory_grant(&snapshot, &path)?;
        if snapshot
            .roots
            .iter()
            .any(|root| path.starts_with(&root.path))
        {
            return Ok(snapshot);
        }
        snapshot.roots.push(WorkspaceRoot::new(path, false)?);
        self.commit(snapshot, cancellation)
    }

    /// Change cwd only within roots the human already authorized.
    pub async fn change_directory(
        &self,
        path: &Path,
        origin: WorkspaceTransitionOrigin,
        cancellation: CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        if origin != WorkspaceTransitionOrigin::HumanCommand {
            return Err(WorkspaceTransitionError::new(
                "Use the human /cd command to change directories",
            ));
        }
        let (_serial, _permit, _fence) = self.begin(origin, &cancellation).await?;
        let mut snapshot = self.ready_snapshot()?;
        let path = state::resolve_directory(&snapshot.cwd, path)?;
        if !snapshot
            .roots
            .iter()
            .any(|root| path.starts_with(&root.path))
        {
            return Err(WorkspaceTransitionError::new(
                "Directory is outside the authorized roots; use /add-dir first",
            ));
        }
        if path == snapshot.cwd {
            return Ok(snapshot);
        }
        snapshot.cwd_identity = DirectoryIdentity::read(&path)?;
        snapshot.cwd = path;
        self.commit(snapshot, &cancellation)
    }

    /// Capture the current repository into a managed detached checkout. Changes
    /// and untracked files are copied by the Git manager; the original checkout
    /// and index are untouched. Cwd and filesystem roots switch together.
    pub async fn enter_worktree(
        &self,
        origin: WorkspaceTransitionOrigin,
        cancellation: CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        let (_serial, _permit, _fence) = self.begin(origin, &cancellation).await?;
        let mut snapshot = self.ready_snapshot()?;
        if snapshot.worktree.is_some() {
            return Err(WorkspaceTransitionError::new(
                "Already in a managed worktree; exit it before entering another",
            ));
        }
        if self.sandbox.policy().mode != SandboxMode::Off {
            return Err(WorkspaceTransitionError::new(
                "The active single-root process sandbox cannot create a worktree in disjoint storage. Worktree entry requires an explicitly configured full-access session",
            ));
        }
        let probe = GitWorktreeManager::new(
            self.git_subprocess.clone(),
            snapshot.cwd.clone(),
            self.worktrees_root.clone(),
            WorktreeRetention::RetainOnFailure,
        )
        .map_err(worktree_error)?;
        let source_root = probe
            .source_root(cancellation.clone())
            .await
            .map_err(worktree_error)?;
        if !snapshot
            .roots
            .iter()
            .any(|root| source_root.starts_with(&root.path))
        {
            return Err(WorkspaceTransitionError::new(
                "The Git repository root is outside authorized directories; explicitly add the complete repository before worktree entry",
            ));
        }
        state::validate_storage(&self.state_path, &self.worktrees_root, &source_root)?;
        // Each attempt has its own durable manager. No subsequent entry invokes
        // recovery against an earlier user's clean, retained checkout.
        let manager_root = self
            .worktrees_root
            .join(heycode_core::SessionId::generate().as_str());
        std::fs::create_dir_all(&self.worktrees_root).map_err(|error| {
            WorkspaceTransitionError::new(format!("Worktree storage unavailable: {error}"))
        })?;
        let manager = Arc::new(
            GitWorktreeManager::new(
                self.git_subprocess.clone(),
                source_root,
                manager_root.clone(),
                WorktreeRetention::RetainOnFailure,
            )
            .map_err(worktree_error)?,
        );
        snapshot.pending_recovery = Some(WorkspaceRecovery {
            manager_root: manager_root.clone(),
            detail: "Worktree setup did not reach a workspace commit. The previous scope remains active; inspect this directory and explicitly acknowledge recovery. No worktree was deleted.".into(),
        });
        self.persist_only(snapshot.clone())?;
        let lease = manager
            .create_from_current(cancellation.clone())
            .await
            .map_err(|error| {
                WorkspaceTransitionError::new(format!(
                    "{error}; workspace unchanged. Recovery inspection required at {}",
                    manager_root.display()
                ))
            })?;
        let path = lease.path().to_path_buf();
        let worktree = WorkspaceWorktree {
            id: lease.id().as_str().to_owned(),
            base: lease.base().as_str().to_owned(),
            path: path.clone(),
            manager_root,
            previous: snapshot.return_scope(),
        };
        // Retain even a clean checkout before publishing it as session scope.
        // This is separate from reporting a child failure or success.
        lease.retain().await.map_err(worktree_error)?;
        snapshot.cwd_identity = DirectoryIdentity::read(&path)?;
        snapshot.cwd = path.clone();
        snapshot.roots = vec![WorkspaceRoot::new(
            path,
            self.sandbox.policy().mode == SandboxMode::ReadOnly,
        )?];
        snapshot.worktree = Some(worktree);
        snapshot.pending_recovery = None;
        self.commit(snapshot, &cancellation)
    }

    /// Return to the saved authority. The worktree is always retained, including
    /// when clean; this method never merges, switches branches, or deletes files.
    pub async fn exit_worktree(
        &self,
        origin: WorkspaceTransitionOrigin,
        cancellation: CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        let (_serial, _permit, _fence) = self.begin(origin, &cancellation).await?;
        let mut snapshot = self.ready_snapshot()?;
        let worktree = snapshot.worktree.take().ok_or_else(|| {
            WorkspaceTransitionError::new("This session is not in a managed worktree")
        })?;
        snapshot.cwd = worktree.previous.cwd;
        snapshot.cwd_identity = worktree.previous.cwd_identity;
        snapshot.roots = worktree.previous.roots;
        snapshot.retained_worktrees.push(worktree.path);
        self.commit(snapshot, &cancellation)
    }

    /// Explicitly acknowledge interrupted setup and keep the last committed
    /// authority. All potential Git results remain on disk for human inspection.
    pub async fn recover_pending(
        &self,
        origin: WorkspaceTransitionOrigin,
        cancellation: CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        if origin != WorkspaceTransitionOrigin::HumanCommand {
            return Err(WorkspaceTransitionError::new(
                "Workspace recovery requires an explicit human command",
            ));
        }
        let (_serial, _permit, _fence) = self.begin(origin, &cancellation).await?;
        let mut snapshot = self.snapshot()?;
        let pending = snapshot
            .pending_recovery
            .take()
            .ok_or_else(|| WorkspaceTransitionError::new("No workspace recovery is pending"))?;
        snapshot.retained_worktrees.push(pending.manager_root);
        self.commit(snapshot, &cancellation)
    }

    fn ready_snapshot(&self) -> Result<WorkspaceSnapshot> {
        let snapshot = self.snapshot()?;
        if let Some(pending) = &snapshot.pending_recovery {
            return Err(WorkspaceTransitionError::new(format!(
                "Workspace recovery must be acknowledged before another transition; retained setup: {}",
                pending.manager_root.display()
            )));
        }
        snapshot.validate()?;
        Ok(snapshot)
    }

    async fn begin<'a>(
        &'a self,
        origin: WorkspaceTransitionOrigin,
        cancellation: &CancellationToken,
    ) -> Result<(
        tokio::sync::MutexGuard<'a, ()>,
        Box<dyn WorkspaceTransitionPermit>,
        WorkspaceFence,
    )> {
        check_cancelled(cancellation)?;
        let serial = self.transition.try_lock().map_err(|_| {
            WorkspaceTransitionError::new("Another workspace transition is already active")
        })?;
        let guard = self.guard.lock().map_err(|_| unavailable())?.clone().ok_or_else(|| WorkspaceTransitionError::new("Workspace transitions are unavailable until the composition installs an admission guard"))?;
        let permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(WorkspaceTransitionError::new("Workspace transition cancelled")),
            permit = guard.acquire(origin) => permit?,
        };
        let mut live = self.live.lock().map_err(|_| unavailable())?;
        if live.active_operations != 0 || live.transitioning || live.closed {
            return Err(WorkspaceTransitionError::new(
                "Filesystem or shell operations still own this workspace; wait for them to settle",
            ));
        }
        live.transitioning = true;
        Ok((serial, permit, WorkspaceFence(self.live.clone())))
    }

    fn build_generation(
        snapshot: &WorkspaceSnapshot,
        sandbox: &SandboxService,
        resolver: &ShellService,
    ) -> Result<WorkspaceGeneration> {
        snapshot.validate()?;
        let policy = snapshot.filesystem_policy()?;
        let sandbox_root = snapshot
            .roots
            .iter()
            .find(|root| snapshot.cwd.starts_with(&root.path))
            .ok_or_else(unavailable)?;
        let scoped = sandbox
            .for_workspace(sandbox_root.path.clone())
            .map_err(|error| WorkspaceTransitionError::new(error.to_string()))?;
        let subprocess = SubprocessService::local_with_sandbox(scoped);
        let resolver = ShellService::new(Arc::new(adapters::PinnedWorkspaceShell {
            resolver: resolver.clone(),
            cwd: snapshot.cwd.clone(),
            roots: snapshot.roots.clone(),
        }))
        .with_executor(subprocess);
        Ok(WorkspaceGeneration {
            snapshot: snapshot.clone(),
            filesystem: FileSystemService::local(policy)
                .map_err(|error| WorkspaceTransitionError::new(error.to_string()))?,
            shell: resolver,
        })
    }

    fn commit(
        &self,
        mut snapshot: WorkspaceSnapshot,
        cancellation: &CancellationToken,
    ) -> Result<WorkspaceSnapshot> {
        check_cancelled(cancellation)?;
        snapshot.revision = snapshot.revision.checked_add(1).ok_or_else(unavailable)?;
        let generation = Arc::new(Self::build_generation(
            &snapshot,
            &self.sandbox,
            &self.resolver,
        )?);
        let mut live = self.live.lock().map_err(|_| unavailable())?;
        // All fallible construction occurs before durable publication; after
        // atomic save there is no await/cancellation gap before the pointer swap.
        self.journal(snapshot.clone()).save(&self.state_path)?;
        live.generation = generation;
        Ok(snapshot)
    }

    fn persist_only(&self, snapshot: WorkspaceSnapshot) -> Result<()> {
        let mut live = self.live.lock().map_err(|_| unavailable())?;
        self.journal(snapshot.clone()).save(&self.state_path)?;
        live.generation = Arc::new(WorkspaceGeneration {
            snapshot,
            filesystem: live.generation.filesystem.clone(),
            shell: live.generation.shell.clone(),
        });
        Ok(())
    }

    fn journal(&self, snapshot: WorkspaceSnapshot) -> WorkspaceJournal {
        WorkspaceJournal {
            version: 1,
            original_root: self.original_root.clone(),
            sandbox_mode: self.sandbox.policy().mode.as_str().to_owned(),
            snapshot,
        }
    }

    fn operation(&self) -> Result<WorkspaceOperation> {
        let mut live = self.live.lock().map_err(|_| unavailable())?;
        if live.transitioning || live.closed {
            return Err(WorkspaceTransitionError::new(
                "Workspace transition is active; retry after it commits",
            ));
        }
        live.active_operations = live
            .active_operations
            .checked_add(1)
            .ok_or_else(unavailable)?;
        Ok(WorkspaceOperation {
            live: self.live.clone(),
            generation: live.generation.clone(),
        })
    }
}

struct WorkspaceOperation {
    live: Arc<Mutex<WorkspaceLive>>,
    generation: Arc<WorkspaceGeneration>,
}
impl Drop for WorkspaceOperation {
    fn drop(&mut self) {
        if let Ok(mut live) = self.live.lock() {
            live.active_operations = live.active_operations.saturating_sub(1);
        }
    }
}
struct WorkspaceFence(Arc<Mutex<WorkspaceLive>>);
impl Drop for WorkspaceFence {
    fn drop(&mut self) {
        if let Ok(mut live) = self.0.lock() {
            live.transitioning = false;
        }
    }
}
fn unavailable() -> WorkspaceTransitionError {
    WorkspaceTransitionError::new("Workspace authority state is unavailable")
}
fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(WorkspaceTransitionError::new(
            "Workspace transition cancelled",
        ))
    } else {
        Ok(())
    }
}
fn worktree_error(error: crate::WorktreeError) -> WorkspaceTransitionError {
    WorkspaceTransitionError::new(error.to_string())
}

#[cfg(test)]
mod tests;

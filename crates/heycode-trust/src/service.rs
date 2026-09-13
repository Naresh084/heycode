//! Revisioned workspace-trust service and startup/project gates.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use heycode_core::PluginScope;

use crate::store::TrustStore;
use crate::{
    ExplicitWorkspaceTrust, ProjectAccess, ProjectContentPolicy, ProjectInputKind, TrustFrontend,
    TrustPersistence, TrustStartupState, UntrustedProjectAccess, WorkspaceIdentity,
    WorkspaceTrustAction, WorkspaceTrustActionOutcome, WorkspaceTrustDecision,
    WorkspaceTrustDialogState, WorkspaceTrustError, WorkspaceTrustSnapshot,
};

const EXECUTABLE_PREREQUISITE: &str =
    "Trust this workspace before activating project executable contributions.";
const INSTRUCTION_PREREQUISITE: &str =
    "Project instructions are blocked by the selected untrusted-content policy.";
const SETTINGS_PREREQUISITE: &str =
    "Project settings are blocked by the selected untrusted-content policy.";
const UNKNOWN_PREREQUISITE: &str =
    "Choose a workspace trust mode before reading or activating project content.";

struct TrustState {
    decision: WorkspaceTrustDecision,
    persistence: TrustPersistence,
    revision: u64,
    store_generation: u64,
}

struct TrustInner {
    identity: WorkspaceIdentity,
    policy: ProjectContentPolicy,
    store: TrustStore,
    state: Mutex<TrustState>,
    operations: Mutex<()>,
    active: AtomicBool,
}

/// Durable, CAS-guarded effective trust decision for one canonical workspace.
#[derive(Clone)]
pub struct WorkspaceTrustService {
    inner: Arc<TrustInner>,
}

/// One typed dialog projection bound to the exact live trust service that
/// owns its CAS revision and persistence operation.
#[derive(Clone)]
pub struct WorkspaceTrustPrompt {
    service: WorkspaceTrustService,
    state: WorkspaceTrustDialogState,
}

impl WorkspaceTrustPrompt {
    /// Current typed dialog state.
    #[must_use]
    pub const fn state(&self) -> &WorkspaceTrustDialogState {
        &self.state
    }

    /// Apply one typed action against the exact dialog revision.
    ///
    /// # Errors
    /// Stale revision, persistence, lifecycle, or store failure leaves the
    /// previous effective state authoritative.
    pub fn apply(
        &self,
        action: WorkspaceTrustAction,
    ) -> Result<WorkspaceTrustActionOutcome, WorkspaceTrustError> {
        self.service
            .apply_dialog_action(action, self.state.revision())
    }

    /// Refresh from the authoritative service after a rejected/stale action.
    ///
    /// # Errors
    /// Service lifecycle/state failures.
    pub fn refresh(&mut self) -> Result<TrustStartupState, WorkspaceTrustError> {
        let startup = self
            .service
            .prepare_startup(TrustFrontend::Interactive, None)?;
        if let TrustStartupState::Prompt(state) = &startup {
            self.state = state.clone();
        }
        Ok(startup)
    }
}

impl std::fmt::Debug for WorkspaceTrustPrompt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceTrustPrompt")
            .field("state", &self.state)
            .field("service", &"<redacted>")
            .finish()
    }
}

impl WorkspaceTrustService {
    /// Build an in-memory service. Persistent calls remain durable only for
    /// this service instance; production uses [`Self::file`].
    ///
    /// # Errors
    /// Invalid workspace roots fail before state exists.
    pub fn memory(
        root: impl AsRef<Path>,
        policy: ProjectContentPolicy,
    ) -> Result<Self, WorkspaceTrustError> {
        Self::open(root, policy, TrustStore::memory())
    }

    /// Build from an owner-only schema-v1 trust store.
    ///
    /// Persistent trust is enabled only on Unix, where the implementation
    /// enforces owner/mode/link identity and a cross-process file lock through
    /// descriptor-relative operations. Other platforms fail closed with
    /// [`WorkspaceTrustError::UnsupportedSecurity`] until an audited native
    /// owner/ACL backend is installed; [`Self::memory`] remains portable.
    ///
    /// # Errors
    /// Invalid roots/store paths, unsafe store types, malformed versions, or
    /// I/O failures publish no service.
    pub fn file(
        root: impl AsRef<Path>,
        store_path: impl AsRef<Path>,
        policy: ProjectContentPolicy,
    ) -> Result<Self, WorkspaceTrustError> {
        Self::open(
            root,
            policy,
            TrustStore::file(store_path.as_ref().to_path_buf())?,
        )
    }

    fn open(
        root: impl AsRef<Path>,
        policy: ProjectContentPolicy,
        store: TrustStore,
    ) -> Result<Self, WorkspaceTrustError> {
        let identity = WorkspaceIdentity::discover(root)?;
        let loaded = store.load(&identity)?;
        let (decision, persistence) = loaded.decision.map_or(
            (WorkspaceTrustDecision::Unknown, TrustPersistence::None),
            |decision| (decision, TrustPersistence::Persistent),
        );
        Ok(Self {
            inner: Arc::new(TrustInner {
                identity,
                policy,
                store,
                state: Mutex::new(TrustState {
                    decision,
                    persistence,
                    revision: loaded.generation,
                    store_generation: loaded.generation,
                }),
                operations: Mutex::new(()),
                active: AtomicBool::new(true),
            }),
        })
    }

    /// Read one immutable effective snapshot.
    ///
    /// # Errors
    /// Shutdown or poisoned state fails loud.
    pub fn snapshot(&self) -> Result<Arc<WorkspaceTrustSnapshot>, WorkspaceTrustError> {
        self.ensure_active()?;
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| WorkspaceTrustError::ServiceUnavailable)?;
        Ok(self.snapshot_from(&state))
    }

    /// Commit a session-only trusted/restricted decision after revision CAS.
    ///
    /// # Errors
    /// Unknown, stale revision, shutdown, or revision exhaustion publishes no state.
    pub fn set_session(
        &self,
        decision: WorkspaceTrustDecision,
        expected_revision: u64,
    ) -> Result<Arc<WorkspaceTrustSnapshot>, WorkspaceTrustError> {
        if !decision.is_explicit() {
            return Err(WorkspaceTrustError::InvalidDecision);
        }
        let _operation = self.operation()?;
        let mut state = self.state_for_update(expected_revision)?;
        let revision = advance(state.revision)?;
        state.decision = decision;
        state.persistence = TrustPersistence::Session;
        state.revision = revision;
        Ok(self.snapshot_from(&state))
    }

    /// Durably commit a trusted/restricted decision, then publish it live.
    ///
    /// # Errors
    /// Unknown, stale live/store revision, unsafe store, I/O, shutdown, or
    /// revision exhaustion keeps the previous live snapshot.
    pub fn persist(
        &self,
        decision: WorkspaceTrustDecision,
        expected_revision: u64,
    ) -> Result<Arc<WorkspaceTrustSnapshot>, WorkspaceTrustError> {
        if !decision.is_explicit() {
            return Err(WorkspaceTrustError::InvalidDecision);
        }
        let _operation = self.operation()?;
        let mut state = self.state_for_update(expected_revision)?;
        let revision = advance(state.revision)?;
        let next_store = self.inner.store.commit(
            &self.inner.identity,
            Some(decision),
            state.store_generation,
        )?;
        state.decision = decision;
        state.persistence = TrustPersistence::Persistent;
        state.revision = revision;
        state.store_generation = next_store;
        Ok(self.snapshot_from(&state))
    }

    /// Durably remove this workspace record, then publish Unknown.
    ///
    /// # Errors
    /// Stale live/store revision, unsafe store, I/O, shutdown, or revision
    /// exhaustion keeps the previous live snapshot.
    pub fn reset(
        &self,
        expected_revision: u64,
    ) -> Result<Arc<WorkspaceTrustSnapshot>, WorkspaceTrustError> {
        let _operation = self.operation()?;
        let mut state = self.state_for_update(expected_revision)?;
        let revision = advance(state.revision)?;
        let next_store =
            self.inner
                .store
                .commit(&self.inner.identity, None, state.store_generation)?;
        state.decision = WorkspaceTrustDecision::Unknown;
        state.persistence = TrustPersistence::None;
        state.revision = revision;
        state.store_generation = next_store;
        Ok(self.snapshot_from(&state))
    }

    /// Evaluate one project-origin input under the effective decision and
    /// explicit non-executable policy.
    ///
    /// # Errors
    /// Shutdown or poisoned state fails loud.
    pub fn access(&self, kind: ProjectInputKind) -> Result<ProjectAccess, WorkspaceTrustError> {
        let snapshot = self.snapshot()?;
        Ok(access_for(snapshot.decision(), self.inner.policy, kind))
    }

    /// Gate project/local-project plugin activation; other trusted scopes pass.
    ///
    /// # Errors
    /// Shutdown or poisoned state fails loud.
    pub fn allows_plugin_scope(&self, scope: PluginScope) -> Result<bool, WorkspaceTrustError> {
        match scope {
            PluginScope::Project | PluginScope::LocalProject => {
                Ok(self.access(ProjectInputKind::Plugin)?.is_allowed())
            }
            PluginScope::BuiltIn
            | PluginScope::User
            | PluginScope::Session
            | PluginScope::Managed => {
                self.ensure_active()?;
                Ok(true)
            }
        }
    }

    /// Project one exact typed U01 dialog state.
    ///
    /// # Errors
    /// Shutdown or poisoned state fails loud.
    pub fn dialog_state(&self) -> Result<WorkspaceTrustDialogState, WorkspaceTrustError> {
        let snapshot = self.snapshot()?;
        Ok(WorkspaceTrustDialogState::new(
            snapshot.clone(),
            access_for(
                snapshot.decision(),
                self.inner.policy,
                ProjectInputKind::Instructions,
            ),
            access_for(
                snapshot.decision(),
                self.inner.policy,
                ProjectInputKind::Settings,
            ),
            access_for(
                snapshot.decision(),
                self.inner.policy,
                ProjectInputKind::Plugin,
            ),
        ))
    }

    /// Bind the current unknown-workspace dialog to this exact live service.
    ///
    /// # Errors
    /// A resolved decision cannot produce a prompt; lifecycle/state failures
    /// also fail loud.
    pub fn dialog_prompt(&self) -> Result<WorkspaceTrustPrompt, WorkspaceTrustError> {
        match self.prepare_startup(TrustFrontend::Interactive, None)? {
            TrustStartupState::Prompt(state) => Ok(WorkspaceTrustPrompt {
                service: self.clone(),
                state,
            }),
            TrustStartupState::Ready(_) => Err(WorkspaceTrustError::InvalidDecision),
        }
    }

    /// Apply startup prompting rules. Explicit flags are session-only.
    ///
    /// # Errors
    /// Unknown headless/ACP workspaces fail instead of prompting; mutation and
    /// lifecycle failures also fail loud.
    pub fn prepare_startup(
        &self,
        frontend: TrustFrontend,
        explicit: Option<ExplicitWorkspaceTrust>,
    ) -> Result<TrustStartupState, WorkspaceTrustError> {
        if let Some(explicit) = explicit {
            let snapshot = self.snapshot()?;
            return self
                .set_session(explicit.decision(), snapshot.revision())
                .map(TrustStartupState::Ready);
        }
        let snapshot = self.snapshot()?;
        if snapshot.decision() != WorkspaceTrustDecision::Unknown {
            return Ok(TrustStartupState::Ready(snapshot));
        }
        match frontend {
            TrustFrontend::Interactive => self.dialog_state().map(TrustStartupState::Prompt),
            TrustFrontend::Headless | TrustFrontend::Acp => {
                Err(WorkspaceTrustError::NonInteractiveTrustRequired { frontend })
            }
        }
    }

    /// Apply one typed U01 dialog action using the dialog's revision.
    ///
    /// # Errors
    /// Stale revision, persistence, lifecycle, or invalid store failures keep
    /// the previous effective decision.
    pub fn apply_dialog_action(
        &self,
        action: WorkspaceTrustAction,
        expected_revision: u64,
    ) -> Result<WorkspaceTrustActionOutcome, WorkspaceTrustError> {
        match action {
            WorkspaceTrustAction::TrustOnce => self
                .set_session(WorkspaceTrustDecision::Trusted, expected_revision)
                .map(WorkspaceTrustActionOutcome::Ready),
            WorkspaceTrustAction::TrustWorkspace => self
                .persist(WorkspaceTrustDecision::Trusted, expected_revision)
                .map(WorkspaceTrustActionOutcome::Ready),
            WorkspaceTrustAction::OpenRestricted => self
                .set_session(WorkspaceTrustDecision::Restricted, expected_revision)
                .map(WorkspaceTrustActionOutcome::Ready),
            WorkspaceTrustAction::Exit => {
                let snapshot = self.snapshot()?;
                if snapshot.revision() != expected_revision {
                    return Err(WorkspaceTrustError::Conflict {
                        expected: expected_revision,
                        actual: snapshot.revision(),
                    });
                }
                Ok(WorkspaceTrustActionOutcome::Exit)
            }
        }
    }

    pub(crate) fn shutdown(&self) {
        if let Ok(_operation) = self.inner.operations.lock() {
            self.inner.active.store(false, Ordering::Release);
        }
    }

    fn operation(&self) -> Result<std::sync::MutexGuard<'_, ()>, WorkspaceTrustError> {
        self.ensure_active()?;
        self.inner
            .operations
            .lock()
            .map_err(|_| WorkspaceTrustError::ServiceUnavailable)
    }

    fn state_for_update(
        &self,
        expected_revision: u64,
    ) -> Result<std::sync::MutexGuard<'_, TrustState>, WorkspaceTrustError> {
        self.ensure_active()?;
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| WorkspaceTrustError::ServiceUnavailable)?;
        if state.revision != expected_revision {
            return Err(WorkspaceTrustError::Conflict {
                expected: expected_revision,
                actual: state.revision,
            });
        }
        Ok(state)
    }

    fn snapshot_from(&self, state: &TrustState) -> Arc<WorkspaceTrustSnapshot> {
        Arc::new(WorkspaceTrustSnapshot::new(
            self.inner.identity.clone(),
            state.decision,
            state.persistence,
            state.revision,
        ))
    }

    fn ensure_active(&self) -> Result<(), WorkspaceTrustError> {
        if self.inner.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(WorkspaceTrustError::ServiceUnavailable)
        }
    }
}

fn access_for(
    decision: WorkspaceTrustDecision,
    policy: ProjectContentPolicy,
    kind: ProjectInputKind,
) -> ProjectAccess {
    if decision == WorkspaceTrustDecision::Trusted {
        return ProjectAccess::Allowed;
    }
    if decision == WorkspaceTrustDecision::Unknown {
        return ProjectAccess::Deferred {
            prerequisite: UNKNOWN_PREREQUISITE,
        };
    }
    if kind.is_executable() {
        return ProjectAccess::Deferred {
            prerequisite: EXECUTABLE_PREREQUISITE,
        };
    }
    match policy.untrusted_access(kind) {
        UntrustedProjectAccess::ReadOnly => ProjectAccess::Allowed,
        UntrustedProjectAccess::Block => ProjectAccess::Deferred {
            prerequisite: match kind {
                ProjectInputKind::Instructions => INSTRUCTION_PREREQUISITE,
                ProjectInputKind::Settings => SETTINGS_PREREQUISITE,
                ProjectInputKind::Plugin
                | ProjectInputKind::Process
                | ProjectInputKind::Mcp
                | ProjectInputKind::Hook => EXECUTABLE_PREREQUISITE,
            },
        },
    }
}

fn advance(revision: u64) -> Result<u64, WorkspaceTrustError> {
    revision
        .checked_add(1)
        .ok_or(WorkspaceTrustError::RevisionExhausted)
}

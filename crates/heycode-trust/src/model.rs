//! Typed identity, decisions, policies, and UI/startup projections.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::WorkspaceTrustError;

const ID_DOMAIN: &[u8] = b"dshx-workspace-v1\0";
const MAX_NORMALIZED_ROOT_BYTES: usize = 16 * 1024;

/// Opaque stable identity derived from one canonical workspace root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Lowercase SHA-256 identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_normalized_root(root: &str) -> Result<Self, WorkspaceTrustError> {
        validate_normalized_root(root)?;
        let mut digest = Sha256::new();
        digest.update(ID_DOMAIN);
        digest.update(root.as_bytes());
        Ok(Self(format!("{:x}", digest.finalize())))
    }

    pub(crate) fn validate_stored(value: &str) -> Result<(), WorkspaceTrustError> {
        let valid = value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        if valid {
            Ok(())
        } else {
            Err(WorkspaceTrustError::InvalidStore)
        }
    }
}

/// Canonical path plus its opaque durable identity.
#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceIdentity {
    id: WorkspaceId,
    canonical_root: PathBuf,
    normalized_root: String,
}

impl WorkspaceIdentity {
    /// Canonicalize and validate one existing directory.
    ///
    /// # Errors
    /// Missing/non-directory/non-UTF-8 roots or canonicalization failures are
    /// reported without echoing the path.
    pub fn discover(root: impl AsRef<Path>) -> Result<Self, WorkspaceTrustError> {
        let canonical_root = std::fs::canonicalize(root.as_ref())
            .map_err(|_| WorkspaceTrustError::InvalidWorkspace)?;
        if !canonical_root.is_dir() {
            return Err(WorkspaceTrustError::InvalidWorkspace);
        }
        let normalized_root = normalize_canonical_root(&canonical_root)
            .map_err(|_| WorkspaceTrustError::InvalidWorkspace)?;
        let id = WorkspaceId::from_normalized_root(&normalized_root)?;
        Ok(Self {
            id,
            canonical_root,
            normalized_root,
        })
    }

    /// Opaque durable id.
    #[must_use]
    pub const fn id(&self) -> &WorkspaceId {
        &self.id
    }

    /// Canonical workspace root used by policy consumers and the trust UI.
    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    pub(crate) fn normalized_root(&self) -> &str {
        &self.normalized_root
    }
}

impl std::fmt::Debug for WorkspaceIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceIdentity")
            .field("id", &self.id)
            .field("canonical_root", &"<redacted>")
            .finish()
    }
}

/// Effective human decision for this workspace in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrustDecision {
    /// No affirmative decision exists. This is the only default.
    Unknown,
    /// Open without project executable authority.
    Restricted,
    /// Project executable contributions may cross the K12 gate.
    Trusted,
}

impl WorkspaceTrustDecision {
    /// Stable UI/config identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Restricted => "restricted",
            Self::Trusted => "trusted",
        }
    }

    pub(crate) const fn is_explicit(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// Lifetime of the effective decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustPersistence {
    /// No decision exists.
    None,
    /// Process/session-only explicit decision.
    Session,
    /// Owner-only durable trust-store record.
    Persistent,
}

/// Whether untrusted non-executable project content may be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UntrustedProjectAccess {
    /// Defer content until the workspace is trusted.
    Block,
    /// Permit read-only consumption without granting executable authority.
    ReadOnly,
}

/// Explicit policy for project instructions and project settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectContentPolicy {
    instructions: UntrustedProjectAccess,
    settings: UntrustedProjectAccess,
}

impl ProjectContentPolicy {
    /// Construct an explicit policy. There is deliberately no implicit default.
    #[must_use]
    pub const fn new(
        instructions: UntrustedProjectAccess,
        settings: UntrustedProjectAccess,
    ) -> Self {
        Self {
            instructions,
            settings,
        }
    }

    pub(crate) const fn untrusted_access(self, kind: ProjectInputKind) -> UntrustedProjectAccess {
        match kind {
            ProjectInputKind::Instructions => self.instructions,
            ProjectInputKind::Settings => self.settings,
            ProjectInputKind::Plugin
            | ProjectInputKind::Process
            | ProjectInputKind::Mcp
            | ProjectInputKind::Hook => UntrustedProjectAccess::Block,
        }
    }
}

/// Project-origin capability crossing the trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectInputKind {
    /// Repository instruction/skill content; model-visible but not executable authority.
    Instructions,
    /// Plugin-owned project settings values; never loaded without explicit policy.
    Settings,
    /// Project/local-project plugin/profile activation.
    Plugin,
    /// Project exact-argv process or command-backed credential contribution.
    Process,
    /// Project MCP definition/transport.
    Mcp,
    /// Project hook handler.
    Hook,
}

impl ProjectInputKind {
    pub(crate) const fn is_executable(self) -> bool {
        matches!(self, Self::Plugin | Self::Process | Self::Mcp | Self::Hook)
    }
}

/// Authorization outcome for one project-origin input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectAccess {
    /// The current decision/policy permits this input.
    Allowed,
    /// Discovery may report it, but activation/consumption must not occur.
    Deferred {
        /// Static prerequisite safe for UI/diagnostics.
        prerequisite: &'static str,
    },
}

impl ProjectAccess {
    /// Whether the Consumer may load/activate this input now.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Static repair/prerequisite for a deferred input.
    #[must_use]
    pub const fn prerequisite(self) -> Option<&'static str> {
        match self {
            Self::Allowed => None,
            Self::Deferred { prerequisite } => Some(prerequisite),
        }
    }
}

/// Immutable effective decision snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceTrustSnapshot {
    identity: WorkspaceIdentity,
    decision: WorkspaceTrustDecision,
    persistence: TrustPersistence,
    revision: u64,
}

impl WorkspaceTrustSnapshot {
    pub(crate) fn new(
        identity: WorkspaceIdentity,
        decision: WorkspaceTrustDecision,
        persistence: TrustPersistence,
        revision: u64,
    ) -> Self {
        Self {
            identity,
            decision,
            persistence,
            revision,
        }
    }

    /// Canonical workspace identity.
    #[must_use]
    pub const fn identity(&self) -> &WorkspaceIdentity {
        &self.identity
    }

    /// Effective decision.
    #[must_use]
    pub fn decision(&self) -> WorkspaceTrustDecision {
        self.decision
    }

    /// Effective decision lifetime.
    #[must_use]
    pub const fn persistence(&self) -> TrustPersistence {
        self.persistence
    }

    /// Monotonic service revision used by UI CAS.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Typed U01 dialog input; no action or startup orchestration is implied.
#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceTrustDialogState {
    snapshot: std::sync::Arc<WorkspaceTrustSnapshot>,
    instructions: ProjectAccess,
    settings: ProjectAccess,
    project_executables: ProjectAccess,
}

impl WorkspaceTrustDialogState {
    pub(crate) fn new(
        snapshot: std::sync::Arc<WorkspaceTrustSnapshot>,
        instructions: ProjectAccess,
        settings: ProjectAccess,
        project_executables: ProjectAccess,
    ) -> Self {
        Self {
            snapshot,
            instructions,
            settings,
            project_executables,
        }
    }

    /// Canonical path presented for the trust decision.
    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        self.snapshot.identity.canonical_root()
    }

    /// Opaque identity shown/copied without exposing additional path material.
    #[must_use]
    pub fn workspace_id(&self) -> &WorkspaceId {
        self.snapshot.identity.id()
    }

    /// Effective decision.
    #[must_use]
    pub fn decision(&self) -> WorkspaceTrustDecision {
        self.snapshot.decision
    }

    /// Whether the current decision is absent, session-only, or persistent.
    #[must_use]
    pub fn persistence(&self) -> TrustPersistence {
        self.snapshot.persistence
    }

    /// Snapshot CAS revision.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.snapshot.revision
    }

    /// Project instruction access under the explicit content policy.
    #[must_use]
    pub const fn instructions(&self) -> ProjectAccess {
        self.instructions
    }

    /// Project settings access under the explicit content policy.
    #[must_use]
    pub const fn settings(&self) -> ProjectAccess {
        self.settings
    }

    /// Shared result for project plugins/processes/MCP/hooks.
    #[must_use]
    pub const fn project_executables(&self) -> ProjectAccess {
        self.project_executables
    }

    /// Stable keyboard/action choices for the U01 dialog.
    #[must_use]
    pub const fn actions(&self) -> &'static [WorkspaceTrustAction] {
        &WorkspaceTrustAction::ALL
    }
}

impl std::fmt::Debug for WorkspaceTrustDialogState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceTrustDialogState")
            .field("workspace_id", self.snapshot.identity.id())
            .field("canonical_root", &"<redacted>")
            .field("decision", &self.snapshot.decision)
            .field("revision", &self.snapshot.revision)
            .finish()
    }
}

/// Frontend startup behavior at an unknown workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustFrontend {
    /// May render a typed trust dialog.
    Interactive,
    /// One-shot terminal command; never prompts.
    Headless,
    /// ACP stdio server; never prompts through local terminal UI.
    Acp,
}

impl TrustFrontend {
    /// Stable diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Headless => "headless",
            Self::Acp => "acp",
        }
    }
}

impl std::fmt::Display for TrustFrontend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Explicit noninteractive flag mapped to a session-only decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitWorkspaceTrust {
    /// Trust project executable contributions for this process only.
    TrustOnce,
    /// Continue without project executable contributions for this process.
    RestrictedOnce,
}

impl ExplicitWorkspaceTrust {
    pub(crate) const fn decision(self) -> WorkspaceTrustDecision {
        match self {
            Self::TrustOnce => WorkspaceTrustDecision::Trusted,
            Self::RestrictedOnce => WorkspaceTrustDecision::Restricted,
        }
    }
}

/// Typed U01 trust-dialog action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTrustAction {
    /// Trust executable project contributions for this process only.
    TrustOnce,
    /// Persist trust for this canonical workspace identity.
    TrustWorkspace,
    /// Continue this process with project executable authority deferred.
    OpenRestricted,
    /// Leave without changing trust state.
    Exit,
}

impl WorkspaceTrustAction {
    const ALL: [Self; 4] = [
        Self::TrustOnce,
        Self::TrustWorkspace,
        Self::OpenRestricted,
        Self::Exit,
    ];
}

/// Result of one typed trust-dialog action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTrustActionOutcome {
    /// A session/persistent decision committed and startup may continue.
    Ready(std::sync::Arc<WorkspaceTrustSnapshot>),
    /// Exit was selected; no state changed.
    Exit,
}

/// Result of applying the non-prompting startup rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStartupState {
    /// Startup may continue under this exact decision.
    Ready(std::sync::Arc<WorkspaceTrustSnapshot>),
    /// Interactive caller must render this typed dialog before continuing.
    Prompt(WorkspaceTrustDialogState),
}

fn normalize_canonical_root(path: &Path) -> Result<String, WorkspaceTrustError> {
    let raw = path.to_str().ok_or(WorkspaceTrustError::InvalidWorkspace)?;
    #[cfg(windows)]
    let normalized = normalize_root_text(raw, true)?;
    #[cfg(not(windows))]
    let normalized = normalize_root_text(raw, false)?;
    Ok(normalized)
}

fn normalize_root_text(raw: &str, windows: bool) -> Result<String, WorkspaceTrustError> {
    let normalized = if windows {
        raw.strip_prefix(r"\\?\").unwrap_or(raw).replace('\\', "/")
    } else {
        raw.to_owned()
    };
    validate_normalized_root(&normalized)?;
    Ok(normalized)
}

pub(crate) fn validate_normalized_root(root: &str) -> Result<(), WorkspaceTrustError> {
    let valid = !root.is_empty()
        && root.len() <= MAX_NORMALIZED_ROOT_BYTES
        && !root.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        });
    if valid {
        Ok(())
    } else {
        Err(WorkspaceTrustError::InvalidStore)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::normalize_root_text;

    #[test]
    fn windows_normalization_preserves_case_sensitive_directory_identity() {
        let upper = normalize_root_text(r"\\?\C:\Work\Repo", true).unwrap();
        let lower = normalize_root_text(r"\\?\C:\Work\repo", true).unwrap();
        assert_eq!(upper, "C:/Work/Repo");
        assert_eq!(lower, "C:/Work/repo");
        assert_ne!(upper, lower);
    }
}

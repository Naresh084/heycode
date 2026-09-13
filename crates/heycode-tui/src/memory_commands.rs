//! Source-scoped `/memory` management for user/project instructions and
//! persistent custom-agent memory.
//!
//! The command never accepts a filesystem path. Every operation resolves a
//! stable source id from the current trusted workspace authority, and every
//! write uses the exact revision returned by `show`. This keeps `/add-dir`
//! from silently widening instruction trust and makes a workspace transition
//! invalidate stale edits.

use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::{
    Agent, Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandRegistry,
    CommandSource, CommandTiming, UiEvent, UiPanelId,
};
use heycode_core::{Context, CoreError, CoreResult};
use heycode_prompt::instructions::{
    InstructionSources, MAX_INSTRUCTION_BYTES, WORKSPACE_INSTRUCTION_FILES,
};
use sha2::{Digest as _, Sha256};

const MAX_SOURCE_ID_BYTES: usize = 256;
const MAX_MEMORY_SOURCES: usize = 256;
const MAX_AUTO_DIRECTORY_ENTRIES: usize = 512;
const MAX_DISCOVERY_WARNINGS: usize = 64;
const MAX_COMMAND_OUTPUT_CHARS: usize = 128 * 1024;

/// Source-scoped instruction/auto-memory manager service.
pub const SERVICE_MEMORY_SOURCES: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("memory-sources");

/// Stable native panel requested by bare `/memory`.
pub const MEMORY_PANEL_ID: &str = "memory";

/// Cloneable context handle to the one manager instance used by `/memory`.
pub struct MemorySourceManagerHandle(
    /// Shared manager owned by the composed `memory-commands` plugin.
    pub Arc<MemorySourceManager>,
);

/// Current trusted instruction roots. Implementations must not infer trust
/// from an additional filesystem root alone.
pub trait MemoryAuthority: Send + Sync {
    /// Resolve the exact user/workspace instruction roots now in force.
    ///
    /// # Errors
    /// Unavailable or transitioning authority must fail without returning a
    /// stale prior root.
    fn instruction_sources(&self) -> Result<InstructionSources, MemoryManagerError>;
}

struct WorkspaceMemoryAuthority {
    workspace: Arc<heycode_agent::workspace_transition::WorkspaceTransitionService>,
    user_home: Option<PathBuf>,
    initial_project_trusted: bool,
}

impl MemoryAuthority for WorkspaceMemoryAuthority {
    fn instruction_sources(&self) -> Result<InstructionSources, MemoryManagerError> {
        self.workspace
            .instruction_sources(self.user_home.clone(), self.initial_project_trusted)
            .map_err(|_| MemoryManagerError::AuthorityUnavailable)
    }
}

/// Source category shown by `/memory list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySourceKind {
    /// Standing user/project instructions included in every native request.
    Instructions,
    /// Persistent notes loaded only by the matching custom child preset.
    AutoMemory,
}

impl MemorySourceKind {
    /// Stable diagnostic word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instructions => "instructions",
            Self::AutoMemory => "auto-memory",
        }
    }
}

/// Authority scope for one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySourceScope {
    /// User-wide heycode state.
    User,
    /// Current project, only while workspace instruction trust applies.
    Project,
    /// User state keyed to the current trusted project identity.
    Local,
}

impl MemorySourceScope {
    /// Stable diagnostic word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}

/// Safe inspectability state for one source row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorySourceStatus {
    /// File is absent but this fixed source may be created.
    Missing,
    /// Regular bounded UTF-8 content was read.
    Ready {
        /// Current byte length.
        bytes: usize,
        /// Revision required for replace/clear.
        revision: String,
    },
    /// A source candidate exists but is unsafe or unreadable.
    Blocked {
        /// Fixed safe reason without path or operating-system details.
        reason: &'static str,
    },
}

/// One source-attributed manager row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySourceView {
    id: String,
    label: String,
    kind: MemorySourceKind,
    scope: MemorySourceScope,
    status: MemorySourceStatus,
}

impl MemorySourceView {
    /// Stable id accepted by `show`, `replace`, and `clear`.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Human label without an ambient absolute path.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether this is standing instruction or child auto-memory content.
    #[must_use]
    pub const fn kind(&self) -> MemorySourceKind {
        self.kind
    }

    /// User/project/local authority class.
    #[must_use]
    pub const fn scope(&self) -> MemorySourceScope {
        self.scope
    }

    /// Current safe inspection state.
    #[must_use]
    pub fn status(&self) -> &MemorySourceStatus {
        &self.status
    }
}

/// Complete bounded snapshot. Per-source failures remain rows, not omissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryManagerSnapshot {
    /// Deterministically ordered sources.
    pub sources: Vec<MemorySourceView>,
    /// Source-attributed partial failures encountered during discovery.
    pub warnings: Vec<String>,
}

/// Exact source document returned by `show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDocument {
    /// Source metadata captured with the content.
    pub source: MemorySourceView,
    /// Bounded UTF-8 content; empty when the fixed file is absent.
    pub text: String,
    /// Revision required by replace/clear.
    pub revision: String,
}

/// Result of one revision-checked atomic replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWriteOutcome {
    /// Stable source id.
    pub source_id: String,
    /// New content revision.
    pub revision: String,
    /// Whether a previously absent file was created.
    pub created: bool,
    /// True only for standing instruction sources that affect a future request.
    pub prompt_changed: bool,
}

/// Deterministic manager failures; path and OS error details are intentionally
/// excluded because command output is part of the transcript.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MemoryManagerError {
    /// Slash command grammar is invalid.
    #[error(
        "usage: /memory [list|reload|show <source-id>|replace <source-id> <revision> <text>|clear <source-id> <revision>]"
    )]
    Usage,
    /// Workspace authority could not be resolved now.
    #[error("memory authority is unavailable; no source was read or changed")]
    AuthorityUnavailable,
    /// Caller supplied a path-like or unknown id.
    #[error("unknown memory source id; run /memory list")]
    UnknownSource,
    /// Existing content or path topology is unsafe.
    #[error("memory source is not a regular no-symlink file within its trusted root")]
    UnsafeSource,
    /// Source could not be inspected without exposing lower-level details.
    #[error("memory source could not be read; no change was made")]
    ReadFailed,
    /// Existing content is not valid UTF-8.
    #[error("memory source is not UTF-8; no change was made")]
    InvalidUtf8,
    /// Existing or proposed content exceeds the common bounded limit.
    #[error("memory source exceeds 64 KiB; no change was made")]
    TooLarge,
    /// Optimistic revision no longer matches the exact source binding/content.
    #[error("memory source revision changed; run /memory show again before writing")]
    StaleRevision,
    /// In-process mutation owner was poisoned.
    #[error("memory manager is unavailable; no change was made")]
    ManagerUnavailable,
    /// Atomic stage/commit failed.
    #[error("memory source could not be committed atomically; no prompt refresh is claimed")]
    WriteFailed,
}

#[derive(Clone)]
struct SourceBinding {
    view_id: String,
    label: String,
    kind: MemorySourceKind,
    scope: MemorySourceScope,
    root: PathBuf,
    relative: PathBuf,
    create_parent: bool,
}

/// Live source manager shared by slash command and any future native panel.
pub struct MemorySourceManager {
    authority: Arc<dyn MemoryAuthority>,
    auto_memory_root: Option<PathBuf>,
    writer: Mutex<()>,
}

impl MemorySourceManager {
    /// Bind the manager to the dynamic workspace trust resolver.
    #[must_use]
    pub fn for_workspace(
        workspace: Arc<heycode_agent::workspace_transition::WorkspaceTransitionService>,
        user_home: Option<PathBuf>,
        auto_memory_root: PathBuf,
        initial_project_trusted: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            authority: Arc::new(WorkspaceMemoryAuthority {
                workspace,
                user_home,
                initial_project_trusted,
            }),
            auto_memory_root: Some(auto_memory_root),
            writer: Mutex::new(()),
        })
    }

    /// Bind an explicit authority provider. Primarily useful for composed
    /// front ends and deterministic tests; the provider owns the trust proof.
    #[must_use]
    pub fn new(authority: Arc<dyn MemoryAuthority>) -> Self {
        Self::with_auto_memory_root(authority, None)
    }

    /// Bind explicit instruction authority and persistent-agent state roots.
    /// When `auto_memory_root` is absent, user auto-memory falls back to the
    /// instruction home for compatibility with small embedded compositions.
    #[must_use]
    pub fn with_auto_memory_root(
        authority: Arc<dyn MemoryAuthority>,
        auto_memory_root: Option<PathBuf>,
    ) -> Self {
        Self {
            authority,
            auto_memory_root,
            writer: Mutex::new(()),
        }
    }

    /// Cheap dynamic command readiness check; does not scan or hash files.
    ///
    /// # Errors
    /// Current authority resolution failure is surfaced to command discovery.
    pub fn is_available(&self) -> Result<bool, MemoryManagerError> {
        let sources = self.authority.instruction_sources()?;
        Ok(sources.user_home.is_some() || sources.workspace.is_some())
    }

    /// Discover all fixed instruction candidates and existing auto-memory
    /// preset directories. Unsafe auto-memory rows become warnings/blocked
    /// rows and do not hide valid sources.
    ///
    /// # Errors
    /// Current authority resolution failure aborts the snapshot.
    pub fn snapshot(&self) -> Result<MemoryManagerSnapshot, MemoryManagerError> {
        let (bindings, warnings) = self.bindings()?;
        let sources = bindings
            .iter()
            .map(|binding| self.view(binding))
            .collect::<Vec<_>>();
        Ok(MemoryManagerSnapshot { sources, warnings })
    }

    /// Read one exact current source by stable id.
    ///
    /// # Errors
    /// Unknown ids, trust changes, unsafe files and bounded UTF-8 failures are
    /// explicit. Missing fixed files return empty text and a usable revision.
    pub fn read(&self, source_id: &str) -> Result<MemoryDocument, MemoryManagerError> {
        let binding = self.resolve(source_id)?;
        self.read_binding(&binding)
    }

    /// Replace one exact current source using the revision from [`Self::read`].
    ///
    /// # Errors
    /// Trust/scope changes, stale revisions, unsafe paths, oversized content
    /// or atomic-commit failures write nothing.
    pub fn replace(
        &self,
        source_id: &str,
        expected_revision: &str,
        text: &str,
    ) -> Result<MemoryWriteOutcome, MemoryManagerError> {
        if text.len() > MAX_INSTRUCTION_BYTES {
            return Err(MemoryManagerError::TooLarge);
        }
        let _writer = self
            .writer
            .lock()
            .map_err(|_| MemoryManagerError::ManagerUnavailable)?;
        let binding = self.resolve(source_id)?;
        let current = self.read_binding(&binding)?;
        if current.revision != expected_revision {
            return Err(MemoryManagerError::StaleRevision);
        }
        let created = matches!(current.source.status, MemorySourceStatus::Missing);
        let parent = self.ensure_parent(&binding)?;
        let mode = target_mode(&binding, &binding.root.join(&binding.relative));
        let mut staged = tempfile::NamedTempFile::new_in(&parent)
            .map_err(|_| MemoryManagerError::WriteFailed)?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt as _;
            staged
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(mode))
                .map_err(|_| MemoryManagerError::WriteFailed)?;
        }
        staged
            .write_all(text.as_bytes())
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|_| MemoryManagerError::WriteFailed)?;

        // Resolve the id and re-read immediately before commit. A /cd,
        // worktree transition, external edit, or directory replacement makes
        // the revision/binding differ and the staged file is simply dropped.
        let confirmed = self.resolve(source_id)?;
        if confirmed.root != binding.root || confirmed.relative != binding.relative {
            return Err(MemoryManagerError::StaleRevision);
        }
        if self.read_binding(&confirmed)?.revision != expected_revision {
            return Err(MemoryManagerError::StaleRevision);
        }
        let target = confirmed.root.join(&confirmed.relative);
        staged
            .persist(&target)
            .map_err(|_| MemoryManagerError::WriteFailed)?;
        let updated = self.read_binding(&confirmed)?;
        Ok(MemoryWriteOutcome {
            source_id: source_id.to_owned(),
            revision: updated.revision,
            created,
            prompt_changed: binding.kind == MemorySourceKind::Instructions,
        })
    }

    fn bindings(&self) -> Result<(Vec<SourceBinding>, Vec<String>), MemoryManagerError> {
        let sources = self.authority.instruction_sources()?;
        let mut bindings = Vec::new();
        let mut warnings = Vec::new();
        let user_home = sources
            .user_home
            .as_deref()
            .map(canonical_directory)
            .transpose()?;
        let workspace = sources
            .workspace
            .as_deref()
            .map(canonical_directory)
            .transpose()?;
        let auto_memory_root = self
            .auto_memory_root
            .as_deref()
            .map(canonical_directory)
            .transpose()?
            .or_else(|| user_home.clone());

        if let Some(home) = &user_home {
            bindings.push(SourceBinding {
                view_id: "user:agents".to_owned(),
                label: "~/.heycode/AGENTS.md".to_owned(),
                kind: MemorySourceKind::Instructions,
                scope: MemorySourceScope::User,
                root: home.clone(),
                relative: PathBuf::from("AGENTS.md"),
                create_parent: false,
            });
        }
        if let Some(root) = &workspace {
            for (name, id) in [
                (WORKSPACE_INSTRUCTION_FILES[0], "project:agents"),
                (WORKSPACE_INSTRUCTION_FILES[1], "project:claude"),
                (WORKSPACE_INSTRUCTION_FILES[2], "project:heycode"),
                (WORKSPACE_INSTRUCTION_FILES[3], "project:agents-local"),
                (WORKSPACE_INSTRUCTION_FILES[4], "project:claude-local"),
            ] {
                bindings.push(SourceBinding {
                    view_id: id.to_owned(),
                    label: name.to_owned(),
                    kind: MemorySourceKind::Instructions,
                    scope: MemorySourceScope::Project,
                    root: root.clone(),
                    relative: PathBuf::from(name),
                    create_parent: name == ".heycode/INSTRUCTIONS.md",
                });
            }
        }

        if let Some(home) = &auto_memory_root {
            scan_auto_memory(
                &mut bindings,
                &mut warnings,
                home,
                Path::new(".agent-memory/user"),
                "auto:user",
                "User agent memory",
                MemorySourceScope::User,
            );
        }
        if let Some(project) = &workspace {
            scan_auto_memory(
                &mut bindings,
                &mut warnings,
                project,
                Path::new(".heycode/agent-memory"),
                "auto:project",
                "Project agent memory",
                MemorySourceScope::Project,
            );
            if let Some(home) = &auto_memory_root {
                let project_key =
                    format!("{:x}", Sha256::digest(project.to_string_lossy().as_bytes()));
                scan_auto_memory(
                    &mut bindings,
                    &mut warnings,
                    home,
                    &Path::new(".agent-memory/local").join(project_key),
                    "auto:local",
                    "Local agent memory",
                    MemorySourceScope::Local,
                );
            }
        }
        Ok((bindings, warnings))
    }

    fn resolve(&self, source_id: &str) -> Result<SourceBinding, MemoryManagerError> {
        if source_id.is_empty()
            || source_id.len() > MAX_SOURCE_ID_BYTES
            || source_id.chars().any(char::is_whitespace)
            || source_id.chars().any(char::is_control)
            || source_id.contains('/')
            || source_id.contains('\\')
            || source_id.contains("..")
        {
            return Err(MemoryManagerError::UnknownSource);
        }
        self.bindings()?
            .0
            .into_iter()
            .find(|binding| binding.view_id == source_id)
            .ok_or(MemoryManagerError::UnknownSource)
    }

    fn view(&self, binding: &SourceBinding) -> MemorySourceView {
        let status = match self.read_binding(binding) {
            Ok(document) => document.source.status,
            Err(error) => MemorySourceStatus::Blocked {
                reason: error.safe_reason(),
            },
        };
        MemorySourceView {
            id: binding.view_id.clone(),
            label: binding.label.clone(),
            kind: binding.kind,
            scope: binding.scope,
            status,
        }
    }

    fn read_binding(&self, binding: &SourceBinding) -> Result<MemoryDocument, MemoryManagerError> {
        validate_relative(&binding.relative)?;
        let root = canonical_directory(&binding.root)?;
        if root != binding.root {
            return Err(MemoryManagerError::UnsafeSource);
        }
        let path = root.join(&binding.relative);
        let base_identity = directory_identity(&root)?;
        let (text, status) = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (String::new(), MemorySourceStatus::Missing)
            }
            Err(_) => return Err(MemoryManagerError::ReadFailed),
            Ok(metadata) => {
                if !safe_regular_file(&metadata) {
                    return Err(MemoryManagerError::UnsafeSource);
                }
                if metadata.len() > MAX_INSTRUCTION_BYTES as u64 {
                    return Err(MemoryManagerError::TooLarge);
                }
                validate_parent_chain(&root, &binding.relative)?;
                let expected = SourceFileStamp::of(&metadata);
                let mut file = open_source_file(&path)?;
                let opened = file
                    .metadata()
                    .map_err(|_| MemoryManagerError::ReadFailed)?;
                if !safe_regular_file(&opened) || SourceFileStamp::of(&opened) != expected {
                    return Err(MemoryManagerError::UnsafeSource);
                }
                let mut bytes = Vec::new();
                std::io::Read::by_ref(&mut file)
                    .take(MAX_INSTRUCTION_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| MemoryManagerError::ReadFailed)?;
                if bytes.len() > MAX_INSTRUCTION_BYTES {
                    return Err(MemoryManagerError::TooLarge);
                }
                let after = file
                    .metadata()
                    .map_err(|_| MemoryManagerError::ReadFailed)?;
                let current =
                    std::fs::symlink_metadata(&path).map_err(|_| MemoryManagerError::ReadFailed)?;
                if SourceFileStamp::of(&after) != expected
                    || !safe_regular_file(&current)
                    || SourceFileStamp::of(&current) != expected
                {
                    return Err(MemoryManagerError::ReadFailed);
                }
                let text = String::from_utf8(bytes).map_err(|_| MemoryManagerError::InvalidUtf8)?;
                let revision = revision(&binding.view_id, &base_identity, text.as_bytes());
                let status = MemorySourceStatus::Ready {
                    bytes: text.len(),
                    revision,
                };
                (text, status)
            }
        };
        let revision = match &status {
            MemorySourceStatus::Ready { revision, .. } => revision.clone(),
            MemorySourceStatus::Missing => revision(&binding.view_id, &base_identity, &[]),
            MemorySourceStatus::Blocked { .. } => return Err(MemoryManagerError::ReadFailed),
        };
        Ok(MemoryDocument {
            source: MemorySourceView {
                id: binding.view_id.clone(),
                label: binding.label.clone(),
                kind: binding.kind,
                scope: binding.scope,
                status,
            },
            text,
            revision,
        })
    }

    fn ensure_parent(&self, binding: &SourceBinding) -> Result<PathBuf, MemoryManagerError> {
        validate_relative(&binding.relative)?;
        let parent_relative = binding.relative.parent().unwrap_or_else(|| Path::new(""));
        let mut current = binding.root.clone();
        for component in parent_relative.components() {
            let Component::Normal(name) = component else {
                return Err(MemoryManagerError::UnsafeSource);
            };
            current.push(name);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(MemoryManagerError::UnsafeSource);
                }
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound && binding.create_parent =>
                {
                    std::fs::create_dir(&current).map_err(|_| MemoryManagerError::WriteFailed)?;
                }
                Err(_) => return Err(MemoryManagerError::UnsafeSource),
            }
        }
        canonical_directory(&current)
    }
}

impl MemoryManagerError {
    const fn safe_reason(&self) -> &'static str {
        match self {
            Self::Usage => "invalid command arguments",
            Self::AuthorityUnavailable => "authority unavailable",
            Self::UnknownSource => "unknown source",
            Self::UnsafeSource => "unsafe file or directory topology",
            Self::ReadFailed => "unreadable source",
            Self::InvalidUtf8 => "source is not UTF-8",
            Self::TooLarge => "source exceeds 64 KiB",
            Self::StaleRevision => "stale revision",
            Self::ManagerUnavailable => "manager unavailable",
            Self::WriteFailed => "atomic write unavailable",
        }
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, MemoryManagerError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| MemoryManagerError::AuthorityUnavailable)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MemoryManagerError::UnsafeSource);
    }
    std::fs::canonicalize(path).map_err(|_| MemoryManagerError::AuthorityUnavailable)
}

fn validate_relative(path: &Path) -> Result<(), MemoryManagerError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(MemoryManagerError::UnsafeSource)
    } else {
        Ok(())
    }
}

fn validate_parent_chain(root: &Path, relative: &Path) -> Result<(), MemoryManagerError> {
    let mut current = root.to_path_buf();
    for component in relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .components()
    {
        let Component::Normal(name) = component else {
            return Err(MemoryManagerError::UnsafeSource);
        };
        current.push(name);
        let metadata =
            std::fs::symlink_metadata(&current).map_err(|_| MemoryManagerError::UnsafeSource)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(MemoryManagerError::UnsafeSource);
        }
    }
    Ok(())
}

fn directory_identity(path: &Path) -> Result<Vec<u8>, MemoryManagerError> {
    let metadata = std::fs::metadata(path).map_err(|_| MemoryManagerError::AuthorityUnavailable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()).into_bytes())
    }
    #[cfg(not(unix))]
    Ok(path.to_string_lossy().as_bytes().to_vec())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceFileStamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl SourceFileStamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

fn safe_regular_file(metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return false;
        }
    }
    true
}

fn open_source_file(path: &Path) -> Result<std::fs::File, MemoryManagerError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| MemoryManagerError::ReadFailed)
}

fn revision(source_id: &str, identity: &[u8], text: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(source_id.as_bytes());
    digest.update([0]);
    digest.update(identity);
    digest.update([0]);
    digest.update(text);
    format!("{:x}", digest.finalize())
}

#[cfg(unix)]
fn target_mode(binding: &SourceBinding, target: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(target)
        .ok()
        .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .map(|metadata| metadata.permissions().mode())
        .or_else(|| {
            Some(if binding.kind == MemorySourceKind::Instructions {
                0o644
            } else {
                0o600
            })
        })
}

#[cfg(not(unix))]
fn target_mode(_binding: &SourceBinding, _target: &Path) -> Option<u32> {
    None
}

fn scan_auto_memory(
    bindings: &mut Vec<SourceBinding>,
    warnings: &mut Vec<String>,
    root: &Path,
    relative_base: &Path,
    id_prefix: &str,
    label_prefix: &str,
    scope: MemorySourceScope,
) {
    if bindings.len() >= MAX_MEMORY_SOURCES {
        push_discovery_warning(
            warnings,
            format!("{id_prefix}: source limit reached; remaining presets were skipped"),
        );
        return;
    }
    let base = root.join(relative_base);
    let metadata = match std::fs::symlink_metadata(&base) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            push_discovery_warning(
                warnings,
                format!("{id_prefix}: auto-memory directory is unreadable"),
            );
            return;
        }
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            push_discovery_warning(
                warnings,
                format!("{id_prefix}: auto-memory directory is unsafe"),
            );
            return;
        }
        Ok(metadata) => metadata,
    };
    let _ = metadata;
    if validate_relative(relative_base).is_err()
        || validate_parent_chain(root, &relative_base.join("MEMORY.md")).is_err()
    {
        push_discovery_warning(
            warnings,
            format!("{id_prefix}: auto-memory directory is unsafe"),
        );
        return;
    }
    let directory = match std::fs::read_dir(&base) {
        Ok(entries) => entries,
        Err(_) => {
            push_discovery_warning(
                warnings,
                format!("{id_prefix}: auto-memory directory is unreadable"),
            );
            return;
        }
    };
    let mut entries = Vec::new();
    let mut truncated = false;
    for (index, entry) in directory.enumerate() {
        if index >= MAX_AUTO_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }
        match entry {
            Ok(entry) => entries.push(entry),
            Err(_) => push_discovery_warning(
                warnings,
                format!("{id_prefix}: skipped an unreadable directory entry"),
            ),
        }
    }
    if truncated {
        push_discovery_warning(
            warnings,
            format!("{id_prefix}: directory entry limit reached; remaining entries were skipped"),
        );
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if bindings.len() >= MAX_MEMORY_SOURCES {
            push_discovery_warning(
                warnings,
                format!("{id_prefix}: source limit reached; remaining presets were skipped"),
            );
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            push_discovery_warning(
                warnings,
                format!("{id_prefix}: skipped a non-UTF-8 preset directory"),
            );
            continue;
        };
        if heycode_agent::SubagentPresetId::new(name).is_err() {
            push_discovery_warning(warnings, format!("{id_prefix}: skipped invalid preset id"));
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(_) => {
                push_discovery_warning(
                    warnings,
                    format!("{id_prefix}:{name}: preset directory is unreadable"),
                );
                continue;
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            push_discovery_warning(
                warnings,
                format!("{id_prefix}:{name}: preset directory is unsafe"),
            );
            continue;
        }
        bindings.push(SourceBinding {
            view_id: format!("{id_prefix}:{name}"),
            label: format!("{label_prefix}: {name}"),
            kind: MemorySourceKind::AutoMemory,
            scope,
            root: root.to_path_buf(),
            relative: relative_base.join(name).join("MEMORY.md"),
            create_parent: false,
        });
    }
}

fn push_discovery_warning(warnings: &mut Vec<String>, warning: String) {
    if warnings.len() < MAX_DISCOVERY_WARNINGS {
        warnings.push(warning);
    }
}

struct MemoryCommand {
    manager: Arc<MemorySourceManager>,
    descriptor: CommandDescriptor,
    unavailable: CommandAvailability,
    panel: UiPanelId,
}

#[async_trait]
impl Command for MemoryCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        match self.manager.is_available() {
            Ok(false) => self.unavailable.clone(),
            Ok(true) => CommandAvailability::available(),
            Err(_) => CommandAvailability::unavailable("Memory authority is unavailable")
                .unwrap_or_else(|_| self.unavailable.clone()),
        }
    }

    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        if args.trim().is_empty() {
            agent.ui().emit(UiEvent::CapabilityPanelRequested {
                panel: self.panel.clone(),
            });
            return Ok(());
        }
        let output = execute_memory(&self.manager, args)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent.ui().emit(UiEvent::Info {
            text: sanitize_output(&output),
        });
        Ok(())
    }
}

/// Register the source-scoped `/memory` command in the owning composition.
///
/// # Errors
/// Invalid static metadata or a registry collision fails composition.
pub fn register_memory_command(
    context: &Context,
    commands: &CommandRegistry,
    manager: Arc<MemorySourceManager>,
) -> CoreResult<()> {
    let action = CommandArgument::optional("action", "list, show, replace, clear, or reload")
        .map_err(|error| CoreError::other(error.to_string()))?;
    let arguments =
        CommandArgument::optional("arguments", "Source id, revision, and replacement text")
            .map_err(|error| CoreError::other(error.to_string()))?
            .variadic();
    let descriptor = CommandDescriptor::new(
        "memory",
        "Manage trusted instruction and auto-memory sources",
        vec![action, arguments],
        CommandTiming::Queued,
        CommandSource::from_plugin("memory-commands")
            .map_err(|error| CoreError::other(error.to_string()))?,
    )
    .map_err(|error| CoreError::other(error.to_string()))?;
    let unavailable = CommandAvailability::unavailable("No trusted memory sources are attached")
        .map_err(|error| CoreError::other(error.to_string()))?;
    let panel =
        UiPanelId::new(MEMORY_PANEL_ID).map_err(|error| CoreError::other(error.to_string()))?;
    commands
        .register_effect(
            context,
            Arc::new(MemoryCommand {
                manager,
                descriptor,
                unavailable,
                panel,
            }),
        )
        .map_err(|error| CoreError::other(error.to_string()))
}

/// Compose the `/memory` command and its shared source manager against the
/// dynamic workspace authority. `user_home` owns standing user instructions;
/// `auto_memory_root` must be the same state root used by persistent subagents.
#[must_use]
pub fn memory_commands_plugin(
    user_home: Option<PathBuf>,
    auto_memory_root: PathBuf,
    initial_project_trusted: bool,
) -> Box<dyn heycode_core::Plugin> {
    struct MemoryCommandsPlugin {
        user_home: Option<PathBuf>,
        auto_memory_root: PathBuf,
        initial_project_trusted: bool,
    }
    impl heycode_core::Plugin for MemoryCommandsPlugin {
        fn name(&self) -> &'static str {
            "memory-commands"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::Command,
                "memory",
            )]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MEMORY_SOURCES]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_agent::SERVICE_COMMANDS,
                heycode_agent::workspace_transition::SERVICE_WORKSPACE_TRANSITION,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands = context
                .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("memory command registry missing"))?;
            let workspace = context
                .get::<heycode_agent::workspace_transition::WorkspaceTransitionHandle>(
                    heycode_agent::workspace_transition::SERVICE_WORKSPACE_TRANSITION,
                )
                .ok_or_else(|| CoreError::other("memory workspace authority missing"))?;
            let manager = MemorySourceManager::for_workspace(
                workspace.0.clone(),
                self.user_home.clone(),
                self.auto_memory_root.clone(),
                self.initial_project_trusted,
            );
            register_memory_command(context, &commands, manager.clone())?;
            context.provide(
                SERVICE_MEMORY_SOURCES,
                self.name(),
                MemorySourceManagerHandle(manager),
            )
        }
    }
    Box::new(MemoryCommandsPlugin {
        user_home,
        auto_memory_root,
        initial_project_trusted,
    })
}

fn execute_memory(manager: &MemorySourceManager, args: &str) -> Result<String, MemoryManagerError> {
    let trimmed = args.trim();
    let (action, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    match action {
        "list" | "reload" => Ok(render_snapshot(&manager.snapshot()?)),
        "show" => {
            let id = one_argument(rest).ok_or(MemoryManagerError::Usage)?;
            memory_show_text(manager, id)
        }
        "replace" => {
            let (id, rest) = split_required(rest)?;
            let (revision, text) = split_required(rest)?;
            Ok(render_write(manager.replace(id, revision, text)?))
        }
        "clear" => {
            let (id, revision) = split_required(rest)?;
            if revision.split_whitespace().count() != 1 {
                return Err(MemoryManagerError::Usage);
            }
            Ok(render_write(manager.replace(id, revision, "")?))
        }
        _ => Err(MemoryManagerError::Usage),
    }
}

fn render_snapshot(snapshot: &MemoryManagerSnapshot) -> String {
    let mut rows = vec![format!(
        "Memory sources: {} trusted candidates; {} warnings",
        snapshot.sources.len(),
        snapshot.warnings.len()
    )];
    for source in &snapshot.sources {
        let status = match &source.status {
            MemorySourceStatus::Missing => {
                format!("missing; run /memory show {} before creating it", source.id)
            }
            MemorySourceStatus::Ready { bytes, .. } => {
                format!("{bytes} bytes; run /memory show {} for details", source.id)
            }
            MemorySourceStatus::Blocked { reason } => format!("blocked: {reason}"),
        };
        rows.push(format!(
            "{} — {} — {} / {} — {status}",
            source.id,
            source.label,
            source.scope.as_str(),
            source.kind.as_str()
        ));
    }
    rows.extend(
        snapshot
            .warnings
            .iter()
            .map(|warning| format!("warning: {warning}")),
    );
    rows.push("Standing instructions are rendered fresh on the next request. Auto-memory is loaded only by its matching custom agent; listing or editing it does not inject it into this conversation.".to_owned());
    rows.join("\n")
}

/// Render the same bounded, source-attributed detail used by explicit
/// `/memory show` and by the native chooser's deliberate selection action.
///
/// # Errors
/// The id is re-resolved against current authority and unsafe or unreadable
/// sources remain explicit failures.
pub(crate) fn memory_show_text(
    manager: &MemorySourceManager,
    source_id: &str,
) -> Result<String, MemoryManagerError> {
    let document = manager.read(source_id)?;
    Ok(sanitize_output(&format!(
        "{} [{} {}]\nsource-id: {}\nrevision: {}\n<source-content>\n{}\n</source-content>",
        document.source.label,
        document.source.scope.as_str(),
        document.source.kind.as_str(),
        document.source.id,
        document.revision,
        document.text
    )))
}

fn render_write(outcome: MemoryWriteOutcome) -> String {
    let prompt = if outcome.prompt_changed {
        "standing instructions changed; the next request renders the new source"
    } else {
        "auto-memory changed; no current-conversation prompt invalidation is claimed"
    };
    format!(
        "{} {} at revision {}; {prompt}",
        if outcome.created {
            "created"
        } else {
            "updated"
        },
        outcome.source_id,
        outcome.revision
    )
}

fn one_argument(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.split_whitespace().count() == 1).then_some(value)
}

fn split_required(value: &str) -> Result<(&str, &str), MemoryManagerError> {
    let value = value.trim_start();
    let (first, rest) = value
        .split_once(char::is_whitespace)
        .ok_or(MemoryManagerError::Usage)?;
    if first.is_empty() {
        return Err(MemoryManagerError::Usage);
    }
    Ok((first, rest.trim_start()))
}

fn sanitize_output(value: &str) -> String {
    const TRUNCATED: &str = "\n[output truncated by /memory]";
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                '\u{fffd}'
            } else {
                character
            }
        })
        .take(MAX_COMMAND_OUTPUT_CHARS + 1)
        .collect::<String>();
    if sanitized.chars().count() <= MAX_COMMAND_OUTPUT_CHARS {
        return sanitized;
    }
    let visible = MAX_COMMAND_OUTPUT_CHARS.saturating_sub(TRUNCATED.chars().count());
    let mut bounded = sanitized.chars().take(visible).collect::<String>();
    bounded.push_str(TRUNCATED);
    bounded
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FixedAuthority {
        sources: Mutex<InstructionSources>,
    }

    impl FixedAuthority {
        fn new(user_home: Option<PathBuf>, workspace: Option<PathBuf>) -> Self {
            Self {
                sources: Mutex::new(InstructionSources {
                    user_home,
                    workspace,
                }),
            }
        }

        fn set_workspace(&self, workspace: Option<PathBuf>) {
            self.sources.lock().unwrap().workspace = workspace;
        }
    }

    impl MemoryAuthority for FixedAuthority {
        fn instruction_sources(&self) -> Result<InstructionSources, MemoryManagerError> {
            Ok(self.sources.lock().unwrap().clone())
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn inventory_attributes_fixed_instructions_and_all_existing_auto_memory_scopes() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(&home.path().join("AGENTS.md"), "user law");
        write(
            &home.path().join(".agent-memory/user/reviewer/MEMORY.md"),
            "user notes",
        );
        write(
            &project
                .path()
                .join(".heycode/agent-memory/builder/MEMORY.md"),
            "project notes",
        );
        let canonical_project = project.path().canonicalize().unwrap();
        let local_key = format!(
            "{:x}",
            Sha256::digest(canonical_project.to_string_lossy().as_bytes())
        );
        write(
            &home
                .path()
                .join(".agent-memory/local")
                .join(local_key)
                .join("advisor/MEMORY.md"),
            "local notes",
        );
        let authority = Arc::new(FixedAuthority::new(
            Some(home.path().to_path_buf()),
            Some(project.path().to_path_buf()),
        ));
        let manager = MemorySourceManager::new(authority);
        let snapshot = manager.snapshot().unwrap();
        let ids = snapshot
            .sources
            .iter()
            .map(|source| source.id())
            .collect::<Vec<_>>();
        assert_eq!(
            &ids[..6],
            [
                "user:agents",
                "project:agents",
                "project:claude",
                "project:heycode",
                "project:agents-local",
                "project:claude-local",
            ]
        );
        assert!(ids.contains(&"auto:user:reviewer"));
        assert!(ids.contains(&"auto:project:builder"));
        assert!(ids.contains(&"auto:local:advisor"));
        let user = snapshot
            .sources
            .iter()
            .find(|source| source.id() == "user:agents")
            .unwrap();
        assert_eq!(user.label(), "~/.heycode/AGENTS.md");
        assert_eq!(user.scope(), MemorySourceScope::User);
        assert_eq!(user.kind(), MemorySourceKind::Instructions);
        assert!(matches!(user.status(), MemorySourceStatus::Ready { .. }));
        assert!(snapshot.warnings.is_empty());
    }

    #[test]
    fn exact_revision_writes_are_atomic_and_prompt_effect_is_source_specific() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".agent-memory/user/reviewer/MEMORY.md"),
            "old notes",
        );
        let authority = Arc::new(FixedAuthority::new(
            Some(home.path().to_path_buf()),
            Some(project.path().to_path_buf()),
        ));
        let manager = MemorySourceManager::new(authority);

        let missing = manager.read("project:agents").unwrap();
        assert!(matches!(
            missing.source.status(),
            MemorySourceStatus::Missing
        ));
        let created = manager
            .replace("project:agents", &missing.revision, "project law")
            .unwrap();
        assert!(created.created);
        assert!(created.prompt_changed);
        assert_eq!(
            std::fs::read_to_string(project.path().join("AGENTS.md")).unwrap(),
            "project law"
        );
        assert_eq!(
            manager
                .replace("project:agents", &missing.revision, "stale")
                .unwrap_err(),
            MemoryManagerError::StaleRevision
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("AGENTS.md")).unwrap(),
            "project law"
        );

        let auto = manager.read("auto:user:reviewer").unwrap();
        let updated = manager
            .replace("auto:user:reviewer", &auto.revision, "new notes")
            .unwrap();
        assert!(!updated.created);
        assert!(!updated.prompt_changed);
        assert_eq!(
            manager.read("auto:user:reviewer").unwrap().text,
            "new notes"
        );
    }

    #[test]
    fn workspace_trust_change_removes_project_and_local_sources_and_stales_old_edits() {
        let home = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let authority = Arc::new(FixedAuthority::new(
            Some(home.path().to_path_buf()),
            Some(first.path().to_path_buf()),
        ));
        let manager = MemorySourceManager::new(authority.clone());
        let old = manager.read("project:agents").unwrap();

        authority.set_workspace(Some(second.path().to_path_buf()));
        assert_eq!(
            manager
                .replace("project:agents", &old.revision, "wrong project")
                .unwrap_err(),
            MemoryManagerError::StaleRevision
        );
        assert!(!second.path().join("AGENTS.md").exists());

        authority.set_workspace(None);
        let snapshot = manager.snapshot().unwrap();
        assert!(
            snapshot
                .sources
                .iter()
                .all(|source| source.scope() == MemorySourceScope::User)
        );
        assert_eq!(
            manager.read("project:agents").unwrap_err(),
            MemoryManagerError::UnknownSource
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_auto_memory_and_instruction_symlinks_are_visible_but_never_read() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("MEMORY.md"), "outside secret");
        std::fs::create_dir_all(home.path().join(".agent-memory/user")).unwrap();
        std::os::unix::fs::symlink(
            outside.path(),
            home.path().join(".agent-memory/user/reviewer"),
        )
        .unwrap();
        write(&outside.path().join("AGENTS.md"), "outside instruction");
        std::os::unix::fs::symlink(
            outside.path().join("AGENTS.md"),
            project.path().join("AGENTS.md"),
        )
        .unwrap();
        let manager = MemorySourceManager::new(Arc::new(FixedAuthority::new(
            Some(home.path().to_path_buf()),
            Some(project.path().to_path_buf()),
        )));
        let snapshot = manager.snapshot().unwrap();
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("auto:user:reviewer") && warning.contains("unsafe"))
        );
        let project_row = snapshot
            .sources
            .iter()
            .find(|source| source.id() == "project:agents")
            .unwrap();
        assert!(matches!(
            project_row.status(),
            MemorySourceStatus::Blocked { .. }
        ));
        assert_eq!(
            manager.read("project:agents").unwrap_err(),
            MemoryManagerError::UnsafeSource
        );
        assert_eq!(
            manager.read("auto:user:reviewer").unwrap_err(),
            MemoryManagerError::UnknownSource
        );
    }

    #[test]
    fn command_metadata_is_queued_source_owned_and_command_output_is_bounded() {
        let home = tempfile::tempdir().unwrap();
        let manager = Arc::new(MemorySourceManager::new(Arc::new(FixedAuthority::new(
            Some(home.path().to_path_buf()),
            None,
        ))));
        let context = heycode_core::compose(&[heycode_agent::commands_plugin()]).unwrap();
        let commands = context
            .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
            .unwrap();
        register_memory_command(&context, &commands, manager.clone()).unwrap();
        let command = commands.get("memory").unwrap().unwrap();
        assert_eq!(command.descriptor().source().plugin(), "memory-commands");
        assert_eq!(command.descriptor().timing(), CommandTiming::Queued);
        assert_eq!(
            command.descriptor().synopsis(),
            "/memory [action] [arguments...]"
        );
        assert!(command.availability().is_available());
        let listing = execute_memory(&manager, "list").unwrap();
        assert!(listing.contains("user:agents"));
        assert!(listing.contains("rendered fresh on the next request"));
        let revision = manager.read("user:agents").unwrap().revision;
        assert!(!listing.contains(&revision));
        assert!(listing.contains("/memory show user:agents"));
        assert_eq!(
            execute_memory(&manager, "").unwrap_err(),
            MemoryManagerError::Usage
        );
        assert_eq!(
            execute_memory(&manager, "replace user:agents").unwrap_err(),
            MemoryManagerError::Usage
        );

        let oversized = "x".repeat(MAX_COMMAND_OUTPUT_CHARS + 100);
        let sanitized = sanitize_output(&oversized);
        assert_eq!(sanitized.chars().count(), MAX_COMMAND_OUTPUT_CHARS);
        assert!(sanitized.ends_with("[output truncated by /memory]"));
    }

    #[test]
    fn discovery_caps_sources_and_warnings() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join(".agent-memory/user");
        for index in 0..(MAX_MEMORY_SOURCES + MAX_DISCOVERY_WARNINGS + 10) {
            std::fs::create_dir_all(base.join(format!("a-preset-{index:03}"))).unwrap();
        }
        for index in 0..(MAX_DISCOVERY_WARNINGS + 10) {
            std::fs::create_dir_all(base.join(format!("z invalid id {index:03}"))).unwrap();
        }
        let manager = MemorySourceManager::new(Arc::new(FixedAuthority::new(
            Some(home.path().to_path_buf()),
            None,
        )));
        let snapshot = manager.snapshot().unwrap();
        assert_eq!(snapshot.sources.len(), MAX_MEMORY_SOURCES);
        assert!(snapshot.warnings.len() <= MAX_DISCOVERY_WARNINGS);
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("source limit reached"))
        );
    }
}

//! U15/CMD06 session picker state and lifecycle command bridge.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandDescriptor, CommandMetadataError, CommandSource, CommandTiming,
};
use heycode_session::{
    SessionActivityStatus, SessionCursor, SessionExportFormat, SessionFilter, SessionLineageFilter,
    SessionPage, SessionQuery, SessionQueryError, SessionQueryService, SessionStorageFilter,
    SessionSummary, SessionTitle,
};

const SESSION_PAGE_SIZE: usize = 10;
const MAX_SEARCH_CHARS: usize = 128;

/// Human-readable age of a durable session's last recorded activity, in the
/// source's `1 second ago` / `3 minutes ago` shape. A session whose store never
/// recorded an activity time says so instead of inventing "just now".
#[must_use]
pub(crate) fn relative_age(last_activity_ms: Option<i64>) -> String {
    let Some(recorded) = last_activity_ms else {
        return "unknown age".to_owned();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0));
    let Some(seconds) = now.checked_sub(recorded).filter(|gap| *gap >= 0) else {
        return "unknown age".to_owned();
    };
    let seconds = seconds / 1000;
    let (count, unit) = if seconds < 60 {
        (seconds, "second")
    } else if seconds < 3600 {
        (seconds / 60, "minute")
    } else if seconds < 86_400 {
        (seconds / 3600, "hour")
    } else {
        (seconds / 86_400, "day")
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

/// Recovery guidance for a committed conversation branch. Carry this UI notice
/// across composition; it is not a message in the model's conversation.
#[must_use]
pub fn branch_receipt(
    session_id: &heycode_core::SessionId,
    parent_session_id: &heycode_core::SessionId,
    title: Option<&str>,
    parent_title: Option<&str>,
) -> String {
    let name = title.map_or_else(String::new, |title| format!(" {title:?}"));
    let original = parent_title.map_or_else(String::new, |title| format!(" ({title:?})"));
    format!(
        "Branched conversation{name}. You are now in the new branch (session {session_id}). Use /resume {parent_session_id}{original} to return to the original, or run heycode --resume {parent_session_id} in a new terminal."
    )
}

/// Typed session-world request queued by a slash command.
#[derive(Clone, PartialEq, Eq)]
pub enum SessionCommandRequest {
    /// Open the bounded session browser.
    Browse,
    /// Create a fresh durable session, optionally titled.
    New(Option<SessionTitle>),
    /// Validate and select one existing session.
    Resume(heycode_core::SessionId),
    /// Resolve an exact current title; duplicates require a browser choice.
    ResumeSearch(String),
    /// Fork this saved id, or the current session when absent.
    Fork(Option<heycode_core::SessionId>),
    /// Fork an explicit source or the current conversation, then title the child.
    Branch {
        /// Optional explicit saved-session source.
        session_id: Option<heycode_core::SessionId>,
        /// Optional title committed before the shell switches to the child.
        title: Option<SessionTitle>,
    },
    /// Open a snapshot of this session's durable rewind boundaries.
    RewindPicker(crate::rewind_picker::RewindPickerRequest),
    /// Copy the conversation into a separately hosted background session.
    #[cfg(unix)]
    BackgroundFork(heycode_session::background::ForkOptions),
    /// Rename the current session through its live durable owner.
    Rename(Option<SessionTitle>),
    /// Archive this saved id; absence opens the browser.
    Archive(Option<heycode_core::SessionId>),
    /// Delete this saved id after confirmation; absence opens the browser.
    Delete(Option<heycode_core::SessionId>),
    /// Export the selected id, or the current session when absent.
    Export {
        /// Optional saved-session target.
        session_id: Option<heycode_core::SessionId>,
        /// Exact lower-service representation.
        format: SessionExportFormat,
    },
    /// Export the current rendered conversation as plain text. Absence opens
    /// the clipboard/file chooser; a destination writes directly.
    PlainTextExport {
        /// Explicit user destination after command parsing, before `.txt`
        /// normalization or filesystem authority resolution.
        destination: Option<String>,
    },
}

#[derive(Default)]
struct BridgeState {
    pending: Option<SessionCommandRequest>,
}

/// Single-command inbox shared by lifecycle slash commands and the shell.
#[derive(Clone, Default)]
pub struct SessionCommandBridge {
    state: Arc<Mutex<BridgeState>>,
}

impl SessionCommandBridge {
    /// Empty bridge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the one pending request.
    pub fn request(&self, request: SessionCommandRequest) {
        self.lock().pending = Some(request);
    }

    /// Drain the pending request.
    #[must_use]
    pub fn take(&self) -> Option<SessionCommandRequest> {
        self.lock().pending.take()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BridgeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl std::fmt::Debug for SessionCommandBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionCommandBridge")
            .field("pending", &self.lock().pending.is_some())
            .finish()
    }
}

#[derive(Clone, Copy)]
enum SessionCommandKind {
    New,
    Resume,
    Fork,
    Rename,
    Archive,
    Delete,
    Export,
}

struct SessionCommand {
    descriptor: CommandDescriptor,
    kind: SessionCommandKind,
    bridge: SessionCommandBridge,
}

#[async_trait]
impl Command for SessionCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let request = parse_command(self.kind, args)?;
        self.bridge.request(request);
        Ok(())
    }
}

/// Build all seven lifecycle commands against one shell-owned bridge.
///
/// # Errors
/// Invalid command metadata fails composition.
pub fn session_commands(
    source: CommandSource,
    bridge: SessionCommandBridge,
) -> Result<Vec<Arc<dyn Command>>, CommandMetadataError> {
    let rows = [
        (
            SessionCommandKind::New,
            "new",
            "Create a new durable session",
            vec![CommandArgument::optional("title", "Optional session title")?.variadic()],
        ),
        (
            SessionCommandKind::Resume,
            "resume",
            "Browse or resume a saved session",
            vec![
                CommandArgument::optional(
                    "session-or-search",
                    "Exact title, session UUID, or --session <id>; omit to search in the browser",
                )?
                .variadic(),
            ],
        ),
        (
            SessionCommandKind::Fork,
            "branch",
            "Fork and switch to a conversation branch",
            vec![
                CommandArgument::optional(
                    "name",
                    "Branch name; a UUID or --session <id> [name] selects a saved source",
                )?
                .variadic(),
            ],
        ),
        (
            SessionCommandKind::Rename,
            "rename",
            "Rename the current session",
            vec![
                CommandArgument::optional("title", "New title; omitted derives one locally")?
                    .variadic(),
            ],
        ),
        (
            SessionCommandKind::Archive,
            "archive",
            "Archive a saved session recoverably",
            vec![CommandArgument::optional("session", "Saved session id")?],
        ),
        (
            SessionCommandKind::Delete,
            "delete",
            "Delete a saved session after confirmation",
            vec![CommandArgument::optional("session", "Saved session id")?],
        ),
        (
            SessionCommandKind::Export,
            "export",
            "Copy or save this conversation, or export a durable session",
            vec![
                CommandArgument::optional(
                    "target",
                    "file <path>, a plain-text filename, session id, or jsonl/markdown/support",
                )?,
                CommandArgument::optional(
                    "format-or-path",
                    "Saved-session format, or the rest of a file destination",
                )?
                .variadic(),
            ],
        ),
    ];
    rows.into_iter()
        .map(|(kind, id, description, arguments)| {
            Ok(Arc::new(SessionCommand {
                descriptor: CommandDescriptor::new(
                    id,
                    description,
                    arguments,
                    CommandTiming::Queued,
                    source.clone(),
                )?,
                kind,
                bridge: bridge.clone(),
            }) as Arc<dyn Command>)
        })
        .collect()
}

fn parse_command(kind: SessionCommandKind, args: &str) -> anyhow::Result<SessionCommandRequest> {
    let args = args.trim();
    match kind {
        SessionCommandKind::New => Ok(SessionCommandRequest::New(
            (!args.is_empty())
                .then(|| SessionTitle::new(args))
                .transpose()
                .map_err(|_| anyhow::anyhow!("invalid session title"))?,
        )),
        SessionCommandKind::Resume => {
            if args.is_empty() {
                Ok(SessionCommandRequest::Browse)
            } else if let Some(id) = explicit_session_args(args) {
                Ok(SessionCommandRequest::Resume(parse_session_id(id)?))
            } else if uuid::Uuid::parse_str(args).is_ok() {
                Ok(SessionCommandRequest::Resume(parse_session_id(args)?))
            } else {
                SessionFilter::new().with_exact_title(args)?;
                Ok(SessionCommandRequest::ResumeSearch(args.to_owned()))
            }
        }
        SessionCommandKind::Fork => parse_branch(args),
        SessionCommandKind::Rename => Ok(SessionCommandRequest::Rename(
            (!args.is_empty())
                .then(|| SessionTitle::from_input(args))
                .transpose()
                .map_err(|_| anyhow::anyhow!("invalid session title"))?,
        )),
        SessionCommandKind::Archive => Ok(if args.is_empty() {
            SessionCommandRequest::Browse
        } else {
            SessionCommandRequest::Archive(Some(parse_session_id(args)?))
        }),
        SessionCommandKind::Delete => Ok(if args.is_empty() {
            SessionCommandRequest::Browse
        } else {
            SessionCommandRequest::Delete(Some(parse_session_id(args)?))
        }),
        SessionCommandKind::Export => parse_export(args),
    }
}

fn explicit_session_args(args: &str) -> Option<&str> {
    args.strip_prefix("--session").and_then(|tail| {
        (tail.is_empty() || tail.starts_with(char::is_whitespace)).then(|| tail.trim())
    })
}

fn validate_search(search: &str) -> Result<(), SessionQueryError> {
    if search.chars().count() > MAX_SEARCH_CHARS || search.chars().any(char::is_control) {
        return Err(SessionQueryError::InvalidQuery);
    }
    Ok(())
}

fn parse_branch(args: &str) -> anyhow::Result<SessionCommandRequest> {
    let (session_id, name) = if let Some(tail) = explicit_session_args(args) {
        let (id, name) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
        (Some(parse_session_id(id)?), name.trim())
    } else if uuid::Uuid::parse_str(args).is_ok() {
        (Some(parse_session_id(args)?), "")
    } else {
        (None, args)
    };
    Ok(SessionCommandRequest::Branch {
        session_id,
        title: (!name.is_empty())
            .then(|| SessionTitle::from_input(name))
            .transpose()?,
    })
}

fn parse_export(args: &str) -> anyhow::Result<SessionCommandRequest> {
    if args.is_empty() {
        return Ok(SessionCommandRequest::PlainTextExport { destination: None });
    }
    anyhow::ensure!(args != "file", "usage: /export file <path>");
    if let Some(destination) = args.strip_prefix("file").and_then(|suffix| {
        suffix
            .chars()
            .next()
            .filter(|character| character.is_whitespace())
            .map(|_| suffix.trim())
    }) {
        anyhow::ensure!(!destination.is_empty(), "usage: /export file <path>");
        return Ok(SessionCommandRequest::PlainTextExport {
            destination: Some(destination.to_owned()),
        });
    }
    let parts = args.split_whitespace().collect::<Vec<_>>();
    let (session_id, format) = match parts.as_slice() {
        [format] if matches!(*format, "jsonl" | "markdown" | "support") => {
            (None, parse_export_format(format)?)
        }
        [target] if looks_like_plain_text_destination(target) => {
            return Ok(SessionCommandRequest::PlainTextExport {
                destination: Some((*target).to_owned()),
            });
        }
        [id] => (
            Some(parse_session_id(id)?),
            SessionExportFormat::LosslessJsonl,
        ),
        [id, format] => (Some(parse_session_id(id)?), parse_export_format(format)?),
        _ => anyhow::bail!(
            "usage: /export [file <path>|filename|jsonl|markdown|support|session [format]]"
        ),
    };
    Ok(SessionCommandRequest::Export { session_id, format })
}

fn looks_like_plain_text_destination(value: &str) -> bool {
    let path = std::path::Path::new(value);
    value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || path.extension().is_some()
        || parse_session_id(value).is_err()
}

fn parse_export_format(value: &str) -> anyhow::Result<SessionExportFormat> {
    match value {
        "jsonl" => Ok(SessionExportFormat::LosslessJsonl),
        "markdown" => Ok(SessionExportFormat::Markdown),
        "support" => Ok(SessionExportFormat::RedactedSupport),
        _ => anyhow::bail!("usage: /export <session> [jsonl|markdown|support]"),
    }
}

fn parse_session_id(value: &str) -> anyhow::Result<heycode_core::SessionId> {
    if value.split_whitespace().count() != 1 {
        anyhow::bail!("invalid session id");
    }
    let id = heycode_core::SessionId::from_raw(value);
    heycode_session::SessionFilter::new()
        .with_parent(id.clone())
        .map_err(|_| anyhow::anyhow!("invalid session id"))?;
    Ok(id)
}

/// Active/archived browser filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStorageView {
    /// Ordinary active sessions.
    Active,
    /// Archived sessions.
    Archived,
    /// Both states.
    All,
}

impl SessionStorageView {
    fn query(self) -> SessionStorageFilter {
        match self {
            Self::Active => SessionStorageFilter::Active,
            Self::Archived => SessionStorageFilter::Archived,
            Self::All => SessionStorageFilter::All,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::All => "all",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Active => Self::Archived,
            Self::Archived => Self::All,
            Self::All => Self::Active,
        }
    }
}

/// Root/fork browser filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLineageView {
    /// Roots and forks.
    All,
    /// Root sessions only.
    Roots,
    /// Fork sessions only.
    Forks,
}

impl SessionLineageView {
    fn query(self) -> SessionLineageFilter {
        match self {
            Self::All => SessionLineageFilter::All,
            Self::Roots => SessionLineageFilter::Roots,
            Self::Forks => SessionLineageFilter::Forks,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::All => "all lineage",
            Self::Roots => "roots",
            Self::Forks => "forks",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Roots,
            Self::Roots => Self::Forks,
            Self::Forks => Self::All,
        }
    }
}

/// One picker row with markers relative to the running shell.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionBrowserRow {
    summary: SessionSummary,
    current: bool,
    latest: bool,
}

impl SessionBrowserRow {
    /// Safe durable summary.
    #[must_use]
    pub const fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    /// Whether this is the composed current session.
    #[must_use]
    pub const fn is_current(&self) -> bool {
        self.current
    }

    /// Whether this is the latest active session at refresh time.
    #[must_use]
    pub const fn is_latest(&self) -> bool {
        self.latest
    }
}

/// Delete dialog choice. Cancel is the construction default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDeleteChoice {
    /// Leave durable storage unchanged.
    Cancel,
    /// Execute the lower service's safety-checked trash move.
    Confirm,
}

/// Cancel-default destructive confirmation.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionDeleteConfirmation {
    session_id: heycode_core::SessionId,
    choice: SessionDeleteChoice,
}

impl SessionDeleteConfirmation {
    fn new(session_id: heycode_core::SessionId) -> Self {
        Self {
            session_id,
            choice: SessionDeleteChoice::Cancel,
        }
    }

    /// Exact target id.
    #[must_use]
    pub const fn session_id(&self) -> &heycode_core::SessionId {
        &self.session_id
    }

    /// Highlighted safe choice.
    #[must_use]
    pub const fn choice(&self) -> SessionDeleteChoice {
        self.choice
    }
}

pub(crate) enum SessionBrowserAction {
    None,
    Refresh,
    Resume(heycode_core::SessionId),
    Fork(heycode_core::SessionId),
    New,
    Rename(heycode_core::SessionId, SessionTitle),
    ToggleArchive(heycode_core::SessionId),
    Delete(heycode_core::SessionId),
    Export(heycode_core::SessionId, SessionExportFormat),
    Close,
}

/// Bounded deterministic session browser state.
pub struct SessionBrowserView {
    service: Arc<SessionQueryService>,
    current_id: heycode_core::SessionId,
    current_cwd: std::path::PathBuf,
    current_runtime: Option<String>,
    rows: Vec<SessionBrowserRow>,
    selected: usize,
    total_matches: usize,
    page_cursors: Vec<SessionCursor>,
    next_cursor: Option<SessionCursor>,
    storage: SessionStorageView,
    lineage: SessionLineageView,
    status: Option<SessionActivityStatus>,
    source: Option<heycode_session::SessionSource>,
    current_cwd_only: bool,
    current_runtime_only: bool,
    search: String,
    exact_title: Option<String>,
    search_active: bool,
    error: Option<SessionQueryError>,
    notice: Option<String>,
    delete_confirmation: Option<SessionDeleteConfirmation>,
    rename: Option<(heycode_core::SessionId, String)>,
}

impl SessionBrowserView {
    pub(crate) fn new(
        service: Arc<SessionQueryService>,
        current_id: heycode_core::SessionId,
        current_cwd: std::path::PathBuf,
        current_runtime: Option<String>,
    ) -> Self {
        let mut view = Self {
            service,
            current_id,
            current_cwd,
            current_runtime,
            rows: Vec::new(),
            selected: 0,
            total_matches: 0,
            page_cursors: Vec::new(),
            next_cursor: None,
            storage: SessionStorageView::Active,
            lineage: SessionLineageView::All,
            status: None,
            source: None,
            current_cwd_only: true,
            current_runtime_only: false,
            search: String::new(),
            exact_title: None,
            search_active: false,
            error: None,
            notice: None,
            delete_confirmation: None,
            rename: None,
        };
        view.refresh();
        view
    }

    /// Current page rows.
    #[must_use]
    pub fn rows(&self) -> &[SessionBrowserRow] {
        &self.rows
    }

    /// Selected row; absent on empty/error pages.
    #[must_use]
    pub fn selected(&self) -> Option<&SessionBrowserRow> {
        self.rows.get(self.selected)
    }

    /// Zero-based keyset page number visited by this browser.
    #[must_use]
    pub fn page_index(&self) -> usize {
        self.page_cursors.len()
    }

    /// Matches in the deterministic scan.
    #[must_use]
    pub const fn total_matches(&self) -> usize {
        self.total_matches
    }

    /// Visible fixed lower-service error, when any.
    #[must_use]
    pub const fn error(&self) -> Option<SessionQueryError> {
        self.error
    }

    /// Last committed action notice.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Cancel-default destructive dialog.
    #[must_use]
    pub const fn delete_confirmation(&self) -> Option<&SessionDeleteConfirmation> {
        self.delete_confirmation.as_ref()
    }

    /// Set active/archive visibility and reset keyset history.
    pub fn set_storage(&mut self, storage: SessionStorageView) {
        self.storage = storage;
        self.reset_page();
    }

    /// Show only shared-prefix forks.
    pub fn show_forks_only(&mut self) {
        self.lineage = SessionLineageView::Forks;
        self.reset_page();
    }

    /// Show roots and forks.
    pub fn show_all_lineage(&mut self) {
        self.lineage = SessionLineageView::All;
        self.reset_page();
    }

    pub(crate) fn storage(&self) -> SessionStorageView {
        self.storage
    }

    pub(crate) fn lineage(&self) -> SessionLineageView {
        self.lineage
    }

    pub(crate) fn status(&self) -> Option<SessionActivityStatus> {
        self.status
    }

    pub(crate) fn source(&self) -> Option<heycode_session::SessionSource> {
        self.source
    }

    pub(crate) fn current_cwd_only(&self) -> bool {
        self.current_cwd_only
    }

    pub(crate) fn current_runtime_only(&self) -> bool {
        self.current_runtime_only
    }

    /// Workspace label for a group, repeated when a keyset page starts mid-group.
    #[must_use]
    pub fn workspace_group(&self, index: usize) -> Option<String> {
        let row = self.rows.get(index)?;
        let cwd = row.summary.cwd();
        if index > 0 && self.rows[index - 1].summary.cwd() == cwd {
            return None;
        }
        Some(cwd.map_or_else(
            || "Unknown workspace".to_owned(),
            |cwd| {
                if self.current_cwd_only {
                    cwd.file_name().map_or_else(
                        || cwd.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    )
                } else {
                    cwd.display().to_string()
                }
            },
        ))
    }

    pub(crate) fn filters_active(&self) -> bool {
        self.storage != SessionStorageView::Active
            || self.lineage != SessionLineageView::All
            || self.status.is_some()
            || self.source.is_some()
            || self.current_runtime_only
    }

    pub(crate) fn hint(&self) -> String {
        let scope = if self.current_cwd_only {
            "all projects"
        } else {
            "this project"
        };
        if self.search_active {
            format!(
                "  Ctrl+A to show {scope} · Ctrl+R to rename · Type to search · ↑↓ choose · Enter resume · Esc actions"
            )
        } else {
            format!(
                "  Ctrl+A to show {scope} · Ctrl+R to rename · / search · ↑↓ choose · Enter resume · Esc to cancel"
            )
        }
    }

    pub(crate) fn start_search(&mut self) {
        self.search_active = true;
        if self.exact_title.take().is_some() {
            self.reset_page();
            self.refresh();
        }
    }

    /// Prefill a bounded search, refresh its first page, and focus its input.
    ///
    /// # Errors
    /// Control characters and text exceeding the browser input bound are rejected
    /// without changing the current query or selection.
    pub fn set_search(&mut self, search: &str) -> Result<(), SessionQueryError> {
        let search = search.trim();
        validate_search(search)?;
        self.search = search.to_owned();
        self.start_search();
        self.reset_page();
        self.refresh();
        Ok(())
    }

    /// Resolve only the current exact title across the owner's complete scan.
    pub(crate) fn set_exact_title(&mut self, title: &str) -> Result<(), SessionQueryError> {
        SessionFilter::new().with_exact_title(title)?;
        self.exact_title = Some(title.to_owned());
        self.search.clear();
        self.search_active = false;
        self.reset_page();
        self.refresh();
        Ok(())
    }

    pub(crate) fn search(&self) -> &str {
        &self.search
    }

    pub(crate) fn search_active(&self) -> bool {
        self.search_active
    }

    pub(crate) fn rename_input(&self) -> Option<&str> {
        self.rename.as_ref().map(|(_, value)| value.as_str())
    }

    pub(crate) fn has_next_page(&self) -> bool {
        self.next_cursor.is_some()
    }

    pub(crate) fn refresh(&mut self) {
        let mut filter = SessionFilter::new()
            .with_used_only()
            .with_storage(self.storage.query())
            .with_lineage(self.lineage.query());
        // An explicit exact title still resolves the current session (and detects
        // duplicate titles); the browse/search surface only offers other sessions.
        if self.exact_title.is_none() {
            filter = filter.excluding_session(self.current_id.clone());
        }
        if let Some(status) = self.status {
            filter = filter.with_status(status);
        }
        if let Some(source) = self.source {
            filter = filter.with_source(source);
        }
        if self.current_cwd_only {
            match filter.with_cwd(self.current_cwd.clone()) {
                Ok(next) => filter = next,
                Err(error) => {
                    self.apply_error(error);
                    return;
                }
            }
        }
        if self.current_runtime_only
            && let Some(runtime) = self.current_runtime.clone()
        {
            match filter.with_runtime(runtime) {
                Ok(next) => filter = next,
                Err(error) => {
                    self.apply_error(error);
                    return;
                }
            }
        }
        if let Some(title) = self.exact_title.clone() {
            match filter.with_exact_title(title) {
                Ok(next) => filter = next,
                Err(error) => {
                    self.apply_error(error);
                    return;
                }
            }
        }
        if !self.search.is_empty() {
            match filter.with_text(self.search.clone()) {
                Ok(next) => filter = next,
                Err(error) => {
                    self.apply_error(error);
                    return;
                }
            }
        }
        let latest = self
            .service
            .latest(&filter)
            .ok()
            .flatten()
            .map(|summary| summary.id().clone());
        let mut query = match SessionQuery::new(filter, SESSION_PAGE_SIZE) {
            Ok(query) => query,
            Err(error) => {
                self.apply_error(error);
                return;
            }
        };
        if let Some(cursor) = self.page_cursors.last().cloned() {
            query = query.with_cursor(cursor);
        }
        match self.service.query(&query) {
            Ok(page) => self.apply_page(page, latest.as_ref()),
            Err(error) => self.apply_error(error),
        }
    }

    fn apply_page(&mut self, page: SessionPage, latest: Option<&heycode_core::SessionId>) {
        self.rows = page
            .items()
            .iter()
            .cloned()
            .map(|summary| SessionBrowserRow {
                current: summary.id() == &self.current_id,
                latest: latest == Some(summary.id()),
                summary,
            })
            .collect();
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.total_matches = page.total_matches();
        self.next_cursor = page.next_cursor().cloned();
        self.error = None;
    }

    fn apply_error(&mut self, error: SessionQueryError) {
        self.rows.clear();
        self.selected = 0;
        self.total_matches = 0;
        self.next_cursor = None;
        self.error = Some(error);
    }

    fn reset_page(&mut self) {
        self.page_cursors.clear();
        self.selected = 0;
    }

    pub(crate) fn begin_delete(&mut self, id: heycode_core::SessionId) {
        self.delete_confirmation = Some(SessionDeleteConfirmation::new(id));
    }

    pub(crate) fn set_notice(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub(crate) fn handle_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> SessionBrowserAction {
        use crossterm::event::KeyCode;
        if let Some(confirmation) = self.delete_confirmation.as_mut() {
            return match code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    confirmation.choice = match confirmation.choice {
                        SessionDeleteChoice::Cancel => SessionDeleteChoice::Confirm,
                        SessionDeleteChoice::Confirm => SessionDeleteChoice::Cancel,
                    };
                    SessionBrowserAction::None
                }
                KeyCode::Esc => {
                    self.delete_confirmation = None;
                    SessionBrowserAction::None
                }
                KeyCode::Enter => {
                    let confirmation = self.delete_confirmation.take();
                    match confirmation {
                        Some(confirmation)
                            if confirmation.choice == SessionDeleteChoice::Confirm =>
                        {
                            SessionBrowserAction::Delete(confirmation.session_id)
                        }
                        _ => SessionBrowserAction::None,
                    }
                }
                _ => SessionBrowserAction::None,
            };
        }
        if let Some((id, input)) = self.rename.as_mut() {
            return match code {
                KeyCode::Esc => {
                    self.rename = None;
                    SessionBrowserAction::None
                }
                KeyCode::Backspace => {
                    input.pop();
                    SessionBrowserAction::None
                }
                KeyCode::Char(character) if input.chars().count() < 160 => {
                    input.push(character);
                    SessionBrowserAction::None
                }
                KeyCode::Enter => match SessionTitle::from_input(input) {
                    Ok(title) => {
                        let id = id.clone();
                        self.rename = None;
                        SessionBrowserAction::Rename(id, title)
                    }
                    Err(error) => {
                        self.error = Some(error);
                        SessionBrowserAction::None
                    }
                },
                _ => SessionBrowserAction::None,
            };
        }
        if modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
            return match code {
                KeyCode::Char('a') => {
                    self.current_cwd_only = !self.current_cwd_only;
                    self.reset_page();
                    SessionBrowserAction::Refresh
                }
                KeyCode::Char('r') => {
                    if let Some(row) = self.selected() {
                        self.rename = Some((
                            row.summary.id().clone(),
                            row.summary.title().unwrap_or_default().to_owned(),
                        ));
                    }
                    SessionBrowserAction::None
                }
                _ => SessionBrowserAction::None,
            };
        }
        if self.search_active {
            return match code {
                KeyCode::Esc => {
                    self.search_active = false;
                    SessionBrowserAction::None
                }
                KeyCode::Enter => self.selected().map_or(SessionBrowserAction::None, |row| {
                    SessionBrowserAction::Resume(row.summary.id().clone())
                }),
                KeyCode::Up | KeyCode::Down => {
                    if !self.rows.is_empty() {
                        self.selected = if code == KeyCode::Up {
                            self.selected.checked_sub(1).unwrap_or(self.rows.len() - 1)
                        } else {
                            (self.selected + 1) % self.rows.len()
                        };
                    }
                    SessionBrowserAction::None
                }
                KeyCode::PageDown if self.next_cursor.is_some() => {
                    if let Some(cursor) = self.next_cursor.clone() {
                        self.page_cursors.push(cursor);
                    }
                    self.selected = 0;
                    SessionBrowserAction::Refresh
                }
                KeyCode::PageUp if !self.page_cursors.is_empty() => {
                    self.page_cursors.pop();
                    self.selected = 0;
                    SessionBrowserAction::Refresh
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.reset_page();
                    SessionBrowserAction::Refresh
                }
                KeyCode::Char(character) if self.search.chars().count() < MAX_SEARCH_CHARS => {
                    self.search.push(character);
                    self.reset_page();
                    SessionBrowserAction::Refresh
                }
                _ => SessionBrowserAction::None,
            };
        }
        match code {
            KeyCode::Esc => SessionBrowserAction::Close,
            KeyCode::Up if !self.rows.is_empty() => {
                self.selected = self.selected.checked_sub(1).unwrap_or(self.rows.len() - 1);
                SessionBrowserAction::None
            }
            KeyCode::Down if !self.rows.is_empty() => {
                self.selected = (self.selected + 1) % self.rows.len();
                SessionBrowserAction::None
            }
            KeyCode::PageDown => {
                if let Some(cursor) = self.next_cursor.clone() {
                    self.page_cursors.push(cursor);
                    self.selected = 0;
                    SessionBrowserAction::Refresh
                } else {
                    SessionBrowserAction::None
                }
            }
            KeyCode::PageUp => {
                if self.page_cursors.pop().is_some() {
                    self.selected = 0;
                    SessionBrowserAction::Refresh
                } else {
                    SessionBrowserAction::None
                }
            }
            KeyCode::Char('/') => {
                self.search_active = true;
                SessionBrowserAction::None
            }
            KeyCode::Char('s') => {
                self.storage = self.storage.next();
                self.reset_page();
                SessionBrowserAction::Refresh
            }
            KeyCode::Char('l') => {
                self.lineage = self.lineage.next();
                self.reset_page();
                SessionBrowserAction::Refresh
            }
            KeyCode::Char('w') => {
                self.current_cwd_only = !self.current_cwd_only;
                self.reset_page();
                SessionBrowserAction::Refresh
            }
            KeyCode::Char('v') => {
                self.current_runtime_only = !self.current_runtime_only;
                self.reset_page();
                SessionBrowserAction::Refresh
            }
            KeyCode::Char('t') => {
                self.status = match self.status {
                    None => Some(SessionActivityStatus::Idle),
                    Some(SessionActivityStatus::Idle) => Some(SessionActivityStatus::OpenTurn),
                    Some(SessionActivityStatus::OpenTurn) => Some(SessionActivityStatus::Empty),
                    Some(SessionActivityStatus::Empty) => None,
                };
                self.reset_page();
                SessionBrowserAction::Refresh
            }
            KeyCode::Char('c') => {
                self.source = next_source(self.source);
                self.reset_page();
                SessionBrowserAction::Refresh
            }
            KeyCode::Enter if !self.rows.is_empty() => {
                SessionBrowserAction::Resume(self.rows[self.selected].summary.id().clone())
            }
            KeyCode::Char('n') => SessionBrowserAction::New,
            KeyCode::Char('f') if !self.rows.is_empty() => {
                SessionBrowserAction::Fork(self.rows[self.selected].summary.id().clone())
            }
            KeyCode::Char('r') if !self.rows.is_empty() => {
                let row = &self.rows[self.selected];
                self.rename = Some((
                    row.summary.id().clone(),
                    row.summary.title().unwrap_or_default().to_owned(),
                ));
                SessionBrowserAction::None
            }
            KeyCode::Char('a') if !self.rows.is_empty() => {
                SessionBrowserAction::ToggleArchive(self.rows[self.selected].summary.id().clone())
            }
            KeyCode::Char('d') if !self.rows.is_empty() => {
                self.begin_delete(self.rows[self.selected].summary.id().clone());
                SessionBrowserAction::None
            }
            KeyCode::Char('e') if !self.rows.is_empty() => SessionBrowserAction::Export(
                self.rows[self.selected].summary.id().clone(),
                SessionExportFormat::LosslessJsonl,
            ),
            _ => SessionBrowserAction::None,
        }
    }
}

fn next_source(
    source: Option<heycode_session::SessionSource>,
) -> Option<heycode_session::SessionSource> {
    use heycode_session::SessionSource;
    match source {
        None => Some(SessionSource::Interactive),
        Some(SessionSource::Interactive) => Some(SessionSource::Headless),
        Some(SessionSource::Headless) => Some(SessionSource::Acp),
        Some(SessionSource::Acp) => Some(SessionSource::Subagent),
        Some(SessionSource::Subagent) => Some(SessionSource::Scheduled),
        Some(SessionSource::Scheduled) => Some(SessionSource::Delegated),
        Some(SessionSource::Delegated) => Some(SessionSource::Fork),
        Some(SessionSource::Fork) => None,
    }
}

/// UI-neutral descriptor for the TUI-owned session panel.
///
/// # Errors
/// Static metadata drift is returned to plugin composition.
pub fn panel_descriptor()
-> Result<heycode_ui::UiContributionDescriptor, heycode_ui::UiRegistryError> {
    heycode_ui::UiContributionDescriptor::new(
        heycode_ui::UiSlot::Panel,
        "sessions",
        "Session browser",
        90,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn lifecycle_command_set_is_closed_complete_and_queued() {
        let commands = session_commands(
            CommandSource::from_plugin("tui").unwrap(),
            SessionCommandBridge::new(),
        )
        .unwrap();
        assert_eq!(
            commands
                .iter()
                .map(|command| command.descriptor().id())
                .collect::<Vec<_>>(),
            [
                "new", "resume", "branch", "rename", "archive", "delete", "export"
            ]
        );
        assert!(
            commands
                .iter()
                .all(|command| command.descriptor().timing() == CommandTiming::Queued)
        );
    }

    #[test]
    fn branch_accepts_names_and_keeps_explicit_saved_sources() {
        assert!(
            matches!(parse_command(SessionCommandKind::Fork, "Fix login  redirects").unwrap(),
            SessionCommandRequest::Branch { session_id: None, title: Some(title) }
            if title.as_str() == "Fix login  redirects")
        );
        assert!(
            matches!(parse_command(SessionCommandKind::Fork, "--session saved-id Child name").unwrap(),
            SessionCommandRequest::Branch { session_id: Some(id), title: Some(title) }
            if id.as_str() == "saved-id" && title.as_str() == "Child name")
        );
        assert!(matches!(
            parse_command(SessionCommandKind::Fork, "").unwrap(),
            SessionCommandRequest::Branch {
                session_id: None,
                title: None
            }
        ));
        assert!(parse_command(SessionCommandKind::Fork, "--session").is_err());
        assert!(parse_command(SessionCommandKind::Fork, "--session ../outside").is_err());
        assert!(parse_command(SessionCommandKind::Fork, "\u{1b}").is_err());
    }

    #[test]
    fn resume_distinguishes_exact_titles_from_ids_without_truncation() {
        assert!(
            matches!(parse_command(SessionCommandKind::Resume, "login redirect").unwrap(),
            SessionCommandRequest::ResumeSearch(search) if search == "login redirect")
        );
        let id = "6db1c0b0-876b-4f71-a0cd-e8ef663f4110";
        assert!(
            matches!(parse_command(SessionCommandKind::Resume, id).unwrap(),
            SessionCommandRequest::Resume(session) if session.as_str() == id)
        );
        assert!(
            matches!(parse_command(SessionCommandKind::Resume, "--session custom-id").unwrap(),
            SessionCommandRequest::Resume(session) if session.as_str() == "custom-id")
        );
        assert!(
            matches!(parse_command(SessionCommandKind::Fork, id).unwrap(),
            SessionCommandRequest::Branch { session_id: Some(session), title: None } if session.as_str() == id)
        );
        assert!(parse_command(SessionCommandKind::Resume, &"é".repeat(200)).is_ok());
        assert!(parse_command(SessionCommandKind::Resume, &"é".repeat(201)).is_err());
        assert!(parse_command(SessionCommandKind::Resume, "query\u{1b}control").is_err());
        assert!(parse_command(SessionCommandKind::Resume, "--session").is_err());
    }

    #[test]
    fn parsing_never_repeats_invalid_ids_or_titles() {
        assert!(parse_command(SessionCommandKind::Resume, "--session bad id").is_err());
        assert!(matches!(
            parse_command(SessionCommandKind::Rename, "").unwrap(),
            SessionCommandRequest::Rename(None)
        ));
        assert!(
            matches!(parse_command(SessionCommandKind::Rename, "  bad\u{1b}title  ").unwrap(), SessionCommandRequest::Rename(Some(title)) if title.as_str() == "badtitle")
        );
        assert!(parse_command(SessionCommandKind::Rename, "\u{1b}").is_err());
        assert!(matches!(
            parse_export("").unwrap(),
            SessionCommandRequest::PlainTextExport { destination: None }
        ));
        assert!(parse_export("markdown").is_ok());
        assert!(matches!(
            parse_export("support").unwrap(),
            SessionCommandRequest::Export {
                format: SessionExportFormat::RedactedSupport,
                ..
            }
        ));
        assert!(parse_export("safe-id markdown").is_ok());
        assert!(parse_export("safe-id unknown").is_err());
        assert!(matches!(
            parse_export("safe-id").unwrap(),
            SessionCommandRequest::Export {
                session_id: Some(id),
                format: SessionExportFormat::LosslessJsonl,
            } if id.as_str() == "safe-id"
        ));
        assert!(matches!(
            parse_export("fixture-export.txt").unwrap(),
            SessionCommandRequest::PlainTextExport {
                destination: Some(path),
            } if path == "fixture-export.txt"
        ));
        assert!(matches!(
            parse_export("nested/export").unwrap(),
            SessionCommandRequest::PlainTextExport {
                destination: Some(path),
            } if path == "nested/export"
        ));
        assert!(matches!(
            parse_export("file reports/my export").unwrap(),
            SessionCommandRequest::PlainTextExport {
                destination: Some(path),
            } if path == "reports/my export"
        ));
        assert!(matches!(
            parse_export("file markdown").unwrap(),
            SessionCommandRequest::PlainTextExport {
                destination: Some(path),
            } if path == "markdown"
        ));
        assert!(parse_export("file").is_err());
    }

    #[test]
    fn bridge_is_one_owned_request_not_an_unbounded_queue() {
        let bridge = SessionCommandBridge::new();
        bridge.request(SessionCommandRequest::Browse);
        bridge.request(SessionCommandRequest::New(None));
        assert!(matches!(
            bridge.take(),
            Some(SessionCommandRequest::New(None))
        ));
        assert!(bridge.take().is_none());
    }
}

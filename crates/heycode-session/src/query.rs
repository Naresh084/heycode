//! Replaceable bounded session discovery over JSONL truth.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use sha2::{Digest as _, Sha256};

use crate::stats::SessionStatsAccumulator;
use crate::{
    ForkBoundary, Session, SessionArchiveAction, SessionCreateRequest, SessionDeleteReceipt,
    SessionDeleteRequest, SessionEventKind, SessionExportFormat, SessionExportReceipt,
    SessionLineageFilter, SessionParent, SessionSource, SessionStatsSnapshot, SessionStorageFilter,
    SessionStorageState, SessionTitle,
};

const MAX_PAGE_SIZE: usize = 100;
const MAX_QUERY_TEXT_BYTES: usize = 256;
const MAX_SESSION_DIRECTORIES: usize = 10_000;
const MAX_QUERY_SCAN_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SAFE_TITLE_CHARS: usize = 200;

/// Activity state derived only from durable turn/work facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionActivityStatus {
    /// No admitted/pending work has been recorded.
    Empty,
    /// Work exists and no turn is currently open in the log.
    Idle,
    /// A turn/start has no matching turn/end in the selected log.
    OpenTurn,
}

/// Validated filters for one listing operation.
#[derive(Clone, Default)]
pub struct SessionFilter {
    text: Option<String>,
    exact_title: Option<String>,
    provider: Option<String>,
    runtime: Option<String>,
    source: Option<SessionSource>,
    status: Option<SessionActivityStatus>,
    cwd: Option<PathBuf>,
    parent: Option<heycode_core::SessionId>,
    storage: SessionStorageFilter,
    lineage: SessionLineageFilter,
    used_only: bool,
    excluded_session: Option<heycode_core::SessionId>,
}

impl SessionFilter {
    /// Empty filter matching every valid session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Exclude logs containing only startup housekeeping.
    #[must_use]
    pub fn with_used_only(mut self) -> Self {
        self.used_only = true;
        self
    }

    /// Exclude the composed session before counting and keyset pagination.
    #[must_use]
    pub fn excluding_session(mut self, id: heycode_core::SessionId) -> Self {
        self.excluded_session = Some(id);
        self
    }

    /// Match title, user/assistant text, pending input or session id.
    ///
    /// # Errors
    /// Text is blank, contains terminal controls or exceeds 256 bytes.
    pub fn with_text(mut self, value: impl Into<String>) -> Result<Self, SessionQueryError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.len() > MAX_QUERY_TEXT_BYTES
            || value.chars().any(unsafe_terminal_char)
        {
            return Err(SessionQueryError::InvalidQuery);
        }
        self.text = Some(value.to_lowercase());
        Ok(self)
    }

    /// Match the current title exactly, independently of inherited text and
    /// without the substring search byte limit.
    /// # Errors
    /// The title is blank, exceeds 200 Unicode characters or contains controls.
    pub fn with_exact_title(mut self, value: impl Into<String>) -> Result<Self, SessionQueryError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.chars().count() > MAX_SAFE_TITLE_CHARS
            || value.chars().any(unsafe_terminal_char)
        {
            return Err(SessionQueryError::InvalidQuery);
        }
        self.exact_title = Some(value);
        Ok(self)
    }

    /// Match one exact safe provider id.
    ///
    /// # Errors
    /// Provider id is not a bounded safe identifier.
    pub fn with_provider(mut self, value: impl Into<String>) -> Result<Self, SessionQueryError> {
        let value = value.into();
        if !safe_identifier(&value) {
            return Err(SessionQueryError::InvalidQuery);
        }
        self.provider = Some(value);
        Ok(self)
    }

    /// Match one exact runtime id.
    ///
    /// # Errors
    /// Runtime id is not bounded lowercase kebab-case.
    pub fn with_runtime(mut self, value: impl Into<String>) -> Result<Self, SessionQueryError> {
        let value = value.into();
        if !crate::creation::valid_runtime_id(&value) {
            return Err(SessionQueryError::InvalidQuery);
        }
        self.runtime = Some(value);
        Ok(self)
    }

    /// Match one creation source.
    #[must_use]
    pub fn with_source(mut self, source: SessionSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Match one durable activity state.
    #[must_use]
    pub fn with_status(mut self, status: SessionActivityStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Match one exact safe absolute cwd.
    ///
    /// # Errors
    /// Path is relative, non-normalized, non-UTF-8 or control-bearing.
    pub fn with_cwd(mut self, cwd: PathBuf) -> Result<Self, SessionQueryError> {
        if !crate::creation::valid_cwd(&cwd) {
            return Err(SessionQueryError::InvalidQuery);
        }
        self.cwd = Some(cwd);
        Ok(self)
    }

    /// Match direct children of one safe parent id.
    ///
    /// # Errors
    /// Parent id cannot name one sessions-root component.
    pub fn with_parent(
        mut self,
        parent: heycode_core::SessionId,
    ) -> Result<Self, SessionQueryError> {
        if !crate::creation::valid_session_component(parent.as_str()) {
            return Err(SessionQueryError::InvalidQuery);
        }
        self.parent = Some(parent);
        Ok(self)
    }

    /// Select active, archived or both storage states.
    #[must_use]
    pub fn with_storage(mut self, storage: SessionStorageFilter) -> Self {
        self.storage = storage;
        self
    }

    /// Select roots, forks or both lineage classes.
    #[must_use]
    pub fn with_lineage(mut self, lineage: SessionLineageFilter) -> Self {
        self.lineage = lineage;
        self
    }
}

/// Opaque keyset cursor bound to one exact filter generation.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionCursor {
    filter_sha256: [u8; 32],
    last_activity_ms: Option<i64>,
    session_id: heycode_core::SessionId,
}

/// One bounded listing request.
#[derive(Clone)]
pub struct SessionQuery {
    filter: SessionFilter,
    limit: usize,
    cursor: Option<SessionCursor>,
}

impl SessionQuery {
    /// Construct a request with a 1..=100 item page.
    ///
    /// # Errors
    /// Limit is outside the bounded range.
    pub fn new(filter: SessionFilter, limit: usize) -> Result<Self, SessionQueryError> {
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(SessionQueryError::InvalidQuery);
        }
        Ok(Self {
            filter,
            limit,
            cursor: None,
        })
    }

    /// Continue after one cursor returned for the same filter.
    #[must_use]
    pub fn with_cursor(mut self, cursor: SessionCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }
}

/// Safe picker/list metadata rebuilt from one valid logical session log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    id: heycode_core::SessionId,
    title: Option<String>,
    used: bool,
    cwd: Option<PathBuf>,
    runtime: Option<String>,
    source: Option<SessionSource>,
    provider: Option<String>,
    status: SessionActivityStatus,
    lineage: Option<SessionParent>,
    created_at_ms: Option<i64>,
    last_activity_ms: Option<i64>,
    event_count: u64,
    local_event_count: u64,
    storage: SessionStorageState,
    readable: bool,
}

impl SessionSummary {
    /// Session resume/fork identity.
    #[must_use]
    pub const fn id(&self) -> &heycode_core::SessionId {
        &self.id
    }

    /// Bounded one-line title projection.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Validated absolute creation cwd.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Validated runtime id.
    #[must_use]
    pub fn runtime(&self) -> Option<&str> {
        self.runtime.as_deref()
    }

    /// Local stream origin.
    #[must_use]
    pub const fn source(&self) -> Option<SessionSource> {
        self.source
    }

    /// Latest safe provider id evidenced by a request header.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Durable activity state.
    #[must_use]
    pub const fn status(&self) -> SessionActivityStatus {
        self.status
    }

    /// Verified direct parent, when forked.
    #[must_use]
    pub const fn lineage(&self) -> Option<&SessionParent> {
        self.lineage.as_ref()
    }

    /// Commit time of this stream's local creation/first event.
    #[must_use]
    pub const fn created_at_ms(&self) -> Option<i64> {
        self.created_at_ms
    }

    /// Latest logical event commit time.
    #[must_use]
    pub const fn last_activity_ms(&self) -> Option<i64> {
        self.last_activity_ms
    }

    /// Logical inherited plus local event count.
    #[must_use]
    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Events physically stored in this session's own JSONL suffix.
    #[must_use]
    pub const fn local_event_count(&self) -> u64 {
        self.local_event_count
    }

    /// Active or recoverably archived storage state.
    #[must_use]
    pub const fn storage(&self) -> SessionStorageState {
        self.storage
    }

    /// Whether this build could open the durable log behind the row.
    ///
    /// An unreadable session is still listed — one damaged log must not hide
    /// the rest of the store — but it carries no projected facts and cannot be
    /// resumed, forked, renamed, archived or exported until it is repaired
    /// outside the product.
    #[must_use]
    pub const fn is_readable(&self) -> bool {
        self.readable
    }
}

/// One deterministic keyset page.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionPage {
    items: Vec<SessionSummary>,
    next_cursor: Option<SessionCursor>,
    total_matches: usize,
}

impl SessionPage {
    /// Page rows in deterministic newest-first order.
    #[must_use]
    pub fn items(&self) -> &[SessionSummary] {
        &self.items
    }

    /// Opaque cursor for the next page.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<&SessionCursor> {
        self.next_cursor.as_ref()
    }

    /// Number of rows matching the filter at this scan.
    #[must_use]
    pub const fn total_matches(&self) -> usize {
        self.total_matches
    }
}

/// Stable safe query/resume/fork failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionQueryError {
    /// Request field is invalid.
    #[error("invalid session query")]
    InvalidQuery,
    /// Cursor belongs to another filter.
    #[error("invalid session query cursor")]
    InvalidCursor,
    /// Effect-owned provider has stopped.
    #[error("session query service stopped")]
    ServiceStopped,
    /// Sessions root cannot be read safely.
    #[error("session store unavailable")]
    StoreUnavailable,
    /// This replaceable backend does not own cross-session statistics.
    #[error("session statistics unavailable")]
    StatisticsUnavailable,
    /// Requested safe id is absent.
    #[error("session not found")]
    SessionNotFound,
    /// One discovered session is corrupt, unsafe or incompatible.
    #[error("invalid session in store")]
    InvalidSession,
    /// Shared-prefix fork was rejected.
    #[error("session fork rejected")]
    ForkRejected,
    /// Archive marker already exists.
    #[error("session is already archived")]
    AlreadyArchived,
    /// Archive restore targeted an active session.
    #[error("session is not archived")]
    NotArchived,
    /// Destructive action targeted the host's current session.
    #[error("current session cannot be archived or deleted")]
    CurrentSession,
    /// Another live session handle owns the target log.
    #[error("session is open")]
    OpenSession,
    /// Removing this parent would invalidate shared-prefix descendants.
    #[error("session has descendants")]
    HasDescendants,
    /// Recoverable-trash receipt no longer names a restorable entry.
    #[error("session recovery entry not found")]
    RecoveryNotFound,
    /// Export could not commit a complete bounded artifact.
    #[error("session export failed")]
    ExportFailed,
    /// Host cannot prove cross-process lifecycle exclusion.
    #[error("session lifecycle mutation is unsupported on this platform")]
    UnsupportedPlatform,
}

/// Replaceable query and durable lifecycle provider contract.
pub trait SessionQueryBackend: Send + Sync {
    /// Execute one bounded deterministic listing.
    ///
    /// # Errors
    /// Invalid request/cursor, stopped provider or unsafe/corrupt store.
    fn query(&self, query: &SessionQuery) -> Result<SessionPage, SessionQueryError>;

    /// Project one bounded cross-session statistics snapshot.
    ///
    /// Replacement backends may omit this optional capability. Absence is an
    /// explicit unavailable state, never an all-zero snapshot.
    ///
    /// # Errors
    /// Stopped provider, unavailable capability or unsafe/corrupt store.
    fn stats(&self) -> Result<SessionStatsSnapshot, SessionQueryError> {
        Err(SessionQueryError::StatisticsUnavailable)
    }

    /// Resume one exact safe id.
    ///
    /// # Errors
    /// Missing, stopped, unsafe or corrupt session.
    fn resume(&self, id: &heycode_core::SessionId) -> Result<Session, SessionQueryError>;

    /// Fork one persisted source at a stable boundary.
    ///
    /// # Errors
    /// Missing/invalid source, stopped provider or rejected boundary.
    fn fork(
        &self,
        id: &heycode_core::SessionId,
        boundary: ForkBoundary,
    ) -> Result<Session, SessionQueryError>;

    /// Create one metadata-bearing session and optional title transaction.
    ///
    /// # Errors
    /// Stopped/unsafe store or failed durable creation.
    fn create(&self, request: &SessionCreateRequest) -> Result<Session, SessionQueryError>;

    /// Append one title to a closed saved session.
    ///
    /// # Errors
    /// Missing/open/invalid target or failed durable append.
    fn rename(
        &self,
        id: &heycode_core::SessionId,
        title: &SessionTitle,
    ) -> Result<SessionSummary, SessionQueryError>;

    /// Append one title through the host-owned current session handle.
    ///
    /// # Errors
    /// Stopped/wrong-root session or failed durable append.
    fn rename_open(
        &self,
        session: &mut Session,
        title: &SessionTitle,
    ) -> Result<SessionSummary, SessionQueryError>;

    /// Apply or remove the recoverable archive marker without moving JSONL.
    ///
    /// # Errors
    /// Missing/open/invalid target or failed durable marker commit.
    fn archive(
        &self,
        id: &heycode_core::SessionId,
        action: SessionArchiveAction,
    ) -> Result<SessionSummary, SessionQueryError>;

    /// Move one safe leaf session into recoverable trash.
    ///
    /// # Errors
    /// Current/open/ancestor/missing/invalid targets are refused.
    fn delete(
        &self,
        request: &SessionDeleteRequest,
    ) -> Result<SessionDeleteReceipt, SessionQueryError>;

    /// Restore one exact recoverable-trash move.
    ///
    /// # Errors
    /// Receipt is absent, destination exists or restored lineage is invalid.
    fn restore_deleted(&self, receipt: &SessionDeleteReceipt) -> Result<(), SessionQueryError>;

    /// Commit a lossless-lineage JSONL bundle or human Markdown artifact.
    ///
    /// # Errors
    /// Missing/invalid source or failed bounded export commit.
    fn export(
        &self,
        id: &heycode_core::SessionId,
        format: SessionExportFormat,
    ) -> Result<SessionExportReceipt, SessionQueryError>;
}

/// Typed wrapper around one replaceable session query/index provider.
#[derive(Clone)]
pub struct SessionQueryService {
    backend: Arc<dyn SessionQueryBackend>,
}

impl SessionQueryService {
    /// Bind a provider implementation.
    #[must_use]
    pub fn new(backend: Arc<dyn SessionQueryBackend>) -> Self {
        Self { backend }
    }

    /// Construct a standalone local JSONL-truth service.
    #[must_use]
    pub fn local(root: PathBuf) -> Self {
        Self::new(Arc::new(LocalSessionQueryBackend::new(root)))
    }

    /// Execute one bounded deterministic listing.
    ///
    /// # Errors
    /// Invalid request/cursor, stopped provider or unsafe/corrupt store.
    pub fn query(&self, query: &SessionQuery) -> Result<SessionPage, SessionQueryError> {
        self.backend.query(query)
    }

    /// Project one bounded cross-session statistics snapshot.
    ///
    /// # Errors
    /// Stopped provider, unavailable capability or unsafe/corrupt store.
    pub fn stats(&self) -> Result<SessionStatsSnapshot, SessionQueryError> {
        self.backend.stats()
    }

    /// Return the newest row matching one filter.
    ///
    /// # Errors
    /// Stopped provider or unsafe/corrupt store.
    pub fn latest(
        &self,
        filter: &SessionFilter,
    ) -> Result<Option<SessionSummary>, SessionQueryError> {
        let page = self.backend.query(&SessionQuery::new(filter.clone(), 1)?)?;
        Ok(page.items.into_iter().next())
    }

    /// Resume one exact safe id.
    ///
    /// # Errors
    /// Missing, stopped, unsafe or corrupt session.
    pub fn resume(&self, id: &heycode_core::SessionId) -> Result<Session, SessionQueryError> {
        self.backend.resume(id)
    }

    /// Fork one persisted source at a stable boundary.
    ///
    /// # Errors
    /// Missing/invalid source, stopped provider or rejected boundary.
    pub fn fork(
        &self,
        id: &heycode_core::SessionId,
        boundary: ForkBoundary,
    ) -> Result<Session, SessionQueryError> {
        self.backend.fork(id, boundary)
    }

    /// Create one metadata-bearing session and optional title transaction.
    ///
    /// # Errors
    /// Stopped/unsafe store or failed durable creation.
    pub fn create(&self, request: &SessionCreateRequest) -> Result<Session, SessionQueryError> {
        self.backend.create(request)
    }

    /// Append one title to a closed saved session.
    ///
    /// # Errors
    /// Missing/open/invalid target or failed durable append.
    pub fn rename(
        &self,
        id: &heycode_core::SessionId,
        title: &SessionTitle,
    ) -> Result<SessionSummary, SessionQueryError> {
        self.backend.rename(id, title)
    }

    /// Append one title through the host-owned current session handle.
    ///
    /// # Errors
    /// Stopped/wrong-root session or failed durable append.
    pub fn rename_open(
        &self,
        session: &mut Session,
        title: &SessionTitle,
    ) -> Result<SessionSummary, SessionQueryError> {
        self.backend.rename_open(session, title)
    }

    /// Apply or remove the recoverable archive marker without moving JSONL.
    ///
    /// # Errors
    /// Missing/open/invalid target or failed durable marker commit.
    pub fn archive(
        &self,
        id: &heycode_core::SessionId,
        action: SessionArchiveAction,
    ) -> Result<SessionSummary, SessionQueryError> {
        self.backend.archive(id, action)
    }

    /// Move one safe leaf session into recoverable trash.
    ///
    /// # Errors
    /// Current/open/ancestor/missing/invalid targets are refused.
    pub fn delete(
        &self,
        request: &SessionDeleteRequest,
    ) -> Result<SessionDeleteReceipt, SessionQueryError> {
        self.backend.delete(request)
    }

    /// Restore one exact recoverable-trash move.
    ///
    /// # Errors
    /// Receipt is absent, destination exists or restored lineage is invalid.
    pub fn restore_deleted(&self, receipt: &SessionDeleteReceipt) -> Result<(), SessionQueryError> {
        self.backend.restore_deleted(receipt)
    }

    /// Commit a lossless-lineage JSONL bundle or human Markdown artifact.
    ///
    /// # Errors
    /// Missing/invalid source or failed bounded export commit.
    pub fn export(
        &self,
        id: &heycode_core::SessionId,
        format: SessionExportFormat,
    ) -> Result<SessionExportReceipt, SessionQueryError> {
        self.backend.export(id, format)
    }
}

pub(crate) struct LocalSessionQueryBackend {
    root: PathBuf,
    active: Arc<AtomicBool>,
    operations: Mutex<()>,
}

impl LocalSessionQueryBackend {
    fn distinct_title(
        &self,
        id: &heycode_core::SessionId,
        title: &SessionTitle,
    ) -> Result<SessionTitle, SessionQueryError> {
        let titles: std::collections::BTreeSet<String> = self
            .summaries(&SessionFilter::new())?
            .into_iter()
            .filter(|summary| summary.id() != id)
            .filter_map(|summary| summary.title().map(str::to_owned))
            .collect();
        let mut candidate = title.clone();
        let mut ordinal = 2;
        while titles.contains(candidate.as_str()) {
            candidate = title.with_ordinal(ordinal);
            ordinal += 1;
        }
        Ok(candidate)
    }

    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            active: Arc::new(AtomicBool::new(true)),
            operations: Mutex::new(()),
        }
    }

    pub(crate) fn shutdown_handle(&self) -> Arc<AtomicBool> {
        self.active.clone()
    }

    fn check(&self) -> Result<(), SessionQueryError> {
        if self.active.load(AtomicOrdering::Acquire) {
            Ok(())
        } else {
            Err(SessionQueryError::ServiceStopped)
        }
    }

    fn session_directories(&self) -> Result<Vec<(String, PathBuf)>, SessionQueryError> {
        self.check()?;
        let root_metadata = match std::fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(SessionQueryError::StoreUnavailable),
        };
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(SessionQueryError::StoreUnavailable);
        }
        let entries = std::fs::read_dir(&self.root)
            .map_err(|_| SessionQueryError::StoreUnavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        if entries.len() > MAX_SESSION_DIRECTORIES {
            return Err(SessionQueryError::StoreUnavailable);
        }
        let mut directories = Vec::new();
        let mut scan_bytes = 0_u64;
        for entry in entries {
            self.check()?;
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|_| SessionQueryError::InvalidSession)?;
            if metadata.file_type().is_symlink() {
                return Err(SessionQueryError::InvalidSession);
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return Err(SessionQueryError::InvalidSession);
            };
            let reserved = matches!(
                name.as_str(),
                crate::lifecycle::TRASH_DIRECTORY_NAME | crate::lifecycle::EXPORT_DIRECTORY_NAME
            );
            if reserved {
                if !metadata.is_dir() {
                    return Err(SessionQueryError::InvalidSession);
                }
                continue;
            }
            if !metadata.is_dir() {
                continue;
            }
            if name.starts_with(crate::session::FORK_STAGING_PREFIX) && name.ends_with(".tmp") {
                continue;
            }
            if !crate::creation::valid_session_component(&name) {
                continue;
            }
            let Ok(log_metadata) =
                std::fs::symlink_metadata(entry.path().join(crate::session::LOG_FILE_NAME))
            else {
                continue;
            };
            if log_metadata.file_type().is_symlink() {
                return Err(SessionQueryError::InvalidSession);
            }
            if !log_metadata.is_file() {
                continue;
            }
            scan_bytes = scan_bytes
                .checked_add(log_metadata.len())
                .ok_or(SessionQueryError::StoreUnavailable)?;
            if scan_bytes > MAX_QUERY_SCAN_BYTES {
                return Err(SessionQueryError::StoreUnavailable);
            }
            directories.push((name, entry.path()));
        }
        directories.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(directories)
    }

    pub(crate) fn summaries(
        &self,
        filter: &SessionFilter,
    ) -> Result<Vec<SessionSummary>, SessionQueryError> {
        let directories = self.session_directories()?;
        let mut summaries = Vec::with_capacity(directories.len());
        for (name, directory) in directories {
            self.check()?;
            let storage = crate::lifecycle::storage_state(&directory)?;
            let opened = match open_store_session(&directory) {
                Ok(session) => Some(session),
                Err(error) if store_scan_is_fatal(&error) => return Err(map_open_error(error)),
                Err(_) => None,
            };
            let summary = match &opened {
                Some(session) => summarize(session, storage)?,
                None => unreadable_summary(heycode_core::SessionId::from_raw(&name), storage),
            };
            if matches_filter(&summary, filter)
                && filter.text.as_ref().is_none_or(|needle| match &opened {
                    Some(session) => session_matches_text(session, &summary, needle),
                    None => summary_matches_text(&summary, needle),
                })
            {
                summaries.push(summary);
            }
        }
        summaries.sort_by(compare_summaries);
        Ok(summaries)
    }
}

impl SessionQueryBackend for LocalSessionQueryBackend {
    fn query(&self, query: &SessionQuery) -> Result<SessionPage, SessionQueryError> {
        self.check()?;
        let fingerprint = filter_fingerprint(&query.filter);
        if query
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.filter_sha256 != fingerprint)
        {
            return Err(SessionQueryError::InvalidCursor);
        }
        let mut matched = self.summaries(&query.filter)?;
        let total_matches = matched.len();
        if let Some(cursor) = &query.cursor {
            matched.retain(|summary| compare_summary_cursor(summary, cursor) == Ordering::Greater);
        }
        let has_more = matched.len() > query.limit;
        let items = matched.into_iter().take(query.limit).collect::<Vec<_>>();
        let next_cursor = if has_more {
            items.last().map(|summary| SessionCursor {
                filter_sha256: fingerprint,
                last_activity_ms: summary.last_activity_ms,
                session_id: summary.id.clone(),
            })
        } else {
            None
        };
        Ok(SessionPage {
            items,
            next_cursor,
            total_matches,
        })
    }

    fn stats(&self) -> Result<SessionStatsSnapshot, SessionQueryError> {
        let directories = self.session_directories()?;
        let mut stats = SessionStatsAccumulator::default();
        for (_name, directory) in directories {
            self.check()?;
            let storage = crate::lifecycle::storage_state(&directory)?;
            match open_store_session(&directory) {
                Ok(session) => {
                    let start = usize::try_from(session.first_local_seq())
                        .map_err(|_| SessionQueryError::InvalidSession)?;
                    let local_events = session
                        .events()
                        .get(start..)
                        .ok_or(SessionQueryError::InvalidSession)?;
                    stats.observe(local_events, session.lineage().is_some(), storage);
                }
                Err(error) if store_scan_is_fatal(&error) => return Err(map_open_error(error)),
                Err(_) => stats.unreadable(),
            }
        }
        Ok(stats.finish())
    }

    fn resume(&self, id: &heycode_core::SessionId) -> Result<Session, SessionQueryError> {
        self.check()?;
        if !crate::creation::valid_session_component(id.as_str()) {
            return Err(SessionQueryError::InvalidQuery);
        }
        let directory = self.root.join(id.as_str());
        match std::fs::symlink_metadata(&directory) {
            Ok(_) => open_query_session(&directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(SessionQueryError::SessionNotFound)
            }
            Err(_) => Err(SessionQueryError::InvalidSession),
        }
    }

    fn fork(
        &self,
        id: &heycode_core::SessionId,
        boundary: ForkBoundary,
    ) -> Result<Session, SessionQueryError> {
        let _operation = self
            .operations
            .lock()
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        let source = self.resume(id)?;
        source
            .fork(&self.root, boundary)
            .map_err(|_| SessionQueryError::ForkRejected)
    }

    fn create(&self, request: &SessionCreateRequest) -> Result<Session, SessionQueryError> {
        self.check()?;
        let _operation = self
            .operations
            .lock()
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        let mut session = Session::create_with_metadata(&self.root, request.metadata().clone())
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        if let Some(title) = request.title()
            && session
                .append(SessionEventKind::SessionTitle {
                    title: title.as_str().to_owned(),
                })
                .is_err()
        {
            let directory = session.path().parent().map(Path::to_path_buf);
            drop(session);
            if let Some(directory) = directory {
                let _ = std::fs::remove_dir_all(directory);
            }
            return Err(SessionQueryError::StoreUnavailable);
        }
        Ok(session)
    }

    fn rename(
        &self,
        id: &heycode_core::SessionId,
        title: &SessionTitle,
    ) -> Result<SessionSummary, SessionQueryError> {
        self.check()?;
        #[cfg(not(unix))]
        return Err(SessionQueryError::UnsupportedPlatform);
        #[cfg(unix)]
        {
            let _operation = self
                .operations
                .lock()
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let _lineage = crate::session::lock_lineage_mutation(&self.root)
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let title = self.distinct_title(id, title)?;
            let mut session = self.resume(id)?;
            if !session
                .try_lock_lifecycle_exclusive()
                .map_err(|_| SessionQueryError::StoreUnavailable)?
            {
                return Err(SessionQueryError::OpenSession);
            }
            session
                .append(SessionEventKind::SessionTitle {
                    title: title.as_str().to_owned(),
                })
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let directory = session_directory(&session)?;
            let storage = crate::lifecycle::storage_state(&directory)?;
            summarize(&session, storage)
        }
    }

    fn rename_open(
        &self,
        session: &mut Session,
        title: &SessionTitle,
    ) -> Result<SessionSummary, SessionQueryError> {
        self.check()?;
        let _operation = self
            .operations
            .lock()
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        let directory = session_directory(session)?;
        if directory.parent() != Some(self.root.as_path()) {
            return Err(SessionQueryError::InvalidSession);
        }
        let _lineage = crate::session::lock_lineage_mutation(&self.root)
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        let title = self.distinct_title(session.id(), title)?;
        session
            .append(SessionEventKind::SessionTitle {
                title: title.as_str().to_owned(),
            })
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        let storage = crate::lifecycle::storage_state(&directory)?;
        summarize(session, storage)
    }

    fn archive(
        &self,
        id: &heycode_core::SessionId,
        action: SessionArchiveAction,
    ) -> Result<SessionSummary, SessionQueryError> {
        self.check()?;
        #[cfg(not(unix))]
        return Err(SessionQueryError::UnsupportedPlatform);
        #[cfg(unix)]
        {
            let _operation = self
                .operations
                .lock()
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let session = self.resume(id)?;
            if !session
                .try_lock_lifecycle_exclusive()
                .map_err(|_| SessionQueryError::StoreUnavailable)?
            {
                return Err(SessionQueryError::OpenSession);
            }
            let directory = session_directory(&session)?;
            crate::lifecycle::apply_archive_marker(&directory, action)?;
            let storage = crate::lifecycle::storage_state(&directory)?;
            summarize(&session, storage)
        }
    }

    fn delete(
        &self,
        request: &SessionDeleteRequest,
    ) -> Result<SessionDeleteReceipt, SessionQueryError> {
        self.check()?;
        #[cfg(not(unix))]
        return Err(SessionQueryError::UnsupportedPlatform);
        #[cfg(unix)]
        {
            if request.current() == Some(request.id()) {
                return Err(SessionQueryError::CurrentSession);
            }
            let _operation = self
                .operations
                .lock()
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let _lineage = crate::session::lock_lineage_mutation(&self.root)
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let descendants =
                self.summaries(&SessionFilter::new().with_storage(SessionStorageFilter::All))?;
            if descendants.iter().any(|summary| {
                summary
                    .lineage()
                    .is_some_and(|lineage| lineage.parent_session_id() == request.id())
            }) {
                return Err(SessionQueryError::HasDescendants);
            }
            // Deleting a parent is only safe against a COMPLETE lineage
            // picture; an unreadable row states no lineage, so the scan cannot
            // prove this target has no descendants.
            if descendants.iter().any(|summary| !summary.is_readable()) {
                return Err(SessionQueryError::InvalidSession);
            }
            let session = self.resume(request.id())?;
            if !session
                .try_lock_lifecycle_exclusive()
                .map_err(|_| SessionQueryError::StoreUnavailable)?
            {
                return Err(SessionQueryError::OpenSession);
            }
            let source = session_directory(&session)?;
            let receipt = SessionDeleteReceipt::new(
                request.id().clone(),
                heycode_core::SessionId::generate(),
            );
            let destination = crate::lifecycle::trash_path(&self.root, &receipt)?;
            if destination.exists() {
                return Err(SessionQueryError::StoreUnavailable);
            }
            std::fs::rename(&source, &destination)
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let trash = destination
                .parent()
                .ok_or(SessionQueryError::StoreUnavailable)?;
            if crate::lifecycle::sync_directory(trash)
                .and_then(|()| crate::lifecycle::sync_directory(&self.root))
                .is_err()
            {
                let _ = std::fs::rename(&destination, &source);
                let _ = crate::lifecycle::sync_directory(&self.root);
                let _ = crate::lifecycle::sync_directory(trash);
                return Err(SessionQueryError::StoreUnavailable);
            }
            Ok(receipt)
        }
    }

    fn restore_deleted(&self, receipt: &SessionDeleteReceipt) -> Result<(), SessionQueryError> {
        self.check()?;
        #[cfg(not(unix))]
        return Err(SessionQueryError::UnsupportedPlatform);
        #[cfg(unix)]
        {
            let _operation = self
                .operations
                .lock()
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let _lineage = crate::session::lock_lineage_mutation(&self.root)
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            let source = crate::lifecycle::trash_path(&self.root, receipt)?;
            match std::fs::symlink_metadata(&source) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(SessionQueryError::InvalidSession);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SessionQueryError::RecoveryNotFound);
                }
                Err(_) => return Err(SessionQueryError::StoreUnavailable),
            }
            let destination = self.root.join(receipt.session_id().as_str());
            if destination.exists() {
                return Err(SessionQueryError::StoreUnavailable);
            }
            std::fs::rename(&source, &destination)
                .map_err(|_| SessionQueryError::StoreUnavailable)?;
            if Session::open(&destination).is_err() {
                let _ = std::fs::rename(&destination, &source);
                return Err(SessionQueryError::InvalidSession);
            }
            let trash = source.parent().ok_or(SessionQueryError::StoreUnavailable)?;
            if crate::lifecycle::sync_directory(&self.root)
                .and_then(|()| crate::lifecycle::sync_directory(trash))
                .is_err()
            {
                let _ = std::fs::rename(&destination, &source);
                let _ = crate::lifecycle::sync_directory(&self.root);
                let _ = crate::lifecycle::sync_directory(trash);
                return Err(SessionQueryError::StoreUnavailable);
            }
            Ok(())
        }
    }

    fn export(
        &self,
        id: &heycode_core::SessionId,
        format: SessionExportFormat,
    ) -> Result<SessionExportReceipt, SessionQueryError> {
        self.check()?;
        let _operation = self
            .operations
            .lock()
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        let _lineage = crate::session::lock_lineage_mutation(&self.root)
            .map_err(|_| SessionQueryError::StoreUnavailable)?;
        match format {
            SessionExportFormat::LosslessJsonl => {
                crate::lifecycle::export_lossless_jsonl(&self.root, id)
            }
            SessionExportFormat::Markdown => crate::lifecycle::export_markdown(&self.root, id),
            SessionExportFormat::RedactedSupport => {
                crate::lifecycle::export_redacted_support(&self.root, id)
            }
        }
    }
}

fn summarize(
    session: &Session,
    storage: SessionStorageState,
) -> Result<SessionSummary, SessionQueryError> {
    let event_count =
        u64::try_from(session.events().len()).map_err(|_| SessionQueryError::InvalidSession)?;
    let local_event_count = event_count
        .checked_sub(session.first_local_seq())
        .ok_or(SessionQueryError::InvalidSession)?;
    let title = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            SessionEventKind::SessionTitle { title } => Some(title.as_str()),
            _ => None,
        })
        .and_then(safe_title)
        .or_else(|| conversation_title(session.events()));
    let provider = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            SessionEventKind::RequestHeader { header, .. } => Some(header.provider.as_str()),
            _ => None,
        })
        .filter(|provider| safe_identifier(provider))
        .map(str::to_owned);
    let metadata = session.metadata();
    let created_at_ms = usize::try_from(session.first_local_seq())
        .ok()
        .and_then(|index| session.events().get(index))
        .map(|event| event.time_ms);
    Ok(SessionSummary {
        id: session.id().clone(),
        title,
        used: session.lineage().is_some() || !crate::is_unused_session(session.events()),
        cwd: metadata.and_then(|metadata| metadata.cwd().map(Path::to_path_buf)),
        runtime: session
            .runtime_link()
            .map(|(runtime, _)| runtime.to_owned())
            .or_else(|| metadata.and_then(|metadata| metadata.runtime().map(str::to_owned))),
        source: metadata.map(|metadata| metadata.source()),
        provider,
        status: activity_status(session),
        lineage: session.lineage().cloned(),
        created_at_ms,
        last_activity_ms: session
            .events()
            .iter()
            .rev()
            .find(|event| {
                !matches!(
                    event.kind,
                    SessionEventKind::SessionTitle { .. }
                        | SessionEventKind::RuntimeConfigured { .. }
                        | SessionEventKind::RuntimeLinked { .. }
                )
            })
            .or_else(|| session.events().first())
            .map(|event| event.time_ms),
        event_count,
        local_event_count,
        storage,
        readable: true,
    })
}

/// The row for a store entry this build cannot project.
///
/// Nothing is invented: every fact a valid log would supply stays absent, and
/// [`SessionSummary::is_readable`] says why.
fn unreadable_summary(id: heycode_core::SessionId, storage: SessionStorageState) -> SessionSummary {
    SessionSummary {
        id,
        title: None,
        used: false,
        cwd: None,
        runtime: None,
        source: None,
        provider: None,
        status: SessionActivityStatus::Empty,
        lineage: None,
        created_at_ms: None,
        last_activity_ms: None,
        event_count: 0,
        local_event_count: 0,
        storage,
        readable: false,
    }
}

fn activity_status(session: &Session) -> SessionActivityStatus {
    let mut open_turn = None;
    let mut has_work = false;
    for event in session.events() {
        match &event.kind {
            SessionEventKind::TurnStart { turn } => {
                open_turn = Some(*turn);
                has_work = true;
            }
            SessionEventKind::TurnEnd { turn, .. } if open_turn == Some(*turn) => {
                open_turn = None;
            }
            SessionEventKind::UserMessage { .. }
            | SessionEventKind::HookContribution { .. }
            | SessionEventKind::UserAttachments { .. }
            | SessionEventKind::AttachmentAdded { .. }
            | SessionEventKind::AssistantMessage { .. }
            | SessionEventKind::AssistantProviderItem { .. }
            | SessionEventKind::AssistantResponseMetadata { .. }
            | SessionEventKind::ServerToolCall { .. }
            | SessionEventKind::ServerToolResult { .. }
            | SessionEventKind::AssistantCitation { .. }
            | SessionEventKind::ToolCall { .. }
            | SessionEventKind::ToolResult { .. }
            | SessionEventKind::RichToolResult { .. }
            | SessionEventKind::AgentInboxSplice { .. }
            | SessionEventKind::RequestHeader { .. } => has_work = true,
            _ => {}
        }
    }
    if open_turn.is_some() {
        SessionActivityStatus::OpenTurn
    } else if has_work {
        SessionActivityStatus::Idle
    } else {
        SessionActivityStatus::Empty
    }
}

fn matches_filter(summary: &SessionSummary, filter: &SessionFilter) -> bool {
    if filter.excluded_session.as_ref() == Some(summary.id()) {
        return false;
    }
    if filter
        .exact_title
        .as_ref()
        .is_some_and(|title| summary.title.as_ref() != Some(title))
    {
        return false;
    }
    if filter.used_only && summary.readable && !summary.used {
        return false;
    }
    if filter
        .provider
        .as_ref()
        .is_some_and(|provider| summary.provider.as_ref() != Some(provider))
        || filter
            .runtime
            .as_ref()
            .is_some_and(|runtime| summary.runtime.as_ref() != Some(runtime))
        || filter
            .source
            .is_some_and(|source| summary.source != Some(source))
        || filter.status.is_some_and(|status| summary.status != status)
        || filter
            .cwd
            .as_ref()
            .is_some_and(|cwd| summary.cwd.as_ref() != Some(cwd))
        || filter.parent.as_ref().is_some_and(|parent| {
            summary
                .lineage
                .as_ref()
                .map(SessionParent::parent_session_id)
                != Some(parent)
        })
        || match filter.storage {
            SessionStorageFilter::Active => summary.storage != SessionStorageState::Active,
            SessionStorageFilter::Archived => summary.storage != SessionStorageState::Archived,
            SessionStorageFilter::All => false,
        }
        || match filter.lineage {
            SessionLineageFilter::All => false,
            SessionLineageFilter::Roots => summary.lineage.is_some(),
            SessionLineageFilter::Forks => summary.lineage.is_none(),
        }
    {
        return false;
    }
    true
}

fn summary_matches_text(summary: &SessionSummary, needle: &str) -> bool {
    summary.id.as_str().to_lowercase().contains(needle)
        || summary
            .title
            .as_ref()
            .is_some_and(|title| title.to_lowercase().contains(needle))
        || summary
            .cwd
            .as_ref()
            .and_then(|cwd| cwd.to_str())
            .is_some_and(|cwd| cwd.to_lowercase().contains(needle))
        || summary
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.to_lowercase().contains(needle))
        || summary
            .provider
            .as_ref()
            .is_some_and(|provider| provider.to_lowercase().contains(needle))
}

fn session_matches_text(session: &Session, summary: &SessionSummary, needle: &str) -> bool {
    summary_matches_text(summary, needle)
        || session.events().iter().any(|event| match &event.kind {
            SessionEventKind::UserMessage { text }
            | SessionEventKind::SessionTitle { title: text } => {
                text.to_lowercase().contains(needle)
            }
            SessionEventKind::HookContribution { contribution } => {
                contribution.owner().to_lowercase().contains(needle)
                    || contribution.text().to_lowercase().contains(needle)
            }
            SessionEventKind::AttachmentAdded { attachment } => attachment
                .display_name()
                .is_some_and(|name| name.to_lowercase().contains(needle)),
            SessionEventKind::UserAttachments { attachments, .. } => {
                attachments.iter().any(|item| {
                    item.display_name()
                        .is_some_and(|name| name.to_lowercase().contains(needle))
                })
            }
            SessionEventKind::AssistantMessage {
                content, reasoning, ..
            } => {
                content.to_lowercase().contains(needle)
                    || reasoning
                        .as_ref()
                        .is_some_and(|text| text.to_lowercase().contains(needle))
            }
            SessionEventKind::AgentInboxSplice { inserted, .. } => inserted
                .iter()
                .any(|message| message.text().to_lowercase().contains(needle)),
            SessionEventKind::ServerToolCall { call, .. } => {
                call.logical().to_lowercase().contains(needle)
                    || call.provider_name().to_lowercase().contains(needle)
            }
            SessionEventKind::ServerToolResult { result, .. } => result
                .error_code()
                .is_some_and(|code| code.to_lowercase().contains(needle)),
            SessionEventKind::AssistantCitation { citation, .. } => {
                citation.url().to_lowercase().contains(needle)
                    || citation
                        .title()
                        .is_some_and(|title| title.to_lowercase().contains(needle))
                    || citation
                        .cited_text()
                        .is_some_and(|text| text.to_lowercase().contains(needle))
            }
            SessionEventKind::RichToolResult { result, .. } => {
                result.render_for_model().to_lowercase().contains(needle)
            }
            _ => false,
        })
}

fn compare_summaries(left: &SessionSummary, right: &SessionSummary) -> Ordering {
    right
        .last_activity_ms
        .cmp(&left.last_activity_ms)
        .then_with(|| left.id.as_str().cmp(right.id.as_str()))
}

fn compare_summary_cursor(summary: &SessionSummary, cursor: &SessionCursor) -> Ordering {
    cursor
        .last_activity_ms
        .cmp(&summary.last_activity_ms)
        .then_with(|| summary.id.as_str().cmp(cursor.session_id.as_str()))
}

fn filter_fingerprint(filter: &SessionFilter) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"dshx-session-filter-v4\0");
    update_optional(
        &mut digest,
        filter
            .excluded_session
            .as_ref()
            .map(heycode_core::SessionId::as_str),
    );
    update_optional(&mut digest, filter.text.as_deref());
    update_optional(&mut digest, filter.exact_title.as_deref());
    update_optional(&mut digest, filter.provider.as_deref());
    update_optional(&mut digest, filter.runtime.as_deref());
    update_optional(&mut digest, filter.source.map(source_name));
    update_optional(&mut digest, filter.status.map(status_name));
    update_optional(
        &mut digest,
        filter.cwd.as_ref().and_then(|cwd| cwd.to_str()),
    );
    update_optional(
        &mut digest,
        filter.parent.as_ref().map(heycode_core::SessionId::as_str),
    );
    update_optional(
        &mut digest,
        Some(if filter.used_only { "used" } else { "all" }),
    );
    update_optional(&mut digest, Some(storage_filter_name(filter.storage)));
    update_optional(&mut digest, Some(lineage_filter_name(filter.lineage)));
    digest.finalize().into()
}

fn update_optional(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
            digest.update(value.as_bytes());
        }
        None => digest.update([0]),
    }
}

fn source_name(source: SessionSource) -> &'static str {
    match source {
        SessionSource::Interactive => "interactive",
        SessionSource::Headless => "headless",
        SessionSource::Acp => "acp",
        SessionSource::Subagent => "subagent",
        SessionSource::Scheduled => "scheduled",
        SessionSource::Delegated => "delegated",
        SessionSource::Fork => "fork",
    }
}

fn status_name(status: SessionActivityStatus) -> &'static str {
    match status {
        SessionActivityStatus::Empty => "empty",
        SessionActivityStatus::Idle => "idle",
        SessionActivityStatus::OpenTurn => "open_turn",
    }
}

fn storage_filter_name(storage: SessionStorageFilter) -> &'static str {
    match storage {
        SessionStorageFilter::Active => "active",
        SessionStorageFilter::Archived => "archived",
        SessionStorageFilter::All => "all",
    }
}

fn lineage_filter_name(lineage: SessionLineageFilter) -> &'static str {
    match lineage {
        SessionLineageFilter::All => "all",
        SessionLineageFilter::Roots => "roots",
        SessionLineageFilter::Forks => "forks",
    }
}

/// Derive an immediate name for untitled conversations, including older logs.
/// Explicit durable titles take precedence; this projection never rewrites history.
fn conversation_title(events: &[crate::SessionEvent]) -> Option<String> {
    events.iter().find_map(|event| {
        let SessionEventKind::UserMessage { text } = &event.kind else {
            return None;
        };
        let line = text.lines().find(|line| !line.trim().is_empty())?;
        let words = line
            .split_whitespace()
            .take(10)
            .collect::<Vec<_>>()
            .join(" ");
        let clean = safe_title(&words)?;
        let mut title: String = clean.chars().take(64).collect();
        if clean.chars().count() > 64 || line.split_whitespace().count() > 10 {
            title = title.trim_end().to_owned();
            title.push('…');
        }
        Some(title)
    })
}

pub(crate) fn safe_title(value: &str) -> Option<String> {
    // Preserve intentional spacing in titles accepted by the human title
    // contract. Historical/provider titles still use defensive cleanup below.
    if let Ok(title) = SessionTitle::new(value) {
        return Some(title.as_str().to_owned());
    }
    let mut output = String::new();
    let mut previous_space = false;
    for character in value.chars() {
        let character = if unsafe_terminal_char(character) || character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' {
            if output.is_empty() || previous_space {
                continue;
            }
            previous_space = true;
        } else {
            previous_space = false;
        }
        if output.chars().count() == MAX_SAFE_TITLE_CHARS {
            break;
        }
        output.push(character);
    }
    let trimmed = output.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}

fn unsafe_terminal_char(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

/// Translate a store-entry open failure into the query vocabulary.
///
/// [`crate::OpenError::AlreadyOpen`] has no arm on purpose: every path that
/// reaches here opens through [`Session::open`], which takes no writer lease,
/// so the variant cannot occur. A path that starts opening for writing must
/// add the arm — [`SessionQueryError::OpenSession`] is its answer — rather
/// than let it fall through to `InvalidSession` and call a busy session
/// broken.
pub(crate) fn map_open_error(error: crate::OpenError) -> SessionQueryError {
    match error {
        crate::OpenError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            SessionQueryError::SessionNotFound
        }
        _ => SessionQueryError::InvalidSession,
    }
}

fn session_directory(session: &Session) -> Result<PathBuf, SessionQueryError> {
    session
        .path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or(SessionQueryError::InvalidSession)
}

fn open_query_session(directory: &Path) -> Result<Session, SessionQueryError> {
    open_store_session(directory).map_err(map_open_error)
}

/// Open one store entry, retrying only while the log is still growing: a
/// concurrent writer produces the same evidence as a torn tail.
fn open_store_session(directory: &Path) -> Result<Session, crate::OpenError> {
    let log = directory.join(crate::session::LOG_FILE_NAME);
    let mut attempt = 0_u8;
    loop {
        let before = std::fs::symlink_metadata(&log)?.len();
        let error = match Session::open(directory) {
            Ok(session) => return Ok(session),
            Err(error) => error,
        };
        let after = std::fs::symlink_metadata(&log)?.len();
        let transient_tail = matches!(
            &error,
            crate::OpenError::UnterminatedTail | crate::OpenError::CorruptLine { .. }
        );
        if attempt == 2 || (before == after && !transient_tail) {
            return Err(error);
        }
        attempt += 1;
        std::thread::yield_now();
    }
}

/// Whether one unopenable entry condemns the whole scan.
///
/// A log that is not well-formed append-only JSONL, or storage that is not a
/// plain file where a log belongs, means the store itself is damaged or
/// tampered with: that stays fail-loud (AGENTS.md §4). A log that is perfectly
/// well-formed but that this build cannot project — outgrown bounds, a newer
/// schema, a broken semantic postcondition — is one bad session, and listing
/// it as unreadable is what keeps `--continue`, the picker and delete alive for
/// every healthy session beside it.
/// Whether one log's open failure is about the STORE rather than the log.
///
/// Only an unsafe path (a symlink where a file must be) or an I/O failure says
/// something about the store. A corrupt line or an unterminated tail is damage
/// to one log — a crash in the middle of an append is the commonest damage a
/// real store carries — and lists as an unreadable row so every other session,
/// `--continue` and the picker keep working.
fn store_scan_is_fatal(error: &crate::OpenError) -> bool {
    matches!(
        error,
        crate::OpenError::UnsafePath | crate::OpenError::Io(_)
    )
}

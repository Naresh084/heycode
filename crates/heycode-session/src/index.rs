//! Rebuildable SQLite projection over authoritative session JSONL.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::query::LocalSessionQueryBackend;
use crate::{
    Session, SessionActivityStatus, SessionFilter, SessionLineageFilter, SessionQueryError,
    SessionSource, SessionStorageFilter, SessionStorageState, SessionSummary,
};

const INDEX_FILE_NAME: &str = ".session-index.sqlite3";
const STAGING_PREFIX: &str = ".session-index-rebuild-";
const STAGING_SUFFIX: &str = ".tmp";
const INDEX_SCHEMA_VERSION: u32 = 1;
const INDEX_APPLICATION_ID: i64 = 0x4453_4858;
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INDEX_ROWS: usize = 10_000;
const MAX_SUMMARY_BYTES: usize = 16 * 1024;

const INDEX_SCHEMA: &str = r#"
CREATE TABLE index_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    manifest_sha256 TEXT NOT NULL,
    session_count INTEGER NOT NULL
) STRICT;
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    summary_json TEXT NOT NULL,
    source_sha256 TEXT NOT NULL
) STRICT, WITHOUT ROWID;
"#;

/// Stable reason a disposable index must be rebuilt from JSONL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIndexIssue {
    /// No index has been built.
    Missing,
    /// SQLite structure or internally committed projection metadata is invalid.
    Corrupt,
    /// Valid SQLite rows describe a different JSONL generation.
    Stale,
    /// A prior or concurrent rebuild left a recognizable staging file.
    UnfinishedRebuild,
    /// Index schema is not the one implemented by this binary.
    IncompatibleSchema {
        /// Schema stored in SQLite.
        found: u32,
    },
}

/// Result of comparing one SQLite projection with a fresh JSONL scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionIndexComparison {
    /// SQLite exactly matches the current validated JSONL generation.
    Current(SessionIndexSnapshot),
    /// SQLite is disposable and must be rebuilt for this reason.
    RebuildRequired(SessionIndexIssue),
}

/// Content-free identity of one indexed JSONL generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndexSnapshot {
    session_count: usize,
    manifest_sha256: String,
}

impl SessionIndexSnapshot {
    /// Number of validated sessions represented by the projection.
    #[must_use]
    pub const fn session_count(&self) -> usize {
        self.session_count
    }

    /// Lowercase SHA-256 over sorted summary rows and exact persisted-line hashes.
    #[must_use]
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
}

/// Safe failures from rebuilding or comparing the disposable index.
#[derive(Debug, thiserror::Error)]
pub enum SessionIndexError {
    /// Authoritative JSONL could not be fully validated.
    #[error("session index source JSONL is invalid")]
    InvalidTruth(#[from] SessionQueryError),
    /// Sessions root or index storage is unsafe.
    #[error("session index storage is unsafe")]
    UnsafeStorage,
    /// JSONL changed while a replacement projection was being prepared.
    #[error("session JSONL changed during index rebuild")]
    SourceChanged,
    /// A bounded SQLite operation failed while preparing a disposable index.
    #[error("session SQLite index operation failed")]
    Database,
    /// Summary serialization failed before any index commit.
    #[error("session index projection failed")]
    Projection,
    /// Filesystem operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A fixed local SQLite projection beside the session directories.
///
/// The database is never consulted as durable truth. [`Self::compare`] and
/// [`Self::rebuild`] first reopen and validate every JSONL session, including
/// shared-prefix lineage, then compare or replace this derived file.
pub struct SqliteSessionIndex {
    root: PathBuf,
    path: PathBuf,
}

impl SqliteSessionIndex {
    /// Bind an existing safe sessions root.
    ///
    /// # Errors
    /// The root is missing, a symlink, not a directory or cannot be canonicalized.
    pub fn new(root: PathBuf) -> Result<Self, SessionIndexError> {
        let metadata = fs::symlink_metadata(&root).map_err(|_| SessionIndexError::UnsafeStorage)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SessionIndexError::UnsafeStorage);
        }
        let root = fs::canonicalize(root).map_err(|_| SessionIndexError::UnsafeStorage)?;
        let path = root.join(INDEX_FILE_NAME);
        Ok(Self { root, path })
    }

    /// Fixed projection path beneath the sessions root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rebuild SQLite from one stable JSONL generation and atomically replace
    /// the prior projection.
    ///
    /// The prior index remains untouched if JSONL is invalid, projection fails
    /// or a second source scan observes concurrent change.
    ///
    /// # Errors
    /// Unsafe storage, invalid JSONL, concurrent source change, SQLite or
    /// filesystem failure.
    pub fn rebuild(&self) -> Result<SessionIndexSnapshot, SessionIndexError> {
        let _mutation = crate::session::lock_lineage_mutation(&self.root)?;
        remove_staging_residue(&self.root)?;
        let projection = project_truth(&self.root)?;
        let staging = StagingPath::create(&self.root)?;
        write_sqlite(staging.path(), &projection)?;

        let staged = fs::read(staging.path())?;
        if u64::try_from(staged.len()).unwrap_or(u64::MAX) > MAX_INDEX_BYTES
            || !staged.starts_with(b"SQLite format 3\0")
        {
            return Err(SessionIndexError::Database);
        }

        let confirmed = project_truth(&self.root)?;
        if confirmed != projection {
            return Err(SessionIndexError::SourceChanged);
        }
        validate_existing_index_path(&self.path)?;
        atomic_replace(&self.path, &staged)?;
        sync_directory(&self.root)?;
        drop(staging);

        match self.compare()? {
            SessionIndexComparison::Current(snapshot) => Ok(snapshot),
            SessionIndexComparison::RebuildRequired(_) => Err(SessionIndexError::Database),
        }
    }

    /// Compare SQLite with a newly validated JSONL projection.
    ///
    /// Index damage or staleness is a rebuild status, not a session failure.
    /// Invalid JSONL remains a hard source error and is never masked by SQLite.
    ///
    /// # Errors
    /// Authoritative JSONL or sessions-root storage cannot be validated.
    pub fn compare(&self) -> Result<SessionIndexComparison, SessionIndexError> {
        let truth = project_truth(&self.root)?;
        if has_staging_residue(&self.root)? {
            return Ok(SessionIndexComparison::RebuildRequired(
                SessionIndexIssue::UnfinishedRebuild,
            ));
        }
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SessionIndexComparison::RebuildRequired(
                    SessionIndexIssue::Missing,
                ));
            }
            Err(error) => return Err(SessionIndexError::Io(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SessionIndexError::UnsafeStorage);
        }
        if metadata.len() > MAX_INDEX_BYTES {
            return Ok(SessionIndexComparison::RebuildRequired(
                SessionIndexIssue::Corrupt,
            ));
        }
        let stored = match read_sqlite(&self.path) {
            Ok(stored) => stored,
            Err(issue) => return Ok(SessionIndexComparison::RebuildRequired(issue)),
        };
        if stored == truth {
            Ok(SessionIndexComparison::Current(stored.snapshot()))
        } else {
            Ok(SessionIndexComparison::RebuildRequired(
                SessionIndexIssue::Stale,
            ))
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct IndexProjection {
    rows: Vec<IndexRow>,
    manifest_sha256: String,
}

impl IndexProjection {
    fn snapshot(&self) -> SessionIndexSnapshot {
        SessionIndexSnapshot {
            session_count: self.rows.len(),
            manifest_sha256: self.manifest_sha256.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct IndexRow {
    session_id: String,
    summary_json: String,
    source_sha256: String,
}

#[derive(Serialize)]
struct IndexedSummary<'a> {
    id: &'a str,
    title: Option<&'a str>,
    cwd: Option<&'a str>,
    runtime: Option<&'a str>,
    source: Option<&'static str>,
    provider: Option<&'a str>,
    status: &'static str,
    parent_id: Option<&'a str>,
    parent_seed_event_count: Option<u64>,
    created_at_ms: Option<i64>,
    last_activity_ms: Option<i64>,
    event_count: u64,
    local_event_count: u64,
    storage: &'static str,
}

fn project_truth(root: &Path) -> Result<IndexProjection, SessionIndexError> {
    let backend = LocalSessionQueryBackend::new(root.to_path_buf());
    let filter = SessionFilter::new()
        .with_storage(SessionStorageFilter::All)
        .with_lineage(SessionLineageFilter::All);
    let summaries = backend.summaries(&filter)?;
    if summaries.len() > MAX_INDEX_ROWS {
        return Err(SessionIndexError::UnsafeStorage);
    }
    let mut rows = Vec::with_capacity(summaries.len());
    for summary in &summaries {
        let session = Session::open(root.join(summary.id().as_str()))
            .map_err(crate::query::map_open_error)?;
        let summary_json = serialize_summary(summary)?;
        if summary_json.len() > MAX_SUMMARY_BYTES {
            return Err(SessionIndexError::Projection);
        }
        rows.push(IndexRow {
            session_id: summary.id().as_str().to_owned(),
            summary_json,
            source_sha256: session.logical_source_sha256(),
        });
    }
    rows.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    let manifest_sha256 = projection_manifest(&rows);
    Ok(IndexProjection {
        rows,
        manifest_sha256,
    })
}

fn serialize_summary(summary: &SessionSummary) -> Result<String, SessionIndexError> {
    let cwd = match summary.cwd() {
        Some(path) => Some(path.to_str().ok_or(SessionIndexError::Projection)?),
        None => None,
    };
    let parent = summary.lineage();
    serde_json::to_string(&IndexedSummary {
        id: summary.id().as_str(),
        title: summary.title(),
        cwd,
        runtime: summary.runtime(),
        source: summary.source().map(source_name),
        provider: summary.provider(),
        status: status_name(summary.status()),
        parent_id: parent.map(|value| value.parent_session_id().as_str()),
        parent_seed_event_count: parent.map(crate::SessionParent::seed_event_count),
        created_at_ms: summary.created_at_ms(),
        last_activity_ms: summary.last_activity_ms(),
        event_count: summary.event_count(),
        local_event_count: summary.local_event_count(),
        storage: storage_name(summary.storage()),
    })
    .map_err(|_| SessionIndexError::Projection)
}

fn projection_manifest(rows: &[IndexRow]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"dshx/session-index-manifest/v1\0");
    digest.update(u64::try_from(rows.len()).unwrap_or(u64::MAX).to_be_bytes());
    for row in rows {
        update_field(&mut digest, row.session_id.as_bytes());
        update_field(&mut digest, row.summary_json.as_bytes());
        update_field(&mut digest, row.source_sha256.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn update_field(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}

fn write_sqlite(path: &Path, projection: &IndexProjection) -> Result<(), SessionIndexError> {
    let mut connection = Connection::open(path).map_err(|_| SessionIndexError::Database)?;
    connection
        .execute_batch(&format!(
            "PRAGMA page_size=4096;\
             PRAGMA journal_mode=OFF;\
             PRAGMA synchronous=OFF;\
             PRAGMA auto_vacuum=NONE;\
             PRAGMA application_id={INDEX_APPLICATION_ID};\
             PRAGMA user_version={INDEX_SCHEMA_VERSION};\
             {INDEX_SCHEMA}"
        ))
        .map_err(|_| SessionIndexError::Database)?;
    let transaction = connection
        .transaction()
        .map_err(|_| SessionIndexError::Database)?;
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO sessions(session_id, summary_json, source_sha256) VALUES (?1, ?2, ?3)",
            )
            .map_err(|_| SessionIndexError::Database)?;
        for row in &projection.rows {
            insert
                .execute(params![
                    &row.session_id,
                    &row.summary_json,
                    &row.source_sha256
                ])
                .map_err(|_| SessionIndexError::Database)?;
        }
    }
    transaction
        .execute(
            "INSERT INTO index_metadata(singleton, schema_version, manifest_sha256, session_count) VALUES (1, ?1, ?2, ?3)",
            params![
                i64::from(INDEX_SCHEMA_VERSION),
                &projection.manifest_sha256,
                i64::try_from(projection.rows.len()).map_err(|_| SessionIndexError::Projection)?
            ],
        )
        .map_err(|_| SessionIndexError::Database)?;
    transaction
        .commit()
        .map_err(|_| SessionIndexError::Database)?;
    connection
        .execute_batch("VACUUM;")
        .map_err(|_| SessionIndexError::Database)?;
    connection
        .close()
        .map_err(|_| SessionIndexError::Database)?;
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn read_sqlite(path: &Path) -> Result<IndexProjection, SessionIndexIssue> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| SessionIndexIssue::Corrupt)?;
    let integrity = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .map_err(|_| SessionIndexIssue::Corrupt)?;
    if integrity != "ok" {
        return Err(SessionIndexIssue::Corrupt);
    }
    let application_id = connection
        .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
        .map_err(|_| SessionIndexIssue::Corrupt)?;
    if application_id != INDEX_APPLICATION_ID {
        return Err(SessionIndexIssue::Corrupt);
    }
    let schema = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|_| SessionIndexIssue::Corrupt)?;
    let schema = u32::try_from(schema).map_err(|_| SessionIndexIssue::Corrupt)?;
    if schema != INDEX_SCHEMA_VERSION {
        return Err(SessionIndexIssue::IncompatibleSchema { found: schema });
    }
    let (declared_schema, manifest_sha256, session_count) = connection
        .query_row(
            "SELECT schema_version, manifest_sha256, session_count FROM index_metadata WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(|_| SessionIndexIssue::Corrupt)?;
    if declared_schema != i64::from(INDEX_SCHEMA_VERSION) || !valid_sha256(&manifest_sha256) {
        return Err(SessionIndexIssue::Corrupt);
    }
    let session_count = usize::try_from(session_count).map_err(|_| SessionIndexIssue::Corrupt)?;
    if session_count > MAX_INDEX_ROWS {
        return Err(SessionIndexIssue::Corrupt);
    }

    let mut statement = connection
        .prepare("SELECT session_id, summary_json, source_sha256 FROM sessions ORDER BY session_id")
        .map_err(|_| SessionIndexIssue::Corrupt)?;
    let mut query = statement
        .query([])
        .map_err(|_| SessionIndexIssue::Corrupt)?;
    let mut rows = Vec::with_capacity(session_count);
    while let Some(row) = query.next().map_err(|_| SessionIndexIssue::Corrupt)? {
        if rows.len() >= MAX_INDEX_ROWS {
            return Err(SessionIndexIssue::Corrupt);
        }
        let session_id = row
            .get::<_, String>(0)
            .map_err(|_| SessionIndexIssue::Corrupt)?;
        let summary_json = row
            .get::<_, String>(1)
            .map_err(|_| SessionIndexIssue::Corrupt)?;
        let source_sha256 = row
            .get::<_, String>(2)
            .map_err(|_| SessionIndexIssue::Corrupt)?;
        if !crate::creation::valid_session_component(&session_id)
            || summary_json.len() > MAX_SUMMARY_BYTES
            || !valid_sha256(&source_sha256)
        {
            return Err(SessionIndexIssue::Corrupt);
        }
        rows.push(IndexRow {
            session_id,
            summary_json,
            source_sha256,
        });
    }
    if rows.len() != session_count || projection_manifest(&rows) != manifest_sha256 {
        return Err(SessionIndexIssue::Corrupt);
    }
    Ok(IndexProjection {
        rows,
        manifest_sha256,
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_existing_index_path(path: &Path) -> Result<(), SessionIndexError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(SessionIndexError::UnsafeStorage)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SessionIndexError::Io(error)),
    }
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), SessionIndexError> {
    let mut options = AtomicWriteFile::options();
    #[cfg(unix)]
    {
        use atomic_write_file::unix::OpenOptionsExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        options.preserve_mode(false).mode(0o600);
    }
    let mut output = options.open(path)?;
    output.write_all(bytes)?;
    output.commit()?;
    Ok(())
}

struct StagingPath {
    path: PathBuf,
}

impl StagingPath {
    fn create(root: &Path) -> Result<Self, SessionIndexError> {
        let name = format!(
            "{STAGING_PREFIX}{}{STAGING_SUFFIX}",
            uuid::Uuid::new_v4().simple()
        );
        let path = root.join(name);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        file.sync_all()?;
        drop(file);
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn has_staging_residue(root: &Path) -> Result<bool, SessionIndexError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if staging_name(&name) {
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(SessionIndexError::UnsafeStorage);
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn remove_staging_residue(root: &Path) -> Result<(), SessionIndexError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if staging_name(&name) {
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_INDEX_BYTES
            {
                return Err(SessionIndexError::UnsafeStorage);
            }
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn staging_name(name: &str) -> bool {
    name.starts_with(STAGING_PREFIX)
        && name.ends_with(STAGING_SUFFIX)
        && name.len() > STAGING_PREFIX.len() + STAGING_SUFFIX.len()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

const fn source_name(source: SessionSource) -> &'static str {
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

const fn status_name(status: SessionActivityStatus) -> &'static str {
    match status {
        SessionActivityStatus::Empty => "empty",
        SessionActivityStatus::Idle => "idle",
        SessionActivityStatus::OpenTurn => "open_turn",
    }
}

const fn storage_name(storage: SessionStorageState) -> &'static str {
    match storage {
        SessionStorageState::Active => "active",
        SessionStorageState::Archived => "archived",
    }
}

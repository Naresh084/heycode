//! Durable session state: the append-only JSONL log with resume.

#[cfg(unix)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
#[cfg(unix)]
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

#[cfg(unix)]
use std::os::fd::AsRawFd;

use chrono::Utc;
use heycode_core::{EventBus, SessionId};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::event::{
    CURRENT_SESSION_LOG_VERSION, KNOWN_KINDS_V1, KNOWN_KINDS_V2, MIN_SESSION_LOG_VERSION,
    SessionEvent, SessionEventKind,
};
use crate::{
    InboxProjection, SessionCreation, SessionCreationMetadata, SessionMetadataError, SessionParent,
    SessionSource,
};

/// Log file name inside a session directory.
pub(crate) const LOG_FILE_NAME: &str = "session.jsonl";
pub(crate) const FORK_STAGING_PREFIX: &str = ".heycode-fork-";
const MAX_SESSION_LOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LOGICAL_SESSION_EVENTS: usize = 1_000_000;
const MAX_LINEAGE_DEPTH: usize = 64;
const LINEAGE_LOCK_FILE_NAME: &str = ".lineage.lock";

/// Inclusive prefix selection for a shared-prefix fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkBoundary {
    /// Inherit the complete current log; an empty parent yields an empty prefix.
    Latest,
    /// Inherit through this exact event sequence, inclusive.
    Through(u64),
    /// Inherit exactly this many leading logical events. Zero is valid and is
    /// used when a fork begins while the parent's first turn is still open.
    EventCount(u64),
}

/// Failure to create one shared-prefix child.
#[derive(Debug, Error)]
pub enum ForkError {
    /// Requested event does not exist.
    #[error("invalid fork boundary")]
    InvalidBoundary {
        /// Requested inclusive sequence, when explicit.
        requested: Option<u64>,
        /// Parent's latest sequence, when non-empty.
        latest: Option<u64>,
    },
    /// Prefix ends before one turn closes.
    #[error("fork boundary ends inside open turn {turn}")]
    OpenTurn {
        /// Unclosed turn.
        turn: u64,
    },
    /// Child lineage plus creation event would exceed the logical event cap.
    #[error("fork prefix exceeds logical event limit")]
    TooManyEvents,
    /// Child would exceed the supported shared-prefix ancestry depth.
    #[error("fork lineage exceeds ancestor limit")]
    LineageTooDeep,
    /// Shared prefixes require parent and child under one sessions root.
    #[error("fork sessions root does not own the parent")]
    WrongRoot,
    /// Creation metadata/lineage failed validation.
    #[error(transparent)]
    Metadata(#[from] SessionMetadataError),
    /// Constructor-owned child event could not serialize.
    #[error("session fork creation event failed to serialize")]
    Serialize(#[from] serde_json::Error),
    /// Parent prefix could not rebuild an operational projection.
    #[error("session prefix projection failed")]
    Projection,
    /// Child storage could not be created durably.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Failure to create a metadata-bearing root session.
#[derive(Debug, Error)]
pub enum CreateError {
    /// Creation metadata is inconsistent.
    #[error(transparent)]
    Metadata(#[from] SessionMetadataError),
    /// Storage creation or first append failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The constructor-owned first event could not commit.
    #[error(transparent)]
    Append(#[from] AppendError),
}

/// Failures when resuming an existing session log.
#[derive(Debug, Error)]
pub enum OpenError {
    /// A line is not valid JSON or misses required envelope fields.
    #[error("session log line {line_no}: invalid JSON envelope")]
    CorruptLine {
        /// One-based line number of the offender.
        line_no: usize,
        /// Underlying parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// Non-empty append-only logs must end at a complete line boundary.
    #[error("session log has an unterminated final line")]
    UnterminatedTail,

    /// The log was written by an incompatible format version.
    #[error(
        "session log format version {found} is not supported (supported: {minimum}..={maximum}); use a heycode build supporting this version — the log was not modified"
    )]
    UnsupportedVersion {
        /// Version found on the offending line.
        found: u64,
        /// Oldest readable version.
        minimum: u8,
        /// Current writable/readable version.
        maximum: u8,
    },

    /// A log may migrate from a v1 prefix to v2 appends, never back to v1.
    #[error("session log version regressed at line {line_no}: previous {previous}, found {found}")]
    VersionRegression {
        /// One-based line number of the lower-version line.
        line_no: usize,
        /// Prior envelope version.
        previous: u8,
        /// Lower version found.
        found: u8,
    },

    /// The log contains a kind this build does not know; it may come from a
    /// newer heycode. Never skipped (AGENTS.md §4).
    #[error("unknown session event kind `{kind}` at line {line_no}")]
    UnknownKind {
        /// One-based line number of the offender.
        line_no: usize,
        /// The unrecognized kind tag.
        kind: String,
    },

    /// Sequence numbers must be contiguous from 0; a mismatch means the log
    /// was truncated or tampered with.
    #[error("session log sequence gap: expected {expected}, found {found}")]
    SeqGap {
        /// Sequence number the reader required at that position.
        expected: u64,
        /// Sequence number actually found.
        found: u64,
    },

    /// A known event payload violated its version-specific semantic contract.
    #[error("invalid session event at line {line_no}: {message}")]
    InvalidEvent {
        /// One-based line number.
        line_no: usize,
        /// Safe validation detail.
        message: String,
    },

    /// Session directory or log is a symlink/non-regular path.
    #[error("unsafe session storage path")]
    UnsafePath,

    /// Another live process already holds this log's writer lease. Only one
    /// writable handle per log may exist across processes; a second one would
    /// mint the same sequence numbers and destroy the log.
    #[error("session is already open for writing by another heycode process")]
    AlreadyOpen,

    /// Log exceeds the bounded reader limit.
    #[error("session log exceeds {maximum_bytes} bytes")]
    LogTooLarge {
        /// Maximum accepted bytes.
        maximum_bytes: u64,
    },

    /// Logical inherited plus local event count exceeds the reader bound.
    #[error("session log exceeds {maximum} logical events")]
    TooManyEvents {
        /// Maximum accepted logical events.
        maximum: usize,
    },

    /// Parent id cannot be resolved beneath the same sessions root.
    #[error("fork parent session `{parent}` is missing")]
    LineageParentMissing {
        /// Missing safe parent id.
        parent: SessionId,
    },

    /// Parent references eventually return to an already-open descendant.
    #[error("session lineage cycle at `{session}`")]
    LineageCycle {
        /// Repeated safe session id.
        session: SessionId,
    },

    /// Lineage chain exceeds the bounded recursion depth.
    #[error("session lineage exceeds {maximum} ancestors")]
    LineageTooDeep {
        /// Maximum traversed ancestors.
        maximum: usize,
    },

    /// Parent prefix no longer matches the child's committed proof.
    #[error("session `{session}` lineage prefix proof does not match")]
    LineageDigestMismatch {
        /// Child whose proof failed.
        session: SessionId,
    },

    /// Creation/lineage position or identity is inconsistent.
    #[error("invalid session lineage at line {line_no}")]
    InvalidLineage {
        /// Physical child-log line.
        line_no: usize,
    },

    /// An underlying filesystem failure (missing log, unreadable dir, ...).
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Failures when appending to the session log. Nothing is published to the
/// bus unless the durable write succeeded.
#[derive(Debug, Error)]
pub enum AppendError {
    /// The durable write failed.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// Event payload failed to serialize; payloads are JSON-native so this
    /// indicates an internal defect rather than bad input.
    #[error("session event failed to serialize")]
    Serialize(#[from] serde_json::Error),

    /// Typed event violated a current-version semantic contract.
    #[error("invalid session event: {message}")]
    InvalidEvent {
        /// Safe validation detail.
        message: String,
    },

    /// The line would push the physical log past the bound [`Session::open`]
    /// enforces, so committing it would make this session unopenable.
    #[error("session log would exceed {maximum_bytes} bytes; fork or start a new session")]
    LogFull {
        /// Maximum physical bytes a readable log may occupy.
        maximum_bytes: u64,
    },

    /// The batch would push the logical event count past the bound
    /// [`Session::open`] enforces.
    #[error("session log would exceed {maximum} logical events; fork or start a new session")]
    TooManyEvents {
        /// Maximum logical events a readable log may carry.
        maximum: usize,
    },

    /// A newer writable handle in this process took the log's writer lease,
    /// so this handle's sequence numbers are no longer authoritative.
    #[error("session handle was superseded by a newer writer")]
    Superseded,
}

/// Guidance for opening one validated session with an older format reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDowngradeGuidance {
    /// Every durable line is readable by the selected reader.
    Compatible {
        /// Newest envelope version the selected reader understands.
        reader_max_version: u8,
    },
    /// The append-only log contains a newer line. Use a reader supporting that
    /// line or restore a copy retained before it was appended; never truncate
    /// or rewrite the durable log in place.
    RestorePreUpgradeCopyOrUseNewerBinary {
        /// Newest envelope version the selected reader understands.
        reader_max_version: u8,
        /// First logical sequence the reader cannot understand.
        first_incompatible_seq: u64,
        /// Envelope version required by that first incompatible line.
        required_version: u8,
    },
}

/// Invalid request for session-version compatibility guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionVersionGuidanceError {
    /// Session v1 is the oldest supported persisted representation.
    #[error("session reader version must be at least {minimum}")]
    ReaderTooOld {
        /// Oldest supported session reader.
        minimum: u8,
    },
}

/// Envelope head read before any kind reconstruction: validates the four
/// envelope fields with native serde errors and exposes `kind` as a plain
/// string so unknown tags fail loudly before enum deserialization.
#[derive(Deserialize)]
struct EnvelopeHead {
    v: u64,
    seq: u64,
    time_ms: i64,
    kind: String,
}

/// A durable append-only session log rooted at `<dir>/<session-id>/session.jsonl`.
///
/// Appends are write+flush before anything is published: listeners on the
/// shared [`EventBus`] only ever see committed lines (publish at commit
/// point, AGENTS.md principle 8). Resume replays the full file and continues
/// appending with contiguous sequence numbers. Forks expose one logical event
/// stream while their physical JSONL contains only `session/created` lineage
/// plus the child suffix.
pub struct Session {
    id: SessionId,
    path: PathBuf,
    file: fs::File,
    events: Vec<SessionEvent>,
    event_hashes: Vec<[u8; 32]>,
    inbox: InboxProjection,
    creation: Option<SessionCreation>,
    first_local_seq: u64,
    physical_bytes: u64,
    writer: Option<WriterLease>,
    bus: EventBus,
}

impl Session {
    /// Create a fresh session under `dir` (the sessions root): mints a new
    /// [`SessionId`], creates `<dir>/<id>/`, and opens a brand-new
    /// `session.jsonl` (fails if it already exists).
    ///
    /// # Errors
    /// Propagates filesystem failures from directory creation or file
    /// creation (`AlreadyExists` included — never truncate an existing log).
    pub fn create(dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::create_with_id(dir, SessionId::generate())
    }

    /// Create one fresh session using a caller-minted opaque id.
    ///
    /// This is the product-composition seam for capabilities such as MCP that
    /// must bind an exact session route before plugins apply. The child
    /// directory is create-new; an existing id never becomes an append target.
    ///
    /// # Errors
    /// Invalid session identity, root/child creation failure, collision, or
    /// log-file creation failure.
    pub fn create_with_id(dir: impl AsRef<Path>, id: SessionId) -> io::Result<Self> {
        if !crate::creation::valid_session_component(id.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session id is invalid",
            ));
        }
        fs::create_dir_all(dir.as_ref())?;
        let session_dir = dir.as_ref().join(id.as_str());
        fs::create_dir(&session_dir)?;
        let path = session_dir.join(LOG_FILE_NAME);
        let file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) => {
                let _ = fs::remove_dir(&session_dir);
                return Err(error);
            }
        };
        let lease = lock_session_shared(&file).and_then(|()| acquire_writer_lease(&session_dir));
        let writer = match lease {
            // A directory created moments ago by this call cannot already be
            // owned by another process, so `None` is unreachable; treat it as
            // a lock failure rather than inventing an unleased writer.
            Ok(Some(pending)) => pending.commit(),
            Ok(None) => {
                drop(file);
                let _ = fs::remove_file(&path);
                let _ = fs::remove_dir(&session_dir);
                return Err(io::Error::other("new session log is already locked"));
            }
            Err(error) => {
                drop(file);
                let _ = fs::remove_file(&path);
                let _ = fs::remove_dir(&session_dir);
                return Err(error);
            }
        };
        Ok(Self {
            id,
            path,
            file,
            events: Vec::new(),
            event_hashes: Vec::new(),
            inbox: InboxProjection::default(),
            creation: None,
            first_local_seq: 0,
            physical_bytes: 0,
            writer: Some(writer),
            bus: EventBus::default(),
        })
    }

    /// Create a fresh root session and commit immutable safe metadata as its
    /// first v2 event.
    ///
    /// # Errors
    /// Metadata claims fork origin, storage creation fails, or the first
    /// durable append fails.
    pub fn create_with_metadata(
        dir: impl AsRef<Path>,
        metadata: SessionCreationMetadata,
    ) -> Result<Self, CreateError> {
        let creation = SessionCreation::new(metadata)?;
        let session = Self::create(dir)?;
        Self::commit_creation(session, creation)
    }

    /// Create a fresh metadata-bearing session with a caller-minted id.
    ///
    /// # Errors
    /// Invalid id/metadata, storage collision/failure, or first-event commit
    /// failure. A failed metadata commit cleans up the new empty stream.
    pub fn create_with_id_and_metadata(
        dir: impl AsRef<Path>,
        id: SessionId,
        metadata: SessionCreationMetadata,
    ) -> Result<Self, CreateError> {
        let creation = SessionCreation::new(metadata)?;
        let session = Self::create_with_id(dir, id).map_err(CreateError::Io)?;
        Self::commit_creation(session, creation)
    }

    fn commit_creation(mut session: Self, creation: SessionCreation) -> Result<Self, CreateError> {
        match session.append_creation(creation) {
            Ok(_) => Ok(session),
            Err(error) => {
                let path = session.path.clone();
                let directory = path.parent().map(Path::to_path_buf);
                let _ = fs::remove_file(path);
                if let Some(directory) = directory {
                    let _ = fs::remove_dir(directory);
                }
                Err(CreateError::Append(error))
            }
        }
    }

    /// Resume the session living in `session_dir`: reads `session.jsonl`
    /// fully, validates every envelope line, and returns a session whose
    /// next append continues the sequence. The id is recovered from the
    /// directory name.
    ///
    /// # Errors
    /// - [`OpenError::CorruptLine`] — unparsable JSON or missing envelope fields
    /// - [`OpenError::UnsupportedVersion`] — envelope `v` outside 1..=2
    /// - [`OpenError::VersionRegression`] — a v1 line follows a v2 line
    /// - [`OpenError::UnknownKind`] — kind tag outside the closed set
    /// - [`OpenError::SeqGap`] — non-contiguous sequence numbers
    /// - [`OpenError::InvalidEvent`] — a payload or cross-event projection is invalid
    /// - [`OpenError::UnsafePath`] — symlink/non-regular storage or unsafe id
    /// - lineage errors — missing/cyclic/mutated/unbounded parent prefix
    /// - size/tail errors — unbounded or incomplete append-only input
    /// - [`OpenError::Io`] — missing/unreadable log or unusable directory name
    pub fn open(session_dir: impl AsRef<Path>) -> Result<Self, OpenError> {
        let mut visited = HashSet::new();
        Self::open_inner(session_dir.as_ref(), &mut visited)
    }

    /// Resume a session as its single writer: identical to [`Session::open`]
    /// but takes the log's exclusive writer lease first.
    ///
    /// Two processes appending to one log both derive `seq` from their own
    /// replay, so both mint the same sequence numbers and the log stops
    /// opening forever. Every path that will append — resume, create, fork —
    /// goes through a lease; read-only consumers (listing, export, repair)
    /// keep using [`Session::open`] so the picker never blocks on a live
    /// session.
    ///
    /// # Errors
    /// [`OpenError::AlreadyOpen`] when another process owns the log, plus
    /// every failure [`Session::open`] reports.
    pub fn open_for_writing(session_dir: impl AsRef<Path>) -> Result<Self, OpenError> {
        let session_dir = session_dir.as_ref();
        // The cross-process lock is reserved before replay so a contended
        // resume fails with the actionable error instead of a torn read of the
        // live writer. It only becomes THIS handle's lease once the replay
        // succeeded, so a failed resume leaves an older writable handle in
        // this process exactly as current as it was.
        let Some(pending) = acquire_writer_lease(session_dir)? else {
            return Err(OpenError::AlreadyOpen);
        };
        let mut session = Self::open(session_dir)?;
        session.writer = Some(pending.commit());
        Ok(session)
    }

    fn open_inner(dir: &Path, visited: &mut HashSet<String>) -> Result<Self, OpenError> {
        let id = match dir.file_name().and_then(|name| name.to_str()) {
            Some(name) if crate::creation::valid_session_component(name) => {
                SessionId::from_raw(name)
            }
            _ => return Err(OpenError::UnsafePath),
        };
        if visited.len() > MAX_LINEAGE_DEPTH {
            return Err(OpenError::LineageTooDeep {
                maximum: MAX_LINEAGE_DEPTH,
            });
        }
        if !visited.insert(id.as_str().to_owned()) {
            return Err(OpenError::LineageCycle { session: id });
        }
        let result = Self::open_inner_loaded(dir, id.clone(), visited);
        visited.remove(id.as_str());
        result
    }

    fn open_inner_loaded(
        dir: &Path,
        id: SessionId,
        visited: &mut HashSet<String>,
    ) -> Result<Self, OpenError> {
        let directory_metadata = fs::symlink_metadata(dir)?;
        if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
            return Err(OpenError::UnsafePath);
        }
        let path = dir.join(LOG_FILE_NAME);
        let log_metadata = fs::symlink_metadata(&path)?;
        if log_metadata.file_type().is_symlink() || !log_metadata.is_file() {
            return Err(OpenError::UnsafePath);
        }
        if log_metadata.len() > MAX_SESSION_LOG_BYTES {
            return Err(OpenError::LogTooLarge {
                maximum_bytes: MAX_SESSION_LOG_BYTES,
            });
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        lock_session_shared(&file)?;
        let raw = fs::read_to_string(&path)?;
        if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_SESSION_LOG_BYTES {
            return Err(OpenError::LogTooLarge {
                maximum_bytes: MAX_SESSION_LOG_BYTES,
            });
        }
        if !raw.is_empty() && !raw.ends_with('\n') {
            return Err(OpenError::UnterminatedTail);
        }
        let lines = if raw.is_empty() {
            Vec::new()
        } else {
            raw[..raw.len() - 1].split('\n').collect::<Vec<_>>()
        };
        let mut events = Vec::new();
        let mut event_hashes = Vec::new();
        let mut first_local_seq = 0_u64;
        let mut creation = None;

        if let Some(first_line) = lines.first() {
            let first = Self::parse_line_unsequenced(1, first_line)?;
            if let SessionEventKind::SessionCreated {
                creation: candidate,
            } = &first.kind
                && let Some(parent) = candidate.parent()
            {
                if first.seq != parent.seed_event_count()
                    || parent.parent_session_id() == &id
                    || parent.seed_event_count()
                        >= u64::try_from(MAX_LOGICAL_SESSION_EVENTS).unwrap_or(u64::MAX)
                {
                    return Err(OpenError::InvalidLineage { line_no: 1 });
                }
                let Some(root) = dir.parent() else {
                    return Err(OpenError::InvalidLineage { line_no: 1 });
                };
                let parent_dir = root.join(parent.parent_session_id().as_str());
                match fs::symlink_metadata(&parent_dir) {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        return Err(OpenError::LineageParentMissing {
                            parent: parent.parent_session_id().clone(),
                        });
                    }
                    Err(error) => return Err(OpenError::Io(error)),
                }
                let parent_session = Self::open_inner(&parent_dir, visited)?;
                let seed_count = usize::try_from(parent.seed_event_count())
                    .map_err(|_| OpenError::InvalidLineage { line_no: 1 })?;
                if seed_count > parent_session.events.len() {
                    return Err(OpenError::InvalidLineage { line_no: 1 });
                }
                let prefix = &parent_session.events[..seed_count];
                if seed_count > parent_session.event_hashes.len() {
                    return Err(OpenError::InvalidLineage { line_no: 1 });
                }
                let prefix_hashes = &parent_session.event_hashes[..seed_count];
                let digest = prefix_sha256(prefix_hashes);
                if digest != parent.prefix_sha256() {
                    return Err(OpenError::LineageDigestMismatch {
                        session: id.clone(),
                    });
                }
                events.extend_from_slice(prefix);
                event_hashes.extend_from_slice(prefix_hashes);
                first_local_seq = parent.seed_event_count();
                creation = Some(candidate.as_ref().clone());
            }
        }

        let mut inbox =
            crate::project_inbox(&events).map_err(|_| OpenError::InvalidLineage { line_no: 1 })?;
        let mut previous_version = events.last().map(|event| event.v);
        for (idx, line) in lines.iter().enumerate() {
            let line_no = idx + 1;
            let expected_seq = first_local_seq
                .checked_add(u64::try_from(idx).map_err(|_| OpenError::InvalidLineage { line_no })?)
                .ok_or(OpenError::InvalidLineage { line_no })?;
            let event = Self::parse_line(line_no, line, expected_seq)?;
            if let Some(previous) = previous_version
                && event.v < previous
            {
                return Err(OpenError::VersionRegression {
                    line_no,
                    previous,
                    found: event.v,
                });
            }
            previous_version = Some(event.v);
            match &event.kind {
                SessionEventKind::SessionCreated {
                    creation: candidate,
                } if idx == 0 => {
                    if candidate.parent().is_none() && event.seq != 0 {
                        return Err(OpenError::InvalidLineage { line_no });
                    }
                    creation = Some(candidate.as_ref().clone());
                }
                SessionEventKind::SessionCreated { .. } => {
                    return Err(OpenError::InvalidLineage { line_no });
                }
                _ => {}
            }
            inbox
                .apply_event(&event)
                .map_err(|error| OpenError::InvalidEvent {
                    line_no,
                    message: error.to_string(),
                })?;
            events.push(event);
            event_hashes.push(event_leaf_sha256(line.as_bytes()));
            if events.len() > MAX_LOGICAL_SESSION_EVENTS {
                return Err(OpenError::TooManyEvents {
                    maximum: MAX_LOGICAL_SESSION_EVENTS,
                });
            }
        }

        crate::project_code_mode(&events).map_err(|message| OpenError::InvalidEvent {
            line_no: 1,
            message,
        })?;
        validate_user_attachment_sequence(&events)
            .map_err(|(line_no, message)| OpenError::InvalidEvent { line_no, message })?;
        validate_assistant_audio_sequence(&events)
            .map_err(|(line_no, message)| OpenError::InvalidEvent { line_no, message })?;

        Ok(Self {
            id,
            path,
            file,
            events,
            event_hashes,
            inbox,
            creation,
            first_local_seq,
            physical_bytes: u64::try_from(raw.len()).unwrap_or(u64::MAX),
            writer: None,
            bus: EventBus::default(),
        })
    }

    /// Validate one envelope line and rebuild its event. Validation order:
    /// JSON parse → envelope head → version gate → closed-kind gate →
    /// sequence contiguity → kind reconstruction.
    fn parse_line(
        line_no: usize,
        line: &str,
        expected_seq: u64,
    ) -> Result<SessionEvent, OpenError> {
        let event = Self::parse_line_unsequenced(line_no, line)?;
        if event.seq != expected_seq {
            return Err(OpenError::SeqGap {
                expected: expected_seq,
                found: event.seq,
            });
        }
        validate_compaction_boundary(&event.kind, event.seq)
            .map_err(|message| OpenError::InvalidEvent { line_no, message })?;
        Ok(event)
    }

    fn parse_line_unsequenced(line_no: usize, line: &str) -> Result<SessionEvent, OpenError> {
        let value: Value = serde_json::from_str(line)
            .map_err(|source| OpenError::CorruptLine { line_no, source })?;
        let head: EnvelopeHead = serde_json::from_value(value.clone())
            .map_err(|source| OpenError::CorruptLine { line_no, source })?;

        let (source_version, known_kinds) = match head.v {
            1 => (1_u8, KNOWN_KINDS_V1),
            2 => (2_u8, KNOWN_KINDS_V2),
            _ => {
                return Err(OpenError::UnsupportedVersion {
                    found: head.v,
                    minimum: MIN_SESSION_LOG_VERSION,
                    maximum: CURRENT_SESSION_LOG_VERSION,
                });
            }
        };
        if !known_kinds.contains(&head.kind.as_str()) {
            return Err(OpenError::UnknownKind {
                line_no,
                kind: head.kind,
            });
        }
        let kind: SessionEventKind = serde_json::from_value(value)
            .map_err(|source| OpenError::CorruptLine { line_no, source })?;
        kind.validate_for_version(source_version)
            .map_err(|message| OpenError::InvalidEvent { line_no, message })?;
        Ok(SessionEvent {
            v: source_version,
            seq: head.seq,
            time_ms: head.time_ms,
            kind,
        })
    }

    /// The session id (recovered from the directory name on resume).
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Path of this stream's physical local-suffix `session.jsonl`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Complete logical events in sequence order, including a verified
    /// inherited prefix for forks.
    #[must_use]
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// Generate downgrade guidance from the exact source versions retained on
    /// every validated event.
    ///
    /// This never changes JSONL. A mixed v1/v2 log tells a v1 reader to use a
    /// pre-v2 copy or a newer binary rather than pretending the v2 suffix can
    /// be reverse-migrated.
    ///
    /// # Errors
    /// `reader_max_version` predates the oldest supported session schema.
    pub fn downgrade_guidance(
        &self,
        reader_max_version: u8,
    ) -> Result<SessionDowngradeGuidance, SessionVersionGuidanceError> {
        if reader_max_version < MIN_SESSION_LOG_VERSION {
            return Err(SessionVersionGuidanceError::ReaderTooOld {
                minimum: MIN_SESSION_LOG_VERSION,
            });
        }
        match self
            .events
            .iter()
            .find(|event| event.v > reader_max_version)
        {
            Some(event) => Ok(
                SessionDowngradeGuidance::RestorePreUpgradeCopyOrUseNewerBinary {
                    reader_max_version,
                    first_incompatible_seq: event.seq,
                    required_version: event.v,
                },
            ),
            None => Ok(SessionDowngradeGuidance::Compatible { reader_max_version }),
        }
    }

    pub(crate) fn logical_source_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"dshx/session-index-source/v1\0");
        digest.update(
            u64::try_from(self.event_hashes.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        for hash in &self.event_hashes {
            digest.update(hash);
        }
        format!("{:x}", digest.finalize())
    }

    /// Current pending-input state and durable claim/cancellation accounting.
    #[must_use]
    pub const fn inbox(&self) -> &InboxProjection {
        &self.inbox
    }

    /// Safe immutable metadata for this physical stream, when recorded.
    #[must_use]
    pub fn metadata(&self) -> Option<&SessionCreationMetadata> {
        self.creation.as_ref().map(SessionCreation::metadata)
    }

    /// Verified shared-prefix lineage for a fork.
    #[must_use]
    pub fn lineage(&self) -> Option<&SessionParent> {
        self.creation.as_ref().and_then(SessionCreation::parent)
    }

    /// First sequence physically stored in this stream's own JSONL file.
    #[must_use]
    pub const fn first_local_seq(&self) -> u64 {
        self.first_local_seq
    }

    /// Whether this root can still start its first runtime session.
    ///
    /// Configuration audit events may precede a failed child start. They do
    /// not make the root resumable until a runtime link or conversation work
    /// is durable.
    ///
    /// Forks are never fresh even when their inherited prefix is empty.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        self.lineage().is_none()
            && self.events.iter().all(|event| {
                matches!(
                    &event.kind,
                    SessionEventKind::SessionCreated { .. }
                        | SessionEventKind::RuntimeConfigured { .. }
                )
            })
    }

    /// Force this stream's already-appended suffix to stable storage.
    ///
    /// # Errors
    /// The operating system cannot complete the durability checkpoint.
    pub fn flush(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// Read the exact physical JSONL suffix only when it still matches this
    /// validated in-memory snapshot.
    ///
    /// # Errors
    /// The path became unsafe/unreadable, exceeded the log bound, or changed
    /// after this [`Session`] was opened.
    pub(crate) fn validated_raw_suffix(&self) -> io::Result<Vec<u8>> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_SESSION_LOG_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session suffix path is unsafe",
            ));
        }
        let raw = fs::read(&self.path)?;
        if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_SESSION_LOG_BYTES
            || (!raw.is_empty() && !raw.ends_with(b"\n"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session suffix is not a complete bounded JSONL stream",
            ));
        }
        let start = usize::try_from(self.first_local_seq).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session suffix boundary is invalid",
            )
        })?;
        let expected = self.event_hashes.get(start..).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session suffix boundary is invalid",
            )
        })?;
        let actual = if raw.is_empty() {
            Vec::new()
        } else {
            raw[..raw.len() - 1]
                .split(|byte| *byte == b'\n')
                .map(event_leaf_sha256)
                .collect::<Vec<_>>()
        };
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session suffix changed after validation",
            ));
        }
        Ok(raw)
    }

    /// Create a child whose JSONL stores only a creation/lineage event and
    /// later suffix while logical replay verifies and stitches the parent prefix.
    ///
    /// # Errors
    /// Boundary is absent/open, root differs from the parent root, prefix
    /// proof/projection fails, or child storage cannot commit.
    pub fn fork(
        &self,
        sessions_root: impl AsRef<Path>,
        boundary: ForkBoundary,
    ) -> Result<Self, ForkError> {
        let metadata = match self.metadata() {
            Some(metadata) => metadata.inherited_for_fork(),
            None => SessionCreationMetadata::new(None, None, SessionSource::Fork)?,
        };
        self.fork_with_metadata(sessions_root, boundary, metadata)
    }

    /// Fork a verified prefix while recording the child's actual workspace/runtime.
    /// `metadata` must have source Fork; the lineage proof remains constructor-owned.
    ///
    /// # Errors
    /// Invalid metadata, open boundary, invalid prefix, or durable storage failure.
    pub fn fork_with_metadata(
        &self,
        sessions_root: impl AsRef<Path>,
        boundary: ForkBoundary,
        metadata: SessionCreationMetadata,
    ) -> Result<Self, ForkError> {
        let sessions_root = sessions_root.as_ref();
        let parent_root = self
            .path
            .parent()
            .and_then(Path::parent)
            .ok_or(ForkError::WrongRoot)?;
        if parent_root != sessions_root {
            return Err(ForkError::WrongRoot);
        }
        let _lineage_lock = lock_lineage_mutation(sessions_root)?;
        self.flush()?;
        let lineage_depth = self
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.kind,
                    SessionEventKind::SessionCreated { creation }
                        if creation.parent().is_some()
                )
            })
            .count();
        if lineage_depth >= MAX_LINEAGE_DEPTH {
            return Err(ForkError::LineageTooDeep);
        }
        let seed_count = match boundary {
            ForkBoundary::Latest => self.events.len(),
            ForkBoundary::Through(requested) => {
                let index = usize::try_from(requested).map_err(|_| ForkError::InvalidBoundary {
                    requested: Some(requested),
                    latest: self.events.last().map(|event| event.seq),
                })?;
                if self
                    .events
                    .get(index)
                    .is_none_or(|event| event.seq != requested)
                {
                    return Err(ForkError::InvalidBoundary {
                        requested: Some(requested),
                        latest: self.events.last().map(|event| event.seq),
                    });
                }
                index + 1
            }
            ForkBoundary::EventCount(requested) => {
                let count = usize::try_from(requested).map_err(|_| ForkError::InvalidBoundary {
                    requested: Some(requested),
                    latest: self.events.last().map(|event| event.seq),
                })?;
                if count > self.events.len() {
                    return Err(ForkError::InvalidBoundary {
                        requested: Some(requested),
                        latest: self.events.last().map(|event| event.seq),
                    });
                }
                count
            }
        };
        let prefix = &self.events[..seed_count];
        if seed_count >= MAX_LOGICAL_SESSION_EVENTS {
            return Err(ForkError::TooManyEvents);
        }
        if let Some(turn) = open_turn(prefix) {
            return Err(ForkError::OpenTurn { turn });
        }
        if validate_user_attachment_sequence(prefix).is_err() {
            return Err(ForkError::Projection);
        }
        let seed_event_count =
            u64::try_from(seed_count).map_err(|_| ForkError::InvalidBoundary {
                requested: match boundary {
                    ForkBoundary::Latest => None,
                    ForkBoundary::Through(value) | ForkBoundary::EventCount(value) => Some(value),
                },
                latest: self.events.last().map(|event| event.seq),
            })?;
        if seed_count > self.event_hashes.len() {
            return Err(ForkError::Projection);
        }
        let digest = prefix_sha256(&self.event_hashes[..seed_count]);
        let parent = SessionParent::new(self.id.clone(), seed_event_count, digest)?;
        let creation = SessionCreation::fork(metadata, parent)?;
        let event = SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq: seed_event_count,
            time_ms: Utc::now().timestamp_millis(),
            kind: SessionEventKind::SessionCreated {
                creation: Box::new(creation.clone()),
            },
        };
        let mut line = serde_json::to_string(&event)?;
        let creation_hash = event_leaf_sha256(line.as_bytes());
        line.push('\n');
        let mut inbox = crate::project_inbox(prefix).map_err(|_| ForkError::Projection)?;
        inbox
            .apply_event(&event)
            .map_err(|_| ForkError::Projection)?;

        let child_id = SessionId::generate();
        let child_dir = sessions_root.join(child_id.as_str());
        let staging_dir =
            sessions_root.join(format!("{FORK_STAGING_PREFIX}{}.tmp", child_id.as_str()));
        fs::create_dir(&staging_dir)?;
        let staging_path = staging_dir.join(LOG_FILE_NAME);
        let file_result = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&staging_path);
        let mut file = match file_result {
            Ok(file) => file,
            Err(error) => {
                let _ = fs::remove_dir(&staging_dir);
                return Err(ForkError::Io(error));
            }
        };
        lock_session_shared(&file)?;
        if let Err(error) = file
            .write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_data())
            .and_then(|()| sync_session_directory(&staging_dir))
        {
            drop(file);
            let _ = fs::remove_file(&staging_path);
            let _ = fs::remove_dir(&staging_dir);
            return Err(ForkError::Io(error));
        }
        #[cfg(not(unix))]
        drop(file);
        if let Err(error) = fs::rename(&staging_dir, &child_dir) {
            let _ = fs::remove_file(&staging_path);
            let _ = fs::remove_dir(&staging_dir);
            return Err(ForkError::Io(error));
        }
        if let Err(error) = sync_session_directory(sessions_root) {
            #[cfg(unix)]
            drop(file);
            let _ = fs::remove_file(child_dir.join(LOG_FILE_NAME));
            let _ = fs::remove_dir(&child_dir);
            let _ = sync_session_directory(sessions_root);
            return Err(ForkError::Io(error));
        }
        let child_path = child_dir.join(LOG_FILE_NAME);
        #[cfg(not(unix))]
        let file = OpenOptions::new().append(true).open(&child_path)?;
        // The child is a writable handle from the moment it exists, so it
        // takes its own lease; the directory was minted by this call, so the
        // lease is uncontended.
        let Some(pending) = acquire_writer_lease(&child_dir)? else {
            return Err(ForkError::Io(io::Error::other(
                "new fork log is already locked",
            )));
        };
        let writer = Some(pending.commit());
        let mut events = prefix.to_vec();
        events.push(event);
        let mut event_hashes = self.event_hashes[..seed_count].to_vec();
        event_hashes.push(creation_hash);
        Ok(Self {
            id: child_id,
            path: child_path,
            file,
            events,
            event_hashes,
            inbox,
            creation: Some(creation),
            first_local_seq: seed_event_count,
            physical_bytes: u64::try_from(line.len()).unwrap_or(u64::MAX),
            writer,
            bus: EventBus::default(),
        })
    }

    /// A clone of the shared event bus; [`Session::append`] emits each
    /// committed [`SessionEvent`] here after the durable write completes.
    #[must_use]
    pub fn bus(&self) -> EventBus {
        self.bus.clone()
    }

    #[cfg_attr(unix, allow(deprecated))]
    pub(crate) fn try_lock_lifecycle_exclusive(&self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            match nix::fcntl::flock(
                self.file.as_raw_fd(),
                nix::fcntl::FlockArg::LockExclusiveNonblock,
            ) {
                Ok(()) => Ok(true),
                Err(nix::errno::Errno::EWOULDBLOCK) => Ok(false),
                Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
        #[cfg(not(unix))]
        {
            Ok(false)
        }
    }

    /// Append one event: assign the next contiguous seq, stamp commit time,
    /// write + flush the line, then publish to the bus and return the event.
    ///
    /// A single event can only carry a rule decidable from what is already
    /// durable. `session/created` is constructor-owned, a second
    /// `runtime/linked` is refused, and `user/attachments` — whose validity
    /// depends on the event that must FOLLOW it — can only commit through
    /// [`Session::append_user_message_with_attachments`], which writes the pair
    /// in one line batch.
    ///
    /// # Errors
    /// - [`AppendError::Io`] when the durable write fails (nothing published)
    /// - [`AppendError::Serialize`] when the payload cannot be serialized
    /// - [`AppendError::InvalidEvent`] when payload/projection validation fails
    /// - [`AppendError::LogFull`] / [`AppendError::TooManyEvents`] when the line
    ///   would push the log past a bound [`Session::open`] enforces
    /// - [`AppendError::Superseded`] when a newer writable handle owns the log
    pub fn append(&mut self, kind: SessionEventKind) -> Result<SessionEvent, AppendError> {
        if matches!(kind, SessionEventKind::SessionCreated { .. }) {
            return Err(AppendError::InvalidEvent {
                message: "session/created is constructor-owned".to_owned(),
            });
        }
        if matches!(kind, SessionEventKind::RuntimeLinked { .. })
            && self
                .events
                .iter()
                .filter(|event| event.seq >= self.first_local_seq)
                .any(|event| matches!(event.kind, SessionEventKind::RuntimeLinked { .. }))
        {
            return Err(AppendError::InvalidEvent {
                message: "runtime/linked may appear only once".to_owned(),
            });
        }
        self.append_kind(kind)
    }

    /// This physical session's runtime link, retained for exact resume.
    /// An inherited conversation prefix never grants ownership of its parent's
    /// live provider session. Forks must establish their own runtime identity.
    #[must_use]
    pub fn runtime_link(&self) -> Option<(&str, &str)> {
        self.events
            .iter()
            .filter(|event| event.seq >= self.first_local_seq)
            .find_map(|event| match &event.kind {
                SessionEventKind::RuntimeLinked {
                    runtime,
                    runtime_session_id,
                } => Some((runtime.as_str(), runtime_session_id.as_str())),
                _ => None,
            })
    }

    /// Atomically append one selected-attachment event and its user message.
    /// Empty attachment input uses the ordinary one-event path.
    ///
    /// # Errors
    /// Unadmitted/duplicate/invalid attachment metadata, serialization or
    /// durable write/flush failure publishes no bus event.
    pub fn append_user_message_with_attachments(
        &mut self,
        text: impl Into<String>,
        attachments: Vec<heycode_core::AttachmentMetadata>,
    ) -> Result<SessionEvent, AppendError> {
        self.append_user_message_with_attachment_routes(text, attachments, Vec::new())
    }

    /// Atomically append resolved image/document selections and their user
    /// message. Document routes must refer to prior exact admissions.
    ///
    /// # Errors
    /// Unadmitted/incoherent selections, serialization or durable write/flush
    /// failure publishes no bus event.
    pub fn append_user_message_with_attachment_routes(
        &mut self,
        text: impl Into<String>,
        attachments: Vec<heycode_core::AttachmentMetadata>,
        document_routes: Vec<heycode_core::DocumentInputRoute>,
    ) -> Result<SessionEvent, AppendError> {
        let message = SessionEventKind::UserMessage { text: text.into() };
        if attachments.is_empty() {
            if !document_routes.is_empty() {
                return Err(AppendError::InvalidEvent {
                    message: "document routes require selected attachments".to_owned(),
                });
            }
            return self.append_kind(message);
        }
        let selection = SessionEventKind::UserAttachments {
            attachments,
            document_routes,
        };
        self.append_kinds_atomically([selection, message])
    }

    /// Append one exact audio-output association after its ATT01 objects and
    /// producing request are durable.
    ///
    /// # Errors
    /// Invalid/non-audio metadata, missing prior admission/request, route
    /// mismatch, serialization or durable append failure.
    pub fn append_assistant_audio(
        &mut self,
        turn: u64,
        step: u32,
        request_id: heycode_core::RequestId,
        attachments: Vec<heycode_core::AttachmentMetadata>,
    ) -> Result<SessionEvent, AppendError> {
        let kind = SessionEventKind::AssistantAudio {
            turn,
            step,
            request_id,
            attachments,
        };
        self.append_kind(kind)
    }

    /// Atomically claim one pending inbox message and admit its text as
    /// model-visible input.
    ///
    /// Claiming is the only path from the operational inbox to the model: the
    /// removal splice and the `user/message` reach durable storage together, so
    /// replay can never show a message that was consumed without being admitted
    /// or admitted without being consumed. A claim carries no
    /// `InboxSpliceOutcome`; that field marks cancellation only.
    ///
    /// # Errors
    /// The position does not hold a message, the claimed text is invalid, or
    /// serialization or the durable write/flush fails. No bus event publishes
    /// on failure.
    pub fn append_inbox_claim(
        &mut self,
        target: crate::InboxTarget,
        start: u32,
    ) -> Result<crate::InboxMessage, AppendError> {
        let index = usize::try_from(start).map_err(|_| AppendError::InvalidEvent {
            message: "inbox claim position exceeds the supported range".to_owned(),
        })?;
        let message = match target {
            crate::InboxTarget::NextTurn => self.inbox.next_turn().get(index),
            crate::InboxTarget::NextStep => self.inbox.next_step().get(index),
        }
        .cloned()
        .ok_or_else(|| AppendError::InvalidEvent {
            message: "inbox claim position holds no pending message".to_owned(),
        })?;
        self.append_kinds_atomically([
            SessionEventKind::AgentInboxSplice {
                target,
                start,
                removed_count: Some(1),
                inserted: Vec::new(),
                outcome: None,
            },
            SessionEventKind::UserMessage {
                text: message.text().to_owned(),
            },
        ])?;
        Ok(message)
    }

    /// Pending human messages in their original admission order across both queues.
    #[must_use]
    pub fn pending_human_messages(&self) -> Vec<crate::InboxMessage> {
        let pending = self
            .inbox
            .next_turn()
            .iter()
            .chain(self.inbox.next_step())
            .filter(|message| matches!(message.source(), crate::InboxSource::Human))
            .map(|message| message.id())
            .collect::<std::collections::HashSet<_>>();
        self.events
            .iter()
            .filter_map(|event| match &event.kind {
                SessionEventKind::AgentInboxSplice { inserted, .. } => Some(inserted),
                _ => None,
            })
            .flatten()
            .filter(|message| pending.contains(message.id()))
            .cloned()
            .collect()
    }

    /// Withdraw all still-pending human messages as one serialized operation.
    /// Operational job/team deliveries stay in their queues. A concurrent claim
    /// must hold this same session lock, so consumed messages cannot be recalled.
    ///
    /// # Errors
    /// Validation or durable append failure publishes no recall to the caller.
    pub fn recall_human_messages(&mut self) -> Result<Vec<crate::InboxMessage>, AppendError> {
        let messages = self.pending_human_messages();
        if messages.is_empty() {
            return Ok(messages);
        }
        let mut cancellations = Vec::new();
        for (target, queue) in [
            (crate::InboxTarget::NextTurn, self.inbox.next_turn()),
            (crate::InboxTarget::NextStep, self.inbox.next_step()),
        ] {
            for (index, message) in queue.iter().enumerate().rev() {
                if matches!(message.source(), crate::InboxSource::Human) {
                    cancellations.push(SessionEventKind::AgentInboxSplice {
                        target,
                        start: u32::try_from(index).map_err(|_| AppendError::InvalidEvent {
                            message: "inbox recall position is out of range".into(),
                        })?,
                        removed_count: Some(1),
                        inserted: Vec::new(),
                        outcome: Some(crate::InboxSpliceOutcome::Canceled),
                    });
                }
            }
        }
        self.append_kinds_atomically(cancellations)?;
        Ok(messages)
    }

    /// Refuse to append through a handle a newer in-process writer replaced.
    fn check_writable(&self) -> Result<(), AppendError> {
        if self
            .writer
            .as_ref()
            .is_some_and(|lease| !lease.is_current())
        {
            return Err(AppendError::Superseded);
        }
        Ok(())
    }

    /// Refuse a write the reader would refuse. `Session::open` rejects a log
    /// past either bound and the durable log is never rewritten, so a writer
    /// that ignores them can commit a session nothing can ever reopen.
    fn reserve(&self, events: usize, bytes: usize) -> Result<(), AppendError> {
        if self.events.len().saturating_add(events) > MAX_LOGICAL_SESSION_EVENTS {
            return Err(AppendError::TooManyEvents {
                maximum: MAX_LOGICAL_SESSION_EVENTS,
            });
        }
        let added = u64::try_from(bytes).unwrap_or(u64::MAX);
        if self.physical_bytes.saturating_add(added) > MAX_SESSION_LOG_BYTES {
            return Err(AppendError::LogFull {
                maximum_bytes: MAX_SESSION_LOG_BYTES,
            });
        }
        Ok(())
    }

    fn append_creation(&mut self, creation: SessionCreation) -> Result<SessionEvent, AppendError> {
        if !self.events.is_empty() || self.creation.is_some() {
            return Err(AppendError::InvalidEvent {
                message: "session/created must be the first local event".to_owned(),
            });
        }
        let event = self.append_kind(SessionEventKind::SessionCreated {
            creation: Box::new(creation.clone()),
        })?;
        self.creation = Some(creation);
        Ok(event)
    }

    fn append_kind(&mut self, kind: SessionEventKind) -> Result<SessionEvent, AppendError> {
        self.check_writable()?;
        kind.validate_for_version(CURRENT_SESSION_LOG_VERSION)
            .map_err(|message| AppendError::InvalidEvent { message })?;
        validate_against_events(&self.events, &kind)
            .map_err(|message| AppendError::InvalidEvent { message })?;
        settle_pairing(std::slice::from_ref(&kind))
            .map_err(|message| AppendError::InvalidEvent { message })?;
        let event = SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq: self.events.len() as u64,
            time_ms: Utc::now().timestamp_millis(),
            kind,
        };
        validate_compaction_boundary(&event.kind, event.seq)
            .map_err(|message| AppendError::InvalidEvent { message })?;
        let inbox_splice =
            self.inbox
                .validate_event(&event)
                .map_err(|error| AppendError::InvalidEvent {
                    message: error.to_string(),
                })?;
        let mut line = serde_json::to_string(&event)?;
        let event_hash = event_leaf_sha256(line.as_bytes());
        line.push('\n');
        self.reserve(1, line.len())?;
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        self.physical_bytes = self
            .physical_bytes
            .saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX));
        self.events.push(event.clone());
        self.event_hashes.push(event_hash);
        if let Some(validated) = inbox_splice {
            self.inbox.commit_validated_event(&event, validated);
        }
        self.bus.emit(event.clone());
        Ok(event)
    }

    fn append_kinds_atomically(
        &mut self,
        kinds: impl IntoIterator<Item = SessionEventKind>,
    ) -> Result<SessionEvent, AppendError> {
        let kinds: Vec<_> = kinds.into_iter().collect();
        self.check_writable()?;
        self.reserve(kinds.len(), 0)?;
        settle_pairing(&kinds).map_err(|message| AppendError::InvalidEvent { message })?;
        let mut events = Vec::with_capacity(kinds.len());
        let mut hashes = Vec::with_capacity(kinds.len());
        let mut bytes = String::new();
        // The inbox projection is stateful, so every event in the batch is
        // validated against the state its predecessors would produce. Nothing
        // is committed until the whole batch reaches durable storage.
        let mut inbox = self.inbox.clone();
        for (offset, kind) in kinds.into_iter().enumerate() {
            kind.validate_for_version(CURRENT_SESSION_LOG_VERSION)
                .map_err(|message| AppendError::InvalidEvent { message })?;
            validate_against_events(&self.events, &kind)
                .map_err(|message| AppendError::InvalidEvent { message })?;
            let seq = u64::try_from(self.events.len().saturating_add(offset)).map_err(|_| {
                AppendError::InvalidEvent {
                    message: "session sequence exceeds the supported range".to_owned(),
                }
            })?;
            let event = SessionEvent {
                v: CURRENT_SESSION_LOG_VERSION,
                seq,
                time_ms: Utc::now().timestamp_millis(),
                kind,
            };
            validate_compaction_boundary(&event.kind, event.seq)
                .map_err(|message| AppendError::InvalidEvent { message })?;
            if let Some(validated) =
                inbox
                    .validate_event(&event)
                    .map_err(|error| AppendError::InvalidEvent {
                        message: error.to_string(),
                    })?
            {
                inbox.commit_validated_event(&event, validated);
            }
            let line = serde_json::to_string(&event)?;
            hashes.push(event_leaf_sha256(line.as_bytes()));
            bytes.push_str(&line);
            bytes.push('\n');
            events.push(event);
        }
        self.reserve(events.len(), bytes.len())?;
        self.file.write_all(bytes.as_bytes())?;
        self.file.flush()?;
        self.physical_bytes = self
            .physical_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        self.events.extend(events.iter().cloned());
        self.event_hashes.extend(hashes);
        self.inbox = inbox;
        for event in &events {
            self.bus.emit(event.clone());
        }
        events.pop().ok_or_else(|| AppendError::InvalidEvent {
            message: "user input batch is empty".to_owned(),
        })
    }
}

/// The exclusive right to append to one log, held for as long as any writable
/// handle on it lives.
///
/// Cross-process exclusion is an advisory lock on the session directory, so a
/// second `heycode` cannot resume a log this process is writing and mint the same
/// sequence numbers. Within one process the lease is shared rather than
/// refused — composition legitimately replaces the world's session handle
/// while the previous one is still referenced — and the newest holder wins:
/// [`WriterLease::is_current`] goes false for every older holder, whose
/// appends then fail loud instead of colliding.
#[cfg(unix)]
struct WriterLease {
    shared: Arc<WriterLeaseFile>,
    generation: u64,
}

/// Platforms without advisory directory locks get a lease that excludes
/// nothing; the guarantee is documented as unix-only, never silently faked.
#[cfg(not(unix))]
struct WriterLease;

/// A lease that holds the cross-process lock but has not yet claimed the
/// in-process generation.
///
/// Superseding an older handle is a publication, so it happens at the commit
/// point and nowhere earlier: a resume that fails to replay drops its pending
/// lease and the writer that was already there stays current. Only
/// [`PendingWriterLease::commit`], called once the writable handle exists,
/// makes the supersession visible.
#[cfg(unix)]
struct PendingWriterLease {
    shared: Arc<WriterLeaseFile>,
}

/// Off Unix there is nothing to reserve, so committing publishes nothing.
#[cfg(not(unix))]
struct PendingWriterLease;

impl PendingWriterLease {
    /// Publish the lease: from here this holder is the log's current writer
    /// and every older in-process holder is superseded.
    #[cfg(unix)]
    fn commit(self) -> WriterLease {
        let generation = self
            .shared
            .generation
            .fetch_add(1, AtomicOrdering::AcqRel)
            .saturating_add(1);
        WriterLease {
            shared: self.shared,
            generation,
        }
    }

    #[cfg(not(unix))]
    #[allow(clippy::unused_self)]
    fn commit(self) -> WriterLease {
        WriterLease
    }
}

#[cfg(unix)]
struct WriterLeaseFile {
    generation: AtomicU64,
    _file: fs::File,
}

impl WriterLease {
    /// Whether this handle still owns the log. A newer in-process writable
    /// handle supersedes every older one.
    #[cfg(unix)]
    fn is_current(&self) -> bool {
        self.shared.generation.load(AtomicOrdering::Acquire) == self.generation
    }

    #[cfg(not(unix))]
    #[allow(clippy::unused_self)]
    fn is_current(&self) -> bool {
        true
    }
}

#[cfg(unix)]
fn writer_leases() -> &'static Mutex<HashMap<PathBuf, Weak<WriterLeaseFile>>> {
    static LEASES: OnceLock<Mutex<HashMap<PathBuf, Weak<WriterLeaseFile>>>> = OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Reserve the writer lease for `directory`, or report that another process
/// holds it. The lock lives on the session directory rather than on
/// `session.jsonl` because readers take a shared lock on the log itself and
/// must never block behind — or be blocked by — a live writer.
///
/// The returned lease is not current yet: the caller commits it once the
/// handle that owns it exists, so a caller that fails in between supersedes
/// nobody.
#[cfg(unix)]
#[allow(deprecated)]
fn acquire_writer_lease(directory: &Path) -> io::Result<Option<PendingWriterLease>> {
    let key = fs::canonicalize(directory)?;
    let mut leases = writer_leases()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    leases.retain(|_, weak| weak.strong_count() > 0);
    if let Some(shared) = leases.get(&key).and_then(Weak::upgrade) {
        return Ok(Some(PendingWriterLease { shared }));
    }
    let file = fs::File::open(directory)?;
    match nix::fcntl::flock(
        file.as_raw_fd(),
        nix::fcntl::FlockArg::LockExclusiveNonblock,
    ) {
        Ok(()) => {}
        Err(nix::errno::Errno::EWOULDBLOCK) => return Ok(None),
        Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
    }
    let shared = Arc::new(WriterLeaseFile {
        generation: AtomicU64::new(0),
        _file: file,
    });
    leases.insert(key, Arc::downgrade(&shared));
    Ok(Some(PendingWriterLease { shared }))
}

#[cfg(not(unix))]
fn acquire_writer_lease(_directory: &Path) -> io::Result<Option<PendingWriterLease>> {
    Ok(Some(PendingWriterLease))
}

#[cfg(unix)]
#[allow(deprecated)]
fn lock_session_shared(file: &fs::File) -> io::Result<()> {
    nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockShared)
        .map_err(|error| io::Error::from_raw_os_error(error as i32))
}

#[cfg(not(unix))]
fn lock_session_shared(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) struct LineageMutationLock {
    _file: fs::File,
}

#[cfg(not(unix))]
pub(crate) struct LineageMutationLock;

#[cfg(unix)]
#[allow(deprecated)]
pub(crate) fn lock_lineage_mutation(root: &Path) -> io::Result<LineageMutationLock> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let root_metadata = fs::symlink_metadata(root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(io::Error::other("unsafe sessions root"));
    }
    let path = root.join(LINEAGE_LOCK_FILE_NAME);
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW);
    let file = options.open(&path)?;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(&path)?;
    let safe = opened.is_file()
        && named.is_file()
        && !named.file_type().is_symlink()
        && opened.dev() == named.dev()
        && opened.ino() == named.ino()
        && opened.nlink() == 1
        && opened.uid() == root_metadata.uid()
        && opened.mode() & 0o077 == 0
        && opened.len() == 0;
    if !safe {
        return Err(io::Error::other("unsafe lineage lock"));
    }
    nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive)
        .map_err(|error| io::Error::from_raw_os_error(error as i32))?;
    let named = fs::symlink_metadata(&path)?;
    if named.file_type().is_symlink()
        || !named.is_file()
        || named.dev() != opened.dev()
        || named.ino() != opened.ino()
    {
        return Err(io::Error::other("lineage lock identity changed"));
    }
    Ok(LineageMutationLock { _file: file })
}

#[cfg(not(unix))]
pub(crate) fn lock_lineage_mutation(_root: &Path) -> io::Result<LineageMutationLock> {
    Ok(LineageMutationLock)
}

#[cfg(unix)]
fn sync_session_directory(directory: &Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_session_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

fn validate_compaction_boundary(kind: &SessionEventKind, event_seq: u64) -> Result<(), String> {
    let replaced_upto_seq = match kind {
        SessionEventKind::CompactionApplied {
            replaced_upto_seq, ..
        }
        | SessionEventKind::NativeCompactionApplied {
            replaced_upto_seq, ..
        } => Some(*replaced_upto_seq),
        _ => None,
    };
    if replaced_upto_seq.is_some_and(|replaced| replaced >= event_seq) {
        Err("compaction boundary must precede its settlement event".to_owned())
    } else {
        Ok(())
    }
}

fn validate_selection_against_events(
    events: &[SessionEvent],
    selection: &SessionEventKind,
) -> Result<(), String> {
    let SessionEventKind::UserAttachments {
        attachments,
        document_routes,
    } = selection
    else {
        return Err("attachment selection event is invalid".to_owned());
    };
    let mut required = attachments.iter().collect::<Vec<_>>();
    for route in document_routes {
        required.push(route.source());
        required.push(route.selected());
    }
    for selected in required {
        if !events.iter().any(|event| {
            matches!(
                &event.kind,
                SessionEventKind::AttachmentAdded { attachment }
                    if attachment.as_ref() == selected
            )
        }) {
            return Err("user attachment was not previously admitted".to_owned());
        }
    }
    Ok(())
}

/// Progress of the one whole-log rule whose satisfaction depends on the event
/// that must FOLLOW: a `user/attachments` selection binds to the very next
/// event and that event must be its `user/message`.
///
/// Reader and writer fold the same transition so they cannot drift: `open`
/// folds the durable log and requires [`PairingState::Settled`] at the end,
/// and every append folds its batch from `Settled` — the state every committed
/// log ends in — and requires `Settled` again. A rule that looks forward is
/// undecidable for a lone event, which is exactly why a selection may only
/// commit inside the atomic batch that carries its message.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PairingState {
    /// Every neighbour-dependent event so far has the successor it needs.
    Settled,
    /// A selection is committed and still waiting for its `user/message`.
    PendingSelection,
}

fn advance_pairing(state: PairingState, kind: &SessionEventKind) -> Result<PairingState, String> {
    if state == PairingState::PendingSelection
        && !matches!(kind, SessionEventKind::UserMessage { .. })
    {
        return Err("user/attachments must be followed immediately by user/message".to_owned());
    }
    match kind {
        SessionEventKind::UserAttachments { .. } => Ok(PairingState::PendingSelection),
        SessionEventKind::UserMessage { .. } => Ok(PairingState::Settled),
        _ => Ok(state),
    }
}

/// Fold one prospective batch and demand it leave the log settled.
fn settle_pairing(kinds: &[SessionEventKind]) -> Result<(), String> {
    let mut state = PairingState::Settled;
    for kind in kinds {
        state = advance_pairing(state, kind)?;
    }
    if state == PairingState::PendingSelection {
        return Err(
            "user/attachments must commit together with its user/message; use \
             append_user_message_with_attachments"
                .to_owned(),
        );
    }
    Ok(())
}

/// Neighbour preconditions that look BACKWARD, and so are decidable for a
/// single event at its commit point.
fn validate_against_events(events: &[SessionEvent], kind: &SessionEventKind) -> Result<(), String> {
    if let SessionEventKind::CodeModeChange { change } = kind {
        crate::project_code_mode(events)?.apply(change)?;
    }
    match kind {
        SessionEventKind::AssistantAudio { .. } => {
            validate_assistant_audio_against_events(events, kind)
        }
        SessionEventKind::UserAttachments { .. } => validate_selection_against_events(events, kind),
        _ => Ok(()),
    }
}

fn validate_user_attachment_sequence(events: &[SessionEvent]) -> Result<(), (usize, String)> {
    let mut state = PairingState::Settled;
    for event in events {
        let line_no = usize::try_from(event.seq)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        state = advance_pairing(state, &event.kind).map_err(|message| (line_no, message))?;
        if matches!(&event.kind, SessionEventKind::UserAttachments { .. }) {
            validate_selection_against_events(events_before(events, event.seq), &event.kind)
                .map_err(|message| (line_no, message))?;
        }
    }
    if state == PairingState::PendingSelection {
        return Err((
            events.len(),
            "user/attachments has no following user/message".to_owned(),
        ));
    }
    Ok(())
}

fn validate_assistant_audio_against_events(
    events: &[SessionEvent],
    output: &SessionEventKind,
) -> Result<(), String> {
    let SessionEventKind::AssistantAudio {
        turn,
        step,
        request_id,
        attachments,
    } = output
    else {
        return Err("assistant audio event is invalid".to_owned());
    };
    if !events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKind::RequestHeader {
                turn: found_turn,
                step: found_step,
                request_id: found_request,
                ..
            } if found_turn == turn && found_step == step && found_request == request_id
        )
    }) {
        return Err("assistant audio request was not previously admitted".to_owned());
    }
    if !events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKind::RequestContext {
                request_id: found_request,
                ..
            } if found_request == request_id
        )
    }) {
        return Err("assistant audio request context was not previously admitted".to_owned());
    }
    for selected in attachments {
        if !events.iter().any(|event| {
            matches!(
                &event.kind,
                SessionEventKind::AttachmentAdded { attachment }
                    if attachment.as_ref() == selected
            )
        }) {
            return Err("assistant audio was not previously admitted".to_owned());
        }
    }
    Ok(())
}

fn validate_assistant_audio_sequence(events: &[SessionEvent]) -> Result<(), (usize, String)> {
    for event in events {
        if matches!(event.kind, SessionEventKind::AssistantAudio { .. }) {
            validate_assistant_audio_against_events(events_before(events, event.seq), &event.kind)
                .map_err(|message| {
                    (
                        usize::try_from(event.seq)
                            .unwrap_or(usize::MAX)
                            .saturating_add(1),
                        message,
                    )
                })?;
        }
    }
    Ok(())
}

fn events_before(events: &[SessionEvent], seq: u64) -> &[SessionEvent] {
    let boundary = usize::try_from(seq)
        .unwrap_or(events.len())
        .min(events.len());
    &events[..boundary]
}

fn prefix_sha256(event_hashes: &[[u8; 32]]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"dshx-session-prefix-raw-leaves-v1\0");
    digest.update(
        u64::try_from(event_hashes.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for event_hash in event_hashes {
        digest.update(event_hash);
    }
    let output = digest.finalize();
    format!("{output:x}")
}

fn event_leaf_sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn open_turn(events: &[SessionEvent]) -> Option<u64> {
    let mut open = None;
    for event in events {
        match &event.kind {
            SessionEventKind::TurnStart { turn } => open = Some(*turn),
            SessionEventKind::TurnEnd { turn, .. } if open == Some(*turn) => open = None,
            _ => {}
        }
    }
    open
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::event::{TokenUsage, ToolCallOut};

    /// Hand-write one valid envelope line for corruption tests.
    fn envelope(seq: u64, kind: &str, data: serde_json::Value) -> String {
        json!({"v": 1, "seq": seq, "time_ms": 1_730_000_000_000_i64, "kind": kind, "data": data})
            .to_string()
    }

    /// Pre-seed a session directory with raw log contents.
    fn seeded_session_dir(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
        let session_dir = dir.path().join(name);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join(LOG_FILE_NAME), contents).unwrap();
        session_dir
    }

    #[cfg(unix)]
    #[test]
    fn lineage_mutation_lock_excludes_a_second_store_owner() {
        let dir = TempDir::new().unwrap();
        let first = lock_lineage_mutation(dir.path()).unwrap();
        let root = dir.path().to_path_buf();
        let (attempting_tx, attempting_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            attempting_tx.send(()).unwrap();
            let _second = lock_lineage_mutation(&root).unwrap();
            acquired_tx.send(()).unwrap();
        });
        attempting_rx.recv().unwrap();
        assert!(
            acquired_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "a second process-equivalent owner crossed the lineage mutation lock"
        );
        drop(first);
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn raw_export_snapshot_refuses_bytes_appended_after_validation() {
        let dir = TempDir::new().unwrap();
        let mut writer = Session::create(dir.path()).unwrap();
        writer
            .append(SessionEventKind::UserMessage {
                text: "before snapshot".to_owned(),
            })
            .unwrap();
        let opened = Session::open(writer.path().parent().unwrap()).unwrap();
        writer
            .append(SessionEventKind::UserMessage {
                text: "racing append".to_owned(),
            })
            .unwrap();
        assert!(opened.validated_raw_suffix().is_err());
    }

    #[test]
    fn create_append_open_round_trips() {
        let dir = TempDir::new().unwrap();
        let mut session = Session::create(dir.path()).unwrap();

        let appended: Vec<SessionEvent> = vec![
            session
                .append(SessionEventKind::UserMessage {
                    text: "hello".into(),
                })
                .unwrap(),
            session
                .append(SessionEventKind::AssistantMessage {
                    turn: 0,
                    step: 0,
                    content: String::new(),
                    reasoning: Some("hmm".into()),
                    tool_calls: Some(vec![ToolCallOut {
                        id: "call_1".into(),
                        name: "bash".into(),
                        arguments: r#"{"cmd":"ls"}"#.into(),
                    }]),
                    usage: Some(TokenUsage {
                        prompt_tokens: 9,
                        completion_tokens: 3,
                    }),
                })
                .unwrap(),
            session
                .append(SessionEventKind::ToolResult {
                    call_id: heycode_core::CallId::from_raw("call_1"),
                    content: "file.txt".into(),
                    is_error: false,
                    untrusted_content: None,
                })
                .unwrap(),
        ];
        assert_eq!(appended.len(), 3);

        let id = session.id().clone();
        let path = session.path().to_path_buf();
        let snapshot: Vec<Value> = session
            .events()
            .iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .collect();
        drop(session);

        assert!(path.is_file(), "{}", path.display());
        assert_eq!(path, dir.path().join(id.as_str()).join(LOG_FILE_NAME));

        let resumed = Session::open(dir.path().join(id.as_str())).unwrap();
        assert_eq!(resumed.id(), &id);
        let replayed: Vec<Value> = resumed
            .events()
            .iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .collect();
        assert_eq!(replayed, snapshot);
    }

    #[test]
    fn runtime_configuration_audit_without_a_link_keeps_a_root_startable() {
        let dir = TempDir::new().unwrap();
        let mut session = Session::create(dir.path()).unwrap();
        for state in [
            crate::RuntimeConfigurationState::Attempted,
            crate::RuntimeConfigurationState::Failed,
            crate::RuntimeConfigurationState::Committed,
        ] {
            session
                .append(SessionEventKind::RuntimeConfigured {
                    state,
                    system_prompt: None,
                    tools: None,
                    model: Some("model".to_owned()),
                    reasoning_effort: None,
                })
                .unwrap();
            assert!(session.is_fresh());
        }
        session
            .append(SessionEventKind::RuntimeLinked {
                runtime: "delegated".to_owned(),
                runtime_session_id: "runtime-session".to_owned(),
            })
            .unwrap();
        assert!(!session.is_fresh());
    }

    #[test]
    fn seq_is_contiguous_from_zero() {
        let dir = TempDir::new().unwrap();
        let mut session = Session::create(dir.path()).unwrap();
        let mut seqs = Vec::new();
        for i in 0..3 {
            seqs.push(
                session
                    .append(SessionEventKind::UserMessage {
                        text: format!("m{i}"),
                    })
                    .unwrap()
                    .seq,
            );
        }
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn open_rejects_sequence_gap() {
        let dir = TempDir::new().unwrap();
        let contents = [
            envelope(0, "user/message", json!({"text": "a"})),
            envelope(2, "user/message", json!({"text": "b"})),
        ]
        .join("\n")
            + "\n";
        let session_dir = seeded_session_dir(&dir, "gap-session", &contents);
        match Session::open(&session_dir) {
            Err(OpenError::SeqGap { expected, found }) => {
                assert_eq!((expected, found), (1, 2));
            }
            Err(e) => panic!("expected SeqGap, got {e:?}"),
            Ok(_) => panic!("expected SeqGap, got Ok session"),
        }
    }

    #[test]
    fn open_names_unknown_kind() {
        let dir = TempDir::new().unwrap();
        let session_dir = seeded_session_dir(
            &dir,
            "future-session",
            &(envelope(0, "future/thing", json!({})) + "\n"),
        );
        let err = match Session::open(&session_dir) {
            Err(e) => e,
            Ok(_) => panic!("expected UnknownKind, got Ok session"),
        };
        match &err {
            OpenError::UnknownKind { line_no, kind } => {
                assert_eq!((*line_no, kind.as_str()), (1, "future/thing"));
            }
            other => panic!("expected UnknownKind, got {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            "unknown session event kind `future/thing` at line 1"
        );
    }

    #[test]
    fn open_rejects_unsupported_version() {
        let dir = TempDir::new().unwrap();
        let line =
            json!({"v": 3, "seq": 0, "time_ms": 1, "kind": "user/message", "data": {"text": "a"}});
        let session_dir = seeded_session_dir(&dir, "v3-session", &(line.to_string() + "\n"));
        match Session::open(&session_dir) {
            Err(OpenError::UnsupportedVersion { found, .. }) => assert_eq!(found, 3),
            Err(e) => panic!("expected UnsupportedVersion, got {e:?}"),
            Ok(_) => panic!("expected UnsupportedVersion, got Ok session"),
        }
    }

    #[test]
    fn open_reports_corrupt_line_number() {
        let dir = TempDir::new().unwrap();
        let good = envelope(0, "user/message", json!({"text": "a"}));
        let session_dir = seeded_session_dir(&dir, "broken-session", &(good + "\n{oops\n"));
        match Session::open(&session_dir) {
            Err(OpenError::CorruptLine { line_no, .. }) => assert_eq!(line_no, 2),
            Err(e) => panic!("expected CorruptLine, got {e:?}"),
            Ok(_) => panic!("expected CorruptLine, got Ok session"),
        }
    }

    #[test]
    fn open_rejects_unterminated_tail_before_append_can_join_json_values() {
        let dir = TempDir::new().unwrap();
        let session_dir = seeded_session_dir(
            &dir,
            "unterminated-session",
            &envelope(0, "user/message", json!({"text": "a"})),
        );
        assert!(matches!(
            Session::open(session_dir),
            Err(OpenError::UnterminatedTail)
        ));
    }

    #[test]
    fn empty_log_resumes_and_continues_at_zero() {
        let dir = TempDir::new().unwrap();
        let session_dir = seeded_session_dir(&dir, "blank-session", "");
        let mut resumed = Session::open(&session_dir).unwrap();
        assert!(resumed.events().is_empty());
        let first = resumed
            .append(SessionEventKind::UserMessage {
                text: "again".into(),
            })
            .unwrap();
        assert_eq!(first.seq, 0);
    }

    #[test]
    fn bus_emits_after_commit_with_matching_seq() {
        let dir = TempDir::new().unwrap();
        let mut session = Session::create(dir.path()).unwrap();
        let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        session
            .bus()
            .on::<SessionEvent>(move |e| sink.lock().unwrap().push(e.seq));

        for i in 0..3 {
            let event = session
                .append(SessionEventKind::UserMessage {
                    text: format!("m{i}"),
                })
                .unwrap();
            // The just-emitted payload equals the returned, persisted event.
            let last_emitted = *seen.lock().unwrap().last().unwrap();
            assert_eq!(last_emitted, event.seq);
        }
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            session
                .events()
                .iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>()
                .as_slice()
        );
    }

    #[test]
    fn every_append_persists_one_newline_terminated_line() {
        let dir = TempDir::new().unwrap();
        let mut session = Session::create(dir.path()).unwrap();
        session
            .append(SessionEventKind::UserMessage { text: "a".into() })
            .unwrap();
        session
            .append(SessionEventKind::UserMessage { text: "b".into() })
            .unwrap();

        let raw = std::fs::read_to_string(session.path()).unwrap();
        assert!(raw.ends_with('\n'), "each append must end with a newline");
        let lines: Vec<Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), session.events().len());
        for (line, event) in lines.iter().zip(session.events()) {
            assert_eq!(line["seq"], json!(event.seq));
            assert_eq!(line["v"], json!(event.v));
        }
    }

    #[test]
    fn resumed_session_appends_continue_the_sequence() {
        let dir = TempDir::new().unwrap();
        let mut first = Session::create(dir.path()).unwrap();
        let id = first.id().clone();
        first
            .append(SessionEventKind::UserMessage { text: "one".into() })
            .unwrap();
        first
            .append(SessionEventKind::UserMessage { text: "two".into() })
            .unwrap();
        drop(first);

        let mut second = Session::open(dir.path().join(id.as_str())).unwrap();
        let third = second
            .append(SessionEventKind::UserMessage {
                text: "three".into(),
            })
            .unwrap();
        assert_eq!(third.seq, 2);
        drop(second);

        let final_log = Session::open(dir.path().join(id.as_str())).unwrap();
        assert_eq!(final_log.events().len(), 3);
    }

    /// A live writer in ANOTHER process is exactly a foreign advisory lock on
    /// the session directory, which is what this takes on a second open file
    /// description.
    #[cfg(unix)]
    #[test]
    #[allow(deprecated)]
    fn a_second_process_cannot_take_the_writer_lease_but_readers_still_open() {
        let dir = TempDir::new().unwrap();
        let session = Session::create(dir.path()).unwrap();
        let directory = session.path().parent().unwrap().to_path_buf();
        drop(session);

        let foreign = fs::File::open(&directory).unwrap();
        nix::fcntl::flock(
            foreign.as_raw_fd(),
            nix::fcntl::FlockArg::LockExclusiveNonblock,
        )
        .unwrap();

        assert!(matches!(
            Session::open_for_writing(&directory),
            Err(OpenError::AlreadyOpen)
        ));
        // The picker opens every session in the store; a live writer must
        // never block or fail a listing.
        assert!(Session::open(&directory).unwrap().events().is_empty());

        drop(foreign);
        assert!(Session::open_for_writing(&directory).is_ok());
    }

    /// The logical bound cannot be driven with a million real appends, so the
    /// predicate that guards it is pinned directly at its exact boundary.
    #[test]
    fn reserve_refuses_exactly_what_open_refuses() {
        let dir = TempDir::new().unwrap();
        let session = Session::create(dir.path()).unwrap();

        assert!(session.reserve(MAX_LOGICAL_SESSION_EVENTS, 0).is_ok());
        assert!(matches!(
            session.reserve(MAX_LOGICAL_SESSION_EVENTS + 1, 0),
            Err(AppendError::TooManyEvents {
                maximum: MAX_LOGICAL_SESSION_EVENTS
            })
        ));

        let maximum = usize::try_from(MAX_SESSION_LOG_BYTES).unwrap();
        assert!(session.reserve(1, maximum).is_ok());
        assert!(matches!(
            session.reserve(1, maximum + 1),
            Err(AppendError::LogFull {
                maximum_bytes: MAX_SESSION_LOG_BYTES
            })
        ));
    }
}

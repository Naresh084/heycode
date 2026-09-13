//! The durable, bounded, owner-only health-history file.
//!
//! **Torn writes are engineered away rather than tolerated.** The document is
//! written whole through an atomic rename, so a crash mid-write leaves the
//! previous complete file in place; a reader never observes a half-written
//! document from this writer. What the reader still has to survive is a file
//! *someone else* damaged — a hand edit, a truncation, media rot, an older or
//! newer build — and that is what the line format is for.
//!
//! **A damaged line costs one entry, not the history and not the tail.** An
//! entry line that will not parse is skipped and counted; every entry after it
//! is still read. The count reaches the reader in
//! [`HealthHistory::unreadable`], so nothing is dropped silently. A final line
//! with no terminating newline is reported separately as
//! [`HealthHistory::truncated_tail`], because "the last write was torn" and "a
//! line rotted" lead to different next steps.
//!
//! **The header is the one thing that is refused rather than repaired.** If
//! the first line is not a header this build recognizes, the file is not read
//! *and not written*: guessing risks overwriting something else that lives at
//! that path, and a newer build's history is data this build must not destroy
//! (the same refusal `heycode-config` makes for a newer config schema).

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

use super::{
    HEALTH_HISTORY_SCHEMA_VERSION, HealthEntry, HealthHistoryError, HealthLabel, MAX_BYTES,
    MAX_ENTRIES, PROTECTED_UNHEALTHY,
};

/// Longest file this reader will load before parsing anything.
///
/// This writer never produces a document over [`MAX_BYTES`], so a file several
/// times that size was not written by it. Bounding the read is what keeps a
/// hostile or damaged file from being an allocation.
pub const MAX_READ_BYTES: u64 = 4 * MAX_BYTES as u64;

/// Marker distinguishing this document from any other line-delimited file that
/// might occupy the path.
const HEADER_KIND: &str = "heycode-health-history";

#[derive(Debug, Serialize, Deserialize)]
struct Header {
    kind: String,
    schema_version: u32,
}

/// Retained history as one reader observed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthHistory {
    entries: Vec<HealthEntry>,
    unreadable: usize,
    truncated_tail: bool,
}

impl HealthHistory {
    /// Retained entries, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[HealthEntry] {
        &self.entries
    }

    /// How many stored lines could not be read as an entry.
    ///
    /// Never silently zero: a damaged line is skipped so the rest of the
    /// history survives, and this is where the reader learns it happened.
    #[must_use]
    pub const fn unreadable(&self) -> usize {
        self.unreadable
    }

    /// Whether the file ended mid-line, which is what a torn append looks like.
    #[must_use]
    pub const fn truncated_tail(&self) -> bool {
        self.truncated_tail
    }

    /// Whether any run is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Deterministic human form carrying the same safe fields as the JSON.
    ///
    /// Check rows render when they are noteworthy — anything that did not pass,
    /// and anything slow enough to be the GOTCHAS #155 story — so a healthy
    /// history stays one line per run while a diagnosis keeps its detail.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut lines = vec![format!(
            "health history: {} of at most {MAX_ENTRIES} entries, bound {MAX_BYTES} bytes",
            self.entries.len()
        )];
        if self.unreadable > 0 {
            lines.push(format!(
                "unreadable entries skipped: {} (the rest of the history was still read)",
                self.unreadable
            ));
        }
        if self.truncated_tail {
            lines.push("last stored line was incomplete and was dropped".to_owned());
        }
        for entry in &self.entries {
            lines.push(entry.render_line());
            for check in entry.checks.iter().filter(|check| check.is_noteworthy()) {
                lines.push(format!(
                    "  [{}] {} ({}) {}ms{}",
                    check.status.as_str(),
                    check.id,
                    check.code,
                    check.duration_ms,
                    check
                        .evidence
                        .map_or(String::new(), |kind| format!(" evidence={}", kind.as_str())),
                ));
            }
            if entry.omitted_checks > 0 {
                lines.push(format!(
                    "  {} further check(s) not retained",
                    entry.omitted_checks
                ));
            }
        }
        lines.join("\n")
    }
}

/// Owner-only durable store for the bounded health history.
///
/// One process serializes its own read-modify-write behind a lock. Two
/// processes writing the same path is last-writer-wins: ordinary filesystems
/// offer no compare-and-swap against an unrelated writer, and claiming
/// cross-process linearizability here would be a claim this cannot keep
/// (GOTCHAS, atomic rename section).
#[derive(Debug)]
pub struct HealthHistoryStore {
    path: PathBuf,
    guard: Mutex<()>,
}

impl HealthHistoryStore {
    /// A store over `path`. The file is created on the first record.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            guard: Mutex::new(()),
        }
    }

    /// Where the history is retained.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the retained history, enforcing the bound on what was read.
    ///
    /// A missing file is an empty history, not a failure. The bound is applied
    /// here as well as on write, so no caller ever observes an unbounded
    /// history — including one a different build wrote.
    ///
    /// # Errors
    /// [`HealthHistoryError::MalformedHeader`] when the first line is not a
    /// recognized header, [`HealthHistoryError::NewerSchema`] when it names a
    /// version this build cannot read, [`HealthHistoryError::Oversized`] when
    /// the file exceeds [`MAX_READ_BYTES`], and
    /// [`HealthHistoryError::Io`] for an unreadable path.
    pub fn load(&self) -> Result<HealthHistory, HealthHistoryError> {
        let _guard = self.lock()?;
        self.read()
    }

    /// Append `entry`, evict down to the bound, and commit the whole document.
    ///
    /// Returns the history as it now stands, so the caller sees exactly what
    /// was retained — including any damage the read found.
    ///
    /// # Errors
    /// Everything [`Self::load`] returns, plus
    /// [`HealthHistoryError::Io`] when the document cannot be committed. A file
    /// this build cannot read is never overwritten.
    pub fn record(&self, entry: HealthEntry) -> Result<HealthHistory, HealthHistoryError> {
        let _guard = self.lock()?;
        let mut history = self.read()?;
        history.entries.push(entry);
        let document = bound(&mut history.entries)?;
        self.commit(&document)?;
        Ok(history)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, HealthHistoryError> {
        self.guard
            .lock()
            .map_err(|_| HealthHistoryError::Unavailable)
    }

    fn read(&self) -> Result<HealthHistory, HealthHistoryError> {
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HealthHistory {
                    entries: Vec::new(),
                    unreadable: 0,
                    truncated_tail: false,
                });
            }
            Err(source) => return Err(self.io("metadata", source)),
        };
        if !metadata.is_file() {
            return Err(HealthHistoryError::NotRegularFile { path: self.label() });
        }
        if metadata.len() > MAX_READ_BYTES {
            return Err(HealthHistoryError::Oversized {
                path: self.label(),
                limit: MAX_READ_BYTES,
            });
        }
        let raw = std::fs::read(&self.path).map_err(|source| self.io("read", source))?;
        self.parse(&raw)
    }

    fn parse(&self, raw: &[u8]) -> Result<HealthHistory, HealthHistoryError> {
        if raw.is_empty() {
            return Ok(HealthHistory {
                entries: Vec::new(),
                unreadable: 0,
                truncated_tail: false,
            });
        }
        let terminated = raw.last() == Some(&b'\n');
        let mut lines: Vec<&[u8]> = raw.split(|byte| *byte == b'\n').collect();
        if terminated {
            lines.pop();
        }
        let mut lines = lines.into_iter();
        let header = lines
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .and_then(|line| serde_json::from_str::<Header>(line).ok())
            .filter(|header| header.kind == HEADER_KIND)
            .ok_or_else(|| HealthHistoryError::MalformedHeader {
                path: self.label(),
                supported: HEALTH_HISTORY_SCHEMA_VERSION,
            })?;
        if header.schema_version > HEALTH_HISTORY_SCHEMA_VERSION {
            return Err(HealthHistoryError::NewerSchema {
                path: self.label(),
                found: header.schema_version,
                supported: HEALTH_HISTORY_SCHEMA_VERSION,
            });
        }
        if header.schema_version != HEALTH_HISTORY_SCHEMA_VERSION {
            return Err(HealthHistoryError::MalformedHeader {
                path: self.label(),
                supported: HEALTH_HISTORY_SCHEMA_VERSION,
            });
        }
        let mut entries = Vec::new();
        let mut unreadable = 0;
        let mut truncated_tail = false;
        let mut remaining = lines.peekable();
        while let Some(line) = remaining.next() {
            let last = remaining.peek().is_none();
            let parsed = std::str::from_utf8(line)
                .ok()
                .and_then(|line| HealthEntry::from_json(line).ok());
            match parsed {
                Some(entry) => entries.push(entry),
                None if last && !terminated => truncated_tail = true,
                None => unreadable += 1,
            }
        }
        // The bound holds for every observer, not only for what this build
        // wrote: a file with more entries than the cap is truncated on read.
        bound(&mut entries)?;
        Ok(HealthHistory {
            entries,
            unreadable,
            truncated_tail,
        })
    }

    fn commit(&self, document: &str) -> Result<(), HealthHistoryError> {
        if let Some(parent) = self.path.parent() {
            secure_directory(parent).map_err(|source| self.io("create_dir", source))?;
        }
        let mut options = AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(false).mode(0o600);
        }
        let mut output = options
            .open(&self.path)
            .map_err(|source| self.io("open", source))?;
        output
            .write_all(document.as_bytes())
            .map_err(|source| self.io("write", source))?;
        output.commit().map_err(|source| self.io("commit", source))
    }

    fn label(&self) -> HealthLabel {
        HealthLabel::new(self.path.display().to_string())
    }

    fn io(&self, operation: &'static str, source: std::io::Error) -> HealthHistoryError {
        HealthHistoryError::Io {
            path: self.label(),
            operation,
            source,
        }
    }
}

fn secure_directory(parent: &Path) -> std::io::Result<()> {
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Evict until both bounds hold, and render the document that results.
///
/// Both caps are checked on every call, so eviction happens at the boundary
/// rather than at some later tidy-up. The byte accounting is over the exact
/// bytes that will be written — the encoder runs once and the lengths measured
/// are the lengths committed — so the cap is a fact about the file rather than
/// an estimate of it.
fn bound(entries: &mut Vec<HealthEntry>) -> Result<String, HealthHistoryError> {
    let header = serde_json::to_string(&Header {
        kind: HEADER_KIND.to_owned(),
        schema_version: HEALTH_HISTORY_SCHEMA_VERSION,
    })
    .map_err(|_| HealthHistoryError::Encode)?;
    let mut lines = entries
        .iter()
        .map(|entry| serde_json::to_string(entry).map_err(|_| HealthHistoryError::Encode))
        .collect::<Result<Vec<String>, _>>()?;
    let line_bytes = |line: &String| line.len() + 1;
    let mut total = line_bytes(&header) + lines.iter().map(line_bytes).sum::<usize>();
    while entries.len() > MAX_ENTRIES || (entries.len() > 1 && total > MAX_BYTES) {
        let index = eviction_index(entries);
        total -= line_bytes(&lines[index]);
        lines.remove(index);
        entries.remove(index);
    }
    let mut document = String::with_capacity(total);
    document.push_str(&header);
    document.push('\n');
    for line in &lines {
        document.push_str(line);
        document.push('\n');
    }
    Ok(document)
}

/// The entry eviction removes next: the oldest one that is not protected.
///
/// Protection is a preference and never an exemption. When every retained
/// entry is protected — more than [`PROTECTED_UNHEALTHY`] unhealthy runs and
/// nothing else — the oldest is evicted anyway, because the cap is hard and a
/// history that could refuse to shrink is not bounded.
fn eviction_index(entries: &[HealthEntry]) -> usize {
    let protected = protected_indices(entries);
    entries
        .iter()
        .enumerate()
        .find(|(index, _)| !protected.contains(index))
        .map_or(0, |(index, _)| index)
}

fn protected_indices(entries: &[HealthEntry]) -> BTreeSet<usize> {
    entries
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, entry)| !entry.healthy)
        .take(PROTECTED_UNHEALTHY)
        .map(|(index, _)| index)
        .collect()
}

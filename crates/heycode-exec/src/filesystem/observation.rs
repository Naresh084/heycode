//! Provider-owned read observation records.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Files successfully read through one filesystem provider.
///
/// Local records use canonical paths and `(mtime, len)` stamps. A poisoned
/// registry fails closed. `write` checks whether a record exists; `edit`
/// additionally checks that its stamp still matches.
#[derive(Clone, Default)]
pub struct ObservationLog(Arc<Mutex<HashMap<PathBuf, Stamp>>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(windows)]
    created: u64,
    #[cfg(windows)]
    attributes: u32,
}

impl Stamp {
    /// Opaque identity/change token; not a claim to be a content digest.
    pub(crate) fn revision(self) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(format!("{self:?}")))
    }

    fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self::from_metadata(&metadata))
    }

    pub(crate) fn of_file(file: &cap_std::fs::File) -> Option<Self> {
        let metadata = file.try_clone().ok()?.into_std().metadata().ok()?;
        Some(Self::from_metadata(&metadata))
    }

    pub(crate) fn of_dir(directory: &cap_std::fs::Dir) -> Option<Self> {
        let metadata = directory
            .try_clone()
            .ok()?
            .into_std_file()
            .metadata()
            .ok()?;
        Some(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt as _;

        Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
            #[cfg(windows)]
            created: metadata.creation_time(),
            #[cfg(windows)]
            attributes: metadata.file_attributes(),
        }
    }

    pub(crate) fn same_identity(self, other: Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(windows)]
        {
            self.created == other.created && self.attributes == other.attributes
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.modified == other.modified && self.len == other.len
        }
    }
}

impl ObservationLog {
    /// Record the current local state of `path` after a successful read or
    /// mutation commit.
    pub fn mark(&self, path: &Path) {
        let key = observation_key(path);
        let Some(stamp) = Stamp::of(&key) else {
            return;
        };
        if let Ok(mut records) = self.0.lock() {
            records.insert(key, stamp);
        }
    }

    pub(crate) fn mark_stamp(&self, path: &Path, stamp: Stamp) {
        if let Ok(mut records) = self.0.lock() {
            records.insert(path.to_path_buf(), stamp);
        }
    }

    /// Whether `path` has ever been successfully observed.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        let key = observation_key(path);
        self.0
            .lock()
            .map(|records| records.contains_key(&key))
            .unwrap_or(false)
    }

    pub(crate) fn contains_exact(&self, path: &Path) -> bool {
        self.0
            .lock()
            .map(|records| records.contains_key(path))
            .unwrap_or(false)
    }

    /// Whether `path` was observed and its local `(mtime, len)` is unchanged.
    #[must_use]
    pub fn is_fresh(&self, path: &Path) -> bool {
        let key = observation_key(path);
        let Ok(records) = self.0.lock() else {
            return false;
        };
        let Some(observed) = records.get(&key) else {
            return false;
        };
        Stamp::of(&key).is_some_and(|current| current == *observed)
    }

    pub(crate) fn is_fresh_opened(&self, path: &Path, file: &cap_std::fs::File) -> bool {
        let Some(current) = Stamp::of_file(file) else {
            return false;
        };
        self.0
            .lock()
            .ok()
            .and_then(|records| records.get(path).copied())
            .is_some_and(|observed| observed == current)
    }

    /// Sorted canonical paths for tests and bounded diagnostics.
    #[must_use]
    pub fn snapshot(&self) -> BTreeSet<PathBuf> {
        self.0
            .lock()
            .map(|records| records.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Number of distinct observed paths.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.lock().map(|records| records.len()).unwrap_or(0)
    }

    /// Whether no path has been observed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for ObservationLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObservationLog")
            .field("record_count", &self.len())
            .finish()
    }
}

fn observation_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

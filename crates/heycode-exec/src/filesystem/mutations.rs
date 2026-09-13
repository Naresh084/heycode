//! Explicit guarded writes and atomic, single-file edit batches.
use super::{EditFileOutput, EditFileSpec, FileSystemError, FileSystemErrorCode, WriteFileSpec};

/// An ordered edit set, validated completely before one per-file commit.
#[derive(Debug, Clone)]
pub struct MultiEditSpec {
    edits: Vec<EditFileSpec>,
    expected_revision: Option<String>,
    dry_run: bool,
}
impl MultiEditSpec {
    /// All edits must target the same file; later edits see earlier replacements.
    ///
    /// # Errors
    /// Empty sets, more than 32 edits, different targets, or over 256 KiB payload.
    pub fn new(edits: Vec<EditFileSpec>) -> Result<Self, FileSystemError> {
        if !(1..=32).contains(&edits.len())
            || edits
                .iter()
                .any(|edit| edit.path().as_path() != edits[0].path().as_path())
            || edits
                .iter()
                .try_fold(0_usize, |size, edit| {
                    size.checked_add(edit.old().len())?
                        .checked_add(edit.new_text().len())
                })
                .is_none_or(|size| size > 262144)
        {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self {
            edits,
            expected_revision: None,
            dry_run: false,
        })
    }
    /// Require the opaque revision supplied by a prior Read.
    ///
    /// # Errors
    /// Invalid revision token.
    pub fn with_expected_revision(mut self, revision: String) -> Result<Self, FileSystemError> {
        super::model::validate_revision(&revision)?;
        self.expected_revision = Some(revision);
        Ok(self)
    }
    /// Compute a preview without writing or changing observation authority.
    #[must_use]
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }
    /// Ordered replacement requests.
    #[must_use]
    pub fn edits(&self) -> &[EditFileSpec] {
        &self.edits
    }
    /// Optional revision precondition.
    #[must_use]
    pub fn expected_revision(&self) -> Option<&str> {
        self.expected_revision.as_deref()
    }
    /// Whether mutation is disabled for this operation.
    #[must_use]
    pub fn dry_run(&self) -> bool {
        self.dry_run
    }
}

/// Facts returned after one validated edit batch or preview.
#[derive(Debug)]
pub struct MultiEditOutput {
    /// One result per requested edit, in the same order.
    pub edits: Vec<EditFileOutput>,
    /// Revision after commit, or the unchanged revision for a preview/no-op.
    pub revision: String,
    /// True for a preview that did not mutate the file.
    pub dry_run: bool,
    /// Whether the replacements produce different file contents.
    pub changed: bool,
}

/// Explicit create-only or revision-checked full replacement.
#[derive(Debug, Clone)]
pub struct CheckedWriteSpec {
    write: WriteFileSpec,
    expected_revision: Option<String>,
}
impl CheckedWriteSpec {
    /// Create only; an existing target must not be overwritten.
    #[must_use]
    pub fn create(write: WriteFileSpec) -> Self {
        Self {
            write,
            expected_revision: None,
        }
    }
    /// Replace an existing observed file at exactly the supplied revision.
    ///
    /// # Errors
    /// Invalid revision token.
    pub fn replace(write: WriteFileSpec, revision: String) -> Result<Self, FileSystemError> {
        super::model::validate_revision(&revision)?;
        Ok(Self {
            write,
            expected_revision: Some(revision),
        })
    }
    /// Exact target and replacement bytes.
    #[must_use]
    pub fn write(&self) -> &WriteFileSpec {
        &self.write
    }
    /// None means create-only; Some means replace at this revision.
    #[must_use]
    pub fn expected_revision(&self) -> Option<&str> {
        self.expected_revision.as_deref()
    }
}

/// Bounded write receipt, without echoing full file content into model context.
#[derive(Debug)]
pub struct CheckedWriteOutput {
    /// Post-write metadata revision.
    pub revision: String,
    /// Whether bytes were actually changed; no-ops preserve the existing file.
    pub changed: bool,
    /// Number of bytes in the resulting file.
    pub bytes: usize,
}

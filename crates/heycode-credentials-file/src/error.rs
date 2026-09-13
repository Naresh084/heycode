//! Secure file and migration failures.

/// Credential fallback I/O, shape, safety, and migration failures.
#[derive(Debug, thiserror::Error)]
pub enum FileCredentialError {
    /// File operation failed.
    #[error("credential file {path}: {source}")]
    Io {
        /// Path involved.
        path: String,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// TOML or legacy line was malformed.
    #[error("invalid credential file {path}: {message}")]
    Parse {
        /// Path involved.
        path: String,
        /// Safe parse detail (never a value).
        message: String,
    },
    /// Symbolic link was refused.
    #[error("credential path {path} is a symbolic link; use an owner-only regular path")]
    SymbolicLink {
        /// Rejected path.
        path: String,
    },
    /// Existing credential path had the wrong file type.
    #[error("credential path {path} is not {expected}")]
    WrongFileType {
        /// Rejected path.
        path: String,
        /// Expected kind.
        expected: &'static str,
    },
    /// New file schema is newer than this binary.
    #[error("credential file {path} uses newer schema {found}; this heycode supports {supported}")]
    NewerSchema {
        /// Path involved.
        path: String,
        /// Version found.
        found: u32,
        /// Highest supported.
        supported: u32,
    },
    /// New and legacy active values disagreed.
    #[error("legacy credential `{reference}` has conflicting values in old and new stores")]
    MigrationConflict {
        /// Conflicting non-secret reference.
        reference: String,
    },
    /// Backup exists with different bytes.
    #[error("legacy credential backup {path} already exists with different bytes")]
    BackupConflict {
        /// Conflicting backup path.
        path: String,
    },
    /// Writer mutex was poisoned.
    #[error("credential file writer is unavailable after a previous panic")]
    WriterUnavailable,
}

impl FileCredentialError {
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }

    pub(crate) fn parse(path: &std::path::Path, message: impl Into<String>) -> Self {
        Self::Parse {
            path: path.display().to_string(),
            message: message.into(),
        }
    }
}

//! File settings provider failures.

/// Load and persistence failures from a settings document.
#[derive(Debug, thiserror::Error)]
pub enum FileSettingsError {
    /// File operation failed.
    #[error("settings file {path}: {source}")]
    Io {
        /// File involved.
        path: String,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// TOML did not match the settings document contract.
    #[error("invalid settings file {path}: {message}")]
    Parse {
        /// File involved.
        path: String,
        /// Parser/shape detail.
        message: String,
    },
    /// A newer binary owns this schema.
    #[error(
        "settings file {path} uses newer settings schema {found}; this heycode supports {supported}"
    )]
    NewerSchema {
        /// File involved.
        path: String,
        /// Version found.
        found: u32,
        /// Highest version supported here.
        supported: u32,
    },
    /// Symlinks are refused so writes cannot replace a surprising target.
    #[error("settings file {path} is a symbolic link; use a regular file")]
    SymbolicLink {
        /// Rejected path.
        path: String,
    },
    /// Existing path was not a regular file.
    #[error("settings file {path} is not a regular file")]
    NotRegular {
        /// Rejected path.
        path: String,
    },
    /// In-process writer mutex was poisoned.
    #[error("settings file writer is unavailable after a previous panic")]
    WriterUnavailable,
    /// Filesystem watcher could not start or subscribe.
    #[error("settings file watcher: {message}")]
    Watch {
        /// Backend failure.
        message: String,
    },
    /// Reloaded documents failed settings resolution/publication.
    #[error("settings file reload rejected: {message}")]
    ReloadRejected {
        /// Redacted validation/publication detail.
        message: String,
    },
}

impl FileSettingsError {
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

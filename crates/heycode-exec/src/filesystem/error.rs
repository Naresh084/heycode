//! Fixed, body-free filesystem failure taxonomy.

/// Stable filesystem failure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileSystemErrorCode {
    /// A caller supplied an invalid path, cap, pattern, or mutation spec.
    InvalidSpec,
    /// A provider returned an outcome that violates the service contract.
    InvalidOutput,
    /// The selected filesystem provider does not implement the requested operation.
    UnsupportedOperation,
    /// A path contains parent traversal or crosses a capability/symlink root.
    PathTraversal,
    /// No declared root grants access to the requested absolute path.
    OutsideAllowedRoots,
    /// A matching root grants reads but not mutation.
    ReadOnlyRoot,
    /// The requested path does not exist.
    NotFound,
    /// The requested path is not a regular file.
    NotFile,
    /// The requested path is not a directory.
    NotDirectory,
    /// The operating system denied the operation.
    PermissionDenied,
    /// A text operation encountered binary content.
    Binary,
    /// A text edit encountered invalid UTF-8.
    InvalidUtf8,
    /// An overwrite or edit was attempted before a successful read.
    NotObserved,
    /// An edit target changed after its successful read.
    StaleObservation,
    /// A mutation parent or target changed during the final commit window.
    ChangedAtCommit,
    /// A non-global edit did not match exactly once.
    MatchCount,
    /// A search expression is invalid.
    Pattern,
    /// The caller cancelled the operation.
    Cancelled,
    /// The owning filesystem provider has shut down.
    ServiceStopped,
    /// The provider reported an uncategorized filesystem failure.
    Io,
}

impl FileSystemErrorCode {
    /// Stable machine identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid_spec",
            Self::InvalidOutput => "invalid_output",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::PathTraversal => "path_traversal",
            Self::OutsideAllowedRoots => "outside_allowed_roots",
            Self::ReadOnlyRoot => "read_only_root",
            Self::NotFound => "not_found",
            Self::NotFile => "not_file",
            Self::NotDirectory => "not_directory",
            Self::PermissionDenied => "permission_denied",
            Self::Binary => "binary",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::NotObserved => "not_observed",
            Self::StaleObservation => "stale_observation",
            Self::ChangedAtCommit => "changed_at_commit",
            Self::MatchCount => "match_count",
            Self::Pattern => "pattern",
            Self::Cancelled => "cancelled",
            Self::ServiceStopped => "service_stopped",
            Self::Io => "io",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid filesystem specification",
            Self::InvalidOutput => "filesystem provider returned an invalid outcome",
            Self::UnsupportedOperation => "filesystem provider does not support this operation",
            Self::PathTraversal => "filesystem path crosses a protected root boundary",
            Self::OutsideAllowedRoots => "filesystem path is outside allowed roots",
            Self::ReadOnlyRoot => "filesystem root is read-only",
            Self::NotFound => "filesystem path was not found",
            Self::NotFile => "filesystem path is not a regular file",
            Self::NotDirectory => "filesystem path is not a directory",
            Self::PermissionDenied => "filesystem operation was denied",
            Self::Binary => "filesystem content is binary",
            Self::InvalidUtf8 => "filesystem content is not valid UTF-8",
            Self::NotObserved => "filesystem path has not been observed",
            Self::StaleObservation => "filesystem observation is stale",
            Self::ChangedAtCommit => "filesystem target changed before commit",
            Self::MatchCount => "filesystem edit match count is invalid",
            Self::Pattern => "filesystem search pattern is invalid",
            Self::Cancelled => "filesystem operation was cancelled",
            Self::ServiceStopped => "filesystem service has stopped",
            Self::Io => "filesystem operation failed",
        }
    }
}

impl std::fmt::Display for FileSystemErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A fixed filesystem error that retains no path, file body, pattern, or raw
/// operating-system diagnostic.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct FileSystemError {
    code: FileSystemErrorCode,
    message: &'static str,
    match_count: Option<usize>,
    edit_index: Option<usize>,
}

impl FileSystemError {
    /// Construct the fixed public message for `code`.
    #[must_use]
    pub const fn new(code: FileSystemErrorCode) -> Self {
        Self {
            code,
            message: code.message(),
            match_count: None,
            edit_index: None,
        }
    }

    pub(crate) const fn invalid_match_count(count: usize) -> Self {
        Self {
            code: FileSystemErrorCode::MatchCount,
            message: FileSystemErrorCode::MatchCount.message(),
            match_count: Some(count),
            edit_index: None,
        }
    }

    /// Stable failure classification.
    #[must_use]
    pub const fn code(&self) -> FileSystemErrorCode {
        self.code
    }

    /// Safe numeric match count for [`FileSystemErrorCode::MatchCount`].
    #[must_use]
    pub const fn match_count(&self) -> Option<usize> {
        self.match_count
    }

    /// One-based failed edit within a batch, when applicable.
    #[must_use]
    pub const fn edit_index(&self) -> Option<usize> {
        self.edit_index
    }

    pub(crate) fn at_edit(mut self, index: usize) -> Self {
        self.edit_index = Some(index);
        self
    }
}

pub(crate) fn from_io(error: &std::io::Error) -> FileSystemError {
    let code = if is_symlink_loop(error) {
        FileSystemErrorCode::PathTraversal
    } else {
        match error.kind() {
            std::io::ErrorKind::NotFound => FileSystemErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => FileSystemErrorCode::PermissionDenied,
            std::io::ErrorKind::InvalidData => FileSystemErrorCode::InvalidUtf8,
            _ => FileSystemErrorCode::Io,
        }
    };
    FileSystemError::new(code)
}

fn is_symlink_loop(error: &std::io::Error) -> bool {
    let raw = error.raw_os_error();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        raw == Some(40)
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        raw == Some(62)
    }
    #[cfg(windows)]
    {
        // ERROR_TOO_MANY_LINKS and ERROR_CANT_RESOLVE_FILENAME.
        raw == Some(1142) || raw == Some(1921)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        windows
    )))]
    {
        let _ = raw;
        false
    }
}

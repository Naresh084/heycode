//! File-provider paths.

/// Paths owned/read by the settings file provider.
#[derive(Debug, Clone)]
pub struct FileSettingsConfig {
    /// Writable user document.
    pub user_path: std::path::PathBuf,
    /// Optional already-trusted project document (read-only in S02).
    pub project_path: Option<std::path::PathBuf>,
    /// Watch parent directories and publish valid external changes.
    pub watch: bool,
}

impl FileSettingsConfig {
    /// Configure only a writable user document.
    #[must_use]
    pub fn user(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            user_path: path.into(),
            project_path: None,
            watch: true,
        }
    }

    /// Add an already-trusted project document.
    #[must_use]
    pub fn with_project(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.project_path = Some(path.into());
        self
    }

    /// Disable filesystem watching (deterministic provider component tests).
    #[must_use]
    pub fn without_watch(mut self) -> Self {
        self.watch = false;
        self
    }
}

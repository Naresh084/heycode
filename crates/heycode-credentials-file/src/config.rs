//! Credential storage paths.

/// Root and derived paths for the owner-only credential store.
#[derive(Debug, Clone)]
pub struct FileCredentialConfig {
    /// heycode home/credential root, enforced to `0700` on Unix.
    pub root: std::path::PathBuf,
}

impl FileCredentialConfig {
    /// Use `root/credentials.toml`, migrate `root/credentials`, and archive to
    /// `root/credentials.legacy.bak`.
    #[must_use]
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(crate) fn current_path(&self) -> std::path::PathBuf {
        self.root.join("credentials.toml")
    }

    pub(crate) fn legacy_path(&self) -> std::path::PathBuf {
        self.root.join("credentials")
    }

    pub(crate) fn backup_path(&self) -> std::path::PathBuf {
        self.root.join("credentials.legacy.bak")
    }
}

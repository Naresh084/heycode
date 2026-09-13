//! File catalog persistence paths.

/// Exact path of the standalone catalog cache document.
#[derive(Debug, Clone)]
pub struct FileCatalogConfig {
    /// Versioned JSON cache path, normally `$HEYCODE_HOME/cache/models.json`.
    pub path: std::path::PathBuf,
}

impl FileCatalogConfig {
    /// Configure one exact standalone cache path.
    #[must_use]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

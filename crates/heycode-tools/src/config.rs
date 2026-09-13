//! Runtime configuration for the built-in tool set.

/// Tunables for the built-in tools, sourced from the `[tools]` config section.
///
/// Filesystem/web built-ins receive an [`std::sync::Arc<ToolsConfig>`] at
/// construction. Shell defaults belong to `heycode-exec::ShellService`.
#[derive(Debug, Clone)]
pub struct ToolsConfig {
    /// Maximum number of file bytes `read` returns before truncating.
    pub read_max_bytes: usize,
    /// Maximum number of lines `read` returns before truncating.
    pub read_max_lines: usize,
    /// Register `web_fetch` / `web_search` (native-grade: keyless search).
    pub web_enabled: bool,
    /// Register the E07 persistent-terminal tools. Requires the `terminal`
    /// service; composition fails loud when it is absent rather than silently
    /// dropping the tools.
    pub terminals_enabled: bool,
    /// Working directory persistent terminals start in.
    pub cwd: std::path::PathBuf,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            read_max_bytes: 262_144,
            read_max_lines: 2_000,
            web_enabled: true,
            // Persistent terminals are opt-in: they hold live processes for the
            // life of the context, so an intentional profile must ask for them.
            terminals_enabled: false,
            cwd: std::path::PathBuf::from("."),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_documented_caps() {
        let cfg = ToolsConfig::default();
        assert_eq!(cfg.read_max_bytes, 262_144);
        assert_eq!(cfg.read_max_lines, 2_000);
    }

    #[test]
    fn config_is_cloneable_and_debuggable() {
        let cfg = ToolsConfig::default();
        let clone = cfg.clone();
        assert_eq!(clone.read_max_bytes, cfg.read_max_bytes);
        assert!(format!("{cfg:?}").contains("read_max_bytes"));
    }
}

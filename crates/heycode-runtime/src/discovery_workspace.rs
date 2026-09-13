//! Empty effect-owned workspaces for account/catalog probes before project trust.

use std::path::Path;
use std::sync::Arc;

/// Private temporary directory retained by every runtime operation using it.
///
/// It contains no invocation-directory configuration. Actual sessions still
/// receive the caller's workspace through `RuntimeStart`/`RuntimeResume`.
pub struct RuntimeDiscoveryWorkspace {
    directory: tempfile::TempDir,
}

impl RuntimeDiscoveryWorkspace {
    /// Create an empty directory and register its ownership before runtime effects.
    ///
    /// # Errors
    /// Temporary-directory creation fails without publishing a workspace.
    pub fn register(context: &heycode_core::Context) -> heycode_core::CoreResult<Arc<Self>> {
        let directory = tempfile::Builder::new()
            .prefix("heycode-runtime-discovery-")
            .tempdir()
            .map_err(|_| {
                heycode_core::CoreError::other("runtime discovery workspace is unavailable")
            })?;
        let owner = Arc::new(Self { directory });
        let effect_owner = owner.clone();
        context.effect(move || drop(effect_owner));
        Ok(owner)
    }

    /// Empty absolute directory used exclusively for discovery operations.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}

impl std::fmt::Debug for RuntimeDiscoveryWorkspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RuntimeDiscoveryWorkspace(<private>)")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn operation_owner_outlives_context_and_last_owner_removes_directory() {
        let mut context = heycode_core::Context::new();
        let workspace = RuntimeDiscoveryWorkspace::register(&context).unwrap();
        let path = workspace.path().to_owned();
        assert!(path.is_absolute());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        context.shutdown();
        assert!(path.is_dir());
        drop(workspace);
        assert!(!path.exists());
    }
}

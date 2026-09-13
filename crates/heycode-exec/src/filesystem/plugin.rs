//! Built-in local filesystem provider plugin.

use heycode_core::{Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor};

use super::{FileSystemPolicy, FileSystemService, LocalFileSystemBackend};
use crate::{SERVICE_FILESYSTEM, SERVICE_SANDBOX, SandboxService};

/// Publish the built-in local filesystem provider.
#[must_use]
pub fn local_filesystem_plugin() -> Box<dyn Plugin> {
    struct LocalFileSystemPlugin;

    impl Plugin for LocalFileSystemPlugin {
        fn name(&self) -> &'static str {
            "filesystem-local"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_FILESYSTEM]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SANDBOX]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let sandbox = context
                .get::<SandboxService>(SERVICE_SANDBOX)
                .ok_or_else(|| CoreError::other("sandbox service type mismatch"))?;
            let policy = FileSystemPolicy::from_sandbox(sandbox.policy())
                .map_err(|_| CoreError::other("filesystem policy is invalid"))?;
            let backend = std::sync::Arc::new(
                LocalFileSystemBackend::new(policy)
                    .map_err(|_| CoreError::other("filesystem roots could not be opened"))?,
            );
            let shutdown = backend.shutdown_token();
            context.effect(move || shutdown.cancel());
            context.provide(
                SERVICE_FILESYSTEM,
                self.name(),
                FileSystemService::new(backend),
            )
        }
    }

    Box::new(LocalFileSystemPlugin)
}

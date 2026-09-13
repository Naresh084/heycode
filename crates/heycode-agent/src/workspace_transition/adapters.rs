//! Stable service handles shared by already-composed Consumers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use heycode_exec::*;
use tokio_util::sync::CancellationToken;

use super::{WorkspaceRoot, WorkspaceTransitionService};

pub(super) struct WorkspaceFilesystem(pub Arc<WorkspaceTransitionService>);

// Each future owns a generation lease until completion/cancellation. A transition
// cannot race an in-flight mutation simply because resolution already finished.
#[async_trait]
impl FileSystemBackend for WorkspaceFilesystem {
    fn resolve(&self, request: PathRequest) -> Result<ResolvedPath, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation.generation.filesystem.resolve(request)
    }

    fn observations(&self) -> ObservationLog {
        self.0
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
            .filesystem
            .observations()
    }

    fn policy(&self) -> FileSystemPolicy {
        self.0
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
            .filesystem
            .policy()
    }

    async fn metadata(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<FileMetadata, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .metadata(path, cancellation)
            .await
    }

    async fn create_dir_all(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .create_dir_all(path, cancellation)
            .await
    }

    async fn read(
        &self,
        spec: ReadFileSpec,
        cancellation: CancellationToken,
    ) -> Result<ReadFileOutput, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .read(spec, cancellation)
            .await
    }

    async fn write(
        &self,
        spec: WriteFileSpec,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .write(spec, cancellation)
            .await
    }

    async fn edit(
        &self,
        spec: EditFileSpec,
        cancellation: CancellationToken,
    ) -> Result<EditFileOutput, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .edit(spec, cancellation)
            .await
    }

    async fn write_checked(
        &self,
        spec: CheckedWriteSpec,
        cancellation: CancellationToken,
    ) -> Result<CheckedWriteOutput, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .write_checked(spec, cancellation)
            .await
    }

    async fn edit_many(
        &self,
        spec: MultiEditSpec,
        cancellation: CancellationToken,
    ) -> Result<MultiEditOutput, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .edit_many(spec, cancellation)
            .await
    }

    async fn glob(
        &self,
        spec: GlobSpec,
        cancellation: CancellationToken,
    ) -> Result<GlobOutput, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .glob(spec, cancellation)
            .await
    }

    async fn grep(
        &self,
        spec: GrepSpec,
        cancellation: CancellationToken,
    ) -> Result<GrepOutput, FileSystemError> {
        let operation = self.0.operation().map_err(|_| fs_stopped())?;
        operation
            .generation
            .filesystem
            .grep(spec, cancellation)
            .await
    }
}

fn fs_stopped() -> FileSystemError {
    FileSystemError::new(FileSystemErrorCode::ServiceStopped)
}
fn shell_stopped() -> ProcessError {
    ProcessError::new(ProcessErrorCode::ServiceStopped)
}

pub(super) struct WorkspaceShell(pub Arc<WorkspaceTransitionService>);

#[async_trait]
impl ShellBackend for WorkspaceShell {
    fn subprocess(&self) -> Option<SubprocessService> {
        self.0.operation().ok()?.generation.shell.subprocess()
    }

    fn resolve(&self, request: ShellRequest) -> Result<ShellSpec, ProcessError> {
        let operation = self.0.operation().map_err(|_| shell_stopped())?;
        operation.generation.shell.resolve(request)
    }

    async fn execute(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        let operation = self.0.operation().map_err(|_| shell_stopped())?;
        validate_shell_scope(spec.cwd(), &operation.generation.snapshot.roots)?;
        operation.generation.shell.execute(spec, cancellation).await
    }

    async fn execute_streaming(
        &self,
        spec: ShellSpec,
        cancellation: CancellationToken,
        sink: Arc<dyn ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        let operation = self.0.operation().map_err(|_| shell_stopped())?;
        validate_shell_scope(spec.cwd(), &operation.generation.snapshot.roots)?;
        operation
            .generation
            .shell
            .execute_streaming(spec, cancellation, sink)
            .await
    }
}

/// Exact cwd/roots pin for child Consumers and the current generation resolver.
pub(super) struct PinnedWorkspaceShell {
    pub resolver: ShellService,
    pub cwd: PathBuf,
    pub roots: Vec<WorkspaceRoot>,
}

#[async_trait]
impl ShellBackend for PinnedWorkspaceShell {
    fn resolve(&self, request: ShellRequest) -> Result<ShellSpec, ProcessError> {
        let spec = self
            .resolver
            .resolve(request.with_default_cwd(self.cwd.clone())?)?;
        validate_shell_scope(spec.cwd(), &self.roots)?;
        Ok(spec)
    }

    async fn execute(
        &self,
        _spec: ShellSpec,
        _cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        // Only ShellService::with_executor may publish this resolver; direct
        // execution without the host-created sandbox executor is forbidden.
        Err(ProcessError::new(ProcessErrorCode::Sandbox))
    }
}

fn validate_shell_scope(cwd: &Path, roots: &[WorkspaceRoot]) -> Result<(), ProcessError> {
    let canonical =
        std::fs::canonicalize(cwd).map_err(|_| ProcessError::new(ProcessErrorCode::InvalidSpec))?;
    if canonical != cwd || !roots.iter().any(|root| canonical.starts_with(&root.path)) {
        return Err(ProcessError::new(ProcessErrorCode::Sandbox));
    }
    Ok(())
}

/// Replace the ordinary `filesystem-local` factory before Consumers are built.
pub fn workspace_filesystem_plugin(
    service: Arc<WorkspaceTransitionService>,
) -> Box<dyn heycode_core::Plugin> {
    struct Plugin(Arc<WorkspaceTransitionService>);
    impl heycode_core::Plugin for Plugin {
        fn name(&self) -> &'static str {
            "filesystem-local"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_FILESYSTEM]
        }
        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            context.provide(SERVICE_FILESYSTEM, self.name(), self.0.filesystem())
        }
    }
    Box::new(Plugin(service))
}

/// Replace the ordinary `shell-local` factory before Consumers are built.
pub fn workspace_shell_plugin(
    service: Arc<WorkspaceTransitionService>,
) -> Box<dyn heycode_core::Plugin> {
    struct Plugin(Arc<WorkspaceTransitionService>);
    impl heycode_core::Plugin for Plugin {
        fn name(&self) -> &'static str {
            "shell-local"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SHELL]
        }
        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            context.provide(SERVICE_SHELL, self.name(), self.0.shell())
        }
    }
    Box::new(Plugin(service))
}

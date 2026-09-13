//! O06 worktree-backed delegated-runtime subagent provider.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_llm::CapabilitySupport;
use heycode_runtime::{AgentRuntime, AgentRuntimeKind};
use tokio_util::sync::CancellationToken;

use crate::approval::ApprovalPolicy;
use crate::runtime_subagent::RuntimeSubagentProvider;
use crate::subagent_provider::{
    SubagentCapabilities, SubagentContinuation, SubagentError, SubagentErrorCode, SubagentProvider,
    SubagentProviderDescriptor, SubagentRegistry, SubagentRequest, SubagentSeed, SubagentStarted,
};
use crate::{GitCommitId, GitWorktreeManager, WorktreeOutcome, WorktreeRetention};

/// Static plugin binding for one exact-base worktree runtime provider.
#[derive(Clone)]
pub struct WorktreeSubagentConfig {
    plugin_id: &'static str,
    runtime_id: &'static str,
    provider_id: &'static str,
    display_name: &'static str,
    sessions_root: PathBuf,
    repository: PathBuf,
    worktrees_root: PathBuf,
    base: GitCommitId,
    retention: WorktreeRetention,
    max_depth: u32,
}

impl WorktreeSubagentConfig {
    /// Bind a plugin/runtime/provider identity to exact storage and base facts.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        plugin_id: &'static str,
        runtime_id: &'static str,
        provider_id: &'static str,
        display_name: &'static str,
        sessions_root: PathBuf,
        repository: PathBuf,
        worktrees_root: PathBuf,
        base: GitCommitId,
        retention: WorktreeRetention,
        max_depth: u32,
    ) -> Self {
        Self {
            plugin_id,
            runtime_id,
            provider_id,
            display_name,
            sessions_root,
            repository,
            worktrees_root,
            base,
            retention,
            max_depth,
        }
    }
}

/// Register one delegated runtime through an isolated exact-base Git worktree.
#[must_use]
pub fn worktree_subagent_plugin(config: WorktreeSubagentConfig) -> Box<dyn heycode_core::Plugin> {
    struct WorktreeSubagentPlugin(WorktreeSubagentConfig);

    impl heycode_core::Plugin for WorktreeSubagentPlugin {
        fn name(&self) -> &'static str {
            self.0.plugin_id
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.0.plugin_id,
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SubagentProvider,
                self.0.provider_id,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_exec::SERVICE_SUBPROCESS,
                heycode_runtime::SERVICE_RUNTIMES,
                crate::SERVICE_SUBAGENTS,
                crate::SERVICE_APPROVAL,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let subprocess = context
                .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                .ok_or_else(|| heycode_core::CoreError::other("subprocess service missing"))?;
            let runtimes = context
                .get::<heycode_runtime::AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
                .ok_or_else(|| heycode_core::CoreError::other("runtime registry missing"))?;
            let runtime = runtimes
                .get(self.0.runtime_id)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?
                .ok_or_else(|| heycode_core::CoreError::other("worktree runtime missing"))?;
            if runtime.descriptor().kind() != AgentRuntimeKind::Delegated
                || runtime.descriptor().capabilities().permissions != CapabilitySupport::Supported
            {
                return Err(heycode_core::CoreError::other(
                    "worktree runtime cannot prove delegated permission callbacks",
                ));
            }
            let approval = context
                .get::<crate::plugin::ApprovalHandle>(crate::SERVICE_APPROVAL)
                .ok_or_else(|| heycode_core::CoreError::other("approval policy missing"))?;
            let registry = context
                .get::<SubagentRegistry>(crate::SERVICE_SUBAGENTS)
                .ok_or_else(|| heycode_core::CoreError::other("subagent registry missing"))?;
            let manager = Arc::new(
                GitWorktreeManager::new(
                    (*subprocess).clone(),
                    self.0.repository.clone(),
                    self.0.worktrees_root.clone(),
                    self.0.retention,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
            );
            let provider = Arc::new(
                WorktreeRuntimeSubagentProvider::new(
                    runtime,
                    approval.0.clone(),
                    self.0.provider_id,
                    self.0.display_name,
                    self.0.sessions_root.clone(),
                    manager.clone(),
                    self.0.base.clone(),
                    self.0.max_depth,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
            );
            let registration = registry
                .register_owned(provider)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || drop(registration));
            context.effect(move || manager.dispose());
            Ok(())
        }
    }

    Box::new(WorktreeSubagentPlugin(config))
}

/// Fresh one-shot delegated provider whose runtime sees only a managed
/// exact-base Git checkout.
pub struct WorktreeRuntimeSubagentProvider {
    runtime: Arc<dyn AgentRuntime>,
    approval: Arc<dyn ApprovalPolicy>,
    descriptor: SubagentProviderDescriptor,
    sessions_root: PathBuf,
    manager: Arc<GitWorktreeManager>,
    base: GitCommitId,
    max_depth: u32,
}

impl WorktreeRuntimeSubagentProvider {
    /// Validate and bind the worktree provider.
    ///
    /// # Errors
    /// Invalid provider metadata is refused.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Arc<dyn AgentRuntime>,
        approval: Arc<dyn ApprovalPolicy>,
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        sessions_root: PathBuf,
        manager: Arc<GitWorktreeManager>,
        base: GitCommitId,
        max_depth: u32,
    ) -> Result<Self, SubagentError> {
        let descriptor = SubagentProviderDescriptor::new(
            provider_id,
            display_name,
            SubagentCapabilities {
                fork: CapabilitySupport::Unsupported,
                continuation: CapabilitySupport::Unsupported,
                interrupt: CapabilitySupport::Supported,
            },
        )
        .map_err(|error| SubagentError::new(SubagentErrorCode::Failed, error.to_string()))?;
        Ok(Self {
            runtime,
            approval,
            descriptor,
            sessions_root,
            manager,
            base,
            max_depth,
        })
    }
}

#[async_trait]
impl SubagentProvider for WorktreeRuntimeSubagentProvider {
    async fn readiness(
        &self,
        cancellation: CancellationToken,
    ) -> Result<crate::subagent_provider::SubagentReadiness, SubagentError> {
        crate::runtime_subagent::runtime_readiness(&self.runtime, cancellation).await
    }
    fn descriptor(&self) -> &SubagentProviderDescriptor {
        &self.descriptor
    }

    async fn start(
        &self,
        request: SubagentRequest,
        cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        if request.seed != SubagentSeed::Fresh
            || request.continuation != SubagentContinuation::OneShot
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Unsupported,
                "worktree subagent supports fresh one-shot work only",
            ));
        }
        if request.authority().depth() >= self.max_depth {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                format!("subagent depth limit {} reached", self.max_depth),
            ));
        }
        let lease = self
            .manager
            .create(self.base.clone(), cancellation.clone())
            .await
            .map_err(worktree_error)?;
        if let Some(record) = &request.task {
            record
                .update(|row| row.workspace = Some(lease.path().display().to_string()))
                .map_err(|error| {
                    SubagentError::new(SubagentErrorCode::Failed, error.to_string())
                })?;
        }
        let inner = match RuntimeSubagentProvider::new(
            self.runtime.clone(),
            self.approval.clone(),
            self.descriptor.id().as_str(),
            self.descriptor.display(),
            self.sessions_root.clone(),
            lease.path().to_path_buf(),
            self.max_depth,
        ) {
            Ok(provider) => provider,
            Err(error) => {
                let _cleanup = lease
                    .finish(WorktreeOutcome::Failure, CancellationToken::new())
                    .await;
                return Err(error);
            }
        };
        let operation = CancellationToken::new();
        let mut run = Box::pin(inner.start(request, operation.clone()));
        let manager_shutdown = self.manager.shutdown_token();
        let result = tokio::select! {
            result = &mut run => result,
            () = cancellation.cancelled() => {
                operation.cancel();
                run.await
            }
            () = manager_shutdown.cancelled() => {
                operation.cancel();
                run.await
            }
        };
        let outcome = match &result {
            Ok(_) => WorktreeOutcome::Success,
            Err(error) if error.code() == SubagentErrorCode::Cancelled => {
                WorktreeOutcome::Cancelled
            }
            Err(_) => WorktreeOutcome::Failure,
        };
        let result_path = lease.path().to_path_buf();
        let cleanup = lease
            .finish(outcome, CancellationToken::new())
            .await
            .map_err(worktree_error);
        match (result, cleanup) {
            (Ok(mut started), Ok(())) => {
                if result_path.exists() {
                    started.text.push_str(&format!(
                        "\n\n[worktree results retained: {}]",
                        result_path.display()
                    ));
                }
                Ok(started)
            }
            (Err(error), _) => Err(SubagentError::new(
                error.code(),
                format!(
                    "{}; worktree result location: {}",
                    error.message(),
                    result_path.display()
                ),
            )),
            (Ok(_), Err(error)) => Err(error),
        }
    }
}

fn worktree_error(error: crate::WorktreeError) -> SubagentError {
    let code = match error.code() {
        crate::WorktreeErrorCode::Cancelled => SubagentErrorCode::Cancelled,
        crate::WorktreeErrorCode::InvalidBase
        | crate::WorktreeErrorCode::InvalidConfig
        | crate::WorktreeErrorCode::Unknown => SubagentErrorCode::Refused,
        crate::WorktreeErrorCode::GitUnavailable
        | crate::WorktreeErrorCode::StateCorrupt
        | crate::WorktreeErrorCode::OperationFailed => SubagentErrorCode::Failed,
    };
    SubagentError::new(code, error.to_string())
}

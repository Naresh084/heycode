//! Truthful placeholder for an optional runtime whose executable is absent.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_llm::{CapabilitySupport, CatalogSnapshot};
use tokio_util::sync::CancellationToken;

use crate::{
    AccountState, AccountStatus, AgentRuntime, AgentRuntimeDescriptor, RuntimeError, RuntimeFork,
    RuntimeResume, RuntimeSession, RuntimeStart,
};

/// Registered discovery row for an optional runtime unavailable on this host.
///
/// This keeps product composition and runtime discovery truthful without
/// launching a process or converting a missing optional installation into a
/// plugin activation failure. A recompose is required after installation.
pub struct UnavailableAgentRuntime {
    descriptor: AgentRuntimeDescriptor,
    lifecycle: CancellationToken,
}

impl UnavailableAgentRuntime {
    /// Bind the immutable capabilities the implementation will expose when its
    /// external runtime becomes available.
    #[must_use]
    pub fn new(descriptor: AgentRuntimeDescriptor) -> Self {
        Self {
            descriptor,
            lifecycle: CancellationToken::new(),
        }
    }

    /// Retire this optional runtime generation during plugin disposal.
    pub fn shutdown(&self) {
        self.lifecycle.cancel();
    }

    fn check(&self, cancellation: &CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else if self.lifecycle.is_cancelled() {
            Err(RuntimeError::closed())
        } else {
            Ok(())
        }
    }

    fn unavailable_capability<T>(
        &self,
        support: CapabilitySupport,
        cancellation: &CancellationToken,
    ) -> Result<T, RuntimeError> {
        self.check(cancellation)?;
        match support {
            CapabilitySupport::Unsupported => Err(RuntimeError::unsupported()),
            CapabilitySupport::Supported | CapabilitySupport::Unknown => {
                Err(RuntimeError::unavailable())
            }
        }
    }
}

#[async_trait]
impl AgentRuntime for UnavailableAgentRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.descriptor
    }

    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError> {
        self.check(&cancellation)?;
        Ok(AccountState::without_label(AccountStatus::Unavailable))
    }

    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, RuntimeError> {
        self.unavailable_capability(self.descriptor.capabilities().models, &cancellation)
    }

    async fn start(
        &self,
        _request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        self.check(&cancellation)?;
        Err(RuntimeError::unavailable())
    }

    async fn resume(
        &self,
        _request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        self.unavailable_capability(self.descriptor.capabilities().resume, &cancellation)
    }

    async fn fork(
        &self,
        _request: RuntimeFork,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        self.unavailable_capability(self.descriptor.capabilities().fork, &cancellation)
    }
}

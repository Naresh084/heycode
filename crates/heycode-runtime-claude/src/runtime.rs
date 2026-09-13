//! R01 runtime registration surface over the R07 process client.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use heycode_llm::{CapabilitySupport, CatalogSnapshot, ModelDescriptor, ProviderDescriptor};
use heycode_runtime::{
    AccountState, AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeKind, RuntimeConfiguration,
    RuntimeConfigurationCapabilities, RuntimeError, RuntimeFork, RuntimeModelConfiguration,
    RuntimeResume, RuntimeSession, RuntimeStart, RuntimeToolExecutor,
};
use tokio_util::sync::CancellationToken;

use crate::{
    ClaudeCliClient, ClaudeCliVersion, ClaudeHandshakeReceipt, ClaudeRuntimeConfig,
    ClaudeRuntimeConfigError,
};

/// Stable delegated runtime registry id.
pub const CLAUDE_RUNTIME_ID: &str = "claude";

/// Installed Claude Code delegated-runtime Provider foundation.
pub struct ClaudeRuntime {
    descriptor: AgentRuntimeDescriptor,
    client: ClaudeCliClient,
    catalog_revision: AtomicU64,
}

impl ClaudeRuntime {
    /// Resolve the executable and build the immutable delegated descriptor.
    ///
    /// # Errors
    /// Invalid configuration, missing executable or impossible constant
    /// descriptor validation fails before registry publication.
    pub fn new(
        subprocess: heycode_exec::SubprocessService,
        config: ClaudeRuntimeConfig,
    ) -> Result<Self, ClaudeRuntimeConfigError> {
        let descriptor = AgentRuntimeDescriptor::new(
            CLAUDE_RUNTIME_ID,
            "Claude Code",
            AgentRuntimeKind::Delegated,
            crate::session::runtime_capabilities(),
        )
        .map(|descriptor| {
            descriptor.with_configuration_capabilities(RuntimeConfigurationCapabilities {
                system_prompt: CapabilitySupport::Supported,
                tools: CapabilitySupport::Supported,
                model: CapabilitySupport::Supported,
                reasoning_effort: CapabilitySupport::Supported,
            })
        })
        .and_then(|descriptor| descriptor.with_connection_help("Install Claude Code from code.claude.com/docs/en/setup. Sign in with `claude auth login --claudeai`, then return here and press Enter to retry."))
        .map_err(|_| ClaudeRuntimeConfigError::InvalidDescriptor)?;
        Ok(Self {
            descriptor,
            client: ClaudeCliClient::new(subprocess, config),
            catalog_revision: AtomicU64::new(0),
        })
    }

    /// Detect the compatible installed CLI version.
    ///
    /// # Errors
    /// See [`ClaudeCliClient::version`].
    pub async fn version(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ClaudeCliVersion, RuntimeError> {
        self.client.version(cancellation).await
    }

    /// Run the fixed tool-free no-persistence compatibility query.
    ///
    /// # Errors
    /// See [`ClaudeCliClient::handshake`].
    pub async fn handshake(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ClaudeHandshakeReceipt, RuntimeError> {
        self.client.handshake(cancellation).await
    }

    /// Quiescent and idempotent runtime process close.
    ///
    /// # Errors
    /// See [`ClaudeCliClient::close`].
    pub async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        self.client.close(cancellation).await
    }

    pub(crate) fn force_close(&self) {
        self.client.force_close();
    }

    async fn open_session(
        &self,
        identity: crate::session::SessionIdentity<'_>,
        expected_session: Option<String>,
        configuration: &RuntimeConfiguration,
        tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
        workspace: &std::path::Path,
        cancellation: CancellationToken,
    ) -> Result<Arc<crate::session::ClaudeSession>, RuntimeError> {
        validate_configuration(configuration, tool_executor.as_ref())?;
        let lazy_init = matches!(
            identity,
            crate::session::SessionIdentity::Ephemeral { .. }
                | crate::session::SessionIdentity::New { .. }
        );
        let (process, lifecycle) = self
            .client
            .launch_session(identity, configuration, workspace, cancellation.clone())
            .await?;
        let (process, input, lines) = process.into_parts();
        let session = crate::session::ClaudeSession::open(
            crate::session::SessionParts {
                process,
                input,
                lines,
                lifecycle,
                runtime_id: self.descriptor.id().clone(),
                expected_session,
                lazy_init,
                configuration: configuration.clone(),
                tool_executor,
            },
            cancellation,
        )
        .await?;
        Ok(session)
    }

    async fn probe_models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<crate::session::ClaudeAdvertisedModel>, RuntimeError> {
        let session_id = crate::session::uuid_v4();
        let session = self
            .open_session(
                crate::session::SessionIdentity::Ephemeral {
                    session_id: &session_id,
                },
                Some(session_id.clone()),
                &RuntimeConfiguration::new(),
                None,
                self.client.discovery_workspace(),
                cancellation,
            )
            .await?;
        let models = session.advertised_models();
        let closed = session.close(CancellationToken::new()).await;
        match (models, closed) {
            (Ok(models), Ok(())) => Ok(models),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }
}

fn validate_configuration(
    configuration: &RuntimeConfiguration,
    executor: Option<&Arc<dyn RuntimeToolExecutor>>,
) -> Result<(), RuntimeError> {
    if !configuration.tools().is_empty() && executor.is_none() {
        return Err(RuntimeError::invalid_request());
    }
    Ok(())
}

fn advertised_model_descriptor(row: crate::session::ClaudeAdvertisedModel) -> ModelDescriptor {
    let mut model = ModelDescriptor::unknown(row.id);
    model.display_name = row.display_name;
    // An omitted/empty provider list is absence of evidence, not evidence
    // that the model cannot reason. Preserve Unknown unless initialize
    // positively advertises effort choices.
    if !row.reasoning_efforts.is_empty() {
        model.capabilities.reasoning = CapabilitySupport::Supported;
    }
    model
}

impl std::fmt::Debug for ClaudeRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClaudeRuntime")
            .field("descriptor", &self.descriptor)
            .field("client", &self.client)
            .finish()
    }
}

#[async_trait]
impl AgentRuntime for ClaudeRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.descriptor
    }

    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError> {
        self.client.account(cancellation).await
    }

    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<heycode_llm::CatalogSnapshot, RuntimeError> {
        let rows = self.probe_models(cancellation).await?;
        let revision = self
            .catalog_revision
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::internal("claude catalog revision exhausted"))?
            .saturating_add(1);
        let fetched_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeError::internal("clock unavailable"))?
            .as_millis()
            .try_into()
            .map_err(|_| RuntimeError::internal("clock overflow"))?;
        let mut models = rows
            .into_iter()
            .map(advertised_model_descriptor)
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(CatalogSnapshot {
            provider: ProviderDescriptor {
                id: CLAUDE_RUNTIME_ID.to_owned(),
                display_name: "Claude Code".to_owned(),
                protocols: vec![heycode_core::ProviderProtocol::DelegatedAgent],
            },
            models,
            revision,
            fetched_at_ms,
        })
    }

    async fn model_configurations(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<RuntimeModelConfiguration>, RuntimeError> {
        let rows = self.probe_models(cancellation).await?;
        Ok(crate::session::advertised_model_configurations(&rows))
    }

    async fn start(
        &self,
        request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if request.ephemeral() {
            let session_id = crate::session::uuid_v4();
            return self
                .open_session(
                    crate::session::SessionIdentity::Ephemeral {
                        session_id: &session_id,
                    },
                    Some(session_id.clone()),
                    request.configuration(),
                    request.tool_executor().cloned(),
                    request.workspace(),
                    cancellation,
                )
                .await
                .map(|session| session as Arc<dyn RuntimeSession>);
        }
        // The host chooses the identity so `system/init` can be checked against
        // an expected value rather than trusted blindly.
        let session_id = crate::session::uuid_v4();
        self.open_session(
            crate::session::SessionIdentity::New {
                session_id: &session_id,
            },
            Some(session_id.clone()),
            request.configuration(),
            request.tool_executor().cloned(),
            request.workspace(),
            cancellation,
        )
        .await
        .map(|session| session as Arc<dyn RuntimeSession>)
    }

    async fn resume(
        &self,
        request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        let source = request.runtime_session_id().as_str().to_owned();
        self.open_session(
            crate::session::SessionIdentity::Resume {
                session_id: &source,
            },
            Some(source.clone()),
            request.configuration(),
            request.tool_executor().cloned(),
            request.workspace(),
            cancellation,
        )
        .await
        .map(|session| session as Arc<dyn RuntimeSession>)
    }

    async fn fork(
        &self,
        request: RuntimeFork,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        let source = request.source_runtime_session_id().as_str().to_owned();
        // A fork deliberately returns a NEW provider-native id, so the observed
        // identity must not be constrained to the source.
        self.open_session(
            crate::session::SessionIdentity::Fork {
                session_id: &source,
            },
            None,
            request.configuration(),
            request.tool_executor().cloned(),
            request.workspace(),
            cancellation,
        )
        .await
        .map(|session| session as Arc<dyn RuntimeSession>)
    }
}

#[cfg(test)]
mod tests {
    use super::advertised_model_descriptor;
    use crate::session::ClaudeAdvertisedModel;
    use heycode_llm::CapabilitySupport;

    #[test]
    fn catalog_reasoning_support_requires_positive_provider_evidence() {
        let unknown = advertised_model_descriptor(ClaudeAdvertisedModel {
            id: "claude-unspecified".to_owned(),
            display_name: "Claude Unspecified".to_owned(),
            resolved_model: None,
            description: None,
            reasoning_efforts: Vec::new(),
        });
        assert_eq!(unknown.capabilities.reasoning, CapabilitySupport::Unknown);

        let supported = advertised_model_descriptor(ClaudeAdvertisedModel {
            id: "claude-reasoning".to_owned(),
            display_name: "Claude Reasoning".to_owned(),
            resolved_model: None,
            description: None,
            reasoning_efforts: vec!["adaptive".to_owned()],
        });
        assert_eq!(
            supported.capabilities.reasoning,
            CapabilitySupport::Supported
        );
    }
}

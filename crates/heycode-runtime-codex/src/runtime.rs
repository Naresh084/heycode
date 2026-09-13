//! Concrete delegated runtime Provider and pinned connection factory.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_exec::{ProcessExit, ProcessOutput, RawInteractiveProcess, SubprocessService};
use heycode_llm::CapabilitySupport;
use heycode_runtime::{
    AccountState, AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeKind, RuntimeCapabilities,
    RuntimeConfigurationCapabilities, RuntimeError, RuntimeErrorCode, RuntimeFork, RuntimeResume,
    RuntimeSession, RuntimeStart,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::discovery::{
    MAX_MODEL_PAGES, MAX_MODELS, MODEL_PAGE_LIMIT, ModelPage, ProviderCapabilities, finish_catalog,
    parse_account,
};
use crate::{
    CodexAppServerClient, CodexAppServerConfig, CodexAppServerError, CodexAppServerErrorCode,
    CodexCliVersion,
};

/// Codex delegated runtime implementation at the R03/R04 process, account and
/// model-discovery boundary plus the R06 primary-session boundary. Follow-up
/// delivery remains explicitly unsupported: the pinned 0.153.2 app-server
/// offers no queued post-turn delivery method, and `thread/inject_items`
/// appends raw Responses items straight into model-visible history rather than
/// queueing a settled-turn follow-up.
pub struct CodexRuntime {
    subprocess: SubprocessService,
    config: Arc<CodexAppServerConfig>,
    descriptor: AgentRuntimeDescriptor,
    shutdown: CancellationToken,
}

impl CodexRuntime {
    /// Construct a standalone runtime connection factory.
    ///
    /// # Errors
    /// Static runtime descriptor validation failure.
    pub fn new(
        subprocess: SubprocessService,
        config: CodexAppServerConfig,
    ) -> Result<Self, CodexAppServerError> {
        Self::with_parts(subprocess, Arc::new(config), CancellationToken::new())
    }

    pub(crate) fn with_parts(
        subprocess: SubprocessService,
        config: Arc<CodexAppServerConfig>,
        shutdown: CancellationToken,
    ) -> Result<Self, CodexAppServerError> {
        let unsupported = CapabilitySupport::Unsupported;
        let descriptor = AgentRuntimeDescriptor::new(
            "codex",
            "Codex app server",
            AgentRuntimeKind::Delegated,
            RuntimeCapabilities {
                models: CapabilitySupport::Supported,
                resume: CapabilitySupport::Supported,
                fork: CapabilitySupport::Supported,
                steer: CapabilitySupport::Supported,
                follow_up: unsupported,
                permissions: CapabilitySupport::Supported,
                questions: CapabilitySupport::Supported,
                compaction: CapabilitySupport::Supported,
            },
        )
        .map(|descriptor| {
            descriptor.with_configuration_capabilities(RuntimeConfigurationCapabilities {
                system_prompt: CapabilitySupport::Supported,
                tools: CapabilitySupport::Supported,
                model: CapabilitySupport::Supported,
                reasoning_effort: CapabilitySupport::Supported,
            })
        })
        .and_then(|descriptor| descriptor.with_connection_help("Install the supported Codex CLI from developers.openai.com/codex/cli. Sign in with `codex login --device-auth`, then return here and press Enter to retry."))
        .map_err(|_| CodexAppServerError::new(CodexAppServerErrorCode::InvalidConfig))?;
        Ok(Self {
            subprocess,
            config,
            descriptor,
            shutdown,
        })
    }

    /// Resolve and verify the exact CLI, spawn one contained stdio app-server,
    /// and complete initialize/initialized.
    ///
    /// Every call re-resolves and re-verifies the executable so replacement is
    /// visible. Higher operations may use the returned client for reviewed
    /// stable account/model methods; this function itself invokes none.
    ///
    /// # Errors
    /// Resolution, identity/version mismatch, cancellation, handshake,
    /// protocol or contained-process settlement failure returns a body-free
    /// error.
    pub async fn connect(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CodexAppServerClient, CodexAppServerError> {
        self.connect_with_experimental(false, cancellation).await
    }

    pub(crate) async fn connect_with_experimental(
        &self,
        experimental_api: bool,
        cancellation: CancellationToken,
    ) -> Result<CodexAppServerClient, CodexAppServerError> {
        if self.shutdown.is_cancelled() || cancellation.is_cancelled() {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled));
        }
        let resolved = self.config.resolve(&self.subprocess)?;
        resolved.identity.verify()?;
        let version_operation = self.shutdown.child_token();
        let version = await_version_output(
            &self.subprocess,
            resolved.version,
            version_operation,
            cancellation.clone(),
        )
        .await?;
        let version = validate_version_output(version)?;
        resolved.identity.verify()?;
        if self.shutdown.is_cancelled() || cancellation.is_cancelled() {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled));
        }
        let lifecycle = self.shutdown.child_token();
        let process = await_server_spawn(
            &self.subprocess,
            resolved.server,
            lifecycle.clone(),
            cancellation.clone(),
        )
        .await?;
        CodexAppServerClient::initialize(
            process,
            version,
            &resolved.client_info,
            lifecycle,
            cancellation,
            experimental_api,
        )
        .await
        .map_err(|error| classify_initialization_error(error, self.config.outer_sandbox_mode))
    }
}

fn classify_initialization_error(
    error: CodexAppServerError,
    outer_sandbox_mode: heycode_exec::SandboxMode,
) -> CodexAppServerError {
    if outer_sandbox_mode != heycode_exec::SandboxMode::Off
        && matches!(
            error.code(),
            CodexAppServerErrorCode::Protocol
                | CodexAppServerErrorCode::Process
                | CodexAppServerErrorCode::Closed
        )
    {
        CodexAppServerError::new(CodexAppServerErrorCode::SandboxStartup)
    } else {
        error
    }
}

async fn await_server_spawn(
    subprocess: &SubprocessService,
    spec: heycode_exec::ProcessSpec,
    lifecycle: CancellationToken,
    cancellation: CancellationToken,
) -> Result<RawInteractiveProcess, CodexAppServerError> {
    let spawn = subprocess.spawn_interactive_raw(spec, lifecycle.clone());
    tokio::pin!(spawn);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            lifecycle.cancel();
            if let Ok(process) = spawn.await {
                settle_uninitialized(process).await;
            }
            Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled))
        }
        result = &mut spawn => result.map_err(CodexAppServerError::from),
    }
}

async fn settle_uninitialized(process: RawInteractiveProcess) {
    let (process, input, output) = process.into_raw_parts();
    drop(output);
    let _input_settled = input.finish().await;
    let _process_settled = process.cancel().await;
}

impl std::fmt::Debug for CodexRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexRuntime")
            .field("runtime_id", &self.descriptor.id())
            .field("config", &self.config)
            .field("shutdown", &self.shutdown.is_cancelled())
            .finish()
    }
}

#[async_trait]
impl AgentRuntime for CodexRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.descriptor
    }

    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError> {
        self.ensure_operation(&cancellation)?;
        let client = self
            .connect(cancellation.clone())
            .await
            .map_err(runtime_error)?;
        let result = async {
            let payload = client
                .request("account/read", json!({"refreshToken":false}), cancellation)
                .await?;
            parse_account(payload)
        }
        .await;
        settle_client(client, result).await
    }

    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<heycode_llm::CatalogSnapshot, RuntimeError> {
        self.ensure_operation(&cancellation)?;
        let client = self
            .connect(cancellation.clone())
            .await
            .map_err(runtime_error)?;
        let result = discover_models(&client, cancellation).await;
        settle_client(client, result).await
    }

    async fn model_configurations(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<heycode_runtime::RuntimeModelConfiguration>, RuntimeError> {
        self.ensure_operation(&cancellation)?;
        let client = self
            .connect(cancellation.clone())
            .await
            .map_err(runtime_error)?;
        let result = fetch_raw_models(&client, cancellation).await.map(|models| {
            models
                .iter()
                .filter_map(|model| model.configuration())
                .collect()
        });
        settle_client(client, result).await
    }

    async fn start(
        &self,
        request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        crate::session::start(self, request, cancellation).await
    }

    async fn resume(
        &self,
        request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        crate::session::resume(self, request, cancellation).await
    }

    async fn fork(
        &self,
        request: RuntimeFork,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        crate::session::fork(self, request, cancellation).await
    }
}

async fn discover_models(
    client: &CodexAppServerClient,
    cancellation: CancellationToken,
) -> Result<heycode_llm::CatalogSnapshot, CodexAppServerError> {
    let provider_capabilities = ProviderCapabilities::parse(
        client
            .request(
                "modelProvider/capabilities/read",
                json!({}),
                cancellation.clone(),
            )
            .await?,
    )?;
    let raw_models = fetch_raw_models(client, cancellation).await?;
    finish_catalog(raw_models, provider_capabilities, unix_time_ms()?)
}

async fn fetch_raw_models(
    client: &CodexAppServerClient,
    cancellation: CancellationToken,
) -> Result<Vec<crate::discovery::RawModel>, CodexAppServerError> {
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut raw_models = Vec::new();
    for _page in 0..MAX_MODEL_PAGES {
        let page = ModelPage::parse(
            client
                .request(
                    "model/list",
                    json!({
                        "cursor":cursor,
                        "includeHidden":false,
                        "limit":MODEL_PAGE_LIMIT,
                    }),
                    cancellation.clone(),
                )
                .await?,
        )?;
        raw_models.extend(page.models);
        if raw_models.len() > MAX_MODELS {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Protocol));
        }
        match page.next_cursor {
            None => return Ok(raw_models),
            Some(next) if seen_cursors.insert(next.clone()) => cursor = Some(next),
            Some(_) => {
                return Err(CodexAppServerError::new(CodexAppServerErrorCode::Protocol));
            }
        }
    }
    Err(CodexAppServerError::new(CodexAppServerErrorCode::Protocol))
}

async fn settle_client<T>(
    client: CodexAppServerClient,
    result: Result<T, CodexAppServerError>,
) -> Result<T, RuntimeError> {
    let close = client.close(CancellationToken::new()).await;
    match result {
        Err(error) => Err(runtime_error(error)),
        Ok(value) => match close {
            Ok(()) => Ok(value),
            Err(error) => Err(runtime_error(error)),
        },
    }
}

/// Classify a Codex failure and keep its own static, redacted message so the
/// user learns *which* component failed ("Codex CLI is unavailable"), not just
/// the class.
pub(crate) fn runtime_error(error: CodexAppServerError) -> RuntimeError {
    let code = match error.code() {
        CodexAppServerErrorCode::Cancelled => return RuntimeError::cancelled(),
        CodexAppServerErrorCode::Protocol => RuntimeErrorCode::Protocol,
        CodexAppServerErrorCode::Conflict => RuntimeErrorCode::Conflict,
        CodexAppServerErrorCode::Remote => RuntimeErrorCode::Unauthorized,
        CodexAppServerErrorCode::InvalidConfig => RuntimeErrorCode::InvalidRequest,
        CodexAppServerErrorCode::Unavailable
        | CodexAppServerErrorCode::UnsupportedVersion
        | CodexAppServerErrorCode::Process
        | CodexAppServerErrorCode::SandboxStartup
        | CodexAppServerErrorCode::Overloaded
        | CodexAppServerErrorCode::Closed => RuntimeErrorCode::Unavailable,
    };
    RuntimeError::try_new(code, error.to_string()).unwrap_or_else(|_| match code {
        RuntimeErrorCode::Protocol => RuntimeError::protocol(),
        RuntimeErrorCode::Conflict => RuntimeError::conflict(),
        RuntimeErrorCode::Unauthorized => RuntimeError::unauthorized(),
        RuntimeErrorCode::InvalidRequest => RuntimeError::invalid_request(),
        _ => RuntimeError::unavailable(),
    })
}

fn unix_time_ms() -> Result<u64, CodexAppServerError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CodexAppServerError::new(CodexAppServerErrorCode::Protocol))?
        .as_millis()
        .try_into()
        .map_err(|_| CodexAppServerError::new(CodexAppServerErrorCode::Protocol))
}

impl CodexRuntime {
    pub(crate) fn ensure_operation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else if self.shutdown.is_cancelled() {
            Err(RuntimeError::unavailable())
        } else {
            Ok(())
        }
    }
}

async fn await_version_output(
    subprocess: &SubprocessService,
    spec: heycode_exec::ProcessSpec,
    operation: CancellationToken,
    cancellation: CancellationToken,
) -> Result<ProcessOutput, CodexAppServerError> {
    let future = subprocess.output(spec, operation.clone());
    tokio::pin!(future);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            operation.cancel();
            let _settled = future.await;
            Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled))
        }
        result = &mut future => result.map_err(CodexAppServerError::from),
    }
}

fn validate_version_output(output: ProcessOutput) -> Result<CodexCliVersion, CodexAppServerError> {
    // The installed CLI may successfully print its version while reporting a
    // nonfatal startup warning on stderr (for example, PATH alias creation is
    // denied by the workspace sandbox). Version identity comes from strict
    // stdout parsing and the bound executable, not an empty diagnostic stream.
    // Keep both streams bounded and never retain or expose diagnostic bodies.
    if !matches!(output.exit(), ProcessExit::Exited { code: 0 }) || output.truncated() {
        return Err(CodexAppServerError::new(
            CodexAppServerErrorCode::UnsupportedVersion,
        ));
    }
    CodexCliVersion::parse_output(output.stdout())?.ensure_supported()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn restricted_initialization_errors_are_actionable_without_reclassifying_other_failures() {
        use heycode_exec::SandboxMode;
        for mode in [SandboxMode::WorkspaceWrite, SandboxMode::ReadOnly] {
            let error = classify_initialization_error(
                CodexAppServerError::new(CodexAppServerErrorCode::Protocol),
                mode,
            );
            assert_eq!(error.code(), CodexAppServerErrorCode::SandboxStartup);
            assert!(error.to_string().contains("--sandbox off"));
            assert_eq!(runtime_error(error).code(), RuntimeErrorCode::Unavailable);
            for code in [
                CodexAppServerErrorCode::Cancelled,
                CodexAppServerErrorCode::UnsupportedVersion,
                CodexAppServerErrorCode::Remote,
            ] {
                assert_eq!(
                    classify_initialization_error(CodexAppServerError::new(code), mode).code(),
                    code
                );
            }
        }
        assert_eq!(
            classify_initialization_error(
                CodexAppServerError::new(CodexAppServerErrorCode::Protocol),
                SandboxMode::Off
            )
            .code(),
            CodexAppServerErrorCode::Protocol
        );
    }

    #[test]
    fn runtime_errors_keep_the_codex_message_as_their_cause() {
        let mapped = runtime_error(CodexAppServerError::new(
            CodexAppServerErrorCode::Unavailable,
        ));
        assert_eq!(mapped.code(), RuntimeErrorCode::Unavailable);
        assert_eq!(mapped.message(), "Codex CLI is unavailable");
        let remote = runtime_error(CodexAppServerError::new(CodexAppServerErrorCode::Remote));
        assert_eq!(remote.code(), RuntimeErrorCode::Unauthorized);
        assert_eq!(remote.message(), "Codex app-server request failed");
        assert_eq!(
            runtime_error(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled)).code(),
            RuntimeErrorCode::Cancelled
        );
    }
}

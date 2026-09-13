//! Generic ACP v1 delegated runtime and OpenCode profile boundary.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fmt::{Debug, Formatter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::future::FutureExt as _;
use futures::lock::Mutex as AsyncMutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use heycode_core::{CallId, ProviderProtocol};
use heycode_llm::{CapabilitySupport, CatalogSnapshot, ModelDescriptor, ProviderDescriptor};

use crate::{
    AccountState, AccountStatus, AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeId,
    AgentRuntimeKind, RuntimeCapabilities, RuntimeCompactOutcome, RuntimeConfiguration,
    RuntimeConfigurationCapabilities, RuntimeContractError, RuntimeError, RuntimeErrorCode,
    RuntimeEventHub, RuntimeEventKind, RuntimeEventStream, RuntimeFinishReason, RuntimeFork,
    RuntimeInput, RuntimeModelConfiguration, RuntimePermissionDecision, RuntimePermissionResponse,
    RuntimeQuestionResponse, RuntimeRequestId, RuntimeResume, RuntimeSession, RuntimeSessionId,
    RuntimeStart, RuntimeTurnId,
};

/// Default maximum size of one ACP newline-delimited JSON frame.
pub const DEFAULT_MAX_ACP_FRAME_BYTES: usize = 4 * 1024 * 1024;

const ACP_PROTOCOL_VERSION: u64 = 1;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_NODES: usize = 65_536;
const MAX_CONFIG_OPTIONS: usize = 128;
const MAX_MODEL_OPTIONS: usize = 4_096;
const MAX_PERMISSION_OPTIONS: usize = 32;
const MAX_ACCUMULATED_TEXT_BYTES: usize = 1024 * 1024;

/// Strict ACP NDJSON framing failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AcpFrameError {
    /// Configured frame bound is outside the supported range.
    #[error("invalid ACP frame limit")]
    InvalidLimit,
    /// One frame exceeded its configured byte bound.
    #[error("ACP frame exceeds the supported limit")]
    FrameTooLarge,
    /// A complete frame was empty.
    #[error("ACP frame is empty")]
    EmptyFrame,
    /// A complete frame was not strict UTF-8.
    #[error("ACP frame is not UTF-8")]
    InvalidUtf8,
    /// A complete frame was not bounded JSON.
    #[error("ACP frame is not valid bounded JSON")]
    InvalidJson,
    /// EOF arrived with a partial frame.
    #[error("ACP stream ended with an incomplete frame")]
    IncompleteFrame,
}

/// Incremental strict UTF-8 newline-delimited ACP JSON decoder.
pub struct AcpFrameDecoder {
    bytes: Vec<u8>,
    maximum: usize,
    finished: bool,
}

impl AcpFrameDecoder {
    /// Construct a decoder with an explicit per-frame byte ceiling.
    ///
    /// # Errors
    /// Zero or a value above 64 MiB is rejected.
    pub fn new(maximum: usize) -> Result<Self, AcpFrameError> {
        if maximum == 0 || maximum > 64 * 1024 * 1024 {
            return Err(AcpFrameError::InvalidLimit);
        }
        Ok(Self {
            bytes: Vec::new(),
            maximum,
            finished: false,
        })
    }

    /// Consume one arbitrary raw process-output fragment.
    ///
    /// # Errors
    /// Oversized, empty, non-UTF-8, malformed or over-complex frames fail.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, AcpFrameError> {
        if self.finished {
            return Err(AcpFrameError::IncompleteFrame);
        }
        let mut frames = Vec::new();
        let mut start = 0;
        for (index, byte) in chunk.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            self.extend(&chunk[start..index])?;
            frames.push(self.take_frame()?);
            start = index.saturating_add(1);
        }
        self.extend(&chunk[start..])?;
        Ok(frames)
    }

    /// Seal the stream after explicit EOF.
    ///
    /// # Errors
    /// A non-empty unterminated tail is rejected.
    pub fn finish(&mut self) -> Result<(), AcpFrameError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(AcpFrameError::IncompleteFrame)
        }
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), AcpFrameError> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > self.maximum)
        {
            return Err(AcpFrameError::FrameTooLarge);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn take_frame(&mut self) -> Result<Value, AcpFrameError> {
        if self.bytes.last() == Some(&b'\r') {
            self.bytes.pop();
        }
        if self.bytes.is_empty() {
            return Err(AcpFrameError::EmptyFrame);
        }
        let text = std::str::from_utf8(&self.bytes).map_err(|_| AcpFrameError::InvalidUtf8)?;
        let value = serde_json::from_str::<Value>(text).map_err(|_| AcpFrameError::InvalidJson)?;
        validate_json_shape(&value)?;
        self.bytes.clear();
        Ok(value)
    }
}

impl Debug for AcpFrameDecoder {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpFrameDecoder")
            .field("buffered_bytes", &self.bytes.len())
            .field("maximum", &self.maximum)
            .field("finished", &self.finished)
            .finish()
    }
}

fn validate_json_shape(value: &Value) -> Result<(), AcpFrameError> {
    fn visit(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), AcpFrameError> {
        if depth > MAX_JSON_DEPTH {
            return Err(AcpFrameError::InvalidJson);
        }
        *nodes = nodes.checked_add(1).ok_or(AcpFrameError::InvalidJson)?;
        if *nodes > MAX_JSON_NODES {
            return Err(AcpFrameError::InvalidJson);
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, depth.saturating_add(1), nodes)?;
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    visit(value, depth.saturating_add(1), nodes)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
        Ok(())
    }

    let mut nodes = 0;
    visit(value, 0, &mut nodes)
}

/// One exact ACP subprocess launch request.
pub struct AcpProcessSpec {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

impl AcpProcessSpec {
    /// Validate an exact executable, argv, environment and working directory.
    ///
    /// # Errors
    /// Relative paths, NULs, duplicate environment names or excessive data fail.
    pub fn new(
        program: impl Into<PathBuf>,
        args: Vec<OsString>,
        cwd: impl Into<PathBuf>,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<Self, RuntimeContractError> {
        let program = program.into();
        let cwd = cwd.into();
        if !program.is_absolute()
            || !cwd.is_absolute()
            || path_has_nul(&program)
            || path_has_nul(&cwd)
        {
            return Err(RuntimeContractError::invalid(
                "ACP process path",
                "absolute NUL-free executable and working directory",
            ));
        }
        if args.len() > 128 || args.iter().any(|value| os_has_nul(value)) {
            return Err(RuntimeContractError::invalid(
                "ACP process arguments",
                "at most 128 NUL-free values",
            ));
        }
        let mut names = BTreeSet::new();
        if environment.len() > 128
            || environment.iter().any(|(name, value)| {
                name.is_empty()
                    || os_has_nul(name)
                    || os_has_nul(value)
                    || name.to_string_lossy().contains('=')
                    || !names.insert(name.clone())
            })
        {
            return Err(RuntimeContractError::invalid(
                "ACP process environment",
                "at most 128 unique NUL-free explicit entries",
            ));
        }
        let total = args
            .iter()
            .map(|value| os_len(value.as_os_str()))
            .chain(
                environment
                    .iter()
                    .flat_map(|(name, value)| [os_len(name), os_len(value)]),
            )
            .try_fold(0_usize, |total, length| total.checked_add(length))
            .ok_or_else(|| {
                RuntimeContractError::invalid("ACP process specification", "at most 1 MiB")
            })?;
        if total > 1024 * 1024 {
            return Err(RuntimeContractError::invalid(
                "ACP process specification",
                "at most 1 MiB",
            ));
        }
        Ok(Self {
            program,
            args,
            cwd,
            environment,
        })
    }

    /// Exact absolute executable.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Exact argv tail.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// Exact absolute working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Complete explicit child environment.
    #[must_use]
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }
}

impl Debug for AcpProcessSpec {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpProcessSpec")
            .field("program", &"<redacted>")
            .field("cwd", &"<redacted>")
            .field("argument_count", &self.args.len())
            .field("environment_count", &self.environment.len())
            .finish()
    }
}

fn path_has_nul(path: &Path) -> bool {
    os_has_nul(path.as_os_str())
}

fn os_has_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

fn os_len(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

/// Owned raw ACP subprocess connection.
///
/// Implementations serialize writes, make a cancelled read consume no bytes,
/// and kill/reap the process on drop as a final safety net. The runtime calls
/// [`Self::close`] for quiescent normal teardown.
#[async_trait]
pub trait AcpProcess: Send + Sync {
    /// Write one complete newline-terminated JSON frame.
    ///
    /// # Errors
    /// Cancellation, closed input or process failure returns a body-free error.
    async fn write(
        &self,
        frame: &[u8],
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError>;

    /// Read one non-empty raw stdout fragment, or `None` after EOF.
    ///
    /// Cancellation affects only this read and consumes no pending bytes.
    ///
    /// # Errors
    /// Cancellation or process/read failure returns a body-free error.
    async fn read(&self, cancellation: CancellationToken) -> Result<Option<Vec<u8>>, RuntimeError>;

    /// Stop accepting work and reap the complete owned process boundary.
    ///
    /// # Errors
    /// Teardown failure returns a body-free error.
    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError>;
}

/// Provider that resolves and launches one exact ACP process per connection.
#[async_trait]
pub trait AcpProcessFactory: Send + Sync {
    /// Spawn one owned ACP process connection.
    ///
    /// `lifecycle` owns the connection after a successful spawn; cancelling
    /// the operation-scoped `cancellation` token after this method returns
    /// must not stop the published process.
    ///
    /// # Errors
    /// Resolution, policy, cancellation or launch failure returns a body-free error.
    async fn spawn(
        &self,
        spec: AcpProcessSpec,
        lifecycle: CancellationToken,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn AcpProcess>, RuntimeError>;
}

/// Configuration for one generic ACP v1 runtime provider.
pub struct AcpRuntimeConfig {
    descriptor: AgentRuntimeDescriptor,
    program: PathBuf,
    args: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    catalog_workspace: PathBuf,
    account: AccountState,
    expected_agent: Option<(String, String)>,
    cached_authentication: Option<String>,
    discovery_workspace: Option<Arc<crate::RuntimeDiscoveryWorkspace>>,
}

impl AcpRuntimeConfig {
    /// Construct a generic ACP runtime configuration.
    ///
    /// # Errors
    /// The process template and catalog workspace must form a valid exact spec.
    pub fn new(
        descriptor: AgentRuntimeDescriptor,
        program: impl Into<PathBuf>,
        args: Vec<OsString>,
        environment: Vec<(OsString, OsString)>,
        catalog_workspace: impl Into<PathBuf>,
        account: AccountState,
    ) -> Result<Self, RuntimeContractError> {
        let program = program.into();
        let catalog_workspace = catalog_workspace.into();
        let _validated = AcpProcessSpec::new(
            program.clone(),
            args.clone(),
            catalog_workspace.clone(),
            environment.clone(),
        )?;
        Ok(Self {
            descriptor,
            program,
            args,
            environment,
            catalog_workspace,
            account,
            expected_agent: None,
            cached_authentication: None,
            discovery_workspace: None,
        })
    }

    /// Retain an empty effect-owned directory for account and catalog probes.
    #[must_use]
    pub fn with_discovery_workspace(
        mut self,
        workspace: Arc<crate::RuntimeDiscoveryWorkspace>,
    ) -> Self {
        self.catalog_workspace = workspace.path().to_path_buf();
        self.discovery_workspace = Some(workspace);
        self
    }

    /// Require an advertised cached-account method before every ACP operation.
    ///
    /// The peer owns all credentials. heycode sends only the method id and headless flag.
    ///
    /// # Errors
    /// Blank, oversized or control-bearing method ids fail.
    pub fn with_cached_authentication(
        mut self,
        method: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let method = method.into();
        validate_config_text(&method, "ACP authentication method")?;
        self.cached_authentication = Some(method);
        Ok(self)
    }

    /// Require the initialized ACP peer to report one exact agent identity.
    ///
    /// # Errors
    /// Name and version must be trimmed, control-free text up to 128 bytes.
    pub fn with_expected_agent_info(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let name = name.into();
        let version = version.into();
        validate_config_text(&name, "ACP agent name")?;
        validate_config_text(&version, "ACP agent version")?;
        self.expected_agent = Some((name, version));
        Ok(self)
    }

    fn process_spec(&self, cwd: &Path) -> Result<AcpProcessSpec, RuntimeError> {
        AcpProcessSpec::new(
            self.program.clone(),
            self.args.clone(),
            cwd,
            self.environment.clone(),
        )
        .map_err(|_| RuntimeError::invalid_request())
    }
}

impl Debug for AcpRuntimeConfig {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpRuntimeConfig")
            .field("runtime", &self.descriptor.id().as_str())
            .field("program", &"<redacted>")
            .field("catalog_workspace", &"<redacted>")
            .field("argument_count", &self.args.len())
            .field("environment_count", &self.environment.len())
            .finish()
    }
}

fn validate_config_text(value: &str, field: &'static str) -> Result<(), RuntimeContractError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 128
        || value.chars().any(char::is_control)
    {
        Err(RuntimeContractError::InvalidField {
            field,
            requirement: "1..=128 trimmed control-free bytes",
        })
    } else {
        Ok(())
    }
}

/// Generic ACP v1 delegated [`AgentRuntime`].
pub struct AcpRuntime {
    config: Arc<AcpRuntimeConfig>,
    factory: Arc<dyn AcpProcessFactory>,
    catalog: Arc<Mutex<Option<CatalogSnapshot>>>,
    catalog_revision: Arc<AtomicU64>,
    lifecycle: CancellationToken,
}

impl AcpRuntime {
    /// Bind a process Provider to one validated runtime configuration.
    #[must_use]
    pub fn new(config: AcpRuntimeConfig, factory: Arc<dyn AcpProcessFactory>) -> Self {
        Self {
            config: Arc::new(config),
            factory,
            catalog: Arc::new(Mutex::new(None)),
            catalog_revision: Arc::new(AtomicU64::new(0)),
            lifecycle: CancellationToken::new(),
        }
    }

    /// Stop every process connection owned by this runtime generation.
    ///
    /// Concrete plugins register this synchronous cancellation as a context
    /// effect. Individual session [`RuntimeSession::close`] calls remain the
    /// quiescent async teardown boundary.
    pub fn shutdown(&self) {
        self.lifecycle.cancel();
    }

    async fn spawn_peer(
        &self,
        workspace: &Path,
        cancellation: CancellationToken,
    ) -> Result<Arc<AcpPeer>, RuntimeError> {
        if self.lifecycle.is_cancelled() {
            return Err(RuntimeError::closed());
        }
        let spec = self.config.process_spec(workspace)?;
        let lifecycle = self.lifecycle.child_token();
        let process = self
            .factory
            .spawn(spec, lifecycle, cancellation.clone())
            .await?;
        let peer = Arc::new(AcpPeer::new(process)?);
        let expected_agent = self
            .config
            .expected_agent
            .as_ref()
            .map(|(name, version)| (name.as_str(), version.as_str()));
        let initialized = peer
            .initialize(
                cancellation,
                expected_agent,
                self.config.cached_authentication.as_deref(),
            )
            .await;
        match initialized {
            Ok(negotiated) => {
                peer.set_negotiated(negotiated);
                Ok(peer)
            }
            Err(error) => {
                let _closed = peer.close(CancellationToken::new()).await;
                Err(error)
            }
        }
    }

    fn catalog_from_options(
        &self,
        options: &AcpConfigOptions,
    ) -> Result<CatalogSnapshot, RuntimeError> {
        let Some(model) = &options.model else {
            return Err(RuntimeError::protocol());
        };
        let revision = self
            .catalog_revision
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::internal("catalog revision exhausted"))?
            .saturating_add(1);
        let fetched_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeError::internal("clock unavailable"))?
            .as_millis()
            .try_into()
            .map_err(|_| RuntimeError::internal("clock overflow"))?;
        let mut models = model
            .options
            .iter()
            .map(|option| {
                let mut descriptor = ModelDescriptor::unknown(option.value.clone());
                descriptor.display_name = option.name.clone();
                descriptor
            })
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(CatalogSnapshot {
            provider: ProviderDescriptor {
                id: self.config.descriptor.id().as_str().to_owned(),
                display_name: self.config.descriptor.display_name().to_owned(),
                protocols: vec![ProviderProtocol::DelegatedAgent],
            },
            models,
            revision,
            fetched_at_ms,
        })
    }

    fn publish_catalog(&self, options: &AcpConfigOptions) -> Result<CatalogSnapshot, RuntimeError> {
        let catalog = self.catalog_from_options(options)?;
        *self
            .catalog
            .lock()
            .map_err(|_| RuntimeError::internal("catalog unavailable"))? = Some(catalog.clone());
        Ok(catalog)
    }

    async fn probe_catalog(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, RuntimeError> {
        let peer = self
            .spawn_peer(&self.config.catalog_workspace, cancellation.clone())
            .await?;
        let result = async {
            let response = peer
                .request(
                    "session/new",
                    session_open_params(&self.config.catalog_workspace, None)?,
                    cancellation.clone(),
                    pre_session_message,
                )
                .await?;
            let opened = parse_opened_session(&response, None)?;
            let catalog = self.publish_catalog(&opened.options)?;
            if peer.negotiated().close {
                let _closed = peer
                    .request(
                        "session/close",
                        serde_json::json!({"sessionId":opened.session_id.as_str()}),
                        CancellationToken::new(),
                        pre_session_message,
                    )
                    .await?;
            }
            Ok(catalog)
        }
        .await;
        let closed = peer.close(CancellationToken::new()).await;
        match (result, closed) {
            (Ok(catalog), Ok(())) => Ok(catalog),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn start_session(
        &self,
        workspace: &Path,
        method: &'static str,
        params: Value,
        expected_id: Option<&RuntimeSessionId>,
        configuration: RuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        validate_acp_configuration(&configuration)?;
        let peer = self.spawn_peer(workspace, cancellation.clone()).await?;
        let result = async {
            if method == "session/load"
                && self.config.descriptor.capabilities().resume != CapabilitySupport::Supported
            {
                return Err(RuntimeError::unsupported());
            }
            if method == "session/load" && !peer.negotiated().can_resume() {
                return Err(RuntimeError::protocol());
            }
            let response = peer
                .request(method, params, cancellation.clone(), pre_session_message)
                .await?;
            let opened = parse_opened_session(&response, expected_id)?;
            let mut options = opened.options;
            if self.config.descriptor.capabilities().models == CapabilitySupport::Supported {
                self.publish_catalog(&options)?;
            }
            for (kind, value) in [
                (AcpSelectionKind::Model, configuration.model()),
                (
                    AcpSelectionKind::Effort,
                    configuration.reasoning_effort(),
                ),
            ] {
                let Some(value) = value else {
                    continue;
                };
                // Model selection can replace the authoritative option list,
                // including the id and values of a dependent effort control.
                // Resolve each selection immediately before sending it.
                let selection = options.select(kind, value)?;
                let config_option = matches!(selection, AcpModelControl::ConfigOption(_));
                let (method, params) = match &selection {
                    AcpModelControl::ConfigOption(config_id) => ("session/set_config_option", serde_json::json!({
                        "sessionId":opened.session_id.as_str(), "configId":config_id, "value":value,
                    })),
                    AcpModelControl::Legacy => ("session/set_model", serde_json::json!({
                        "sessionId":opened.session_id.as_str(), "modelId":value,
                    })),
                };
                let response = peer
                    .request(
                        method,
                        params,
                        cancellation.clone(),
                        pre_session_message,
                    )
                    .await?;
                if config_option {
                    let parsed = parse_required_config_options(response.get("configOptions"))?;
                    ensure_config_selection_applied(&parsed, kind, &selection, value)?;
                    options = parsed;
                    if self.config.descriptor.capabilities().models == CapabilitySupport::Supported
                        && options.model.is_some()
                    {
                        self.publish_catalog(&options)?;
                    }
                }
            }
            let session = AcpRuntimeSession::new(AcpRuntimeSessionInit {
                peer: Arc::clone(&peer),
                id: opened.session_id,
                runtime_id: self.config.descriptor.id().clone(),
                capabilities: self.config.descriptor.capabilities().clone(),
                catalog: Arc::clone(&self.catalog),
                config: Arc::clone(&self.config),
                catalog_revision: Arc::clone(&self.catalog_revision),
                options,
                configuration,
            })?;
            Ok(Arc::new(session) as Arc<dyn RuntimeSession>)
        }
        .await;
        if result.is_err() {
            let _closed = peer.close(CancellationToken::new()).await;
        }
        result
    }
}

impl Debug for AcpRuntime {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpRuntime")
            .field("runtime", &self.config.descriptor.id().as_str())
            .field(
                "catalog_available",
                &self
                    .catalog
                    .lock()
                    .map(|value| value.is_some())
                    .unwrap_or(false),
            )
            .finish_non_exhaustive()
    }
}

/// Construct the current documented OpenCode `opencode acp` runtime profile.
///
/// The executable must already be resolved to an absolute path. The process
/// Provider revalidates it at launch. Model rows come from the session-scoped
/// `configOptions` catalog; capability evidence is limited to implemented ACP
/// operations and the current OpenCode ACP contract.
///
/// # Errors
/// Invalid process/configuration metadata.
pub fn opencode_acp_runtime(
    factory: Arc<dyn AcpProcessFactory>,
    executable: &Path,
    catalog_workspace: &Path,
    environment: Vec<(OsString, OsString)>,
) -> Result<AcpRuntime, RuntimeContractError> {
    let config = opencode_acp_config(executable, catalog_workspace, environment)?;
    Ok(AcpRuntime::new(config, factory))
}

/// Construct the OpenCode ACP profile with an exact initialized-agent version.
///
/// # Errors
/// Invalid process metadata or expected version text.
pub fn opencode_acp_runtime_pinned(
    factory: Arc<dyn AcpProcessFactory>,
    executable: &Path,
    catalog_workspace: &Path,
    environment: Vec<(OsString, OsString)>,
    expected_version: &str,
) -> Result<AcpRuntime, RuntimeContractError> {
    let config = opencode_acp_config(executable, catalog_workspace, environment)?
        .with_expected_agent_info("OpenCode", expected_version)?;
    Ok(AcpRuntime::new(config, factory))
}

fn opencode_acp_config(
    executable: &Path,
    catalog_workspace: &Path,
    environment: Vec<(OsString, OsString)>,
) -> Result<AcpRuntimeConfig, RuntimeContractError> {
    let descriptor = opencode_acp_descriptor()?;
    AcpRuntimeConfig::new(
        descriptor,
        executable,
        vec![OsString::from("acp")],
        environment,
        catalog_workspace,
        AccountState::without_label(AccountStatus::Unknown),
    )
}

/// Construct the immutable capability row shared by available and missing
/// optional OpenCode registrations.
///
/// # Errors
/// The compile-time descriptor constants must satisfy runtime validation.
pub fn opencode_acp_descriptor() -> Result<AgentRuntimeDescriptor, RuntimeContractError> {
    let capabilities = RuntimeCapabilities {
        models: CapabilitySupport::Supported,
        resume: CapabilitySupport::Supported,
        fork: CapabilitySupport::Unsupported,
        steer: CapabilitySupport::Unsupported,
        follow_up: CapabilitySupport::Unsupported,
        permissions: CapabilitySupport::Supported,
        questions: CapabilitySupport::Unsupported,
        compaction: CapabilitySupport::Unsupported,
    };
    AgentRuntimeDescriptor::new(
        "opencode",
        "OpenCode",
        AgentRuntimeKind::Delegated,
        capabilities,
    )
    .map(|descriptor| {
        descriptor.with_configuration_capabilities(RuntimeConfigurationCapabilities {
            system_prompt: CapabilitySupport::Unsupported,
            tools: CapabilitySupport::Unsupported,
            model: CapabilitySupport::Supported,
            reasoning_effort: CapabilitySupport::Supported,
        })
    })
}

#[async_trait]
impl AgentRuntime for AcpRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.config.descriptor
    }

    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if self.config.cached_authentication.is_some() {
            let peer = match self
                .spawn_peer(&self.config.catalog_workspace, cancellation.clone())
                .await
            {
                Ok(peer) => peer,
                Err(error) if error.code() == RuntimeErrorCode::Unauthorized => {
                    return Ok(AccountState::without_label(AccountStatus::Disconnected));
                }
                Err(error) => return Err(error),
            };
            peer.close(cancellation).await?;
            return Ok(AccountState::without_label(AccountStatus::Connected));
        }
        Ok(self.config.account.clone())
    }

    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        require_supported(self.config.descriptor.capabilities().models)?;
        self.probe_catalog(cancellation).await
    }

    async fn model_configurations(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<RuntimeModelConfiguration>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let peer = self
            .spawn_peer(&self.config.catalog_workspace, cancellation.clone())
            .await?;
        let result = async {
            let response = peer
                .request(
                    "session/new",
                    session_open_params(&self.config.catalog_workspace, None)?,
                    cancellation.clone(),
                    pre_session_message,
                )
                .await?;
            let opened = parse_opened_session(&response, None)?;
            let mut options = opened.options;
            let advertised = options
                .model
                .as_ref()
                .map(|model| {
                    model
                        .options
                        .iter()
                        .map(|row| row.value.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut models = Vec::with_capacity(advertised.len());
            for model in advertised {
                if options.model.as_ref().map(|row| row.current.as_str()) != Some(model.as_str()) {
                    let selection = options.select_model(&model)?;
                    let config_option = matches!(selection, AcpModelControl::ConfigOption(_));
                    let (method, params) = match &selection {
                        AcpModelControl::ConfigOption(config_id) => (
                            "session/set_config_option",
                            serde_json::json!({
                                "sessionId":opened.session_id.as_str(),
                                "configId":config_id,
                                "value":model,
                            }),
                        ),
                        AcpModelControl::Legacy => (
                            "session/set_model",
                            serde_json::json!({
                                "sessionId":opened.session_id.as_str(),
                                "modelId":model,
                            }),
                        ),
                    };
                    let response = peer
                        .request(method, params, cancellation.clone(), pre_session_message)
                        .await?;
                    if config_option {
                        let parsed = parse_required_config_options(response.get("configOptions"))?;
                        ensure_config_selection_applied(
                            &parsed,
                            AcpSelectionKind::Model,
                            &selection,
                            &model,
                        )?;
                        options = parsed;
                    }
                }
                let effort = options.effort.as_ref();
                let display_name = options
                    .model
                    .as_ref()
                    .and_then(|configuration| {
                        configuration
                            .options
                            .iter()
                            .find(|option| option.value == model)
                    })
                    .map_or_else(|| model.clone(), |option| option.name.clone());
                models.push(RuntimeModelConfiguration {
                    model,
                    display_name,
                    resolved_model: None,
                    description: None,
                    context_window: None,
                    default_reasoning_effort: effort.map(|row| row.current.clone()),
                    reasoning_efforts: effort.map_or_else(Vec::new, |row| {
                        row.options
                            .iter()
                            .map(|option| option.value.clone())
                            .collect()
                    }),
                });
            }
            if peer.negotiated().close {
                peer.request(
                    "session/close",
                    serde_json::json!({"sessionId":opened.session_id.as_str()}),
                    CancellationToken::new(),
                    pre_session_message,
                )
                .await?;
            }
            Ok(models)
        }
        .await;
        let closed = peer.close(CancellationToken::new()).await;
        match (result, closed) {
            (Ok(models), Ok(())) => Ok(models),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn start(
        &self,
        request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        validate_acp_configuration(request.configuration())?;
        let params = session_open_params(request.workspace(), None)?;
        self.start_session(
            request.workspace(),
            "session/new",
            params,
            None,
            request.configuration().clone(),
            cancellation,
        )
        .await
    }

    async fn resume(
        &self,
        request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        require_supported(self.config.descriptor.capabilities().resume)?;
        let params = session_open_params(request.workspace(), Some(request.runtime_session_id()))?;
        self.start_session(
            request.workspace(),
            "session/load",
            params,
            Some(request.runtime_session_id()),
            request.configuration().clone(),
            cancellation,
        )
        .await
    }

    async fn fork(
        &self,
        _request: RuntimeFork,
        _cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        Err(RuntimeError::unsupported())
    }
}

fn require_supported(support: CapabilitySupport) -> Result<(), RuntimeError> {
    match support {
        CapabilitySupport::Supported => Ok(()),
        CapabilitySupport::Unsupported => Err(RuntimeError::unsupported()),
        CapabilitySupport::Unknown => Err(RuntimeError::unavailable()),
    }
}

fn validate_acp_configuration(configuration: &RuntimeConfiguration) -> Result<(), RuntimeError> {
    let mut unsupported = Vec::new();
    if configuration.system_prompt().is_some() {
        unsupported.push("system_prompt");
    }
    if configuration.tools_configured() {
        unsupported.push("tools");
    }
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError::unsupported_field_names(&unsupported))
    }
}

fn session_open_params(
    workspace: &Path,
    session: Option<&RuntimeSessionId>,
) -> Result<Value, RuntimeError> {
    let cwd = workspace
        .to_str()
        .ok_or_else(RuntimeError::invalid_request)?;
    let mut params = serde_json::json!({"cwd":cwd,"mcpServers":[]});
    if let Some(session) = session {
        params["sessionId"] = Value::String(session.as_str().to_owned());
    }
    Ok(params)
}

#[derive(Debug, Clone, Copy)]
struct NegotiatedCapabilities {
    load_session: bool,
    close: bool,
    resume: bool,
}

impl NegotiatedCapabilities {
    const fn can_resume(self) -> bool {
        self.load_session || self.resume
    }
}

struct ReaderState {
    decoder: AcpFrameDecoder,
    ready: VecDeque<Value>,
    eof: bool,
}

struct AcpPeer {
    process: Arc<dyn AcpProcess>,
    reader: AsyncMutex<ReaderState>,
    next_id: AtomicU64,
    negotiated: Mutex<Option<NegotiatedCapabilities>>,
    closed: AtomicBool,
}

impl AcpPeer {
    fn new(process: Arc<dyn AcpProcess>) -> Result<Self, RuntimeError> {
        Ok(Self {
            process,
            reader: AsyncMutex::new(ReaderState {
                decoder: AcpFrameDecoder::new(DEFAULT_MAX_ACP_FRAME_BYTES)
                    .map_err(|_| RuntimeError::internal("invalid frame limit"))?,
                ready: VecDeque::new(),
                eof: false,
            }),
            next_id: AtomicU64::new(1),
            negotiated: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    async fn initialize(
        &self,
        cancellation: CancellationToken,
        expected_agent: Option<(&str, &str)>,
        cached_authentication: Option<&str>,
    ) -> Result<NegotiatedCapabilities, RuntimeError> {
        let result = self
            .request(
                "initialize",
                serde_json::json!({
                    "protocolVersion":ACP_PROTOCOL_VERSION,
                    "clientCapabilities":{},
                    "clientInfo":{"name":"heycode","version":env!("CARGO_PKG_VERSION")},
                }),
                cancellation.clone(),
                pre_session_message,
            )
            .await?;
        let negotiated = parse_initialize(&result, expected_agent)?;
        if let Some(method) = cached_authentication {
            let advertised = result
                .get("authMethods")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row.get("id").and_then(Value::as_str) == Some(method))
                });
            if !advertised {
                return Err(RuntimeError::unauthorized());
            }
            let authenticated = self
                .request(
                    "authenticate",
                    serde_json::json!({
                        "methodId": method, "_meta": {"headless": true},
                    }),
                    cancellation,
                    pre_session_message,
                )
                .await?;
            if !authenticated.is_object() {
                return Err(RuntimeError::protocol());
            }
        }
        Ok(negotiated)
    }

    fn set_negotiated(&self, capabilities: NegotiatedCapabilities) {
        if let Ok(mut negotiated) = self.negotiated.lock() {
            *negotiated = Some(capabilities);
        }
    }

    fn negotiated(&self) -> NegotiatedCapabilities {
        self.negotiated
            .lock()
            .ok()
            .and_then(|value| *value)
            .unwrap_or(NegotiatedCapabilities {
                load_session: false,
                close: false,
                resume: false,
            })
    }

    async fn request<F>(
        &self,
        method: &'static str,
        params: Value,
        cancellation: CancellationToken,
        mut inbound: F,
    ) -> Result<Value, RuntimeError>
    where
        F: FnMut(&Value) -> Result<(), RuntimeError>,
    {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let id = self
            .next_id
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::internal("ACP request ids exhausted"))?;
        self.write_value(
            &serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            cancellation.clone(),
        )
        .await?;
        loop {
            let message = self
                .read_value(cancellation.clone())
                .await?
                .ok_or_else(RuntimeError::protocol)?;
            if message.get("method").is_some() {
                inbound(&message)?;
                continue;
            }
            let response_id = message.get("id").and_then(Value::as_u64);
            if response_id != Some(id) {
                return Err(RuntimeError::protocol());
            }
            return parse_response_result(&message);
        }
    }

    async fn notify(
        &self,
        method: &'static str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.write_value(
            &serde_json::json!({"jsonrpc":"2.0","method":method,"params":params}),
            cancellation,
        )
        .await
    }

    async fn respond(
        &self,
        id: Value,
        result: Value,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.write_value(
            &serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
            cancellation,
        )
        .await
    }

    async fn respond_method_not_found(
        &self,
        id: Value,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.write_value(
            &serde_json::json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code":-32601,"message":"client method is unsupported"}
            }),
            cancellation,
        )
        .await
    }

    async fn write_value(
        &self,
        value: &Value,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RuntimeError::closed());
        }
        validate_json_shape(value).map_err(|_| RuntimeError::invalid_request())?;
        let mut frame = serde_json::to_vec(value).map_err(|_| RuntimeError::invalid_request())?;
        if frame.len() > DEFAULT_MAX_ACP_FRAME_BYTES {
            return Err(RuntimeError::invalid_request());
        }
        frame.push(b'\n');
        self.process.write(&frame, cancellation).await
    }

    async fn read_value(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Option<Value>, RuntimeError> {
        let mut reader = self.reader.lock().await;
        loop {
            if let Some(value) = reader.ready.pop_front() {
                validate_envelope(&value)?;
                return Ok(Some(value));
            }
            if reader.eof {
                return Ok(None);
            }
            match self.process.read(cancellation.clone()).await? {
                Some(bytes) if !bytes.is_empty() => {
                    let frames = reader
                        .decoder
                        .push(&bytes)
                        .map_err(|_| RuntimeError::protocol())?;
                    reader.ready.extend(frames);
                }
                Some(_) => return Err(RuntimeError::protocol()),
                None => {
                    reader
                        .decoder
                        .finish()
                        .map_err(|_| RuntimeError::protocol())?;
                    reader.eof = true;
                }
            }
        }
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.process.close(cancellation).await
    }
}

fn validate_envelope(value: &Value) -> Result<(), RuntimeError> {
    let object = value.as_object().ok_or_else(RuntimeError::protocol)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RuntimeError::protocol());
    }
    let method = object.get("method").and_then(Value::as_str);
    let result = object.contains_key("result");
    let error = object.contains_key("error");
    if method.is_some() {
        if result || error {
            return Err(RuntimeError::protocol());
        }
    } else if result == error || !object.contains_key("id") {
        return Err(RuntimeError::protocol());
    }
    Ok(())
}

fn parse_response_result(message: &Value) -> Result<Value, RuntimeError> {
    if let Some(result) = message.get("result") {
        return Ok(result.clone());
    }
    let code = message.pointer("/error/code").and_then(Value::as_i64);
    Err(match code {
        Some(-32800) => RuntimeError::cancelled(),
        Some(-32601) => RuntimeError::unsupported(),
        Some(-32602) => RuntimeError::invalid_request(),
        _ => RuntimeError::unavailable(),
    })
}

fn parse_initialize(
    result: &Value,
    expected_agent: Option<(&str, &str)>,
) -> Result<NegotiatedCapabilities, RuntimeError> {
    if result.get("protocolVersion").and_then(Value::as_u64) != Some(ACP_PROTOCOL_VERSION) {
        return Err(RuntimeError::protocol());
    }
    let capabilities = result
        .get("agentCapabilities")
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    let load_session = optional_bool(capabilities.get("loadSession"))?;
    let session = capabilities
        .get("sessionCapabilities")
        .and_then(Value::as_object);
    let close = session.is_some_and(|values| values.contains_key("close"));
    let resume = session.is_some_and(|values| values.contains_key("resume"));
    if !result.get("authMethods").is_none_or(Value::is_array) {
        return Err(RuntimeError::protocol());
    }
    if let Some(info) = result.get("agentInfo") {
        let object = info.as_object().ok_or_else(RuntimeError::protocol)?;
        validate_wire_text(object.get("name"), 128)?;
        validate_wire_text(object.get("version"), 128)?;
        if let Some((expected_name, expected_version)) = expected_agent
            && (object.get("name").and_then(Value::as_str) != Some(expected_name)
                || object.get("version").and_then(Value::as_str) != Some(expected_version))
        {
            return Err(RuntimeError::protocol());
        }
    } else if expected_agent.is_some() {
        return Err(RuntimeError::protocol());
    }
    Ok(NegotiatedCapabilities {
        load_session,
        close,
        resume,
    })
}

fn optional_bool(value: Option<&Value>) -> Result<bool, RuntimeError> {
    match value {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(RuntimeError::protocol()),
    }
}

fn validate_wire_text(value: Option<&Value>, maximum: usize) -> Result<(), RuntimeError> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    if value.is_empty()
        || value.trim() != value
        || value.len() > maximum
        || value.chars().any(|character| character.is_control())
    {
        return Err(RuntimeError::protocol());
    }
    Ok(())
}

fn pre_session_message(message: &Value) -> Result<(), RuntimeError> {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    if message.get("id").is_some() || (method != "session/update" && !method.starts_with('_')) {
        return Err(RuntimeError::protocol());
    }
    Ok(())
}

struct OpenedSession {
    session_id: RuntimeSessionId,
    options: AcpConfigOptions,
}

fn parse_opened_session(
    response: &Value,
    expected: Option<&RuntimeSessionId>,
) -> Result<OpenedSession, RuntimeError> {
    let session_id = match response.get("sessionId").and_then(Value::as_str) {
        Some(value) => RuntimeSessionId::new(value).map_err(|_| RuntimeError::protocol())?,
        None => expected.cloned().ok_or_else(RuntimeError::protocol)?,
    };
    if expected.is_some_and(|expected| expected != &session_id) {
        return Err(RuntimeError::protocol());
    }
    Ok(OpenedSession {
        session_id,
        options: match response.get("configOptions") {
            Some(options) => parse_config_options(Some(options))?,
            None => parse_legacy_models(response.get("models"))?,
        },
    })
}

#[derive(Clone)]
struct AcpSelectOption {
    value: String,
    name: String,
}

#[derive(Clone)]
struct AcpSelectConfig {
    control: AcpModelControl,
    current: String,
    options: Vec<AcpSelectOption>,
}

#[derive(Clone, Default)]
struct AcpConfigOptions {
    model: Option<AcpSelectConfig>,
    effort: Option<AcpSelectConfig>,
}

#[derive(Clone, PartialEq, Eq)]
enum AcpModelControl {
    ConfigOption(String),
    Legacy,
}

#[derive(Clone, Copy)]
enum AcpSelectionKind {
    Model,
    Effort,
}

impl AcpConfigOptions {
    fn select_model(&self, requested: &str) -> Result<AcpModelControl, RuntimeError> {
        let model = self.model.as_ref().ok_or_else(RuntimeError::protocol)?;
        if !model.options.iter().any(|option| option.value == requested) {
            return Err(RuntimeError::invalid_request());
        }
        Ok(model.control.clone())
    }

    fn select_effort(&self, requested: &str) -> Result<AcpModelControl, RuntimeError> {
        let effort = self.effort.as_ref().ok_or_else(RuntimeError::protocol)?;
        if !effort
            .options
            .iter()
            .any(|option| option.value == requested)
        {
            return Err(RuntimeError::invalid_request());
        }
        Ok(effort.control.clone())
    }

    fn select(
        &self,
        kind: AcpSelectionKind,
        requested: &str,
    ) -> Result<AcpModelControl, RuntimeError> {
        match kind {
            AcpSelectionKind::Model => self.select_model(requested),
            AcpSelectionKind::Effort => self.select_effort(requested),
        }
    }

    fn selected(&self, kind: AcpSelectionKind) -> Option<&AcpSelectConfig> {
        match kind {
            AcpSelectionKind::Model => self.model.as_ref(),
            AcpSelectionKind::Effort => self.effort.as_ref(),
        }
    }
}

fn parse_legacy_models(value: Option<&Value>) -> Result<AcpConfigOptions, RuntimeError> {
    let Some(value) = value else {
        return Ok(AcpConfigOptions::default());
    };
    let current = bounded_text(value.get("currentModelId"), 256)?;
    let rows = value
        .get("availableModels")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty() && rows.len() <= MAX_MODEL_OPTIONS)
        .ok_or_else(RuntimeError::protocol)?;
    let mut seen = BTreeSet::new();
    let mut options = Vec::with_capacity(rows.len());
    for row in rows {
        let id = bounded_text(row.get("modelId"), 256)?;
        if !seen.insert(id.clone()) {
            return Err(RuntimeError::protocol());
        }
        options.push(AcpSelectOption {
            value: id,
            name: bounded_text(row.get("name"), 256)?,
        });
    }
    if !seen.contains(&current) {
        return Err(RuntimeError::protocol());
    }
    Ok(AcpConfigOptions {
        model: Some(AcpSelectConfig {
            control: AcpModelControl::Legacy,
            current,
            options,
        }),
        effort: None,
    })
}

fn parse_config_options(value: Option<&Value>) -> Result<AcpConfigOptions, RuntimeError> {
    let Some(value) = value else {
        return Ok(AcpConfigOptions::default());
    };
    let rows = value.as_array().ok_or_else(RuntimeError::protocol)?;
    if rows.len() > MAX_CONFIG_OPTIONS {
        return Err(RuntimeError::protocol());
    }
    let mut model = None;
    let mut effort = None;
    for row in rows {
        let object = row.as_object().ok_or_else(RuntimeError::protocol)?;
        let id = bounded_text(object.get("id"), 128)?;
        let Some(category) = object.get("category") else {
            continue;
        };
        let category = bounded_text(Some(category), 64)?;
        let destination = match category.as_str() {
            "model" => &mut model,
            "thought_level" => &mut effort,
            _ => continue,
        };
        let kind = bounded_text(object.get("type"), 64)?;
        if kind != "select" {
            continue;
        }
        if destination.is_some() {
            return Err(RuntimeError::protocol());
        }
        let current = bounded_text(object.get("currentValue"), 256)?;
        let values = object
            .get("options")
            .and_then(Value::as_array)
            .ok_or_else(RuntimeError::protocol)?;
        if values.is_empty() || values.len() > MAX_MODEL_OPTIONS {
            return Err(RuntimeError::protocol());
        }
        let mut seen = BTreeSet::new();
        let mut options = Vec::with_capacity(values.len());
        for value in values {
            let value = value.as_object().ok_or_else(RuntimeError::protocol)?;
            let option = AcpSelectOption {
                value: bounded_text(value.get("value"), 256)?,
                name: bounded_text(value.get("name"), 256)?,
            };
            if !seen.insert(option.value.clone()) {
                return Err(RuntimeError::protocol());
            }
            options.push(option);
        }
        if !seen.contains(&current) {
            return Err(RuntimeError::protocol());
        }
        *destination = Some(AcpSelectConfig {
            control: AcpModelControl::ConfigOption(id),
            current,
            options,
        });
    }
    Ok(AcpConfigOptions { model, effort })
}

fn parse_required_config_options(value: Option<&Value>) -> Result<AcpConfigOptions, RuntimeError> {
    let value = value.ok_or_else(RuntimeError::protocol)?;
    parse_config_options(Some(value))
}

fn ensure_config_selection_applied(
    options: &AcpConfigOptions,
    kind: AcpSelectionKind,
    control: &AcpModelControl,
    requested: &str,
) -> Result<(), RuntimeError> {
    let selected = options.selected(kind).ok_or_else(RuntimeError::protocol)?;
    if &selected.control == control && selected.current == requested {
        Ok(())
    } else {
        Err(RuntimeError::protocol())
    }
}

fn bounded_text(value: Option<&Value>, maximum: usize) -> Result<String, RuntimeError> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    if value.is_empty()
        || value.trim() != value
        || value.len() > maximum
        || value.chars().any(char::is_control)
    {
        return Err(RuntimeError::protocol());
    }
    Ok(value.to_owned())
}

struct PendingPermission {
    wire_id: Value,
    allow_once: Option<String>,
    allow_session: Option<String>,
    deny: Option<String>,
}

struct ActiveOperation {
    cancellation: CancellationToken,
    cancel_sent: AtomicBool,
}

struct AcpRuntimeSessionInit {
    peer: Arc<AcpPeer>,
    id: RuntimeSessionId,
    runtime_id: AgentRuntimeId,
    capabilities: RuntimeCapabilities,
    catalog: Arc<Mutex<Option<CatalogSnapshot>>>,
    config: Arc<AcpRuntimeConfig>,
    catalog_revision: Arc<AtomicU64>,
    options: AcpConfigOptions,
    configuration: RuntimeConfiguration,
}

struct AcpRuntimeSession {
    peer: Arc<AcpPeer>,
    id: RuntimeSessionId,
    runtime_id: AgentRuntimeId,
    capabilities: RuntimeCapabilities,
    hub: RuntimeEventHub,
    operation_gate: AsyncMutex<()>,
    active: Mutex<Option<Arc<ActiveOperation>>>,
    permissions: Mutex<BTreeMap<RuntimeRequestId, PendingPermission>>,
    turn_counter: AtomicU64,
    lifecycle: CancellationToken,
    closed: AtomicBool,
    catalog: Arc<Mutex<Option<CatalogSnapshot>>>,
    config: Arc<AcpRuntimeConfig>,
    catalog_revision: Arc<AtomicU64>,
    options: Mutex<AcpConfigOptions>,
    configuration: Mutex<RuntimeConfiguration>,
}

impl AcpRuntimeSession {
    fn new(init: AcpRuntimeSessionInit) -> Result<Self, RuntimeError> {
        let session = Self {
            peer: init.peer,
            id: init.id,
            runtime_id: init.runtime_id,
            capabilities: init.capabilities,
            hub: RuntimeEventHub::new(),
            operation_gate: AsyncMutex::new(()),
            active: Mutex::new(None),
            permissions: Mutex::new(BTreeMap::new()),
            turn_counter: AtomicU64::new(1),
            lifecycle: CancellationToken::new(),
            closed: AtomicBool::new(false),
            catalog: init.catalog,
            config: init.config,
            catalog_revision: init.catalog_revision,
            options: Mutex::new(init.options),
            configuration: Mutex::new(init.configuration),
        };
        session.publish(RuntimeEventKind::SessionReady)?;
        Ok(session)
    }

    fn publish(&self, kind: RuntimeEventKind) -> Result<(), RuntimeError> {
        self.hub.emit(kind)
    }

    fn active_operation(&self) -> Result<Option<Arc<ActiveOperation>>, RuntimeError> {
        self.active
            .lock()
            .map(|active| active.clone())
            .map_err(|_| RuntimeError::internal("operation state unavailable"))
    }

    async fn terminate(&self, error: RuntimeError) -> RuntimeError {
        self.closed.store(true, Ordering::SeqCst);
        self.lifecycle.cancel();
        self.hub.fail(error.clone());
        let _closed = self.peer.close(CancellationToken::new()).await;
        error
    }

    async fn request_cancel(&self, active: &Arc<ActiveOperation>) -> Result<(), RuntimeError> {
        active.cancellation.cancel();
        self.cancel_permissions().await?;
        if active.cancel_sent.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.peer
            .notify(
                "session/cancel",
                serde_json::json!({"sessionId":self.id.as_str()}),
                CancellationToken::new(),
            )
            .await
    }

    async fn cancel_permissions(&self) -> Result<(), RuntimeError> {
        let pending = {
            let mut permissions = self
                .permissions
                .lock()
                .map_err(|_| RuntimeError::internal("permission state unavailable"))?;
            std::mem::take(&mut *permissions)
                .into_iter()
                .collect::<Vec<_>>()
        };
        let mut failure = None;
        for (_, permission) in pending {
            if let Err(error) = self
                .peer
                .respond(
                    permission.wire_id,
                    serde_json::json!({"outcome":{"outcome":"cancelled"}}),
                    CancellationToken::new(),
                )
                .await
            {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn register_permission(&self, message: &Value) -> Result<(), RuntimeError> {
        let wire_id = message
            .get("id")
            .cloned()
            .ok_or_else(RuntimeError::protocol)?;
        let request_id = runtime_request_id(&wire_id)?;
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        if params.get("sessionId").and_then(Value::as_str) != Some(self.id.as_str()) {
            return Err(RuntimeError::protocol());
        }
        let tool = params
            .get("toolCall")
            .and_then(Value::as_object)
            .ok_or_else(RuntimeError::protocol)?;
        let action = tool
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("ACP tool request");
        if action.is_empty()
            || action.len() > 256
            || action.chars().any(|character| character.is_control())
        {
            return Err(RuntimeError::protocol());
        }
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .ok_or_else(RuntimeError::protocol)?;
        if options.is_empty() || options.len() > MAX_PERMISSION_OPTIONS {
            return Err(RuntimeError::protocol());
        }
        let mut permission = PendingPermission {
            wire_id,
            allow_once: None,
            allow_session: None,
            deny: None,
        };
        let mut option_ids = BTreeSet::new();
        for option in options {
            let option = option.as_object().ok_or_else(RuntimeError::protocol)?;
            let id = bounded_text(option.get("optionId"), 128)?;
            let kind = bounded_text(option.get("kind"), 64)?;
            if !option_ids.insert(id.clone()) {
                return Err(RuntimeError::protocol());
            }
            match kind.as_str() {
                "allow_once" if permission.allow_once.is_none() => permission.allow_once = Some(id),
                "allow_always" if permission.allow_session.is_none() => {
                    permission.allow_session = Some(id)
                }
                "reject_once" | "reject_always" if permission.deny.is_none() => {
                    permission.deny = Some(id)
                }
                "allow_once" | "allow_always" | "reject_once" | "reject_always" => {
                    return Err(RuntimeError::protocol());
                }
                _ => {}
            }
        }
        if permission.allow_once.is_none() || permission.deny.is_none() {
            return Err(RuntimeError::protocol());
        }
        let mut permissions = self
            .permissions
            .lock()
            .map_err(|_| RuntimeError::internal("permission state unavailable"))?;
        if permissions.insert(request_id.clone(), permission).is_some() {
            return Err(RuntimeError::protocol());
        }
        drop(permissions);
        self.publish(RuntimeEventKind::PermissionRequested {
            request_id,
            action: action.to_owned(),
            detail: "ACP agent requested authorization for one tool call".to_owned(),
        })?;
        Ok(())
    }

    async fn handle_inbound(
        &self,
        message: &Value,
        prompt: &mut PromptAccumulator,
    ) -> Result<(), RuntimeError> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(RuntimeError::protocol)?;
        match method {
            "session/request_permission" if message.get("id").is_some() => {
                self.register_permission(message)
            }
            "session/update" if message.get("id").is_none() => {
                let params = message
                    .get("params")
                    .and_then(Value::as_object)
                    .ok_or_else(RuntimeError::protocol)?;
                if params.get("sessionId").and_then(Value::as_str) != Some(self.id.as_str()) {
                    return Err(RuntimeError::protocol());
                }
                let update = params.get("update").ok_or_else(RuntimeError::protocol)?;
                self.handle_update(update, prompt)
            }
            extension if message.get("id").is_none() && extension.starts_with('_') => Ok(()),
            _ if message.get("id").is_some() => {
                let id = message
                    .get("id")
                    .cloned()
                    .ok_or_else(RuntimeError::protocol)?;
                self.peer
                    .respond_method_not_found(id, CancellationToken::new())
                    .await?;
                Err(RuntimeError::protocol())
            }
            _ => Err(RuntimeError::protocol()),
        }
    }

    fn handle_update(
        &self,
        update: &Value,
        prompt: &mut PromptAccumulator,
    ) -> Result<(), RuntimeError> {
        let object = update.as_object().ok_or_else(RuntimeError::protocol)?;
        let kind = object
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .ok_or_else(RuntimeError::protocol)?;
        match kind {
            "user_message_chunk" => Ok(()),
            "agent_message_chunk" => {
                let text = content_text(object.get("content"))?;
                prompt.append_final(&text)?;
                self.publish(RuntimeEventKind::CommentaryDelta { text })?;
                Ok(())
            }
            "agent_thought_chunk" => {
                let text = content_text(object.get("content"))?;
                self.publish(RuntimeEventKind::ReasoningDelta { text })?;
                Ok(())
            }
            "tool_call" => {
                let call_id = bounded_text(object.get("toolCallId"), 256)?;
                let name = object
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("other");
                if name.is_empty()
                    || name.len() > 128
                    || name.chars().any(|character| character.is_control())
                {
                    return Err(RuntimeError::protocol());
                }
                let call_id = CallId::from_raw(call_id);
                if !prompt.open_tools.insert(call_id.clone()) {
                    return Err(RuntimeError::protocol());
                }
                let arguments = object.get("rawInput").cloned().unwrap_or(Value::Null);
                validate_json_shape(&arguments).map_err(|_| RuntimeError::protocol())?;
                self.publish(RuntimeEventKind::ToolCall {
                    call_id,
                    name: name.to_owned(),
                    arguments,
                })?;
                Ok(())
            }
            "tool_call_update" => {
                let call_id = CallId::from_raw(bounded_text(object.get("toolCallId"), 256)?);
                let status = object
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("in_progress");
                if matches!(status, "pending" | "in_progress") {
                    return Ok(());
                }
                let is_error = match status {
                    "completed" => false,
                    "failed" => true,
                    _ => return Err(RuntimeError::protocol()),
                };
                if !prompt.open_tools.remove(&call_id) {
                    return Err(RuntimeError::protocol());
                }
                let result = object
                    .get("rawOutput")
                    .or_else(|| object.get("content"))
                    .cloned()
                    .unwrap_or(Value::Null);
                validate_json_shape(&result).map_err(|_| RuntimeError::protocol())?;
                self.publish(RuntimeEventKind::ToolResult {
                    call_id,
                    result,
                    is_error,
                })?;
                Ok(())
            }
            "config_option_update" => {
                let options = parse_required_config_options(object.get("configOptions"))?;
                self.update_catalog(&options)?;
                *self
                    .options
                    .lock()
                    .map_err(|_| RuntimeError::internal("ACP configuration options"))? = options;
                self.publish(RuntimeEventKind::Notice {
                    code: "acp.config.updated".to_owned(),
                    message: "ACP session configuration changed".to_owned(),
                })?;
                Ok(())
            }
            "plan"
            | "available_commands_update"
            | "current_mode_update"
            | "session_info_update"
            | "usage_update" => {
                self.publish(RuntimeEventKind::Notice {
                    code: format!("acp.update.{kind}"),
                    message: "ACP session metadata changed".to_owned(),
                })?;
                Ok(())
            }
            _ => {
                self.publish(RuntimeEventKind::Notice {
                    code: "acp.update.unhandled".to_owned(),
                    message: "ACP agent emitted an unhandled extension update".to_owned(),
                })?;
                Ok(())
            }
        }
    }

    fn update_catalog(&self, options: &AcpConfigOptions) -> Result<(), RuntimeError> {
        let Some(model) = &options.model else {
            return Ok(());
        };
        let revision = self
            .catalog_revision
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::internal("catalog revision exhausted"))?
            .saturating_add(1);
        let fetched_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeError::internal("clock unavailable"))?
            .as_millis()
            .try_into()
            .map_err(|_| RuntimeError::internal("clock overflow"))?;
        let models = model
            .options
            .iter()
            .map(|option| {
                let mut descriptor = ModelDescriptor::unknown(option.value.clone());
                descriptor.display_name = option.name.clone();
                descriptor
            })
            .collect();
        *self
            .catalog
            .lock()
            .map_err(|_| RuntimeError::internal("catalog unavailable"))? = Some(CatalogSnapshot {
            provider: ProviderDescriptor {
                id: self.runtime_id.as_str().to_owned(),
                display_name: self.config.descriptor.display_name().to_owned(),
                protocols: vec![ProviderProtocol::DelegatedAgent],
            },
            models,
            revision,
            fetched_at_ms,
        });
        Ok(())
    }

    async fn send_turn(
        &self,
        input: RuntimeInput,
        caller: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        if caller.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(RuntimeError::closed());
        }
        if !input.attachments().is_empty() {
            return Err(RuntimeError::unsupported());
        }
        let gate = lock_operation(&self.operation_gate, &caller).await?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(RuntimeError::closed());
        }
        let turn_number = self
            .turn_counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::internal("turn ids exhausted"))?;
        let turn = RuntimeTurnId::new(turn_number.to_string())
            .map_err(|_| RuntimeError::internal("turn id invalid"))?;
        let active = Arc::new(ActiveOperation {
            cancellation: self.lifecycle.child_token(),
            cancel_sent: AtomicBool::new(false),
        });
        {
            let mut slot = self
                .active
                .lock()
                .map_err(|_| RuntimeError::internal("operation state unavailable"))?;
            if slot.is_some() {
                return Err(RuntimeError::conflict());
            }
            *slot = Some(Arc::clone(&active));
        }
        let clear = ActiveGuard {
            slot: &self.active,
            active: Arc::clone(&active),
        };
        self.publish(RuntimeEventKind::TurnStarted { turn: turn.clone() })?;
        let request_id = self
            .peer
            .next_id
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::internal("ACP request ids exhausted"))?;
        if let Err(error) = self
            .peer
            .write_value(
                &serde_json::json!({
                    "jsonrpc":"2.0","id":request_id,"method":"session/prompt",
                    "params":{"sessionId":self.id.as_str(),"prompt":[{"type":"text","text":input.text()}]}
                }),
                caller.clone(),
            )
            .await
        {
            return Err(self.terminate(error).await);
        }
        let mut prompt = PromptAccumulator::default();
        let mut cancelled = false;
        let result: Result<Value, RuntimeError> = async {
            loop {
                let message = if cancelled {
                    self.peer.read_value(self.lifecycle.clone()).await
                } else {
                    read_prompt_message(&self.peer, &active.cancellation, &caller).await
                };
                let message = match message {
                    Err(error) if error.code() == RuntimeErrorCode::Cancelled && !cancelled => {
                        self.request_cancel(&active).await?;
                        cancelled = true;
                        continue;
                    }
                    other => other?,
                }
                .ok_or_else(RuntimeError::protocol)?;
                if message.get("method").is_some() {
                    self.handle_inbound(&message, &mut prompt).await?;
                    continue;
                }
                if message.get("id").and_then(Value::as_u64) != Some(request_id) {
                    break Err(RuntimeError::protocol());
                }
                break parse_response_result(&message);
            }
        }
        .await;
        let result = match result {
            Ok(value) => value,
            Err(error) => return Err(self.terminate(error).await),
        };
        if !self
            .permissions
            .lock()
            .map_err(|_| RuntimeError::internal("permission state unavailable"))?
            .is_empty()
            || !prompt.open_tools.is_empty()
        {
            let error = RuntimeError::protocol();
            return Err(self.terminate(error).await);
        }
        let reason = if cancelled || caller.is_cancelled() || active.cancellation.is_cancelled() {
            RuntimeFinishReason::Cancelled
        } else {
            match parse_stop_reason(&result) {
                Ok(reason) => reason,
                Err(error) => return Err(self.terminate(error).await),
            }
        };
        // A normally stopped turn always publishes its final message, empty
        // text included: R02 requires it, and a silent completion is a
        // completion, not a protocol failure worth killing the session for.
        if (!prompt.final_text.is_empty() || reason == RuntimeFinishReason::Stop)
            && let Err(error) = self.publish(RuntimeEventKind::FinalMessage {
                text: prompt.final_text,
            })
        {
            return Err(self.terminate(error).await);
        }
        if let Err(error) = self.publish(RuntimeEventKind::TurnFinished {
            turn: turn.clone(),
            reason,
        }) {
            return Err(self.terminate(error).await);
        }
        drop(clear);
        drop(gate);
        if reason == RuntimeFinishReason::Cancelled {
            Err(RuntimeError::cancelled())
        } else if reason == RuntimeFinishReason::Error {
            Err(RuntimeError::unavailable())
        } else {
            Ok(turn)
        }
    }
}

struct ActiveGuard<'a> {
    slot: &'a Mutex<Option<Arc<ActiveOperation>>>,
    active: Arc<ActiveOperation>,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        let Ok(mut slot) = self.slot.lock() else {
            return;
        };
        if slot
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.active))
        {
            *slot = None;
        }
    }
}

#[derive(Default)]
struct PromptAccumulator {
    final_text: String,
    open_tools: HashSet<CallId>,
}

impl PromptAccumulator {
    fn append_final(&mut self, text: &str) -> Result<(), RuntimeError> {
        if self
            .final_text
            .len()
            .checked_add(text.len())
            .is_none_or(|length| length > MAX_ACCUMULATED_TEXT_BYTES)
        {
            return Err(RuntimeError::protocol());
        }
        self.final_text.push_str(text);
        Ok(())
    }
}

fn content_text(value: Option<&Value>) -> Result<String, RuntimeError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    if object.get("type").and_then(Value::as_str) != Some("text") {
        return Err(RuntimeError::protocol());
    }
    let text = object
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(RuntimeError::protocol)?;
    if text.is_empty() || text.len() > MAX_ACCUMULATED_TEXT_BYTES || text.contains('\0') {
        return Err(RuntimeError::protocol());
    }
    Ok(text.to_owned())
}

fn parse_stop_reason(result: &Value) -> Result<RuntimeFinishReason, RuntimeError> {
    match result.get("stopReason").and_then(Value::as_str) {
        Some("end_turn") => Ok(RuntimeFinishReason::Stop),
        Some("max_tokens" | "max_turn_requests") => Ok(RuntimeFinishReason::Limit),
        Some("cancelled") => Ok(RuntimeFinishReason::Cancelled),
        Some("refusal") => Ok(RuntimeFinishReason::Error),
        _ => Err(RuntimeError::protocol()),
    }
}

async fn lock_operation<'a>(
    gate: &'a AsyncMutex<()>,
    cancellation: &CancellationToken,
) -> Result<futures::lock::MutexGuard<'a, ()>, RuntimeError> {
    let lock = gate.lock().fuse();
    let cancelled = cancellation.cancelled().fuse();
    futures::pin_mut!(lock, cancelled);
    futures::select_biased! {
        () = cancelled => Err(RuntimeError::cancelled()),
        guard = lock => Ok(guard),
    }
}

async fn read_prompt_message(
    peer: &AcpPeer,
    operation: &CancellationToken,
    caller: &CancellationToken,
) -> Result<Option<Value>, RuntimeError> {
    let read_cancellation = CancellationToken::new();
    let read = peer.read_value(read_cancellation.clone()).fuse();
    let operation_cancelled = operation.cancelled().fuse();
    let caller_cancelled = caller.cancelled().fuse();
    futures::pin_mut!(read, operation_cancelled, caller_cancelled);
    futures::select_biased! {
        () = caller_cancelled => {
            read_cancellation.cancel();
            Err(RuntimeError::cancelled())
        },
        () = operation_cancelled => {
            read_cancellation.cancel();
            Err(RuntimeError::cancelled())
        },
        value = read => value,
    }
}

fn runtime_request_id(id: &Value) -> Result<RuntimeRequestId, RuntimeError> {
    let value = match id {
        Value::String(value) if value.len() <= 240 => format!("s:{value}"),
        Value::Number(value) if value.as_u64().is_some() => format!("n:{value}"),
        _ => return Err(RuntimeError::protocol()),
    };
    RuntimeRequestId::new(value).map_err(|_| RuntimeError::protocol())
}

#[async_trait]
impl RuntimeSession for AcpRuntimeSession {
    fn id(&self) -> &RuntimeSessionId {
        &self.id
    }

    fn runtime_id(&self) -> &AgentRuntimeId {
        &self.runtime_id
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    fn subscribe(&self) -> RuntimeEventStream {
        self.hub.subscribe()
    }

    async fn send(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        self.send_turn(input, cancellation).await
    }

    async fn configure(
        &self,
        configuration: RuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<RuntimeConfiguration, RuntimeError> {
        validate_acp_configuration(&configuration)?;
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if self.active_operation()?.is_some() {
            return Err(RuntimeError::conflict());
        }
        let _gate = lock_operation(&self.operation_gate, &cancellation).await?;
        if self.active_operation()?.is_some() {
            return Err(RuntimeError::conflict());
        }
        let mut options = self
            .options
            .lock()
            .map_err(|_| RuntimeError::internal("ACP configuration options"))?
            .clone();
        let mut provider_changed = false;
        for (kind, value) in [
            (AcpSelectionKind::Model, configuration.model()),
            (AcpSelectionKind::Effort, configuration.reasoning_effort()),
        ] {
            let Some(value) = value else {
                continue;
            };
            // A model update can replace dependent controls. Re-resolve the
            // next selection from the complete state returned by the agent.
            let selection = match options.select(kind, value) {
                Ok(selection) => selection,
                Err(error) if provider_changed => return Err(self.terminate(error).await),
                Err(error) => return Err(error),
            };
            let config_option = matches!(selection, AcpModelControl::ConfigOption(_));
            let (method, params) = match &selection {
                AcpModelControl::ConfigOption(config_id) => (
                    "session/set_config_option",
                    serde_json::json!({
                        "sessionId":self.id.as_str(), "configId":config_id, "value":value,
                    }),
                ),
                AcpModelControl::Legacy => (
                    "session/set_model",
                    serde_json::json!({"sessionId":self.id.as_str(), "modelId":value}),
                ),
            };
            let response = match self
                .peer
                .request(method, params, cancellation.clone(), pre_session_message)
                .await
            {
                Ok(response) => response,
                Err(error) => return Err(self.terminate(error).await),
            };
            provider_changed = true;
            if config_option {
                let parsed = match parse_required_config_options(response.get("configOptions")) {
                    Ok(parsed) => parsed,
                    Err(error) => return Err(self.terminate(error).await),
                };
                if let Err(error) =
                    ensure_config_selection_applied(&parsed, kind, &selection, value)
                {
                    return Err(self.terminate(error).await);
                }
                if let Err(error) = self.update_catalog(&parsed) {
                    return Err(self.terminate(error).await);
                }
                options = parsed;
            }
        }
        let effective = {
            let mut effective = self
                .configuration
                .lock()
                .map_err(|_| RuntimeError::internal("ACP session configuration"))?;
            *effective = effective.merged_with(&configuration);
            effective.clone()
        };
        *self
            .options
            .lock()
            .map_err(|_| RuntimeError::internal("ACP configuration options"))? = options;
        Ok(effective)
    }

    async fn steer(
        &self,
        _input: RuntimeInput,
        _cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::unsupported())
    }

    async fn follow_up(
        &self,
        _input: RuntimeInput,
        _cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::unsupported())
    }

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let Some(active) = self.active_operation()? else {
            return Ok(());
        };
        match self.request_cancel(&active).await {
            Ok(()) => Ok(()),
            Err(error) => Err(self.terminate(error).await),
        }
    }

    async fn respond_permission(
        &self,
        response: RuntimePermissionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let (permission, option) = {
            let mut permissions = self
                .permissions
                .lock()
                .map_err(|_| RuntimeError::internal("permission state unavailable"))?;
            let permission = permissions
                .get(response.request_id())
                .ok_or_else(RuntimeError::conflict)?;
            let option = match response.decision() {
                RuntimePermissionDecision::AllowOnce => permission.allow_once.clone(),
                RuntimePermissionDecision::AllowSession => permission.allow_session.clone(),
                RuntimePermissionDecision::Deny => permission.deny.clone(),
            }
            .ok_or_else(RuntimeError::unsupported)?;
            let permission = permissions
                .remove(response.request_id())
                .ok_or_else(RuntimeError::conflict)?;
            (permission, option)
        };
        let result = self
            .peer
            .respond(
                permission.wire_id,
                serde_json::json!({
                    "outcome":{"outcome":"selected","optionId":option}
                }),
                cancellation,
            )
            .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(self.terminate(error).await),
        }
    }

    async fn respond_question(
        &self,
        _response: RuntimeQuestionResponse,
        _cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::unsupported())
    }

    async fn compact(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<RuntimeCompactOutcome, RuntimeError> {
        Err(RuntimeError::unsupported())
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if cancellation.is_cancelled() {
            self.closed.store(false, Ordering::SeqCst);
            return Err(RuntimeError::cancelled());
        }
        let operation_result = match self.active_operation() {
            Ok(Some(active)) => self.request_cancel(&active).await,
            Ok(None) => {
                let mut result = self.cancel_permissions().await;
                if result.is_ok() && self.peer.negotiated().close {
                    result = self
                        .peer
                        .request(
                            "session/close",
                            serde_json::json!({"sessionId":self.id.as_str()}),
                            CancellationToken::new(),
                            pre_session_message,
                        )
                        .await
                        .map(|_| ());
                }
                result
            }
            Err(error) => Err(error),
        };
        self.lifecycle.cancel();
        let close_result = self.peer.close(CancellationToken::new()).await;
        let _gate = self.operation_gate.lock().await;
        self.hub.close();
        match (operation_result, close_result) {
            (Err(error), _) => Err(error),
            (Ok(()), result) => result,
        }
    }
}

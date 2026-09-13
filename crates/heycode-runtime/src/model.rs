//! Runtime metadata, requests, controls and normalized event vocabulary.

use std::fmt::{Debug, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heycode_core::{CallId, SessionId, TokenUsage};
use heycode_llm::CapabilitySupport;

use crate::{
    AgentRuntimeId, RuntimeContractError, RuntimeRequestId, RuntimeSessionId, RuntimeTurnId,
};

/// Whether heycode or an external coding-agent implementation owns the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntimeKind {
    /// heycode owns the loop and invokes inference adapters.
    Native,
    /// An official/external agent owns the loop; heycode bridges lifecycle/events.
    Delegated,
}

/// Tri-state evidence for optional runtime/session operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    /// Runtime-owned model discovery.
    pub models: CapabilitySupport,
    /// Resume an existing external session.
    pub resume: CapabilitySupport,
    /// Fork an existing external session.
    pub fork: CapabilitySupport,
    /// Inject text into the next step of an active turn.
    pub steer: CapabilitySupport,
    /// Queue a next-turn message.
    pub follow_up: CapabilitySupport,
    /// Runtime-originated permission requests and answers.
    pub permissions: CapabilitySupport,
    /// Runtime-originated human questions and answers.
    pub questions: CapabilitySupport,
    /// Provider/runtime-native compaction.
    pub compaction: CapabilitySupport,
}

/// Evidence for model-visible controls a runtime can apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfigurationCapabilities {
    /// Replace the runtime's model-visible system instructions.
    pub system_prompt: CapabilitySupport,
    /// Execute host-defined tools through a correlated callback bridge.
    pub tools: CapabilitySupport,
    /// Select a provider-native model id.
    pub model: CapabilitySupport,
    /// Select a provider-native reasoning/thought effort.
    pub reasoning_effort: CapabilitySupport,
}

impl RuntimeConfigurationCapabilities {
    /// Conservative value before an adapter proves support.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            system_prompt: CapabilitySupport::Unknown,
            tools: CapabilitySupport::Unknown,
            model: CapabilitySupport::Unknown,
            reasoning_effort: CapabilitySupport::Unknown,
        }
    }
}

impl Default for RuntimeConfigurationCapabilities {
    fn default() -> Self {
        Self::unknown()
    }
}

impl RuntimeCapabilities {
    /// Whether delegated tool execution has a proven human permission bridge.
    ///
    /// Start/send/cancel/close are required runtime operations. Resume, fork,
    /// steering and compaction remain independently gated optional controls.
    #[must_use]
    pub fn supports_primary_sessions(&self) -> bool {
        self.permissions.is_supported()
    }

    /// Conservative descriptor before a runtime proves support.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            models: CapabilitySupport::Unknown,
            resume: CapabilitySupport::Unknown,
            fork: CapabilitySupport::Unknown,
            steer: CapabilitySupport::Unknown,
            follow_up: CapabilitySupport::Unknown,
            permissions: CapabilitySupport::Unknown,
            questions: CapabilitySupport::Unknown,
            compaction: CapabilitySupport::Unknown,
        }
    }
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        Self::unknown()
    }
}

/// Safe, immutable runtime discovery row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeDescriptor {
    id: AgentRuntimeId,
    display_name: String,
    kind: AgentRuntimeKind,
    capabilities: RuntimeCapabilities,
    connection_help: Option<String>,
    configuration: RuntimeConfigurationCapabilities,
}

impl AgentRuntimeDescriptor {
    /// Validate one runtime descriptor before registration.
    ///
    /// # Errors
    /// Invalid id or display text.
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        kind: AgentRuntimeKind,
        capabilities: RuntimeCapabilities,
    ) -> Result<Self, RuntimeContractError> {
        let id = AgentRuntimeId::new(id)?;
        let display_name = display_name.into();
        validate_one_line(&display_name, "display name", 128)?;
        Ok(Self {
            id,
            display_name,
            kind,
            capabilities,
            connection_help: None,
            configuration: RuntimeConfigurationCapabilities::unknown(),
        })
    }

    /// Attach evidence for model-visible session controls.
    #[must_use]
    pub fn with_configuration_capabilities(
        mut self,
        capabilities: RuntimeConfigurationCapabilities,
    ) -> Self {
        self.configuration = capabilities;
        self
    }

    /// Attach provider-owned installation and sign-in instructions.
    ///
    /// # Errors
    /// Empty, oversized or control-bearing help is rejected.
    pub fn with_connection_help(
        mut self,
        help: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let help = help.into();
        validate_one_line(&help, "connection help", 512)?;
        self.connection_help = Some(help);
        Ok(self)
    }

    /// Provider-owned recovery instructions, when supplied.
    #[must_use]
    pub fn connection_help(&self) -> Option<&str> {
        self.connection_help.as_deref()
    }

    /// Stable registry id.
    #[must_use]
    pub const fn id(&self) -> &AgentRuntimeId {
        &self.id
    }

    /// Human runtime name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Native-vs-delegated loop ownership.
    #[must_use]
    pub const fn kind(&self) -> AgentRuntimeKind {
        self.kind
    }

    /// Evidence-backed optional operations.
    #[must_use]
    pub const fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    /// Evidence-backed model-visible configuration controls.
    #[must_use]
    pub const fn configuration_capabilities(&self) -> &RuntimeConfigurationCapabilities {
        &self.configuration
    }
}

/// Complete model-visible configuration requested for one runtime session.
#[derive(Clone, Default, PartialEq)]
pub struct RuntimeConfiguration {
    system_prompt: Option<String>,
    tools: Vec<heycode_core::ToolSpec>,
    tools_configured: bool,
    model: Option<String>,
    reasoning_effort: Option<String>,
}

impl Debug for RuntimeConfiguration {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeConfiguration")
            .field(
                "system_prompt_bytes",
                &self.system_prompt.as_ref().map(String::len),
            )
            .field("tool_count", &self.tools.len())
            .field("tools_configured", &self.tools_configured)
            .field("model", &self.model.as_deref().map(|_| "<redacted>"))
            .field(
                "reasoning_effort",
                &self.reasoning_effort.as_deref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl RuntimeConfiguration {
    /// Construct an empty configuration that lets the runtime use its defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            system_prompt: None,
            tools: Vec::new(),
            tools_configured: false,
            model: None,
            reasoning_effort: None,
        }
    }

    /// Replace the model-visible system prompt.
    ///
    /// # Errors
    /// Empty, NUL-bearing, or over-1-MiB prompts are rejected.
    pub fn with_system_prompt(
        mut self,
        prompt: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() || prompt.len() > 1024 * 1024 || prompt.contains('\0') {
            return Err(RuntimeContractError::invalid(
                "system prompt",
                "non-blank NUL-free UTF-8 up to 1 MiB",
            ));
        }
        self.system_prompt = Some(prompt);
        Ok(self)
    }

    /// Attach the exact host tool catalog exposed to the runtime.
    ///
    /// # Errors
    /// Invalid, duplicate, or oversized tool definitions are rejected.
    pub fn with_tools(
        mut self,
        tools: Vec<heycode_core::ToolSpec>,
    ) -> Result<Self, RuntimeContractError> {
        validate_tools(&tools)?;
        self.tools = tools;
        self.tools_configured = true;
        Ok(self)
    }

    /// Select a provider-native model id.
    ///
    /// # Errors
    /// Blank/control-bearing/over-256-byte ids are rejected.
    pub fn with_model(mut self, model: impl Into<String>) -> Result<Self, RuntimeContractError> {
        let model = model.into();
        validate_one_line(&model, "model id", 256)?;
        self.model = Some(model);
        Ok(self)
    }

    /// Select a provider-native reasoning effort id.
    ///
    /// # Errors
    /// Blank/control-bearing/over-128-byte ids are rejected.
    pub fn with_reasoning_effort(
        mut self,
        effort: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let effort = effort.into();
        validate_one_line(&effort, "reasoning effort", 128)?;
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    /// Exact system prompt, when explicitly requested.
    #[must_use]
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    /// Exact ordered host tool definitions.
    #[must_use]
    pub fn tools(&self) -> &[heycode_core::ToolSpec] {
        &self.tools
    }

    /// Whether the caller explicitly supplied a tool catalog, including an
    /// empty catalog that disables inherited tools.
    #[must_use]
    pub const fn tools_configured(&self) -> bool {
        self.tools_configured
    }

    /// Provider-native model id, when explicitly selected.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Provider-native reasoning effort, when explicitly selected.
    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    /// Whether no field overrides a runtime default.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.system_prompt.is_none()
            && !self.tools_configured
            && self.model.is_none()
            && self.reasoning_effort.is_none()
    }

    /// Merge the non-empty fields of an update into this complete value.
    #[must_use]
    pub fn merged_with(&self, update: &Self) -> Self {
        Self {
            system_prompt: update
                .system_prompt
                .clone()
                .or_else(|| self.system_prompt.clone()),
            tools: if update.tools_configured {
                update.tools.clone()
            } else {
                self.tools.clone()
            },
            tools_configured: self.tools_configured || update.tools_configured,
            model: update.model.clone().or_else(|| self.model.clone()),
            reasoning_effort: update
                .reasoning_effort
                .clone()
                .or_else(|| self.reasoning_effort.clone()),
        }
    }
}

/// One provider-advertised model and its exact effort choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModelConfiguration {
    /// Exact provider-native value accepted by the runtime control wire.
    pub model: String,
    /// Provider-advertised human label, falling back to `model` only when the
    /// runtime omitted one.
    pub display_name: String,
    /// Provider-advertised resolved/canonical identity, when the runtime
    /// distinguishes it from the accepted control value.
    pub resolved_model: Option<String>,
    /// Provider-advertised model description, when present.
    pub description: Option<String>,
    /// Structured input-context limit, when the runtime advertises one.
    /// Aliases and human descriptions are not converted into a numeric limit.
    pub context_window: Option<u64>,
    /// Provider-native default effort, when advertised.
    pub default_reasoning_effort: Option<String>,
    /// Ordered provider-native effort choices.
    pub reasoning_efforts: Vec<String>,
}

/// One correlated host-tool invocation from a delegated runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeToolCall {
    /// Provider/runtime call identity.
    pub call_id: CallId,
    /// Exact registered tool name.
    pub name: String,
    /// Parsed JSON arguments.
    pub arguments: serde_json::Value,
}

/// Bounded model-visible settlement returned to a delegated runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeToolOutput {
    /// Model-visible text after durable result admission.
    pub content: String,
    /// True when the tool was denied or failed.
    pub is_error: bool,
}

/// Provider-reported size of the latest model request and its exact context
/// window. This is current-request evidence, unlike turn-aggregate usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContextUsage {
    /// Input tokens in the latest provider request, including cache reads and
    /// cache creation when the provider reports those components separately.
    pub tokens: u64,
    /// Provider-reported context capacity for that request's model.
    pub context_window: u64,
    /// Concrete model identity reported for this request.
    pub resolved_model: Option<String>,
}

/// Host callback that owns approval, execution, and durable tool logging.
#[async_trait::async_trait]
pub trait RuntimeToolExecutor: Send + Sync {
    /// Execute one correlated call and return only after its result is durable.
    async fn execute(
        &self,
        call: RuntimeToolCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<RuntimeToolOutput, crate::RuntimeError>;
}

/// Safe runtime account/authentication state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    /// Runtime reports an authenticated account.
    Connected,
    /// The selected runtime/provider can operate without an account credential.
    NotRequired,
    /// Runtime is installed/available but not authenticated.
    Disconnected,
    /// Previously authenticated state expired or was revoked.
    Expired,
    /// Runtime/account inspection cannot currently run.
    Unavailable,
    /// Runtime cannot prove the state.
    Unknown,
}

/// Runtime account status with an optional non-secret display label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountState {
    status: AccountStatus,
    label: Option<String>,
}

impl AccountState {
    /// Construct an authenticated account projection.
    ///
    /// # Errors
    /// A supplied label must be trimmed, control-free and at most 128 bytes.
    pub fn connected(label: Option<&str>) -> Result<Self, RuntimeContractError> {
        let label = label.map(str::to_owned);
        if let Some(label) = &label {
            validate_one_line(label, "account label", 128)?;
        }
        Ok(Self {
            status: AccountStatus::Connected,
            label,
        })
    }

    /// Construct a label-free non-connected state.
    #[must_use]
    pub const fn without_label(status: AccountStatus) -> Self {
        Self {
            status,
            label: None,
        }
    }

    /// Stable auth state.
    #[must_use]
    pub const fn status(&self) -> AccountStatus {
        self.status
    }

    /// Optional non-secret account label.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

/// New delegated/native runtime session request.
#[derive(Clone)]
pub struct RuntimeStart {
    session_id: SessionId,
    workspace: PathBuf,
    configuration: RuntimeConfiguration,
    tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
    ephemeral: bool,
}

impl RuntimeStart {
    /// Bind a new runtime session to one durable heycode session and absolute workspace.
    ///
    /// # Errors
    /// Empty heycode ids or relative workspaces are rejected.
    pub fn new(
        session_id: SessionId,
        workspace: impl AsRef<Path>,
    ) -> Result<Self, RuntimeContractError> {
        validate_session_id(&session_id)?;
        let workspace = validate_workspace(workspace.as_ref())?;
        Ok(Self {
            session_id,
            workspace,
            configuration: RuntimeConfiguration::new(),
            tool_executor: None,
            ephemeral: false,
        })
    }

    /// Attach a runtime-native model id.
    ///
    /// # Errors
    /// Blank/control-bearing/over-256-byte ids are rejected.
    pub fn with_model(mut self, model: impl Into<String>) -> Result<Self, RuntimeContractError> {
        let model = model.into();
        validate_one_line(&model, "model id", 256)?;
        self.configuration = self.configuration.with_model(model)?;
        Ok(self)
    }

    /// Attach the complete validated model-visible configuration.
    #[must_use]
    pub fn with_configuration(mut self, configuration: RuntimeConfiguration) -> Self {
        self.configuration = configuration;
        self
    }

    /// Attach the guarded host tool executor required by non-empty tool definitions.
    #[must_use]
    pub fn with_tool_executor(mut self, executor: Arc<dyn RuntimeToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    /// Request a provider-native session that leaves no resumable history.
    #[must_use]
    pub const fn with_ephemeral(mut self) -> Self {
        self.ephemeral = true;
        self
    }

    /// Durable heycode session identity.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Absolute workspace path.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Optional runtime-native model selection.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.configuration.model()
    }

    /// Complete requested model-visible configuration.
    #[must_use]
    pub const fn configuration(&self) -> &RuntimeConfiguration {
        &self.configuration
    }

    /// Guarded callback used for host-defined tools.
    #[must_use]
    pub fn tool_executor(&self) -> Option<&Arc<dyn RuntimeToolExecutor>> {
        self.tool_executor.as_ref()
    }

    /// Whether the external runtime must suppress resumable persistence.
    #[must_use]
    pub const fn ephemeral(&self) -> bool {
        self.ephemeral
    }
}

impl Debug for RuntimeStart {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeStart")
            .field("session_id", &"<redacted>")
            .field("workspace", &"<redacted>")
            .field("configuration", &self.configuration)
            .field("has_tool_executor", &self.tool_executor.is_some())
            .field("ephemeral", &self.ephemeral)
            .finish()
    }
}

/// Resume one provider-native runtime session into a durable heycode session.
#[derive(Clone)]
pub struct RuntimeResume {
    session_id: SessionId,
    workspace: PathBuf,
    runtime_session_id: RuntimeSessionId,
    configuration: RuntimeConfiguration,
    tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
}

impl RuntimeResume {
    /// Validate a resume request.
    ///
    /// # Errors
    /// Empty heycode ids or relative workspaces are rejected.
    pub fn new(
        session_id: SessionId,
        workspace: impl AsRef<Path>,
        runtime_session_id: RuntimeSessionId,
    ) -> Result<Self, RuntimeContractError> {
        validate_session_id(&session_id)?;
        Ok(Self {
            session_id,
            workspace: validate_workspace(workspace.as_ref())?,
            runtime_session_id,
            configuration: RuntimeConfiguration::new(),
            tool_executor: None,
        })
    }

    /// Attach the complete validated model-visible configuration.
    #[must_use]
    pub fn with_configuration(mut self, configuration: RuntimeConfiguration) -> Self {
        self.configuration = configuration;
        self
    }

    /// Attach the guarded host tool executor required by non-empty tool definitions.
    #[must_use]
    pub fn with_tool_executor(mut self, executor: Arc<dyn RuntimeToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    /// Durable heycode session identity.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Absolute workspace path.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Provider-native session to resume.
    #[must_use]
    pub const fn runtime_session_id(&self) -> &RuntimeSessionId {
        &self.runtime_session_id
    }

    /// Complete requested model-visible configuration.
    #[must_use]
    pub const fn configuration(&self) -> &RuntimeConfiguration {
        &self.configuration
    }

    /// Guarded callback used for host-defined tools.
    #[must_use]
    pub fn tool_executor(&self) -> Option<&Arc<dyn RuntimeToolExecutor>> {
        self.tool_executor.as_ref()
    }
}

impl Debug for RuntimeResume {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeResume")
            .field("session_id", &"<redacted>")
            .field("workspace", &"<redacted>")
            .field("runtime_session_id", &"<redacted>")
            .field("configuration", &self.configuration)
            .field("has_tool_executor", &self.tool_executor.is_some())
            .finish()
    }
}

/// Fork one provider-native runtime session into a new durable heycode session.
#[derive(Clone)]
pub struct RuntimeFork {
    session_id: SessionId,
    workspace: PathBuf,
    source_runtime_session_id: RuntimeSessionId,
    configuration: RuntimeConfiguration,
    tool_executor: Option<Arc<dyn RuntimeToolExecutor>>,
}

impl RuntimeFork {
    /// Validate a fork request.
    ///
    /// # Errors
    /// Empty heycode ids or relative workspaces are rejected.
    pub fn new(
        session_id: SessionId,
        workspace: impl AsRef<Path>,
        source_runtime_session_id: RuntimeSessionId,
    ) -> Result<Self, RuntimeContractError> {
        validate_session_id(&session_id)?;
        Ok(Self {
            session_id,
            workspace: validate_workspace(workspace.as_ref())?,
            source_runtime_session_id,
            configuration: RuntimeConfiguration::new(),
            tool_executor: None,
        })
    }

    /// Attach the complete validated model-visible configuration.
    #[must_use]
    pub fn with_configuration(mut self, configuration: RuntimeConfiguration) -> Self {
        self.configuration = configuration;
        self
    }

    /// Attach the guarded host tool executor required by non-empty tool definitions.
    #[must_use]
    pub fn with_tool_executor(mut self, executor: Arc<dyn RuntimeToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    /// New durable heycode session identity.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Absolute workspace path.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Provider-native source session.
    #[must_use]
    pub const fn source_runtime_session_id(&self) -> &RuntimeSessionId {
        &self.source_runtime_session_id
    }

    /// Complete requested model-visible configuration.
    #[must_use]
    pub const fn configuration(&self) -> &RuntimeConfiguration {
        &self.configuration
    }

    /// Guarded callback used for host-defined tools.
    #[must_use]
    pub fn tool_executor(&self) -> Option<&Arc<dyn RuntimeToolExecutor>> {
        self.tool_executor.as_ref()
    }
}

impl Debug for RuntimeFork {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeFork")
            .field("session_id", &"<redacted>")
            .field("workspace", &"<redacted>")
            .field("source_runtime_session_id", &"<redacted>")
            .field("configuration", &self.configuration)
            .field("has_tool_executor", &self.tool_executor.is_some())
            .finish()
    }
}

/// One bounded human text/media input to a runtime session.
#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeInput {
    text: String,
    attachments: Vec<heycode_core::AttachmentMetadata>,
}

impl RuntimeInput {
    /// Validate non-empty text up to 1 MiB.
    ///
    /// # Errors
    /// Blank, oversized or NUL-bearing input is rejected.
    pub fn new(text: impl Into<String>) -> Result<Self, RuntimeContractError> {
        Self::with_attachments(text, Vec::new())
    }

    /// Validate one text/media input. Empty text is permitted only when at
    /// least one unique attachment is present.
    ///
    /// # Errors
    /// Blank text-only, oversized/NUL text, invalid/duplicate metadata or more
    /// than sixteen attachments fails.
    pub fn with_attachments(
        text: impl Into<String>,
        attachments: Vec<heycode_core::AttachmentMetadata>,
    ) -> Result<Self, RuntimeContractError> {
        let text = text.into();
        if (text.trim().is_empty() && attachments.is_empty())
            || text.len() > 1024 * 1024
            || text.contains('\0')
            || attachments.len() > 16
        {
            return Err(RuntimeContractError::invalid(
                "input",
                "text or attachments with NUL-free UTF-8 up to 1 MiB",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for attachment in &attachments {
            attachment.validate().map_err(|_| {
                RuntimeContractError::invalid("input attachment", "validated durable metadata")
            })?;
            if !ids.insert(attachment.content_id().as_str().to_owned()) {
                return Err(RuntimeContractError::invalid(
                    "input attachment",
                    "unique durable content ids",
                ));
            }
        }
        Ok(Self { text, attachments })
    }

    /// Exact human text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Ordered already-admitted attachment selections.
    #[must_use]
    pub fn attachments(&self) -> &[heycode_core::AttachmentMetadata] {
        &self.attachments
    }
}

impl Debug for RuntimeInput {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeInput")
            .field("text_bytes", &self.text.len())
            .field("attachment_count", &self.attachments.len())
            .finish()
    }
}

/// Human decision for a runtime-originated permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePermissionDecision {
    /// Allow this one operation.
    AllowOnce,
    /// Allow matching operations for the active runtime session.
    AllowSession,
    /// Deny the operation.
    Deny,
}

/// Correlated answer to a runtime permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePermissionResponse {
    request_id: RuntimeRequestId,
    decision: RuntimePermissionDecision,
}

impl RuntimePermissionResponse {
    /// Bind a decision to its provider-native request id.
    #[must_use]
    pub const fn new(request_id: RuntimeRequestId, decision: RuntimePermissionDecision) -> Self {
        Self {
            request_id,
            decision,
        }
    }

    /// Provider-native request id.
    #[must_use]
    pub const fn request_id(&self) -> &RuntimeRequestId {
        &self.request_id
    }

    /// Human decision.
    #[must_use]
    pub const fn decision(&self) -> RuntimePermissionDecision {
        self.decision
    }
}

/// Correlated answer to a runtime-originated human question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeQuestionResponse {
    request_id: RuntimeRequestId,
    answer: Option<String>,
    selected: Option<Vec<String>>,
}

impl RuntimeQuestionResponse {
    /// Validate a bounded answer.
    ///
    /// # Errors
    /// Blank, NUL-bearing or over-16-KiB answers are rejected.
    pub fn new(
        request_id: RuntimeRequestId,
        answer: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let answer = answer.into();
        if answer.trim().is_empty() || answer.len() > 16 * 1024 || answer.contains('\0') {
            return Err(RuntimeContractError::invalid(
                "question answer",
                "non-blank NUL-free UTF-8 up to 16 KiB",
            ));
        }
        Ok(Self {
            request_id,
            answer: Some(answer),
            selected: None,
        })
    }

    /// Preserve explicit selected labels as an array, never a JSON-encoded string.
    ///
    /// # Errors
    /// Empty, duplicate, blank, NUL-bearing or oversized selections are rejected.
    pub fn selected(
        request_id: RuntimeRequestId,
        labels: Vec<String>,
    ) -> Result<Self, RuntimeContractError> {
        let mut unique = std::collections::HashSet::new();
        if labels.is_empty()
            || labels.len() > 16
            || labels.iter().any(|label| {
                label.trim().is_empty() || label.contains('\0') || !unique.insert(label)
            })
            || labels.iter().map(String::len).sum::<usize>() > 16 * 1024
        {
            return Err(RuntimeContractError::invalid(
                "question selections",
                "1-16 unique bounded non-blank labels",
            ));
        }
        Ok(Self {
            request_id,
            answer: None,
            selected: Some(labels),
        })
    }

    /// Explicit selected labels; absent for custom text or cancellation.
    #[must_use]
    pub fn selected_answers(&self) -> Option<&[String]> {
        self.selected.as_deref()
    }

    /// Explicitly cancel a pending question.
    #[must_use]
    pub const fn cancelled(request_id: RuntimeRequestId) -> Self {
        Self {
            request_id,
            answer: None,
            selected: None,
        }
    }

    /// Provider-native request id.
    #[must_use]
    pub const fn request_id(&self) -> &RuntimeRequestId {
        &self.request_id
    }

    /// Exact human answer.
    #[must_use]
    pub fn answer(&self) -> Option<&str> {
        self.answer.as_deref()
    }
}

/// Result of one runtime-native compaction request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCompactOutcome {
    /// Runtime committed a compaction/checkpoint.
    Applied,
    /// Runtime reported no compaction was necessary.
    Noop,
}

/// Stable turn settlement reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFinishReason {
    /// Normal completion.
    Stop,
    /// Output/runtime budget exhausted.
    Limit,
    /// Human/lifecycle cancellation.
    Cancelled,
    /// Runtime reported a safe classified failure.
    Error,
}

/// One monotonically sequenced normalized session event.
#[derive(Clone, PartialEq)]
pub struct RuntimeEvent {
    sequence: u64,
    kind: RuntimeEventKind,
}

impl Debug for RuntimeEvent {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeEvent")
            .field("sequence", &self.sequence)
            .field("kind", &self.kind)
            .finish()
    }
}

impl RuntimeEvent {
    /// Construct one raw typed adapter event.
    ///
    /// Delegated event consumers pass raw values through
    /// `RuntimeEventNormalizer` before durable or UI projection.
    #[must_use]
    pub const fn new(sequence: u64, kind: RuntimeEventKind) -> Self {
        Self { sequence, kind }
    }

    /// Provider-session-local monotonic sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Normalized semantic payload.
    #[must_use]
    pub const fn kind(&self) -> &RuntimeEventKind {
        &self.kind
    }
}

/// Provider-neutral runtime events consumed through the R02 normalizer.
#[derive(Clone, PartialEq)]
pub enum RuntimeEventKind {
    /// External/native session is initialized and accepting input.
    SessionReady,
    /// One runtime turn began.
    TurnStarted {
        /// Provider-native turn id.
        turn: RuntimeTurnId,
    },
    /// User-visible progress/commentary text.
    CommentaryDelta {
        /// Incremental text.
        text: String,
    },
    /// Optional reasoning/thinking text.
    ReasoningDelta {
        /// Incremental text. Empty text signals observed reasoning activity
        /// when the provider exposes only opaque continuation state.
        text: String,
    },
    /// Final assistant text for the turn.
    FinalMessage {
        /// Complete final text.
        text: String,
    },
    /// Runtime-owned tool/action start.
    ToolCall {
        /// Correlation id.
        call_id: CallId,
        /// Logical/provider action name.
        name: String,
        /// Structured arguments after adapter validation.
        arguments: serde_json::Value,
    },
    /// Runtime-owned tool/action settlement.
    ToolResult {
        /// Correlation id.
        call_id: CallId,
        /// Structured result after adapter validation.
        result: serde_json::Value,
        /// True for denied/failed operations.
        is_error: bool,
    },
    /// Runtime asks heycode for a permission decision.
    PermissionRequested {
        /// Provider-native request id.
        request_id: RuntimeRequestId,
        /// Safe action title.
        action: String,
        /// Safe bounded explanation.
        detail: String,
    },
    /// Runtime asks the human a question.
    QuestionRequested {
        /// Required interaction shape, retained from the provider.
        mode: heycode_core::QuestionMode,
        /// One-based position and total within this provider request.
        progress: (usize, usize),
        /// Provider-native request id.
        request_id: RuntimeRequestId,
        /// Optional short category label.
        header: Option<String>,
        /// Safe question text.
        prompt: String,
        /// Ordered safe choices; empty permits free text.
        choices: Vec<String>,
        /// Explanations aligned one-for-one with `choices`.
        choice_descriptions: Vec<Option<String>>,
    },
    /// Current request budget, independent of billing usage.
    ContextBudgetChanged {
        /// Model capacity, measurement confidence and compaction state.
        budget: heycode_llm::ContextBudget,
    },
    /// Runtime/provider token accounting.
    ///
    /// A runtime turn may contain several inner provider/model steps, so this
    /// phase may repeat and does not by itself settle the turn.
    Usage {
        /// Prompt/completion totals when exposed.
        usage: TokenUsage,
        /// Exact latest-request context evidence when exposed by the runtime.
        context: Option<RuntimeContextUsage>,
    },
    /// One turn settled exactly once.
    TurnFinished {
        /// Provider-native turn id.
        turn: RuntimeTurnId,
        /// Settlement class.
        reason: RuntimeFinishReason,
    },
    /// Safe non-model-visible runtime notice.
    Notice {
        /// Stable adapter-defined code.
        code: String,
        /// Safe user-facing text.
        message: String,
    },
}

impl Debug for RuntimeEventKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContextBudgetChanged { budget } => formatter
                .debug_struct("ContextBudgetChanged")
                .field("budget", budget)
                .finish(),
            Self::SessionReady => formatter.write_str("SessionReady"),
            Self::TurnStarted { .. } => formatter.write_str("TurnStarted { turn: <redacted> }"),
            Self::CommentaryDelta { text } => formatter
                .debug_struct("CommentaryDelta")
                .field("bytes", &text.len())
                .finish(),
            Self::ReasoningDelta { text } => formatter
                .debug_struct("ReasoningDelta")
                .field("bytes", &text.len())
                .finish(),
            Self::FinalMessage { text } => formatter
                .debug_struct("FinalMessage")
                .field("bytes", &text.len())
                .finish(),
            Self::ToolCall { arguments, .. } => formatter
                .debug_struct("ToolCall")
                .field("call_id", &"<redacted>")
                .field("name", &"<redacted>")
                .field("arguments_kind", &json_kind(arguments))
                .finish(),
            Self::ToolResult {
                result, is_error, ..
            } => formatter
                .debug_struct("ToolResult")
                .field("call_id", &"<redacted>")
                .field("result_kind", &json_kind(result))
                .field("is_error", is_error)
                .finish(),
            Self::PermissionRequested { action, detail, .. } => formatter
                .debug_struct("PermissionRequested")
                .field("request_id", &"<redacted>")
                .field("action_bytes", &action.len())
                .field("detail_bytes", &detail.len())
                .finish(),
            Self::QuestionRequested {
                header,
                prompt,
                choices,
                choice_descriptions,
                ..
            } => formatter
                .debug_struct("QuestionRequested")
                .field("request_id", &"<redacted>")
                .field("header_bytes", &header.as_ref().map(String::len))
                .field("prompt_bytes", &prompt.len())
                .field("choice_count", &choices.len())
                .field("description_count", &choice_descriptions.len())
                .finish(),
            Self::Usage { usage, context } => formatter
                .debug_struct("Usage")
                .field("usage", usage)
                .field("context", context)
                .finish(),
            Self::TurnFinished { reason, .. } => formatter
                .debug_struct("TurnFinished")
                .field("turn", &"<redacted>")
                .field("reason", reason)
                .finish(),
            Self::Notice { code, message } => formatter
                .debug_struct("Notice")
                .field("code_bytes", &code.len())
                .field("message_bytes", &message.len())
                .finish(),
        }
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn validate_tools(tools: &[heycode_core::ToolSpec]) -> Result<(), RuntimeContractError> {
    if tools.len() > 256 {
        return Err(RuntimeContractError::invalid(
            "tools",
            "at most 256 unique validated definitions",
        ));
    }
    let mut names = std::collections::BTreeSet::new();
    for tool in tools {
        validate_one_line(&tool.name, "tool name", 128)?;
        if !tool
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || !names.insert(tool.name.as_str())
            || tool.description.trim().is_empty()
            || tool.description.len() > 4 * 1024
            || tool.description.contains('\0')
            || !tool.parameters.is_object()
            || serde_json::to_vec(&tool.parameters).map_or(true, |wire| wire.len() > 64 * 1024)
        {
            return Err(RuntimeContractError::invalid(
                "tools",
                "at most 256 unique validated definitions",
            ));
        }
    }
    Ok(())
}

fn validate_session_id(session_id: &SessionId) -> Result<(), RuntimeContractError> {
    validate_one_line(session_id.as_str(), "heycode session id", 256)
}

fn validate_workspace(workspace: &Path) -> Result<PathBuf, RuntimeContractError> {
    if !workspace.is_absolute() {
        return Err(RuntimeContractError::invalid(
            "workspace",
            "an absolute path",
        ));
    }
    Ok(workspace.to_path_buf())
}

fn validate_one_line(
    value: &str,
    field: &'static str,
    max_bytes: usize,
) -> Result<(), RuntimeContractError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
    {
        return Err(RuntimeContractError::invalid(
            field,
            "trimmed control-free text within its byte limit",
        ));
    }
    Ok(())
}

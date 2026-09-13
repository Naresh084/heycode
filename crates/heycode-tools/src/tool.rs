//! The tool contract every model-callable capability implements.

use heycode_core::ToolSpec;

/// Per-invocation context handed to a tool's [`Tool::run`].
pub struct ToolCtx {
    /// Working directory relative paths resolve against.
    pub cwd: std::path::PathBuf,
    /// Cancellation source owned by this tool invocation.
    pub cancellation: tokio_util::sync::CancellationToken,
}

impl Default for ToolCtx {
    fn default() -> Self {
        Self {
            cwd: std::path::PathBuf::from("."),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }
}

impl ToolCtx {
    /// Set the working directory, keeping other defaults.
    #[must_use]
    pub fn with_cwd(mut self, cwd: std::path::PathBuf) -> Self {
        self.cwd = cwd;
        self
    }
}

/// One model-callable tool: a schema for the model plus the behavior behind it.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// The name, description, and JSON-Schema parameters the model sees.
    fn spec(&self) -> ToolSpec;

    /// Historical names accepted for compatibility, never advertised as extra schemas.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Canonical control that replaces this compatibility schema in model catalogs.
    /// Dispatch, historical aliases and registry ownership remain available. If
    /// the replacement is absent in a partial composition, advertise this tool.
    fn model_replacement(&self) -> Option<&'static str> {
        None
    }

    /// Read-only setup evidence. This must never launch processes, prompt for
    /// permissions, or infer successful execution from configuration alone.
    fn prerequisite_status(&self) -> ToolPrerequisiteStatus {
        ToolPrerequisiteStatus {
            configured: None,
            detail: "Prerequisites are checked per invocation; no setup evidence published".into(),
        }
    }

    /// What running this tool can do to state other calls can observe.
    ///
    /// The scheduler overlaps [`ToolEffect::ReadOnly`] and explicit
    /// [`ToolEffect::Orchestration`] calls; other calls form barriers, so the default is the safe one: a tool
    /// that says nothing serializes. Declaring it here rather than in a list
    /// beside the scheduler is what lets a tool this crate has never heard of
    /// — an extension's, a future built-in — be scheduled correctly.
    fn effect(&self) -> ToolEffect {
        ToolEffect::Mutates
    }

    /// Whether an invocation may be owned by the background job runtime.
    ///
    /// Opting in requires cancellation to release local resources without
    /// detached tasks. Cancellation does not promise rollback of remote effects.
    /// The default denies promotion; adapters must attest this lifecycle contract.
    fn supports_background(&self) -> bool {
        false
    }

    /// Rebind captured local services to a host-created workspace, preserving this
    /// tool's configuration. Tools with no captured filesystem/process service inherit unchanged.
    fn rebind_workspace(
        &self,
        _filesystem: &heycode_exec::FileSystemService,
        _shell: &heycode_exec::ShellService,
    ) -> Option<std::sync::Arc<dyn Tool>> {
        None
    }

    /// Classification applied to successful model-visible output. External
    /// providers override this; ordinary local tools default to none.
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        None
    }

    /// Execute one call with already-validated JSON arguments.
    ///
    /// # Errors
    /// A [`ToolError`] becomes the model-visible failure text; implementers
    /// should write messages that tell the model how to recover.
    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError>;

    /// Execute and retain an optional typed rich-result plane for the durable
    /// Agent owner. Ordinary tools inherit the plain JSON adapter.
    ///
    /// # Errors
    /// Same model-visible failure contract as [`Self::run`].
    async fn run_output(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<crate::ToolOutput, ToolError> {
        self.run(args, cx).await.map(crate::ToolOutput::plain)
    }
    /// Execute with a bounded non-blocking live output observer. Tools without
    /// a streaming plane retain their ordinary result-only contract.
    async fn run_output_observed(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
        _sink: std::sync::Arc<dyn heycode_exec::ProcessOutputSink>,
    ) -> Result<crate::ToolOutput, ToolError> {
        self.run_output(args, cx).await
    }
}

/// Configuration evidence, separate from permission and execution evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolPrerequisiteStatus {
    /// None means the implementation does not publish configuration evidence.
    pub configured: Option<bool>,
    /// Operation-specific limitations or setup instructions, without secrets.
    pub detail: String,
}

/// What one tool call can do to state that other calls can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// Explicit concurrent delegation. Child effects remain guarded and may mutate;
    /// callers coordinate shared-file ownership. This is never a read-only claim.
    Orchestration,
    /// Observes only. Two concurrent invocations can neither see nor corrupt
    /// each other's effects, so a batch may overlap them.
    ReadOnly,
    /// May change files, processes, durable session state or provider state.
    /// Runs as a barrier: nothing overlaps it in either direction.
    Mutates,
}

/// Model-visible tool failure. The message is returned to the model verbatim.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    /// Human/model-readable description of what went wrong and how to retry.
    pub message: String,
}

impl ToolError {
    /// Build an error from any string-like message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

//! O09 providers: the prompt, subagent and MCP-tool handlers.
//!
//! Each provider owns one thing and only one: the map from its domain's
//! outcomes onto the hook vocabulary. The call-out itself is a port
//! (`PromptHookRunner`, `SubagentHookLauncher`, `McpToolHookCaller`) that the
//! crate owning that capability implements, because `heycode-hooks` sits below
//! `heycode-agent` and `heycode-mcp` in the dependency table and reaching up would be
//! a cycle. This is the same shape `heycode-exec` uses for `ShellBackend`, and it
//! keeps the interesting part — the failure policy — here, where it is tested.
//!
//! The maps are not mechanical. Two of them decide something a caller could
//! plausibly get wrong:
//!
//! * A subagent delegation refused by a depth or authority gate is **not** a
//!   hook refusal. The hook never reached a decision; a gate stopped it from
//!   running. Reporting that as "the hook said no" would let an unrelated limit
//!   silently veto operations.
//! * An MCP handler **cannot refuse at all**. Its result is content authored by
//!   whoever runs the server, which heycode labels as data and explicitly not as
//!   authorization. A veto derived from it would be authorization.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::HookFault;
use crate::handler::{HookAction, HookAnswer, HookHandler, HookHandlerKind, HookInvocation};

/// Runs one prompt through the model on a hook's behalf.
///
/// Implementations must obtain the [`crate::HookDecision`] from a structured
/// channel — a tool call, a constrained schema — and never by pattern-matching
/// the model's prose. A decision parsed out of free text is a decision an
/// untrusted payload can write.
#[async_trait]
pub trait PromptHookRunner: Send + Sync {
    /// Run `prompt` and report what it decided.
    ///
    /// # Errors
    /// Provider failure, refusal to serve, or cancellation. A model that
    /// answers "no" is not an error: that is `HookAnswer::refuse`.
    async fn run(
        &self,
        prompt: &str,
        cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault>;
}

/// The prompt hook provider.
pub struct PromptHookHandler {
    runner: Arc<dyn PromptHookRunner>,
}

impl PromptHookHandler {
    /// A prompt handler over one model runner.
    #[must_use]
    pub fn new(runner: Arc<dyn PromptHookRunner>) -> Self {
        Self { runner }
    }
}

impl std::fmt::Debug for PromptHookHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PromptHookHandler").finish()
    }
}

#[async_trait]
impl HookHandler for PromptHookHandler {
    fn kind(&self) -> HookHandlerKind {
        HookHandlerKind::Prompt
    }

    async fn invoke(
        &self,
        invocation: HookInvocation,
        cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault> {
        let HookAction::Prompt(instruction) = invocation.action() else {
            return Err(HookFault::Unlaunchable);
        };
        let prompt = compose(instruction, invocation.body());
        self.runner.run(&prompt, cancellation).await
    }
}

/// Why one delegation did not settle successfully.
///
/// Mirrors `heycode_agent::SubagentErrorCode` variant for variant, so the adapter
/// that owns both types is a total map with no default arm. `heycode-hooks` cannot
/// name that type directly without depending upward on `heycode-agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubagentHookFailure {
    /// The provider does not prove the requested seed or continuation.
    Unsupported,
    /// A nesting limit or authority gate refused the delegation.
    Refused,
    /// The named agent or child is unknown.
    Unknown,
    /// The caller cancelled before the child settled.
    Cancelled,
    /// The child run failed.
    Failed,
}

impl SubagentHookFailure {
    /// The hook fault this delegation failure becomes.
    ///
    /// `Refused` maps to [`HookFault::Unlaunchable`], not to a hook refusal.
    /// A gate refusing to *start* the child is the hook failing to run: the
    /// hook itself never expressed an opinion about the surrounded operation,
    /// and letting a depth limit read as a veto would hand any unrelated
    /// authority gate a silent kill switch over hooked operations.
    #[must_use]
    pub const fn into_fault(self) -> HookFault {
        match self {
            Self::Unsupported | Self::Refused | Self::Unknown => HookFault::Unlaunchable,
            Self::Cancelled => HookFault::Cancelled,
            Self::Failed => HookFault::Failed,
        }
    }
}

impl std::fmt::Display for SubagentHookFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "the delegation provider does not support this request",
            Self::Refused => "an authority gate refused the delegation",
            Self::Unknown => "the delegated agent is unknown",
            Self::Cancelled => "the delegation was cancelled",
            Self::Failed => "the delegated child failed",
        })
    }
}

impl std::error::Error for SubagentHookFailure {}

/// Delegates to a subagent on a hook's behalf.
///
/// The same structured-decision obligation as [`PromptHookRunner`] applies: a
/// child's final text is model output, so a refusal must come from a typed
/// channel and never from reading the child's prose.
#[async_trait]
pub trait SubagentHookLauncher: Send + Sync {
    /// Delegate `prompt` to `agent` and report what the child decided.
    ///
    /// # Errors
    /// Any reason the delegation did not settle with a decision.
    async fn launch(
        &self,
        agent: &str,
        prompt: &str,
        cancellation: CancellationToken,
    ) -> Result<HookAnswer, SubagentHookFailure>;
}

/// The subagent hook provider.
pub struct SubagentHookHandler {
    launcher: Arc<dyn SubagentHookLauncher>,
}

impl SubagentHookHandler {
    /// A subagent handler over one delegation launcher.
    #[must_use]
    pub fn new(launcher: Arc<dyn SubagentHookLauncher>) -> Self {
        Self { launcher }
    }
}

impl std::fmt::Debug for SubagentHookHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SubagentHookHandler").finish()
    }
}

#[async_trait]
impl HookHandler for SubagentHookHandler {
    fn kind(&self) -> HookHandlerKind {
        HookHandlerKind::Subagent
    }

    async fn invoke(
        &self,
        invocation: HookInvocation,
        cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault> {
        let HookAction::Subagent { agent, prompt } = invocation.action() else {
            return Err(HookFault::Unlaunchable);
        };
        let prompt = compose(prompt, invocation.body());
        self.launcher
            .launch(agent, &prompt, cancellation)
            .await
            .map_err(SubagentHookFailure::into_fault)
    }
}

/// One MCP `tools/call` result, already rendered for a reader.
#[derive(Clone, PartialEq, Eq)]
pub struct McpHookResult {
    text: String,
    is_error: bool,
}

impl McpHookResult {
    /// A result the server reported as successful.
    #[must_use]
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    /// A result the server flagged with `isError`.
    #[must_use]
    pub fn failed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }

    /// Whether the server reported the tool call as failed.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.is_error
    }
}

impl std::fmt::Debug for McpHookResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHookResult")
            .field("text_len", &self.text.len())
            .field("is_error", &self.is_error)
            .finish()
    }
}

/// Calls one MCP tool on a hook's behalf.
#[async_trait]
pub trait McpToolHookCaller: Send + Sync {
    /// Call `tool` on `server` with `arguments`.
    ///
    /// # Errors
    /// Transport, protocol, or cancellation failure. A tool the server ran and
    /// reported as failed is `Ok` with [`McpHookResult::is_error`] set — the
    /// call succeeded, the tool did not.
    async fn call(
        &self,
        server: &str,
        tool: &str,
        arguments: &serde_json::Value,
        cancellation: CancellationToken,
    ) -> Result<McpHookResult, HookFault>;
}

/// The MCP-tool hook provider.
pub struct McpToolHookHandler {
    caller: Arc<dyn McpToolHookCaller>,
}

impl McpToolHookHandler {
    /// An MCP handler over one tool caller.
    #[must_use]
    pub fn new(caller: Arc<dyn McpToolHookCaller>) -> Self {
        Self { caller }
    }
}

impl std::fmt::Debug for McpToolHookHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("McpToolHookHandler").finish()
    }
}

#[async_trait]
impl HookHandler for McpToolHookHandler {
    fn kind(&self) -> HookHandlerKind {
        HookHandlerKind::McpTool
    }

    /// Answers `Allow` or faults, and never refuses.
    ///
    /// A server-reported tool failure is [`HookFault::Failed`]: the tool broke,
    /// which is not the same as the tool objecting. And a successful call
    /// contributes its text as data — the service labels it
    /// [`heycode_core::UntrustedContentSource::Mcp`] on the way out.
    async fn invoke(
        &self,
        invocation: HookInvocation,
        cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault> {
        let HookAction::McpTool {
            server,
            tool,
            arguments,
        } = invocation.action()
        else {
            return Err(HookFault::Unlaunchable);
        };
        let result = self
            .caller
            .call(server, tool, arguments, cancellation)
            .await?;
        if result.is_error {
            return Err(HookFault::Failed);
        }
        Ok(HookAnswer::allow_with(result.text))
    }
}

/// Join a hook's configured instruction to the operation's content.
fn compose(instruction: &str, body: &str) -> String {
    if body.is_empty() {
        instruction.to_owned()
    } else {
        format!("{instruction}\n\n{body}")
    }
}

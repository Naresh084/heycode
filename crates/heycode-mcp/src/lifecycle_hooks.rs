//! Product hook port at the exact MCP tool-call lifecycle point.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::McpServerId;

/// MCP lifecycle phase exposed to an O09 product adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpLifecycleHookPhase {
    /// Before approval and `tools/call` transport.
    Pre,
    /// After a complete MCP result parsed successfully.
    Post,
}

/// One exact MCP lifecycle hook invocation.
#[derive(Clone, PartialEq)]
pub struct McpLifecycleHookRequest {
    phase: McpLifecycleHookPhase,
    server: McpServerId,
    tool: String,
    qualified_tool: String,
    arguments: Option<serde_json::Value>,
    result_text: Option<String>,
    result_is_error: Option<bool>,
}

impl McpLifecycleHookRequest {
    pub(crate) fn pre(
        server: McpServerId,
        tool: String,
        qualified_tool: String,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            phase: McpLifecycleHookPhase::Pre,
            server,
            tool,
            qualified_tool,
            arguments: Some(arguments),
            result_text: None,
            result_is_error: None,
        }
    }

    pub(crate) fn post(
        server: McpServerId,
        tool: String,
        qualified_tool: String,
        result_text: String,
        result_is_error: bool,
    ) -> Self {
        Self {
            phase: McpLifecycleHookPhase::Post,
            server,
            tool,
            qualified_tool,
            arguments: None,
            result_text: Some(result_text),
            result_is_error: Some(result_is_error),
        }
    }

    /// Lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> McpLifecycleHookPhase {
        self.phase
    }

    /// Exact configured server.
    #[must_use]
    pub const fn server(&self) -> &McpServerId {
        &self.server
    }

    /// Unqualified remote tool name.
    #[must_use]
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// Public model-facing tool name.
    #[must_use]
    pub fn qualified_tool(&self) -> &str {
        &self.qualified_tool
    }

    /// Exact pre-call arguments, absent after settlement.
    #[must_use]
    pub const fn arguments(&self) -> Option<&serde_json::Value> {
        self.arguments.as_ref()
    }

    /// Complete rendered post-call result, absent before transport.
    #[must_use]
    pub fn result_text(&self) -> Option<&str> {
        self.result_text.as_deref()
    }

    /// Server-reported result status, present only post-call.
    #[must_use]
    pub const fn result_is_error(&self) -> Option<bool> {
        self.result_is_error
    }
}

impl std::fmt::Debug for McpLifecycleHookRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpLifecycleHookRequest")
            .field("phase", &self.phase)
            .field("server", &self.server)
            .field("tool", &self.tool)
            .field("qualified_tool", &self.qualified_tool)
            .field(
                "argument_bytes",
                &self
                    .arguments
                    .as_ref()
                    .and_then(|value| serde_json::to_vec(value).ok())
                    .map(|bytes| bytes.len()),
            )
            .field("result_bytes", &self.result_text.as_ref().map(String::len))
            .field("result_is_error", &self.result_is_error)
            .finish()
    }
}

/// Whether an entitled pre-hook permits the MCP call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpLifecycleHookDecision {
    /// Continue the MCP operation.
    Proceed,
    /// Refuse before approval or transport.
    Refuse,
}

/// Body-free aggregate of one MCP hook firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpLifecycleHookReport {
    decision: McpLifecycleHookDecision,
    faults: u32,
}

impl McpLifecycleHookReport {
    /// Continue with no faults.
    #[must_use]
    pub const fn proceed() -> Self {
        Self {
            decision: McpLifecycleHookDecision::Proceed,
            faults: 0,
        }
    }

    /// Refuse with a count of earlier non-vetoing faults.
    #[must_use]
    pub const fn refuse(faults: u32) -> Self {
        Self {
            decision: McpLifecycleHookDecision::Refuse,
            faults,
        }
    }

    /// Proceed while retaining non-vetoing fault count.
    #[must_use]
    pub const fn proceed_with_faults(faults: u32) -> Self {
        Self {
            decision: McpLifecycleHookDecision::Proceed,
            faults,
        }
    }

    /// Effective decision.
    #[must_use]
    pub const fn decision(self) -> McpLifecycleHookDecision {
        self.decision
    }

    /// Number of broken/refused-ineligible/commit-failed hooks.
    #[must_use]
    pub const fn faults(self) -> u32 {
        self.faults
    }
}

/// Product adapter for O09 at MCP's exact call site.
///
/// Contribution text never returns through this port. The adapter commits it
/// to the session, and the next Agent request obtains it from durable
/// projection.
#[async_trait]
pub trait McpLifecycleHookPort: Send + Sync {
    /// Run matching hooks under the tool operation's cancellation token.
    async fn run(
        &self,
        request: McpLifecycleHookRequest,
        cancellation: CancellationToken,
    ) -> McpLifecycleHookReport;
}

//! heycode-tools — the model-callable tool capability.
//!
//! Owns the [`Tool`] trait, the insertion-ordered [`ToolRegistry`], the single
//! guarded execution pipeline ([`execute_tool`]), and the built-in tools:
//! `read`, `write`, `edit`, `bash`, `glob`, `grep`, `todo_write`, plus
//! `web_fetch` / `web_search` when `ToolsConfig::web_enabled` (default true).
//!
//! Pipeline: resolve the tool → run the [`SEAM_PRE_TOOL`] waterfall (a `Deny`
//! short-circuits and the tool never runs) → `tool.run()`. Logging is the
//! agent's job; tools never touch the session. The pre-tool seam is exposed
//! as a service so approval and guard plugins can mount layers without this
//! crate knowing them.

pub mod builtins;
mod config;
mod exec;
pub mod interactive;
mod lsp;
mod plugin;
mod registry;
mod rich;
mod sandbox;
mod tool;

/// Model-callable tool registry service.
pub const SERVICE_TOOLS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("tools");

pub use builtins::web::{WebFetch, WebSearch};
pub use config::ToolsConfig;
pub use exec::{
    PreToolDecision, SEAM_PRE_TOOL, ToolCallInput, ToolOutcome, Verdict, execute_tool,
    execute_tool_observed, run_tool,
};
pub use heycode_exec::ObservationLog;
pub use lsp::{lsp_tools, lsp_tools_plugin};
pub use plugin::{builtin_tools, tools_plugin};
pub use registry::{OwnedToolRegistration, RegisterError, ToolRegistry};
pub use rich::{
    PendingRichToolResult, PendingRichToolResultError, PendingToolMedia, PendingToolResultBlock,
    ToolOutput,
};
pub use sandbox::{Sandbox, SandboxError, SandboxMode, SandboxPolicy};
pub use tool::{Tool, ToolCtx, ToolEffect, ToolError, ToolPrerequisiteStatus};

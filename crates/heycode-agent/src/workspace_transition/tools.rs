//! Model adapters over the exact current-session workspace owner.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::ToolSpec;
use heycode_tools::{Tool, ToolCtx, ToolEffect, ToolError};
use serde_json::{Value, json};

use super::{WorkspaceSnapshot, WorkspaceTransitionOrigin, WorkspaceTransitionService};

/// Enter a managed, retained worktree for the current native session.
pub struct EnterWorktreeTool(pub Arc<WorkspaceTransitionService>);
impl EnterWorktreeTool {
    /// Bind the exact composed workspace owner.
    pub fn new(service: Arc<WorkspaceTransitionService>) -> Self {
        Self(service)
    }
}

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "enter_worktree".into(),
            description: "Move this native session into a managed detached Git worktree containing the current tracked edits and bounded untracked files. Rebinds actual file roots, shell cwd and agent scope. Active jobs, terminals, delegated runtimes or resumable children prevent entry. The source checkout is untouched; nested entry is refused.".into(),
            parameters: json!({"type":"object", "properties":{}, "additionalProperties":false}),
        }
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["EnterWorktree"]
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::Mutates
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        empty_args(args)?;
        let snapshot = self
            .0
            .enter_worktree(
                WorkspaceTransitionOrigin::ModelTool,
                cx.cancellation.clone(),
            )
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        output(snapshot, "entered")
    }
}

/// Restore the saved session authority while retaining worktree files.
pub struct ExitWorktreeTool(pub Arc<WorkspaceTransitionService>);
impl ExitWorktreeTool {
    /// Bind the exact composed workspace owner.
    pub fn new(service: Arc<WorkspaceTransitionService>) -> Self {
        Self(service)
    }
}

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exit_worktree".into(),
            description: "Return this native session to the cwd and file roots saved before enter_worktree. Always retains the worktree on disk, including clean worktrees. Does not merge, delete files, switch the original checkout, or discard changes. Refuses while other jobs or runtimes own workspace state.".into(),
            parameters: json!({"type":"object", "properties":{}, "additionalProperties":false}),
        }
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["ExitWorktree"]
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::Mutates
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        empty_args(args)?;
        let snapshot = self
            .0
            .exit_worktree(
                WorkspaceTransitionOrigin::ModelTool,
                cx.cancellation.clone(),
            )
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        output(snapshot, "exited_retained")
    }
}

fn empty_args(args: Value) -> Result<(), ToolError> {
    if !args.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(ToolError::new(
            "Worktree transitions take no arguments; cleanup and force options are intentionally unsupported",
        ));
    }
    Ok(())
}
fn output(snapshot: WorkspaceSnapshot, status: &str) -> Result<Value, ToolError> {
    Ok(json!({
        "status": status,
        "revision": snapshot.revision,
        "cwd": snapshot.cwd,
        "roots": snapshot.roots.iter().map(|root| json!({"path": root.path, "read_only": root.read_only})).collect::<Vec<_>>(),
        "worktree": snapshot.worktree,
        "retained_worktrees": snapshot.retained_worktrees,
        "recovery": snapshot.pending_recovery,
    }))
}

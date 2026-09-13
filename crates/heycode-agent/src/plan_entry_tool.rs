//! Model-facing entry into the existing durable Plan mode.
//!
//! The tool is deliberately only an adapter over [`crate::PlanMode::set`].
//! Durable logging, mid-turn queuing, active-work settlement, delegated-runtime
//! refusal and fail-closed mutation guards remain owned by Plan mode itself.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::ToolSpec;
use heycode_tools::{Tool, ToolCtx, ToolEffect, ToolError};

use crate::{PlanMode, PlanSelection};

/// Enter the session's durable, enforced Plan mode.
pub struct EnterPlanModeTool {
    plan: Arc<PlanMode>,
}

impl EnterPlanModeTool {
    /// Bind the tool to the exact Plan owner for this session.
    #[must_use]
    pub fn new(plan: Arc<PlanMode>) -> Self {
        Self { plan }
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "enter_plan_mode".into(),
            description: "Enter durable Plan mode before researching and proposing a multi-step implementation. Plan mode blocks mutations and delegation until the full plan is explicitly reviewed through exit_plan_mode.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Mutates
    }

    async fn run(
        &self,
        _args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        if cx.cancellation.is_cancelled() {
            return Err(ToolError::new("Plan entry was cancelled"));
        }
        let selection = tokio::select! {
            biased;
            () = cx.cancellation.cancelled() => {
                return Err(ToolError::new("Plan entry was cancelled"));
            }
            selection = self.plan.set(true) => selection.map_err(|error| ToolError::new(error.to_string()))?,
        };
        let status = match selection {
            PlanSelection::Committed => "committed",
            PlanSelection::Queued => "queued",
            PlanSelection::Cancelled => "cancelled",
            PlanSelection::Noop => "already_active",
        };
        Ok(serde_json::json!({
            "status": status,
            "plan_mode": self.plan.active(),
            "pending": self.plan.pending(),
            "next": "Research without mutations, then call exit_plan_mode with the full Markdown plan for explicit review."
        }))
    }
}

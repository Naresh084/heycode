//! Explicit workflow presentation metadata and durable native task observations.
use serde::{Deserialize, Serialize};

/// One declared phase, in the exact planned order of the definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPhase {
    /// Stable phase key referenced by steps.
    pub id: String,
    /// Explicit human-facing title.
    pub title: String,
}

/// Measured provider accounting. Zero reported requests means usage is unavailable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowUsage {
    /// Committed responses with reported usage.
    pub reported_requests: u32,
    /// Committed responses whose provider omitted usage.
    pub unreported_requests: u32,
    /// Sum of reported prompt tokens, never estimated.
    pub prompt_tokens: u64,
    /// Sum of reported completion tokens, never estimated.
    pub completion_tokens: u64,
}
impl WorkflowUsage {
    /// Combine measured accounting without inventing missing responses.
    pub fn add(&mut self, other: Self) {
        self.reported_requests = self
            .reported_requests
            .saturating_add(other.reported_requests);
        self.unreported_requests = self
            .unreported_requests
            .saturating_add(other.unreported_requests);
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
    }
}

/// Native task lifecycle captured by the host, without a live Agent dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAgentState {
    /// Admitted but waiting for execution capacity.
    Queued,
    /// Native startup or inference is active.
    Running,
    /// Cancellation requested but not settled.
    Cancelling,
    /// Retained child waiting for input.
    Idle,
    /// One-shot result committed.
    Completed,
    /// Execution failed.
    Failed,
    /// Cancellation settled.
    Cancelled,
    /// Conversation closed.
    Closed,
    /// Process execution did not survive.
    Interrupted,
}

/// A host-authored correlation and latest observation for one actual native task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAgentRecord {
    /// Declared graph node, never inferred from labels.
    pub node_id: String,
    /// Exact graph node attempt that admitted this child.
    pub node_attempt: u32,
    /// Actual owner session journal receiving this observation.
    pub owner_session_id: String,
    /// Stable public task ID minted before provider startup.
    pub task_id: String,
    /// Actual durable native conversation, published before first inference.
    pub session_id: Option<String>,
    /// Correlated task job, if that native operation uses one.
    pub job_id: Option<String>,
    /// Declared step label used for native task admission.
    pub label: String,
    /// Bounded exact resolved prompt; the conversation holds its complete text.
    pub prompt: String,
    /// Prompt exceeded the bounded observer representation.
    pub prompt_truncated: bool,
    /// Task revision from the authoritative registry.
    pub revision: u64,
    /// Actual lifecycle.
    pub state: WorkflowAgentState,
    /// Actual task admission timestamp.
    pub created_at_ms: i64,
    /// Actual most recent task revision timestamp.
    pub updated_at_ms: i64,
    /// Bounded committed result or error, retained after the handle closes.
    pub summary: String,
    /// Result exceeded the observer representation.
    pub summary_truncated: bool,
    /// Provider-reported totals with missing-response counts.
    pub usage: WorkflowUsage,
}

/// Exact coordinator admitted for one workflow attempt, retained for inbox replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowJobRecord {
    /// Workflow attempt that admitted this job.
    pub attempt: u32,
    /// Actual canonical JobRegistry ID.
    pub job_id: String,
    /// Durable admission commit time.
    pub admitted_at_ms: i64,
    /// Durable attempt End time, absent until an End was committed.
    pub settled_at_ms: Option<i64>,
    /// Exact attempt outcome; Paused corresponds to a routinely completed coordinator.
    pub outcome: Option<crate::WorkflowOutcome>,
}

//! The sole adapter from the owned workflow service to the native workspace.
use crate::{
    task_console::{TaskKey, TaskOutputKind, TaskSource, TaskStatus},
    workflow_console::*,
};
use async_trait::async_trait;
use heycode_agent::{
    SubagentAuthority, TaskState, WorkflowService, WorkflowTiming, WorkflowUsage, WorkflowViewState,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) struct ServiceWorkflowSource {
    service: Arc<WorkflowService>,
    authority: SubagentAuthority,
    tasks: Arc<dyn TaskSource>,
}
impl ServiceWorkflowSource {
    pub(crate) fn new(
        service: Arc<WorkflowService>,
        authority: SubagentAuthority,
        tasks: Arc<dyn TaskSource>,
    ) -> Self {
        Self {
            service,
            authority,
            tasks,
        }
    }
}
fn status(state: WorkflowViewState) -> WorkflowStatus {
    match state {
        WorkflowViewState::Queued => WorkflowStatus::Queued,
        WorkflowViewState::Running => WorkflowStatus::Running,
        WorkflowViewState::Pausing => WorkflowStatus::Pausing,
        WorkflowViewState::Paused => WorkflowStatus::Paused,
        WorkflowViewState::Completed => WorkflowStatus::Completed,
        WorkflowViewState::Failed => WorkflowStatus::Failed,
        WorkflowViewState::Cancelled => WorkflowStatus::Cancelled,
        WorkflowViewState::Interrupted => WorkflowStatus::Interrupted,
        WorkflowViewState::Skipped => WorkflowStatus::Skipped,
    }
}
fn measured(timing: &WorkflowTiming, usage: &WorkflowUsage) -> WorkflowStats {
    WorkflowStats {
        elapsed_ms: timing.elapsed_ms,
        input_tokens: (usage.reported_requests > 0).then_some(usage.prompt_tokens),
        output_tokens: (usage.reported_requests > 0).then_some(usage.completion_tokens),
        partial_usage: usage.unreported_requests > 0,
    }
}
fn task_status(state: TaskState, node: Option<WorkflowViewState>) -> WorkflowStatus {
    match state {
        TaskState::Queued => WorkflowStatus::Queued,
        TaskState::Running => WorkflowStatus::Running,
        TaskState::Cancelling => WorkflowStatus::Pausing,
        TaskState::Idle => WorkflowStatus::Waiting,
        TaskState::Completed => WorkflowStatus::Completed,
        TaskState::Failed => WorkflowStatus::Failed,
        TaskState::Cancelled => WorkflowStatus::Cancelled,
        TaskState::Interrupted => WorkflowStatus::Interrupted,
        // A closed handle alone is never evidence of successful work.
        TaskState::Closed => node
            .map(status)
            .filter(|s| s.finished())
            .unwrap_or(WorkflowStatus::Interrupted),
    }
}
#[async_trait]
impl WorkflowSource for ServiceWorkflowSource {
    fn snapshot(&self) -> Result<Vec<WorkflowRun>, String> {
        let views = self
            .service
            .views_for(&self.authority)
            .map_err(|e| e.to_string())?;
        let tasks = self.tasks.snapshot().unwrap_or_default();
        Ok(views
            .into_iter()
            .map(|run| WorkflowRun {
                job_id: run.job_id.as_ref().map(ToString::to_string),
                jobs: run
                    .jobs
                    .iter()
                    .map(|job| WorkflowJob {
                        id: job.job_id.to_string(),
                        routine_notice: matches!(
                            job.outcome,
                            Some(
                                heycode_session::WorkflowOutcome::Completed
                                    | heycode_session::WorkflowOutcome::Paused
                                    | heycode_session::WorkflowOutcome::Cancelled
                            )
                        ),
                    })
                    .collect(),
                id: run.run_id.as_str().to_owned(),
                title: run.title,
                description: run.description,
                status: status(run.state),
                stats: measured(&run.timing, &run.usage),
                controls: WorkflowControls {
                    pause: run.controls.can_pause,
                    resume: run.controls.can_resume,
                    stop: run.controls.can_cancel,
                },
                phases: run
                    .phases
                    .into_iter()
                    .map(|phase| {
                        let agents = phase
                            .agents
                            .into_iter()
                            .map(|agent| {
                                let key = TaskKey(format!("child:{}", agent.task_id));
                                let task = tasks.iter().find(|task| task.key == key);
                                let node = phase.nodes.iter().find(|node| {
                                    node.id == agent.node_id && node.attempt == agent.node_attempt
                                });
                                let mut state =
                                    task_status(agent.state, node.map(|node| node.state));
                                if state.active()
                                    && task.is_some_and(|task| task.status == TaskStatus::Waiting)
                                {
                                    state = WorkflowStatus::Waiting;
                                }
                                let mut summary = agent.summary;
                                let mut output_truncated =
                                    agent.summary_truncated || agent.prompt_truncated;
                                if summary.is_empty()
                                    && state.active()
                                    && let Ok(page) = self.tasks.output(&key, None, 12)
                                {
                                    summary = page
                                        .events
                                        .into_iter()
                                        .filter_map(|event| match event.kind {
                                            TaskOutputKind::Text(text) => Some(text),
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    output_truncated |= page.truncated || page.has_older;
                                }
                                WorkflowAgent {
                                    task_id: agent.task_id,
                                    node_id: agent.node_id,
                                    label: agent.label,
                                    status: state,
                                    assignment: agent.prompt,
                                    summary,
                                    activity: task
                                        .and_then(|task| task.telemetry.current_tool.clone())
                                        .map(|tool| format!("Using {tool}")),
                                    stats: measured(&agent.timing, &agent.usage),
                                    session_id: agent.session_id,
                                    output_truncated,
                                }
                            })
                            .collect();
                        WorkflowPhase {
                            id: phase.id,
                            title: phase.title,
                            status: status(phase.state),
                            agents,
                            stats: measured(&phase.timing, &phase.usage),
                            nodes: phase
                                .nodes
                                .into_iter()
                                .map(|node| WorkflowNode {
                                    id: node.id,
                                    label: node.label,
                                    status: status(node.state),
                                    kind: match node.kind {
                                        heycode_agent::WorkflowNodeKind::Agent => "agent",
                                        heycode_agent::WorkflowNodeKind::Tool => "tool",
                                        heycode_agent::WorkflowNodeKind::Delay => "delay",
                                        heycode_agent::WorkflowNodeKind::Emit => "step",
                                    }
                                    .into(),
                                    result: node
                                        .result
                                        .and_then(|value| value.as_str().map(str::to_owned)),
                                })
                                .collect(),
                        }
                    })
                    .collect(),
            })
            .collect())
    }
    async fn execute(
        &self,
        action: WorkflowAction,
        cancellation: CancellationToken,
    ) -> Result<String, String> {
        if cancellation.is_cancelled() {
            return Err("Workflow workspace closed".into());
        }
        let id = heycode_session::WorkflowRunId::new(action.run_id).map_err(|e| e.to_string())?;
        match action.kind {
            WorkflowActionKind::Pause => self
                .service
                .pause_for(&self.authority, &id)
                .map(|()| "Pause requested · admitted work will settle first".into()),
            WorkflowActionKind::Resume => self
                .service
                .resume_for(&self.authority, &id)
                .map(|_| "Workflow resumed".into()),
            WorkflowActionKind::Stop => self
                .service
                .cancel_for(&self.authority, &id)
                .map(|_| "Stop requested · checking settlement".into()),
        }
        .map_err(|e| e.to_string())
    }
}

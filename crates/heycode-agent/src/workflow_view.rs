//! Read-only workflow progress from explicit definitions and committed observations.
use std::collections::{BTreeMap, BTreeSet};

use heycode_session::{
    Session, SessionEventKind, WorkflowAction, WorkflowChange, WorkflowNodeState, WorkflowOutcome,
    WorkflowRunId, WorkflowState, project_workflows,
};
use serde::{Deserialize, Serialize};

use crate::{JobId, TaskState, WorkflowError};
pub use heycode_session::WorkflowUsage;

/// Current workflow/phase/node state. Completion always requires actual settled work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowViewState {
    /// Declared work waiting to run.
    Queued,
    /// Work is currently executing.
    Running,
    /// A pause was requested; admitted work is settling.
    Pausing,
    /// Stopped at a durable safe boundary.
    Paused,
    /// All required work completed successfully.
    Completed,
    /// Work failed or a required dependency failed.
    Failed,
    /// Owner cancellation ended this work.
    Cancelled,
    /// No live execution owns incomplete recorded work.
    Interrupted,
    /// Execution was explicitly skipped by the graph.
    Skipped,
}
/// Declared action type; ordinary steps must never be rendered as invented agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeKind {
    /// Deterministic JSON result.
    Emit,
    /// Cancellable timer.
    Delay,
    /// Actual guarded tool call.
    Tool,
    /// Actual native child delegation.
    Agent,
}
/// Exact progress counts over declared nodes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowCounts {
    /// Number of declared nodes.
    pub total: u32,
    /// Completed, skipped, failed, or cancelled nodes.
    pub settled: u32,
    /// Nodes that have not executed or are paused/interrupted.
    pub queued: u32,
    /// Nodes currently executing.
    pub running: u32,
    /// Nodes with committed successful results.
    pub completed: u32,
    /// Nodes explicitly skipped by their condition or dependency.
    pub skipped: u32,
    /// Nodes with committed failed execution.
    pub failed: u32,
    /// Nodes stopped by cancellation.
    pub cancelled: u32,
}
impl WorkflowCounts {
    fn count(&mut self, state: WorkflowViewState) {
        use WorkflowViewState as S;
        self.total += 1;
        match state {
            S::Queued | S::Paused | S::Interrupted => self.queued += 1,
            S::Running | S::Pausing => self.running += 1,
            S::Completed => {
                self.completed += 1;
                self.settled += 1;
            }
            S::Skipped => {
                self.skipped += 1;
                self.settled += 1;
            }
            S::Failed => {
                self.failed += 1;
                self.settled += 1;
            }
            S::Cancelled => {
                self.cancelled += 1;
                self.settled += 1;
            }
        }
    }
}
/// Actual commit-time bounds and active elapsed time, excluding durable pauses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTiming {
    /// First actual start commit; absent for work that never ran.
    pub started_at_ms: Option<i64>,
    /// Actual settlement commit; absent while unfinished.
    pub finished_at_ms: Option<i64>,
    /// Measured active wall time, excluding pauses; unknown before start or after an unknown interruption.
    pub elapsed_ms: Option<u64>,
}
/// Current safe controls from the owning service. History alone grants no controls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowControls {
    /// A live run can accept a cooperative pause.
    pub can_pause: bool,
    /// An inactive run has safe, resumable checkpoints.
    pub can_resume: bool,
    /// Owner can stop the active or resumable run without replay.
    pub can_cancel: bool,
}
/// One actual native task, retained in the workflow journal after its handle closes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAgentView {
    /// Stable public native task ID; never a backend ID.
    pub task_id: String,
    /// Actual durable native session ID once startup publishes it.
    pub session_id: Option<String>,
    /// Actual owned background job, when present.
    pub job_id: Option<JobId>,
    /// Exact declaring graph node ID.
    pub node_id: String,
    /// Exact node attempt that admitted this task.
    pub node_attempt: u32,
    /// Exact declared step or task label.
    pub label: String,
    /// Authoritative lifecycle for this item.
    pub state: TaskState,
    /// Exact original prompt or binding; agent rows contain the resolved text.
    pub prompt: String,
    /// The observer prompt reached its bounded retention limit.
    pub prompt_truncated: bool,
    /// Committed result or error retained after handle closure.
    pub summary: String,
    /// The committed output exceeded retention.
    pub summary_truncated: bool,
    /// Actual task admission time since the Unix epoch.
    pub created_at_ms: i64,
    /// Actual most recent task revision time since the Unix epoch.
    pub updated_at_ms: i64,
    /// Measured commit-time bounds and active elapsed time.
    pub timing: WorkflowTiming,
    /// Provider-reported accounting; no estimates.
    pub usage: WorkflowUsage,
}
/// One declared step with its latest committed attempt and result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNodeView {
    /// Stable declared phase or node ID.
    pub id: String,
    /// Exact declared step or task label.
    pub label: String,
    /// Explicit dependency node IDs.
    pub depends_on: Vec<String>,
    /// Declared action category.
    pub kind: WorkflowNodeKind,
    /// Authoritative lifecycle for this item.
    pub state: WorkflowViewState,
    /// Current durable attempt number.
    pub attempt: u32,
    /// Latest committed result or error; absent before settlement.
    pub result: Option<serde_json::Value>,
    /// Exact original prompt or binding; agent rows contain the resolved text.
    pub prompt: Option<serde_json::Value>,
    /// Measured commit-time bounds and active elapsed time.
    pub timing: WorkflowTiming,
}
/// One explicit phase in declared order; absent metadata maps one phase per step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPhaseView {
    /// Stable declared phase or node ID.
    pub id: String,
    /// Explicit declared human title.
    pub title: String,
    /// Declared steps in definition order.
    pub nodes: Vec<WorkflowNodeView>,
    /// Actual admitted native tasks, ordered by node then attempt.
    pub agents: Vec<WorkflowAgentView>,
    /// Authoritative lifecycle for this item.
    pub state: WorkflowViewState,
    /// Progress over declared steps.
    pub counts: WorkflowCounts,
    /// Measured commit-time bounds and active elapsed time.
    pub timing: WorkflowTiming,
    /// Provider-reported accounting; no estimates.
    pub usage: WorkflowUsage,
}
/// Durable association for one actual workflow coordinator attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowJobView {
    /// Workflow attempt that admitted this coordinator.
    pub attempt: u32,
    /// Exact job ID usable for owner-local InboxSource::Job matching.
    pub job_id: JobId,
    /// Actual durable admission time.
    pub admitted_at_ms: i64,
    /// Actual attempt End time; absent while unknown or unfinished.
    pub settled_at_ms: Option<i64>,
    /// Exact attempt End outcome, committed before its job inbox notice.
    pub outcome: Option<WorkflowOutcome>,
}

/// An owned, cloneable snapshot. Reading it never executes or resumes a workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowView {
    /// Exact current and historical coordinator attempts; old logs may have none.
    #[serde(default)]
    pub jobs: Vec<WorkflowJobView>,
    /// Stable owning workflow run ID.
    pub run_id: WorkflowRunId,
    /// Explicit declared human title.
    pub title: String,
    /// Declared purpose.
    pub description: String,
    /// Authoritative lifecycle for this item.
    pub state: WorkflowViewState,
    /// Current durable attempt number.
    pub attempt: u32,
    /// Phases in the exact planned order.
    pub phases: Vec<WorkflowPhaseView>,
    /// Progress over declared steps.
    pub counts: WorkflowCounts,
    /// Measured commit-time bounds and active elapsed time.
    pub timing: WorkflowTiming,
    /// Provider-reported accounting; no estimates.
    pub usage: WorkflowUsage,
    /// Safe controls currently available from the owning service.
    pub controls: WorkflowControls,
    /// Actual owned background job, when present.
    pub job_id: Option<JobId>,
}

#[derive(Default)]
pub(crate) struct WorkflowRuntimeView {
    pub active: BTreeMap<WorkflowRunId, Option<JobId>>,
    pub pausing: BTreeSet<WorkflowRunId>,
    pub closed: bool,
}
#[derive(Default)]
struct NodeClock {
    start: Option<i64>,
    finish: Option<i64>,
    legacy_result: Option<serde_json::Value>,
}
#[derive(Default)]
struct RunClock {
    start: Option<i64>,
    finish: Option<i64>,
    last: i64,
    ranges: Vec<(i64, Option<i64>)>,
    nodes: BTreeMap<String, NodeClock>,
    agent_finishes: BTreeMap<String, i64>,
}

/// Replay only this physical session's workflows. Incomplete history is Interrupted;
/// a live service overlays its owned jobs and safe controls through `views`.
pub fn project_workflow_views(
    session: &Session,
    now_ms: i64,
) -> Result<Vec<WorkflowView>, WorkflowError> {
    build_views(
        session,
        now_ms,
        &WorkflowRuntimeView {
            closed: true,
            ..Default::default()
        },
    )
}

pub(crate) fn build_views(
    session: &Session,
    now_ms: i64,
    runtime: &WorkflowRuntimeView,
) -> Result<Vec<WorkflowView>, WorkflowError> {
    use WorkflowViewState as S;
    let events = session
        .events()
        .iter()
        .filter(|event| event.seq >= session.first_local_seq())
        .cloned()
        .collect::<Vec<_>>();
    let projection = project_workflows(&events).map_err(|_| WorkflowError::Persistence)?;
    let mut clocks = BTreeMap::<WorkflowRunId, RunClock>::new();
    for event in &events {
        let SessionEventKind::WorkflowChange { change } = &event.kind else {
            continue;
        };
        match change.as_ref() {
            WorkflowChange::Saved { .. } => {}
            WorkflowChange::Job { run_id, .. } => {
                clocks.entry(run_id.clone()).or_default().last = event.time_ms;
            }
            WorkflowChange::Start { run_id, .. } => {
                let clock = clocks.entry(run_id.clone()).or_default();
                clock.start = Some(event.time_ms);
                clock.last = event.time_ms;
                clock.ranges.push((event.time_ms, None));
            }
            WorkflowChange::Resume { run_id, .. } => {
                let clock = clocks.entry(run_id.clone()).or_default();
                if let Some(range) = clock.ranges.last_mut()
                    && range.1.is_none()
                {
                    range.1 = Some(clock.last);
                }
                clock.last = event.time_ms;
                clock.finish = None;
                clock.ranges.push((event.time_ms, None));
            }
            WorkflowChange::End {
                run_id, outcome, ..
            } => {
                let clock = clocks.entry(run_id.clone()).or_default();
                clock.last = event.time_ms;
                if *outcome != WorkflowOutcome::Paused {
                    clock.finish = Some(event.time_ms);
                }
                if let Some(range) = clock.ranges.last_mut() {
                    range.1 = Some(event.time_ms);
                }
            }
            WorkflowChange::Agent { run_id, record, .. } => {
                let clock = clocks.entry(run_id.clone()).or_default();
                clock.last = event.time_ms;
                if matches!(
                    record.state,
                    heycode_session::WorkflowAgentState::Completed
                        | heycode_session::WorkflowAgentState::Failed
                        | heycode_session::WorkflowAgentState::Cancelled
                ) {
                    clock
                        .agent_finishes
                        .entry(record.task_id.clone())
                        .or_insert(record.updated_at_ms);
                }
            }
            WorkflowChange::Node {
                run_id,
                step_id,
                record,
                ..
            } => {
                let clock = clocks.entry(run_id.clone()).or_default();
                clock.last = event.time_ms;
                let node = clock.nodes.entry(step_id.clone()).or_default();
                if record.state == WorkflowNodeState::Started {
                    node.start.get_or_insert(event.time_ms);
                    node.finish = None;
                } else {
                    node.finish = Some(event.time_ms);
                }
            }
            WorkflowChange::Progress { run_id, step, .. } => {
                let clock = clocks.entry(run_id.clone()).or_default();
                clock.last = event.time_ms;
                if let Some(step) = projection.get(run_id).and_then(|run| {
                    run.definition()
                        .steps()
                        .get((*step as usize).saturating_sub(1))
                }) {
                    clock
                        .nodes
                        .entry(step.id().to_owned())
                        .or_default()
                        .start
                        .get_or_insert(event.time_ms);
                }
            }
            WorkflowChange::Checkpoint {
                run_id,
                completed_steps,
                value,
                ..
            } => {
                let clock = clocks.entry(run_id.clone()).or_default();
                clock.last = event.time_ms;
                if let Some(step) = projection.get(run_id).and_then(|run| {
                    run.definition()
                        .steps()
                        .get((*completed_steps as usize).saturating_sub(1))
                }) {
                    let node = clock.nodes.entry(step.id().to_owned()).or_default();
                    node.finish = Some(event.time_ms);
                    node.legacy_result = Some(value.clone());
                }
            }
        }
    }
    let mut views = Vec::new();
    for (run_id, run) in projection.iter() {
        let clock = clocks.get(run_id).ok_or(WorkflowError::Persistence)?;
        let live = runtime.active.contains_key(run_id);
        let end_bound = if live {
            now_ms.max(clock.last)
        } else {
            clock.last
        };
        let mut state = match run.state() {
            WorkflowState::Paused => S::Paused,
            WorkflowState::Completed => S::Completed,
            WorkflowState::Failed => S::Failed,
            WorkflowState::Cancelled => S::Cancelled,
            WorkflowState::Running if !live => S::Interrupted,
            WorkflowState::Running if runtime.pausing.contains(run_id) => S::Pausing,
            WorkflowState::Running => S::Running,
        };
        if state == S::Running && run.nodes().is_empty() && run.progress_sequence() == 0 {
            state = S::Queued;
        }
        let declared = if run.definition().phases().is_empty() {
            run.definition()
                .steps()
                .iter()
                .map(|step| (step.id().to_owned(), step.label().to_owned()))
                .collect::<Vec<_>>()
        } else {
            run.definition()
                .phases()
                .iter()
                .map(|phase| (phase.id.clone(), phase.title.clone()))
                .collect()
        };
        let mut phases = Vec::new();
        let mut counts = WorkflowCounts::default();
        let mut usage = WorkflowUsage::default();
        for (phase_id, title) in declared {
            let mut nodes = Vec::new();
            let mut phase_counts = WorkflowCounts::default();
            let mut phase_usage = WorkflowUsage::default();
            for (index, step) in run
                .definition()
                .steps()
                .iter()
                .enumerate()
                .filter(|(_, step)| step.phase_id().unwrap_or(step.id()) == phase_id)
            {
                let record = run.nodes().get(step.id());
                let node_clock = clock.nodes.get(step.id());
                let legacy_completed =
                    !run.definition().is_graph() && index < (run.completed_steps() as usize);
                let node_state = match record.map(|record| record.state) {
                    Some(WorkflowNodeState::Completed) => S::Completed,
                    Some(WorkflowNodeState::Skipped) => S::Skipped,
                    Some(WorkflowNodeState::Failed) if state == S::Cancelled => S::Cancelled,
                    Some(WorkflowNodeState::Failed)
                        if live
                            && step.replay_safe()
                            && record
                                .is_some_and(|record| record.attempt < step.max_attempts()) =>
                    {
                        S::Queued
                    }
                    Some(WorkflowNodeState::Failed) => S::Failed,
                    _ if legacy_completed => S::Completed,
                    Some(WorkflowNodeState::Started) if !live => S::Interrupted,
                    _ if state == S::Cancelled && record.is_some() => S::Cancelled,
                    _ if state == S::Failed && record.is_some() => S::Interrupted,
                    _ if matches!(state, S::Failed | S::Cancelled) => S::Queued,
                    _ if state == S::Paused => S::Paused,
                    _ if state == S::Interrupted => S::Interrupted,
                    Some(WorkflowNodeState::Started) => S::Running,
                    _ if node_clock
                        .is_some_and(|clock| clock.start.is_some() && clock.finish.is_none()) =>
                    {
                        S::Running
                    }
                    _ => S::Queued,
                };
                let start = node_clock.and_then(|clock| clock.start);
                let finish = node_clock.and_then(|clock| clock.finish).or_else(|| {
                    start.and_then(|_| {
                        matches!(node_state, S::Failed | S::Cancelled)
                            .then_some(clock.finish)
                            .flatten()
                    })
                });
                let (kind, prompt) = match step.action() {
                    WorkflowAction::Emit { .. } => (WorkflowNodeKind::Emit, None),
                    WorkflowAction::Delay { .. } => (WorkflowNodeKind::Delay, None),
                    WorkflowAction::Tool { .. } => (WorkflowNodeKind::Tool, None),
                    WorkflowAction::Agent { prompt } => {
                        (WorkflowNodeKind::Agent, Some(prompt.clone()))
                    }
                };
                let mut node = WorkflowNodeView {
                    id: step.id().to_owned(),
                    label: step.label().to_owned(),
                    depends_on: step.depends_on().to_vec(),
                    kind,
                    state: node_state,
                    attempt: record.map_or(u32::from(start.is_some()), |record| record.attempt),
                    result: record
                        .filter(|record| record.state != WorkflowNodeState::Started)
                        .map(|record| record.value.clone())
                        .or_else(|| node_clock.and_then(|clock| clock.legacy_result.clone())),
                    prompt,
                    timing: timing(start, finish, end_bound, &clock.ranges),
                };
                if node_state == S::Interrupted && node.timing.finished_at_ms.is_none() {
                    node.timing.elapsed_ms = None;
                }
                phase_counts.count(node_state);
                counts.count(node_state);
                nodes.push(node);
            }
            let mut agents = Vec::new();
            for agent in run
                .agents()
                .values()
                .filter(|agent| nodes.iter().any(|node| node.id == agent.node_id))
            {
                if agent.owner_session_id != session.id().as_str() {
                    return Err(WorkflowError::Persistence);
                }
                let mut agent_state = task_state(agent.state);
                if !live
                    && matches!(
                        agent_state,
                        TaskState::Queued
                            | TaskState::Running
                            | TaskState::Cancelling
                            | TaskState::Idle
                    )
                {
                    agent_state = TaskState::Interrupted;
                }
                let finish = clock.agent_finishes.get(&agent.task_id).copied();
                phase_usage.add(agent.usage);
                agents.push(WorkflowAgentView {
                    task_id: agent.task_id.clone(),
                    session_id: agent.session_id.clone(),
                    job_id: agent
                        .job_id
                        .as_deref()
                        .map(JobId::parse)
                        .transpose()
                        .map_err(|_| WorkflowError::Persistence)?,
                    node_id: agent.node_id.clone(),
                    node_attempt: agent.node_attempt,
                    label: agent.label.clone(),
                    state: agent_state,
                    prompt: agent.prompt.clone(),
                    prompt_truncated: agent.prompt_truncated,
                    summary: agent.summary.clone(),
                    summary_truncated: agent.summary_truncated,
                    created_at_ms: agent.created_at_ms,
                    updated_at_ms: agent.updated_at_ms,
                    timing: if agent_state == TaskState::Interrupted && finish.is_none() {
                        WorkflowTiming {
                            started_at_ms: Some(agent.created_at_ms),
                            finished_at_ms: None,
                            elapsed_ms: None,
                        }
                    } else {
                        timing(Some(agent.created_at_ms), finish, end_bound, &clock.ranges)
                    },
                    usage: agent.usage,
                });
            }
            agents.sort_by_key(|agent| {
                (
                    nodes
                        .iter()
                        .position(|node| node.id == agent.node_id)
                        .unwrap_or(usize::MAX),
                    agent.node_attempt,
                    agent.created_at_ms,
                    agent.task_id.clone(),
                )
            });
            let phase_state = aggregate(&phase_counts, state);
            let start = nodes
                .iter()
                .filter_map(|node| node.timing.started_at_ms)
                .min();
            let finish = if phase_counts.total > 0 && phase_counts.settled == phase_counts.total {
                nodes
                    .iter()
                    .filter_map(|node| node.timing.finished_at_ms)
                    .max()
            } else if start.is_some() && matches!(phase_state, S::Failed | S::Cancelled) {
                clock.finish
            } else {
                None
            };
            usage.add(phase_usage);
            let mut phase_timing = timing(start, finish, end_bound, &clock.ranges);
            if phase_state == S::Interrupted && finish.is_none() {
                phase_timing.elapsed_ms = None;
            }
            phases.push(WorkflowPhaseView {
                id: phase_id,
                title,
                nodes,
                agents,
                state: phase_state,
                counts: phase_counts,
                timing: phase_timing,
                usage: phase_usage,
            });
        }
        let resumable = matches!(run.state(), WorkflowState::Paused | WorkflowState::Running)
            && !live
            && !run
                .nodes()
                .values()
                .any(|node| node.state == WorkflowNodeState::Started);
        let controls = if runtime.closed {
            WorkflowControls::default()
        } else {
            WorkflowControls {
                can_pause: live
                    && run.state() == WorkflowState::Running
                    && !runtime.pausing.contains(run_id),
                can_resume: resumable,
                can_cancel: matches!(run.state(), WorkflowState::Running | WorkflowState::Paused),
            }
        };
        let mut run_timing = timing(clock.start, clock.finish, end_bound, &clock.ranges);
        if state == S::Interrupted && clock.finish.is_none() {
            run_timing.elapsed_ms = None;
        }
        views.push(WorkflowView {
            run_id: run_id.clone(),
            title: run.definition().title().to_owned(),
            description: run.definition().description().to_owned(),
            state,
            attempt: run.attempt(),
            phases,
            counts,
            timing: run_timing,
            usage,
            controls,
            jobs: run
                .jobs()
                .values()
                .map(|job| {
                    Ok(WorkflowJobView {
                        attempt: job.attempt,
                        job_id: JobId::parse(&job.job_id)
                            .map_err(|_| WorkflowError::Persistence)?,
                        admitted_at_ms: job.admitted_at_ms,
                        settled_at_ms: job.settled_at_ms,
                        outcome: job.outcome,
                    })
                })
                .collect::<Result<Vec<_>, WorkflowError>>()?,
            job_id: runtime.active.get(run_id).cloned().flatten(),
        });
    }
    views.sort_by_key(|view| (view.timing.started_at_ms, view.run_id.clone()));
    Ok(views)
}
fn aggregate(counts: &WorkflowCounts, run: WorkflowViewState) -> WorkflowViewState {
    use WorkflowViewState as S;
    if counts.total > 0 && counts.completed + counts.skipped == counts.total {
        return if counts.skipped == counts.total {
            S::Skipped
        } else {
            S::Completed
        };
    }
    if counts.running > 0 {
        return if run == S::Pausing {
            S::Pausing
        } else {
            S::Running
        };
    }
    if counts.failed > 0 {
        return S::Failed;
    }
    if counts.cancelled > 0 {
        return S::Cancelled;
    }
    match run {
        S::Paused => S::Paused,
        S::Interrupted => S::Interrupted,
        S::Failed => S::Failed,
        S::Cancelled => S::Cancelled,
        _ => S::Queued,
    }
}
fn timing(
    start: Option<i64>,
    finish: Option<i64>,
    bound: i64,
    ranges: &[(i64, Option<i64>)],
) -> WorkflowTiming {
    let elapsed = start.map(|start| {
        let finish = finish.unwrap_or(bound);
        ranges
            .iter()
            .map(|(left, right)| {
                let left = (*left).max(start);
                let right = right.unwrap_or(bound).min(finish);
                u64::try_from(right.saturating_sub(left)).unwrap_or(0)
            })
            .fold(0u64, u64::saturating_add)
    });
    WorkflowTiming {
        started_at_ms: start,
        finished_at_ms: finish,
        elapsed_ms: elapsed,
    }
}
fn task_state(state: heycode_session::WorkflowAgentState) -> TaskState {
    use heycode_session::WorkflowAgentState as W;
    match state {
        W::Queued => TaskState::Queued,
        W::Running => TaskState::Running,
        W::Cancelling => TaskState::Cancelling,
        W::Idle => TaskState::Idle,
        W::Completed => TaskState::Completed,
        W::Failed => TaskState::Failed,
        W::Cancelled => TaskState::Cancelled,
        W::Closed => TaskState::Closed,
        W::Interrupted => TaskState::Interrupted,
    }
}

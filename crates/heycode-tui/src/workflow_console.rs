//! Native workflow workspace state. Selection follows identities, never labels.

use async_trait::async_trait;
use ratatui::layout::Rect;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

/// Lifecycle presented by the workflow's actual execution owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkflowStatus {
    /// Declared work awaiting admission.
    #[default]
    Queued,
    /// Execution is in progress.
    Running,
    /// Execution needs approval or input.
    Waiting,
    /// Pause or cancellation requested; admitted work is settling.
    Pausing,
    /// Stopped at a safe durable boundary.
    Paused,
    /// Committed successful completion.
    Completed,
    /// Execution failed.
    Failed,
    /// Cancellation settled.
    Cancelled,
    /// Execution lost its live owner.
    Interrupted,
    /// Graph explicitly skipped this work.
    Skipped,
}
impl WorkflowStatus {
    /// Color-independent lifecycle text.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Working",
            Self::Waiting => "Waiting",
            Self::Pausing => "Pausing",
            Self::Paused => "Paused",
            Self::Completed => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Stopped",
            Self::Interrupted => "Interrupted",
            Self::Skipped => "Skipped",
        }
    }
    /// Whether work is currently outstanding.
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(self, Self::Running | Self::Waiting | Self::Pausing)
    }
    /// Whether this item has a terminal outcome, including failure.
    #[must_use]
    pub const fn finished(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Skipped | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
    /// Whether this item counts toward successful progress.
    #[must_use]
    pub const fn settled(self) -> bool {
        matches!(self, Self::Completed | Self::Skipped)
    }
    /// Compact symbol always accompanied by a status label.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Queued => "○",
            Self::Running => "●",
            Self::Waiting => "◷",
            Self::Pausing | Self::Paused => "Ⅱ",
            Self::Completed => "✓",
            Self::Failed => "!",
            Self::Cancelled | Self::Interrupted => "■",
            Self::Skipped => "–",
        }
    }
}

/// Optional measured values. Missing values are omitted, never synthesized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowStats {
    /// Measured active elapsed milliseconds, absent before start.
    pub elapsed_ms: Option<u64>,
    /// Provider-reported input tokens, absent when unreported.
    pub input_tokens: Option<u64>,
    /// Provider-reported output tokens, absent when unreported.
    pub output_tokens: Option<u64>,
    /// Some responses omitted usage accounting.
    pub partial_usage: bool,
}

/// Agent admitted for an exact workflow node. `task_id` opens the real conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowAgent {
    /// Exact public child task identity.
    pub task_id: String,
    /// Exact declared graph node identity.
    pub node_id: String,
    /// Human label from the execution owner.
    pub label: String,
    /// Authoritative lifecycle presented with text.
    pub status: WorkflowStatus,
    /// Actual resolved agent prompt.
    pub assignment: String,
    /// Committed result or retained live assistant update.
    pub summary: String,
    /// Current owner-reported tool activity.
    pub activity: Option<String>,
    /// Measured timing and usage.
    pub stats: WorkflowStats,
    /// Durable child session identity, when recorded.
    pub session_id: Option<String>,
    /// Some assignment or output exceeded retained bounds.
    pub output_truncated: bool,
}

/// Real graph step, including steps that do not launch an agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowNode {
    /// Stable declared or service-owned identity.
    pub id: String,
    /// Human label from the execution owner.
    pub label: String,
    /// Actual graph action category.
    pub kind: String,
    /// Authoritative lifecycle presented with text.
    pub status: WorkflowStatus,
    /// Committed plain-text output, when present.
    pub result: Option<String>,
}

/// Declared phase with actual steps and admitted children.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowPhase {
    /// Stable declared or service-owned identity.
    pub id: String,
    /// Declared human-readable title.
    pub title: String,
    /// Authoritative lifecycle presented with text.
    pub status: WorkflowStatus,
    /// Real declared graph steps in order.
    pub nodes: Vec<WorkflowNode>,
    /// Actually admitted children, correlated by identity.
    pub agents: Vec<WorkflowAgent>,
    /// Measured timing and usage.
    pub stats: WorkflowStats,
}
impl WorkflowPhase {
    /// Successful phase or step count and actual declared total.
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        (
            self.nodes.iter().filter(|n| n.status.settled()).count(),
            self.nodes.len(),
        )
    }
}

/// Lifecycle actions currently offered by the owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowControls {
    /// Owner currently accepts a cooperative pause.
    pub pause: bool,
    /// Owner currently permits checkpoint resume.
    pub resume: bool,
    /// Owner currently permits cancellation.
    pub stop: bool,
}

/// One durable workflow coordinator admission with a typed settlement classification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowJob {
    /// Exact actual coordinator job identity.
    pub id: String,
    /// A typed successful, paused or cancelled outcome permits a quiet routine notice.
    pub routine_notice: bool,
}

/// One owner-scoped durable workflow projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowRun {
    /// Actual workflow job identity while its service retains the live run.
    pub job_id: Option<String>,
    /// All durably admitted coordinator jobs, including completed prior attempts.
    pub jobs: Vec<WorkflowJob>,
    /// Stable declared or service-owned identity.
    pub id: String,
    /// Declared human-readable title.
    pub title: String,
    /// Declared purpose of this workflow.
    pub description: String,
    /// Authoritative lifecycle presented with text.
    pub status: WorkflowStatus,
    /// Declared phases in execution order.
    pub phases: Vec<WorkflowPhase>,
    /// Measured timing and usage.
    pub stats: WorkflowStats,
    /// Owner-backed available lifecycle actions.
    pub controls: WorkflowControls,
}
impl WorkflowRun {
    /// Successful phase or step count and actual declared total.
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        (
            self.phases.iter().filter(|p| p.status.settled()).count(),
            self.phases.len(),
        )
    }
    /// Successful declared agent steps and total planned agent steps, including queued work.
    #[must_use]
    pub fn agent_progress(&self) -> (usize, usize) {
        let agents = self
            .phases
            .iter()
            .flat_map(|phase| &phase.nodes)
            .filter(|node| node.kind == "agent");
        agents.fold((0, 0), |(done, total), node| {
            (
                done + usize::from(node.status == WorkflowStatus::Completed),
                total + 1,
            )
        })
    }
    /// Count only recorded native task admissions.
    #[must_use]
    pub fn agent_count(&self) -> usize {
        self.phases.iter().map(|p| p.agents.len()).sum()
    }
    /// Current executing or next incomplete phase.
    #[must_use]
    pub fn current_phase(&self) -> Option<&WorkflowPhase> {
        self.phases
            .iter()
            .find(|p| p.status.active())
            .or_else(|| {
                self.phases
                    .iter()
                    .find(|p| p.status == WorkflowStatus::Paused)
            })
            .or_else(|| self.phases.iter().find(|p| !p.status.settled()))
            .or_else(|| self.phases.last())
    }
}

/// Native lifecycle command sent to the existing owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowActionKind {
    /// Request cooperative pause.
    Pause,
    /// Resume committed progress.
    Resume,
    /// Cancel this workflow.
    Stop,
}
/// An exact run-scoped lifecycle request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowAction {
    /// Exact workflow receiving this action.
    pub run_id: String,
    /// Actual graph action category.
    pub kind: WorkflowActionKind,
}

/// Exact service projection plus owner-backed control operations.
#[async_trait]
pub trait WorkflowSource: Send + Sync {
    /// Read current owner-scoped workflows without executing them.
    fn snapshot(&self) -> Result<Vec<WorkflowRun>, String>;
    /// Apply an authorized control through the existing service.
    async fn execute(
        &self,
        action: WorkflowAction,
        cancellation: CancellationToken,
    ) -> Result<String, String>;
}

/// Workspace presentation, independent of execution state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkflowView {
    /// Only the persistent bottom rail is visible.
    #[default]
    Collapsed,
    /// Phase and agent workspace.
    Workspace,
    /// Selected agent assignment and result.
    Agent,
    /// Choose among workflows.
    Picker,
}
/// Keyboard focus within the workflow workspace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkflowFocus {
    /// Phase navigation has keyboard focus.
    #[default]
    Phases,
    /// Agent navigation has keyboard focus.
    Agents,
}
/// Mouse targets rebuilt from the rendered layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowHit {
    /// Expand or collapse the workflow rail.
    Toggle,
    /// Choose among workflows.
    Picker,
    /// Open an exact workflow identity.
    PickRun(String),
    /// Select an exact phase identity.
    Phase(String),
    /// Selected agent assignment and result.
    Agent(String),
    /// Return to the prior workflow view.
    Back,
    /// Open the actual child conversation.
    Conversation,
    /// Request an owner-backed lifecycle operation.
    Action(WorkflowActionKind),
    /// Scroll the selected result upward.
    ScrollUp,
    /// Scroll the selected result downward.
    ScrollDown,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkflowPosition {
    phase_id: Option<String>,
    agents: BTreeMap<String, String>,
    detail_scroll: BTreeMap<String, usize>,
}

/// Navigation and cached projection. It never owns an agent execution loop.
#[derive(Default)]
pub struct WorkflowConsole {
    pub(crate) source: Option<Arc<dyn WorkflowSource>>,
    pub(crate) task_keys: BTreeSet<String>,
    pub(crate) job_notices: BTreeMap<String, bool>,
    pub(crate) first_local_seq: u64,
    pub(crate) inbox_next_turn: Vec<heycode_session::InboxMessage>,
    pub(crate) inbox_next_step: Vec<heycode_session::InboxMessage>,
    pub(crate) claimed_workflow_notice: Option<String>,
    /// Current owner-scoped snapshots.
    pub runs: Vec<WorkflowRun>,
    /// Selected workflow identity, stable across refresh.
    pub selected_run: Option<String>,
    /// Current workflow presentation.
    pub view: WorkflowView,
    /// Keyboard focus within the workspace.
    pub focus: WorkflowFocus,
    /// The bottom rail owns keyboard focus while the composer draft is preserved.
    pub rail_focused: bool,
    /// Owner acknowledgement or actionable failure.
    pub notice: Option<String>,
    pub(crate) positions: BTreeMap<String, WorkflowPosition>,
    pub(crate) hits: Vec<(Rect, WorkflowHit)>,
    pub(crate) body: Rect,
    pub(crate) detail_height: usize,
    pub(crate) detail_rows: usize,
    pub(crate) pending: Option<WorkflowAction>,
    pub(crate) operations: tokio::task::JoinSet<(String, Result<String, String>)>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) opened_from_workflow: Option<(String, String)>,
}

impl WorkflowConsole {
    pub(crate) fn observe_inbox_splice(
        &mut self,
        kind: &heycode_session::SessionEventKind,
        routine_jobs: &BTreeSet<String>,
    ) {
        let heycode_session::SessionEventKind::AgentInboxSplice {
            target,
            start,
            removed_count,
            inserted,
            outcome,
        } = kind
        else {
            return;
        };
        let queue = match target {
            heycode_session::InboxTarget::NextTurn => &mut self.inbox_next_turn,
            heycode_session::InboxTarget::NextStep => &mut self.inbox_next_step,
        };
        let start = usize::try_from(*start).unwrap_or(usize::MAX);
        let count = usize::try_from(removed_count.unwrap_or(0)).unwrap_or(usize::MAX);
        if start > queue.len() || count > queue.len().saturating_sub(start) {
            queue.clear();
            self.claimed_workflow_notice = None;
            return;
        }
        if count == 1
            && outcome.is_none()
            && inserted.is_empty()
            && let Some(message) = queue.get(start)
            && let heycode_session::InboxSource::Job { job_id } = message.source()
            && self.task_keys.contains(&format!("job:{job_id}"))
            && self
                .job_notices
                .get(job_id)
                .copied()
                .unwrap_or_else(|| routine_jobs.contains(job_id))
        {
            self.claimed_workflow_notice = Some(message.text().to_owned());
        }
        queue.splice(start..start + count, inserted.iter().cloned());
    }
    pub(crate) fn consume_workflow_notice(&mut self, text: &str) -> bool {
        self.claimed_workflow_notice
            .take()
            .is_some_and(|notice| notice == text)
    }
    /// Linear, color-independent view of the selected workflow and focus.
    #[must_use]
    pub fn accessible_lines(&self) -> Vec<String> {
        let mut out = vec!["Workflows".to_owned()];
        let Some(run) = self.selected() else {
            out.push("No workflows yet. Escape returns to the conversation.".into());
            if let Some(notice) = &self.notice {
                out.push(crate::task_console::safe(notice));
            }
            return out;
        };
        let (done, total) = run.progress();
        out.push(format!(
            "{} · {} · {done}/{total} phases",
            run.title,
            run.status.label()
        ));
        out.push(run.description.clone());
        if self.view == WorkflowView::Picker {
            for item in &self.runs {
                out.push(format!(
                    "{}{} · {}",
                    if item.id == run.id { "Selected: " } else { "" },
                    item.title,
                    item.status.label()
                ));
            }
            out.push("Up or Down chooses; Enter opens; Escape returns.".into());
        } else if self.view == WorkflowView::Agent {
            if let Some(agent) = self.agent() {
                out.push(format!("{} · {}", agent.label, agent.status.label()));
                out.push(format!("Assignment: {}", agent.assignment));
                out.push(format!(
                    "{}: {}",
                    if agent.status.finished() {
                        "Result"
                    } else {
                        "Latest update"
                    },
                    if agent.summary.is_empty() {
                        "Waiting for the first update."
                    } else {
                        &agent.summary
                    }
                ));
                if agent.output_truncated {
                    out.push("Some output is outside this retained summary. Open the conversation for more.".into());
                }
            }
            out.push("C opens the actual conversation. Escape returns to the phase. Up, Down, Page Up and Page Down scroll.".into());
        } else {
            out.push("Phases".into());
            for phase in &run.phases {
                let (done, total) = phase.progress();
                out.push(format!(
                    "{}{} · {} · {done}/{total} steps",
                    if self.phase().is_some_and(|p| p.id == phase.id) {
                        "Selected: "
                    } else {
                        ""
                    },
                    phase.title,
                    phase.status.label()
                ));
            }
            if let Some(phase) = self.phase() {
                out.push(format!("{} · {} agents", phase.title, phase.agents.len()));
                for agent in &phase.agents {
                    out.push(format!(
                        "{}{} · {}",
                        if self.agent().is_some_and(|a| a.task_id == agent.task_id) {
                            "Selected: "
                        } else {
                            ""
                        },
                        agent.label,
                        agent.status.label()
                    ));
                }
                for node in &phase.nodes {
                    if phase.agents.is_empty() {
                        out.push(format!("{} · {}", node.label, node.status.label()));
                    }
                }
            }
            out.push(
                "Tab switches panes. Arrow keys select. Enter inspects an agent. Escape closes."
                    .into(),
            );
            if run.controls.pause {
                out.push("P pauses after admitted work settles.".into());
            }
            if run.controls.resume {
                out.push("R resumes from committed progress.".into());
            }
            if run.controls.stop {
                out.push("X stops this workflow.".into());
            }
            if self.runs.len() > 1 {
                out.push("W chooses another workflow.".into());
            }
        }
        if let Some(notice) = &self.notice {
            out.push(notice.clone());
        }
        out.into_iter()
            .map(|line| crate::task_console::safe(&line))
            .collect()
    }
    /// Attach the owner projection and read its current state.
    pub fn attach(&mut self, source: Arc<dyn WorkflowSource>) {
        self.source = Some(source);
        self.refresh();
    }
    /// Whether an owner service is connected.
    #[must_use]
    pub fn attached(&self) -> bool {
        self.source.is_some()
    }
    /// Whether the workflow workspace owns the content area.
    #[must_use]
    pub fn expanded(&self) -> bool {
        self.view != WorkflowView::Collapsed
    }
    /// Resolve the selected run by exact identity.
    #[must_use]
    pub fn selected(&self) -> Option<&WorkflowRun> {
        self.runs
            .iter()
            .find(|run| Some(&run.id) == self.selected_run.as_ref())
    }
    /// Resolve remembered phase selection or the current phase.
    #[must_use]
    pub fn phase(&self) -> Option<&WorkflowPhase> {
        let run = self.selected()?;
        let phase = self
            .positions
            .get(&run.id)
            .and_then(|position| position.phase_id.as_ref());
        run.phases
            .iter()
            .find(|p| Some(&p.id) == phase)
            .or_else(|| run.current_phase())
    }
    /// Resolve the selected child by exact identity.
    #[must_use]
    pub fn agent(&self) -> Option<&WorkflowAgent> {
        let phase = self.phase()?;
        let selected = self
            .selected_run
            .as_ref()
            .and_then(|run| self.positions.get(run))
            .and_then(|position| position.agents.get(&phase.id));
        phase
            .agents
            .iter()
            .find(|agent| Some(&agent.task_id) == selected)
            .or_else(|| phase.agents.first())
    }
    /// Refresh state and drain asynchronous owner acknowledgements.
    pub fn refresh(&mut self) {
        let Some(source) = self.source.as_ref() else {
            return;
        };
        match source.snapshot() {
            Ok(runs) => {
                for run in &runs {
                    for job in &run.jobs {
                        self.task_keys.insert(format!("job:{}", job.id));
                        self.job_notices.insert(job.id.clone(), job.routine_notice);
                    }
                    if let Some(id) = &run.job_id {
                        self.task_keys.insert(format!("job:{id}"));
                    }
                    for agent in run.phases.iter().flat_map(|phase| &phase.agents) {
                        self.task_keys.insert(format!("child:{}", agent.task_id));
                    }
                }
                self.runs = runs;
                if self.view == WorkflowView::Collapsed
                    && !self.selected().is_some_and(|run| run.status.active())
                    && let Some(active) = self.runs.iter().find(|run| run.status.active())
                {
                    self.selected_run = Some(active.id.clone());
                }
                if !self
                    .runs
                    .iter()
                    .any(|r| Some(&r.id) == self.selected_run.as_ref())
                {
                    self.selected_run = self
                        .runs
                        .iter()
                        .find(|r| r.status.active())
                        .or_else(|| self.runs.first())
                        .map(|r| r.id.clone());
                }
            }
            Err(error) => {
                self.runs.clear();
                self.notice = Some(format!("Workflow status unavailable: {error}"));
            }
        }
        while let Some(result) = self.operations.try_join_next() {
            self.notice = Some(match result {
                Ok((_, Ok(message))) => message,
                Ok((_, Err(error))) => format!("Could not update workflow: {error}"),
                Err(_) => "Workflow control stopped; check its current state.".into(),
            });
        }
        if self.operations.is_empty()
            && let Some(action) = self.pending.take()
            && let Some(source) = self.source.clone()
        {
            let cancellation = self.cancellation.child_token();
            self.operations.spawn(async move {
                use futures::FutureExt;
                let id = action.run_id.clone();
                let result = std::panic::AssertUnwindSafe(source.execute(action, cancellation))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| Err("Workflow owner stopped unexpectedly".into()));
                (id, result)
            });
        }
    }
    /// Select an existing workflow while retaining its navigation history.
    pub fn select_run(&mut self, id: String) {
        if self.runs.iter().any(|r| r.id == id) {
            self.selected_run = Some(id);
            self.view = WorkflowView::Workspace;
            self.focus = WorkflowFocus::Phases;
            self.notice = None;
        }
    }
    /// Select an existing phase by identity.
    pub fn select_phase(&mut self, id: String) {
        if let Some(run) = self.selected_run.clone()
            && self
                .selected()
                .is_some_and(|r| r.phases.iter().any(|p| p.id == id))
        {
            self.positions.entry(run).or_default().phase_id = Some(id);
            self.focus = WorkflowFocus::Phases;
            self.view = WorkflowView::Workspace;
        }
    }
    /// Select an actual child and optionally inspect its summary.
    pub fn select_agent(&mut self, id: String, open: bool) {
        if let (Some(run), Some(phase)) = (
            self.selected_run.clone(),
            self.phase().map(|p| p.id.clone()),
        ) && self
            .phase()
            .is_some_and(|p| p.agents.iter().any(|a| a.task_id == id))
        {
            self.positions
                .entry(run)
                .or_default()
                .agents
                .insert(phase, id);
            self.focus = WorkflowFocus::Agents;
            if open {
                self.view = WorkflowView::Agent;
            }
        }
    }
    /// Move within declared phases without wrapping.
    pub fn move_phase(&mut self, forward: bool) {
        let Some(run) = self.selected() else {
            return;
        };
        let index = run
            .phases
            .iter()
            .position(|p| self.phase().is_some_and(|s| s.id == p.id))
            .unwrap_or(0);
        let next = if forward {
            (index + 1).min(run.phases.len().saturating_sub(1))
        } else {
            index.saturating_sub(1)
        };
        if let Some(phase) = run.phases.get(next) {
            self.select_phase(phase.id.clone());
        }
    }
    /// Move within actual child rows without wrapping.
    pub fn move_agent(&mut self, forward: bool) {
        let Some(phase) = self.phase() else {
            return;
        };
        let index = phase
            .agents
            .iter()
            .position(|a| self.agent().is_some_and(|s| s.task_id == a.task_id))
            .unwrap_or(0);
        let next = if forward {
            (index + 1).min(phase.agents.len().saturating_sub(1))
        } else {
            index.saturating_sub(1)
        };
        if let Some(agent) = phase.agents.get(next) {
            self.select_agent(agent.task_id.clone(), false);
        }
    }
    /// Remembered summary scroll offset for the selected child.
    #[must_use]
    pub fn scroll(&self) -> usize {
        self.selected_run
            .as_ref()
            .and_then(|id| self.positions.get(id))
            .and_then(|p| self.agent().and_then(|a| p.detail_scroll.get(&a.task_id)))
            .copied()
            .unwrap_or(0)
    }
    /// Set summary scroll, bounded by rendered content.
    pub fn set_scroll(&mut self, value: usize) {
        if let (Some(run), Some(agent)) = (
            self.selected_run.clone(),
            self.agent().map(|a| a.task_id.clone()),
        ) {
            self.positions.entry(run).or_default().detail_scroll.insert(
                agent,
                value.min(self.detail_rows.saturating_sub(self.detail_height)),
            );
        }
    }
    /// Queue only an available action against the selected run.
    pub fn request(&mut self, kind: WorkflowActionKind) {
        let Some(run) = self.selected() else {
            return;
        };
        let allowed = match kind {
            WorkflowActionKind::Pause => run.controls.pause,
            WorkflowActionKind::Resume => run.controls.resume,
            WorkflowActionKind::Stop => run.controls.stop,
        };
        if !allowed {
            self.notice =
                Some("This control is unavailable in the workflow's current state.".into());
            return;
        }
        if !self.operations.is_empty() || self.pending.is_some() {
            self.notice =
                Some("Waiting for the workflow owner to acknowledge the previous action.".into());
            return;
        }
        self.pending = Some(WorkflowAction {
            run_id: run.id.clone(),
            kind,
        });
        self.notice = Some(
            match kind {
                WorkflowActionKind::Pause => "Pause requested · admitted work will settle first",
                WorkflowActionKind::Resume => "Resuming from the last committed phase",
                WorkflowActionKind::Stop => "Stop requested · waiting for running work to settle",
            }
            .into(),
        );
    }
}
impl Drop for WorkflowConsole {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.operations.abort_all();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use heycode_session::{
        InboxDelivery, InboxMessage, InboxMessageId, InboxSource, InboxTarget, SessionEventKind,
    };
    fn insert(source: InboxSource, text: &str) -> SessionEventKind {
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![
                InboxMessage::with_source(
                    InboxMessageId::new("notice").unwrap(),
                    InboxDelivery::Inject,
                    text,
                    source,
                )
                .unwrap(),
            ],
            outcome: None,
        }
    }
    fn claim() -> SessionEventKind {
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: Some(1),
            inserted: vec![],
            outcome: None,
        }
    }
    #[test]
    fn workflow_notice_suppression_requires_typed_source_exact_owner_and_routine_outcome() {
        for (source, known, routine, expected) in [
            (
                InboxSource::Job {
                    job_id: "job-1".into(),
                },
                true,
                true,
                true,
            ),
            (InboxSource::Human, true, true, false),
            (
                InboxSource::Job {
                    job_id: "job-2".into(),
                },
                true,
                true,
                false,
            ),
            (
                InboxSource::Job {
                    job_id: "job-1".into(),
                },
                false,
                true,
                false,
            ),
            (
                InboxSource::Job {
                    job_id: "job-1".into(),
                },
                true,
                false,
                false,
            ),
        ] {
            let mut console = WorkflowConsole::default();
            if known {
                console.task_keys.insert("job:job-1".into());
            }
            let settled = if routine {
                BTreeSet::from(["job-1".into()])
            } else {
                BTreeSet::new()
            };
            console.observe_inbox_splice(&insert(source, "identical message"), &settled);
            console.observe_inbox_splice(&claim(), &settled);
            assert_eq!(
                console.consume_workflow_notice("identical message"),
                expected
            );
            assert!(
                !console.consume_workflow_notice("identical message"),
                "Only the atomic claimed message may be hidden"
            );
        }
    }
    #[test]
    fn workflow_notice_cancellation_and_unmatched_human_input_are_never_hidden() {
        let mut console = WorkflowConsole::default();
        console.task_keys.insert("job:job-1".into());
        let settled = BTreeSet::from(["job-1".into()]);
        console.observe_inbox_splice(
            &insert(
                InboxSource::Job {
                    job_id: "job-1".into(),
                },
                "notice",
            ),
            &settled,
        );
        assert!(
            !console.consume_workflow_notice("notice"),
            "Pending input is not a claimed workflow notice"
        );
        console.observe_inbox_splice(&claim(), &settled);
        assert!(!console.consume_workflow_notice("human text"));
        assert!(!console.consume_workflow_notice("notice"));
    }
}

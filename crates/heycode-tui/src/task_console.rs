//! Authoritative task navigation, output paging and asynchronous execution controls.
//!
//! This module deliberately does not inspect transcript prose. `TaskSource` is
//! the compatibility boundary between lifecycle owners and the terminal. The
//! console never invents timestamps, token counts or terminal settlement.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

pub use crate::task_source::RegistryTaskSource;
use async_trait::async_trait;
use ratatui::layout::Rect;
use tokio_util::sync::CancellationToken;

/// Maximum rows requested from a retained event/output owner at once.
pub const OUTPUT_PAGE_SIZE: usize = 128;
const MAX_PENDING_ACTIONS: usize = 8;

/// Stable identity scoped by its execution owner (`child:`, `job:`, `tool:`, `team:`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskKey(pub String);

/// A task's actual execution category, independent of the selected model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// A retained or running child conversation.
    Child,
    /// A process or arbitrary tool job.
    Job,
    /// A foreground tool execution.
    Tool,
    /// Durable planned work, independent of execution.
    Work,
    /// A committed team and its dependency graph.
    Team,
}

/// Semantic lifecycle states; text accompanies every color in both renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// Admitted but not started.
    Queued,
    /// Starting its runtime.
    Starting,
    /// Executing.
    Running,
    /// Waiting for input or approval.
    Waiting,
    /// Cancellation requested, settlement still pending.
    Cancelling,
    /// Continuable conversation between turns.
    Idle,
    /// Completed successfully.
    Completed,
    /// Failed execution.
    Failed,
    /// Cancellation actually settled.
    Cancelled,
    /// Shutdown interrupted execution.
    Interrupted,
    /// Conversation explicitly closed.
    Closed,
}

impl TaskStatus {
    /// Color-independent, stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Cancelling => "cancelling",
            Self::Idle => "idle",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Closed => "closed",
        }
    }

    /// Whether work remains outstanding.
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Starting | Self::Running | Self::Waiting | Self::Cancelling
        )
    }
}

/// Controls proved by the execution owner at the current revision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskCapabilities {
    /// Can read retained/live output.
    pub output: bool,
    /// Can submit a child follow-up or steer.
    pub steer: bool,
    /// Can send a line to the task's running PTY.
    pub terminal_input: bool,
    /// Can request interruption.
    pub interrupt: bool,
    /// Can explicitly start a new run in a retained failed native conversation.
    pub retry: bool,
    /// Can close a child conversation.
    pub close: bool,
    /// Can detach this exact foreground execution.
    pub background: bool,
}

/// Actual lifecycle telemetry; absence must stay visible as unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskTelemetry {
    /// First retained user prompt, for the foreground switcher and inspector.
    pub initial_prompt: Option<String>,
    /// Exact shell program when this is a process job.
    pub command: Option<String>,
    /// Actual deadline description; absent when unavailable.
    pub deadline: Option<String>,
    /// Failed/denied tools in the latest turn, not an inferred objective verdict.
    pub tool_errors: u32,
    /// Authoritative terminal failure, independent of recovered tool errors.
    pub terminal_diagnostic: Option<heycode_agent::TaskDiagnostic>,
    /// Parent tool occurrence that spawned this conversation.
    pub spawn_call_id: Option<String>,
    /// Monotonic execution-owner elapsed milliseconds, frozen at settlement.
    pub elapsed_ms: Option<u64>,
    /// Latest tool name reported by the owner.
    pub current_tool: Option<String>,
    /// Accounted input tokens, when the provider supplied usage.
    pub input_tokens: Option<u64>,
    /// Accounted output tokens, when the provider supplied usage.
    pub output_tokens: Option<u64>,
    /// Execution provider/runtime identity.
    pub runtime: Option<String>,
    /// Selected child model identity.
    pub model: Option<String>,
    /// Actual workspace reported by the execution owner.
    pub workspace: Option<String>,
    /// Actual start time supplied by the owner.
    pub started: Option<String>,
    /// Actual settlement time supplied by the owner.
    pub finished: Option<String>,
    /// Bytes retained in this task's bounded output buffer.
    pub output_bytes: Option<u64>,
    /// Provider-reported total billing, when available; never inferred from prose.
    pub cost: Option<String>,
    /// Paths changed by successful committed native edit/write results.
    pub changed_paths: Vec<String>,
}

/// One authoritative execution snapshot. Labels and messages are sanitized at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    /// Stable namespaced identity.
    pub key: TaskKey,
    /// Execution category.
    pub kind: TaskKind,
    /// Human label.
    pub label: String,
    /// Actual lifecycle state.
    pub status: TaskStatus,
    /// Stable owner/parent identity.
    pub parent: Option<String>,
    /// Correlated durable session.
    pub session: Option<String>,
    /// Correlated background job.
    pub job: Option<String>,
    /// Supported actions at this snapshot.
    pub capabilities: TaskCapabilities,
    /// Measured telemetry.
    pub telemetry: TaskTelemetry,
    /// Additional owner-supplied limitation or error.
    pub detail: Option<String>,
}

pub(crate) fn timestamp(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map_or_else(
            || "unavailable".into(),
            |time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
}

impl TaskRecord {
    /// Whether this child belongs on the normal active/attention surface.
    #[must_use]
    pub const fn active_child(&self) -> bool {
        matches!(self.kind, TaskKind::Child) && self.status.active()
    }

    /// Domain-appropriate status text; work is not described as a queued process.
    #[must_use]
    pub const fn status_label(&self) -> &'static str {
        if matches!(self.kind, TaskKind::Work) {
            match self.status {
                TaskStatus::Queued => "pending",
                TaskStatus::Running => "in progress",
                TaskStatus::Waiting => "blocked",
                other => other.label(),
            }
        } else {
            self.status.label()
        }
    }

    /// Full metadata shared by terminal and screen-reader detail views.
    #[must_use]
    pub fn detail_lines(&self) -> Vec<String> {
        let t = &self.telemetry;
        if self.kind == TaskKind::Work {
            return vec![
                format!("{} · {}", self.label, self.status_label()),
                format!(
                    "Work ID: {}",
                    self.key.0.strip_prefix("work:").unwrap_or(&self.key.0)
                ),
                format!("Owner: {}", self.parent.as_deref().unwrap_or("unassigned")),
                self.detail.clone().unwrap_or_default(),
            ]
            .into_iter()
            .map(|line| safe(&line))
            .collect();
        }
        if self.kind == TaskKind::Job {
            return vec![
                format!("{} · {}", self.label, self.status.label()),
                format!("Command: {}", t.command.as_deref().unwrap_or("unavailable")),
                format!(
                    "Directory: {}",
                    t.workspace.as_deref().unwrap_or("unavailable")
                ),
                format!("Started: {}", t.started.as_deref().unwrap_or("unavailable")),
                format!(
                    "Settled: {}",
                    t.finished.as_deref().unwrap_or(if self.status.active() {
                        "not settled"
                    } else {
                        "unavailable"
                    })
                ),
                format!(
                    "Elapsed: {}",
                    t.elapsed_ms.map_or_else(
                        || "unavailable".into(),
                        |ms| format!("{}.{:03}s", ms / 1000, ms % 1000)
                    )
                ),
                format!(
                    "Time limit: {}",
                    t.deadline.as_deref().unwrap_or("unavailable")
                ),
                format!(
                    "Result: {}",
                    self.detail.as_deref().unwrap_or(self.status.label())
                ),
                format!("Job: {}", self.job.as_deref().unwrap_or("unavailable")),
            ]
            .into_iter()
            .map(|line| safe(&line))
            .collect();
        }
        let mut lines = vec![
            format!("{} · {} · {}", self.key.0, self.status.label(), self.label),
            format!("Owner: {}", self.parent.as_deref().unwrap_or("root")),
            format!(
                "Session: {} · job: {}",
                self.session.as_deref().unwrap_or("unavailable"),
                self.job.as_deref().unwrap_or("none")
            ),
            format!(
                "Runtime/provider: {} · model: {}",
                t.runtime.as_deref().unwrap_or("unavailable"),
                t.model.as_deref().unwrap_or("unavailable")
            ),
            format!(
                "Execution elapsed: {}",
                t.elapsed_ms.map_or_else(
                    || "unavailable".into(),
                    |value| format!("{}.{:03} seconds", value / 1000, value % 1000)
                )
            ),
            format!(
                "Tokens: {} input / {} output",
                t.input_tokens
                    .map_or_else(|| "unavailable".into(), |n| n.to_string()),
                t.output_tokens
                    .map_or_else(|| "unavailable".into(), |n| n.to_string())
            ),
            format!(
                "Cost: {}",
                t.cost
                    .as_deref()
                    .unwrap_or("unavailable; provider has not reported billing")
            ),
            format!(
                "Retained output: {} bytes",
                t.output_bytes
                    .map_or_else(|| "unavailable".into(), |n| n.to_string())
            ),
            format!(
                "Current tools: {}",
                t.current_tool.as_deref().unwrap_or("none reported")
            ),
            format!(
                "Started: {} · settled: {}",
                t.started.as_deref().unwrap_or("unavailable"),
                t.finished.as_deref().unwrap_or(if self.status.active() {
                    "not settled"
                } else {
                    "unavailable"
                })
            ),
        ];
        lines.push(format!(
            "Directory: {}",
            t.workspace.as_deref().unwrap_or("unavailable")
        ));
        if t.changed_paths.is_empty() {
            lines.push("Workspace: no native edit/write changes recorded".into());
        } else {
            lines.push(format!(
                "Workspace: {} changed paths",
                t.changed_paths.len()
            ));
            lines.extend(t.changed_paths.iter().map(|path| format!("  {path}")));
        }
        if let Some(diagnostic) = &self.telemetry.terminal_diagnostic {
            lines.extend(diagnostic_lines(diagnostic));
        } else if let Some(detail) = &self.detail {
            lines.push(detail.clone());
        }
        if self.kind == TaskKind::Child
            && self.status == TaskStatus::Failed
            && !self.capabilities.retry
        {
            lines.push(
                "Retry unavailable: this failed run has no eligible retained native conversation."
                    .into(),
            );
        }
        lines.into_iter().map(|line| safe(&line)).collect()
    }
}

/// Typed, correlated retained output. Tool starts and finishes use the same call id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutputKind {
    /// User message.
    User(String),
    /// Assistant text or a process output chunk.
    Text(String),
    /// Committed tool identity and arguments, correlated without parsing prose.
    ToolMetadata {
        /// Exact call identity.
        call_id: String,
        /// Actual tool name.
        name: String,
        /// Committed arguments.
        args: serde_json::Value,
    },
    /// Explicitly exposed reasoning summary.
    Reasoning(String),
    /// Actual execution start, not a declared or queued call.
    ToolStarted {
        /// Stable execution identity.
        call_id: String,
        /// Tool name.
        name: String,
    },
    /// Actual execution settlement.
    ToolFinished {
        /// Matching execution identity.
        call_id: String,
        /// Success result.
        ok: bool,
        /// Safe-to-display output.
        text: String,
    },
    /// Explicit execution diagnostic; arbitrary status prose is never classified as an error.
    Diagnostic {
        /// Stable source-owned failure evidence.
        diagnostic: heycode_agent::TaskDiagnostic,
        /// Whether this is the authoritative terminal failure for its run.
        terminal: bool,
    },
    /// Lifecycle or team information.
    Status(String),
}

/// One entry in a source-owned retained output stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutputEvent {
    /// Source-owned strictly increasing cursor.
    pub sequence: u64,
    /// Authoritative event, not parsed from prose.
    pub kind: TaskOutputKind,
}

/// One bounded page, oldest entry first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskOutputPage {
    /// Events in this page.
    pub events: Vec<TaskOutputEvent>,
    /// An earlier page can be read with `before = events[0].sequence`.
    pub has_older: bool,
    /// Owner retention removed some earlier output.
    pub truncated: bool,
    /// Provider/runtime limitation, if output is not supported.
    pub unavailable: Option<String>,
}

impl TaskOutputPage {
    /// Merge the authoritative terminal evidence even when a live observer exists.
    /// The stable diagnostic identity prevents duplicate refreshes from adding rows.
    pub fn merge_terminal_diagnostic(
        &mut self,
        diagnostic: heycode_agent::TaskDiagnostic,
        limit: usize,
    ) {
        if limit == 0 {
            return;
        }
        if let Some(event) = self.events.iter_mut().find(|event| matches!(&event.kind, TaskOutputKind::Diagnostic { diagnostic: existing, .. } if existing.id == diagnostic.id)) {
            event.kind = TaskOutputKind::Diagnostic { diagnostic, terminal: true };
            return;
        }
        let sequence = self
            .events
            .last()
            .map_or(1, |event| event.sequence.saturating_add(1));
        self.events.push(TaskOutputEvent {
            sequence,
            kind: TaskOutputKind::Diagnostic {
                diagnostic,
                terminal: true,
            },
        });
        if self.events.len() > limit.min(OUTPUT_PAGE_SIZE) {
            self.events.remove(0);
            self.has_older = true;
        }
    }
}

/// Independent retained process stream. Native child conversations ignore this selector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskOutputChannel {
    /// Terminal for PTYs, stdout for pipe processes.
    #[default]
    Default,
    /// Standard output bytes.
    Stdout,
    /// Standard error bytes.
    Stderr,
    /// PTY output bytes.
    Terminal,
}
impl TaskOutputChannel {
    /// Human-facing channel label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Terminal => "terminal",
        }
    }
    /// Next channel in the explicit stream selector.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Default | Self::Terminal => Self::Stdout,
            Self::Stdout => Self::Stderr,
            Self::Stderr => Self::Terminal,
        }
    }
}

/// Human actions always retain the selected stable execution identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAction {
    /// Send an exact line to the selected running terminal.
    TerminalInput {
        /// Target job.
        key: TaskKey,
        /// Text typed in this task's own composer.
        text: String,
    },
    /// Submit text to this child, preserving the parent composer.
    Steer {
        /// Target child.
        key: TaskKey,
        /// Exact human text.
        text: String,
    },
    /// Cooperatively interrupt this execution.
    Interrupt(TaskKey),
    /// Retry a failed retained native run after reconciling partial effects.
    Retry(TaskKey),
    /// Close and settle this child.
    Close(TaskKey),
    /// Promote this exact foreground execution.
    Background(TaskKey),
}

impl TaskAction {
    /// Stable action target.
    #[must_use]
    pub fn key(&self) -> &TaskKey {
        match self {
            Self::Steer { key, .. }
            | Self::TerminalInput { key, .. }
            | Self::Interrupt(key)
            | Self::Retry(key)
            | Self::Close(key)
            | Self::Background(key) => key,
        }
    }
}

/// Execution owner's read/control contract. Implementations must filter child
/// identities by the root authority before returning data and recheck actions.
#[async_trait]
pub trait TaskSource: Send + Sync {
    /// Return a bounded current snapshot. Errors replace stale success state.
    fn snapshot(&self) -> Result<Vec<TaskRecord>, String>;
    /// Read the latest page, or an older page strictly before a source cursor.
    fn output(
        &self,
        key: &TaskKey,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TaskOutputPage, String>;
    /// Persist review/dismissal of one owned diagnostic without cancelling execution.
    fn acknowledge_issue(&self, _key: &TaskKey, _diagnostic_id: &str) -> Result<(), String> {
        Ok(())
    }
    /// Read a chosen process stream. Conversation sources use their ordinary event page.
    fn output_channel(
        &self,
        key: &TaskKey,
        before: Option<u64>,
        limit: usize,
        _channel: TaskOutputChannel,
    ) -> Result<TaskOutputPage, String> {
        self.output(key, before, limit)
    }
    /// Read pending human input for the root (None) or an authorized child.
    fn pending_messages(
        &self,
        _key: Option<&TaskKey>,
    ) -> Result<Vec<heycode_session::InboxMessage>, String> {
        Ok(Vec::new())
    }
    /// Atomically recall unconsumed human input from the selected owner.
    fn recall_messages(
        &self,
        _key: Option<&TaskKey>,
    ) -> Result<Vec<heycode_session::InboxMessage>, String> {
        Ok(Vec::new())
    }
    /// Synchronous durable admission for ordinary conversation text. None uses
    /// the asynchronous compatibility action path.
    fn enqueue_message(&self, _key: &TaskKey, _text: &str) -> Option<Result<String, String>> {
        None
    }
    /// Execute an action asynchronously. Cancelling is not a settled outcome.
    async fn execute(
        &self,
        action: TaskAction,
        cancellation: CancellationToken,
    ) -> Result<String, String>;
}

/// Navigation mode. Parent ingestion continues in every mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConsoleView {
    /// Persistent bottom strip only.
    #[default]
    Collapsed,
    /// Selectable task inventory.
    List,
    /// Selected task output plus its metadata and actions.
    Detail,
}

/// Separate conversation navigation from process and team inventories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskCategory {
    /// Compatibility inventory used by `/tasks`.
    #[default]
    All,
    /// Delegated conversations only.
    Agents,
    /// Background executions only.
    Jobs,
    /// Durable work items only.
    Work,
    /// Team coordination records only.
    Teams,
}

impl TaskCategory {
    /// User-facing name, independent of internal task identifiers.
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All activity",
            Self::Work => "Work",
            Self::Agents => "Agents",
            Self::Jobs => "Jobs",
            Self::Teams => "Teams",
        }
    }

    /// Whether this navigation category contains the record.
    pub const fn contains(self, kind: TaskKind) -> bool {
        matches!(
            (self, kind),
            (Self::All, _)
                | (Self::Agents, TaskKind::Child)
                | (Self::Jobs, TaskKind::Job)
                | (Self::Teams, TaskKind::Team)
                | (Self::Work, TaskKind::Work)
        )
    }
}

/// Mouse targets rebuilt by each frame (never guessed from terminal coordinates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskHit {
    /// Open one independent navigation category.
    Category(TaskCategory),
    /// Review the first unread failure without losing evidence.
    ReviewIssue,
    /// Clear attention on the selected failure without retrying or cancelling it.
    DismissIssue,
    /// Return focus to the main conversation.
    Main,
    /// Expand or collapse one selected conversation tool group.
    ToolDetails(String),
    /// Show raw retained events.
    Raw,
    /// Toggle the background output between bottom inspector and transcript area.
    ExpandOutput,
    /// Expand or collapse the task console.
    Toggle,
    /// Select and open a task.
    Open(TaskKey),
    /// Switch directly to an already foregrounded conversation.
    Switch(TaskKey),
    /// Open the selected conversation in the foreground.
    Foreground,
    /// Return to the parent and task list.
    Back,
    /// Interrupt selected task.
    Interrupt,
    /// Explicitly retry the selected failed native conversation.
    Retry,
    /// Close selected child.
    Close,
    /// Background selected execution.
    Background,
    /// Read older output.
    Older,
    /// Resume following latest output.
    Follow,
    /// Cycle stdout, stderr and terminal output.
    Channel,
    /// Toggle complete measured task metadata.
    Metadata,
}

type SavedOutputPosition = (usize, bool, Option<u64>, Vec<Option<u64>>);

/// Saved foreground state underneath an inspector; the composer never changes owner.
#[derive(Clone)]
pub(crate) struct TaskViewState {
    view: ConsoleView,
    category: TaskCategory,
    focused: Option<TaskKey>,
    focus_main: bool,
    strip_focused: bool,
    selected: Option<TaskKey>,
    pub(crate) active: bool,
    page: TaskOutputPage,
    output_scroll: usize,
    output_rows: usize,
    output_height: usize,
    follow: bool,
    before: Option<u64>,
    page_stack: Vec<Option<u64>>,
    channel: TaskOutputChannel,
    show_metadata: bool,
    raw_output: bool,
    notice: Option<String>,
}

/// The task that owns the foreground transcript and composer, independently of
/// a selector or read-only inspector. A missing record remains a task owner;
/// it must not make a renderer fall back to the parent conversation's data.
#[derive(Debug, Clone, Copy)]
pub struct ForegroundTask<'a> {
    /// Stable foreground identity, if the navigation state still has one.
    pub key: Option<&'a TaskKey>,
    /// Current authoritative record; absent after removal or a registry error.
    pub record: Option<&'a TaskRecord>,
}

/// TUI-owned navigation and bounded page cache, not an execution registry.
#[derive(Default)]
pub struct TaskConsole {
    pub(crate) source: Option<Arc<dyn TaskSource>>,
    /// Last authoritative inventory.
    pub records: Vec<TaskRecord>,
    acknowledged_issues: std::collections::BTreeSet<(TaskKey, String)>,
    /// Current navigation mode.
    pub view: ConsoleView,
    /// Filter for the selector only; authoritative records remain intact.
    pub category: TaskCategory,
    /// Selection survives inventory reordering by key.
    pub selected: Option<TaskKey>,
    /// Current bounded source page.
    pub page: TaskOutputPage,
    /// Rendered-row offset within the current page.
    pub output_scroll: usize,
    /// Selected process output stream.
    pub channel: TaskOutputChannel,
    /// Show complete measured metadata in the output viewport.
    pub show_metadata: bool,
    pub(crate) output_rows: usize,
    pub(crate) output_height: usize,
    /// Whether output tracks the newest source page.
    pub follow: bool,
    /// Last snapshot error; records remain the last known inventory until recovery.
    pub inventory_error: Option<String>,
    /// Last action acknowledgement or read error.
    pub notice: Option<String>,
    pub(crate) notices: BTreeMap<TaskKey, String>,
    /// Whether a task currently owns the transcript/composer, even while selecting another.
    pub active: bool,
    /// Inspect a background item without replacing the transcript or composer owner.
    pub preview: bool,
    pub(crate) preview_underlay: Option<TaskViewState>,
    /// Conversations explicitly brought into the foreground switcher.
    pub(crate) foregrounded: std::collections::BTreeSet<TaskKey>,
    /// Keyboard focus is independent of the viewed conversation.
    pub focused: Option<TaskKey>,
    pub(crate) strip_focused: bool,
    pub(crate) focus_main: bool,
    pub(crate) raw_output: bool,
    pub(crate) expanded_output: bool,
    pub(crate) expanded_tools: std::collections::BTreeSet<(TaskKey, String)>,
    pub(crate) tool_focus: Option<String>,
    pub(crate) rendered_tools: Vec<String>,
    pub(crate) positions: BTreeMap<TaskKey, SavedOutputPosition>,
    /// Frame-scoped mouse regions.
    pub hits: Vec<(Rect, TaskHit)>,
    /// Source pages to return to when paging forward.
    pub(crate) page_stack: Vec<Option<u64>>,
    pub(crate) before: Option<u64>,
    pub(crate) actions: VecDeque<TaskAction>,
    pub(crate) drafts: BTreeMap<TaskKey, tui_textarea::TextArea<'static>>,
}

impl TaskConsole {
    /// Number of actual failed runs awaiting human review; unrelated to running count.
    #[must_use]
    pub fn pending_issue_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| self.unread_issue(record))
            .count()
    }
    fn issue_id(record: &TaskRecord) -> String {
        record.telemetry.terminal_diagnostic.as_ref().map_or_else(
            || {
                format!(
                    "{}:{}",
                    record.key.0,
                    record
                        .telemetry
                        .finished
                        .as_deref()
                        .unwrap_or("unavailable")
                )
            },
            |diagnostic| diagnostic.id.clone(),
        )
    }
    fn unread_issue(&self, record: &TaskRecord) -> bool {
        record.kind == TaskKind::Child
            && record.status == TaskStatus::Failed
            && !record
                .telemetry
                .terminal_diagnostic
                .as_ref()
                .is_some_and(|diagnostic| diagnostic.acknowledged)
            && !self
                .acknowledged_issues
                .contains(&(record.key.clone(), Self::issue_id(record)))
    }
    /// Stable first unread failure target for keyboard and mouse activation.
    #[must_use]
    pub fn first_unread_issue(&self) -> Option<TaskKey> {
        self.records
            .iter()
            .find(|record| self.unread_issue(record))
            .map(|record| record.key.clone())
    }
    /// Reviewing clears attention only after durable acknowledgement succeeds.
    pub fn review_issue(&mut self, key: &TaskKey) {
        let Some(record) = self
            .records
            .iter()
            .find(|record| &record.key == key && record.status == TaskStatus::Failed)
        else {
            return;
        };
        if !self.unread_issue(record) {
            return;
        }
        let id = Self::issue_id(record);
        if record.telemetry.terminal_diagnostic.is_some()
            && let Some(source) = &self.source
            && let Err(error) = source.acknowledge_issue(key, &id)
        {
            self.notice = Some(format!("Could not mark issue reviewed: {}", safe(&error)));
            return;
        }
        self.acknowledged_issues.insert((key.clone(), id));
    }
    /// Dismiss attention while keeping the diagnostic, partial result, and history readable.
    pub fn dismiss_issue(&mut self, key: &TaskKey) {
        self.review_issue(key);
    }

    /// Resolve the transcript owner beneath any read-only inspector. `None`
    /// means the main conversation owns the view; a task with unavailable data
    /// is represented by `Some` with no record instead of borrowing main data.
    #[must_use]
    pub fn foreground(&self) -> Option<ForegroundTask<'_>> {
        let (active, key) = self
            .preview_underlay
            .as_ref()
            .map_or((self.active, self.selected.as_ref()), |view| {
                (view.active, view.selected.as_ref())
            });
        active.then(|| ForegroundTask {
            key,
            record: key.and_then(|key| self.records.iter().find(|row| &row.key == key)),
        })
    }

    pub(crate) fn capture_view(&self) -> TaskViewState {
        TaskViewState {
            view: self.view,
            category: self.category,
            focused: self.focused.clone(),
            focus_main: self.focus_main,
            strip_focused: self.strip_focused,
            selected: self.selected.clone(),
            active: self.active,
            page: self.page.clone(),
            output_scroll: self.output_scroll,
            output_rows: self.output_rows,
            output_height: self.output_height,
            follow: self.follow,
            before: self.before,
            page_stack: self.page_stack.clone(),
            channel: self.channel,
            show_metadata: self.show_metadata,
            raw_output: self.raw_output,
            notice: self.notice.clone(),
        }
    }
    pub(crate) fn apply_view(&mut self, view: TaskViewState) {
        self.view = view.view;
        self.category = view.category;
        self.focused = view.focused;
        self.focus_main = view.focus_main;
        self.strip_focused = view.strip_focused;
        self.selected = view.selected;
        self.active = view.active;
        self.page = view.page;
        self.output_scroll = view.output_scroll;
        self.output_rows = view.output_rows;
        self.output_height = view.output_height;
        self.follow = view.follow;
        self.before = view.before;
        self.page_stack = view.page_stack;
        self.channel = view.channel;
        self.show_metadata = view.show_metadata;
        self.raw_output = view.raw_output;
        self.notice = view.notice;
    }
    pub(crate) fn inspect(&mut self, key: TaskKey) {
        self.restore_preview();
        let mut saved = self.capture_view();
        if saved.view == ConsoleView::List {
            saved.focused = Some(key.clone());
            saved.focus_main = false;
        }
        self.selected = Some(key);
        self.open_selected();
        self.active = saved.active;
        self.preview_underlay = Some(saved);
        self.preview = true;
    }
    pub(crate) fn restore_preview(&mut self) {
        if let Some(saved) = self.preview_underlay.take() {
            self.apply_view(saved);
        }
        self.preview = false;
    }

    /// Current selector rows without changing the source inventory.
    pub fn visible_records(&self) -> Vec<&TaskRecord> {
        self.records
            .iter()
            .filter(|row| self.category.contains(row.kind))
            .collect()
    }

    /// Change categories without moving the active conversation or its draft.
    pub fn set_category(&mut self, category: TaskCategory) {
        self.category = category;
        self.focused = self.visible_records().first().map(|row| row.key.clone());
        self.focus_main = self.focused.is_none();
    }
    /// Notice for the selected owner only.
    #[must_use]
    pub fn selected_notice(&self) -> Option<&str> {
        self.selected
            .as_ref()
            .and_then(|key| self.notices.get(key))
            .map(String::as_str)
    }
    pub(crate) fn set_notice(&mut self, key: TaskKey, text: String) {
        self.notices.insert(key, safe(&text));
    }

    /// Attach the authoritative owner, then refresh its snapshot.
    pub fn attach(&mut self, source: Arc<dyn TaskSource>) {
        self.source = Some(source);
        self.follow = true;
        self.refresh();
    }

    /// Whether the strip should be present, including an empty attached registry.
    #[must_use]
    pub fn attached(&self) -> bool {
        self.source.is_some()
    }

    /// Current selected record, re-resolved after every refresh.
    #[must_use]
    pub fn selected_record(&self) -> Option<&TaskRecord> {
        self.selected
            .as_ref()
            .and_then(|key| self.records.iter().find(|row| &row.key == key))
    }

    /// Human-readable accessible status counts.
    #[must_use]
    pub fn summary(&self) -> String {
        Self::summary_for(&self.records.iter().collect::<Vec<_>>())
    }
    pub(crate) fn summary_for(records: &[&TaskRecord]) -> String {
        let running = records
            .iter()
            .filter(|r| {
                r.kind != TaskKind::Work
                    && matches!(r.status, TaskStatus::Running | TaskStatus::Starting)
            })
            .count();
        let waiting = records
            .iter()
            .filter(|r| {
                r.kind != TaskKind::Work
                    && matches!(r.status, TaskStatus::Waiting | TaskStatus::Queued)
            })
            .count();
        let cancelling = records
            .iter()
            .filter(|r| r.kind != TaskKind::Work && r.status == TaskStatus::Cancelling)
            .count();
        let failed = records
            .iter()
            .filter(|r| {
                r.kind != TaskKind::Work
                    && matches!(r.status, TaskStatus::Failed | TaskStatus::Interrupted)
            })
            .count();
        let executions = records
            .iter()
            .filter(|record| record.kind != TaskKind::Work)
            .count();
        let mut summary = if executions == 0 {
            format!("Tasks {}", records.len())
        } else {
            format!(
                "Tasks {} · {running} running · {waiting} waiting · {cancelling} cancelling · {failed} failed",
                records.len()
            )
        };
        let work = records
            .iter()
            .filter(|record| record.kind == TaskKind::Work)
            .copied()
            .collect::<Vec<_>>();
        if !work.is_empty() {
            let count = |status| work.iter().filter(|record| record.status == status).count();
            summary.push_str(&format!(
                " · Work {}: {} pending, {} in progress, {} blocked, {} completed, {} failed, {} cancelled",
                work.len(),
                count(TaskStatus::Queued),
                count(TaskStatus::Running),
                count(TaskStatus::Waiting),
                count(TaskStatus::Completed),
                count(TaskStatus::Failed),
                count(TaskStatus::Cancelled)
            ));
        }
        summary
    }

    /// Read state without creating or changing any execution.
    pub fn refresh(&mut self) {
        let Some(source) = self.source.as_ref() else {
            return;
        };
        match source.snapshot() {
            Ok(rows) => {
                self.inventory_error = None;
                self.records = rows
                    .into_iter()
                    .filter(|row| row.kind != TaskKind::Tool)
                    .collect();
                if self.selected.is_none() {
                    self.selected = self.records.first().map(|r| r.key.clone());
                }
            }
            Err(error) => {
                self.inventory_error = Some(safe(&format!("Task registry unavailable: {error}")));
            }
        }
        if (self.active || self.preview) && self.follow {
            self.read_page();
        }
    }

    /// Move inventory selection without depending on row order across updates.
    pub fn move_selection(&mut self, down: bool) {
        let records = self.visible_records();
        if records.is_empty() {
            self.focused = None;
            return;
        }
        let index = records
            .iter()
            .position(|r| Some(&r.key) == self.focused.as_ref())
            .unwrap_or(0);
        let next = if down {
            (index + 1).min(records.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.focused = Some(records[next].key.clone());
    }

    /// Open current task and start at its newest retained output.
    pub fn open_selected(&mut self) {
        if self.selected_record().is_none() {
            return;
        }
        self.notice = None;
        self.view = ConsoleView::Detail;
        self.active = true;
        self.focused = self.selected.clone();
        self.focus_main = false;
        self.tool_focus = None;
        self.raw_output = false;
        self.expanded_output = false;
        self.channel = TaskOutputChannel::Default;
        self.show_metadata = false;
        self.follow = true;
        self.before = None;
        self.page_stack.clear();
        self.output_scroll = 0;
        self.read_page();
        if let Some(key) = self.selected.clone() {
            self.review_issue(&key);
        }
    }

    /// Read the current page, showing errors instead of stale output.
    pub fn read_page(&mut self) {
        let (Some(source), Some(key)) = (&self.source, &self.selected) else {
            return;
        };
        self.page = source
            .output_channel(key, self.before, OUTPUT_PAGE_SIZE, self.channel)
            .unwrap_or_else(|error| {
                let mut page = TaskOutputPage {
                    unavailable: Some(safe(&error)),
                    ..TaskOutputPage::default()
                };
                if let Some(diagnostic) = self
                    .records
                    .iter()
                    .find(|record| &record.key == key)
                    .and_then(|record| record.telemetry.terminal_diagnostic.clone())
                {
                    page.merge_terminal_diagnostic(diagnostic, OUTPUT_PAGE_SIZE);
                }
                page
            });
        if self.before.is_none()
            && let Some(record) = self
                .records
                .iter()
                .find(|record| &record.key == key && record.status == TaskStatus::Failed)
            && !self.page.events.iter().any(|event| {
                matches!(
                    event.kind,
                    TaskOutputKind::Diagnostic { terminal: true, .. }
                )
            })
        {
            let diagnostic = record
                .telemetry
                .terminal_diagnostic
                .clone()
                .unwrap_or_else(|| heycode_agent::TaskDiagnostic {
                    id: Self::issue_id(record),
                    message: record.detail.clone().unwrap_or_else(|| {
                        "Failure diagnostics were not provided by this execution owner.".into()
                    }),
                    ..Default::default()
                });
            self.page
                .merge_terminal_diagnostic(diagnostic, OUTPUT_PAGE_SIZE);
        }
        self.page.events.truncate(OUTPUT_PAGE_SIZE);
    }

    /// Page backward at source cursor granularity, independent of output rate.
    pub fn older_page(&mut self) {
        if self.page.has_older
            && let Some(first) = self.page.events.first()
        {
            self.page_stack.push(self.before);
            self.before = Some(first.sequence);
            self.follow = false;
            self.output_scroll = 0;
            self.read_page();
        }
    }

    /// Return toward the newest source page.
    pub fn newer_page(&mut self) {
        if let Some(before) = self.page_stack.pop() {
            self.before = before;
            self.follow = before.is_none();
            self.output_scroll = 0;
            self.read_page();
        } else {
            self.follow_latest();
        }
    }

    /// Reattach the view to live output.
    pub fn follow_latest(&mut self) {
        self.before = None;
        self.page_stack.clear();
        self.follow = true;
        self.output_scroll = 0;
        self.read_page();
    }

    /// Queue only supported actions; execution owner rechecks capability on dispatch.
    pub fn queue(&mut self, action: TaskAction) {
        let Some(record) = self.records.iter().find(|r| &r.key == action.key()) else {
            self.notice = Some("Task is no longer available.".into());
            return;
        };
        let allowed = match &action {
            TaskAction::Steer { text, .. } => record.capabilities.steer && !text.trim().is_empty(),
            TaskAction::TerminalInput { text, .. } => {
                record.capabilities.terminal_input && !text.is_empty()
            }
            TaskAction::Interrupt(_) => record.capabilities.interrupt,
            TaskAction::Retry(_) => {
                record.status == TaskStatus::Failed && record.capabilities.retry
            }
            TaskAction::Close(_) => record.capabilities.close,
            TaskAction::Background(_) => record.capabilities.background,
        };
        if !allowed {
            self.notice = Some(if matches!(action, TaskAction::Retry(_)) {
                "Retry unavailable: only a failed run with its retained native conversation can be retried. Create a new task with an explicit prompt if its runtime is unavailable.".into()
            } else {
                "This action is unavailable in the task's current state.".into()
            });
        } else if matches!(action, TaskAction::Retry(_)) && self.actions.contains(&action) {
            self.notice = Some("Retry is already queued for this agent.".into());
        } else if self.actions.len() >= MAX_PENDING_ACTIONS {
            self.notice =
                Some("Task controls are busy; try again after an acknowledgement.".into());
        } else {
            self.actions.push_back(action);
        }
    }

    /// Control-free lines for both visual and screen-reader output renderers.
    #[must_use]
    /// Compact child activity for the bottom inspector; full output remains
    /// available in the foreground transcript and output pager.
    pub fn activity_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(diagnostic) = self.page.events.iter().find_map(|event| match &event.kind {
            TaskOutputKind::Diagnostic {
                diagnostic,
                terminal: true,
            } => Some(diagnostic),
            _ => None,
        }) {
            lines.push("Run failed".into());
            lines.extend(diagnostic_lines(diagnostic));
        }
        if let Some(reason) = &self.page.unavailable {
            lines.push(format!("Output unavailable: {}", safe(reason)));
        }
        let names = self
            .page
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                TaskOutputKind::ToolStarted { call_id, name }
                | TaskOutputKind::ToolMetadata { call_id, name, .. } => {
                    Some((call_id.as_str(), name.as_str()))
                }
                _ => None,
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let finished = self
            .page
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                TaskOutputKind::ToolFinished { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        fn excerpt(lines: &mut Vec<String>, text: &str) {
            let source = text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .collect::<Vec<_>>();
            for line in source.iter().take(3) {
                let text = safe(line);
                let clipped = text.chars().take(240).collect::<String>();
                lines.push(format!(
                    "  {clipped}{}",
                    if text.chars().count() > 240 {
                        "…"
                    } else {
                        ""
                    }
                ));
            }
            if source.len() > 3 {
                lines.push("  … more in foreground transcript".into());
            }
        }
        for event in &self.page.events {
            match &event.kind {
                TaskOutputKind::User(_) | TaskOutputKind::ToolMetadata { .. } => {}
                TaskOutputKind::ToolStarted { call_id, name }
                    if !finished.contains(call_id.as_str()) =>
                {
                    lines.push(format!("{} · running", safe(name)))
                }
                TaskOutputKind::ToolStarted { .. } => {}
                TaskOutputKind::ToolFinished { call_id, ok, text } => {
                    lines.push(format!(
                        "{} · {}",
                        safe(names.get(call_id.as_str()).copied().unwrap_or(call_id)),
                        if *ok { "completed" } else { "failed" }
                    ));
                    let parsed = serde_json::from_str::<serde_json::Value>(text).ok();
                    let content = parsed
                        .as_ref()
                        .and_then(|value| value.get("content"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(text);
                    excerpt(&mut lines, content);
                }
                TaskOutputKind::Text(text) => excerpt(&mut lines, text),
                TaskOutputKind::Reasoning(text) => {
                    if !text.trim().is_empty() {
                        lines.push("Reasoning".into());
                        excerpt(&mut lines, text);
                    }
                }
                TaskOutputKind::Diagnostic {
                    diagnostic,
                    terminal: false,
                } => {
                    lines.push("Execution diagnostic".into());
                    lines.extend(diagnostic_lines(diagnostic));
                }
                TaskOutputKind::Diagnostic { terminal: true, .. } => {}
                TaskOutputKind::Status(text) => excerpt(&mut lines, text),
            }
        }
        if lines.is_empty() {
            lines.push("No activity yet; waiting for the agent.".into());
        }
        lines
    }

    /// Full retained event text for the foreground transcript and output pager.
    pub fn output_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.page.truncated {
            lines.push("[Earlier output expired from retention]".into());
        }
        if let Some(reason) = &self.page.unavailable {
            lines.push(format!("Output unavailable: {}", safe(reason)));
        }
        for event in &self.page.events {
            match &event.kind {
                TaskOutputKind::User(text) => append_lines(&mut lines, "You: ", text),
                TaskOutputKind::Text(text) => append_lines(&mut lines, "", text),
                TaskOutputKind::ToolMetadata { name, args, .. } => {
                    lines.push(safe(&format!("{name}: {args}")))
                }
                TaskOutputKind::Reasoning(text) => append_lines(&mut lines, "Reasoning: ", text),
                TaskOutputKind::ToolStarted { call_id, name } => {
                    lines.push(safe(&format!("▶ running tool {name} · {call_id}")))
                }
                TaskOutputKind::ToolFinished { call_id, ok, text } => {
                    lines.push(safe(&format!(
                        "{} tool {call_id}",
                        if *ok { "✓ completed" } else { "! failed" }
                    )));
                    append_lines(&mut lines, "  ", text);
                }
                TaskOutputKind::Diagnostic { diagnostic, .. } => {
                    lines.extend(diagnostic_lines(diagnostic))
                }
                TaskOutputKind::Status(text) => append_lines(&mut lines, "· ", text),
            }
        }
        if lines.is_empty() {
            lines.push(
                if self
                    .selected_record()
                    .is_some_and(|record| record.kind == TaskKind::Work)
                {
                    "No Work record is available at this revision."
                } else {
                    "No output yet. This view follows live execution events."
                }
                .into(),
            );
        }
        lines
    }
}

/// Safe readable diagnostic facts shared by compact, full, and accessible views.
pub fn diagnostic_lines(diagnostic: &heycode_agent::TaskDiagnostic) -> Vec<String> {
    let mut lines = vec![safe(&diagnostic.message)];
    if let Some(stage) = &diagnostic.stage {
        lines.push(format!("Stage: {}", safe(stage)));
    }
    if let Some(code) = &diagnostic.code {
        lines.push(format!("Code: {}", safe(code)));
    }
    if let Some(run) = &diagnostic.run_id {
        lines.push(format!("Run: {}", safe(run)));
    }
    if let Some(partial) = &diagnostic.partial_result {
        lines.push(format!("Partial result: {}", safe(partial)));
    }
    if let Some(log) = &diagnostic.log_location {
        lines.push(format!("Retained log: {}", safe(log)));
    } else {
        lines.push("Retained log location was not provided by this execution owner.".into());
    }
    lines
}

fn append_lines(lines: &mut Vec<String>, prefix: &str, text: &str) {
    for line in text.lines().take(512) {
        lines.push(format!("{prefix}{}", safe(line)));
    }
}

/// Terminal-safe bounded human text. Newlines are handled by the caller.
#[must_use]
pub fn safe(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(4_096)
        .collect()
}

/// Owned asynchronous UI operations. Dropping this value cancels and aborts
/// every pending operation; no task outlives the terminal loop.
#[derive(Default)]
pub(crate) struct TaskOperations {
    tasks: tokio::task::JoinSet<(TaskAction, Result<String, String>)>,
    cancellation: CancellationToken,
}

impl TaskOperations {
    pub(crate) fn dispatch(&mut self, console: &mut TaskConsole) -> Vec<(TaskKey, String)> {
        let mut rejected = Vec::new();
        while self.tasks.len() < MAX_PENDING_ACTIONS {
            let Some(action) = console.actions.pop_front() else {
                break;
            };
            let Some(source) = console.source.clone() else {
                break;
            };
            if let TaskAction::Steer { key, text } = &action
                && heycode_agent::parse_slash(text).is_none()
                && let Some(result) = source.enqueue_message(key, text)
            {
                match result {
                    Ok(message) => console.set_notice(key.clone(), message),
                    Err(error) => {
                        console.set_notice(key.clone(), error);
                        rejected.push((key.clone(), text.clone()));
                    }
                }
                continue;
            }
            let cancellation = self.cancellation.child_token();
            self.tasks.spawn(async move {
                let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(source.execute(action.clone(), cancellation)))
                    .await.unwrap_or_else(|_| Err("Task control stopped unexpectedly; check its actual state before retrying.".into()));
                (action, result)
            });
        }
        while let Some(result) = self.tasks.try_join_next() {
            match result {
                Ok((action, result)) => {
                    let key = action.key().clone();
                    match result {
                        Ok(message) => console.set_notice(key, message),
                        Err(error) => {
                            console.set_notice(key, error);
                            if let TaskAction::Steer { key, text }
                            | TaskAction::TerminalInput { key, text } = action
                            {
                                rejected.push((key, text));
                            }
                        }
                    }
                }
                Err(_) => {
                    console.notice = Some(
                        "Task control stopped unexpectedly; refresh the task's actual state."
                            .into(),
                    )
                }
            }
        }
        rejected
    }
}

impl Drop for TaskOperations {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.tasks.abort_all();
    }
}

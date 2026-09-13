//! Bounded live projections of exact native child/session execution events.
//!
//! Subscription and replay occur while holding the session writer mutex, so
//! there is no replay-to-live gap. Callbacks reduce committed chunks directly;
//! the UI never races a transcript-file reader or copies child text to parent.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use heycode_agent::{Agent, ToolExecutionEvent, ToolExecutionPhase, UiEvent};
use heycode_session::{SessionEvent, SessionEventKind};

use crate::task_console::{
    OUTPUT_PAGE_SIZE, TaskCapabilities, TaskKey, TaskKind, TaskOutputEvent, TaskOutputKind,
    TaskOutputPage, TaskRecord, TaskStatus, TaskTelemetry,
};

const MAX_OUTPUT_EVENTS: usize = 1_024;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_EVENT_BYTES: usize = 32 * 1024;
const MAX_CALLS: usize = 128;

#[derive(Default)]
struct Segment {
    text: Option<u64>,
    reasoning: Option<u64>,
}

struct ToolRun {
    name: String,
    path: Option<String>,
    status: TaskStatus,
    started: Option<u64>,
    finished: Option<u64>,
    output: String,
    committed: bool,
}

/// An observation retains no child runtime handle. Its effect context removes
/// all subscriptions on drop, including after child closure and UI teardown.
pub(crate) struct ObservedTask {
    agent: Weak<Agent>,
    context: heycode_core::Context,
    events: VecDeque<TaskOutputEvent>,
    next_sequence: u64,
    bytes: usize,
    truncated: bool,
    segments: BTreeMap<(u64, u32), Segment>,
    tools: BTreeMap<String, ToolRun>,
    last_session_sequence: Option<u64>,
    started: Option<u64>,
    active_started: Option<u64>,
    elapsed_ms: u64,
    finished: Option<u64>,
    last_team_revision: Option<u64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    provider: String,
    model: String,
    session_id: String,
    changed_paths: BTreeSet<String>,
    pending_approvals: BTreeSet<u64>,
    turn_tool_errors: u32,
    initial_prompt: Option<String>,
}

impl ObservedTask {
    pub(crate) fn attach(agent: &Arc<Agent>) -> Result<Arc<Mutex<Self>>, String> {
        let selection = agent.selection();
        let session = agent
            .session()
            .lock()
            .map_err(|_| "Child session is unavailable")?;
        let observed = Arc::new(Mutex::new(Self {
            agent: Arc::downgrade(agent),
            context: heycode_core::Context::new(),
            events: VecDeque::new(),
            next_sequence: 1,
            bytes: 0,
            truncated: false,
            segments: BTreeMap::new(),
            tools: BTreeMap::new(),
            last_session_sequence: None,
            started: None,
            active_started: None,
            elapsed_ms: 0,
            finished: None,
            last_team_revision: None,
            input_tokens: None,
            output_tokens: None,
            provider: selection.provider_name,
            model: selection.model,
            session_id: session.id().to_string(),
            changed_paths: BTreeSet::new(),
            pending_approvals: BTreeSet::new(),
            turn_tool_errors: 0,
            initial_prompt: None,
        }));
        let weak = Arc::downgrade(&observed);
        {
            let mut state = observed
                .lock()
                .map_err(|_| "Child observation unavailable")?;
            session
                .bus()
                .on_effect::<SessionEvent>(&state.context, move |event| {
                    if let Some(observed) = weak.upgrade()
                        && let Ok(mut state) = observed.lock()
                    {
                        state.session_event(event);
                    }
                });
            let weak = Arc::downgrade(&observed);
            agent
                .ui()
                .on_effect::<ToolExecutionEvent>(&state.context, move |event| {
                    if let Some(observed) = weak.upgrade()
                        && let Ok(mut state) = observed.lock()
                    {
                        state.tool_event(event);
                    }
                });
            let weak = Arc::downgrade(&observed);
            agent
                .ui()
                .on_effect::<UiEvent>(&state.context, move |event| {
                    if let Some(observed) = weak.upgrade()
                        && let Ok(mut state) = observed.lock()
                    {
                        state.ui_event(event);
                    }
                });
            for event in session.events() {
                state.session_event(event);
            }
        }
        Ok(observed)
    }

    fn push(&mut self, kind: TaskOutputKind) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.events.push_back(TaskOutputEvent { sequence, kind });
        self.bound();
        sequence
    }

    fn bound(&mut self) {
        self.bytes = self.events.iter().map(event_bytes).sum();
        while self.events.len() > MAX_OUTPUT_EVENTS || self.bytes > MAX_OUTPUT_BYTES {
            if let Some(event) = self.events.pop_front() {
                self.bytes = self.bytes.saturating_sub(event_bytes(&event));
                self.truncated = true;
            } else {
                break;
            }
        }
        while self.tools.len() > MAX_CALLS {
            let first = self
                .tools
                .iter()
                .find(|(_, tool)| tool.committed)
                .map(|(id, _)| id.clone());
            if let Some(first) = first {
                self.tools.remove(&first);
            } else {
                break;
            }
        }
    }

    fn segment(&mut self, turn: u64, step: u32, reasoning: bool, text: &str, complete: bool) {
        let cursor = self.segments.get(&(turn, step)).and_then(|segment| {
            if reasoning {
                segment.reasoning
            } else {
                segment.text
            }
        });
        let existing = cursor.and_then(|cursor| {
            self.events
                .iter_mut()
                .find(|event| event.sequence == cursor)
        });
        let sequence = if let Some(event) = existing {
            let value = match &mut event.kind {
                TaskOutputKind::Text(text) | TaskOutputKind::Reasoning(text) => text,
                _ => return,
            };
            if complete {
                *value = bounded(text);
            } else {
                let remaining = MAX_EVENT_BYTES.saturating_sub(value.len());
                let end = boundary(text, remaining);
                value.push_str(&text[..end]);
                self.truncated |= end < text.len();
            }
            event.sequence
        } else {
            if text.is_empty() {
                return;
            }
            self.truncated |= text.len() > MAX_EVENT_BYTES;
            self.push(if reasoning {
                TaskOutputKind::Reasoning(bounded(text))
            } else {
                TaskOutputKind::Text(bounded(text))
            })
        };
        let segment = self.segments.entry((turn, step)).or_default();
        if reasoning {
            segment.reasoning = Some(sequence);
        } else {
            segment.text = Some(sequence);
        }
        self.bound();
    }

    fn session_event(&mut self, event: &SessionEvent) {
        if self
            .last_session_sequence
            .is_some_and(|sequence| sequence >= event.seq)
        {
            return;
        }
        self.last_session_sequence = Some(event.seq);
        let timestamp = u64::try_from(event.time_ms).unwrap_or(0);
        match &event.kind {
            SessionEventKind::TurnStart { .. } => {
                self.turn_tool_errors = 0;
                self.started.get_or_insert(timestamp);
                self.active_started = Some(timestamp);
                self.finished = None;
            }
            SessionEventKind::TurnEnd { reason, .. } => {
                self.finished = Some(timestamp);
                if let Some(start) = self.active_started.take() {
                    self.elapsed_ms = self
                        .elapsed_ms
                        .saturating_add(timestamp.saturating_sub(start));
                }
                for tool in self.tools.values_mut().filter(|tool| !tool.committed) {
                    tool.status = TaskStatus::Interrupted;
                    tool.finished.get_or_insert(timestamp);
                }
                self.push(TaskOutputKind::Status(format!("Turn settled: {reason:?}")));
            }
            SessionEventKind::TeamChange { .. } => self.last_team_revision = Some(event.seq),
            SessionEventKind::UserMessage { text } => {
                if self.initial_prompt.is_none() {
                    self.initial_prompt = Some(bounded(text));
                }
                self.push(TaskOutputKind::User(bounded(text)));
            }
            SessionEventKind::AgentInboxSplice { inserted, .. } => {
                for message in inserted {
                    self.push(TaskOutputKind::Status(format!(
                        "Queued {:?} {}: {}",
                        message.delivery(),
                        message.id().as_str(),
                        bounded(message.text())
                    )));
                }
            }
            SessionEventKind::AssistantChunk {
                turn,
                step,
                text,
                reasoning,
            } => {
                if let Some(text) = text {
                    self.segment(*turn, *step, false, text, false);
                }
                if let Some(reasoning) = reasoning {
                    self.segment(*turn, *step, true, reasoning, false);
                }
            }
            SessionEventKind::AssistantMessage {
                turn,
                step,
                content,
                reasoning,
                usage,
                ..
            } => {
                self.segment(*turn, *step, false, content, true);
                if let Some(reasoning) = reasoning {
                    self.segment(*turn, *step, true, reasoning, true);
                }
                self.segments.remove(&(*turn, *step));
                if let Some(usage) = usage {
                    self.input_tokens = Some(
                        self.input_tokens
                            .unwrap_or(0)
                            .saturating_add(usage.prompt_tokens),
                    );
                    self.output_tokens = Some(
                        self.output_tokens
                            .unwrap_or(0)
                            .saturating_add(usage.completion_tokens),
                    );
                }
            }
            SessionEventKind::ToolCall {
                call_id,
                name,
                args,
                ..
            } => {
                self.push(TaskOutputKind::ToolMetadata {
                    call_id: call_id.as_str().into(),
                    name: name.clone(),
                    args: args.clone(),
                });
                // A durable call may be announced after it ran. Never label it
                // running merely because the commit cursor reached it.
                self.tools
                    .entry(call_id.as_str().into())
                    .or_insert_with(|| ToolRun {
                        name: name.clone(),
                        path: None,
                        status: TaskStatus::Waiting,
                        started: None,
                        finished: None,
                        output: String::new(),
                        committed: false,
                    });
                if let Some(tool) = self.tools.get_mut(call_id.as_str()) {
                    tool.path = args
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                }
            }
            SessionEventKind::ToolResult {
                call_id,
                content,
                is_error,
                ..
            } => self.commit_tool(call_id.as_str(), content, *is_error, timestamp),
            SessionEventKind::RichToolResult {
                call_id,
                result,
                is_error,
                ..
            } => self.commit_tool(
                call_id.as_str(),
                &result.render_for_model(),
                *is_error,
                timestamp,
            ),
            SessionEventKind::RequestHeader { header, .. } => {
                self.provider.clone_from(&header.provider);
                self.model.clone_from(&header.model);
            }
            _ => {}
        }
        self.bound();
    }

    fn commit_tool(&mut self, call_id: &str, content: &str, is_error: bool, timestamp: u64) {
        if is_error {
            self.turn_tool_errors = self.turn_tool_errors.saturating_add(1);
        }
        if let Some(tool) = self.tools.get_mut(call_id) {
            tool.output = bounded(content);
            if !is_error
                && matches!(tool.name.as_str(), "edit" | "write")
                && let Some(path) = &tool.path
            {
                self.changed_paths.insert(path.clone());
            }
            tool.committed = true;
            tool.status = if is_error {
                TaskStatus::Failed
            } else {
                TaskStatus::Completed
            };
            tool.finished.get_or_insert(timestamp);
        }
        self.truncated |= content.len() > MAX_EVENT_BYTES;
        self.push(TaskOutputKind::ToolFinished {
            call_id: call_id.into(),
            ok: !is_error,
            text: bounded(content),
        });
    }

    fn tool_event(&mut self, event: &ToolExecutionEvent) {
        let id = event.call_id.as_str().to_owned();
        let tool = self.tools.entry(id.clone()).or_insert_with(|| ToolRun {
            name: event.name.clone(),
            path: None,
            status: TaskStatus::Waiting,
            started: None,
            finished: None,
            output: String::new(),
            committed: false,
        });
        match event.phase {
            ToolExecutionPhase::Admitting => tool.status = TaskStatus::Waiting,
            ToolExecutionPhase::Running => {
                tool.status = TaskStatus::Running;
                tool.started = Some(event.timestamp_ms);
                self.push(TaskOutputKind::ToolStarted {
                    call_id: id,
                    name: event.name.clone(),
                });
            }
            ToolExecutionPhase::Finished { ok } => {
                tool.status = TaskStatus::Waiting;
                tool.finished = Some(event.timestamp_ms);
                self.push(TaskOutputKind::Status(format!(
                    "Tool {} execution {} · awaiting ordered commit",
                    id,
                    if ok { "finished" } else { "failed" }
                )));
            }
            ToolExecutionPhase::Committed { ok } => {
                tool.status = if ok {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                };
                tool.committed = true;
            }
        }
        self.bound();
    }

    fn ui_event(&mut self, event: &UiEvent) {
        match event {
            UiEvent::Info { text } => {
                self.push(TaskOutputKind::Status(bounded(text)));
            }
            UiEvent::Error { message } => {
                self.push(TaskOutputKind::Diagnostic {
                    diagnostic: heycode_agent::TaskDiagnostic {
                        id: format!("{}:observed-error:{}", self.session_id, self.next_sequence),
                        message: bounded(message),
                        log_location: self.agent.upgrade().and_then(|agent| {
                            agent
                                .session()
                                .try_lock()
                                .ok()
                                .map(|session| session.path().display().to_string())
                        }),
                        ..heycode_agent::TaskDiagnostic::default()
                    },
                    terminal: false,
                });
            }
            UiEvent::ApprovalRequested { id, name, .. } => {
                self.pending_approvals.insert(*id);
                self.push(TaskOutputKind::Status(format!(
                    "Waiting for approval {id}: {name}"
                )));
            }
            UiEvent::ApprovalResolved { id, allowed } => {
                self.pending_approvals.remove(id);
                self.push(TaskOutputKind::Status(format!(
                    "Approval {id}: {}",
                    if *allowed { "allowed" } else { "denied" }
                )));
            }
            _ => {}
        }
    }

    pub(crate) fn telemetry(&self) -> TaskTelemetry {
        TaskTelemetry {
            initial_prompt: self.initial_prompt.clone(),
            command: None,
            deadline: None,
            tool_errors: self.turn_tool_errors,
            terminal_diagnostic: None,
            spawn_call_id: None,
            elapsed_ms: self.started.map(|_| {
                self.elapsed_ms.saturating_add(
                    self.active_started
                        .map_or(0, |start| now_ms().saturating_sub(start)),
                )
            }),
            current_tool: {
                let names = self
                    .tools
                    .values()
                    .filter(|tool| tool.status == TaskStatus::Running)
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>();
                if names.is_empty() {
                    None
                } else {
                    Some(names.join(", "))
                }
            },
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            runtime: Some(self.provider.clone()),
            model: Some(self.model.clone()),
            workspace: self
                .agent
                .upgrade()
                .map(|agent| agent.cwd().display().to_string()),
            started: self.started.map(crate::task_console::timestamp),
            finished: self.finished.map(crate::task_console::timestamp),
            output_bytes: Some(self.bytes as u64),
            cost: None,
            changed_paths: self.changed_paths.iter().cloned().collect(),
        }
    }

    pub(crate) fn output(&self, before: Option<u64>, limit: usize) -> TaskOutputPage {
        let end = before.unwrap_or(self.next_sequence);
        let available = self
            .events
            .iter()
            .filter(|event| event.sequence < end)
            .collect::<Vec<_>>();
        let start = available.len().saturating_sub(limit.min(OUTPUT_PAGE_SIZE));
        TaskOutputPage {
            events: available[start..]
                .iter()
                .map(|event| (*event).clone())
                .collect(),
            has_older: start > 0,
            truncated: self.truncated,
            unavailable: None,
        }
    }

    pub(crate) fn tool_records(&self) -> Vec<TaskRecord> {
        self.tools
            .iter()
            .filter(|(_, tool)| {
                !matches!(
                    tool.name
                        .strip_prefix("mcp__heycode__")
                        .unwrap_or(&tool.name),
                    "task" | "agent" | "spawn_agent" | "background_shell" | "background_terminal"
                )
            })
            .map(|(id, tool)| TaskRecord {
                key: TaskKey(format!("tool:{id}")),
                kind: TaskKind::Tool,
                label: tool.name.clone(),
                status: tool.status,
                parent: Some(self.session_id.clone()),
                session: Some(self.session_id.clone()),
                job: None,
                capabilities: TaskCapabilities {
                    output: true,
                    ..TaskCapabilities::default()
                },
                telemetry: TaskTelemetry {
                    elapsed_ms: tool
                        .started
                        .map(|start| tool.finished.unwrap_or_else(now_ms).saturating_sub(start)),
                    current_tool: Some(tool.name.clone()),
                    runtime: Some(self.provider.clone()),
                    model: Some(self.model.clone()),
                    workspace: self
                        .agent
                        .upgrade()
                        .map(|agent| agent.cwd().display().to_string()),
                    ..TaskTelemetry::default()
                },
                detail: Some(
                    if tool.committed {
                        "Result committed"
                    } else if tool.finished.is_some() {
                        "Execution finished; waiting for ordered result commit"
                    } else if tool.started.is_some() {
                        "Executing"
                    } else {
                        "Waiting for admission; not yet executing"
                    }
                    .into(),
                ),
            })
            .collect()
    }

    pub(crate) fn tool_output(&self, id: &str) -> Option<TaskOutputPage> {
        let tool = self.tools.get(id)?;
        let mut events = vec![TaskOutputEvent {
            sequence: 1,
            kind: TaskOutputKind::Status(format!(
                "Tool {} · {} · {}",
                tool.name,
                id,
                tool.status.label()
            )),
        }];
        if !tool.output.is_empty() {
            events.push(TaskOutputEvent {
                sequence: 2,
                kind: TaskOutputKind::Text(tool.output.clone()),
            });
        }
        Some(TaskOutputPage {
            events,
            ..TaskOutputPage::default()
        })
    }

    pub(crate) fn waiting_for_approval(&self) -> bool {
        !self.pending_approvals.is_empty()
    }

    pub(crate) fn team_revision(&self) -> Option<u64> {
        self.last_team_revision
    }

    pub(crate) fn alive(&self) -> bool {
        self.agent.strong_count() > 0
    }
}

impl Drop for ObservedTask {
    fn drop(&mut self) {
        self.context.shutdown();
    }
}

fn event_bytes(event: &TaskOutputEvent) -> usize {
    match &event.kind {
        TaskOutputKind::User(text)
        | TaskOutputKind::Text(text)
        | TaskOutputKind::Reasoning(text)
        | TaskOutputKind::Status(text) => text.len(),
        TaskOutputKind::Diagnostic { diagnostic, .. } => {
            diagnostic.id.len()
                + diagnostic.message.len()
                + diagnostic.partial_result.as_ref().map_or(0, String::len)
        }
        TaskOutputKind::ToolStarted { call_id, name } => call_id.len() + name.len(),
        TaskOutputKind::ToolMetadata {
            call_id,
            name,
            args,
        } => call_id.len() + name.len() + args.to_string().len(),
        TaskOutputKind::ToolFinished { call_id, text, .. } => call_id.len() + text.len(),
    }
}

fn boundary(text: &str, max: usize) -> usize {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}
fn bounded(text: &str) -> String {
    text[..boundary(text, MAX_EVENT_BYTES)].to_owned()
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

//! Adapter from composed execution and child registries to task UI contracts.

use crate::task_console::*;
use crate::task_observation::ObservedTask;
use async_trait::async_trait;
use heycode_agent::{JobRegistry, JobState, SubagentAuthority, SubagentId, SubagentRegistry};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const fn work_status_label(status: heycode_session::WorkStatus) -> &'static str {
    use heycode_session::WorkStatus;
    match status {
        WorkStatus::Pending => "pending",
        WorkStatus::InProgress => "in progress",
        WorkStatus::Blocked => "blocked",
        WorkStatus::Completed => "completed",
        WorkStatus::Failed => "failed",
        WorkStatus::Cancelled => "cancelled",
        WorkStatus::Deleted => "deleted",
    }
}

/// Baseline registry adapter. New lifecycle/output owners extend this one
/// boundary; no UI branch depends on a provider name.
pub struct RegistryTaskSource {
    agent: Arc<heycode_agent::Agent>,
    jobs: Option<Arc<JobRegistry>>,
    execution: Option<Arc<heycode_agent::ExecutionJobService>>,
    commands: Option<Arc<heycode_agent::CommandRegistry>>,
    subagents: Option<Arc<SubagentRegistry>>,
    authority: Option<SubagentAuthority>,
    root_output: Arc<Mutex<ObservedTask>>,
    child_output: Arc<Mutex<BTreeMap<String, Arc<Mutex<ObservedTask>>>>>,
    _child_observation: Option<heycode_agent::subagent_provider::NativeChildObservation>,
    work_cache: Mutex<Option<(usize, Arc<heycode_session::WorkProjection>)>>,
    team_cache: Mutex<(Option<u64>, heycode_session::TeamProjection)>,
}

impl RegistryTaskSource {
    fn conversation_agent(
        &self,
        key: Option<&TaskKey>,
    ) -> Result<Arc<heycode_agent::Agent>, String> {
        let Some(key) = key else {
            return Ok(self.agent.clone());
        };
        let id = key
            .0
            .strip_prefix("child:")
            .ok_or("This task has no conversation inbox")?;
        let id = SubagentId::new(id).map_err(|e| e.to_string())?;
        let registry = self
            .subagents
            .as_ref()
            .ok_or("Child registry unavailable")?;
        let authority = self
            .authority
            .as_ref()
            .ok_or("Child authority unavailable")?;
        registry
            .native_child_for(authority, &id)
            .ok_or("Child conversation is unavailable".into())
    }

    /// Attach only the composed root session and its actual registries.
    pub fn new(
        agent: Arc<heycode_agent::Agent>,
        jobs: Option<Arc<JobRegistry>>,
        subagents: Option<Arc<SubagentRegistry>>,
    ) -> Result<Self, String> {
        let owner = {
            let session = agent
                .session()
                .lock()
                .map_err(|_| "Root session unavailable")?;
            SubagentId::new(session.id().as_str()).map_err(|e| e.to_string())?
        };
        let authority = subagents
            .as_ref()
            .map(|registry| registry.root_authority(owner));
        let root_output = ObservedTask::attach(&agent)?;
        let child_output = Arc::new(Mutex::new(BTreeMap::new()));
        let child_observation = if let (Some(registry), Some(authority)) = (&subagents, &authority)
        {
            let observed = child_output.clone();
            let registry_weak = Arc::downgrade(registry);
            let observer_authority = authority.clone();
            let guard = registry.attach_native_child_observer(Arc::new(move |id, child| {
                let Some(registry) = registry_weak.upgrade() else {
                    return;
                };
                if registry
                    .task_snapshots_for(&observer_authority)
                    .iter()
                    .any(|record| record.id == id.as_str())
                {
                    observe_child(&observed, id.as_str(), &child);
                }
            }));
            for record in registry.task_snapshots_for(authority) {
                if let Ok(id) = SubagentId::new(record.id.clone())
                    && let Some(child) = registry.native_child_for(authority, &id)
                {
                    observe_child(&child_output, &record.id, &child);
                }
            }
            Some(guard)
        } else {
            None
        };
        Ok(Self {
            agent,
            jobs,
            execution: None,
            commands: None,
            subagents,
            authority,
            root_output,
            child_output,
            _child_observation: child_observation,
            work_cache: Mutex::new(None),
            team_cache: Mutex::new((None, heycode_session::TeamProjection::default())),
        })
    }

    /// Attach the existing execution service. The UI creates no process or terminal owner.
    #[must_use]
    pub fn with_execution(
        mut self,
        execution: Option<Arc<heycode_agent::ExecutionJobService>>,
    ) -> Self {
        self.execution = execution;
        self
    }

    /// Bind only the composed command registry for explicitly child-owned commands.
    #[must_use]
    pub fn with_commands(mut self, commands: Arc<heycode_agent::CommandRegistry>) -> Self {
        self.commands = Some(commands);
        self
    }

    fn process_output(
        &self,
        id: &str,
        before: Option<u64>,
        limit: usize,
        channel: TaskOutputChannel,
    ) -> Result<TaskOutputPage, String> {
        let execution = self
            .execution
            .as_ref()
            .ok_or("Execution output service unavailable")?;
        let id = heycode_agent::JobId::parse(id).map_err(|e| e.to_string())?;
        let output = execution
            .output(&id)
            .ok_or("This job has no retained process output")?;
        let stream = match channel {
            TaskOutputChannel::Default if execution.job_terminal(&id).is_some() => {
                heycode_exec::OutputStream::Terminal
            }
            TaskOutputChannel::Default | TaskOutputChannel::Stdout => {
                heycode_exec::OutputStream::Stdout
            }
            TaskOutputChannel::Stderr => heycode_exec::OutputStream::Stderr,
            TaskOutputChannel::Terminal => heycode_exec::OutputStream::Terminal,
        };
        let meta = output.read(stream, 0, 0);
        let end = before
            .map_or(meta.total_bytes, |cursor| cursor.saturating_sub(1))
            .min(meta.total_bytes)
            .max(meta.retained_from);
        let start = end
            .saturating_sub((limit.min(16) * 2048) as u64)
            .max(meta.retained_from);
        let mut cursor = start;
        let mut events = Vec::new();
        while cursor < end {
            let page = output.read(
                stream,
                cursor,
                usize::try_from(end - cursor)
                    .unwrap_or(usize::MAX)
                    .min(2048),
            );
            if page.next_offset <= cursor {
                break;
            }
            events.push(TaskOutputEvent {
                sequence: page.offset.saturating_add(1),
                kind: TaskOutputKind::Text(page.text),
            });
            cursor = page.next_offset;
        }
        Ok(TaskOutputPage {
            events,
            has_older: start > meta.retained_from,
            truncated: meta.retained_from > 0,
            unavailable: if output.persistence_error() {
                Some("Output checkpoint failed; currently retained output is readable but may not survive restart.".into())
            } else {
                None
            },
        })
    }

    fn work(&self) -> Result<Arc<heycode_session::WorkProjection>, String> {
        let session = self
            .agent
            .session()
            .lock()
            .map_err(|_| "Work session unavailable")?;
        let revision = session.events().len();
        let mut cache = self
            .work_cache
            .lock()
            .map_err(|_| "Work projection unavailable")?;
        let changed = cache.as_ref().is_none_or(|(seen, _)| {
            *seen > revision
                || session.events()[*seen..].iter().any(|event| {
                    matches!(
                        event.kind,
                        heycode_session::SessionEventKind::WorkChange { .. }
                            | heycode_session::SessionEventKind::TeamChange { .. }
                            | heycode_session::SessionEventKind::ToolResult { .. }
                    )
                })
        });
        if changed {
            *cache = Some((
                revision,
                Arc::new(
                    heycode_session::project_work_items(session.events())
                        .map_err(|error| error.to_string())?,
                ),
            ));
        } else if let Some((seen, _)) = cache.as_mut() {
            *seen = revision;
        }
        cache
            .as_ref()
            .map(|(_, projection)| projection.clone())
            .ok_or("Work projection unavailable".into())
    }

    fn teams(&self) -> Result<heycode_session::TeamProjection, String> {
        let revision = self
            .root_output
            .lock()
            .map_err(|_| "Root events unavailable")?
            .team_revision();
        let mut cache = self
            .team_cache
            .lock()
            .map_err(|_| "Team projection unavailable")?;
        if cache.0 != revision {
            let session = self
                .agent
                .session()
                .lock()
                .map_err(|_| "Root session unavailable")?;
            cache.1 =
                heycode_session::project_teams(session.events()).map_err(|e| e.to_string())?;
            cache.0 = revision;
        }
        Ok(cache.1.clone())
    }
}

#[async_trait]
impl TaskSource for RegistryTaskSource {
    fn enqueue_message(&self, key: &TaskKey, text: &str) -> Option<Result<String, String>> {
        Some((|| {
            let (Some(registry), Some(authority)) = (&self.subagents, &self.authority) else {
                return Err("Child registry unavailable".into());
            };
            let id = key
                .0
                .strip_prefix("child:")
                .ok_or("This owner cannot accept a conversation message")?;
            let id = SubagentId::new(id).map_err(|e| e.to_string())?;
            registry
                .queue_message(
                    authority,
                    &id,
                    text.to_owned(),
                    registry.native_child_for(authority, &id).is_some(),
                    heycode_session::InboxDelivery::Inject,
                )
                .map(|_| "Message queued".into())
                .map_err(|e| e.to_string())
        })())
    }

    fn pending_messages(
        &self,
        key: Option<&TaskKey>,
    ) -> Result<Vec<heycode_session::InboxMessage>, String> {
        Ok(self.conversation_agent(key)?.pending_human_messages())
    }
    fn recall_messages(
        &self,
        key: Option<&TaskKey>,
    ) -> Result<Vec<heycode_session::InboxMessage>, String> {
        self.conversation_agent(key)?
            .recall_human_messages()
            .map_err(|e| e.to_string())
    }

    fn snapshot(&self) -> Result<Vec<TaskRecord>, String> {
        let child_records = match (&self.subagents, &self.authority) {
            (Some(registry), Some(authority)) => registry.task_snapshots_for(authority),
            _ => Vec::new(),
        };
        let mut records = self
            .root_output
            .lock()
            .map_err(|_| "Root execution events unavailable")?
            .tool_records();
        records.retain(|record| {
            !child_records.iter().any(|child| {
                child
                    .spawn_call_id
                    .as_deref()
                    .is_some_and(|id| record.key.0 == format!("tool:{id}"))
            })
        });
        if let Some(jobs) = &self.jobs {
            for job in jobs.list() {
                if job.coordinator {
                    continue;
                }
                // One row per child execution; the job correlation remains in its details.
                if child_records
                    .iter()
                    .any(|child| child.job_id.as_deref() == Some(job.id.as_str()))
                {
                    continue;
                }
                let status = match job.state {
                    JobState::Queued => TaskStatus::Queued,
                    JobState::Cancelling => TaskStatus::Cancelling,
                    JobState::Running => TaskStatus::Running,
                    JobState::Settled(heycode_agent::JobOutcome::Completed) => {
                        TaskStatus::Completed
                    }
                    JobState::Settled(heycode_agent::JobOutcome::Cancelled) => {
                        TaskStatus::Cancelled
                    }
                    JobState::Settled(heycode_agent::JobOutcome::Interrupted) => {
                        TaskStatus::Interrupted
                    }
                    JobState::Settled(_) => TaskStatus::Failed,
                };
                let output = self
                    .execution
                    .as_ref()
                    .and_then(|service| service.output(&job.id));
                let metadata = output.as_ref().and_then(|output| output.metadata());
                records.push(TaskRecord {
                    key: TaskKey(format!("job:{}", job.id)),
                    kind: TaskKind::Job,
                    label: job.label,
                    status,
                    parent: None,
                    session: None,
                    job: Some(job.id.to_string()),
                    capabilities: TaskCapabilities {
                        interrupt: status.active() && status != TaskStatus::Cancelling,
                        output: self
                            .execution
                            .as_ref()
                            .is_some_and(|service| service.output(&job.id).is_some()),
                        terminal_input: self.execution.as_ref().is_some_and(|service| {
                            service.job_terminal(&job.id).is_some()
                                && service
                                    .output(&job.id)
                                    .is_some_and(|output| !output.ended())
                        }),
                        background: self
                            .execution
                            .as_ref()
                            .is_some_and(|service| service.foreground_jobs().contains(&job.id)),
                        ..TaskCapabilities::default()
                    },
                    telemetry: TaskTelemetry {
                        command: metadata.as_ref().map(|m| m.command.clone()),
                        deadline: metadata.as_ref().map(|m| {
                            m.timeout_ms
                                .map_or_else(|| "none".into(), |ms| format!("{}s", ms / 1000))
                        }),
                        elapsed_ms: metadata.as_ref().map(|m| m.elapsed_ms),
                        workspace: metadata.as_ref().map(|m| m.cwd.clone()),
                        started: metadata.as_ref().and_then(|m| m.started_ms).map(timestamp),
                        finished: metadata.as_ref().and_then(|m| m.finished_ms).map(timestamp),
                        output_bytes: self
                            .execution
                            .as_ref()
                            .and_then(|service| service.output(&job.id))
                            .map(|output| {
                                [
                                    heycode_exec::OutputStream::Stdout,
                                    heycode_exec::OutputStream::Stderr,
                                    heycode_exec::OutputStream::Terminal,
                                ]
                                .into_iter()
                                .map(|stream| {
                                    let page = output.read(stream, 0, 0);
                                    page.total_bytes.saturating_sub(page.retained_from)
                                })
                                .sum()
                            }),
                        ..TaskTelemetry::default()
                    },
                    detail: metadata.and_then(|m| m.reason),
                });
            }
        }
        if let (Some(registry), Some(authority)) = (&self.subagents, &self.authority) {
            for child in child_records {
                let id = SubagentId::new(child.id.clone()).map_err(|e| e.to_string())?;
                let mut status = match child.state {
                    heycode_agent::TaskState::Queued => TaskStatus::Queued,
                    heycode_agent::TaskState::Running => TaskStatus::Running,
                    heycode_agent::TaskState::Cancelling => TaskStatus::Cancelling,
                    heycode_agent::TaskState::Idle => TaskStatus::Idle,
                    heycode_agent::TaskState::Completed => TaskStatus::Completed,
                    heycode_agent::TaskState::Failed => TaskStatus::Failed,
                    heycode_agent::TaskState::Cancelled => TaskStatus::Cancelled,
                    heycode_agent::TaskState::Closed => TaskStatus::Closed,
                    heycode_agent::TaskState::Interrupted => TaskStatus::Interrupted,
                };
                let mut telemetry = self
                    .child_output
                    .lock()
                    .map_err(|_| "Child events unavailable")?
                    .get(&child.id)
                    .and_then(|observed| observed.lock().ok().map(|observed| observed.telemetry()))
                    .unwrap_or_else(|| TaskTelemetry {
                        runtime: Some(child.provider.clone()),
                        ..TaskTelemetry::default()
                    });
                telemetry.spawn_call_id = child.spawn_call_id.clone();
                telemetry.terminal_diagnostic = child.terminal_diagnostic.clone();
                telemetry.workspace = child.workspace.clone().or(telemetry.workspace);
                if status == TaskStatus::Running
                    && self
                        .child_output
                        .lock()
                        .map_err(|_| "Child events unavailable")?
                        .get(&child.id)
                        .is_some_and(|observed| {
                            observed
                                .lock()
                                .is_ok_and(|observed| observed.waiting_for_approval())
                        })
                {
                    status = TaskStatus::Waiting;
                }
                let can_message = (registry.native_child_for(authority, &id).is_some()
                    || registry.child_for(authority, &id).is_some())
                    && !matches!(
                        status,
                        TaskStatus::Closed | TaskStatus::Interrupted | TaskStatus::Completed
                    );
                records.push(TaskRecord {
                    key: TaskKey(format!("child:{}", child.id)),
                    kind: TaskKind::Child,
                    label: child.label,
                    status,
                    parent: Some(child.owner),
                    session: child.session_id,
                    job: child.job_id,
                    capabilities: TaskCapabilities {
                        output: true,
                        steer: can_message,
                        retry: registry.retry_available_for(authority, &id),
                        interrupt: status.active() && status != TaskStatus::Cancelling,
                        close: !status.active() && registry.child_for(authority, &id).is_some(),
                        ..TaskCapabilities::default()
                    },
                    detail: if status == TaskStatus::Failed {
                        Some(child.terminal_diagnostic.as_ref().map_or_else(
                            || {
                                if child.output.is_empty() {
                                    "The run ended without a retained diagnostic message.".into()
                                } else {
                                    child.output.clone()
                                }
                            },
                            |diagnostic| diagnostic.message.clone(),
                        ))
                    } else if child.output_truncated {
                        Some("Result summary truncated; inspect retained output.".into())
                    } else {
                        None
                    },
                    telemetry,
                });
            }
        }
        for team in self.teams()?.teams() {
            let tasks = team.tasks();
            records.push(TaskRecord {
                key: TaskKey(format!("team:{}", team.id().as_str())),
                kind: TaskKind::Team,
                label: format!(
                    "Team {} · {} members · {} tasks",
                    team.id().as_str(),
                    team.members().len(),
                    tasks.len()
                ),
                status: if matches!(
                    team.lifecycle(),
                    heycode_session::TeamLifecycle::Stopped
                        | heycode_session::TeamLifecycle::Archived
                ) {
                    TaskStatus::Closed
                } else if team.lifecycle() == heycode_session::TeamLifecycle::ShuttingDown {
                    TaskStatus::Cancelling
                } else if tasks
                    .iter()
                    .any(|t| t.state() == heycode_session::TeamTaskState::InProgress)
                {
                    TaskStatus::Running
                } else if tasks
                    .iter()
                    .any(|t| t.state() == heycode_session::TeamTaskState::Failed)
                {
                    TaskStatus::Failed
                } else if tasks.iter().any(|t| {
                    matches!(
                        t.state(),
                        heycode_session::TeamTaskState::Pending
                            | heycode_session::TeamTaskState::Blocked
                    )
                }) {
                    TaskStatus::Waiting
                } else if !tasks.is_empty()
                    && tasks
                        .iter()
                        .all(|t| t.state() == heycode_session::TeamTaskState::Completed)
                {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Idle
                },
                parent: Some(team.lead().as_str().into()),
                session: None,
                job: None,
                capabilities: TaskCapabilities {
                    output: true,
                    ..TaskCapabilities::default()
                },
                telemetry: TaskTelemetry::default(),
                detail: Some(format!(
                    "Team {:?} · revision {}",
                    team.lifecycle(),
                    team.revision()
                )),
            });
        }
        for item in self
            .work()?
            .items()
            .filter(|item| item.fields().status != heycode_session::WorkStatus::Deleted)
        {
            use heycode_session::WorkStatus;
            records.push(TaskRecord {
                key: TaskKey(format!("work:{}", item.id().as_str())),
                kind: TaskKind::Work,
                label: item.fields().subject.clone(),
                status: match item.fields().status {
                    WorkStatus::Pending => TaskStatus::Queued,
                    WorkStatus::InProgress => TaskStatus::Running,
                    WorkStatus::Blocked => TaskStatus::Waiting,
                    WorkStatus::Completed => TaskStatus::Completed,
                    WorkStatus::Failed => TaskStatus::Failed,
                    WorkStatus::Cancelled | WorkStatus::Deleted => TaskStatus::Cancelled,
                },
                parent: item.fields().owner.clone(),
                session: None,
                job: None,
                capabilities: TaskCapabilities {
                    output: true,
                    ..TaskCapabilities::default()
                },
                telemetry: TaskTelemetry::default(),
                detail: Some(format!(
                    "Revision {} · {} dependencies",
                    item.revision(),
                    item.fields().dependencies.len()
                )),
            });
        }
        Ok(records)
    }

    fn acknowledge_issue(&self, key: &TaskKey, diagnostic_id: &str) -> Result<(), String> {
        let id = key
            .0
            .strip_prefix("child:")
            .ok_or("This issue has no child owner")?;
        let (Some(registry), Some(authority)) = (&self.subagents, &self.authority) else {
            return Err("Child registry unavailable".into());
        };
        registry
            .acknowledge_task_diagnostic_for(
                authority,
                &SubagentId::new(id).map_err(|error| error.to_string())?,
                diagnostic_id,
            )
            .map_err(|error| error.to_string())
    }

    fn output(
        &self,
        key: &TaskKey,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TaskOutputPage, String> {
        if let Some(id) = key.0.strip_prefix("job:") {
            return self.process_output(id, before, limit, TaskOutputChannel::Default);
        }
        if let Some(id) = key.0.strip_prefix("tool:") {
            return self
                .root_output
                .lock()
                .map_err(|_| "Root execution events unavailable")?
                .tool_output(id)
                .ok_or_else(|| "Tool output is no longer retained".into());
        }
        if let Some(id) = key.0.strip_prefix("child:") {
            let (Some(registry), Some(authority)) = (&self.subagents, &self.authority) else {
                return Err("Child registry unavailable".into());
            };
            let record = registry
                .task_snapshots_for(authority)
                .into_iter()
                .find(|record| record.id == id)
                .ok_or("Child no longer available")?;
            let observed = self
                .child_output
                .lock()
                .map_err(|_| "Child events unavailable")?
                .get(id)
                .cloned();
            let mut page = if let Some(observed) = observed {
                observed
                    .lock()
                    .map(|observed| observed.output(before, limit))
                    .unwrap_or_else(|_| TaskOutputPage {
                        unavailable: Some(
                            "Live child output is unavailable; retained terminal evidence follows."
                                .into(),
                        ),
                        ..TaskOutputPage::default()
                    })
            } else {
                TaskOutputPage {
                    events: if record.output.is_empty()
                        || record.state == heycode_agent::TaskState::Failed
                        || limit == 0
                        || before.is_some_and(|before| before <= 1)
                    {
                        Vec::new()
                    } else {
                        vec![TaskOutputEvent {
                            sequence: 1,
                            kind: TaskOutputKind::Text(record.output.clone()),
                        }]
                    },
                    truncated: record.output_truncated,
                    unavailable: if record.state == heycode_agent::TaskState::Running {
                        Some("This runtime has not exposed a live conversation stream; its final result remains available here.".into())
                    } else {
                        None
                    },
                    ..TaskOutputPage::default()
                }
            };
            if before.is_none() && limit > 0 {
                for diagnostic in &record.diagnostics {
                    if page.events.iter().any(|event| matches!(&event.kind, TaskOutputKind::Diagnostic { diagnostic: existing, .. } if existing.id == diagnostic.id)) { continue; }
                    let sequence = page
                        .events
                        .last()
                        .map_or(1, |event| event.sequence.saturating_add(1));
                    page.events.push(TaskOutputEvent {
                        sequence,
                        kind: TaskOutputKind::Diagnostic {
                            diagnostic: diagnostic.clone(),
                            terminal: false,
                        },
                    });
                }
                while page.events.len() > limit.min(OUTPUT_PAGE_SIZE) {
                    page.events.remove(0);
                    page.has_older = true;
                }
            }
            if before.is_none() && record.state == heycode_agent::TaskState::Failed {
                let mut diagnostic = record.terminal_diagnostic.clone().unwrap_or_else(|| {
                    heycode_agent::TaskDiagnostic {
                        id: format!("{}:legacy-failure:{}", record.id, record.updated_at_ms),
                        message: if record.output.is_empty() {
                            "The run ended without a retained diagnostic message.".into()
                        } else {
                            record.output.clone()
                        },
                        ..heycode_agent::TaskDiagnostic::default()
                    }
                });
                if diagnostic.log_location.is_none() {
                    diagnostic.log_location = record.session_id.as_deref().and_then(|id| {
                        if id.is_empty()
                            || !id
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        {
                            return None;
                        }
                        self.agent.session().lock().ok().and_then(|session| {
                            session
                                .path()
                                .parent()
                                .and_then(std::path::Path::parent)
                                .map(|root| {
                                    root.join(id).join("session.jsonl").display().to_string()
                                })
                        })
                    });
                }
                page.merge_terminal_diagnostic(diagnostic, limit);
            }
            return Ok(page);
        }
        if let Some(id) = key.0.strip_prefix("work:") {
            let projection = self.work()?;
            let id = heycode_session::WorkItemId::new(id).map_err(|error| error.to_string())?;
            let item = projection.get(&id).ok_or("Work item unavailable")?;
            let sequence = item.revision();
            let dependencies = item
                .fields()
                .dependencies
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let mut content = format!(
                "Work ID: {}\nState: {}\nRevision: {}\nOwner: {}\nDependencies: {}\n\nDescription:\n{}\n\nMetadata: {}",
                item.id().as_str(),
                work_status_label(item.fields().status),
                item.revision(),
                item.fields().owner.as_deref().unwrap_or("unassigned"),
                if dependencies.is_empty() {
                    "none"
                } else {
                    &dependencies
                },
                item.fields().description,
                serde_json::to_string(&item.fields().metadata).map_err(|error| error.to_string())?
            );
            if let Some(result) = item.result_summary() {
                content.push_str("\n\nResult:\n");
                content.push_str(result);
            }
            return Ok(TaskOutputPage {
                events: if limit > 0 && before.is_none_or(|before| sequence < before) {
                    vec![TaskOutputEvent {
                        sequence,
                        kind: TaskOutputKind::Text(content),
                    }]
                } else {
                    Vec::new()
                },
                ..TaskOutputPage::default()
            });
        }
        if let Some(id) = key.0.strip_prefix("team:") {
            let projection = self.teams()?;
            let team = projection
                .teams()
                .into_iter()
                .find(|team| team.id().as_str() == id)
                .ok_or("Team no longer available")?;
            let mut lines = vec![format!(
                "Team {} · revision {} · lead {}",
                id,
                team.revision(),
                team.lead().as_str()
            )];
            for member in team.members() {
                lines.push(format!(
                    "Member {} · {} · {:?} · active task {}",
                    member.id().as_str(),
                    member.display(),
                    member.role(),
                    team.tasks()
                        .into_iter()
                        .find(|task| task.assignee() == member.id()
                            && task.state() == heycode_session::TeamTaskState::InProgress)
                        .map_or("none", |task| task.id().as_str())
                ));
            }
            for (mail, delivered, claimed) in team.mail() {
                lines.push(format!(
                    "Mail {} · {} → {} · delivered {} · claimed {} · {}",
                    mail.id().as_str(),
                    mail.from().as_str(),
                    mail.to().as_str(),
                    delivered,
                    claimed,
                    mail.body()
                ));
            }
            for task in team.tasks() {
                let dependencies = task
                    .dependencies()
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "Task {} · {:?} · {} · member {} · dependencies [{}] · {}",
                    task.id().as_str(),
                    task.state(),
                    task.title(),
                    task.assignee().as_str(),
                    dependencies,
                    if task.is_runnable(team) {
                        "ready"
                    } else {
                        "not ready"
                    }
                ));
                if let Some(result) = task.result_summary() {
                    lines.push(format!("Result: {result}"));
                }
            }
            let end = before
                .map_or(lines.len(), |n| {
                    usize::try_from(n).unwrap_or(usize::MAX).saturating_sub(1)
                })
                .min(lines.len());
            let start = end.saturating_sub(limit.min(OUTPUT_PAGE_SIZE));
            return Ok(TaskOutputPage {
                events: lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, text)| TaskOutputEvent {
                        sequence: (start + i + 1) as u64,
                        kind: TaskOutputKind::Status(text.clone()),
                    })
                    .collect(),
                has_older: start > 0,
                ..TaskOutputPage::default()
            });
        }
        Ok(TaskOutputPage {
            unavailable: Some("This runtime does not expose retained task output.".into()),
            ..TaskOutputPage::default()
        })
    }

    fn output_channel(
        &self,
        key: &TaskKey,
        before: Option<u64>,
        limit: usize,
        channel: TaskOutputChannel,
    ) -> Result<TaskOutputPage, String> {
        if let Some(id) = key.0.strip_prefix("job:") {
            self.process_output(id, before, limit, channel)
        } else {
            self.output(key, before, limit)
        }
    }

    async fn execute(
        &self,
        action: TaskAction,
        _cancellation: CancellationToken,
    ) -> Result<String, String> {
        if let TaskAction::Background(key) = &action
            && let Some(id) = key.0.strip_prefix("job:")
        {
            let id = heycode_agent::JobId::parse(id).map_err(|e| e.to_string())?;
            return if self
                .execution
                .as_ref()
                .is_some_and(|service| service.promote(&id))
            {
                Ok(format!(
                    "{id} moved to background; execution and output identity are unchanged."
                ))
            } else {
                Err("This execution is no longer eligible for foreground promotion.".into())
            };
        }
        if let TaskAction::TerminalInput { key, text } = &action {
            let id = heycode_agent::JobId::parse(
                key.0.strip_prefix("job:").ok_or("Not a process task")?,
            )
            .map_err(|e| e.to_string())?;
            let execution = self
                .execution
                .as_ref()
                .ok_or("Terminal service unavailable")?;
            let mut line = text.clone();
            if !line.ends_with('\n') {
                line.push('\n');
            }
            execution
                .write_terminal(&id, line.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            return Ok(format!("Input sent to task {id}."));
        }
        if let TaskAction::Interrupt(key) = &action
            && let Some(id) = key.0.strip_prefix("job:")
        {
            let jobs = self.jobs.as_ref().ok_or("Job registry unavailable")?;
            let id = heycode_agent::JobId::parse(id).map_err(|e| e.to_string())?;
            return if jobs.cancel(&id) {
                Ok("Cancellation requested; waiting for actual settlement.".into())
            } else {
                Err("Job already settled or unavailable.".into())
            };
        }
        let (Some(registry), Some(authority)) = (&self.subagents, &self.authority) else {
            return Err("Child registry unavailable".into());
        };
        let id = action
            .key()
            .0
            .strip_prefix("child:")
            .ok_or("This action is unavailable for this execution owner")?;
        let id = SubagentId::new(id).map_err(|e| e.to_string())?;
        match action {
            TaskAction::Steer { text, .. } => {
                if let Some((name, args)) = heycode_agent::parse_slash(&text) {
                    if ![
                        "btw",
                        "recap",
                        "questions",
                        "answer",
                        "output-style",
                        "scripts",
                    ]
                    .contains(&name.as_str())
                    {
                        return Err("This command is unavailable inside a child conversation. Use Esc to return to the parent; child commands are /btw, /recap, /questions, /answer, /output-style and /scripts.".into());
                    }
                    let child = registry
                        .native_child_for(authority, &id)
                        .ok_or("Child commands require a live native conversation")?;
                    let command = self
                        .commands
                        .as_ref()
                        .ok_or("Child command service unavailable")?
                        .get(&name)
                        .map_err(|e| e.to_string())?
                        .ok_or("This command is not configured in the current profile")?;
                    command
                        .execute(&child, &args)
                        .await
                        .map_err(|e| e.to_string())?;
                    return Ok(format!("/{name} completed in the selected child."));
                }
                registry
                    .send_background(
                        authority,
                        &id,
                        text,
                        registry.native_child_for(authority, &id).is_some(),
                        heycode_session::InboxDelivery::Inject,
                    )
                    .map(|job| format!("Child follow-up admitted as {job}."))
                    .map_err(|e| e.to_string())
            }
            TaskAction::Interrupt(_) => {
                if registry.interrupt_for(authority, &id) {
                    Ok("Child interruption requested; waiting for settlement.".into())
                } else {
                    Err("Child has no running turn.".into())
                }
            }
            TaskAction::Retry(_) => registry
                .retry_for(authority, &id)
                .map(|_| "Retry queued in the retained conversation. The agent will reconcile partial effects before starting a new run; the previous failure remains in history.".into())
                .map_err(|error| error.to_string()),
            TaskAction::Close(_) => registry
                .archive_child_for(authority, &id)
                .map(|removed| {
                    if removed {
                        "Child conversation closed.".into()
                    } else {
                        "Child already closed or unavailable.".into()
                    }
                })
                .map_err(|e| e.to_string()),
            TaskAction::TerminalInput { .. } => Err("This task has no terminal input".into()),
            TaskAction::Background(_) => {
                Err("This runtime does not support foreground promotion.".into())
            }
        }
    }
}

fn observe_child(
    observed: &Mutex<BTreeMap<String, Arc<Mutex<ObservedTask>>>>,
    id: &str,
    child: &Arc<heycode_agent::Agent>,
) {
    let Ok(mut rows) = observed.lock() else {
        return;
    };
    if rows.contains_key(id) {
        return;
    }
    if rows.len() >= 128 {
        let expired = rows
            .iter()
            .find(|(_, row)| row.lock().is_ok_and(|row| !row.alive()))
            .map(|(id, _)| id.clone());
        if let Some(expired) = expired {
            rows.remove(&expired);
        } else {
            return;
        }
    }
    if let Ok(child) = ObservedTask::attach(child) {
        rows.insert(id.into(), child);
    }
}

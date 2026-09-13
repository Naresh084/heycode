//! O12 — versioned workflow Provider, background service, and tool Consumer.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

use crate::code_mode::{EXECUTING_TOOLS, ToolExecutionContext};

use heycode_core::{CoreError, CoreResult, Plugin, ToolSpec};
use heycode_session::{
    InboxDelivery, Session, SessionEventKind, WorkflowAction, WorkflowCapability, WorkflowChange,
    WorkflowDefinition, WorkflowOutcome, WorkflowRunId, WorkflowState, project_workflows,
};
use heycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry};
use tokio_util::sync::CancellationToken;

use crate::{Agent, JobId, JobOutcome, JobRegistry, JobSettlement};

/// Stable workflow service failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowError {
    /// Definition or run id failed validation.
    #[error("workflow request is invalid")]
    Invalid,
    /// Required host capability is unavailable from the selected worker.
    #[error("workflow worker lacks a required capability")]
    Unsupported,
    /// Run does not exist or already settled.
    #[error("workflow run is not resumable")]
    NotFound,
    /// A live job already owns this durable run.
    #[error("workflow run is already active")]
    Conflict,
    /// Durable append/replay failed.
    #[error("workflow persistence failed")]
    Persistence,
    /// Service or job registry has stopped.
    #[error("workflow service is unavailable")]
    Unavailable,
}

/// Input handed to one workflow worker attempt.
#[derive(Debug, Clone)]
pub struct WorkflowWorkerRequest {
    run_id: WorkflowRunId,
    definition: WorkflowDefinition,
    completed_steps: u32,
    pause: CancellationToken,
}

impl WorkflowWorkerRequest {
    /// Cooperative pause request. Stop only after all admitted effects settle.
    #[must_use]
    pub fn pause_requested(&self) -> bool {
        self.pause.is_cancelled()
    }

    /// Stable run id.
    #[must_use]
    pub const fn run_id(&self) -> &WorkflowRunId {
        &self.run_id
    }

    /// Immutable definition.
    #[must_use]
    pub const fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }

    /// Durable completed prefix that must not be re-executed.
    #[must_use]
    pub const fn completed_steps(&self) -> u32 {
        self.completed_steps
    }
}

/// Worker settlement before the owning service appends `workflow/end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowWorkerResult {
    outcome: WorkflowOutcome,
    message: Option<String>,
}

impl WorkflowWorkerResult {
    /// Settlement at a safe checkpoint boundary, retaining a resumable run.
    #[must_use]
    pub fn paused() -> Self {
        Self {
            outcome: WorkflowOutcome::Paused,
            message: Some("workflow paused at a committed boundary".to_owned()),
        }
    }

    /// Successful completion.
    #[must_use]
    pub const fn completed() -> Self {
        Self {
            outcome: WorkflowOutcome::Completed,
            message: None,
        }
    }

    /// Cancellation settlement.
    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            outcome: WorkflowOutcome::Cancelled,
            message: Some("workflow cancelled".to_owned()),
        }
    }

    /// Failed settlement with bounded static text.
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            outcome: WorkflowOutcome::Failed,
            message: Some(bounded_workflow_message(&message.into())),
        }
    }
}

/// Replaceable worker Provider.
#[async_trait::async_trait]
pub trait WorkflowWorker: Send + Sync {
    /// Stable provider id.
    fn id(&self) -> &'static str;

    /// Explicit host capabilities implemented by this worker.
    fn capabilities(&self) -> &'static [WorkflowCapability];

    /// Native owner authority for explicit read/control calls. Replacement workers may omit it.
    fn owner_authority(&self) -> Option<&crate::SubagentAuthority> {
        None
    }

    /// Whether this provider implements version-two dependency graphs.
    fn supports_graph(&self) -> bool {
        false
    }

    /// Execute one attempt from its completed durable prefix.
    async fn run(
        &self,
        request: WorkflowWorkerRequest,
        reporter: WorkflowReporter,
        cancellation: CancellationToken,
    ) -> WorkflowWorkerResult;
}

/// Default deterministic in-process worker.
///
/// It is an API-shaping worker, not a security sandbox. Definitions can only
/// emit bounded JSON or wait on a bounded cancellable timer.
pub struct SequentialWorkflowWorker;

#[async_trait::async_trait]
impl WorkflowWorker for SequentialWorkflowWorker {
    fn id(&self) -> &'static str {
        "sequential-local"
    }

    fn capabilities(&self) -> &'static [WorkflowCapability] {
        &[WorkflowCapability::Progress, WorkflowCapability::Delay]
    }

    async fn run(
        &self,
        request: WorkflowWorkerRequest,
        reporter: WorkflowReporter,
        cancellation: CancellationToken,
    ) -> WorkflowWorkerResult {
        let start = match usize::try_from(request.completed_steps) {
            Ok(start) => start,
            Err(_) => return WorkflowWorkerResult::failed("workflow checkpoint is invalid"),
        };
        for (index, step) in request.definition.steps().iter().enumerate().skip(start) {
            if cancellation.is_cancelled() {
                return WorkflowWorkerResult::cancelled();
            }
            if request.pause_requested() {
                return WorkflowWorkerResult::paused();
            }
            let step_number = match u32::try_from(index.saturating_add(1)) {
                Ok(step) => step,
                Err(_) => return WorkflowWorkerResult::failed("workflow step index overflowed"),
            };
            if reporter.progress(step_number, step.label()).is_err() {
                return WorkflowWorkerResult::failed("workflow progress could not be committed");
            }
            let value = match step.action() {
                WorkflowAction::Tool { .. } | WorkflowAction::Agent { .. } => {
                    return WorkflowWorkerResult::failed(
                        "use the native graph worker for agent/tool actions",
                    );
                }
                WorkflowAction::Emit { value } => value.clone(),
                WorkflowAction::Delay { millis, value } => {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => {
                            return WorkflowWorkerResult::cancelled();
                        }
                        () = tokio::time::sleep(std::time::Duration::from_millis(*millis)) => {}
                    }
                    value.clone()
                }
            };
            if reporter.checkpoint(step_number, value).is_err() {
                return WorkflowWorkerResult::failed("workflow checkpoint could not be committed");
            }
        }
        WorkflowWorkerResult::completed()
    }
}

/// Provider-independent graph worker using the native host's guarded tools and
/// registry-owned children. JobRegistry remains the owner of the whole run.
pub struct NativeWorkflowWorker {
    agent: Arc<Agent>,
    subagents: Arc<crate::SubagentRegistry>,
    authority: crate::SubagentAuthority,
}

impl NativeWorkflowWorker {
    /// Bind the real execution host and its root authority.
    pub fn new(
        agent: Arc<Agent>,
        subagents: Arc<crate::SubagentRegistry>,
        authority: crate::SubagentAuthority,
    ) -> Self {
        Self {
            agent,
            subagents,
            authority,
        }
    }

    async fn action(
        &self,
        step: &heycode_session::WorkflowStep,
        attempt: u32,
        reporter: &WorkflowReporter,
        nodes: &BTreeMap<String, heycode_session::WorkflowNodeRecord>,
        cancellation: CancellationToken,
    ) -> Result<serde_json::Value, String> {
        match step.action() {
            WorkflowAction::Emit { value } => resolve_binding(value, nodes),
            WorkflowAction::Delay { millis, value } => {
                tokio::select! {
                    () = cancellation.cancelled() => return Err("workflow cancelled".to_owned()),
                    () = tokio::time::sleep(std::time::Duration::from_millis(*millis)) => {}
                }
                resolve_binding(value, nodes)
            }
            WorkflowAction::Tool { name, arguments } => {
                let arguments = resolve_binding(arguments, nodes)?;
                let execution = EXECUTING_TOOLS
                    .try_with(Clone::clone)
                    .unwrap_or_else(|_| self.agent.tool_execution_context());
                execution
                    .execute(name.clone(), arguments, cancellation)
                    .await
                    .map_err(|error| bounded_workflow_message(&error.to_string()))
            }
            WorkflowAction::Agent { prompt } => {
                let prompt = resolve_binding(prompt, nodes)?;
                let prompt = prompt
                    .as_str()
                    .ok_or_else(|| "agent prompt must resolve to a string".to_owned())?;
                let request = crate::SubagentRequest::with_authority(
                    step.label()
                        .chars()
                        .map(|ch| if ch.is_control() { ' ' } else { ch })
                        .collect::<String>(),
                    prompt,
                    crate::SubagentSeed::Fresh,
                    crate::SubagentContinuation::OneShot,
                    self.authority.clone(),
                )
                .map_err(|error| error.to_string())?
                .with_task_observer(reporter.task_observer(step, attempt, prompt)?);
                let result = self
                    .subagents
                    .start(
                        request.with_provider(
                            crate::SubagentProviderId::new("native")
                                .map_err(|error| error.to_string())?,
                        ),
                        cancellation.clone(),
                    )
                    .await;
                if cancellation.is_cancelled() {
                    return Err("workflow agent cancelled".to_owned());
                }
                result
                    .map(|started| serde_json::Value::String(started.text))
                    .map_err(|error| error.to_string())
            }
        }
    }
}

fn resolve_binding(
    value: &serde_json::Value,
    nodes: &BTreeMap<String, heycode_session::WorkflowNodeRecord>,
) -> Result<serde_json::Value, String> {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                let (id, pointer) = reference
                    .as_str()
                    .and_then(|reference| reference.split_once('#'))
                    .ok_or_else(|| "binding must be step#/pointer".to_owned())?;
                return nodes
                    .get(id)
                    .filter(|node| node.state == heycode_session::WorkflowNodeState::Completed)
                    .and_then(|node| node.value.pointer(pointer))
                    .cloned()
                    .ok_or_else(|| {
                        format!("binding {id}#{pointer} has no completed result at that pointer")
                    });
            }
            object
                .iter()
                .map(|(key, value)| Ok((key.clone(), resolve_binding(value, nodes)?)))
                .collect::<Result<serde_json::Map<_, _>, String>>()
                .map(serde_json::Value::Object)
        }
        serde_json::Value::Array(array) => array
            .iter()
            .map(|value| resolve_binding(value, nodes))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        value => Ok(value.clone()),
    }
}

#[async_trait::async_trait]
impl WorkflowWorker for NativeWorkflowWorker {
    fn supports_graph(&self) -> bool {
        true
    }
    fn owner_authority(&self) -> Option<&crate::SubagentAuthority> {
        Some(&self.authority)
    }

    fn id(&self) -> &'static str {
        "native-graph"
    }
    fn capabilities(&self) -> &'static [WorkflowCapability] {
        &[
            WorkflowCapability::Progress,
            WorkflowCapability::Delay,
            WorkflowCapability::Tool,
            WorkflowCapability::Agent,
        ]
    }
    async fn run(
        &self,
        request: WorkflowWorkerRequest,
        reporter: WorkflowReporter,
        cancellation: CancellationToken,
    ) -> WorkflowWorkerResult {
        crate::subagent::with_task_context(
            self.agent.clone(),
            self.authority.clone(),
            self.run_owned(request, reporter, cancellation),
        )
        .await
    }
}

impl NativeWorkflowWorker {
    async fn run_owned(
        &self,
        request: WorkflowWorkerRequest,
        reporter: WorkflowReporter,
        cancellation: CancellationToken,
    ) -> WorkflowWorkerResult {
        use futures::{StreamExt, stream::FuturesUnordered};
        use heycode_session::WorkflowNodeState as State;
        if !request.definition.is_graph() {
            return SequentialWorkflowWorker
                .run(request, reporter, cancellation)
                .await;
        }
        let mut nodes = match reporter.nodes() {
            Ok(nodes) => nodes,
            Err(_) => return WorkflowWorkerResult::failed("cannot read durable graph state"),
        };
        // A crash can occur after an external side effect and before its result
        // commits. Never infer that the effect did not happen, even when retry
        // was allowed for a known failed attempt.
        if nodes.values().any(|node| node.state == State::Started) {
            return WorkflowWorkerResult::failed(
                "interrupted step has an unknown effect outcome; reconcile it before starting replacement work",
            );
        }
        loop {
            if cancellation.is_cancelled() {
                return WorkflowWorkerResult::cancelled();
            }
            if nodes
                .values()
                .filter(|node| matches!(node.state, State::Completed | State::Skipped))
                .count()
                == request.definition.steps().len()
            {
                return WorkflowWorkerResult::completed();
            }
            if request.pause_requested() {
                return WorkflowWorkerResult::paused();
            }
            let ready: Vec<_> = request
                .definition
                .steps()
                .iter()
                .filter(|step| {
                    let may_start = nodes.get(step.id()).is_none_or(|node| {
                        node.state == State::Failed
                            && node.attempt < step.max_attempts()
                            && step.replay_safe()
                    });
                    may_start
                        && step.depends_on().iter().all(|id| {
                            nodes.get(id).is_some_and(|node| {
                                matches!(node.state, State::Completed | State::Skipped)
                            })
                        })
                })
                .take(request.definition.max_parallel())
                .collect();
            if ready.is_empty() {
                return WorkflowWorkerResult::failed(
                    "graph blocked by a failed dependency or exhausted retry budget",
                );
            }
            let mut pending = FuturesUnordered::new();
            for step in ready {
                let attempt = nodes.get(step.id()).map_or(1, |node| node.attempt + 1);
                let skip = step.depends_on().iter().any(|id| {
                    nodes
                        .get(id)
                        .is_some_and(|node| node.state == State::Skipped)
                });
                let condition = if skip {
                    Ok(false)
                } else {
                    step.when().map_or(Ok(true), |when| {
                        resolve_binding(when, &nodes).and_then(|value| {
                            if let Some(values) = value
                                .get("equals")
                                .and_then(serde_json::Value::as_array)
                                .filter(|values| values.len() == 2)
                            {
                                return Ok(values[0] == values[1]);
                            }
                            value.as_bool().ok_or_else(|| {
                                "when must resolve to a boolean or equals pair".to_owned()
                            })
                        })
                    })
                };
                if matches!(condition, Ok(false)) {
                    let record = heycode_session::WorkflowNodeRecord {
                        attempt,
                        state: State::Skipped,
                        value: serde_json::Value::Null,
                    };
                    if reporter.node(step.id(), record.clone()).is_err() {
                        return WorkflowWorkerResult::failed("skip checkpoint could not commit");
                    }
                    nodes.insert(step.id().to_owned(), record);
                    continue;
                }
                let record = heycode_session::WorkflowNodeRecord {
                    attempt,
                    state: State::Started,
                    value: serde_json::Value::Null,
                };
                if reporter.node(step.id(), record).is_err() {
                    return WorkflowWorkerResult::failed("step intent could not commit");
                }
                let token = cancellation.child_token();
                let inputs = nodes.clone();
                let node_reporter = reporter.clone();
                pending.push(async move {
                    if attempt > 1 {
                        tokio::select! {
                            () = token.cancelled() => return (step.id(), attempt, Err("workflow cancelled".to_owned())),
                            () = tokio::time::sleep(std::time::Duration::from_millis(100 * u64::from(attempt - 1))) => {}
                        }
                    }
                    let result = match condition { Ok(true) => self.action(step, attempt, &node_reporter, &inputs, token).await, Err(error) => Err(error), Ok(false) => Ok(serde_json::Value::Null) };
                    (step.id(), attempt, result)
                });
            }
            // Join every admitted effect, including on failure/cancellation. No
            // detached futures and no parallel owner writing a node twice.
            let mut persistence_failed = false;
            while let Some((id, attempt, result)) = pending.next().await {
                let record = match result {
                    Ok(value) => heycode_session::WorkflowNodeRecord {
                        attempt,
                        state: State::Completed,
                        value,
                    },
                    Err(error) => heycode_session::WorkflowNodeRecord {
                        attempt,
                        state: State::Failed,
                        value: serde_json::Value::String(bounded_workflow_message(&error)),
                    },
                };
                if reporter.node(id, record.clone()).is_err() {
                    persistence_failed = true;
                    cancellation.cancel();
                }
                nodes.insert(id.to_owned(), record);
            }
            if persistence_failed {
                return WorkflowWorkerResult::failed(
                    "step result could not commit; reconcile unknown effects before replacement work",
                );
            }
        }
    }
}

/// Durable observer/checkpoint handle supplied to a worker attempt.
#[derive(Clone)]
pub struct WorkflowReporter {
    session: Arc<std::sync::Mutex<Session>>,
    run_id: WorkflowRunId,
    progress_sequence: Arc<Mutex<u32>>,
}

impl std::fmt::Debug for WorkflowReporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowReporter")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

impl WorkflowReporter {
    fn task_observer(
        &self,
        step: &heycode_session::WorkflowStep,
        attempt: u32,
        prompt: &str,
    ) -> Result<Arc<crate::task_inventory::TaskObserver>, String> {
        let owner_session_id = self
            .session
            .lock()
            .map_err(|_| "workflow session unavailable")?
            .id()
            .to_string();
        let session = Arc::downgrade(&self.session);
        let run_id = self.run_id.clone();
        let node_id = step.id().to_owned();
        let label = step.label().to_owned();
        let (prompt, prompt_truncated) = bounded_observation_text(prompt, 16 * 1024);
        Ok(Arc::new(crate::task_inventory::TaskObserver::new(
            move |snapshot| {
                let session = session
                    .upgrade()
                    .ok_or_else(|| std::io::Error::other("workflow owner session closed"))?;
                let (summary, truncated) = bounded_observation_text(&snapshot.output, 32 * 1024);
                let record = heycode_session::WorkflowAgentRecord {
                    node_id: node_id.clone(),
                    node_attempt: attempt,
                    owner_session_id: owner_session_id.clone(),
                    task_id: snapshot.id.clone(),
                    session_id: snapshot.session_id.clone(),
                    job_id: snapshot.job_id.clone(),
                    label: label.clone(),
                    prompt: prompt.clone(),
                    prompt_truncated,
                    revision: snapshot.revision,
                    state: task_workflow_state(snapshot.state),
                    created_at_ms: snapshot.created_at_ms.min(i64::MAX as u64) as i64,
                    updated_at_ms: snapshot.updated_at_ms.min(i64::MAX as u64) as i64,
                    summary,
                    summary_truncated: truncated || snapshot.output_truncated,
                    usage: snapshot.usage,
                };
                append_workflow_change(
                    &session,
                    WorkflowChange::Agent {
                        version: 1,
                        run_id: run_id.clone(),
                        record,
                    },
                )
                .and_then(|()| flush_workflow_session(&session))
                .map_err(|error| std::io::Error::other(error.to_string()))
            },
        )))
    }

    fn nodes(
        &self,
    ) -> Result<BTreeMap<String, heycode_session::WorkflowNodeRecord>, WorkflowError> {
        let session = self
            .session
            .lock()
            .map_err(|_| WorkflowError::Persistence)?;
        let projection = project_owned_workflows(&session)?;
        Ok(projection
            .get(&self.run_id)
            .ok_or(WorkflowError::NotFound)?
            .nodes()
            .clone())
    }

    fn node(
        &self,
        id: &str,
        record: heycode_session::WorkflowNodeRecord,
    ) -> Result<(), WorkflowError> {
        append_workflow_change(
            &self.session,
            WorkflowChange::Node {
                version: 1,
                run_id: self.run_id.clone(),
                step_id: id.to_owned(),
                record,
            },
        )?;
        flush_workflow_session(&self.session)
    }

    /// Append one contiguous progress item.
    ///
    /// # Errors
    /// Invalid text/step or durable transaction failure.
    pub fn progress(&self, step: u32, message: &str) -> Result<(), WorkflowError> {
        let mut sequence = self
            .progress_sequence
            .lock()
            .map_err(|_| WorkflowError::Unavailable)?;
        let next = sequence.saturating_add(1);
        append_workflow_change(
            &self.session,
            WorkflowChange::progress(self.run_id.clone(), next, step, message)
                .map_err(|_| WorkflowError::Invalid)?,
        )?;
        *sequence = next;
        Ok(())
    }

    /// Commit one completed-prefix checkpoint.
    ///
    /// # Errors
    /// Invalid JSON/prefix or durable transaction failure.
    pub fn checkpoint(
        &self,
        completed_steps: u32,
        value: serde_json::Value,
    ) -> Result<(), WorkflowError> {
        append_workflow_change(
            &self.session,
            WorkflowChange::checkpoint(self.run_id.clone(), completed_steps, value)
                .map_err(|_| WorkflowError::Invalid)?,
        )?;
        flush_workflow_session(&self.session)
    }
}

fn bounded_observation_text(text: &str, limit: usize) -> (String, bool) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), end < text.len())
}

fn task_workflow_state(state: crate::TaskState) -> heycode_session::WorkflowAgentState {
    use crate::TaskState as T;
    use heycode_session::WorkflowAgentState as W;
    match state {
        T::Queued => W::Queued,
        T::Running => W::Running,
        T::Cancelling => W::Cancelling,
        T::Idle => W::Idle,
        T::Completed => W::Completed,
        T::Failed => W::Failed,
        T::Cancelled => W::Cancelled,
        T::Closed => W::Closed,
        T::Interrupted => W::Interrupted,
    }
}

fn project_owned_workflows(
    session: &Session,
) -> Result<heycode_session::WorkflowProjection, WorkflowError> {
    let local = session
        .events()
        .iter()
        .filter(|event| event.seq >= session.first_local_seq())
        .cloned()
        .collect::<Vec<_>>();
    project_workflows(&local).map_err(|_| WorkflowError::Persistence)
}

/// Accepted run/job identities returned before background execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStarted {
    run_id: WorkflowRunId,
    job_id: JobId,
}

impl WorkflowStarted {
    /// Durable run id.
    #[must_use]
    pub const fn run_id(&self) -> &WorkflowRunId {
        &self.run_id
    }

    /// Effect-owned background job id.
    #[must_use]
    pub const fn job_id(&self) -> &JobId {
        &self.job_id
    }
}

struct WorkflowRuntime {
    active: BTreeMap<WorkflowRunId, Option<JobId>>,
    pauses: BTreeMap<WorkflowRunId, CancellationToken>,
    closed: bool,
}

/// Workflow engine service over one replaceable worker Provider.
pub struct WorkflowService {
    session: Arc<std::sync::Mutex<Session>>,
    agent: Arc<Agent>,
    jobs: Arc<JobRegistry>,
    worker: Arc<dyn WorkflowWorker>,
    runtime: Mutex<WorkflowRuntime>,
}

impl std::fmt::Debug for WorkflowService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let active = self
            .runtime
            .lock()
            .map(|state| state.active.len())
            .unwrap_or(0);
        formatter
            .debug_struct("WorkflowService")
            .field("worker", &self.worker.id())
            .field("active", &active)
            .finish()
    }
}

impl WorkflowService {
    /// Bind one worker and validate the existing durable workflow stream.
    ///
    /// # Errors
    /// Malformed durable history fails before service publication.
    pub fn new(
        session: Arc<std::sync::Mutex<Session>>,
        agent: Arc<Agent>,
        jobs: Arc<JobRegistry>,
        worker: Arc<dyn WorkflowWorker>,
    ) -> Result<Self, WorkflowError> {
        {
            let session = session.lock().map_err(|_| WorkflowError::Unavailable)?;
            project_owned_workflows(&session)?;
        }
        Ok(Self {
            session,
            agent,
            jobs,
            worker,
            runtime: Mutex::new(WorkflowRuntime {
                active: BTreeMap::new(),
                pauses: BTreeMap::new(),
                closed: false,
            }),
        })
    }

    /// Start one immutable definition as attempt one.
    ///
    /// # Errors
    /// Invalid/unsupported definition, durable admission, or job spawn failure.
    pub fn start(
        self: &Arc<Self>,
        definition: WorkflowDefinition,
    ) -> Result<WorkflowStarted, WorkflowError> {
        let execution = self.capture_execution()?;
        definition.validate().map_err(|_| WorkflowError::Invalid)?;
        self.require_capabilities(&definition)?;
        let run_id = WorkflowRunId::generate();
        append_workflow_change(
            &self.session,
            WorkflowChange::start(run_id.clone(), definition.clone()),
        )?;
        flush_workflow_session(&self.session)?;
        self.spawn_attempt(run_id, definition, 0, 1, execution)
    }

    /// Resume an unterminated durable run from its latest checkpoint.
    ///
    /// # Errors
    /// Unknown/terminal/already-live runs, unsupported definitions, persistence,
    /// or job spawn failure.
    pub fn resume(
        self: &Arc<Self>,
        run_id: &WorkflowRunId,
    ) -> Result<WorkflowStarted, WorkflowError> {
        let execution = self.capture_execution()?;
        let projection = {
            let session = self
                .session
                .lock()
                .map_err(|_| WorkflowError::Unavailable)?;
            if !session.events().iter().any(|event| event.seq >= session.first_local_seq() && matches!(&event.kind, SessionEventKind::WorkflowChange { change } if matches!(change.as_ref(), WorkflowChange::Start { run_id: started, .. } if started == run_id))) {
                return Err(WorkflowError::NotFound);
            }
            project_owned_workflows(&session)?
        };
        let run = projection.get(run_id).ok_or(WorkflowError::NotFound)?;
        if !matches!(run.state(), WorkflowState::Running | WorkflowState::Paused) {
            return Err(WorkflowError::NotFound);
        }
        self.require_capabilities(run.definition())?;
        {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| WorkflowError::Unavailable)?;
            if runtime.active.contains_key(run_id) {
                return Err(WorkflowError::Conflict);
            }
        }
        let attempt = run.attempt().saturating_add(1);
        append_workflow_change(
            &self.session,
            WorkflowChange::resume(run_id.clone(), attempt, run.completed_steps())
                .map_err(|_| WorkflowError::Invalid)?,
        )?;
        flush_workflow_session(&self.session)?;
        self.spawn_attempt(
            run_id.clone(),
            run.definition().clone(),
            run.completed_steps(),
            attempt,
            execution,
        )
    }

    /// Request a cooperative pause after the currently admitted effects settle.
    /// Inspect the projection for authoritative Paused settlement.
    ///
    /// # Errors
    /// The run is unknown, already settled, or the service is unavailable.
    pub fn pause(&self, id: &WorkflowRunId) -> Result<(), WorkflowError> {
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| WorkflowError::Unavailable)?;
        runtime
            .pauses
            .get(id)
            .ok_or(WorkflowError::NotFound)?
            .cancel();
        Ok(())
    }

    /// Save a reusable definition durably without starting execution.
    ///
    /// # Errors
    /// Invalid definition, unsupported capability or persistence failure.
    pub fn save(&self, definition: WorkflowDefinition) -> Result<(), WorkflowError> {
        definition.validate().map_err(|_| WorkflowError::Invalid)?;
        self.require_capabilities(&definition)?;
        append_workflow_change(
            &self.session,
            WorkflowChange::Saved {
                version: 1,
                definition,
            },
        )?;
        flush_workflow_session(&self.session)
    }

    /// Start a new run from a saved definition. The run keeps an immutable copy.
    ///
    /// # Errors
    /// Unknown library name or ordinary start failure.
    pub fn run_saved(self: &Arc<Self>, name: &str) -> Result<WorkflowStarted, WorkflowError> {
        let projection = self.projection()?;
        self.start(
            projection
                .definitions()
                .get(name)
                .ok_or(WorkflowError::NotFound)?
                .clone(),
        )
    }

    /// Current durable projection.
    ///
    /// # Errors
    /// Malformed history or poisoned session state.
    pub fn projection(&self) -> Result<heycode_session::WorkflowProjection, WorkflowError> {
        let session = self
            .session
            .lock()
            .map_err(|_| WorkflowError::Unavailable)?;
        project_owned_workflows(&session)
    }

    /// Clone read-only progress for this service's physical owner session.
    pub fn views(&self) -> Result<Vec<crate::WorkflowView>, WorkflowError> {
        let runtime = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| WorkflowError::Unavailable)?;
            crate::workflow_view::WorkflowRuntimeView {
                active: runtime.active.clone(),
                pausing: runtime
                    .pauses
                    .iter()
                    .filter(|(_, token)| token.is_cancelled())
                    .map(|(id, _)| id.clone())
                    .collect(),
                closed: runtime.closed || self.agent.token().is_shutdown(),
            }
        };
        let session = self
            .session
            .lock()
            .map_err(|_| WorkflowError::Unavailable)?;
        crate::workflow_view::build_views(&session, now_ms(), &runtime)
    }
    /// Clone read-only progress for one owner-local run.
    pub fn view(&self, id: &WorkflowRunId) -> Result<crate::WorkflowView, WorkflowError> {
        self.views()?
            .into_iter()
            .find(|view| &view.run_id == id)
            .ok_or(WorkflowError::NotFound)
    }
    fn require_owner(&self, authority: &crate::SubagentAuthority) -> Result<(), WorkflowError> {
        if self.worker.owner_authority() != Some(authority) {
            return Err(WorkflowError::NotFound);
        }
        Ok(())
    }
    /// Explicit authority-checked progress for a model or task console owner.
    pub fn views_for(
        &self,
        authority: &crate::SubagentAuthority,
    ) -> Result<Vec<crate::WorkflowView>, WorkflowError> {
        self.require_owner(authority)?;
        self.views()
    }
    /// Explicit authority-checked run lookup.
    pub fn view_for(
        &self,
        authority: &crate::SubagentAuthority,
        id: &WorkflowRunId,
    ) -> Result<crate::WorkflowView, WorkflowError> {
        self.require_owner(authority)?;
        self.view(id)
    }
    /// Request a cooperative pause only for the native owning authority.
    pub fn pause_for(
        &self,
        authority: &crate::SubagentAuthority,
        id: &WorkflowRunId,
    ) -> Result<(), WorkflowError> {
        self.require_owner(authority)?;
        self.pause(id)
    }
    /// Resume only for the native owning authority, retaining completed effects.
    pub fn resume_for(
        self: &Arc<Self>,
        authority: &crate::SubagentAuthority,
        id: &WorkflowRunId,
    ) -> Result<WorkflowStarted, WorkflowError> {
        self.require_owner(authority)?;
        self.resume(id)
    }
    /// Cancel only for the native owning authority.
    pub fn cancel_for(
        &self,
        authority: &crate::SubagentAuthority,
        id: &WorkflowRunId,
    ) -> Result<bool, WorkflowError> {
        self.require_owner(authority)?;
        self.cancel(id)
    }
    /// Cancel live work through its owned job; stop paused/interrupted work without executing it.
    pub fn cancel(&self, id: &WorkflowRunId) -> Result<bool, WorkflowError> {
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| WorkflowError::Unavailable)?;
        if runtime.closed {
            return Err(WorkflowError::Unavailable);
        }
        if let Some(job) = runtime.active.get(id) {
            return job
                .as_ref()
                .map(|job| self.jobs.cancel(job))
                .ok_or(WorkflowError::Conflict);
        }
        let projection = self.projection()?;
        let run = projection.get(id).ok_or(WorkflowError::NotFound)?;
        if !matches!(run.state(), WorkflowState::Running | WorkflowState::Paused) {
            return Ok(false);
        }
        append_workflow_change(
            &self.session,
            WorkflowChange::end(
                id.clone(),
                WorkflowOutcome::Cancelled,
                run.completed_steps(),
                Some("workflow cancelled by owner".to_owned()),
            )
            .map_err(|_| WorkflowError::Invalid)?,
        )?;
        flush_workflow_session(&self.session)?;
        Ok(true)
    }

    /// Cancel every run this service started. JobRegistry remains the sole task owner.
    pub fn close(&self) {
        let jobs = self.runtime.lock().map_or_else(
            |_| Vec::new(),
            |mut runtime| {
                runtime.closed = true;
                runtime
                    .active
                    .values()
                    .filter_map(Clone::clone)
                    .collect::<Vec<_>>()
            },
        );
        for id in jobs {
            let _cancelled = self.jobs.cancel(&id);
        }
    }

    fn capture_execution(&self) -> Result<ToolExecutionContext, WorkflowError> {
        let execution = EXECUTING_TOOLS
            .try_with(Clone::clone)
            .unwrap_or_else(|_| self.agent.tool_execution_context());
        if !Arc::ptr_eq(&execution.session, &self.session) || execution.cancellation.is_shutdown() {
            return Err(WorkflowError::Unavailable);
        }
        Ok(execution)
    }

    fn require_capabilities(&self, definition: &WorkflowDefinition) -> Result<(), WorkflowError> {
        if definition.is_graph() && !self.worker.supports_graph() {
            return Err(WorkflowError::Unsupported);
        }
        let available = self
            .worker
            .capabilities()
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if definition
            .capabilities()
            .iter()
            .all(|capability| available.contains(capability))
        {
            Ok(())
        } else {
            Err(WorkflowError::Unsupported)
        }
    }

    fn spawn_attempt(
        self: &Arc<Self>,
        run_id: WorkflowRunId,
        definition: WorkflowDefinition,
        completed_steps: u32,
        attempt: u32,
        execution: ToolExecutionContext,
    ) -> Result<WorkflowStarted, WorkflowError> {
        {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| WorkflowError::Unavailable)?;
            if runtime.closed {
                return Err(WorkflowError::Unavailable);
            }
            if runtime.active.contains_key(&run_id) {
                return Err(WorkflowError::Conflict);
            }
            runtime.active.insert(run_id.clone(), None);
            runtime
                .pauses
                .insert(run_id.clone(), CancellationToken::new());
        }
        let label = format!("workflow: {}", definition.name());
        let service = self.clone();
        let task_run_id = run_id.clone();
        let spawned = self.jobs.spawn_coordinator(
            label,
            InboxDelivery::FollowUp,
            move |job_id, cancellation| {
                let service = service.clone();
                async move {
                    service.attach_job(&task_run_id, &job_id);
                    let admission = append_workflow_change(
                        &service.session,
                        WorkflowChange::Job {
                            version: 1,
                            run_id: task_run_id.clone(),
                            attempt,
                            job_id: job_id.as_str().to_owned(),
                        },
                    )
                    .and_then(|()| flush_workflow_session(&service.session));
                    if admission.is_err() {
                        cancellation.cancel();
                        let _ = WorkflowChange::end(
                            task_run_id.clone(),
                            WorkflowOutcome::Failed,
                            completed_steps,
                            Some("workflow coordinator admission could not be committed".to_owned()),
                        )
                        .map_err(|_| WorkflowError::Invalid)
                        .and_then(|change| append_workflow_change(&service.session, change))
                        .and_then(|()| flush_workflow_session(&service.session));
                        service.remove_active(&task_run_id);
                        let settlement = JobSettlement::bounded_or_failed(
                            JobOutcome::Failed,
                            "workflow coordinator admission could not be committed",
                        );
                        if execution.settle_job(&service.jobs, &job_id, &settlement).is_err() {
                            execution.bus.emit(crate::UiEvent::Error {
                                message: "workflow coordinator admission failed; no workflow work executed".to_owned(),
                            });
                        }
                        // JobRegistry's owned-future completion also marks any
                        // unsettled row Failed when the owner journal is unwritable.
                        return;
                    }
                    service
                        .execute_attempt(
                            task_run_id.clone(),
                            definition,
                            completed_steps,
                            job_id,
                            cancellation,
                            execution,
                        )
                        .await;
                }
            },
        );
        let job_id = match spawned {
            Ok(job_id) => job_id,
            Err(_) => {
                self.remove_active(&run_id);
                let projection = self.projection()?;
                let completed = projection
                    .get(&run_id)
                    .map_or(0, heycode_session::WorkflowRunProjection::completed_steps);
                append_workflow_change(
                    &self.session,
                    WorkflowChange::end(
                        run_id.clone(),
                        WorkflowOutcome::Failed,
                        completed,
                        Some("workflow job could not start".to_owned()),
                    )
                    .map_err(|_| WorkflowError::Invalid)?,
                )?;
                flush_workflow_session(&self.session)?;
                return Err(WorkflowError::Unavailable);
            }
        };
        Ok(WorkflowStarted { run_id, job_id })
    }

    async fn execute_attempt(
        self: Arc<Self>,
        run_id: WorkflowRunId,
        definition: WorkflowDefinition,
        completed_steps: u32,
        job_id: JobId,
        cancellation: CancellationToken,
        execution: ToolExecutionContext,
    ) {
        let progress_sequence = self
            .projection()
            .ok()
            .and_then(|projection| projection.get(&run_id).map(|run| run.progress_sequence()))
            .unwrap_or(0);
        let reporter = WorkflowReporter {
            session: self.session.clone(),
            run_id: run_id.clone(),
            progress_sequence: Arc::new(Mutex::new(progress_sequence)),
        };
        let work = self.worker.run(
            WorkflowWorkerRequest {
                run_id: run_id.clone(),
                definition,
                completed_steps,
                pause: self
                    .runtime
                    .lock()
                    .ok()
                    .and_then(|runtime| runtime.pauses.get(&run_id).cloned())
                    .unwrap_or_default(),
            },
            reporter,
            cancellation.clone(),
        );
        let result = execution
            .own_job(
                &cancellation,
                EXECUTING_TOOLS.scope(execution.clone(), work),
            )
            .await;
        let completed = self
            .projection()
            .ok()
            .and_then(|projection| projection.get(&run_id).map(|run| run.completed_steps()))
            .unwrap_or(completed_steps);
        let durable = WorkflowChange::end(
            run_id.clone(),
            result.outcome,
            completed,
            result.message.clone(),
        )
        .map_err(|_| WorkflowError::Invalid)
        .and_then(|change| append_workflow_change(&self.session, change))
        .and_then(|()| flush_workflow_session(&self.session));
        self.remove_active(&run_id);
        let (job_outcome, notice) = if durable.is_err() {
            (
                JobOutcome::Failed,
                "workflow settlement could not be committed".to_owned(),
            )
        } else {
            match result.outcome {
                WorkflowOutcome::Paused => (
                    JobOutcome::Completed,
                    format!(
                        "workflow {run_id} paused after {completed} step(s); use workflow resume"
                    ),
                ),
                WorkflowOutcome::Completed => (
                    JobOutcome::Completed,
                    format!("workflow {run_id} completed {completed} step(s)"),
                ),
                WorkflowOutcome::Failed => (
                    JobOutcome::Failed,
                    format!("workflow {run_id} failed after {completed} step(s)"),
                ),
                WorkflowOutcome::Cancelled => (
                    JobOutcome::Cancelled,
                    format!("workflow {run_id} cancelled after {completed} step(s)"),
                ),
            }
        };
        let settlement = JobSettlement::bounded_or_failed(job_outcome, notice);
        if execution
            .settle_job(&self.jobs, &job_id, &settlement)
            .is_err()
        {
            execution.bus.emit(crate::UiEvent::Error {
                message: "workflow job notice could not be committed".to_owned(),
            });
        }
    }

    fn attach_job(&self, run_id: &WorkflowRunId, job_id: &JobId) {
        if let Ok(mut runtime) = self.runtime.lock()
            && let Some(slot) = runtime.active.get_mut(run_id)
        {
            *slot = Some(job_id.clone());
        }
    }

    fn remove_active(&self, run_id: &WorkflowRunId) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.active.remove(run_id);
            runtime.pauses.remove(run_id);
        }
    }
}

fn append_workflow_change(
    session: &Arc<std::sync::Mutex<Session>>,
    change: WorkflowChange,
) -> Result<(), WorkflowError> {
    let mut session = session.lock().map_err(|_| WorkflowError::Unavailable)?;
    let mut candidate = session.events().to_vec();
    candidate.push(heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: u64::try_from(candidate.len()).map_err(|_| WorkflowError::Persistence)?,
        time_ms: now_ms(),
        kind: SessionEventKind::WorkflowChange {
            change: Box::new(change.clone()),
        },
    });
    let local = candidate
        .into_iter()
        .filter(|event| event.seq >= session.first_local_seq())
        .collect::<Vec<_>>();
    project_workflows(&local).map_err(|_| WorkflowError::Persistence)?;
    session
        .append(SessionEventKind::WorkflowChange {
            change: Box::new(change),
        })
        .map_err(|_| WorkflowError::Persistence)?;
    Ok(())
}

fn flush_workflow_session(session: &Arc<std::sync::Mutex<Session>>) -> Result<(), WorkflowError> {
    session
        .lock()
        .map_err(|_| WorkflowError::Unavailable)?
        .flush()
        .map_err(|_| WorkflowError::Persistence)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn bounded_workflow_message(message: &str) -> String {
    let normalized = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if normalized.trim().is_empty() {
        return "workflow failed".to_owned();
    }
    let mut end = normalized.len().min(1024);
    while !normalized.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    normalized[..end].to_owned()
}

/// Complete model-facing schema for legacy and native graph definitions.
#[must_use]
pub fn workflow_definition_schema() -> serde_json::Value {
    let binding = serde_json::json!({"type":"object","additionalProperties":false,"required":["$ref"],"properties":{"$ref":{"type":"string","description":"Declared dependency step id, #, then a JSON pointer; for example draft#/text or draft# for the whole result."}}});
    let condition = serde_json::json!({"description":"False skips this step and its dependants. Use equals to select branches from bound JSON results.","oneOf":[{"type":"boolean"},binding.clone(),{"type":"object","additionalProperties":false,"required":["equals"],"properties":{"equals":{"type":"array","minItems":2,"maxItems":2,"items":{}}}}]});
    let emit_action = serde_json::json!({"type":"object","additionalProperties":false,"required":["kind","value"],"properties":{"kind":{"const":"emit"},"value":{"description":"JSON result. Bind a dependency with the exact object {$ref:'step#/pointer'}."}}});
    let delay_action = serde_json::json!({"type":"object","additionalProperties":false,"required":["kind","millis","value"],"properties":{"kind":{"const":"delay"},"millis":{"type":"integer","minimum":1,"maximum":86400000},"value":{}}});
    let tool_action = serde_json::json!({"type":"object","additionalProperties":false,"required":["kind","name","arguments"],"properties":{"kind":{"const":"tool"},"name":{"type":"string","minLength":1,"maxLength":128},"arguments":{"description":"Tool arguments, recursively resolved from dependency result bindings; execution uses host approval and guards."}}});
    let agent_action = serde_json::json!({"type":"object","additionalProperties":false,"required":["kind","prompt"],"properties":{"kind":{"const":"agent"},"prompt":{"oneOf":[{"type":"string","minLength":1,"maxLength":65536},binding],"description":"Runs a fresh native one-shot child using the configured inference route."}}});
    serde_json::json!({
        "type":"object", "additionalProperties":false,
        "required":["version","name","description","capabilities","steps"],
        "properties":{
            "version":{"type":"integer","enum":[1,2],"description":"Use 2 for native agent/tool dependency graphs; 1 preserves ordered emit/delay workflows."},
            "name":{"type":"string","minLength":1,"maxLength":64,"pattern":"^[A-Za-z0-9_.:-]+$"},
            "title":{"type":"string","minLength":1,"maxLength":256,"description":"Explicit human-facing workflow title."},
            "phases":{"type":"array","maxItems":64,"items":{"type":"object","additionalProperties":false,"required":["id","title"],"properties":{"id":{"type":"string","minLength":1,"maxLength":64},"title":{"type":"string","minLength":1,"maxLength":256}}},"description":"Phases in planned order. Every step must reference one; omitted phases means one phase per step. Metadata does not add execution dependencies."},
            "description":{"type":"string","minLength":1,"maxLength":1024},
            "capabilities":{"type":"array","uniqueItems":true,"items":{"enum":["progress","delay","tool","agent"]},"description":"Include progress and the capability for every action used."},
            "max_parallel":{"type":"integer","minimum":1,"maximum":8,"default":4},
            "steps":{"type":"array","minItems":1,"maxItems":64,"items":{
                "type":"object","additionalProperties":false,"required":["id","label","action"],
                "properties":{
                    "id":{"type":"string","minLength":1,"maxLength":64,"pattern":"^[A-Za-z0-9_.:-]+$"},
                    "label":{"type":"string","minLength":1,"maxLength":256},
                    "phase_id":{"type":"string","minLength":1,"maxLength":64,"description":"Required when definition phases are supplied; exact declared phase id."},
                    "depends_on":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string"},"description":"All must settle before this step. Skipped dependencies propagate skip. Cycles are rejected."},
                    "when":condition,
                    "max_attempts":{"type":"integer","minimum":1,"maximum":5,"default":1},
                    "replay_safe":{"type":"boolean","default":false,"description":"Assert the effect is safe to repeat after a known failed attempt. Crash-left unknown effects are never replayed."},
                    "action":{"oneOf":[emit_action,delay_action,tool_action,agent_action]}
                }
            }}
        }
    })
}

type WorkflowOwners = Arc<Mutex<BTreeMap<String, Weak<WorkflowService>>>>;

struct WorkflowTool {
    service: Arc<WorkflowService>,
    subagents: Option<Arc<crate::SubagentRegistry>>,
    owners: WorkflowOwners,
}

impl WorkflowTool {
    fn owner_service(&self) -> Result<Arc<WorkflowService>, ToolError> {
        let execution = match EXECUTING_TOOLS.try_with(Clone::clone) {
            Ok(execution) => execution,
            Err(_) if crate::subagent::scoped_authority().is_none() => {
                return Ok(self.service.clone());
            }
            Err(_) => {
                return Err(ToolError::new(
                    "workflow requires the caller's active tool execution context",
                ));
            }
        };
        if execution.cancellation.is_shutdown() {
            return Err(ToolError::new("workflow owner has closed"));
        }
        if Arc::ptr_eq(&execution.session, &self.service.session) {
            if let Some(authority) = crate::subagent::scoped_authority() {
                let session = execution
                    .session
                    .lock()
                    .map_err(|_| ToolError::new("workflow session unavailable"))?;
                if authority.owner().as_str() != session.id().as_str() {
                    return Err(ToolError::new(
                        "workflow authority does not own this session",
                    ));
                }
            }
            return Ok(self.service.clone());
        }
        let authority = crate::subagent::scoped_authority()
            .ok_or_else(|| ToolError::new("child workflow has no native owner authority"))?;
        let subagents = self
            .subagents
            .as_ref()
            .ok_or_else(|| ToolError::new("child workflows require the native worker"))?;
        let agent = subagents
            .agent_for_authority(&authority)
            .ok_or_else(|| ToolError::new("workflow owner conversation is unavailable"))?;
        if !Arc::ptr_eq(agent.session(), &execution.session) {
            return Err(ToolError::new(
                "workflow authority does not own this session",
            ));
        }
        let owner = execution
            .session
            .lock()
            .map_err(|_| ToolError::new("workflow session unavailable"))?
            .id()
            .to_string();
        let mut owners = self
            .owners
            .lock()
            .map_err(|_| ToolError::new("workflow owner registry unavailable"))?;
        owners.retain(|_, service| service.strong_count() > 0);
        if let Some(service) = owners.get(&owner).and_then(Weak::upgrade) {
            if !Arc::ptr_eq(&service.session, &execution.session) {
                return Err(ToolError::new(
                    "workflow session writer changed while work is active",
                ));
            }
            return Ok(service);
        }
        if owners.len() >= 64 {
            return Err(ToolError::new(
                "workflow owner capacity reached (64 active conversations)",
            ));
        }
        let worker = Arc::new(NativeWorkflowWorker::new(
            agent.clone(),
            subagents.clone(),
            authority,
        ));
        let service = Arc::new(
            WorkflowService::new(execution.session, agent, self.service.jobs.clone(), worker)
                .map_err(|error| ToolError::new(error.to_string()))?,
        );
        owners.insert(owner, Arc::downgrade(&service));
        Ok(service)
    }
}

#[async_trait::async_trait]
impl Tool for WorkflowTool {
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::tool_orchestration())
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "workflow".to_owned(),
            description: "Start, pause, resume, save or inspect native agent/tool workflow graphs. Use version 2, explicit dependencies and result bindings. Pause settles currently running effects before becoming resumable. Saved definitions are session-local.".to_owned(),
            parameters: serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "required":["action"],
                "properties":{
                    "action":{"enum":["start","resume","pause","cancel","list","save","saved","run_saved"]},
                    "definition":workflow_definition_schema(),
                    "run_id":{"type":"string"},
                    "name":{"type":"string","description":"Saved definition name for run_saved"}
                }
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        _cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let service = self.owner_service()?;
        let action = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`action` must be a string"))?;
        match action {
            "start" | "save" => {
                let definition: WorkflowDefinition = serde_json::from_value(
                    args.get("definition")
                        .cloned()
                        .ok_or_else(|| ToolError::new("`definition` is required"))?,
                )
                .map_err(|error| ToolError::new(format!("workflow definition: {error}")))?;
                definition
                    .validate()
                    .map_err(|error| ToolError::new(format!("workflow definition: {error}")))?;
                if action == "save" {
                    let name = definition.name().to_owned();
                    service
                        .save(definition)
                        .map_err(|error| ToolError::new(error.to_string()))?;
                    return Ok(serde_json::json!({"saved":name}));
                }
                let started = service
                    .start(definition)
                    .map_err(|error| ToolError::new(error.to_string()))?;
                Ok(serde_json::json!({
                    "run_id":started.run_id().as_str(),
                    "job_id":started.job_id().as_str()
                }))
            }
            "resume" | "pause" | "cancel" => {
                let id = args
                    .get("run_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| ToolError::new("`run_id` must be a string"))?;
                let id =
                    WorkflowRunId::new(id).map_err(|_| ToolError::new("`run_id` is invalid"))?;
                if action == "cancel" {
                    let cancelled = service
                        .cancel(&id)
                        .map_err(|error| ToolError::new(error.to_string()))?;
                    return Ok(
                        serde_json::json!({"run_id":id.as_str(),"cancel_requested":cancelled}),
                    );
                }
                if action == "pause" {
                    service
                        .pause(&id)
                        .map_err(|error| ToolError::new(error.to_string()))?;
                    return Ok(serde_json::json!({"run_id":id.as_str(),"pause_requested":true}));
                }
                let started = service
                    .resume(&id)
                    .map_err(|error| ToolError::new(error.to_string()))?;
                Ok(serde_json::json!({
                    "run_id":started.run_id().as_str(),
                    "job_id":started.job_id().as_str()
                }))
            }
            "saved" => service
                .projection()
                .map(|projection| serde_json::json!(projection.definitions()))
                .map_err(|error| ToolError::new(error.to_string())),
            "run_saved" => {
                let name = args
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| ToolError::new("name must identify a saved definition"))?;
                let started = service
                    .run_saved(name)
                    .map_err(|error| ToolError::new(error.to_string()))?;
                Ok(
                    serde_json::json!({"run_id":started.run_id().as_str(),"job_id":started.job_id().as_str()}),
                )
            }
            "list" => {
                let projection = service
                    .projection()
                    .map_err(|error| ToolError::new(error.to_string()))?;
                Ok(serde_json::Value::Array(
                    projection
                        .iter()
                        .map(|(id, run)| {
                            serde_json::json!({
                                "run_id":id.as_str(),
                                "name":run.definition().name(),
                                "state":workflow_state_name(run.state()),
                                "attempt":run.attempt(),
                                "completed_steps":run.completed_steps(),
                                "nodes":run.nodes()
                            })
                        })
                        .collect(),
                ))
            }
            _ => Err(ToolError::new("unknown workflow action")),
        }
    }
}

fn workflow_state_name(state: WorkflowState) -> &'static str {
    match state {
        WorkflowState::Paused => "paused",
        WorkflowState::Running => "running",
        WorkflowState::Completed => "completed",
        WorkflowState::Failed => "failed",
        WorkflowState::Cancelled => "cancelled",
    }
}

/// Publish a workflow service and model tool over one worker Provider.
#[must_use]
pub fn workflow_plugin(worker: Arc<dyn WorkflowWorker>) -> Box<dyn Plugin> {
    make_workflow_plugin(Some(worker))
}

/// Mount the provider-independent native agent/tool graph executor.
#[must_use]
pub fn native_workflow_plugin() -> Box<dyn Plugin> {
    make_workflow_plugin(None)
}

fn make_workflow_plugin(worker: Option<Arc<dyn WorkflowWorker>>) -> Box<dyn Plugin> {
    struct WorkflowPlugin(Option<Arc<dyn WorkflowWorker>>);

    impl Plugin for WorkflowPlugin {
        fn name(&self) -> &'static str {
            "workflows"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "workflows",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Tool,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::Tool,
                "workflow",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            if self.0.is_none() {
                &[
                    crate::SERVICE_AGENT,
                    crate::SERVICE_JOBS,
                    heycode_session::SERVICE_SESSION,
                    heycode_tools::SERVICE_TOOLS,
                    crate::SERVICE_SUBAGENTS,
                ]
            } else {
                &[
                    crate::SERVICE_AGENT,
                    crate::SERVICE_JOBS,
                    heycode_session::SERVICE_SESSION,
                    heycode_tools::SERVICE_TOOLS,
                ]
            }
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_WORKFLOWS]
        }

        fn apply(&self, ctx: &mut heycode_core::Context) -> CoreResult<()> {
            let agent = ctx
                .get::<Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| CoreError::other("agent service missing"))?;
            let jobs = ctx
                .get::<Arc<JobRegistry>>(crate::SERVICE_JOBS)
                .map(|jobs| (*jobs).clone())
                .ok_or_else(|| CoreError::other("job registry missing"))?;
            let session = ctx
                .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session service missing"))?;
            let tools = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tool registry missing"))?;
            let worker = if let Some(worker) = &self.0 {
                worker.clone()
            } else {
                let subagents = ctx
                    .get::<crate::SubagentRegistry>(crate::SERVICE_SUBAGENTS)
                    .ok_or_else(|| CoreError::other("subagent registry missing"))?;
                let owner = crate::SubagentId::new(
                    session
                        .lock()
                        .map_err(|_| CoreError::other("session unavailable"))?
                        .id()
                        .as_str(),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
                let authority = subagents.root_authority(owner);
                Arc::new(NativeWorkflowWorker::new(
                    agent.clone(),
                    subagents,
                    authority,
                ))
            };
            let service = WorkflowService::new(session, agent, jobs, worker)
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.provide(crate::SERVICE_WORKFLOWS, "workflows", service)?;
            let service = ctx
                .get::<WorkflowService>(crate::SERVICE_WORKFLOWS)
                .ok_or_else(|| CoreError::other("workflow service missing"))?;
            let disposable = service.clone();
            ctx.effect(move || disposable.close());
            let owners: WorkflowOwners = Arc::new(Mutex::new(BTreeMap::new()));
            let disposable_owners = owners.clone();
            ctx.effect(move || {
                if let Ok(mut owners) = disposable_owners.lock() {
                    for service in owners.values().filter_map(Weak::upgrade) {
                        service.close();
                    }
                    owners.clear();
                }
            });
            let registration = tools
                .register_owned(Arc::new(WorkflowTool {
                    service: service.clone(),
                    subagents: if self.0.is_none() {
                        ctx.get::<crate::SubagentRegistry>(crate::SERVICE_SUBAGENTS)
                    } else {
                        None
                    },
                    owners,
                }))
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.effect(move || drop(registration));
            Ok(())
        }
    }

    Box::new(WorkflowPlugin(worker))
}

#[cfg(test)]
mod ownership_tests;

#[cfg(test)]
mod progress_tests;

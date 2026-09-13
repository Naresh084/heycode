//! E09 — shell and terminal producers for the effect-owned job registry.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_core::{CoreError, CoreResult, Plugin};
use heycode_exec::{
    ProcessError, ProcessErrorCode, ProcessExit, ShellRequest, ShellService, TerminalOwner,
    TerminalService, TerminalSpec,
};
use heycode_session::InboxDelivery;
use heycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry};

use crate::{
    Agent, Command, CommandArgument, CommandDescriptor, CommandRegistry, CommandSource,
    CommandTiming, JobId, JobOutcome, JobRegistry, JobSettlement, JobState, UiEvent,
};

/// Stable failure from admitting a background execution job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionJobError {
    /// Shell request resolution or terminal construction failed.
    #[error("background execution request is invalid")]
    InvalidRequest,
    /// The effect-owned job registry could not admit the operation.
    #[error("background execution jobs are unavailable")]
    Unavailable,
}

/// Bounded output policy for the native execution runtime.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionJobConfig {
    /// Retained bytes per stdout/stderr/terminal stream, 1 KiB..=1 MiB.
    pub retained_bytes: usize,
    /// Maximum inline output body, 1 KiB..=32 KiB (not including JSON metadata).
    pub inline_bytes: usize,
    /// Number of retained output records, 1..=256.
    pub history_limit: usize,
    /// Foreground promotion delay in seconds; zero disables automatic promotion.
    pub foreground_timeout_secs: u64,
}
impl Default for ExecutionJobConfig {
    fn default() -> Self {
        Self {
            retained_bytes: 256 * 1024,
            inline_bytes: 6 * 1024,
            history_limit: 64,
            foreground_timeout_secs: 120,
        }
    }
}
impl ExecutionJobConfig {
    /// Validate resource ceilings before composition.
    ///
    /// # Errors
    /// Any cap outside the documented supported interval.
    pub fn validate(&self) -> Result<(), ExecutionJobError> {
        if !(1024..=1024 * 1024).contains(&self.retained_bytes)
            || !(1024..=32 * 1024).contains(&self.inline_bytes)
            || !(1..=256).contains(&self.history_limit)
            || self.foreground_timeout_secs > 86_400
        {
            return Err(ExecutionJobError::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct ForegroundControl {
    pub(crate) promoted: tokio_util::sync::CancellationToken,
    pub(crate) settling: Arc<Mutex<()>>,
}

/// Shell/terminal producer bound to one Agent and its terminal authority.
///
/// Every operation hands its future and cancellation token to [`JobRegistry`].
/// The service owns no detached task and publishes terminal state only through
/// [`Agent::settle_job`], after the settlement notice is durable in A03.
pub struct ExecutionJobService {
    pub(crate) agent: Arc<Agent>,
    pub(crate) principal: crate::code_mode::ToolExecutionContext,
    scopes: Mutex<BTreeMap<String, ExecutionScope>>,
    jobs: Arc<JobRegistry>,
    session: Arc<std::sync::Mutex<heycode_session::Session>>,
    shell: ShellService,
    terminal: TerminalService,
    terminal_owner: TerminalOwner,
    terminal_jobs: Arc<Mutex<BTreeMap<heycode_exec::TerminalId, JobId>>>,
    outputs: Arc<Mutex<BTreeMap<JobId, crate::ExecutionOutput>>>,
    foreground: Arc<Mutex<BTreeMap<JobId, ForegroundControl>>>,
    config: ExecutionJobConfig,
    output_root: std::path::PathBuf,
    output_slots: Arc<tokio::sync::Semaphore>,
    pub(crate) monitor_slots: Arc<tokio::sync::Semaphore>,
}

// Retained observations never keep a child Session writer/Agent alive after close.
struct ExecutionScope {
    owner: TerminalOwner,
    terminal_jobs: Arc<Mutex<BTreeMap<heycode_exec::TerminalId, JobId>>>,
    outputs: Arc<Mutex<BTreeMap<JobId, crate::ExecutionOutput>>>,
    foreground: Arc<Mutex<BTreeMap<JobId, ForegroundControl>>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for ExecutionJobService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionJobService")
            .field("terminal_owner", &self.terminal_owner)
            .finish_non_exhaustive()
    }
}

impl ExecutionJobService {
    /// Bind one live Agent, job owner, and the composed exec services.
    #[must_use]
    pub fn new(
        agent: Arc<Agent>,
        jobs: Arc<JobRegistry>,
        session: Arc<std::sync::Mutex<heycode_session::Session>>,
        shell: ShellService,
        terminal: TerminalService,
        terminal_owner: TerminalOwner,
    ) -> Self {
        let output_root = session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        Self {
            principal: agent.tool_execution_context(),
            scopes: Mutex::new(BTreeMap::new()),
            agent,
            jobs,
            session,
            shell,
            terminal,
            terminal_owner,
            terminal_jobs: Arc::new(Mutex::new(BTreeMap::new())),
            outputs: Arc::new(Mutex::new(BTreeMap::new())),
            foreground: Arc::new(Mutex::new(BTreeMap::new())),
            config: ExecutionJobConfig::default(),
            output_root,
            output_slots: Arc::new(tokio::sync::Semaphore::new(
                ExecutionJobConfig::default().history_limit,
            )),
            monitor_slots: Arc::new(tokio::sync::Semaphore::new(8)),
        }
    }

    pub(crate) fn caller_context(&self) -> crate::code_mode::ToolExecutionContext {
        crate::code_mode::EXECUTING_TOOLS
            .try_with(Clone::clone)
            .unwrap_or_else(|_| self.principal.clone())
    }

    /// Resolve the actual caller before any spawned task loses task-local authority.
    pub(crate) fn scoped(self: &Arc<Self>) -> Result<Arc<Self>, ExecutionJobError> {
        let Some(context) = crate::code_mode::EXECUTING_TOOLS
            .try_with(Clone::clone)
            .ok()
        else {
            return Ok(self.clone());
        };
        if Arc::ptr_eq(&context.session, &self.session) {
            return Ok(self.clone());
        }
        let key = context
            .session
            .lock()
            .map_err(|_| ExecutionJobError::Unavailable)?
            .id()
            .to_string();
        let mut scopes = self
            .scopes
            .lock()
            .map_err(|_| ExecutionJobError::Unavailable)?;
        let owner = context
            .terminal_owner()
            .map_err(|_| ExecutionJobError::Unavailable)?;
        let mut scope = Self::new(
            self.agent.clone(),
            self.jobs.clone(),
            context.session.clone(),
            self.shell.clone(),
            self.terminal.clone(),
            owner.clone(),
        );
        if let Some(record) = scopes.get(&key) {
            scope.config = self.config;
            scope.outputs = record.outputs.clone();
            scope.terminal_jobs = record.terminal_jobs.clone();
            scope.foreground = record.foreground.clone();
            scope.output_slots = record.slots.clone();
        } else {
            if scopes.len() >= 64 {
                return Err(ExecutionJobError::Unavailable);
            }
            scope = scope.configure(self.config)?;
            scopes.insert(
                key,
                ExecutionScope {
                    owner,
                    terminal_jobs: scope.terminal_jobs.clone(),
                    outputs: scope.outputs.clone(),
                    foreground: scope.foreground.clone(),
                    slots: scope.output_slots.clone(),
                },
            );
        }
        scope.principal = context;
        scope.monitor_slots = self.monitor_slots.clone();
        Ok(Arc::new(scope))
    }

    fn configure(mut self, config: ExecutionJobConfig) -> Result<Self, ExecutionJobError> {
        config.validate()?;
        self.config = config;
        self.output_slots = Arc::new(tokio::sync::Semaphore::new(config.history_limit));
        if let Ok(entries) = std::fs::read_dir(&self.output_root) {
            let mut records: Vec<_> = entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let raw = name.strip_prefix("execution-")?.strip_suffix(".json")?;
                    let id = JobId::parse(raw).ok()?;
                    Some((id, entry.path()))
                })
                .collect();
            records.sort_by_key(|(id, _)| {
                id.as_str()
                    .strip_prefix("job-")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
            });
            let excess = records.len().saturating_sub(config.history_limit);
            for (_, path) in records.drain(..excess) {
                let _removed = std::fs::remove_file(path);
            }
            for (id, path) in records {
                if let Some(output) = crate::ExecutionOutput::load(&path) {
                    let output = output.with_permit(
                        self.output_slots
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| ExecutionJobError::Unavailable)?,
                    );
                    if let Some(terminal) = output
                        .terminal_id()
                        .and_then(|value| heycode_exec::TerminalId::parse(&value).ok())
                    {
                        self.terminal_jobs
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(terminal, id.clone());
                    }
                    self.outputs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(id, output);
                }
            }
        }
        Ok(self)
    }

    /// Effective output caps shared by tools and UI previews.
    #[must_use]
    pub const fn config(&self) -> ExecutionJobConfig {
        self.config
    }

    /// Retained output identities, including interrupted/completed recovered jobs.
    #[must_use]
    pub fn output_jobs(&self) -> Vec<JobId> {
        self.outputs
            .lock()
            .map(|rows| rows.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Resolve and start one bounded-capture shell operation in the background.
    ///
    /// # Errors
    /// Invalid shell intent fails before a job exists. A stopped/poisoned job
    /// registry refuses admission without spawning work.
    pub fn start_shell(
        &self,
        label: impl Into<String>,
        request: ShellRequest,
        delivery: InboxDelivery,
    ) -> Result<JobId, ExecutionJobError> {
        self.start_shell_with_shell(&self.shell, label, request, delivery)
    }

    /// Start with an explicitly rebound shell authority, preserving job and output ownership.
    ///
    /// # Errors
    /// Invalid request, unsupported launch authority or bounded admission failure.
    pub fn start_shell_with_shell(
        &self,
        shell: &ShellService,
        label: impl Into<String>,
        request: ShellRequest,
        delivery: InboxDelivery,
    ) -> Result<JobId, ExecutionJobError> {
        self.flush_session()?;
        let command = request.command().to_owned();
        let request = request.without_timeout();
        let spec = shell
            .resolve(request)
            .map_err(|_| ExecutionJobError::InvalidRequest)?;
        let output = self.new_output()?;
        output.begin(
            command,
            spec.cwd().display().to_string(),
            spec.timeout().map(|t| t.as_millis() as u64),
        );
        let retained = output.clone();
        let completion = crate::execution_output::OutputCompletion(output.clone());
        let inline = self.config.inline_bytes.min(6000);
        let shell = shell.clone();
        let agent = self.caller_context();
        let jobs = self.jobs.clone();
        let id = self
            .jobs
            .spawn(label, delivery, move |id, cancellation| async move {
                let _completion = completion;
                output.mark_started();
                let result = agent
                    .own_job(
                        &cancellation,
                        shell.execute_streaming(
                            spec,
                            cancellation.clone(),
                            Arc::new(output.clone()),
                        ),
                    )
                    .await;
                let settlement = shell_settlement(result, cancellation.is_cancelled(), inline);
                output.finish_reason(settlement.notice().to_owned());
                output.end();
                settle(&agent, &jobs, &id, settlement);
            })
            .map_err(|_| ExecutionJobError::Unavailable)?;
        self.retain_output(id.clone(), retained);
        Ok(id)
    }

    /// Resolve a shell command once, attach its exact spec to a persistent PTY,
    /// and track the terminal's complete lifetime as one background job.
    ///
    /// # Errors
    /// Invalid shell/terminal intent fails before a job exists. A stopped or
    /// poisoned registry refuses admission without spawning work.
    pub fn start_terminal(
        &self,
        label: impl Into<String>,
        request: ShellRequest,
        delivery: InboxDelivery,
    ) -> Result<JobId, ExecutionJobError> {
        self.start_terminal_with_shell(&self.shell, label, request, delivery)
    }

    /// Start with an explicitly rebound shell authority, preserving job and output ownership.
    ///
    /// # Errors
    /// Invalid request, unsupported launch authority or bounded admission failure.
    pub fn start_terminal_with_shell(
        &self,
        shell: &ShellService,
        label: impl Into<String>,
        request: ShellRequest,
        delivery: InboxDelivery,
    ) -> Result<JobId, ExecutionJobError> {
        self.flush_session()?;
        let command = request.command().to_owned();
        let request = request.without_timeout();
        let subprocess = shell
            .subprocess()
            .ok_or(ExecutionJobError::InvalidRequest)?;
        let process = shell
            .resolve(request)
            .map_err(|_| ExecutionJobError::InvalidRequest)?
            .into_process()
            .with_interactive_stdio();
        let output = self.new_output()?;
        output.begin(
            command,
            process.cwd().display().to_string(),
            process.timeout().map(|t| t.as_millis() as u64),
        );
        let retained = output.clone();
        let completion = crate::execution_output::OutputCompletion(output.clone());
        let spec = TerminalSpec::new(process)
            .map_err(|_| ExecutionJobError::InvalidRequest)?
            .with_output_sink(Arc::new(output.clone()));
        let inline = self.config.inline_bytes.min(6000);
        let terminal = self.terminal.clone();
        let owner = self.terminal_owner.clone();
        let agent = self.caller_context();
        let jobs = self.jobs.clone();
        let terminal_jobs = self.terminal_jobs.clone();
        let id = self
            .jobs
            .spawn(label, delivery, move |id, cancellation| async move {
                let _completion = completion;
                output.mark_started();
                let result = if cancellation.is_cancelled() {
                    Err(ProcessError::new(ProcessErrorCode::Cancelled))
                } else {
                    match agent
                        .own_job(
                            &cancellation,
                            terminal.open_with_subprocess(
                                &owner,
                                spec,
                                cancellation.clone(),
                                &subprocess,
                            ),
                        )
                        .await
                    {
                        Ok(terminal_id) => {
                            output.set_terminal(terminal_id.to_string());
                            if let Ok(mut associations) = terminal_jobs.lock() {
                                associations.insert(terminal_id.clone(), id.clone());
                            }
                            agent
                                .own_job(
                                    &cancellation,
                                    terminal.wait(&owner, &terminal_id, cancellation.clone()),
                                )
                                .await
                        }
                        Err(error) => Err(error),
                    }
                };
                let base = terminal_settlement(result, cancellation.is_cancelled());
                output.finish_reason(base.notice().to_owned());
                output.end();
                let tail = output.tail(heycode_exec::OutputStream::Terminal, inline);
                let settlement = static_settlement(
                    base.outcome().clone(),
                    format!(
                        "{}\nterminal output (job_control action=output job_id={}):\n{}",
                        base.notice(),
                        id,
                        tail.text
                    ),
                );
                settle(&agent, &jobs, &id, settlement);
            })
            .map_err(|_| ExecutionJobError::Unavailable)?;
        self.retain_output(id.clone(), retained);
        Ok(id)
    }

    /// Start an explicitly cancellation-capable tool through the Agent's approval and guard pipeline.
    ///
    /// # Errors
    /// Unsupported tool, invalid request or unavailable job admission.
    pub fn start_tool(
        &self,
        registry: &ToolRegistry,
        input: heycode_tools::ToolCallInput,
        delivery: InboxDelivery,
    ) -> Result<JobId, ExecutionJobError> {
        self.start_tool_with_mode(registry, input, delivery, false)
    }

    pub(crate) fn start_tool_with_mode(
        &self,
        _registry: &ToolRegistry,
        input: heycode_tools::ToolCallInput,
        delivery: InboxDelivery,
        foreground: bool,
    ) -> Result<JobId, ExecutionJobError> {
        let principal = self.caller_context();
        let tool = principal
            .tools
            .get(&input.name)
            .ok_or(ExecutionJobError::InvalidRequest)?;
        if !tool.supports_background() {
            return Err(ExecutionJobError::InvalidRequest);
        }
        self.flush_session()?;
        let output = self.new_output()?;
        let retained = output.clone();
        let agent = self.caller_context();
        let jobs = self.jobs.clone();
        let label = format!("tool {}", input.name);
        let run = move |id: JobId, cancellation: tokio_util::sync::CancellationToken| async move {
            let _completion = crate::execution_output::OutputCompletion(output.clone());
            let mut timed_out = false;
            let future = async {
                let outcome = agent
                    .own_job(
                        &cancellation,
                        agent.execute_observed(
                            input,
                            cancellation.clone(),
                            Some(Arc::new(output.clone())),
                        ),
                    )
                    .await?;
                agent.finish_background_result(outcome, &cancellation)
            };
            tokio::pin!(future);
            let result = tokio::select! {
                result = &mut future => result,
                () = cancellation.cancelled() => match tokio::time::timeout(std::time::Duration::from_secs(5), &mut future).await {
                    Ok(result) => result,
                    Err(_) => { timed_out = true; Err(anyhow::anyhow!("tool cancellation timed out; remote settlement is unconfirmed")) },
                }
            };
            let (mut outcome, text) = match result {
                Ok(value) => (JobOutcome::Completed, value.to_string()),
                Err(error) => (JobOutcome::Failed, error.to_string()),
            };
            if cancellation.is_cancelled() && !timed_out {
                outcome = JobOutcome::Cancelled;
            }
            if output
                .read(heycode_exec::OutputStream::Stdout, 0, 0)
                .total_bytes
                == 0
                && output
                    .read(heycode_exec::OutputStream::Stderr, 0, 0)
                    .total_bytes
                    == 0
            {
                heycode_exec::ProcessOutputSink::append(
                    &output,
                    heycode_exec::OutputStream::Stdout,
                    text.as_bytes(),
                );
            }
            output.end();
            let preview = output.read(heycode_exec::OutputStream::Stdout, 0, 4096);
            let settlement = static_settlement(
                outcome,
                format!("tool job {id} ended; retained output\n{}", preview.text),
            );
            if foreground {
                if agent
                    .settle_foreground_job(&jobs, &id, &settlement)
                    .is_err()
                {
                    agent.bus.emit(crate::UiEvent::Error {
                        message: "foreground tool settlement could not be committed".into(),
                    });
                }
            } else {
                settle(&agent, &jobs, &id, settlement);
            }
        };
        let id = if tool.effect() == heycode_tools::ToolEffect::Orchestration {
            self.jobs.spawn_coordinator(label, delivery, run)
        } else {
            self.jobs.spawn(label, delivery, run)
        }
        .map_err(|_| ExecutionJobError::Unavailable)?;
        self.retain_output(id.clone(), retained);
        Ok(id)
    }

    /// Register a foreground wait. The process/tool has already received its job identity.
    pub(crate) fn foreground_wait(&self, id: &JobId) -> tokio_util::sync::CancellationToken {
        self.register_foreground_wait(id, ForegroundControl::default())
    }

    pub(crate) fn register_foreground_wait(
        &self,
        id: &JobId,
        control: ForegroundControl,
    ) -> tokio_util::sync::CancellationToken {
        let token = control.promoted.clone();
        self.foreground
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), control);
        token
    }

    pub(crate) fn end_foreground_wait(&self, id: &JobId) {
        self.foreground
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// Promote an active foreground `run_tool` wait, preserving job and execution identity.
    #[must_use]
    pub fn promote(&self, id: &JobId) -> bool {
        let token = self
            .foreground
            .lock()
            .ok()
            .and_then(|rows| rows.get(id).cloned());
        if let Some(control) = token {
            let _settling = control.settling.lock().unwrap_or_else(|e| e.into_inner());
            let token = &control.promoted;
            if token.is_cancelled() {
                return true;
            }
            if !self.jobs.set_delivery(id, InboxDelivery::FollowUp) {
                return false;
            }
            token.cancel();
            return true;
        }
        let token = self.scopes.lock().ok().and_then(|scopes| {
            scopes
                .values()
                .find_map(|scope| scope.foreground.lock().ok()?.get(id).cloned())
        });
        token.is_some_and(|control| {
            let _settling = control.settling.lock().unwrap_or_else(|e| e.into_inner());
            let token = &control.promoted;
            if token.is_cancelled() {
                return true;
            }
            if !self.jobs.set_delivery(id, InboxDelivery::FollowUp) {
                return false;
            }
            token.cancel();
            true
        })
    }

    /// Wait until this foreground execution is promoted by the configured timer.
    /// A settled job cannot be promoted; its normal result remains authoritative.
    pub(crate) async fn automatic_promotion(&self, id: &JobId) {
        let seconds = self.config.foreground_timeout_secs;
        if seconds == 0 {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
        if self.promote(id) {
            self.principal.bus.emit(crate::UiEvent::Info {
                text: format!(
                    "tool {id} moved to background after {seconds}s; execution continues"
                ),
            });
        } else {
            std::future::pending::<()>().await;
        }
    }

    /// Identities currently eligible for foreground promotion.
    #[must_use]
    pub fn foreground_jobs(&self) -> Vec<JobId> {
        let mut rows: Vec<_> = self
            .foreground
            .lock()
            .map(|rows| rows.keys().cloned().collect())
            .unwrap_or_default();
        if let Ok(scopes) = self.scopes.lock() {
            for scope in scopes.values() {
                if let Ok(foreground) = scope.foreground.lock() {
                    rows.extend(foreground.keys().cloned());
                }
            }
        }
        rows
    }

    pub(crate) fn new_output(&self) -> Result<crate::ExecutionOutput, ExecutionJobError> {
        let mut outputs = self
            .outputs
            .lock()
            .map_err(|_| ExecutionJobError::Unavailable)?;
        if outputs.len() >= self.config.history_limit {
            if let Some(id) = outputs
                .iter()
                .find(|(_, output)| output.ended())
                .map(|(id, _)| id.clone())
            {
                if let Some(output) = outputs.remove(&id) {
                    output.remove_file();
                }
                self.terminal_jobs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|_, job| job != &id);
            } else {
                return Err(ExecutionJobError::Unavailable);
            }
        }
        let permit = self
            .output_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExecutionJobError::Unavailable)?;
        Ok(crate::ExecutionOutput::new(self.config.retained_bytes).with_permit(permit))
    }

    pub(crate) fn retain_output(&self, id: JobId, output: crate::ExecutionOutput) {
        output.attach(self.output_root.join(format!("execution-{id}.json")));
        self.outputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, output);
    }

    /// Independent retained output reader, available during execution and after settlement.
    #[must_use]
    pub fn output(&self, id: &JobId) -> Option<crate::ExecutionOutput> {
        if let Some(output) = self.outputs.lock().ok()?.get(id).cloned() {
            return Some(output);
        }
        self.scopes
            .lock()
            .ok()?
            .values()
            .find_map(|scope| scope.outputs.lock().ok()?.get(id).cloned())
    }

    /// Correlated PTY identity, retained after the process exits.
    #[must_use]
    pub fn job_terminal(&self, id: &JobId) -> Option<heycode_exec::TerminalId> {
        let own = self
            .terminal_jobs
            .lock()
            .ok()?
            .iter()
            .find(|(_, job)| *job == id)
            .map(|(terminal, _)| terminal.clone());
        own.or_else(|| {
            self.scopes.lock().ok()?.values().find_map(|scope| {
                scope
                    .terminal_jobs
                    .lock()
                    .ok()?
                    .iter()
                    .find(|(_, job)| *job == id)
                    .map(|(terminal, _)| terminal.clone())
            })
        })
    }

    /// Host/UI terminal authority for a correlated job, including a child-owned job.
    #[must_use]
    pub fn terminal_owner_for_job(&self, id: &JobId) -> Option<TerminalOwner> {
        if self
            .terminal_jobs
            .lock()
            .ok()?
            .values()
            .any(|job| job == id)
        {
            return Some(self.terminal_owner.clone());
        }
        self.scopes.lock().ok()?.values().find_map(|scope| {
            scope
                .terminal_jobs
                .lock()
                .ok()?
                .values()
                .any(|job| job == id)
                .then(|| scope.owner.clone())
        })
    }

    /// Send input to the terminal actually owned by this job.
    ///
    /// # Errors
    /// No correlated terminal, exited process or terminal input failure.
    pub async fn write_terminal(&self, id: &JobId, bytes: &[u8]) -> Result<(), ProcessError> {
        let terminal = self
            .job_terminal(id)
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::UnknownTerminal))?;
        let owner = self
            .terminal_owner_for_job(id)
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::UnknownTerminal))?;
        self.terminal.write(&owner, &terminal, bytes).await
    }

    /// Terminal authority used by `/ps` and `/stop` Consumers.
    #[must_use]
    pub const fn terminal_owner(&self) -> &TerminalOwner {
        &self.terminal_owner
    }

    /// Composed terminal registry used by control-plane Consumers.
    #[must_use]
    pub const fn terminals(&self) -> &TerminalService {
        &self.terminal
    }

    /// Job registry shared by execution, workflow, and subagent producers.
    #[must_use]
    pub fn jobs(&self) -> &Arc<JobRegistry> {
        &self.jobs
    }

    /// Resolve a live terminal row to the job that owns its waiter.
    #[must_use]
    pub fn terminal_job(&self, id: &heycode_exec::TerminalId) -> Option<JobId> {
        let own = self.terminal_jobs.lock().ok()?.get(id).cloned();
        own.or_else(|| {
            self.scopes
                .lock()
                .ok()?
                .values()
                .find_map(|scope| scope.terminal_jobs.lock().ok()?.get(id).cloned())
        })
    }

    pub(crate) fn flush_session(&self) -> Result<(), ExecutionJobError> {
        self.session
            .lock()
            .map_err(|_| ExecutionJobError::Unavailable)?
            .flush()
            .map_err(|_| ExecutionJobError::Unavailable)
    }
}

struct BackgroundShellTool {
    execution: Arc<ExecutionJobService>,
    shell: Option<ShellService>,
}

#[async_trait::async_trait]
impl Tool for BackgroundShellTool {
    fn rebind_workspace(
        &self,
        _filesystem: &heycode_exec::FileSystemService,
        shell: &ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            execution: self.execution.clone(),
            shell: Some(shell.clone()),
        }))
    }

    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "background_shell".to_owned(),
            description: "Start one resolved shell command as an effect-owned background job. Use job_control(action=list) or /tasks to inspect it and job_control(action=cancel) or /stop to stop it.".to_owned(),
            parameters: serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "required":["command"],
                "properties":{
                    "command":{"type":"string"},
                    "label":{"type":"string"}
                }
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let execution = self
            .execution
            .scoped()
            .map_err(|e| ToolError::new(e.to_string()))?;
        let command = execution_string_arg(&args, "command")?;
        let label = args
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("background shell");
        let request = ShellRequest::new(command)
            .and_then(|request| request.with_cwd(cx.cwd.clone()))
            .map_err(|_| ToolError::new("background shell command is invalid"))?;
        let id = execution
            .start_shell_with_shell(
                self.shell.as_ref().unwrap_or(&execution.shell),
                label,
                request,
                InboxDelivery::FollowUp,
            )
            .map_err(|error| ToolError::new(error.to_string()))?;
        Ok(serde_json::json!({"job_id":id.as_str(),"kind":"shell"}))
    }
}

struct BackgroundTerminalTool {
    execution: Arc<ExecutionJobService>,
    shell: Option<ShellService>,
}

#[async_trait::async_trait]
impl Tool for BackgroundTerminalTool {
    fn rebind_workspace(
        &self,
        _filesystem: &heycode_exec::FileSystemService,
        shell: &ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            execution: self.execution.clone(),
            shell: Some(shell.clone()),
        }))
    }

    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "background_terminal".to_owned(),
            description: "Start one resolved shell command in a persistent PTY tracked as an effect-owned background job. Returns correlated job and terminal ids; job_control(action=output) tails its retained output, terminal_write sends input.".to_owned(),
            parameters: serde_json::json!({
                "type":"object",
                "additionalProperties":false,
                "required":["command"],
                "properties":{
                    "command":{"type":"string"},
                    "label":{"type":"string"}
                }
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let execution = self
            .execution
            .scoped()
            .map_err(|e| ToolError::new(e.to_string()))?;
        let command = execution_string_arg(&args, "command")?;
        let label = args
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("background terminal");
        let request = ShellRequest::new(command)
            .and_then(|request| request.with_cwd(cx.cwd.clone()))
            .map_err(|_| ToolError::new("background terminal command is invalid"))?;
        let id = execution
            .start_terminal_with_shell(
                self.shell.as_ref().unwrap_or(&execution.shell),
                label,
                request,
                InboxDelivery::FollowUp,
            )
            .map_err(|error| ToolError::new(error.to_string()))?;
        let terminal_id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(terminal) = execution.job_terminal(&id) {
                    break Some(terminal);
                }
                if execution
                    .jobs()
                    .list()
                    .iter()
                    .any(|row| row.id == id && matches!(row.state, JobState::Settled(_)))
                {
                    break None;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .ok()
        .flatten();
        let terminal_pending =
            terminal_id.is_none() && execution.output(&id).is_some_and(|output| !output.ended());
        Ok(
            serde_json::json!({"job_id":id.as_str(),"kind":"terminal","terminal_id":terminal_id.as_ref().map(heycode_exec::TerminalId::as_str),"terminal_pending":terminal_pending}),
        )
    }
}

fn execution_string_arg<'a>(args: &'a serde_json::Value, name: &str) -> Result<&'a str, ToolError> {
    args.get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ToolError::new(format!("`{name}` must be a string")))
}

struct TasksCommand {
    execution: Arc<ExecutionJobService>,
    descriptor: CommandDescriptor,
}

#[async_trait::async_trait]
impl Command for TasksCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &Agent, _args: &str) -> anyhow::Result<()> {
        let lines = self
            .execution
            .jobs()
            .list()
            .into_iter()
            .map(|job| {
                let state = match job.state {
                    JobState::Running => "running",
                    JobState::Queued => "queued",
                    JobState::Cancelling => "cancelling",
                    JobState::Settled(JobOutcome::Interrupted) => "interrupted",
                    JobState::Settled(JobOutcome::Completed) => "completed",
                    JobState::Settled(JobOutcome::Failed) => "failed",
                    JobState::Settled(JobOutcome::Cancelled) => "cancelled",
                };
                format!("- {}  {state}  {}", job.id, job.label)
            })
            .collect::<Vec<_>>();
        agent.ui().emit(UiEvent::Info {
            text: if lines.is_empty() {
                "no background tasks".to_owned()
            } else {
                lines.join("\n")
            },
        });
        Ok(())
    }
}

struct PsCommand {
    execution: Arc<ExecutionJobService>,
    descriptor: CommandDescriptor,
}

#[async_trait::async_trait]
impl Command for PsCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &Agent, _args: &str) -> anyhow::Result<()> {
        let lines = self
            .execution
            .terminals()
            .list(self.execution.terminal_owner())
            .await
            .into_iter()
            .map(|status| {
                let state = if status.output_ended() {
                    "output-ended"
                } else {
                    "running"
                };
                format!(
                    "- {}  {state}  {}x{}  pending={} dropped={}",
                    status.id(),
                    status.size().cols(),
                    status.size().rows(),
                    status.pending_bytes(),
                    status.dropped_bytes()
                )
            })
            .collect::<Vec<_>>();
        agent.ui().emit(UiEvent::Info {
            text: if lines.is_empty() {
                "no persistent terminal processes".to_owned()
            } else {
                lines.join("\n")
            },
        });
        Ok(())
    }
}

struct StopCommand {
    execution: Arc<ExecutionJobService>,
    descriptor: CommandDescriptor,
}

#[async_trait::async_trait]
impl Command for StopCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let target = args.trim();
        if target.is_empty() {
            anyhow::bail!("usage: /stop <job-id|terminal-id|all>");
        }
        agent.ui().emit(UiEvent::Info {
            text: format!("cancellation requested for {target}; waiting for actual settlement"),
        });
        let wait = async {
            if target == "all" {
                self.stop_all().await
            } else {
                self.stop_one(target).await
            }
        };
        let text = match tokio::time::timeout(std::time::Duration::from_secs(5), wait).await {
            Ok(result) => result?,
            Err(_) => format!(
                "stop timed out for {target}; cancellation remains requested, but settlement is unconfirmed. Inspect /tasks and /ps; the worker remains owned."
            ),
        };
        agent.ui().emit(UiEvent::Info { text });
        Ok(())
    }
}

impl StopCommand {
    async fn stop_one(&self, target: &str) -> anyhow::Result<String> {
        if let Ok(id) = JobId::parse(target) {
            if !self.execution.jobs().cancel(&id) {
                return Ok(format!("no running background task `{target}`"));
            }
            let outcome = self.execution.jobs().wait_for_settlement(&id).await?;
            return Ok(format!("stopped {id} ({})", outcome.name()));
        }
        let terminal = match heycode_exec::TerminalId::parse(target) {
            Ok(terminal) => terminal,
            Err(_) => return Ok(format!("no running background operation `{target}`")),
        };
        if let Some(job) = self.execution.terminal_job(&terminal) {
            if !self.execution.jobs().cancel(&job) {
                return Ok(format!("no running terminal `{target}`"));
            }
            let outcome = self.execution.jobs().wait_for_settlement(&job).await?;
            return Ok(format!("stopped {terminal} via {job} ({})", outcome.name()));
        }
        match self
            .execution
            .terminals()
            .kill(self.execution.terminal_owner(), &terminal)
            .await
        {
            Ok(_) => Ok(format!("stopped terminal {terminal}")),
            Err(error) if error.code() == ProcessErrorCode::UnknownTerminal => {
                Ok(format!("no running terminal `{target}`"))
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn stop_all(&self) -> anyhow::Result<String> {
        let jobs = self
            .execution
            .jobs()
            .list()
            .into_iter()
            .filter_map(|job| (!matches!(job.state, JobState::Settled(_))).then_some(job.id))
            .collect::<Vec<_>>();
        let _cancelled = self.execution.jobs().cancel_all();

        let terminals = self
            .execution
            .terminals()
            .list(self.execution.terminal_owner())
            .await;
        let mut direct_terminals = 0_usize;
        for status in terminals {
            if self.execution.terminal_job(status.id()).is_none() {
                self.execution
                    .terminals()
                    .kill(self.execution.terminal_owner(), status.id())
                    .await?;
                direct_terminals += 1;
            }
        }
        for id in &jobs {
            let _outcome = self.execution.jobs().wait_for_settlement(id).await?;
        }
        Ok(format!(
            "stopped {} background task(s) and {direct_terminals} standalone terminal(s)",
            jobs.len()
        ))
    }
}

pub(crate) fn settle(
    agent: &crate::code_mode::ToolExecutionContext,
    jobs: &JobRegistry,
    id: &JobId,
    settlement: JobSettlement,
) {
    if agent.settle_job(jobs, id, &settlement).is_err() {
        agent.bus.emit(crate::UiEvent::Error {
            message: "background execution settlement could not be committed".to_owned(),
        });
    }
}

fn shell_settlement(
    result: Result<heycode_exec::ProcessOutput, ProcessError>,
    cancelled: bool,
    inline: usize,
) -> JobSettlement {
    if cancelled || result.as_ref().is_err_and(is_cancelled) {
        return static_settlement(
            JobOutcome::Cancelled,
            "shell cancelled; retained output is available with job_control(action=output)",
        );
    }
    match result {
        Ok(output) if output.exit().is_success() => static_settlement(
            JobOutcome::Completed,
            shell_output_notice("shell exited successfully", &output, inline),
        ),
        Ok(output) => {
            let summary = shell_exit_notice(output.exit());
            static_settlement(
                JobOutcome::Failed,
                shell_output_notice(&summary, &output, inline),
            )
        }
        Err(_) => static_settlement(JobOutcome::Failed, "shell execution failed"),
    }
}

fn terminal_settlement(
    result: Result<ProcessExit, ProcessError>,
    cancelled: bool,
) -> JobSettlement {
    if cancelled || result.as_ref().is_err_and(is_cancelled) {
        return static_settlement(JobOutcome::Cancelled, "terminal cancelled");
    }
    match result {
        Ok(exit) if exit.is_success() => {
            static_settlement(JobOutcome::Completed, "terminal exited successfully")
        }
        Ok(exit) => static_settlement(JobOutcome::Failed, terminal_exit_notice(&exit)),
        Err(_) => static_settlement(JobOutcome::Failed, "terminal execution failed"),
    }
}

fn is_cancelled(error: &ProcessError) -> bool {
    error.code() == ProcessErrorCode::Cancelled
}

fn shell_exit_notice(exit: &ProcessExit) -> String {
    match exit {
        ProcessExit::Exited { code } => format!("shell exited with code {code}"),
        ProcessExit::Signalled { .. } => "shell was terminated by a signal".to_owned(),
        ProcessExit::TimedOut => "shell timed out".to_owned(),
        ProcessExit::InactivityTimedOut => "shell output became inactive".to_owned(),
        _ => "shell execution ended unsuccessfully".to_owned(),
    }
}

fn terminal_exit_notice(exit: &ProcessExit) -> String {
    match exit {
        ProcessExit::Exited { code } => format!("terminal exited with code {code}"),
        ProcessExit::Signalled { .. } => "terminal was terminated by a signal".to_owned(),
        ProcessExit::TimedOut => "terminal timed out".to_owned(),
        ProcessExit::InactivityTimedOut => "terminal output became inactive".to_owned(),
        _ => "terminal execution ended unsuccessfully".to_owned(),
    }
}

fn shell_output_notice(
    summary: &str,
    output: &heycode_exec::ProcessOutput,
    inline: usize,
) -> String {
    let stream_cap = inline.saturating_sub(256) / 2;
    let bytes = output.stdout();
    let stdout = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(stream_cap)..]);
    let mut text = summary.to_owned();
    if !stdout.is_empty() {
        text.push_str("\nstdout:\n");
        text.push_str(&sanitize_output(&stdout));
    }
    if !output.stderr().is_empty() {
        text.push_str("\nstderr:\n");
        let bytes = output.stderr().as_bytes();
        text.push_str(&sanitize_output(&String::from_utf8_lossy(
            &bytes[bytes.len().saturating_sub(stream_cap)..],
        )));
    }
    if output.truncated() {
        text.push_str("\n[provider output was tail-truncated]");
    }
    let mut rendered = String::new();
    for character in text.chars() {
        if rendered.len().saturating_add(character.len_utf8()) > inline {
            break;
        }
        rendered.push(character);
    }
    if rendered.len() < text.len() {
        rendered.push_str("\n[background notice truncated]");
    }
    rendered
}

pub(crate) fn sanitize_output(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn static_settlement(outcome: JobOutcome, notice: impl Into<String>) -> JobSettlement {
    JobSettlement::bounded_or_failed(outcome, notice)
}

/// Publish E09's shell/terminal job producer after Agent and terminal services.
#[must_use]
pub fn execution_jobs_plugin() -> Box<dyn Plugin> {
    execution_jobs_plugin_with_config(ExecutionJobConfig::default())
}

/// Compose execution with validated host configuration and durable retained tails.
#[must_use]
pub fn execution_jobs_plugin_with_config(config: ExecutionJobConfig) -> Box<dyn Plugin> {
    struct ExecutionJobsPlugin(ExecutionJobConfig);

    impl Plugin for ExecutionJobsPlugin {
        fn name(&self) -> &'static str {
            "execution-jobs"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "execution-jobs",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Command,
                    heycode_core::PluginContributionKind::Tool,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                (heycode_core::ContributionKind::Command, "tasks"),
                (heycode_core::ContributionKind::Command, "ps"),
                (heycode_core::ContributionKind::Command, "stop"),
            ]
            .into_iter()
            .map(|(kind, name)| heycode_core::PluginContributionSpec::new(kind, name))
            .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_AGENT,
                crate::SERVICE_JOBS,
                heycode_session::SERVICE_SESSION,
                heycode_exec::SERVICE_SHELL,
                heycode_exec::SERVICE_SUBPROCESS,
                heycode_exec::SERVICE_TERMINAL,
                crate::SERVICE_COMMANDS,
                heycode_tools::SERVICE_TOOLS,
            ]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_EXECUTION_JOBS]
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
                .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session service missing"))?;
            let shell = ctx
                .get::<ShellService>(heycode_exec::SERVICE_SHELL)
                .map(|service| (*service).clone())
                .ok_or_else(|| CoreError::other("shell service missing"))?;
            let terminal = ctx
                .get::<TerminalService>(heycode_exec::SERVICE_TERMINAL)
                .map(|service| (*service).clone())
                .ok_or_else(|| CoreError::other("terminal service missing"))?;
            let commands = ctx
                .get::<CommandRegistry>(crate::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("command registry missing"))?;
            let tools = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tool registry missing"))?;
            let owner = {
                let session = session.lock().unwrap_or_else(|error| error.into_inner());
                TerminalOwner::new(format!("session:{}", session.id()))
                    .map_err(|_| CoreError::other("session id cannot scope terminals"))?
            };
            ctx.provide(
                crate::SERVICE_EXECUTION_JOBS,
                "execution-jobs",
                ExecutionJobService::new(agent, jobs, session, shell, terminal, owner)
                    .configure(self.0)
                    .map_err(|error| CoreError::other(error.to_string()))?,
            )?;
            let execution = ctx
                .get::<ExecutionJobService>(crate::SERVICE_EXECUTION_JOBS)
                .ok_or_else(|| CoreError::other("execution-job service missing"))?;
            execution.agent.install_execution(&execution);
            if tools.get("terminal_open").is_none() {
                let subprocess = ctx
                    .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                    .ok_or_else(|| CoreError::other("subprocess service missing"))?;
                for tool in heycode_tools::builtins::terminal::terminal_tools(
                    execution.terminals().clone(),
                    (*subprocess).clone(),
                    execution.terminal_owner().clone(),
                    execution.agent.cwd().to_path_buf(),
                ) {
                    ctx.contribute(heycode_core::ContributionKind::Tool, tool.spec().name)?;
                    let registration = tools
                        .register_owned(tool)
                        .map_err(|error| CoreError::other(error.to_string()))?;
                    ctx.effect(move || drop(registration));
                }
            }
            for tool in [
                Arc::new(BackgroundShellTool {
                    execution: execution.clone(),
                    shell: None,
                }) as Arc<dyn Tool>,
                Arc::new(BackgroundTerminalTool {
                    execution: execution.clone(),
                    shell: None,
                }),
            ] {
                ctx.contribute(heycode_core::ContributionKind::Tool, tool.spec().name)?;
                let registration = tools
                    .register_owned(tool)
                    .map_err(|error| CoreError::other(error.to_string()))?;
                ctx.effect(move || drop(registration));
            }
            for tool in crate::execution_tools::tools(execution.clone(), tools.clone()) {
                ctx.contribute(heycode_core::ContributionKind::Tool, tool.spec().name)?;
                let registration = tools
                    .register_owned(tool)
                    .map_err(|error| CoreError::other(error.to_string()))?;
                ctx.effect(move || drop(registration));
            }
            let source = CommandSource::from_plugin("execution-jobs")
                .map_err(|error| CoreError::other(error.to_string()))?;
            let tasks = CommandDescriptor::new(
                "tasks",
                "List background jobs and their state",
                Vec::new(),
                CommandTiming::Immediate,
                source.clone(),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let ps = CommandDescriptor::new(
                "ps",
                "List persistent terminal processes",
                Vec::new(),
                CommandTiming::Immediate,
                source.clone(),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let target = CommandArgument::required("target", "A job id, terminal id, or `all`")
                .map_err(|error| CoreError::other(error.to_string()))?;
            let stop = CommandDescriptor::new(
                "stop",
                "Stop one or all background operations",
                vec![target],
                CommandTiming::Immediate,
                source,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(TasksCommand {
                        execution: execution.clone(),
                        descriptor: tasks,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(PsCommand {
                        execution: execution.clone(),
                        descriptor: ps,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(StopCommand {
                        execution,
                        descriptor: stop,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(ExecutionJobsPlugin(config))
}

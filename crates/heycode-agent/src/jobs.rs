//! O04 — background jobs: settlement, notices and the wake budget.
//!
//! A job is work that outlives the turn that started it. Three rules make that
//! safe:
//!
//! * **Settlement is exactly once.** A job reaches exactly one terminal
//!   outcome, and its owned task is always joined — never detached.
//! * **A settled job delivers a notice, not a turn.** The notice goes through
//!   the A03 durable inbox, so it is replayable and is not model-visible until
//!   a claim admits it.
//! * **Waking is budgeted.** Background work must not be able to spin the agent
//!   indefinitely. A settlement that would wake an idle agent spends one wake
//!   token; with none left the notice is still delivered durably, but demoted
//!   to a non-waking delivery. Tokens replenish when a turn settles, so a job
//!   storm costs at most one wake per turn instead of one per job.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_session::InboxDelivery;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;

/// Wake tokens available to background settlements between turns.
const DEFAULT_WAKE_CAPACITY: u32 = 1;
/// Longest accepted job label.
const MAX_LABEL_BYTES: usize = 256;
/// Longest accepted settlement notice.
const MAX_NOTICE_BYTES: usize = 8 * 1024;

/// Why a job identifier or request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    /// Empty, oversized, or control-bearing label.
    InvalidLabel,
    /// Blank or oversized settlement notice.
    InvalidNotice,
    /// The named job is unknown or already settled.
    Unknown,
    /// Registry state was poisoned or the owner shut down.
    Unavailable,
    /// Global queue or lifetime admission budget exhausted.
    Capacity,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLabel => "job label is invalid",
            Self::InvalidNotice => "job notice is invalid",
            Self::Unknown => "job is unknown or already settled",
            Self::Unavailable => "job registry is unavailable",
            Self::Capacity => "job admission budget or queue capacity exhausted",
        })
    }
}

impl std::error::Error for JobError {}

/// Validated background-job identity.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct JobId(String);

impl JobId {
    /// Parse an id previously returned by this registry.
    ///
    /// # Errors
    /// Values outside the canonical `job-<u64>` form are refused.
    pub fn parse(value: &str) -> Result<Self, JobError> {
        let Some(number) = value.strip_prefix("job-") else {
            return Err(JobError::Unknown);
        };
        let parsed = number.parse::<u64>().map_err(|_| JobError::Unknown)?;
        if format!("job-{parsed}") != value {
            return Err(JobError::Unknown);
        }
        Ok(Self(value.to_owned()))
    }

    /// Exact id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How one job finished. A job reaches exactly one of these.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum JobOutcome {
    /// The job produced a result.
    Completed,
    /// The job failed with a bounded safe reason.
    Failed,
    /// The job was cancelled before producing a result.
    Cancelled,
    /// Process ended before the operation settled; never automatically replayed.
    Interrupted,
}

impl JobOutcome {
    /// Stable wire name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Live state of one job.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum JobState {
    /// Waiting for global execution capacity.
    Queued,
    /// Cancellation requested, awaiting real settlement.
    Cancelling,
    /// Still running.
    Running,
    /// Settled exactly once with this outcome.
    Settled(JobOutcome),
}

/// Safe snapshot of one job.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobSnapshot {
    /// Stable identity.
    pub id: JobId,
    /// Human label from spawn time.
    pub label: String,
    /// Current state.
    pub state: JobState,
    /// Delivery the settlement notice requested.
    pub delivery: InboxDelivery,
    /// Internal coordination; its child owns the visible execution.
    #[serde(default)]
    pub coordinator: bool,
}

/// What one settled job reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSettlement {
    outcome: JobOutcome,
    notice: String,
}

impl JobSettlement {
    /// Build a settlement with a bounded safe notice.
    ///
    /// # Errors
    /// Blank or oversized notice text.
    pub fn new(outcome: JobOutcome, notice: impl Into<String>) -> Result<Self, JobError> {
        let notice = notice.into();
        if notice.trim().is_empty() || notice.len() > MAX_NOTICE_BYTES {
            return Err(JobError::InvalidNotice);
        }
        Ok(Self { outcome, notice })
    }

    /// Terminal outcome.
    #[must_use]
    pub const fn outcome(&self) -> &JobOutcome {
        &self.outcome
    }

    /// Bounded notice text delivered to the inbox.
    #[must_use]
    pub fn notice(&self) -> &str {
        &self.notice
    }

    pub(crate) fn bounded_or_failed(outcome: JobOutcome, notice: impl Into<String>) -> Self {
        match Self::new(outcome, notice) {
            Ok(settlement) => settlement,
            Err(_) => Self {
                outcome: JobOutcome::Failed,
                notice: "background job failed".to_owned(),
            },
        }
    }
}

/// Whether a settlement was allowed to wake an idle agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WakeDecision {
    /// A wake token was spent; the requested delivery was honored.
    Woke,
    /// The requested delivery was non-waking to begin with.
    NotRequested,
    /// A wake was requested but the budget was exhausted, so the notice was
    /// delivered with a demoted non-waking delivery instead.
    Demoted,
}

struct JobEntry {
    coordinator: bool,
    label: String,
    delivery: InboxDelivery,
    state: JobState,
    settling: bool,
    cancellation: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
    admission: Option<tokio::sync::OwnedSemaphorePermit>,
}

struct RegistryState {
    active_workers: usize,
    completions: BTreeMap<JobId, AgentCompletionDelivery>,
    workspace_paused: bool,
    jobs: BTreeMap<JobId, JobEntry>,
    next_id: u64,
    wake_capacity: u32,
    wake_remaining: u32,
    demoted: u64,
}

/// Effect-owned registry of background jobs.
///
/// The registry owns every spawned task and every job's cancellation token, so
/// disposal cancels and releases all of them rather than leaving detached work.
pub struct JobRegistry {
    state: Mutex<RegistryState>,
    shutdown: CancellationToken,
    changed: tokio::sync::Notify,
    admission: Arc<tokio::sync::Semaphore>,
    limits: JobLimits,
    history: Mutex<Option<std::path::PathBuf>>,
}

impl std::fmt::Debug for JobRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (running, settled) = self.state.lock().map_or((0, 0), |state| {
            let running = state
                .jobs
                .values()
                .filter(|job| !matches!(job.state, JobState::Settled(_)))
                .count();
            (running, state.jobs.len() - running)
        });
        formatter
            .debug_struct("JobRegistry")
            .field("running", &running)
            .field("settled", &settled)
            .field("shutdown", &self.shutdown.is_cancelled())
            .finish()
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new(DEFAULT_WAKE_CAPACITY)
    }
}

/// A host workspace transition's admission fence; existing work is never cancelled.
pub(crate) struct JobWorkspacePause(Arc<JobRegistry>);
impl Drop for JobWorkspacePause {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.workspace_paused = false;
        }
    }
}

impl JobRegistry {
    pub(crate) fn pause_workspace(self: &Arc<Self>) -> Result<JobWorkspacePause, JobError> {
        let mut state = self.state.lock().map_err(|_| JobError::Unavailable)?;
        if state.workspace_paused
            || state.jobs.values().any(|job| {
                !matches!(job.state, JobState::Settled(_))
                    || job.settling
                    || job
                        .handle
                        .as_ref()
                        .is_some_and(|handle| !handle.is_finished())
            })
        {
            return Err(JobError::Unavailable);
        }
        state.workspace_paused = true;
        Ok(JobWorkspacePause(self.clone()))
    }
}

impl JobRegistry {
    /// Build a registry whose settlements may wake an idle agent at most
    /// `wake_capacity` times before a turn replenishes the budget.
    #[must_use]
    pub fn new(wake_capacity: u32) -> Self {
        Self::with_limits(wake_capacity, JobLimits::default())
    }

    /// Configure global execution, queue, history and admission-spend bounds.
    #[must_use]
    pub fn with_limits(wake_capacity: u32, limits: JobLimits) -> Self {
        let limits = limits.normalized();
        Self {
            state: Mutex::new(RegistryState {
                active_workers: 0,
                completions: BTreeMap::new(),
                workspace_paused: false,
                jobs: BTreeMap::new(),
                next_id: 0,
                wake_capacity,
                wake_remaining: wake_capacity,
                demoted: 0,
            }),
            shutdown: CancellationToken::new(),
            changed: tokio::sync::Notify::new(),
            admission: Arc::new(tokio::sync::Semaphore::new(limits.max_running)),
            limits,
            history: Mutex::new(None),
        }
    }

    /// Register one already-running job and take ownership of its task.
    ///
    /// The caller receives the job's cancellation token before the task is
    /// registered, so a cancel that races admission cannot be lost.
    ///
    /// # Errors
    /// Invalid label, or the registry was disposed.
    pub fn admit(
        &self,
        label: impl Into<String>,
        delivery: InboxDelivery,
        cancellation: CancellationToken,
        handle: tokio::task::JoinHandle<()>,
    ) -> Result<JobId, JobError> {
        let label = label.into();
        let permit = match self.admission.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                cancellation.cancel();
                handle.abort();
                return Err(JobError::Capacity);
            }
        };
        if let Err(error) = validate_label(&label) {
            cancellation.cancel();
            handle.abort();
            return Err(error);
        }
        if self.shutdown.is_cancelled() {
            cancellation.cancel();
            handle.abort();
            return Err(JobError::Unavailable);
        }
        let mut state = self.state.lock().map_err(|_| JobError::Unavailable)?;
        if state.workspace_paused {
            cancellation.cancel();
            handle.abort();
            return Err(JobError::Unavailable);
        }
        if self.limits.max_admissions != 0 && state.next_id >= self.limits.max_admissions {
            cancellation.cancel();
            handle.abort();
            return Err(JobError::Capacity);
        }
        self.prune_locked(&mut state);
        let value = state.next_id;
        state.next_id = state.next_id.saturating_add(1);
        let id = JobId(format!("job-{value}"));
        state.jobs.insert(
            id.clone(),
            JobEntry {
                coordinator: false,
                label,
                delivery,
                state: JobState::Running,
                settling: false,
                cancellation,
                handle: Some(handle),
                admission: Some(permit),
            },
        );
        if let Err(error) = self.persist_locked(&state) {
            if let Some(job) = state.jobs.remove(&id) {
                job.cancellation.cancel();
                if let Some(handle) = job.handle {
                    handle.abort();
                }
            }
            return Err(error);
        }
        drop(state);
        self.changed.notify_waiters();
        Ok(id)
    }

    /// Spawn and own one background operation after reserving its stable id.
    ///
    /// The row and cancellation token exist before the task can run, so a
    /// cancel racing admission is observed by the operation. The JoinHandle is
    /// retained by this registry through settlement and terminal disposal; it
    /// is never detached by the caller.
    ///
    /// # Errors
    /// Invalid label, disposed/poisoned registry, or absence of a Tokio runtime.
    pub fn spawn<F, Fut>(
        self: &Arc<Self>,
        label: impl Into<String>,
        delivery: InboxDelivery,
        run: F,
    ) -> Result<JobId, JobError>
    where
        F: FnOnce(JobId, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.spawn_class(label, delivery, false, run)
    }

    /// Own a coordinator whose provider/process work is admitted separately.
    /// It must not retain a process slot while waiting for descendant jobs.
    pub fn spawn_coordinator<F, Fut>(
        self: &Arc<Self>,
        label: impl Into<String>,
        delivery: InboxDelivery,
        run: F,
    ) -> Result<JobId, JobError>
    where
        F: FnOnce(JobId, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.spawn_class(label, delivery, true, run)
    }

    fn spawn_class<F, Fut>(
        self: &Arc<Self>,
        label: impl Into<String>,
        delivery: InboxDelivery,
        coordinator: bool,
        run: F,
    ) -> Result<JobId, JobError>
    where
        F: FnOnce(JobId, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| JobError::Unavailable)?;
        let label = label.into();
        validate_label(&label)?;
        if self.shutdown.is_cancelled() {
            return Err(JobError::Unavailable);
        }
        let cancellation = self.shutdown.child_token();
        let id = {
            let mut state = self.state.lock().map_err(|_| JobError::Unavailable)?;
            if state.workspace_paused {
                return Err(JobError::Unavailable);
            }
            if self.limits.max_admissions != 0 && state.next_id >= self.limits.max_admissions
                || (!coordinator
                    && state
                        .jobs
                        .values()
                        .filter(|job| {
                            !job.coordinator && !matches!(job.state, JobState::Settled(_))
                        })
                        .count()
                        >= self
                            .limits
                            .max_running
                            .saturating_add(self.limits.max_queued))
            {
                return Err(JobError::Capacity);
            }
            self.prune_locked(&mut state);
            let value = state.next_id;
            state.next_id = state.next_id.saturating_add(1);
            let id = JobId(format!("job-{value}"));
            state.jobs.insert(
                id.clone(),
                JobEntry {
                    coordinator,
                    label,
                    delivery,
                    state: JobState::Queued,
                    settling: false,
                    cancellation: cancellation.clone(),
                    handle: None,
                    admission: None,
                },
            );
            id
        };
        let task_id = id.clone();
        let task_cancellation = cancellation.clone();
        if let Err(error) = self.persist() {
            cancellation.cancel();
            if let Ok(mut state) = self.state.lock() {
                state.jobs.remove(&id);
            }
            return Err(error);
        }
        let owner = self.clone();
        struct WorkerLifetime(Arc<JobRegistry>);
        impl Drop for WorkerLifetime {
            fn drop(&mut self) {
                if let Ok(mut state) = self.0.state.lock() {
                    state.active_workers = state.active_workers.saturating_sub(1);
                }
                self.0.changed.notify_waiters();
            }
        }
        self.state
            .lock()
            .map_err(|_| JobError::Unavailable)?
            .active_workers += 1;
        let worker = WorkerLifetime(self.clone());
        let permits = self.admission.clone();
        let mut handle = Some(runtime.spawn(async move {
            let _worker = worker;
            let permit = if coordinator {
                None
            } else {
                tokio::select! {
                    biased;
                    () = task_cancellation.cancelled() => None,
                    permit = permits.acquire_owned() => permit.ok(),
                }
            };
            if let Ok(mut state) = owner.state.lock()
                && let Some(job) = state.jobs.get_mut(&task_id)
                && job.state == JobState::Queued
            {
                job.state = JobState::Running;
            }
            let _ = owner.persist();
            owner.changed.notify_waiters();
            use futures::FutureExt;
            let _ = std::panic::AssertUnwindSafe(async {
                run(task_id.clone(), task_cancellation).await
            })
            .catch_unwind()
            .await;
            drop(permit);
            // A buggy/panicking consumer must never remain falsely Running forever.
            if let Ok(mut state) = owner.state.lock()
                && let Some(job) = state.jobs.get_mut(&task_id)
                && !matches!(job.state, JobState::Settled(_))
            {
                job.state = JobState::Settled(JobOutcome::Failed);
            }
            let _ = owner.persist();
            owner.changed.notify_waiters();
        }));
        let attached = if let Ok(mut state) = self.state.lock() {
            if let Some(job) = state.jobs.get_mut(&id) {
                job.handle = handle.take();
                true
            } else {
                false
            }
        } else {
            false
        };
        if !attached {
            cancellation.cancel();
            if let Some(handle) = handle {
                handle.abort();
            }
            if let Ok(mut state) = self.state.lock() {
                state.jobs.remove(&id);
            }
            return Err(JobError::Unavailable);
        }
        self.changed.notify_waiters();
        Ok(id)
    }

    /// Ordered snapshot of every known job.
    #[must_use]
    pub fn list(&self) -> Vec<JobSnapshot> {
        self.state
            .lock()
            .map(|state| {
                state
                    .jobs
                    .iter()
                    .map(|(id, job)| JobSnapshot {
                        id: id.clone(),
                        label: job.label.clone(),
                        state: job.state.clone(),
                        delivery: job.delivery,
                        coordinator: job.coordinator,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Cancel one running job. Returns whether a running job was cancelled.
    #[must_use]
    pub fn cancel(&self, id: &JobId) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(job) = state
            .jobs
            .get_mut(id)
            .filter(|job| !matches!(job.state, JobState::Settled(_)))
        else {
            return false;
        };
        job.cancellation.cancel();
        job.state = JobState::Cancelling;
        drop(state);
        let _ = self.persist();
        self.changed.notify_waiters();
        true
    }

    /// Request cancellation of every queued/running operation.
    #[must_use]
    pub fn cancel_all(&self) -> usize {
        let ids = self
            .list()
            .into_iter()
            .filter(|job| !matches!(job.state, JobState::Settled(_)))
            .map(|job| job.id)
            .collect::<Vec<_>>();
        ids.iter().filter(|id| self.cancel(id)).count()
    }

    /// Wait until one known job publishes its terminal state.
    ///
    /// # Errors
    /// A poisoned/disposed registry cannot provide a trustworthy state.
    pub async fn wait_for_settlement(&self, id: &JobId) -> Result<JobOutcome, JobError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let outcome = {
                let state = self.state.lock().map_err(|_| JobError::Unavailable)?;
                match state.jobs.get(id).map(|job| &job.state) {
                    Some(JobState::Settled(outcome)) => Some(Ok(outcome.clone())),
                    Some(JobState::Running | JobState::Queued | JobState::Cancelling) => None,
                    None => Some(Err(JobError::Unknown)),
                }
            };
            if let Some(outcome) = outcome {
                return outcome;
            }
            changed.await;
        }
    }

    /// Wait for admitted jobs and the tails of their owned workers, including
    /// new jobs those workers admit before returning. No polling or cancellation
    /// is performed by this observer.
    pub async fn wait_until_idle(&self, cancellation: CancellationToken) -> Result<(), JobError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let idle = {
                let state = self.state.lock().map_err(|_| JobError::Unavailable)?;
                state.active_workers == 0
                    && state
                        .jobs
                        .values()
                        .all(|job| matches!(job.state, JobState::Settled(_)))
            };
            if idle {
                return Ok(());
            }
            tokio::select! {
                () = &mut changed => {},
                () = cancellation.cancelled() => return Err(JobError::Unavailable),
                () = self.shutdown.cancelled() => return Err(JobError::Unavailable),
            }
        }
    }

    /// Observe both durable settlement and completion of the owned worker future.
    ///
    /// The handle stays in the registry while waiting: cancellation or a timeout
    /// of this waiter cannot detach the worker. A terminal row alone is not proof
    /// that code following its settlement callback has stopped executing.
    pub async fn wait_for_task_exit(&self, id: &JobId) -> Result<JobOutcome, JobError> {
        let outcome = self.wait_for_settlement(id).await?;
        loop {
            let finished = {
                let state = self.state.lock().map_err(|_| JobError::Unavailable)?;
                let entry = state.jobs.get(id).ok_or(JobError::Unknown)?;
                entry
                    .handle
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
            };
            if finished {
                return Ok(outcome);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Replenish the wake budget. Called when a turn settles, so background
    /// work costs at most `wake_capacity` wakes per turn.
    pub fn replenish_wakes(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.wake_remaining = state.wake_capacity;
        }
    }

    /// How many settlements were demoted because the budget was exhausted.
    #[must_use]
    pub fn demoted_wakes(&self) -> u64 {
        self.state.lock().map(|state| state.demoted).unwrap_or(0)
    }

    /// Cancel every running job and release the registry.
    ///
    /// Disposal is synchronous: it cancels and drops owned handles rather than
    /// awaiting them, because a context disposer cannot block.
    pub fn dispose(&self) {
        self.shutdown.cancel();
        if let Ok(mut state) = self.state.lock() {
            for job in state.jobs.values_mut() {
                job.cancellation.cancel();
                if let Some(handle) = job.handle.take() {
                    handle.abort();
                }
            }
            state.jobs.clear();
        }
        self.changed.notify_waiters();
    }

    /// Settle one job exactly once, choosing its effective delivery.
    ///
    /// Returns the delivery to use and whether a wake token was spent. A second
    /// settlement for the same id is refused, which is what makes settlement
    /// exactly-once rather than best-effort.
    #[cfg(test)]
    fn settle(
        &self,
        id: &JobId,
        outcome: JobOutcome,
    ) -> Result<(InboxDelivery, WakeDecision), JobError> {
        let reservation = self.reserve_settlement(id)?;
        self.commit_settlement(id, outcome, reservation)
    }

    fn reserve_settlement(&self, id: &JobId) -> Result<SettlementReservation, JobError> {
        let mut state = self.state.lock().map_err(|_| JobError::Unavailable)?;
        let requested = {
            let Some(job) = state.jobs.get_mut(id) else {
                return Err(JobError::Unknown);
            };
            if matches!(job.state, JobState::Settled(_)) || job.settling {
                return Err(JobError::Unknown);
            }
            job.settling = true;
            job.delivery
        };
        let wakes = matches!(requested, InboxDelivery::FollowUp | InboxDelivery::Steer);
        let (delivery, decision, wake_reserved) = if !wakes {
            (requested, WakeDecision::NotRequested, false)
        } else if state.wake_capacity == 0 {
            (requested, WakeDecision::Woke, false)
        } else if state.wake_remaining > 0 {
            state.wake_remaining -= 1;
            (requested, WakeDecision::Woke, true)
        } else {
            (InboxDelivery::Inject, WakeDecision::Demoted, false)
        };
        Ok(SettlementReservation {
            delivery,
            decision,
            wake_reserved,
        })
    }

    fn rollback_settlement(&self, id: &JobId, reservation: SettlementReservation) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(job) = state.jobs.get_mut(id)
            && !matches!(job.state, JobState::Settled(_))
            && job.settling
        {
            job.settling = false;
            if reservation.wake_reserved {
                state.wake_remaining = state
                    .wake_remaining
                    .saturating_add(1)
                    .min(state.wake_capacity);
            }
        }
    }

    /// Settle an internal coordinator without recursively delivering a notice to its owner.
    pub(crate) fn settle_silent(&self, id: &JobId, outcome: JobOutcome) -> Result<(), JobError> {
        if !self.set_delivery(id, InboxDelivery::Inject) {
            return Err(JobError::Unknown);
        }
        let reservation = self.reserve_settlement(id)?;
        self.commit_settlement(id, outcome, reservation).map(|_| ())
    }

    fn commit_settlement(
        &self,
        id: &JobId,
        outcome: JobOutcome,
        reservation: SettlementReservation,
    ) -> Result<(InboxDelivery, WakeDecision), JobError> {
        let mut state = self.state.lock().map_err(|_| JobError::Unavailable)?;
        let Some(job) = state.jobs.get_mut(id) else {
            return Err(JobError::Unknown);
        };
        if matches!(job.state, JobState::Settled(_)) || !job.settling {
            return Err(JobError::Unknown);
        }
        job.state = JobState::Settled(outcome);
        job.settling = false;
        job.admission = None;
        if reservation.decision == WakeDecision::Demoted {
            state.demoted = state.demoted.saturating_add(1);
        }
        let result = (reservation.delivery, reservation.decision);
        drop(state);
        self.persist()?;
        self.changed.notify_waiters();
        Ok(result)
    }
}

#[derive(Clone, Copy)]
struct SettlementReservation {
    delivery: InboxDelivery,
    decision: WakeDecision,
    wake_reserved: bool,
}

impl Agent {
    /// Attribute one terminal run using registry-owned identity and event-time name.
    pub(crate) fn agent_completion_source(
        &self,
        id: &JobId,
        agent_id: &str,
        agent_name: &str,
        recipient_id: &str,
        outcome: &JobOutcome,
    ) -> heycode_session::InboxSource {
        let session = self.session().lock().unwrap_or_else(|e| e.into_inner());
        heycode_session::InboxSource::Agent {
            agent_id: agent_id.to_owned(),
            agent_name: agent_name.to_owned(),
            recipient_id: recipient_id.to_owned(),
            run_id: format!("agent-run:{}:{id}", session.id()),
            completion_id: Some(format!("agent-completion:{}:{id}", session.id())),
            outcome: Some(match outcome {
                JobOutcome::Completed => heycode_session::AgentCompletionOutcome::Completed,
                JobOutcome::Failed => heycode_session::AgentCompletionOutcome::Failed,
                JobOutcome::Cancelled => heycode_session::AgentCompletionOutcome::Cancelled,
                JobOutcome::Interrupted => heycode_session::AgentCompletionOutcome::Interrupted,
            }),
        }
    }

    /// Commit a named agent result through a retryable durable outbox.
    pub(crate) fn settle_agent_job(
        &self,
        jobs: &JobRegistry,
        id: &JobId,
        settlement: &JobSettlement,
        source: heycode_session::InboxSource,
    ) -> anyhow::Result<WakeDecision> {
        use heycode_session::{InboxMessage, InboxMessageId, InboxSource, SessionEventKind};
        let InboxSource::Agent {
            agent_id,
            agent_name,
            run_id,
            completion_id: Some(completion_id),
            outcome: Some(_),
            ..
        } = &source
        else {
            anyhow::bail!("agent settlement requires terminal agent provenance");
        };
        let message_id = InboxMessageId::new(completion_id.clone())?;
        let text = format!(
            "[agent {agent_name} ({agent_id}) {}; run {run_id}]\n{}",
            settlement.outcome().name(),
            settlement.notice()
        );
        let (recipient_session, existing) = {
            let session = self.session().lock().unwrap_or_else(|e| e.into_inner());
            let existing = session.events().iter().find_map(|event| match &event.kind {
                SessionEventKind::AgentInboxSplice { inserted, .. } => inserted
                    .iter()
                    .find(|message| message.id() == &message_id)
                    .cloned(),
                _ => None,
            });
            (session.id().to_string(), existing)
        };
        anyhow::ensure!(
            completion_id == &format!("agent-completion:{recipient_session}:{id}")
                && run_id == &format!("agent-run:{recipient_session}:{id}"),
            "agent completion identity does not belong to this recipient job"
        );
        let pending = jobs
            .state
            .lock()
            .map_err(|_| JobError::Unavailable)?
            .completions
            .get(id)
            .cloned();
        if let Some(pending) = pending {
            anyhow::ensure!(
                pending.recipient_session == recipient_session
                    && pending.message.source() == &source
                    && pending.message.text() == text
                    && pending.outcome == *settlement.outcome(),
                "agent settlement retry conflicts with durable occurrence"
            );
            return self.deliver_agent_completion(jobs, id, &pending);
        }
        if let Some(existing) = existing {
            anyhow::ensure!(
                existing.source() == &source && existing.text() == text,
                "agent completion identity conflicts with prior result"
            );
            let decision = if existing.delivery() == InboxDelivery::Inject {
                WakeDecision::NotRequested
            } else {
                WakeDecision::Woke
            };
            let submission = crate::inbox::enqueue_identified_inbox(
                self.session(),
                self.token().is_turn_active(),
                existing,
            )?;
            self.publish_inbox_submission(submission);
            return Ok(decision);
        }
        let reservation = jobs.reserve_settlement(id)?;
        let message =
            match InboxMessage::with_source(message_id, reservation.delivery, text, source) {
                Ok(message) => message,
                Err(error) => {
                    jobs.rollback_settlement(id, reservation);
                    return Err(error.into());
                }
            };
        let pending = AgentCompletionDelivery {
            recipient_session,
            message,
            outcome: settlement.outcome().clone(),
            decision: reservation.decision,
        };
        // Keep the completed result retryable even when persistence fails. A
        // failed/uncertain write must not turn completed work back into Running
        // (the worker finalizer would discard its result as an unhandled error).
        let committed = {
            let mut state = jobs.state.lock().map_err(|_| JobError::Unavailable)?;
            let job = state.jobs.get_mut(id).ok_or(JobError::Unknown)?;
            job.state = JobState::Settled(settlement.outcome().clone());
            job.settling = false;
            job.admission = None;
            state.completions.insert(id.clone(), pending.clone());
            if reservation.decision == WakeDecision::Demoted {
                state.demoted = state.demoted.saturating_add(1);
            }
            jobs.persist_locked(&state)
        };
        jobs.changed.notify_waiters();
        committed?;
        self.deliver_agent_completion(jobs, id, &pending)
    }

    /// Retry storage admission of a finished run without repeating its work.
    pub(crate) async fn settle_agent_job_with_retry(
        &self,
        jobs: &JobRegistry,
        id: &JobId,
        settlement: &JobSettlement,
        source: heycode_session::InboxSource,
        cancellation: CancellationToken,
    ) -> anyhow::Result<WakeDecision> {
        let mut delays = [100, 500, 2000].into_iter();
        loop {
            match self.settle_agent_job(jobs, id, settlement, source.clone()) {
                Ok(decision) => return Ok(decision),
                Err(error) => {
                    let retryable = error.chain().any(|cause| {
                        cause.downcast_ref::<std::io::Error>().is_some()
                            || cause.downcast_ref::<JobError>() == Some(&JobError::Unavailable)
                    });
                    let Some(delay) = delays.next().filter(|_| retryable) else {
                        return Err(error);
                    };
                    tokio::select! {
                        () = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {},
                        () = cancellation.cancelled() => return Err(error),
                    }
                }
            }
        }
    }

    fn deliver_agent_completion(
        &self,
        jobs: &JobRegistry,
        id: &JobId,
        pending: &AgentCompletionDelivery,
    ) -> anyhow::Result<WakeDecision> {
        // Retry the durable outbox before admission, including after a failed
        // first write. An uncertain prior write uses the same occurrence.
        jobs.persist()?;
        let submission = crate::inbox::enqueue_identified_inbox(
            self.session(),
            self.token().is_turn_active(),
            pending.message.clone(),
        )?;
        // A failed cleanup leaves the authoritative outbox retryable. The inbox
        // is already durable, so wake publication must still happen.
        let cleanup = {
            let mut state = jobs.state.lock().map_err(|_| JobError::Unavailable)?;
            let previous = state.completions.remove(id);
            let result = jobs.persist_locked(&state);
            if result.is_err()
                && let Some(previous) = previous
            {
                state.completions.insert(id.clone(), previous);
            }
            result
        };
        self.publish_inbox_submission(submission);
        cleanup?;
        Ok(pending.decision)
    }

    pub(crate) fn recover_agent_completions(&self, jobs: &JobRegistry) -> anyhow::Result<()> {
        let session_id = self
            .session()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .id()
            .to_string();
        let pending = jobs
            .state
            .lock()
            .map_err(|_| JobError::Unavailable)?
            .completions
            .iter()
            .filter(|(_, completion)| completion.recipient_session == session_id)
            .map(|(id, completion)| (id.clone(), completion.clone()))
            .collect::<Vec<_>>();
        for (id, completion) in pending {
            self.deliver_agent_completion(jobs, &id, &completion)?;
        }
        Ok(())
    }

    /// Settle one background job and durably deliver its notice.
    ///
    /// The notice reaches the A03 inbox with the job's requested delivery, or a
    /// demoted non-waking delivery when the wake budget is exhausted. Durable
    /// admission happens before any wake is reported, so a reported wake can
    /// never describe a settlement that rolled back.
    ///
    /// # Errors
    /// Unknown or already-settled job, invalid notice, or durable append
    /// failure.
    pub fn settle_job(
        &self,
        jobs: &JobRegistry,
        id: &JobId,
        settlement: &JobSettlement,
    ) -> anyhow::Result<WakeDecision> {
        self.tool_execution_context()
            .settle_job(jobs, id, settlement)
    }
}

fn validate_label(label: &str) -> Result<(), JobError> {
    if label.trim().is_empty()
        || label.len() > MAX_LABEL_BYTES
        || label.chars().any(char::is_control)
    {
        return Err(JobError::InvalidLabel);
    }
    Ok(())
}

/// `list_jobs {}` — running and settled background work.
pub struct ListJobsTool {
    jobs: Arc<JobRegistry>,
}

impl ListJobsTool {
    /// Bind the tool to the job registry.
    #[must_use]
    pub fn new(jobs: Arc<JobRegistry>) -> Self {
        Self { jobs }
    }
}

#[async_trait::async_trait]
impl heycode_tools::Tool for ListJobsTool {
    fn model_replacement(&self) -> Option<&'static str> {
        Some("job_control")
    }
    fn effect(&self) -> heycode_tools::ToolEffect {
        heycode_tools::ToolEffect::ReadOnly
    }

    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "list_jobs".to_owned(),
            description: "List background jobs with their id, label and state.".to_owned(),
            parameters: serde_json::json!({"type": "object", "additionalProperties": false}),
        }
    }

    async fn run(
        &self,
        _args: serde_json::Value,
        _cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        let lines: Vec<String> = self
            .jobs
            .list()
            .into_iter()
            .map(|job| {
                let state = match &job.state {
                    JobState::Running => "running".to_owned(),
                    JobState::Queued => "queued".to_owned(),
                    JobState::Cancelling => "cancelling".to_owned(),
                    JobState::Settled(outcome) => outcome.name().to_owned(),
                };
                format!("- {id}  {state}  {label}", id = job.id, label = job.label)
            })
            .collect();
        Ok(serde_json::Value::String(if lines.is_empty() {
            "no background jobs".to_owned()
        } else {
            lines.join("\n")
        }))
    }
}

/// `cancel_job {job_id}` — cancel one running background job.
pub struct CancelJobTool {
    jobs: Arc<JobRegistry>,
}

impl CancelJobTool {
    /// Bind the tool to the job registry.
    #[must_use]
    pub fn new(jobs: Arc<JobRegistry>) -> Self {
        Self { jobs }
    }
}

#[async_trait::async_trait]
impl heycode_tools::Tool for CancelJobTool {
    fn model_replacement(&self) -> Option<&'static str> {
        Some("job_control")
    }
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "cancel_job".to_owned(),
            description: "Cancel one running background job by id.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["job_id"],
                "properties": {"job_id": {"type": "string"}}
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        _cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        let id = args
            .get("job_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| heycode_tools::ToolError::new("`job_id` must be a string"))?;
        let id = JobId(id.to_owned());
        Ok(serde_json::Value::String(if self.jobs.cancel(&id) {
            format!("cancelled {id}")
        } else {
            format!("no running job `{id}` — try list_jobs")
        }))
    }
}

/// Global job admission bounds. A lifetime start cap limits automatic spend independently of wake replenishment.
#[derive(Debug, Clone, Copy)]
pub struct JobLimits {
    /// Maximum simultaneously executing operations.
    pub max_running: usize,
    /// Maximum operations awaiting execution capacity.
    pub max_queued: usize,
    /// Maximum retained terminal rows.
    pub max_history: usize,
    /// Maximum starts for this durable session; not replenished by model turns.
    pub max_admissions: u64,
}
impl Default for JobLimits {
    fn default() -> Self {
        Self {
            max_running: 8,
            max_queued: 64,
            max_history: 256,
            max_admissions: 0,
        }
    }
}
impl JobLimits {
    fn normalized(self) -> Self {
        Self {
            max_running: self.max_running.clamp(1, 256),
            max_queued: self.max_queued.min(4096),
            max_history: self.max_history.clamp(1, 4096),
            max_admissions: self.max_admissions,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct JobHistory {
    version: u8,
    next_id: u64,
    jobs: Vec<JobSnapshot>,
    #[serde(default)]
    completions: BTreeMap<JobId, AgentCompletionDelivery>,
}

/// Durable outbox payload: written with terminal job state before inbox admission.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AgentCompletionDelivery {
    recipient_session: String,
    message: heycode_session::InboxMessage,
    outcome: JobOutcome,
    decision: WakeDecision,
}
impl JobRegistry {
    /// Persist bounded summaries beside the owning session. Recover live rows as Interrupted.
    pub fn attach_history(&self, path: std::path::PathBuf) -> Result<(), JobError> {
        if self
            .history
            .lock()
            .map_err(|_| JobError::Unavailable)?
            .is_some()
        {
            return Err(JobError::Unavailable);
        }
        if path.exists() {
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| JobError::Unavailable)?;
            if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
                return Err(JobError::Unavailable);
            }
            let history: JobHistory =
                serde_json::from_slice(&std::fs::read(&path).map_err(|_| JobError::Unavailable)?)
                    .map_err(|_| JobError::Unavailable)?;
            if history.version != 1 {
                return Err(JobError::Unavailable);
            }
            let mut state = self.state.lock().map_err(|_| JobError::Unavailable)?;
            state.next_id = history.next_id;
            state.completions = history.completions;
            for row in history.jobs {
                state.jobs.insert(
                    row.id,
                    JobEntry {
                        coordinator: row.coordinator,
                        label: row.label,
                        delivery: row.delivery,
                        state: match row.state {
                            JobState::Settled(outcome) => JobState::Settled(outcome),
                            _ => JobState::Settled(JobOutcome::Interrupted),
                        },
                        settling: false,
                        cancellation: CancellationToken::new(),
                        handle: None,
                        admission: None,
                    },
                );
            }
            self.prune_locked(&mut state);
        }
        *self.history.lock().map_err(|_| JobError::Unavailable)? = Some(path);
        self.persist()
    }
    fn prune_locked(&self, state: &mut RegistryState) {
        let mut settled = state
            .jobs
            .iter()
            .filter(|(id, row)| {
                !state.completions.contains_key(*id)
                    && matches!(row.state, JobState::Settled(_))
                    && row
                        .handle
                        .as_ref()
                        .is_none_or(tokio::task::JoinHandle::is_finished)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        settled.sort_by_key(|id| {
            id.as_str()
                .strip_prefix("job-")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
        });
        let count = settled.len().saturating_sub(self.limits.max_history);
        for id in settled.into_iter().take(count) {
            state.jobs.remove(&id);
        }
    }
    fn persist(&self) -> Result<(), JobError> {
        let state = self.state.lock().map_err(|_| JobError::Unavailable)?;
        self.persist_locked(&state)
    }
    fn persist_locked(&self, state: &RegistryState) -> Result<(), JobError> {
        let history = self.history.lock().map_err(|_| JobError::Unavailable)?;
        let Some(path) = history.as_ref() else {
            return Ok(());
        };
        let rows = state
            .jobs
            .iter()
            .map(|(id, job)| JobSnapshot {
                id: id.clone(),
                label: job.label.clone(),
                state: job.state.clone(),
                delivery: job.delivery,
                coordinator: job.coordinator,
            })
            .collect();
        let bytes = serde_json::to_vec(&JobHistory {
            version: 1,
            next_id: state.next_id,
            jobs: rows,
            completions: state.completions.clone(),
        })
        .map_err(|_| JobError::Unavailable)?;
        let tmp = path.with_extension("tmp");
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp).map_err(|_| JobError::Unavailable)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| JobError::Unavailable)?;
        std::fs::rename(tmp, path).map_err(|_| JobError::Unavailable)?;
        #[cfg(unix)]
        if let Some(directory) = path.parent() {
            std::fs::File::open(directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| JobError::Unavailable)?;
        }
        Ok(())
    }
    /// Change delivery on a live job before settlement is reserved (e.g. foreground promotion).
    /// Returns false if unknown, terminal, settling, or durable persistence fails.
    pub fn set_delivery(&self, id: &JobId, delivery: InboxDelivery) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(job) = state
            .jobs
            .get_mut(id)
            .filter(|job| !job.settling && !matches!(job.state, JobState::Settled(_)))
        else {
            return false;
        };
        let previous = job.delivery;
        job.delivery = delivery;
        if self.persist_locked(&state).is_err() {
            if let Some(job) = state.jobs.get_mut(id) {
                job.delivery = previous;
            }
            return false;
        }
        drop(state);
        self.changed.notify_waiters();
        true
    }

    /// Shared event wake reservation. A monitor event consumes the same budget as job completion.
    pub fn reserve_event_wake(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.wake_capacity == 0 {
            return true;
        }
        if state.wake_remaining == 0 {
            return false;
        }
        state.wake_remaining -= 1;
        true
    }
    /// Configured global limits, visible to execution/UI consumers.
    #[must_use]
    pub const fn limits(&self) -> JobLimits {
        self.limits
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod admission_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn default_jobs_have_no_lifetime_admission_cap() {
        let jobs = Arc::new(JobRegistry::with_limits(0, JobLimits::default()));
        jobs.state.lock().unwrap().next_id = 5000;
        let (send, receive) = tokio::sync::oneshot::channel();
        jobs.spawn(
            "past the old cap",
            InboxDelivery::Inject,
            move |_, _| async move {
                let _ = send.send(());
            },
        )
        .unwrap();
        receive.await.unwrap();
        assert!(jobs.reserve_event_wake());
        assert!(jobs.reserve_event_wake());
    }

    #[tokio::test]
    async fn global_queue_cancellation_and_admission_budget_are_enforced() {
        let jobs = Arc::new(JobRegistry::with_limits(
            1,
            JobLimits {
                max_running: 1,
                max_queued: 1,
                max_history: 2,
                max_admissions: 2,
            },
        ));
        let starts = Arc::new(AtomicUsize::new(0));
        let release = CancellationToken::new();
        let owner = jobs.clone();
        let work = starts.clone();
        let release_work = release.clone();
        let first = jobs.spawn("first", InboxDelivery::Inject, move |id, token| async move {
            work.fetch_add(1, Ordering::SeqCst);
            tokio::select! { () = release_work.cancelled() => {}, () = token.cancelled() => {} }
            owner.settle(&id, JobOutcome::Completed).unwrap();
        }).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while starts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let owner = jobs.clone();
        let work = starts.clone();
        let queued = jobs
            .spawn(
                "queued",
                InboxDelivery::Inject,
                move |id, token| async move {
                    if !token.is_cancelled() {
                        work.fetch_add(1, Ordering::SeqCst);
                    }
                    owner.settle(&id, JobOutcome::Cancelled).unwrap();
                },
            )
            .unwrap();
        assert_eq!(
            jobs.list()
                .iter()
                .find(|row| row.id == queued)
                .unwrap()
                .state,
            JobState::Queued
        );
        assert_eq!(
            jobs.spawn("refused", InboxDelivery::Inject, |_, _| async {}),
            Err(JobError::Capacity)
        );
        assert!(jobs.cancel(&queued));
        assert_eq!(
            jobs.wait_for_settlement(&queued).await.unwrap(),
            JobOutcome::Cancelled
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        release.cancel();
        jobs.wait_for_settlement(&first).await.unwrap();
        assert_eq!(
            jobs.spawn("still refused", InboxDelivery::Inject, |_, _| async {}),
            Err(JobError::Capacity)
        );
    }
    #[tokio::test]
    async fn crash_recovery_marks_interrupted_and_never_reuses_job_ids() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("jobs.json");
        let jobs = Arc::new(JobRegistry::default());
        jobs.attach_history(path.clone()).unwrap();
        let first = jobs
            .spawn("held", InboxDelivery::Inject, |_, token| async move {
                token.cancelled().await;
            })
            .unwrap();
        jobs.dispose(); // simulate process-owned handles disappearing with a durable live snapshot
        let recovered = Arc::new(JobRegistry::default());
        recovered.attach_history(path).unwrap();
        assert_eq!(
            recovered.list()[0].state,
            JobState::Settled(JobOutcome::Interrupted)
        );
        let next = recovered
            .spawn("next", InboxDelivery::Inject, |_, _| async {})
            .unwrap();
        assert_ne!(first, next);
        recovered.dispose();
    }
    #[tokio::test]
    async fn worker_panic_settles_failed_and_does_not_poison_capacity() {
        let jobs = Arc::new(JobRegistry::with_limits(
            1,
            JobLimits {
                max_running: 1,
                ..JobLimits::default()
            },
        ));
        let id = jobs
            .spawn("panic", InboxDelivery::Inject, |_, _| async {
                panic!("fixture panic");
            })
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                jobs.wait_for_settlement(&id)
            )
            .await
            .unwrap()
            .unwrap(),
            JobOutcome::Failed
        );
        let id = jobs
            .spawn("next", InboxDelivery::Inject, |_, _| async {})
            .unwrap();
        assert_eq!(
            jobs.wait_for_settlement(&id).await.unwrap(),
            JobOutcome::Failed
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn idle_handle() -> tokio::task::JoinHandle<()> {
        tokio::spawn(async {})
    }

    #[tokio::test]
    async fn labels_validate_and_ids_are_stable_and_ordered() {
        let registry = JobRegistry::default();
        assert_eq!(
            registry
                .admit(
                    "  ",
                    InboxDelivery::Inject,
                    CancellationToken::new(),
                    idle_handle()
                )
                .unwrap_err(),
            JobError::InvalidLabel
        );
        assert_eq!(
            registry
                .admit(
                    "bad\nlabel",
                    InboxDelivery::Inject,
                    CancellationToken::new(),
                    idle_handle()
                )
                .unwrap_err(),
            JobError::InvalidLabel
        );
        let first = registry
            .admit(
                "index",
                InboxDelivery::Inject,
                CancellationToken::new(),
                idle_handle(),
            )
            .unwrap();
        let second = registry
            .admit(
                "scan",
                InboxDelivery::Inject,
                CancellationToken::new(),
                idle_handle(),
            )
            .unwrap();
        assert_ne!(first, second);
        let listed: Vec<String> = registry.list().into_iter().map(|job| job.label).collect();
        assert_eq!(listed, vec!["index".to_owned(), "scan".to_owned()]);
    }

    #[tokio::test]
    async fn settlement_happens_exactly_once() {
        let registry = JobRegistry::default();
        let id = registry
            .admit(
                "once",
                InboxDelivery::Inject,
                CancellationToken::new(),
                idle_handle(),
            )
            .unwrap();
        assert!(registry.settle(&id, JobOutcome::Completed).is_ok());
        assert_eq!(
            registry.settle(&id, JobOutcome::Failed).unwrap_err(),
            JobError::Unknown,
            "a settled job cannot settle again"
        );
        assert_eq!(
            registry.list()[0].state,
            JobState::Settled(JobOutcome::Completed),
            "the first outcome is the one that stands"
        );
    }

    #[tokio::test]
    async fn the_wake_budget_demotes_rather_than_dropping_and_refills_per_turn() {
        let registry = JobRegistry::new(1);
        let mut ids = Vec::new();
        for index in 0..3 {
            ids.push(
                registry
                    .admit(
                        format!("job{index}"),
                        InboxDelivery::FollowUp,
                        CancellationToken::new(),
                        idle_handle(),
                    )
                    .unwrap(),
            );
        }
        let first = registry.settle(&ids[0], JobOutcome::Completed).unwrap();
        assert_eq!(first, (InboxDelivery::FollowUp, WakeDecision::Woke));

        // The budget is spent, so the notice still goes out — demoted, never
        // dropped.
        let second = registry.settle(&ids[1], JobOutcome::Completed).unwrap();
        assert_eq!(second, (InboxDelivery::Inject, WakeDecision::Demoted));
        assert_eq!(registry.demoted_wakes(), 1);

        // A turn settling refills the budget, so cost is bounded per turn
        // rather than per job.
        registry.replenish_wakes();
        let third = registry.settle(&ids[2], JobOutcome::Completed).unwrap();
        assert_eq!(third, (InboxDelivery::FollowUp, WakeDecision::Woke));
        assert_eq!(registry.demoted_wakes(), 1);
    }

    #[tokio::test]
    async fn a_non_waking_delivery_never_spends_budget() {
        let registry = JobRegistry::new(1);
        let id = registry
            .admit(
                "quiet",
                InboxDelivery::Inject,
                CancellationToken::new(),
                idle_handle(),
            )
            .unwrap();
        assert_eq!(
            registry.settle(&id, JobOutcome::Completed).unwrap(),
            (InboxDelivery::Inject, WakeDecision::NotRequested)
        );
        let waking = registry
            .admit(
                "loud",
                InboxDelivery::Steer,
                CancellationToken::new(),
                idle_handle(),
            )
            .unwrap();
        assert_eq!(
            registry.settle(&waking, JobOutcome::Completed).unwrap().1,
            WakeDecision::Woke,
            "the inject settlement must not have consumed the token"
        );
    }

    #[tokio::test]
    async fn cancel_and_dispose_release_every_owned_task() {
        let registry = JobRegistry::default();
        let token = CancellationToken::new();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = observed.clone();
        let watch = token.clone();
        let id = registry
            .admit(
                "long",
                InboxDelivery::Inject,
                token,
                tokio::spawn(async move {
                    watch.cancelled().await;
                    flag.store(true, std::sync::atomic::Ordering::Release);
                }),
            )
            .unwrap();
        assert!(registry.cancel(&id));
        tokio::task::yield_now().await;
        assert!(observed.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            registry.cancel(&id),
            "cancel is idempotent while the job has not settled"
        );
        registry.settle(&id, JobOutcome::Cancelled).unwrap();
        assert!(
            !registry.cancel(&id),
            "a settled job is no longer cancellable"
        );

        let second = registry
            .admit(
                "kept",
                InboxDelivery::Inject,
                CancellationToken::new(),
                tokio::spawn(std::future::pending::<()>()),
            )
            .unwrap();
        registry.dispose();
        assert!(registry.list().is_empty());
        assert_eq!(
            registry.settle(&second, JobOutcome::Completed).unwrap_err(),
            JobError::Unknown
        );
        assert_eq!(
            registry
                .admit(
                    "after",
                    InboxDelivery::Inject,
                    CancellationToken::new(),
                    idle_handle()
                )
                .unwrap_err(),
            JobError::Unavailable,
            "a disposed registry admits nothing"
        );
    }
}

impl crate::code_mode::ToolExecutionContext {
    pub(crate) fn settle_job(
        &self,
        jobs: &JobRegistry,
        id: &JobId,
        settlement: &JobSettlement,
    ) -> anyhow::Result<WakeDecision> {
        self.settle_job_delivery(jobs, id, settlement, false)
    }

    /// Foreground output returns through the original tool result. Only a job
    /// explicitly promoted before settlement also delivers an inbox notice.
    pub(crate) fn settle_foreground_job(
        &self,
        jobs: &JobRegistry,
        id: &JobId,
        settlement: &JobSettlement,
    ) -> anyhow::Result<WakeDecision> {
        self.settle_job_delivery(jobs, id, settlement, true)
    }

    fn settle_job_delivery(
        &self,
        jobs: &JobRegistry,
        id: &JobId,
        settlement: &JobSettlement,
        foreground: bool,
    ) -> anyhow::Result<WakeDecision> {
        let reservation = jobs.reserve_settlement(id)?;
        if foreground && reservation.decision == WakeDecision::NotRequested {
            let (_, decision) =
                jobs.commit_settlement(id, settlement.outcome().clone(), reservation)?;
            return Ok(decision);
        }
        let text = format!(
            "[job {id} {outcome}] {notice}",
            outcome = settlement.outcome().name(),
            notice = settlement.notice()
        );
        let submission = match self.enqueue_inbox_with_source(
            reservation.delivery,
            text,
            heycode_session::InboxSource::Job {
                job_id: id.to_string(),
            },
        ) {
            Ok(submission) => submission,
            Err(error) => {
                jobs.rollback_settlement(id, reservation);
                return Err(error);
            }
        };
        if let Err(error) = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .flush()
        {
            jobs.rollback_settlement(id, reservation);
            return Err(error.into());
        }
        let (_, decision) =
            jobs.commit_settlement(id, settlement.outcome().clone(), reservation)?;
        self.publish_inbox_submission(submission);
        Ok(decision)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod agent_completion_tests {
    use super::*;
    use heycode_session::{InboxMessage, SessionEventKind};

    pub(crate) fn fixture() -> (heycode_core::Context, tempfile::TempDir, Arc<Agent>) {
        let dir = tempfile::tempdir().unwrap();
        let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
            heycode_session::session_plugin(dir.path().to_path_buf()),
            heycode_prompt::prompt_plugin(),
            heycode_exec::local_execution_plugin(
                heycode_exec::LocalShellConfig::platform(
                    dir.path().to_path_buf(),
                    std::time::Duration::from_secs(30),
                )
                .unwrap(),
            ),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_web::web_registry_plugin(),
            heycode_tools::tools_plugin(heycode_tools::ToolsConfig::default()),
            heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
            heycode_llm::token_counters_plugin(),
            heycode_llm::llm_plugin(
                heycode_llm::LlmSelection {
                    provider_name: "fake".into(),
                    model: "test-model".into(),
                },
                vec![Arc::new(heycode_llm::testing::FakeProvider::repeating(
                    vec![
                        heycode_llm::StreamChunk::TextDelta("used the finding".into()),
                        heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::Stop),
                    ],
                ))],
            ),
            crate::approval_plugin(Arc::new(crate::AutoApprove)),
            crate::commands_plugin(),
            crate::compactions_plugin(),
            crate::agent_options_plugin(crate::AgentOptions::default()),
            crate::agent_plugin(),
        ];
        let context = heycode_core::compose(&plugins).unwrap();
        let agent = context.get::<Agent>(crate::SERVICE_AGENT).unwrap();
        (context, dir, agent)
    }

    fn admitted(agent: &Agent) -> usize {
        agent.session().lock().unwrap().events().iter().filter(|event| matches!(&event.kind, SessionEventKind::AgentInboxSplice { inserted, .. } if inserted.iter().any(|message| matches!(message.source(), heycode_session::InboxSource::Agent { .. })))).count()
    }

    fn job(jobs: &JobRegistry) -> JobId {
        jobs.admit(
            "Architecture",
            InboxDelivery::Steer,
            CancellationToken::new(),
            tokio::spawn(async {}),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn agent_completion_retry_dedupes_pending_claimed_and_cancelled_occurrences() {
        let (mut context, _dir, agent) = fixture();
        let jobs = JobRegistry::new(0);
        let id = job(&jobs);
        let settlement = JobSettlement::new(JobOutcome::Completed, "same finding").unwrap();
        let source = agent.agent_completion_source(
            &id,
            "task-0",
            "Architecture",
            "main",
            settlement.outcome(),
        );
        agent
            .settle_agent_job(&jobs, &id, &settlement, source.clone())
            .unwrap();
        agent
            .settle_agent_job(&jobs, &id, &settlement, source.clone())
            .unwrap();
        assert_eq!(admitted(&agent), 1);
        let pending = agent.next_wakeable_message().unwrap();
        agent
            .send_inbox_id_cancellable(&pending, CancellationToken::new())
            .await
            .unwrap();
        agent
            .settle_agent_job(&jobs, &id, &settlement, source)
            .unwrap();
        assert_eq!(admitted(&agent), 1);
        assert!(agent.pending_inbox().is_empty());
        let second = job(&jobs);
        let source = agent.agent_completion_source(
            &second,
            "task-0",
            "Architecture",
            "main",
            settlement.outcome(),
        );
        agent
            .settle_agent_job(&jobs, &second, &settlement, source.clone())
            .unwrap();
        assert_eq!(
            admitted(&agent),
            2,
            "equal text on a different run is a distinct occurrence"
        );
        agent
            .cancel_inbox(&agent.next_wakeable_message().unwrap())
            .unwrap();
        agent
            .settle_agent_job(&jobs, &second, &settlement, source)
            .unwrap();
        assert_eq!(admitted(&agent), 2);
        assert!(agent.pending_inbox().is_empty());
        jobs.dispose();
        context.shutdown();
    }

    #[tokio::test]
    async fn agent_completion_outbox_survives_restart_before_admission() {
        let (mut context, dir, agent) = fixture();
        let path = dir.path().join("agent-jobs.json");
        let jobs = JobRegistry::new(0);
        jobs.attach_history(path.clone()).unwrap();
        let id = job(&jobs);
        let source = agent.agent_completion_source(
            &id,
            "task-0",
            "Architecture",
            "main",
            &JobOutcome::Completed,
        );
        let completion_id = match &source {
            heycode_session::InboxSource::Agent {
                completion_id: Some(id),
                ..
            } => id.clone(),
            _ => unreachable!(),
        };
        let message = InboxMessage::with_source(
            heycode_session::InboxMessageId::new(completion_id).unwrap(),
            InboxDelivery::Steer,
            "retained finding",
            source,
        )
        .unwrap();
        {
            let mut state = jobs.state.lock().unwrap();
            state.jobs.get_mut(&id).unwrap().state = JobState::Settled(JobOutcome::Completed);
            state.completions.insert(
                id.clone(),
                AgentCompletionDelivery {
                    recipient_session: agent.session().lock().unwrap().id().to_string(),
                    message,
                    outcome: JobOutcome::Completed,
                    decision: WakeDecision::Woke,
                },
            );
            jobs.persist_locked(&state).unwrap();
        }
        let recovered = JobRegistry::new(0);
        recovered.attach_history(path).unwrap();
        agent.recover_agent_completions(&recovered).unwrap();
        agent.recover_agent_completions(&recovered).unwrap();
        assert_eq!(admitted(&agent), 1);
        assert_eq!(agent.pending_inbox().next_step, 1);
        assert!(recovered.state.lock().unwrap().completions.is_empty());
        jobs.dispose();
        recovered.dispose();
        context.shutdown();
    }

    #[tokio::test]
    async fn agent_completion_storage_failures_remain_retryable_without_duplicate_admission() {
        let (mut context, dir, agent) = fixture();
        let jobs = JobRegistry::new(0);
        let history = dir.path().join("agent-jobs.json");
        jobs.attach_history(history.clone()).unwrap();
        let id = job(&jobs);
        let settlement = JobSettlement::new(JobOutcome::Completed, "durable finding").unwrap();
        let source = agent.agent_completion_source(
            &id,
            "task-0",
            "Architecture",
            "main",
            settlement.outcome(),
        );
        let blocker = history.with_extension("tmp");
        std::fs::create_dir(&blocker).unwrap();
        assert!(
            agent
                .settle_agent_job(&jobs, &id, &settlement, source.clone())
                .is_err()
        );
        assert_eq!(
            admitted(&agent),
            0,
            "outbox persistence precedes inbox insertion"
        );
        std::fs::remove_dir(&blocker).unwrap();
        let block_cleanup = blocker.clone();
        agent.session().lock().unwrap().bus().on::<heycode_session::SessionEvent>(move |event| {
            if matches!(&event.kind, SessionEventKind::AgentInboxSplice { inserted, .. } if !inserted.is_empty()) {
                std::fs::create_dir(&block_cleanup).unwrap();
            }
        });
        assert!(
            agent
                .settle_agent_job(&jobs, &id, &settlement, source.clone())
                .is_err()
        );
        assert_eq!(admitted(&agent), 1);
        assert_eq!(jobs.state.lock().unwrap().completions.len(), 1);
        std::fs::remove_dir(blocker).unwrap();
        agent
            .settle_agent_job(&jobs, &id, &settlement, source)
            .unwrap();
        assert_eq!(admitted(&agent), 1);
        assert!(jobs.state.lock().unwrap().completions.is_empty());
        jobs.dispose();
        context.shutdown();
    }

    #[tokio::test]
    async fn runtime_inbox_driver_batches_idle_agent_messages_without_polling() {
        let (mut context, _dir, agent) = fixture();
        let jobs = Arc::new(JobRegistry::new(0));
        let registry = Arc::new(crate::SubagentRegistry::default());
        registry
            .attach_job_host(&context, &agent, jobs.clone())
            .unwrap();
        agent
            .submit_inbox(InboxDelivery::Inject, "context only")
            .unwrap();
        assert!(
            !agent
                .inbox_driver_running
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        let ended = Arc::new(tokio::sync::Notify::new());
        let notified = ended.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let sink = ended.clone();
        agent.ui().on::<crate::UiEvent>(move |event| {
            if matches!(event, crate::UiEvent::TurnFinished { .. }) {
                sink.notify_waiters();
            }
        });
        for number in 0..3 {
            agent
                .submit_inbox_with_source(
                    InboxDelivery::Steer,
                    format!("finding {number}"),
                    heycode_session::InboxSource::Agent {
                        agent_id: format!("task-{number}"),
                        agent_name: format!("Research {number}"),
                        recipient_id: "main".into(),
                        run_id: format!("run-{number}"),
                        completion_id: None,
                        outcome: None,
                    },
                )
                .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), notified)
            .await
            .unwrap();
        jobs.wait_for_task_exit(&JobId::parse("job-0").unwrap())
            .await
            .unwrap();
        assert!(agent.pending_inbox().is_empty());
        assert_eq!(
            agent
                .session()
                .lock()
                .unwrap()
                .events()
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::TurnStart { .. }))
                .count(),
            1
        );
        assert_eq!(
            jobs.list().len(),
            1,
            "one owned driver handles the pending batch"
        );
        jobs.dispose();
        context.shutdown();
    }
    #[tokio::test]
    async fn headless_wait_keeps_background_completion_and_synthesis_alive() {
        let (mut context, _dir, agent) = fixture();
        let jobs = Arc::new(JobRegistry::new(0));
        agent.install_jobs(jobs.clone());
        let registry = Arc::new(crate::SubagentRegistry::default());
        registry
            .attach_job_host(&context, &agent, jobs.clone())
            .unwrap();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let recipient = agent.clone();
        let owned_jobs = jobs.clone();
        jobs.spawn_coordinator(
            "Architecture",
            InboxDelivery::Steer,
            move |id, _| async move {
                released.await.unwrap();
                let settlement =
                    JobSettlement::new(JobOutcome::Completed, "the background finding").unwrap();
                let source = recipient.agent_completion_source(
                    &id,
                    "task-0",
                    "Architecture",
                    "main",
                    settlement.outcome(),
                );
                recipient
                    .settle_agent_job(&owned_jobs, &id, &settlement, source)
                    .unwrap();
            },
        )
        .unwrap();
        agent.send("delegate work").await.unwrap();
        let waiting = agent.wait_for_background(CancellationToken::new());
        tokio::pin!(waiting);
        assert!(matches!(
            futures::poll!(&mut waiting),
            std::task::Poll::Pending
        ));
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admitted(&agent), 1);
        assert!(agent.pending_inbox().is_empty());
        assert_eq!(
            agent
                .session()
                .lock()
                .unwrap()
                .events()
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::TurnStart { .. }))
                .count(),
            2
        );
        assert_eq!(jobs.state.lock().unwrap().active_workers, 0);
        jobs.dispose();
        context.shutdown();
    }

    #[tokio::test]
    async fn cancelled_parent_keeps_agent_input_pending_until_explicit_resume() {
        let (mut context, _dir, agent) = fixture();
        let jobs = Arc::new(JobRegistry::new(0));
        let registry = Arc::new(crate::SubagentRegistry::default());
        registry
            .attach_job_host(&context, &agent, jobs.clone())
            .unwrap();
        let cancel = agent.token();
        let first = std::sync::atomic::AtomicBool::new(true);
        agent.ui().on::<crate::UiEvent>(move |event| {
            if matches!(event, crate::UiEvent::AssistantDelta { .. })
                && first.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                cancel.cancel();
            }
        });
        assert_eq!(agent.send("first turn").await.unwrap().reason, "aborted");
        agent
            .submit_inbox_with_source(
                InboxDelivery::Steer,
                "retained finding",
                heycode_session::InboxSource::Agent {
                    agent_id: "task-0".into(),
                    agent_name: "Architecture".into(),
                    recipient_id: "main".into(),
                    run_id: "run-0".into(),
                    completion_id: None,
                    outcome: None,
                },
            )
            .unwrap();
        assert_eq!(agent.pending_inbox().next_step, 1);
        assert!(
            jobs.list().is_empty(),
            "a completion cannot resurrect a cancelled parent"
        );
        assert_eq!(
            agent.send("resume explicitly").await.unwrap().reason,
            "stop"
        );
        assert!(agent.pending_inbox().is_empty());
        assert_eq!(
            agent
                .session()
                .lock()
                .unwrap()
                .events()
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::TurnStart { .. }))
                .count(),
            2
        );
        jobs.dispose();
        context.shutdown();
    }

    #[tokio::test]
    async fn foreground_delivery_barrier_defers_automatic_start_without_blocking_user_input() {
        let (mut context, _dir, agent) = fixture();
        let barrier = agent.defer_inbox_wakes();
        let (id, _) = agent
            .submit_inbox(InboxDelivery::Steer, "agent finding")
            .unwrap();
        let automatic = agent.send_automatic_inbox_id_cancellable(&id, CancellationToken::new());
        tokio::pin!(automatic);
        assert!(matches!(
            futures::poll!(&mut automatic),
            std::task::Poll::Pending
        ));
        assert!(!agent.token().is_turn_active());
        agent.send("foreground owner still works").await.unwrap();
        assert!(agent.pending_inbox().is_empty());
        drop(barrier);
        let error = automatic.await.unwrap_err();
        assert!(error.downcast_ref::<crate::FollowUpError>().is_some());
        assert_eq!(
            agent
                .session()
                .lock()
                .unwrap()
                .events()
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::TurnStart { .. }))
                .count(),
            1
        );
        context.shutdown();
    }
}

//! Effect-owned async check registry.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::FutureExt as _;
use tokio_util::sync::CancellationToken;

use crate::{DoctorCheckId, DoctorError, DoctorOutcome, DoctorReport};

/// One independently owned diagnostic check.
#[async_trait]
pub trait DoctorCheck: Send + Sync {
    /// Stable registry id.
    fn id(&self) -> &DoctorCheckId;

    /// Execute without publishing secret-bearing or arbitrary runtime text.
    async fn run(&self, cancellation: CancellationToken) -> Result<DoctorOutcome, DoctorError>;
}

/// How long one check occupied the run, alongside the check it belongs to.
///
/// Wall time, not CPU time, and measured around the whole arm — a check that
/// was skipped because the run was cancelled still reports the time that
/// decision took. The point of measuring at all is GOTCHAS #155: a host that
/// takes 11-23s to first-execute a newly written binary reports a good binary
/// as unavailable under a 5s budget, and only a duration next to the verdict
/// distinguishes "slow once" from "broken".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheckTiming {
    /// Check this timing belongs to.
    pub id: DoctorCheckId,
    /// Measured wall time.
    pub duration: Duration,
}

/// One completed doctor run: the report, plus what each check cost.
///
/// Timings are a separate list rather than a field on [`DoctorCheckResult`]
/// because the report is a stable published wire schema and a duration is not
/// part of it. Ids are unique in a registry, so a timing names its check
/// without depending on positional alignment with `report.checks`.
#[derive(Debug, Clone)]
pub struct DoctorRun {
    /// The same report [`DoctorRegistry::run`] would have produced.
    pub report: DoctorReport,
    /// Per-check wall time, in the same order as `report.checks`.
    pub timings: Vec<DoctorCheckTiming>,
}

struct CheckEntry {
    check: Arc<dyn DoctorCheck>,
    token: Arc<()>,
}

#[derive(Default)]
struct RegistryState {
    checks: Vec<CheckEntry>,
}

/// Shared ordered registry for plugin-contributed checks.
#[derive(Clone, Default)]
pub struct DoctorRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

impl DoctorRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a check as a context effect.
    ///
    /// # Errors
    /// Duplicate ids or poisoned registry state fail before publication.
    pub fn register(
        &self,
        context: &heycode_core::Context,
        check: Arc<dyn DoctorCheck>,
    ) -> Result<(), DoctorError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| DoctorError::RegistryUnavailable)?;
        if state
            .checks
            .iter()
            .any(|entry| entry.check.id() == check.id())
        {
            return Err(DoctorError::DuplicateCheck {
                id: check.id().as_str().to_owned(),
            });
        }
        let token = Arc::new(());
        let registration = CheckRegistration {
            inner: Arc::downgrade(&self.inner),
            id: check.id().clone(),
            token: token.clone(),
            active: true,
        };
        state.checks.push(CheckEntry { check, token });
        drop(state);
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Snapshot registered ids in deterministic registration order.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn check_ids(&self) -> Result<Vec<DoctorCheckId>, DoctorError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| DoctorError::RegistryUnavailable)?;
        Ok(state
            .checks
            .iter()
            .map(|entry| entry.check.id().clone())
            .collect())
    }

    /// Run one immutable check snapshot in registration order.
    ///
    /// Cancellation skips current/remaining checks. A plugin panic becomes a
    /// failing structured result and cannot starve later checks.
    ///
    /// # Errors
    /// Poisoned registry state fails before any check runs.
    pub async fn run(&self, cancellation: CancellationToken) -> Result<DoctorReport, DoctorError> {
        self.run_timed(cancellation).await.map(|run| run.report)
    }

    /// Run one immutable check snapshot, measuring each check.
    ///
    /// Identical to [`Self::run`] in every observable way except that it also
    /// returns how long each check took.
    ///
    /// # Errors
    /// Poisoned registry state fails before any check runs.
    pub async fn run_timed(
        &self,
        cancellation: CancellationToken,
    ) -> Result<DoctorRun, DoctorError> {
        let checks: Vec<_> = {
            let state = self
                .inner
                .lock()
                .map_err(|_| DoctorError::RegistryUnavailable)?;
            state
                .checks
                .iter()
                .map(|entry| entry.check.clone())
                .collect()
        };
        let mut results = Vec::with_capacity(checks.len());
        let mut timings = Vec::with_capacity(results.capacity());
        for check in checks {
            let started = Instant::now();
            let outcome = if cancellation.is_cancelled() {
                DoctorOutcome::cancelled()
            } else {
                let future = AssertUnwindSafe(check.run(cancellation.clone())).catch_unwind();
                tokio::select! {
                    () = cancellation.cancelled() => DoctorOutcome::cancelled(),
                    result = future => match result {
                        Ok(Ok(outcome)) => outcome,
                        Ok(Err(_)) => DoctorOutcome::invalid_contract(),
                        Err(_) => DoctorOutcome::panicked(),
                    },
                }
            };
            timings.push(DoctorCheckTiming {
                id: check.id().clone(),
                duration: started.elapsed(),
            });
            results.push(outcome.into_result(check.id().clone()));
        }
        Ok(DoctorRun {
            report: DoctorReport::from_checks(results),
            timings,
        })
    }
}

struct CheckRegistration {
    inner: Weak<Mutex<RegistryState>>,
    id: DoctorCheckId,
    token: Arc<()>,
    active: bool,
}

impl Drop for CheckRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.lock() else {
            return;
        };
        state.checks.retain(|entry| {
            entry.check.id() != &self.id || !Arc::ptr_eq(&entry.token, &self.token)
        });
    }
}

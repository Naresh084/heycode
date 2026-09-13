//! Shared operation admission and quiescent shutdown.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use heycode_runtime::RuntimeError;

pub(crate) struct Lifecycle {
    admission: Mutex<()>,
    closed: AtomicBool,
    active: AtomicUsize,
    settled: Notify,
    shutdown: CancellationToken,
}

impl Lifecycle {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            admission: Mutex::new(()),
            closed: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            settled: Notify::new(),
            shutdown: CancellationToken::new(),
        })
    }

    pub(crate) fn begin(
        self: &Arc<Self>,
        caller: &CancellationToken,
    ) -> Result<OperationLease, RuntimeError> {
        if caller.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let _admission = self
            .admission
            .lock()
            .map_err(|_| RuntimeError::internal("Claude lifecycle admission"))?;
        if self.closed.load(Ordering::SeqCst) || self.shutdown.is_cancelled() {
            return Err(RuntimeError::closed());
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok(OperationLease {
            lifecycle: self.clone(),
        })
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub(crate) fn force_close(&self) {
        self.shutdown.cancel();
        if let Ok(_admission) = self.admission.lock() {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn check(&self, caller: &CancellationToken) -> Result<(), RuntimeError> {
        if caller.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else if self.closed.load(Ordering::SeqCst) || self.shutdown.is_cancelled() {
            Err(RuntimeError::closed())
        } else {
            Ok(())
        }
    }

    pub(crate) async fn close(&self, caller: CancellationToken) -> Result<(), RuntimeError> {
        self.force_close();
        loop {
            let settled = self.settled.notified();
            if self.active.load(Ordering::SeqCst) == 0 {
                break;
            }
            settled.await;
        }
        if caller.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else {
            Ok(())
        }
    }
}

pub(crate) struct OperationLease {
    lifecycle: Arc<Lifecycle>,
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        self.lifecycle.active.fetch_sub(1, Ordering::SeqCst);
        self.lifecycle.settled.notify_waiters();
    }
}

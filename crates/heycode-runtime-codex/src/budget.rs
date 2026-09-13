//! Aggregate retained inbound memory accounting.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::{CodexAppServerError, CodexAppServerErrorCode};

pub(crate) const MAX_INBOUND_RETAINED_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct InboundBudget {
    retained: Mutex<usize>,
}

impl InboundBudget {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            retained: Mutex::new(0),
        })
    }

    pub(crate) fn acquire(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<InboundPermit, CodexAppServerError> {
        let mut retained = lock(&self.retained);
        let next = retained
            .checked_add(bytes)
            .filter(|next| *next <= MAX_INBOUND_RETAINED_BYTES)
            .ok_or_else(|| CodexAppServerError::new(CodexAppServerErrorCode::Overloaded))?;
        *retained = next;
        Ok(InboundPermit {
            budget: Arc::clone(self),
            bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn retained(&self) -> usize {
        *lock(&self.retained)
    }
}

pub(crate) struct InboundPermit {
    budget: Arc<InboundBudget>,
    bytes: usize,
}

impl Drop for InboundPermit {
    fn drop(&mut self) {
        let mut retained = lock(&self.budget.retained);
        *retained = retained.saturating_sub(self.bytes);
    }
}

impl std::fmt::Debug for InboundPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InboundPermit")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

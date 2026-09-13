//! Shared request admission for native descendants. Permits cover inference only,
//! so parents release capacity before waiting on tools or nested children.
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Session-wide native descendant inference guardrails.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SubagentBudgetLimits {
    /// Maximum concurrent provider requests across descendants; zero means unlimited.
    pub max_in_flight: usize,
    /// Maximum provider dispatches; zero means unlimited.
    pub max_requests: u64,
    /// Maximum requested output per provider dispatch; zero disables this cap.
    pub max_output_tokens: u32,
}
/// Parent-visible aggregate admission and spend guardrail counters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SubagentBudgetSnapshot {
    /// Effective bounds.
    pub limits: SubagentBudgetLimits,
    /// Dispatches reserved, including failed/cancelled provider attempts.
    pub requests_reserved: u64,
    /// Current requests waiting on/streaming from providers.
    pub in_flight: usize,
}
pub(crate) struct SubagentBudget {
    limits: SubagentBudgetLimits,
    permits: Arc<tokio::sync::Semaphore>,
    used: Mutex<u64>,
    history: Mutex<Option<std::path::PathBuf>>,
}
impl SubagentBudget {
    pub fn new(limits: SubagentBudgetLimits) -> Self {
        let limits = SubagentBudgetLimits {
            max_in_flight: limits
                .max_in_flight
                .min(tokio::sync::Semaphore::MAX_PERMITS),
            max_requests: limits.max_requests,
            max_output_tokens: limits.max_output_tokens.min(131072),
        };
        Self {
            limits,
            permits: Arc::new(tokio::sync::Semaphore::new(if limits.max_in_flight == 0 {
                tokio::sync::Semaphore::MAX_PERMITS
            } else {
                limits.max_in_flight
            })),
            used: Mutex::new(0),
            history: Mutex::new(None),
        }
    }
    /// Fill an absent output request with a model-aware cap. Explicit requests keep ordinary
    /// adapter validation, and exceeding this session's guardrail is an error, never a rewrite.
    pub fn output_limit(
        &self,
        requested: Option<u64>,
        model_maximum: Option<u64>,
    ) -> anyhow::Result<Option<u64>> {
        if self.limits.max_output_tokens == 0 {
            return Ok(requested);
        }
        let cap = u64::from(self.limits.max_output_tokens);
        match requested {
            Some(requested) if requested > cap => {
                anyhow::bail!("requested output exceeds aggregate subagent output guardrail")
            }
            Some(requested) => Ok(Some(requested)),
            None => Ok(Some(cap.min(model_maximum.unwrap_or(cap)))),
        }
    }

    pub fn snapshot(&self) -> SubagentBudgetSnapshot {
        SubagentBudgetSnapshot {
            limits: self.limits,
            requests_reserved: *self.used.lock().unwrap_or_else(|e| e.into_inner()),
            in_flight: (if self.limits.max_in_flight == 0 {
                tokio::sync::Semaphore::MAX_PERMITS
            } else {
                self.limits.max_in_flight
            })
            .saturating_sub(self.permits.available_permits()),
        }
    }
    pub fn attach_history(&self, path: std::path::PathBuf) -> std::io::Result<()> {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        if history.is_some() {
            return Err(std::io::Error::other("budget already attached"));
        }
        if path.exists() {
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.len() > 4096 {
                return Err(std::io::Error::other("unsafe budget file"));
            }
            *self.used.lock().unwrap_or_else(|e| e.into_inner()) =
                serde_json::from_slice(&std::fs::read(&path)?)?;
        }
        *history = Some(path);
        Ok(())
    }
    pub async fn acquire(
        &self,
        turn: &CancellationToken,
        caller: &CancellationToken,
    ) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
        let permit = tokio::select! {
            biased;
            () = turn.cancelled() => anyhow::bail!("subagent request cancelled while queued"),
            () = caller.cancelled() => anyhow::bail!("subagent request cancelled while queued"),
            permit = self.permits.clone().acquire_owned() => permit?,
        };
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        if self.limits.max_requests != 0 && *used >= self.limits.max_requests {
            anyhow::bail!("aggregate subagent provider-request budget exhausted");
        }
        *used = used.saturating_add(1);
        if let Some(path) = self
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            use std::io::Write;
            let tmp = path.with_extension("tmp");
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&serde_json::to_vec(&*used)?)?;
            file.sync_all()?;
            std::fs::rename(tmp, path)?;
        }
        Ok(permit)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn automatic_output_cap_honors_model_metadata_without_rewriting_explicit_requests() {
        let budget = SubagentBudget::new(SubagentBudgetLimits {
            max_output_tokens: 8192,
            ..Default::default()
        });
        assert_eq!(budget.output_limit(None, Some(4096)).unwrap(), Some(4096));
        assert_eq!(budget.output_limit(None, Some(16384)).unwrap(), Some(8192));
        assert_eq!(budget.output_limit(None, None).unwrap(), Some(8192));
        assert_eq!(
            budget.output_limit(Some(1024), Some(4096)).unwrap(),
            Some(1024)
        );
        assert_eq!(
            budget.output_limit(Some(8192), Some(4096)).unwrap(),
            Some(8192),
            "explicit oversize requests must reach ordinary model validation"
        );
        assert!(budget.output_limit(Some(16384), Some(32768)).is_err());
    }

    #[tokio::test]
    async fn queued_cancellation_does_not_spend_and_restart_keeps_reserved_budget() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("budget.json");
        let limits = SubagentBudgetLimits {
            max_in_flight: 1,
            max_requests: 2,
            max_output_tokens: 123,
        };
        let budget = Arc::new(SubagentBudget::new(limits));
        budget.attach_history(path.clone()).unwrap();
        let live = CancellationToken::new();
        let permit = budget.acquire(&live, &live).await.unwrap();
        let queued = CancellationToken::new();
        queued.cancel();
        assert!(budget.acquire(&live, &queued).await.is_err());
        assert_eq!(budget.snapshot().requests_reserved, 1);
        assert_eq!(budget.snapshot().in_flight, 1);
        drop(permit);
        let recovered = SubagentBudget::new(limits);
        recovered.attach_history(path).unwrap();
        assert_eq!(recovered.snapshot().requests_reserved, 1);
        assert_eq!(recovered.snapshot().in_flight, 0);
        drop(recovered.acquire(&live, &live).await.unwrap());
        assert!(
            recovered
                .acquire(&live, &live)
                .await
                .unwrap_err()
                .to_string()
                .contains("budget exhausted")
        );
        assert_eq!(recovered.snapshot().requests_reserved, 2);
        assert_eq!(recovered.snapshot().in_flight, 0);
    }
}

#[cfg(test)]
mod unlimited_tests {
    use super::*;
    #[tokio::test]
    async fn default_admission_does_not_cap_requests_or_output() -> anyhow::Result<()> {
        let budget = SubagentBudget::new(SubagentBudgetLimits::default());
        *budget
            .used
            .lock()
            .map_err(|_| anyhow::anyhow!("poisoned"))? = 1000;
        let token = CancellationToken::new();
        let permit = budget.acquire(&token, &token).await?;
        assert_eq!(budget.snapshot().requests_reserved, 1001);
        assert_eq!(budget.output_limit(None, Some(131072))?, None);
        assert_eq!(budget.output_limit(Some(64000), Some(131072))?, Some(64000));
        drop(permit);
        Ok(())
    }
}

#[cfg(test)]
mod parallel_parity_tests {
    use super::*;
    #[tokio::test]
    async fn default_allows_more_than_the_previous_concurrency_ceiling() -> anyhow::Result<()> {
        let budget = SubagentBudget::new(SubagentBudgetLimits::default());
        let cancellation = CancellationToken::new();
        let permits = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut permits = Vec::new();
            for _ in 0..300 {
                permits.push(budget.acquire(&cancellation, &cancellation).await?);
            }
            anyhow::Ok(permits)
        })
        .await??;
        assert_eq!(budget.snapshot().limits.max_in_flight, 0);
        assert_eq!(budget.snapshot().in_flight, 300);
        drop(permits);
        assert_eq!(budget.snapshot().in_flight, 0);
        Ok(())
    }
}

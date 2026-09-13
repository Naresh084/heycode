//! Owned, bounded `/agents` readiness probes. Static capabilities stay separate.

use heycode_agent::subagent_provider::SubagentReadiness;
use heycode_agent::{SubagentErrorCode, SubagentProviderId, SubagentRegistry};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_PROVIDERS: usize = 32;
const MAX_IN_FLIGHT: usize = 4;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct AgentReadinessProbes {
    registry: Arc<SubagentRegistry>,
    queue: VecDeque<SubagentProviderId>,
    tasks: tokio::task::JoinSet<(String, &'static str)>,
    cancellation: CancellationToken,
    states: BTreeMap<String, &'static str>,
}

impl AgentReadinessProbes {
    pub(crate) fn new(registry: Arc<SubagentRegistry>) -> Self {
        let mut queue = VecDeque::new();
        let mut states = BTreeMap::new();
        for (index, descriptor) in registry.descriptors().into_iter().enumerate() {
            let state = if index < MAX_PROVIDERS {
                queue.push_back(descriptor.id().clone());
                "Unknown (checking)"
            } else {
                "Unknown (probe limit)"
            };
            states.insert(descriptor.id().as_str().into(), state);
        }
        let mut probes = Self {
            registry,
            queue,
            tasks: tokio::task::JoinSet::new(),
            cancellation: CancellationToken::new(),
            states,
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            probes.fill();
        } else {
            probes.queue.clear();
            probes
                .states
                .values_mut()
                .for_each(|state| *state = "Unknown (probe runtime unavailable)");
        }
        probes
    }

    pub(crate) fn active(&self) -> bool {
        !self.queue.is_empty() || !self.tasks.is_empty()
    }

    pub(crate) fn states(&self) -> &BTreeMap<String, &'static str> {
        &self.states
    }

    fn fill(&mut self) {
        while self.tasks.len() < MAX_IN_FLIGHT {
            let Some(id) = self.queue.pop_front() else {
                break;
            };
            let registry = self.registry.clone();
            let cancellation = self.cancellation.child_token();
            self.tasks.spawn(async move {
                use futures::FutureExt;
                let future = std::panic::AssertUnwindSafe(registry.provider_readiness(&id, cancellation.clone())).catch_unwind();
                let state = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => "Unknown (cancelled)",
                    result = tokio::time::timeout(PROBE_TIMEOUT, future) => match result {
                        Ok(Ok(Ok(SubagentReadiness::Ready))) => "Ready",
                        Ok(Ok(Ok(SubagentReadiness::NeedsAuthentication))) => "NeedsAuthentication",
                        Ok(Ok(Ok(SubagentReadiness::Unavailable))) => "Unavailable",
                        Ok(Ok(Ok(SubagentReadiness::Unknown))) => "Unknown (provider cannot prove readiness)",
                        Ok(Ok(Err(error))) if error.code() == SubagentErrorCode::Cancelled => "Unknown (cancelled)",
                        Ok(Ok(Err(_))) => "Unknown (probe failed)",
                        Ok(Err(_)) => "Unknown (probe stopped)",
                        Err(_) => "Unknown (timed out)",
                    },
                };
                cancellation.cancel();
                (id.as_str().to_owned(), state)
            });
        }
    }

    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(result) = self.tasks.try_join_next() {
            if let Ok((id, state)) = result {
                self.states.insert(id, state);
                changed = true;
            }
        }
        if !self.cancellation.is_cancelled() {
            self.fill();
        }
        changed
    }

    pub(crate) fn cancel(&mut self) {
        self.cancellation.cancel();
        self.tasks.abort_all();
        self.queue.clear();
        self.states
            .values_mut()
            .filter(|state| **state == "Unknown (checking)")
            .for_each(|state| *state = "Unknown (cancelled)");
    }
}

impl Drop for AgentReadinessProbes {
    fn drop(&mut self) {
        self.cancel();
    }
}

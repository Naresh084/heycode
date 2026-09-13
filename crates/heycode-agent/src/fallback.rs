//! Explicit route fallback after a definitive failure before any output.
use heycode_llm::{LlmError, LlmSelection, ProviderErrorClass, ProviderFailureOrigin};
use std::sync::{Arc, RwLock};
use tokio_util::sync::CancellationToken;

/// Routing-owned permission to select a previously configured fallback.
/// The implementation must validate the current route and persist the selection
/// before publishing it. It must not resolve an opaque-state barrier implicitly.
pub trait RequestFallback: Send + Sync {
    /// Select the configured route, returning true only after durable publication.
    /// # Errors
    /// Stale ownership, unavailable credentials/model or persistence failures.
    fn apply(&self, from: &LlmSelection, cancellation: &CancellationToken) -> anyhow::Result<bool>;
}
#[derive(Clone, Default)]
pub(crate) struct FallbackSlot(Arc<RwLock<Option<Arc<dyn RequestFallback>>>>);
impl FallbackSlot {
    pub(crate) fn install(
        &self,
        handler: Arc<dyn RequestFallback>,
    ) -> anyhow::Result<FallbackRegistration> {
        let mut slot = self
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("fallback owner unavailable"))?;
        anyhow::ensure!(slot.is_none(), "a fallback owner is already installed");
        *slot = Some(handler);
        Ok(FallbackRegistration(self.0.clone()))
    }
    pub(crate) fn get(&self) -> Option<Arc<dyn RequestFallback>> {
        self.0.read().ok().and_then(|slot| slot.clone())
    }
}
/// Effect-owned registration. Dropping it removes future fallback authority.
pub struct FallbackRegistration(Arc<RwLock<Option<Arc<dyn RequestFallback>>>>);
impl Drop for FallbackRegistration {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0.write() {
            *slot = None;
        }
    }
}

pub(crate) fn may_fallback(error: &LlmError, output_seen: bool, tools_dispatched: bool) -> bool {
    let Some(failure) = error.provider_failure() else {
        return false;
    };
    !output_seen
        && !tools_dispatched
        && failure.retry_advice() != Some(false)
        && matches!(
            failure.class(),
            ProviderErrorClass::RateLimited
                | ProviderErrorClass::Overloaded
                | ProviderErrorClass::Server
        )
        && matches!(
            failure.origin(),
            ProviderFailureOrigin::Http | ProviderFailureOrigin::ProviderEvent
        )
}
impl crate::Agent {
    /// Install the single explicit routing fallback owner for this agent.
    /// # Errors
    /// Duplicate owners or unavailable state are refused.
    pub fn install_request_fallback(
        &self,
        handler: Arc<dyn RequestFallback>,
    ) -> anyhow::Result<FallbackRegistration> {
        self.fallback.install(handler)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_llm::ProviderFailure;
    #[test]
    fn ambiguous_failures_partial_output_and_tool_effects_never_fallback() {
        let error = LlmError::Provider(ProviderFailure::new(
            ProviderErrorClass::RateLimited,
            ProviderFailureOrigin::Http,
        ));
        assert!(may_fallback(&error, false, false));
        assert!(!may_fallback(&error, true, false));
        assert!(!may_fallback(&error, false, true));
        for class in [
            ProviderErrorClass::Network,
            ProviderErrorClass::Timeout,
            ProviderErrorClass::Authentication,
            ProviderErrorClass::Protocol,
            ProviderErrorClass::InvalidRequest,
        ] {
            assert!(!may_fallback(
                &LlmError::Provider(ProviderFailure::new(class, ProviderFailureOrigin::Http)),
                false,
                false
            ));
        }
        assert!(!may_fallback(
            &LlmError::Provider(ProviderFailure::new(
                ProviderErrorClass::Server,
                ProviderFailureOrigin::Transport
            )),
            false,
            false
        ));
        assert!(!may_fallback(
            &LlmError::Provider(
                ProviderFailure::new(ProviderErrorClass::RateLimited, ProviderFailureOrigin::Http)
                    .with_retry_advice(false)
            ),
            false,
            false
        ));
    }
}

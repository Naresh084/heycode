//! Explicit, effect-owned activation of an independently configured provider.

use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{LlmError, Provider, ProviderRegistration, ProviderRegistry};

/// Host-owned construction boundary. Call only after an explicit user route
/// choice, never to discover providers or recover an inference error silently.
#[async_trait]
pub trait ProviderActivator: Send + Sync {
    /// Prove the target model/credentials and build its exact provider route.
    /// Construction must not reuse the currently selected route's credentials,
    /// endpoint or request options. It must not mutate live routing settings.
    async fn build(
        &self,
        provider: &str,
        model: &str,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn Provider>, LlmError>;
}

#[derive(Default)]
pub(crate) struct ActivationSlot(Arc<Mutex<Option<ActivationOwner>>>);

struct ActivationOwner {
    factory: Arc<dyn ProviderActivator>,
    token: Arc<()>,
    registrations: Vec<ProviderRegistration>,
}

/// Dropping the registration revokes new activation and removes exactly the
/// providers it published. In-flight construction cannot publish after revocation.
pub struct ProviderActivatorRegistration {
    slot: Weak<Mutex<Option<ActivationOwner>>>,
    token: Arc<()>,
}

impl ProviderRegistry {
    /// Install one host activator without constructing or resolving any provider.
    ///
    /// # Errors
    /// Duplicate ownership or an unavailable registry fails before publication.
    pub fn install_activator(
        &self,
        factory: Arc<dyn ProviderActivator>,
    ) -> Result<ProviderActivatorRegistration, LlmError> {
        let mut slot = self.activation.0.lock().map_err(|_| unavailable())?;
        if slot.is_some() {
            return Err(LlmError::Transport(
                "provider activator already installed".into(),
            ));
        }
        let token = Arc::new(());
        *slot = Some(ActivationOwner {
            factory,
            token: token.clone(),
            registrations: Vec::new(),
        });
        Ok(ProviderActivatorRegistration {
            slot: Arc::downgrade(&self.activation.0),
            token,
        })
    }

    /// Activate one explicitly selected route, leaving any existing provider
    /// intact. Model selection/route persistence remains the routing owner's job.
    ///
    /// # Errors
    /// Invalid target, missing factory, failed target admission, cancellation,
    /// stale activation ownership or a concurrent registration fails safely.
    pub async fn activate(
        &self,
        provider: &str,
        model: &str,
        cancellation: CancellationToken,
    ) -> Result<(), LlmError> {
        let safe = |value: &str| {
            !value.is_empty()
                && value.len() <= 256
                && value.trim() == value
                && !value.chars().any(char::is_control)
        };
        if !safe(provider) || !safe(model) {
            return Err(LlmError::Transport(
                "invalid provider activation target".into(),
            ));
        }
        check_cancelled(&cancellation)?;
        if self.get(provider).is_some() {
            return Ok(());
        }
        let (factory, token) = {
            let slot = self.activation.0.lock().map_err(|_| unavailable())?;
            let owner = slot.as_ref().ok_or_else(unavailable)?;
            (owner.factory.clone(), owner.token.clone())
        };
        let pending = factory.build(provider, model, cancellation.clone());
        let built = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled()),
            result = pending => result?,
        };
        check_cancelled(&cancellation)?;
        if built.info().name != provider || built.descriptor().id != provider {
            return Err(LlmError::Transport(
                "activated provider identity differs from target".into(),
            ));
        }
        // The owner lock spans registration, so disposal cannot race publication.
        let mut slot = self.activation.0.lock().map_err(|_| unavailable())?;
        let owner = slot
            .as_mut()
            .filter(|owner| Arc::ptr_eq(&owner.token, &token))
            .ok_or_else(unavailable)?;
        check_cancelled(&cancellation)?;
        if self.get(provider).is_some() {
            return Err(LlmError::Transport(
                "provider activation lost registration ownership".into(),
            ));
        }
        owner
            .registrations
            .push(self.register_owned(built).map_err(LlmError::Transport)?);
        Ok(())
    }
}

impl Drop for ProviderActivatorRegistration {
    fn drop(&mut self) {
        let Some(slot) = self.slot.upgrade() else {
            return;
        };
        let Ok(mut slot) = slot.lock() else { return };
        if slot
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(&owner.token, &self.token))
        {
            *slot = None;
        }
    }
}

fn unavailable() -> LlmError {
    LlmError::Transport("provider activation owner is unavailable".into())
}
fn cancelled() -> LlmError {
    LlmError::Provider(crate::ProviderFailure::new(
        crate::ProviderErrorClass::Cancelled,
        crate::ProviderFailureOrigin::Local,
    ))
}
fn check_cancelled(cancellation: &CancellationToken) -> Result<(), LlmError> {
    if cancellation.is_cancelled() {
        Err(cancelled())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testing::FakeProvider;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Factory {
        calls: AtomicUsize,
        entered: tokio::sync::Notify,
        release: Option<Arc<tokio::sync::Notify>>,
        wrong_identity: bool,
    }
    impl Factory {
        fn immediate() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                entered: tokio::sync::Notify::new(),
                release: None,
                wrong_identity: false,
            })
        }
    }
    #[async_trait]
    impl ProviderActivator for Factory {
        async fn build(
            &self,
            _: &str,
            _: &str,
            _: CancellationToken,
        ) -> Result<Arc<dyn Provider>, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            if let Some(release) = &self.release {
                release.notified().await;
            }
            Ok(Arc::new(FakeProvider::named(
                if self.wrong_identity {
                    "foreign"
                } else {
                    "target"
                },
                "model",
                vec![],
            )))
        }
    }
    #[tokio::test]
    async fn activation_is_explicit_idempotent_and_exactly_effect_owned() {
        let registry = ProviderRegistry::new();
        registry
            .register(Arc::new(FakeProvider::named("source", "model", vec![])))
            .unwrap();
        let factory = Factory::immediate();
        let owner = registry.install_activator(factory.clone()).unwrap();
        assert_eq!(factory.calls.load(Ordering::SeqCst), 0);
        assert!(registry.install_activator(factory.clone()).is_err());
        registry
            .activate("target", "model", CancellationToken::new())
            .await
            .unwrap();
        registry
            .activate("target", "other-model", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(factory.calls.load(Ordering::SeqCst), 1);
        assert_eq!(registry.names(), ["source", "target"]);
        drop(owner);
        assert_eq!(registry.names(), ["source"]);
        assert!(
            registry
                .activate("target", "model", CancellationToken::new())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn teardown_during_construction_revokes_publication() {
        let registry = Arc::new(ProviderRegistry::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let factory = Arc::new(Factory {
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: Some(release.clone()),
            wrong_identity: false,
        });
        let owner = registry.install_activator(factory.clone()).unwrap();
        let pending = {
            let registry = registry.clone();
            tokio::spawn(async move {
                registry
                    .activate("target", "model", CancellationToken::new())
                    .await
            })
        };
        factory.entered.notified().await;
        drop(owner);
        release.notify_one();
        assert!(pending.await.unwrap().is_err());
        assert!(registry.names().is_empty());
    }

    #[tokio::test]
    async fn concurrent_registration_keeps_the_other_owners_provider() {
        let registry = Arc::new(ProviderRegistry::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let factory = Arc::new(Factory {
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: Some(release.clone()),
            wrong_identity: false,
        });
        let owner = registry.install_activator(factory.clone()).unwrap();
        let pending = {
            let registry = registry.clone();
            tokio::spawn(async move {
                registry
                    .activate("target", "model", CancellationToken::new())
                    .await
            })
        };
        factory.entered.notified().await;
        let other = registry
            .register_owned(Arc::new(FakeProvider::named(
                "target",
                "other-model",
                vec![],
            )))
            .unwrap();
        release.notify_one();
        assert!(pending.await.unwrap().is_err());
        drop(owner);
        assert_eq!(
            registry.get("target").unwrap().info().default_model,
            "other-model"
        );
        drop(other);
        assert!(registry.names().is_empty());
    }
    #[tokio::test]
    async fn cancellation_and_foreign_identity_never_publish() {
        let registry = Arc::new(ProviderRegistry::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let factory = Arc::new(Factory {
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: Some(release),
            wrong_identity: false,
        });
        let owner = registry.install_activator(factory.clone()).unwrap();
        let cancellation = CancellationToken::new();
        let pending = {
            let registry = registry.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move { registry.activate("target", "model", cancellation).await })
        };
        factory.entered.notified().await;
        cancellation.cancel();
        assert_eq!(
            pending.await.unwrap().unwrap_err().class(),
            crate::ProviderErrorClass::Cancelled
        );
        assert!(registry.names().is_empty());
        drop(owner);
        let _owner = registry
            .install_activator(Arc::new(Factory {
                calls: AtomicUsize::new(0),
                entered: tokio::sync::Notify::new(),
                release: None,
                wrong_identity: true,
            }))
            .unwrap();
        assert!(
            registry
                .activate("target", "model", CancellationToken::new())
                .await
                .is_err()
        );
        assert!(registry.names().is_empty());
    }
}

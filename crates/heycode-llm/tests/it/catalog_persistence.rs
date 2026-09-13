//! Catalog persistence registration, restore and commit-order contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::Context;
use heycode_llm::{
    CatalogError, CatalogFetchError, CatalogFreshness, CatalogPersistence, CatalogPersistenceError,
    CatalogRefreshMode, CatalogRegistry, CatalogSnapshot, ModelCatalog, ModelDescriptor,
    ModelLifecycle, ProviderDescriptor, ProviderProtocol,
};
use tokio_util::sync::CancellationToken;

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "provider".to_owned(),
        display_name: "Provider".to_owned(),
        protocols: vec![ProviderProtocol::Unknown],
    }
}

fn model(id: &str) -> ModelDescriptor {
    let mut model = ModelDescriptor::unknown(id);
    model.lifecycle = ModelLifecycle::stable();
    model
}

struct CountingCatalog {
    calls: AtomicUsize,
}

impl CountingCatalog {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelCatalog for CountingCatalog {
    fn provider(&self) -> ProviderDescriptor {
        provider()
    }

    async fn fetch(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![model("provider/model")])
    }
}

#[derive(Default)]
struct MemoryPersistence {
    generations: Mutex<Vec<CatalogSnapshot>>,
    saves: AtomicUsize,
    fail_save: AtomicBool,
}

impl CatalogPersistence for MemoryPersistence {
    fn load(&self) -> Result<Vec<CatalogSnapshot>, CatalogPersistenceError> {
        Ok(self.generations.lock().unwrap().clone())
    }

    fn save(
        &self,
        generations: &[Arc<CatalogSnapshot>],
        cancellation: &CancellationToken,
    ) -> Result<(), CatalogPersistenceError> {
        if cancellation.is_cancelled() {
            return Err(CatalogPersistenceError::new("save cancelled"));
        }
        if self.fail_save.load(Ordering::SeqCst) {
            return Err(CatalogPersistenceError::new("durable store unavailable"));
        }
        self.saves.fetch_add(1, Ordering::SeqCst);
        *self.generations.lock().unwrap() = generations
            .iter()
            .map(|snapshot| snapshot.as_ref().clone())
            .collect();
        Ok(())
    }
}

#[tokio::test]
async fn durable_generation_restores_fresh_without_a_provider_call() {
    let persistence = Arc::new(MemoryPersistence::default());
    let mut first_context = Context::new();
    let first = CatalogRegistry::new(Duration::from_secs(300));
    first
        .register_persistence(&first_context, persistence.clone())
        .unwrap();
    let first_source = Arc::new(CountingCatalog::new());
    first
        .register(&first_context, first_source.clone())
        .unwrap();
    let committed = first
        .refresh(
            "provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(first_source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(persistence.saves.load(Ordering::SeqCst), 1);
    assert_eq!(committed.snapshot.revision, 1);
    assert!(committed.snapshot.fetched_at_ms > 0);
    first_context.shutdown();

    let second_context = Context::new();
    let second = CatalogRegistry::new(Duration::from_secs(300));
    second
        .register_persistence(&second_context, persistence.clone())
        .unwrap();
    let second_source = Arc::new(CountingCatalog::new());
    second
        .register(&second_context, second_source.clone())
        .unwrap();
    let restored = second
        .refresh(
            "provider",
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(restored.freshness, CatalogFreshness::FreshCache);
    assert_eq!(restored.snapshot.revision, 1);
    assert_eq!(restored.snapshot.models[0].id, "provider/model");
    assert_eq!(second_source.calls.load(Ordering::SeqCst), 0);

    let refreshed = second
        .refresh(
            "provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(refreshed.snapshot.revision, 2);
    assert_eq!(second_source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(persistence.saves.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persistence_failure_does_not_publish_the_candidate_generation() {
    let persistence = Arc::new(MemoryPersistence::default());
    persistence.fail_save.store(true, Ordering::SeqCst);
    let context = Context::new();
    let registry = CatalogRegistry::new(Duration::from_secs(300));
    registry
        .register_persistence(&context, persistence)
        .unwrap();
    registry
        .register(&context, Arc::new(CountingCatalog::new()))
        .unwrap();

    let error = registry
        .refresh(
            "provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CatalogError::Persistence { message }
            if message == "durable store unavailable"
    ));
    assert!(matches!(
        registry.cached("provider"),
        Err(CatalogError::NoCachedCatalog { .. })
    ));
}

#[test]
fn persistence_registration_is_unique_and_effect_owned() {
    let persistence = Arc::new(MemoryPersistence::default());
    let mut context = Context::new();
    let registry = CatalogRegistry::new(Duration::from_secs(300));
    registry
        .register_persistence(&context, persistence.clone())
        .unwrap();
    assert!(registry.has_persistence().unwrap());
    assert!(matches!(
        registry.register_persistence(&context, persistence),
        Err(CatalogError::DuplicatePersistence)
    ));
    context.shutdown();
    assert!(!registry.has_persistence().unwrap());
}

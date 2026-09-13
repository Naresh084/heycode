//! Model-catalog registry, cache, refresh and lifecycle contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::Context;
use heycode_llm::{
    CatalogError, CatalogFailureKind, CatalogFetchError, CatalogFreshness, CatalogRefreshMode,
    CatalogRegistry, ModelCatalog, ModelDescriptor, ProviderDescriptor, ProviderProtocol,
    SERVICE_MODELS, model_catalog_plugin,
};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "test-provider".to_owned(),
        display_name: "Test Provider".to_owned(),
        protocols: vec![ProviderProtocol::Unknown],
    }
}

fn model(id: &str) -> ModelDescriptor {
    ModelDescriptor::unknown(id)
}

#[derive(Clone)]
enum ScriptedResult {
    Models(Vec<ModelDescriptor>),
    Failure(CatalogFetchError),
}

struct ScriptedCatalog {
    provider: ProviderDescriptor,
    results: Mutex<VecDeque<ScriptedResult>>,
    calls: AtomicUsize,
}

impl ScriptedCatalog {
    fn new(results: impl IntoIterator<Item = ScriptedResult>) -> Self {
        Self {
            provider: provider(),
            results: Mutex::new(results.into_iter().collect()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelCatalog for ScriptedCatalog {
    fn provider(&self) -> ProviderDescriptor {
        self.provider.clone()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.results.lock().unwrap().pop_front().unwrap() {
            ScriptedResult::Models(models) => Ok(models),
            ScriptedResult::Failure(error) => Err(error),
        }
    }
}

struct GatedCatalog {
    provider: ProviderDescriptor,
    calls: AtomicUsize,
    started: Arc<Notify>,
    permits: Arc<Semaphore>,
}

impl GatedCatalog {
    fn new() -> Self {
        Self {
            provider: provider(),
            calls: AtomicUsize::new(0),
            started: Arc::new(Notify::new()),
            permits: Arc::new(Semaphore::new(0)),
        }
    }

    fn release(&self) {
        self.permits.add_permits(8);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelCatalog for GatedCatalog {
    fn provider(&self) -> ProviderDescriptor {
        self.provider.clone()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        tokio::select! {
            _ = cancellation.cancelled() => Err(CatalogFetchError::cancelled()),
            permit = self.permits.acquire() => {
                let _permit = permit.map_err(|_| CatalogFetchError::new(
                    CatalogFailureKind::Unavailable,
                    "test catalog gate closed",
                ))?;
                Ok(vec![model("test-provider/model")])
            }
        }
    }
}

fn registered_registry(
    ttl: Duration,
    catalog: Arc<dyn ModelCatalog>,
) -> (Context, CatalogRegistry) {
    let context = Context::new();
    let registry = CatalogRegistry::new(ttl);
    registry.register(&context, catalog).unwrap();
    (context, registry)
}

#[tokio::test]
async fn concurrent_forced_refreshes_share_exactly_one_provider_call() {
    let catalog = Arc::new(GatedCatalog::new());
    let (_context, registry) = registered_registry(Duration::from_secs(300), catalog.clone());

    let first = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .refresh(
                    "test-provider",
                    CatalogRefreshMode::Force,
                    CancellationToken::new(),
                )
                .await
        }
    });
    catalog.started.notified().await;
    let second = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .refresh(
                    "test-provider",
                    CatalogRefreshMode::Force,
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    catalog.release();

    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    assert_eq!(catalog.calls(), 1);
    assert_eq!(first.snapshot.revision, 1);
    assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));
    assert_eq!(first.freshness, CatalogFreshness::Live);
    assert_eq!(second.freshness, CatalogFreshness::Live);
}

#[tokio::test]
async fn ttl_cache_avoids_fetch_until_forced() {
    let catalog = Arc::new(ScriptedCatalog::new([
        ScriptedResult::Models(vec![model("test-provider/one")]),
        ScriptedResult::Models(vec![model("test-provider/two")]),
    ]));
    let (_context, registry) = registered_registry(Duration::from_secs(300), catalog.clone());

    let first = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let cached = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let forced = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(catalog.calls(), 2);
    assert_eq!(first.freshness, CatalogFreshness::Live);
    assert_eq!(cached.freshness, CatalogFreshness::FreshCache);
    assert!(Arc::ptr_eq(&first.snapshot, &cached.snapshot));
    assert_eq!(forced.snapshot.revision, 2);
    assert_eq!(forced.snapshot.models[0].id, "test-provider/two");
}

#[tokio::test]
async fn failed_ordinary_refresh_returns_visible_stale_last_good_but_force_fails() {
    let failure = CatalogFetchError::new(CatalogFailureKind::Network, "catalog offline");
    let catalog = Arc::new(ScriptedCatalog::new([
        ScriptedResult::Models(vec![model("test-provider/last-good")]),
        ScriptedResult::Failure(failure.clone()),
        ScriptedResult::Failure(failure),
    ]));
    let (_context, registry) = registered_registry(Duration::ZERO, catalog.clone());

    let live = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let stale = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let forced_error = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert_eq!(live.snapshot.revision, 1);
    assert_eq!(stale.freshness, CatalogFreshness::StaleFallback);
    assert!(Arc::ptr_eq(&live.snapshot, &stale.snapshot));
    assert!(matches!(
        stale.warning,
        Some(CatalogError::Refresh {
            kind: CatalogFailureKind::Network,
            ..
        })
    ));
    assert!(matches!(
        forced_error,
        CatalogError::Refresh {
            kind: CatalogFailureKind::Network,
            ..
        }
    ));
    assert_eq!(registry.cached("test-provider").unwrap().revision, 1);
    assert_eq!(catalog.calls(), 3);
}

#[tokio::test]
async fn cancelling_one_waiter_does_not_abort_the_shared_refresh_or_cache_fill() {
    let catalog = Arc::new(GatedCatalog::new());
    let (_context, registry) = registered_registry(Duration::from_secs(300), catalog.clone());
    let cancellation = CancellationToken::new();
    let waiter = tokio::spawn({
        let registry = registry.clone();
        let cancellation = cancellation.clone();
        async move {
            registry
                .refresh("test-provider", CatalogRefreshMode::Force, cancellation)
                .await
        }
    });
    catalog.started.notified().await;
    cancellation.cancel();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(CatalogError::Cancelled { provider }) if provider == "test-provider"
    ));

    catalog.release();
    let next = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(next.snapshot.models[0].id, "test-provider/model");
    assert_eq!(catalog.calls(), 1);
}

#[tokio::test]
async fn invalid_generation_never_replaces_last_good_and_is_visible_as_stale() {
    let catalog = Arc::new(ScriptedCatalog::new([
        ScriptedResult::Models(vec![model("test-provider/last-good")]),
        ScriptedResult::Models(vec![
            model("test-provider/duplicate"),
            model("test-provider/duplicate"),
        ]),
    ]));
    let (_context, registry) = registered_registry(Duration::ZERO, catalog);
    let live = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let stale = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(stale.freshness, CatalogFreshness::StaleFallback);
    assert!(Arc::ptr_eq(&live.snapshot, &stale.snapshot));
    assert!(matches!(
        stale.warning,
        Some(CatalogError::InvalidCatalog { provider, message })
            if provider == "test-provider" && message.contains("duplicate model id")
    ));
    assert_eq!(registry.cached("test-provider").unwrap().revision, 1);
}

#[tokio::test]
async fn context_shutdown_cancels_active_source_and_removes_its_catalog() {
    let catalog = Arc::new(GatedCatalog::new());
    let (mut context, registry) = registered_registry(Duration::from_secs(300), catalog.clone());
    let waiter = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .refresh(
                    "test-provider",
                    CatalogRefreshMode::Force,
                    CancellationToken::new(),
                )
                .await
        }
    });
    catalog.started.notified().await;
    context.shutdown();

    assert!(matches!(
        waiter.await.unwrap(),
        Err(CatalogError::Cancelled { provider }) if provider == "test-provider"
    ));
    assert!(registry.descriptors().unwrap().is_empty());
    assert_eq!(catalog.calls(), 1);
}

#[test]
fn catalog_registration_is_unique_sorted_and_removed_by_context_shutdown() {
    let registry = CatalogRegistry::new(Duration::from_secs(300));
    let mut context = Context::new();
    let first: Arc<dyn ModelCatalog> = Arc::new(ScriptedCatalog::new([]));
    registry.register(&context, first).unwrap();
    assert_eq!(registry.descriptors().unwrap(), [provider()]);

    let duplicate: Arc<dyn ModelCatalog> = Arc::new(ScriptedCatalog::new([]));
    assert!(matches!(
        registry.register(&context, duplicate),
        Err(CatalogError::DuplicateCatalog { provider }) if provider == "test-provider"
    ));
    context.shutdown();
    assert!(registry.descriptors().unwrap().is_empty());
}

#[test]
fn models_plugin_publishes_the_catalog_registry_service() {
    let plugin = model_catalog_plugin(Duration::from_secs(300));
    let context = heycode_core::compose(std::slice::from_ref(&plugin)).unwrap();
    assert_eq!(context.owner_of(SERVICE_MODELS), Some("models"));
    assert!(context.get::<CatalogRegistry>(SERVICE_MODELS).is_some());
}

#[tokio::test]
async fn catalog_sorts_newest_first_with_stable_ties_and_unknown_dates_last() {
    let dated = |id: &str, date| {
        let mut row = model(id);
        row.created_at_ms = date;
        row
    };
    let source = Arc::new(ScriptedCatalog::new([ScriptedResult::Models(vec![
        dated("a-unknown", None),
        dated("b-old", Some(1000)),
        dated("z-new", Some(3000)),
        dated("a-new", Some(3000)),
        dated("z-unknown", None),
    ])]));
    let (_context, registry) = registered_registry(Duration::from_secs(300), source);
    let view = registry
        .refresh(
            "test-provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        view.snapshot
            .models
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["a-new", "z-new", "b-old", "a-unknown", "z-unknown"]
    );
}

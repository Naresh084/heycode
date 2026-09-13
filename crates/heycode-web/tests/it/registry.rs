//! WEB01 provider-independent search/fetch service contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_web::{
    SERVICE_WEB, WebFetchRequest, WebFetchResult, WebProvider, WebProviderDescriptor, WebRegistry,
    WebSearchRequest, WebSearchResult, web_registry_plugin,
};
use tokio_util::sync::CancellationToken;

struct FakeProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl WebProvider for FakeProvider {
    fn descriptor(&self) -> WebProviderDescriptor {
        WebProviderDescriptor::new("fake", true, true).unwrap()
    }

    async fn search(
        &self,
        request: WebSearchRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, heycode_web::WebError> {
        if cancellation.is_cancelled() {
            return Err(heycode_web::WebError::cancelled());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![WebSearchResult::new(
            "Result",
            "https://example.test/result",
            request.query(),
        )?])
    }

    async fn fetch(
        &self,
        request: WebFetchRequest,
        cancellation: CancellationToken,
    ) -> Result<WebFetchResult, heycode_web::WebError> {
        if cancellation.is_cancelled() {
            return Err(heycode_web::WebError::cancelled());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        WebFetchResult::new(
            request.url(),
            format!("fetched {}", request.url()),
            Some("text/plain"),
            false,
        )
    }
}

#[tokio::test]
async fn registry_dispatches_both_consumers_and_shutdown_is_terminal() {
    let plugins = vec![web_registry_plugin()];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    registry.register(&context, provider.clone()).unwrap();
    assert_eq!(registry.descriptors().unwrap()[0].id(), "fake");

    let results = registry
        .search(
            WebSearchRequest::new("rust", 8).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(results[0].snippet(), "rust");
    let fetched = registry
        .fetch(
            WebFetchRequest::new("https://example.test/page", 65_536).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(fetched.content().contains("example.test"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(registry.register(&context, provider).is_err());

    context.shutdown();
    assert!(registry.descriptors().unwrap().is_empty());
    assert!(
        registry
            .search(
                WebSearchRequest::new("rust", 8).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
}

#[test]
fn boundary_values_reject_unsafe_or_unbounded_shapes() {
    assert!(WebSearchRequest::new("", 8).is_err());
    assert!(WebSearchRequest::new("rust", 0).is_err());
    assert!(WebFetchRequest::new("file:///etc/passwd", 1_024).is_err());
    assert!(WebFetchRequest::new("https://user:pass@example.test", 1_024).is_err());
    assert!(WebSearchResult::new("", "https://example.test", "snippet").is_err());
    assert!(
        WebFetchResult::new(
            "https://example.test",
            "x",
            Some("bad\ncontent-type"),
            false,
        )
        .is_err()
    );
}

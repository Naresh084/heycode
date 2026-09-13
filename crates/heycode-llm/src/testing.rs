//! Test harnesses for downstream crates. Always compiled — scripted streams
//! are the sanctioned way to test consumers of [`crate::Provider`] without
//! network or mocks.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::provider::{ChunkStream, Provider, ProviderInfo};
use crate::vocab::{ChatRequest, FinishReason, StreamChunk};

mod conformance;

pub use conformance::{
    CATALOG_CONFORMANCE_FIXTURE_SCHEMA_VERSION, CatalogConformanceFixture, ConformanceFixtureError,
    ConformanceFixtureMetadata, ConformanceRun, ConformanceSourceKind, SseConformanceFixture,
    SseFixtureCase, SseFixtureTransport, run_sse_conformance,
};

/// Provider replaying scripted chunk sequences. Each call to
/// [`Provider::stream`] consumes the next script in the order given; once
/// scripts are exhausted every stream yields a bare `Finish(Stop)` so tests
/// never panic. Scripts are replayed verbatim — the usage/finish placement
/// contract is the script author's responsibility.
pub struct FakeProvider {
    scripts: Mutex<Vec<Vec<StreamChunk>>>,
    name: &'static str,
    model: String,
    /// When set, an exhausted script list replays this script on every later
    /// call instead of yielding a bare `Finish(Stop)`.
    repeat: Option<Vec<StreamChunk>>,
}

impl FakeProvider {
    /// A provider named `"fake"` serving model `"fake-model"`.
    #[must_use]
    pub fn new(scripts: Vec<Vec<StreamChunk>>) -> Self {
        Self::named("fake", "fake-model", scripts)
    }

    /// A provider with an explicit registry name and default model.
    #[must_use]
    pub fn named(
        name: &'static str,
        model: impl Into<String>,
        scripts: Vec<Vec<StreamChunk>>,
    ) -> Self {
        Self {
            scripts: Mutex::new(scripts),
            name,
            model: model.into(),
            repeat: None,
        }
    }

    /// A provider that serves `script` on every call, forever.
    ///
    /// The product's offline `--fake` mode uses this so a long interactive
    /// session gets a real reply on every turn rather than an empty stop after
    /// the first one.
    #[must_use]
    pub fn repeating(script: Vec<StreamChunk>) -> Self {
        Self {
            scripts: Mutex::new(Vec::new()),
            name: "fake",
            model: "fake-model".to_owned(),
            repeat: Some(script),
        }
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.to_owned(),
            default_model: self.model.clone(),
        }
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        // A poisoned lock means a test consumer panicked mid-stream; serving
        // the remaining scripts beats cascading failures.
        let mut scripts = match self.scripts.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let next = if scripts.is_empty() {
            None
        } else {
            Some(scripts.remove(0))
        };
        let chunks = next.unwrap_or_else(|| {
            self.repeat
                .clone()
                .unwrap_or_else(|| vec![StreamChunk::Finish(FinishReason::Stop)])
        });
        Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn request() -> ChatRequest {
        ChatRequest {
            model: "whatever".into(),
            messages: Vec::new(),
            tools: None,
            temperature: None,
            max_tokens: None,
        }
    }

    async fn drain(provider: &dyn Provider) -> Vec<StreamChunk> {
        let mut stream = provider.stream(request());
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn a_repeating_provider_replies_on_every_call() {
        let provider = FakeProvider::repeating(vec![
            StreamChunk::TextDelta("again".into()),
            StreamChunk::Finish(FinishReason::Stop),
        ]);
        for _ in 0..3 {
            let chunks = drain(&provider).await;
            assert_eq!(chunks[0], StreamChunk::TextDelta("again".into()));
            assert_eq!(chunks.len(), 2);
        }
    }

    #[tokio::test]
    async fn scripts_are_served_in_the_order_given() {
        let provider = FakeProvider::new(vec![
            vec![
                StreamChunk::TextDelta("first".into()),
                StreamChunk::Finish(FinishReason::Stop),
            ],
            vec![
                StreamChunk::TextDelta("second".into()),
                StreamChunk::Finish(FinishReason::Stop),
            ],
        ]);
        assert_eq!(
            drain(&provider).await[0],
            StreamChunk::TextDelta("first".into())
        );
        assert_eq!(
            drain(&provider).await[0],
            StreamChunk::TextDelta("second".into())
        );
    }

    #[tokio::test]
    async fn exhausted_provider_yields_bare_stop_finish() {
        let provider = FakeProvider::new(Vec::new());
        assert_eq!(
            drain(&provider).await,
            vec![StreamChunk::Finish(FinishReason::Stop)]
        );
    }

    #[tokio::test]
    async fn info_reports_name_and_model() {
        let provider = FakeProvider::named("scripted", "m-9", Vec::new());
        let info = provider.info();
        assert_eq!(info.name, "scripted");
        assert_eq!(info.default_model, "m-9");
    }
}

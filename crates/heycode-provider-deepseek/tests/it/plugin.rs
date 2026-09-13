//! DeepSeek catalog contribution composition and teardown.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialsService, SERVICE_CREDENTIALS,
};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SERVICE_HTTP, SseEventStream};
use heycode_llm::{
    CatalogError, CatalogFailureKind, CatalogRefreshMode, CatalogRegistry, SERVICE_MODELS,
};
use heycode_provider_deepseek::{DeepSeekCatalogConfig, deepseek_catalog_plugin};
use tokio_util::sync::CancellationToken;

struct EmptyTransport;

impl HttpTransport for EmptyTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

struct HttpPlugin;

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(
            SERVICE_HTTP,
            self.name(),
            HttpService::new(Arc::new(EmptyTransport)),
        )
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("DEEPSEEK_API_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

#[tokio::test]
async fn plugin_registers_catalog_as_an_effect_and_shutdown_removes_it() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(CredentialsPlugin),
        Box::new(HttpPlugin),
        deepseek_catalog_plugin(DeepSeekCatalogConfig::official(query())),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    assert!(matches!(
        models
            .refresh(
                "deepseek",
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await,
        Err(CatalogError::Refresh {
            kind: CatalogFailureKind::Unauthorized,
            ..
        })
    ));

    context.shutdown();
    assert!(matches!(
        models
            .refresh(
                "deepseek",
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await,
        Err(CatalogError::UnknownCatalog { .. })
    ));
}

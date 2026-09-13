//! Model lifecycle and configured-selection refusal contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::Context;
use heycode_llm::{
    CatalogFetchError, CatalogRefreshMode, CatalogRegistry, ModelCatalog, ModelDescriptor,
    ModelLifecycle, ModelLifecycleStatus, ModelSelectionError, ModelSelectionWarning,
    ProviderDescriptor, ProviderProtocol,
};
use tokio_util::sync::CancellationToken;

fn provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "provider".to_owned(),
        display_name: "Provider".to_owned(),
        protocols: vec![ProviderProtocol::Unknown],
    }
}

fn model(id: &str, lifecycle: ModelLifecycle) -> ModelDescriptor {
    let mut descriptor = ModelDescriptor::unknown(id);
    descriptor.display_name = id.replace("provider/", "");
    descriptor.lifecycle = lifecycle;
    descriptor
}

struct StaticCatalog(Vec<ModelDescriptor>);

#[async_trait]
impl ModelCatalog for StaticCatalog {
    fn provider(&self) -> ProviderDescriptor {
        provider()
    }

    async fn fetch(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        Ok(self.0.clone())
    }
}

async fn registry_with(models: Vec<ModelDescriptor>) -> (Context, CatalogRegistry) {
    let context = Context::new();
    let registry = CatalogRegistry::new(Duration::from_secs(300));
    registry
        .register(&context, Arc::new(StaticCatalog(models)))
        .unwrap();
    registry
        .refresh(
            "provider",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    (context, registry)
}

#[test]
fn unknown_descriptor_does_not_invent_lifecycle_evidence() {
    let descriptor = ModelDescriptor::unknown("provider/unknown");
    assert_eq!(descriptor.lifecycle, ModelLifecycle::unknown());
    assert_eq!(
        descriptor.lifecycle.effective_status(9_999),
        ModelLifecycleStatus::Unknown
    );
    assert!(descriptor.lifecycle.is_selectable(9_999));
}

#[tokio::test]
async fn configured_model_past_retirement_fails_with_deterministic_alternatives() {
    let mut old = model(
        "provider/old",
        ModelLifecycle::deprecated(
            Some(1_000),
            vec![
                "provider/new-preview".to_owned(),
                "provider/missing".to_owned(),
                "provider/new-stable".to_owned(),
            ],
        ),
    );
    old.aliases = vec!["provider/old-alias".to_owned()];
    let (_context, registry) = registry_with(vec![
        model("provider/alpha", ModelLifecycle::stable()),
        old,
        model("provider/new-preview", ModelLifecycle::preview()),
        model("provider/new-stable", ModelLifecycle::stable()),
        model(
            "provider/also-retired",
            ModelLifecycle::retired(None, Vec::new()),
        ),
    ])
    .await;

    let error = registry
        .resolve_model("provider", "provider/old-alias", 2_000)
        .unwrap_err();
    assert!(matches!(
        error,
        ModelSelectionError::RetiredModel {
            provider,
            model,
            retirement_at_ms: Some(1_000),
            alternatives,
        } if provider == "provider"
            && model == "provider/old-alias"
            && alternatives == [
                "provider/new-preview",
                "provider/new-stable",
                "provider/alpha",
            ]
    ));
}

#[tokio::test]
async fn deprecated_model_before_retirement_resolves_with_visible_warning() {
    let (_context, registry) = registry_with(vec![
        model(
            "provider/old",
            ModelLifecycle::deprecated(Some(10_000), vec!["provider/new".to_owned()]),
        ),
        model("provider/new", ModelLifecycle::stable()),
    ])
    .await;

    let resolved = registry
        .resolve_model("provider", "provider/old", 2_000)
        .unwrap();
    assert_eq!(resolved.descriptor.id, "provider/old");
    assert_eq!(
        resolved.warning,
        Some(ModelSelectionWarning::Deprecated {
            retirement_at_ms: Some(10_000),
            alternatives: vec!["provider/new".to_owned()],
        })
    );
}

#[tokio::test]
async fn configured_id_absent_from_catalog_fails_instead_of_becoming_unknown_model() {
    let (_context, registry) = registry_with(vec![
        model("provider/beta", ModelLifecycle::preview()),
        model("provider/alpha", ModelLifecycle::stable()),
    ])
    .await;

    let error = registry
        .resolve_model("provider", "provider/missing", 2_000)
        .unwrap_err();
    assert!(matches!(
        error,
        ModelSelectionError::UnknownModel {
            provider,
            model,
            alternatives,
        } if provider == "provider"
            && model == "provider/missing"
            && alternatives == ["provider/alpha", "provider/beta"]
    ));
}

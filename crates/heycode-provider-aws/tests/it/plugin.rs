//! PAWS03 catalog contribution: composition, region resolution and effect
//! ownership.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::time::Duration;

use heycode_authorization_aws::{AWS_REGION_VAR, MapAwsHost};
use heycode_core::{ContributionKind, Plugin, compose};
use heycode_llm::{
    CatalogError, CatalogRefreshMode, CatalogRegistry, ModelLifecycleStatus, SERVICE_MODELS,
};
use heycode_provider_aws::{BEDROCK_PROVIDER, BedrockCatalogConfig, bedrock_catalog_plugin};
use tokio_util::sync::CancellationToken;

use super::support::{
    AwsAuthPlugin, CredentialsPlugin, Secret, TEST_REGION, body, http, http_plugin, query,
    response, summary,
};

const MODEL_ID: &str = "anthropic.claude-sonnet-4-20250514-v1:0";

fn world(host: MapAwsHost) -> Vec<Box<dyn Plugin>> {
    let (service, _requests) = http(vec![response(200, &body(vec![summary(MODEL_ID)]))]);
    vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        http_plugin(service),
        Box::new(CredentialsPlugin(Secret::Present)),
        Box::new(AwsAuthPlugin(host)),
        bedrock_catalog_plugin(BedrockCatalogConfig::api_key(query())),
    ]
}

fn configured_host() -> MapAwsHost {
    MapAwsHost::new().with_var(AWS_REGION_VAR, TEST_REGION)
}

#[tokio::test]
async fn the_plugin_declares_exactly_one_bedrock_model_catalog_contribution() {
    let mut context = compose(&world(configured_host())).unwrap();
    let contributions = context.plugin_inventory().snapshot().unwrap().contributions;
    let owned: Vec<_> = contributions
        .iter()
        .filter(|row| row.plugin == "catalog-bedrock")
        .collect();
    assert_eq!(owned.len(), 1, "one declared contribution");
    assert_eq!(owned[0].kind, ContributionKind::ModelCatalog);
    assert_eq!(owned[0].name, BEDROCK_PROVIDER);
    context.shutdown();
}

#[tokio::test]
async fn the_plugin_registers_a_refreshable_bedrock_catalog_and_disposes_it() {
    let mut context = compose(&world(configured_host())).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let published = view
        .snapshot
        .models
        .iter()
        .find(|model| model.id == MODEL_ID)
        .expect("the fixture row must be published");
    assert_eq!(published.lifecycle.status, ModelLifecycleStatus::Stable);

    context.shutdown();
    assert!(
        matches!(
            models
                .refresh(
                    BEDROCK_PROVIDER,
                    CatalogRefreshMode::Force,
                    CancellationToken::new(),
                )
                .await,
            Err(CatalogError::UnknownCatalog { .. })
        ),
        "shutdown must remove the registration"
    );
}

#[tokio::test]
async fn draft_region_probe_is_live_but_never_publishes_a_cached_generation() {
    let mut context = compose(&world(MapAwsHost::new())).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let snapshot = models
        .probe_parameters(
            BEDROCK_PROVIDER,
            &BTreeMap::from([("region".into(), TEST_REGION.into())]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.models[0].id, MODEL_ID);
    assert!(
        matches!(
            models.cached(BEDROCK_PROVIDER),
            Err(CatalogError::NoCachedCatalog { .. })
        ),
        "a draft must not become the selected region's durable generation"
    );
    context.shutdown();
}

#[tokio::test]
async fn a_catalog_without_a_host_region_composes_for_draft_region_setup() {
    let mut context = compose(&world(MapAwsHost::new())).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let error = models
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no AWS region is configured"),
        "normal refresh remains honest until the form supplies a region: {error}"
    );
    context.shutdown();
}

#[tokio::test]
async fn a_malformed_host_region_stays_a_safe_refresh_failure_without_blocking_setup() {
    let host = MapAwsHost::new().with_var(AWS_REGION_VAR, "US-EAST-1");
    let mut context = compose(&world(host)).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let error = models
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("not a valid region id"),
        "unexpected failure: {message}"
    );
    assert!(
        !message.contains("US-EAST-1"),
        "the rejected value must not be echoed: {message}"
    );
    context.shutdown();
}

#[tokio::test]
async fn the_plugin_refuses_to_compose_without_the_aws_authorization_service() {
    let (service, _requests) = http(Vec::new());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        http_plugin(service),
        Box::new(CredentialsPlugin(Secret::Present)),
        bedrock_catalog_plugin(BedrockCatalogConfig::api_key(query())),
    ];
    let Err(error) = compose(&plugins) else {
        panic!("PAWS01 is a declared dependency");
    };
    assert!(
        error.to_string().contains("aws-auth"),
        "unexpected failure: {error}"
    );
}

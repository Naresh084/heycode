//! PAWS02 Mantle catalog contribution: composition, coexistence with the
//! runtime catalog, region resolution and effect ownership.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use heycode_authorization_aws::{AWS_REGION_VAR, MapAwsHost};
use heycode_core::{ContributionKind, Plugin, compose};
use heycode_llm::{
    CatalogError, CatalogRefreshMode, CatalogRegistry, ModelLifecycleStatus, SERVICE_MODELS,
};
use heycode_provider_aws::{
    BEDROCK_PROVIDER, BedrockCatalogConfig, MANTLE_PROVIDER, MantleCatalogConfig,
    bedrock_catalog_plugin, mantle_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    AwsAuthPlugin, CredentialsPlugin, Secret, TEST_REGION, body, http, http_plugin, mantle_body,
    mantle_entry, query, response, summary,
};

const MANTLE_MODEL: &str = "openai.gpt-oss-120b";
const RUNTIME_MODEL: &str = "anthropic.claude-sonnet-4-20250514-v1:0";

fn configured_host() -> MapAwsHost {
    MapAwsHost::new().with_var(AWS_REGION_VAR, TEST_REGION)
}

/// A world with the Mantle catalog only.
fn mantle_world(host: MapAwsHost) -> Vec<Box<dyn Plugin>> {
    let (service, _requests) = http(vec![response(
        200,
        &mantle_body(vec![mantle_entry(MANTLE_MODEL)]),
    )]);
    vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        http_plugin(service),
        Box::new(CredentialsPlugin(Secret::Present)),
        Box::new(AwsAuthPlugin(host)),
        mantle_catalog_plugin(MantleCatalogConfig::api_key(query())),
    ]
}

#[tokio::test]
async fn the_plugin_declares_exactly_one_mantle_model_catalog_contribution() {
    let mut context = compose(&mantle_world(configured_host())).unwrap();
    let contributions = context.plugin_inventory().snapshot().unwrap().contributions;
    let owned: Vec<_> = contributions
        .iter()
        .filter(|row| row.plugin == "catalog-bedrock-mantle")
        .collect();
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].kind, ContributionKind::ModelCatalog);
    assert_eq!(owned[0].name, MANTLE_PROVIDER);
    context.shutdown();
}

#[tokio::test]
async fn the_plugin_registers_a_refreshable_mantle_catalog_and_disposes_it() {
    let mut context = compose(&mantle_world(configured_host())).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            MANTLE_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let published = view
        .snapshot
        .models
        .iter()
        .find(|model| model.id == MANTLE_MODEL)
        .expect("the fixture row must be published");
    assert_eq!(
        published.lifecycle.status,
        ModelLifecycleStatus::Unknown,
        "this endpoint publishes no lifecycle evidence"
    );

    context.shutdown();
    assert!(
        matches!(
            models
                .refresh(
                    MANTLE_PROVIDER,
                    CatalogRefreshMode::Force,
                    CancellationToken::new()
                )
                .await,
            Err(CatalogError::UnknownCatalog { .. })
        ),
        "shutdown must remove the registration"
    );
}

#[tokio::test]
async fn both_aws_catalogs_compose_together_under_distinct_provider_ids() {
    // The registry keys by provider id and fails loud on a duplicate, so this
    // is the test that proves the two AWS endpoints are two catalogs rather
    // than one contested row.
    let (service, _requests) = http(vec![
        response(200, &body(vec![summary(RUNTIME_MODEL)])),
        response(200, &mantle_body(vec![mantle_entry(MANTLE_MODEL)])),
    ]);
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        http_plugin(service),
        Box::new(CredentialsPlugin(Secret::Present)),
        Box::new(AwsAuthPlugin(configured_host())),
        bedrock_catalog_plugin(BedrockCatalogConfig::api_key(query())),
        mantle_catalog_plugin(MantleCatalogConfig::api_key(query())),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();

    let runtime = models
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mantle = models
        .refresh(
            MANTLE_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let runtime_ids: Vec<&str> = runtime
        .snapshot
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let mantle_ids: Vec<&str> = mantle
        .snapshot
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(runtime_ids, [RUNTIME_MODEL]);
    assert_eq!(mantle_ids, [MANTLE_MODEL]);
    assert!(
        !runtime_ids.contains(&MANTLE_MODEL) && !mantle_ids.contains(&RUNTIME_MODEL),
        "the two endpoints have different model sets and must not be merged"
    );

    let rows: Vec<String> = context
        .plugin_inventory()
        .snapshot()
        .unwrap()
        .contributions
        .iter()
        .filter(|row| row.kind == ContributionKind::ModelCatalog)
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(rows, [BEDROCK_PROVIDER, MANTLE_PROVIDER]);
    context.shutdown();
}

#[tokio::test]
async fn composition_fails_loud_when_no_aws_region_is_configured() {
    let Err(error) = compose(&mantle_world(MapAwsHost::new())) else {
        panic!("a world with no region must not compose");
    };
    let message = error.to_string();
    assert!(message.contains("Mantle"), "{message}");
    assert!(message.contains("no AWS region is configured"), "{message}");
}

#[tokio::test]
async fn a_malformed_region_is_explained_without_echoing_the_rejected_value() {
    let host = MapAwsHost::new().with_var(AWS_REGION_VAR, "US EAST 1");
    let Err(error) = compose(&mantle_world(host)) else {
        panic!("a malformed region must not compose");
    };
    let message = error.to_string();
    assert!(message.contains("not a valid region id"), "{message}");
    assert!(!message.contains("US EAST 1"), "{message}");
}

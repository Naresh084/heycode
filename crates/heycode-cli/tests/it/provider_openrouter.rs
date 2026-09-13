//! POR01 production composition ownership contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_cli::testing::RealCompositionHarness;

#[test]
fn production_openrouter_flow_is_owned_by_the_provider_plugin() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    assert!(world.context().plugins().contains(&"provider-openrouter"));
    assert!(world.context().plugins().contains(&"catalog-openrouter"));

    let inventory = world.context().plugin_inventory().snapshot().unwrap();
    let owners = inventory
        .contributions
        .iter()
        .filter(|row| {
            row.kind == heycode_core::ContributionKind::AuthorizationFlow
                && row.name == "openrouter-api-key"
        })
        .map(|row| row.plugin)
        .collect::<Vec<_>>();
    assert_eq!(owners, ["provider-openrouter"]);
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "catalog-openrouter"
            && row.kind == heycode_core::ContributionKind::ModelCatalog
            && row.name == "openrouter"
    }));

    let authorization = world
        .context()
        .get::<heycode_authorization::AuthorizationService>(
            heycode_authorization::SERVICE_AUTHORIZATION,
        )
        .unwrap();
    assert!(
        authorization
            .descriptors()
            .unwrap()
            .iter()
            .any(|descriptor| {
                descriptor.id.as_str() == "openrouter-api-key"
                    && descriptor.query.reference.as_str() == "OPENROUTER_API_KEY"
            })
    );
    let catalogs = world
        .context()
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        catalogs
            .descriptors()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor.id == "openrouter")
    );
}

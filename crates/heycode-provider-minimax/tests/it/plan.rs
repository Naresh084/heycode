//! PMM01: the two MiniMax products are distinct identities, not labels.

use std::collections::BTreeSet;

use heycode_provider_minimax::{MiniMaxPlan, MiniMaxPlanId, PayAsYouGo, TokenPlan};

#[test]
fn the_two_products_route_under_different_registry_names() {
    assert_eq!(MiniMaxPlanId::PayAsYouGo.registry_name(), "minimax");
    assert_eq!(
        MiniMaxPlanId::TokenPlan.registry_name(),
        "minimax-token-plan"
    );
    assert_ne!(
        MiniMaxPlanId::PayAsYouGo.registry_name(),
        MiniMaxPlanId::TokenPlan.registry_name()
    );
}

#[test]
fn every_product_display_name_states_which_product_is_billed() {
    assert_eq!(
        MiniMaxPlanId::PayAsYouGo.display_name(),
        "MiniMax (Pay-as-you-go)"
    );
    assert_eq!(
        MiniMaxPlanId::TokenPlan.display_name(),
        "MiniMax (Token Plan)"
    );
    for plan in MiniMaxPlanId::ALL {
        assert!(
            plan.display_name().contains(plan.label()),
            "`{}` does not name its plan",
            plan.display_name()
        );
    }
}

#[test]
fn the_two_products_read_different_credential_references() {
    assert_eq!(
        MiniMaxPlanId::PayAsYouGo.credential_reference(),
        "MINIMAX_API_KEY"
    );
    assert_eq!(
        MiniMaxPlanId::TokenPlan.credential_reference(),
        "MINIMAX_TOKEN_PLAN_KEY"
    );
    assert_ne!(
        MiniMaxPlanId::PayAsYouGo.credential_reference(),
        MiniMaxPlanId::TokenPlan.credential_reference()
    );
}

#[test]
fn the_two_products_declare_different_credential_kinds() {
    assert_eq!(MiniMaxPlanId::PayAsYouGo.credential_kind(), "api-key");
    assert_eq!(
        MiniMaxPlanId::TokenPlan.credential_kind(),
        "subscription-key"
    );
    assert_ne!(
        MiniMaxPlanId::PayAsYouGo.credential_kind(),
        MiniMaxPlanId::TokenPlan.credential_kind()
    );
}

#[test]
fn only_the_token_plan_publishes_a_documented_key_prefix() {
    assert_eq!(
        MiniMaxPlanId::TokenPlan.documented_key_prefix(),
        Some("sk-cp")
    );
    assert_eq!(MiniMaxPlanId::PayAsYouGo.documented_key_prefix(), None);
}

#[test]
fn each_marker_type_carries_its_matching_runtime_plan_id() {
    assert_eq!(PayAsYouGo::ID, MiniMaxPlanId::PayAsYouGo);
    assert_eq!(TokenPlan::ID, MiniMaxPlanId::TokenPlan);
}

#[test]
fn enumerated_plans_are_unique_across_every_identity_field() {
    let mut names = BTreeSet::new();
    let mut references = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for plan in MiniMaxPlanId::ALL {
        assert!(
            names.insert(plan.registry_name()),
            "duplicate registry name"
        );
        assert!(
            references.insert(plan.credential_reference()),
            "duplicate credential reference"
        );
        assert!(
            kinds.insert(plan.credential_kind()),
            "duplicate credential kind"
        );
    }
    assert_eq!(names.len(), MiniMaxPlanId::ALL.len());
}

#[test]
fn plan_display_is_the_short_label_and_not_the_provider_name() {
    assert_eq!(MiniMaxPlanId::PayAsYouGo.to_string(), "Pay-as-you-go");
    assert_eq!(MiniMaxPlanId::TokenPlan.to_string(), "Token Plan");
}

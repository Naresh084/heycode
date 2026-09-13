//! PMM01: one profile per product, and resolution binds a secret to it.

use std::sync::Arc;

use heycode_core::{Context, ProviderProtocol};
use heycode_credentials::CredentialsService;
use heycode_llm::CapabilitySupport;
use heycode_provider_minimax::{
    MINIMAX_M3, MiniMaxApiFamily, MiniMaxCredentialError, MiniMaxPlanId, MiniMaxProfile,
    MiniMaxRegion, PayAsYouGo, TokenPlan,
};

use super::support::{FailingCredentials, MapCredentials};

const TOKEN_PLAN_SECRET: &str = "sk-cp-pmm01-not-a-real-key";
const PAYG_SECRET: &str = "pmm01-pay-as-you-go-not-a-real-key";

fn payg() -> MiniMaxProfile<PayAsYouGo> {
    MiniMaxProfile::international()
}

fn token_plan() -> MiniMaxProfile<TokenPlan> {
    MiniMaxProfile::international()
}

fn service(entries: &[(&str, &str, &str)], context: &Context) -> CredentialsService {
    let credentials = CredentialsService::new();
    credentials
        .register(context, Arc::new(MapCredentials::new(entries)))
        .expect("test provider registers once");
    credentials
}

#[test]
fn each_product_builds_a_valid_and_distinct_s04_credential_query() {
    let payg_query = payg()
        .credential_query()
        .expect("built-in literals validate");
    let token_plan_query = token_plan()
        .credential_query()
        .expect("built-in literals validate");

    assert_eq!(payg_query.reference.as_str(), "MINIMAX_API_KEY");
    assert_eq!(payg_query.kind.as_str(), "api-key");
    assert_eq!(
        token_plan_query.reference.as_str(),
        "MINIMAX_TOKEN_PLAN_KEY"
    );
    assert_eq!(token_plan_query.kind.as_str(), "subscription-key");
    assert_ne!(payg_query, token_plan_query);
}

#[test]
fn provider_profiles_expose_the_product_owned_registry_name_and_reference() {
    let payg_profile = payg().provider_profile();
    let token_plan_profile = token_plan().provider_profile();

    assert_eq!(payg_profile.registry_name, "minimax");
    assert_eq!(payg_profile.descriptor.id, "minimax");
    assert_eq!(
        payg_profile.credential_reference.as_deref(),
        Some("MINIMAX_API_KEY")
    );

    assert_eq!(token_plan_profile.registry_name, "minimax-token-plan");
    assert_eq!(token_plan_profile.descriptor.id, "minimax-token-plan");
    assert_eq!(
        token_plan_profile.credential_reference.as_deref(),
        Some("MINIMAX_TOKEN_PLAN_KEY")
    );

    assert_ne!(
        payg_profile.registry_name, token_plan_profile.registry_name,
        "both products would collide in one ProviderRegistry"
    );
    assert_ne!(
        payg_profile.credential_reference,
        token_plan_profile.credential_reference
    );
}

#[test]
fn both_products_default_to_the_documented_minimax_m3_model() {
    assert_eq!(MINIMAX_M3, "MiniMax-M3");
    assert_eq!(payg().provider_profile().default_model, MINIMAX_M3);
    assert_eq!(token_plan().provider_profile().default_model, MINIMAX_M3);
}

#[test]
fn international_profiles_declare_both_documented_protocol_families() {
    for protocols in [
        payg().provider_descriptor().protocols,
        token_plan().provider_descriptor().protocols,
    ] {
        assert_eq!(
            protocols,
            vec![
                ProviderProtocol::OpenAiChatCompletions,
                ProviderProtocol::AnthropicMessages,
            ]
        );
    }
}

#[test]
fn a_mainland_china_profile_omits_the_undocumented_openai_compatible_route() {
    let profile = MiniMaxProfile::<TokenPlan>::new(MiniMaxRegion::MainlandChina);
    assert_eq!(
        profile.provider_descriptor().protocols,
        vec![ProviderProtocol::AnthropicMessages]
    );
    assert_eq!(
        profile.documented_base_url(MiniMaxApiFamily::OpenAiCompatible),
        CapabilitySupport::Unknown
    );
    assert_eq!(
        profile.base_url(MiniMaxApiFamily::AnthropicCompatible),
        "https://api.minimaxi.com/anthropic"
    );
}

#[test]
fn a_profile_keeps_its_product_and_region_after_construction() {
    assert_eq!(payg().plan(), MiniMaxPlanId::PayAsYouGo);
    assert_eq!(token_plan().plan(), MiniMaxPlanId::TokenPlan);
    assert_eq!(payg().region(), MiniMaxRegion::International);
    assert_eq!(
        MiniMaxProfile::<PayAsYouGo>::new(MiniMaxRegion::MainlandChina).region(),
        MiniMaxRegion::MainlandChina
    );
}

#[test]
fn resolution_reports_missing_when_no_provider_holds_the_reference() {
    let credentials = CredentialsService::new();
    assert_eq!(
        payg().resolve(&credentials).unwrap_err(),
        MiniMaxCredentialError::Missing {
            plan: MiniMaxPlanId::PayAsYouGo,
            reference: "MINIMAX_API_KEY",
        }
    );
    assert_eq!(
        token_plan().resolve(&credentials).unwrap_err(),
        MiniMaxCredentialError::Missing {
            plan: MiniMaxPlanId::TokenPlan,
            reference: "MINIMAX_TOKEN_PLAN_KEY",
        }
    );
}

#[test]
fn resolution_binds_each_stored_secret_to_the_profile_that_asked_for_it() {
    let context = Context::new();
    let credentials = service(
        &[
            ("MINIMAX_API_KEY", "api-key", PAYG_SECRET),
            (
                "MINIMAX_TOKEN_PLAN_KEY",
                "subscription-key",
                TOKEN_PLAN_SECRET,
            ),
        ],
        &context,
    );

    let payg_credential = payg()
        .resolve(&credentials)
        .expect("pay-as-you-go resolves");
    let token_plan_credential = token_plan()
        .resolve(&credentials)
        .expect("token plan resolves");

    assert_eq!(payg_credential.plan(), MiniMaxPlanId::PayAsYouGo);
    assert_eq!(payg_credential.expose(), PAYG_SECRET);
    assert_eq!(token_plan_credential.plan(), MiniMaxPlanId::TokenPlan);
    assert_eq!(token_plan_credential.expose(), TOKEN_PLAN_SECRET);
}

#[test]
fn a_token_plan_secret_stored_under_the_pay_as_you_go_reference_is_refused() {
    let context = Context::new();
    let credentials = service(
        &[("MINIMAX_API_KEY", "api-key", TOKEN_PLAN_SECRET)],
        &context,
    );
    assert_eq!(
        payg().resolve(&credentials).unwrap_err(),
        MiniMaxCredentialError::ForeignPlan {
            plan: MiniMaxPlanId::PayAsYouGo,
            other: MiniMaxPlanId::TokenPlan,
            prefix: "sk-cp",
        }
    );
}

#[test]
fn each_product_ignores_a_secret_stored_under_the_other_products_kind() {
    let context = Context::new();
    // Same reference name, wrong kind: the S04 query cannot match it.
    let credentials = service(
        &[("MINIMAX_TOKEN_PLAN_KEY", "api-key", TOKEN_PLAN_SECRET)],
        &context,
    );
    assert_eq!(
        token_plan().resolve(&credentials).unwrap_err(),
        MiniMaxCredentialError::Missing {
            plan: MiniMaxPlanId::TokenPlan,
            reference: "MINIMAX_TOKEN_PLAN_KEY",
        }
    );
}

#[test]
fn a_failing_credential_provider_surfaces_as_a_registry_error_not_as_missing() {
    let context = Context::new();
    let credentials = CredentialsService::new();
    credentials
        .register(&context, Arc::new(FailingCredentials::new()))
        .expect("test provider registers once");

    let error = payg().resolve(&credentials).unwrap_err();
    let MiniMaxCredentialError::Registry { plan, message } = &error else {
        panic!("a provider failure must not be reported as an absent credential: {error:?}");
    };
    assert_eq!(*plan, MiniMaxPlanId::PayAsYouGo);
    assert!(
        message.contains("test-failing"),
        "the redacted S04 cause must survive: {message}"
    );
}

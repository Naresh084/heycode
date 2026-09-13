//! PMM05: coding-tool use requires explicit current Token Plan eligibility.

use heycode_llm::CapabilitySupport;
use heycode_provider_minimax::{
    MiniMaxApiFamily, MiniMaxCodingEligibility, MiniMaxCodingProfile, MiniMaxCodingProfileError,
    MiniMaxPlanId, MiniMaxProfile, MiniMaxRegion, TokenPlan,
};

#[test]
fn a_subscription_key_without_resources_is_not_coding_eligibility() {
    for eligibility in [
        MiniMaxCodingEligibility::Unknown,
        MiniMaxCodingEligibility::NoResources,
    ] {
        assert_eq!(
            MiniMaxCodingProfile::select(MiniMaxProfile::<TokenPlan>::international(), eligibility,),
            Err(MiniMaxCodingProfileError::EligibilityRequired)
        );
    }
}

#[test]
fn a_seat_or_credits_is_an_explicit_eligible_selection() {
    for eligibility in [
        MiniMaxCodingEligibility::TokenPlanSeat,
        MiniMaxCodingEligibility::PurchasedCredits,
    ] {
        let selected =
            MiniMaxCodingProfile::select(MiniMaxProfile::<TokenPlan>::international(), eligibility)
                .unwrap();
        assert_eq!(selected.eligibility(), eligibility);
        assert_eq!(selected.plan(), MiniMaxPlanId::TokenPlan);
        assert_eq!(
            selected.provider_profile().registry_name,
            "minimax-token-plan"
        );
        let query = selected.credential_query().unwrap();
        assert_eq!(query.reference.as_str(), "MINIMAX_TOKEN_PLAN_KEY");
        assert_eq!(query.kind.as_str(), "subscription-key");
    }
}

#[test]
fn eligible_coding_use_routes_only_to_current_documented_token_plan_endpoints() {
    let selected = MiniMaxCodingProfile::select(
        MiniMaxProfile::<TokenPlan>::international(),
        MiniMaxCodingEligibility::TokenPlanSeat,
    )
    .unwrap();
    assert_eq!(
        selected.base_url(MiniMaxApiFamily::OpenAiCompatible),
        Ok("https://api.minimax.io/v1".to_owned())
    );
    assert_eq!(
        selected.base_url(MiniMaxApiFamily::AnthropicCompatible),
        Ok("https://api.minimax.io/anthropic".to_owned())
    );
}

#[test]
fn an_undocumented_region_protocol_pair_stays_unselectable_even_when_eligible() {
    let selected = MiniMaxCodingProfile::select(
        MiniMaxProfile::<TokenPlan>::new(MiniMaxRegion::MainlandChina),
        MiniMaxCodingEligibility::PurchasedCredits,
    )
    .unwrap();
    assert_eq!(
        selected.base_url(MiniMaxApiFamily::OpenAiCompatible),
        Err(MiniMaxCodingProfileError::UnsupportedRoute)
    );
    assert_eq!(
        selected.base_url(MiniMaxApiFamily::AnthropicCompatible),
        Ok("https://api.minimaxi.com/anthropic".to_owned())
    );
}

#[test]
fn former_coding_plan_does_not_invent_a_dedicated_endpoint() {
    assert_eq!(
        MiniMaxCodingProfile::dedicated_endpoint_evidence(),
        CapabilitySupport::Unknown
    );
}

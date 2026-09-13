//! PZA01 profiles: each plan is a separate provider identity.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::ProviderProtocol;
use heycode_provider_zai::{
    Coding, General, ZAI_CODING_PLAN_USAGE_RESTRICTION, ZAI_GLM_5_3, ZaiPlan, ZaiProfile,
    ZaiProtocol,
};

fn general() -> ZaiProfile<General> {
    ZaiProfile::<General>::documented(ZaiProtocol::OpenAiChatCompletions).unwrap()
}

fn coding() -> ZaiProfile<Coding> {
    ZaiProfile::<Coding>::documented(ZaiProtocol::OpenAiChatCompletions).unwrap()
}

#[test]
fn documented_profiles_bind_each_plan_to_its_own_endpoint_and_reference() {
    let general = general();
    let coding = coding();
    assert_eq!(general.plan(), ZaiPlan::General);
    assert_eq!(coding.plan(), ZaiPlan::Coding);
    assert_ne!(
        general.endpoint().base_url(),
        coding.endpoint().base_url(),
        "the two plans must not share an endpoint"
    );
    assert_ne!(
        general.credential().reference_name(),
        coding.credential().reference_name(),
        "the two plans must not share a credential reference"
    );
}

#[test]
fn the_two_plans_publish_distinct_registry_names() {
    assert_eq!(general().registry_name(), "zai");
    assert_eq!(coding().registry_name(), "zai-coding");
}

#[test]
fn provider_profile_metadata_satisfies_the_setup_registry_invariant() {
    for profile in [general().provider_profile(), coding().provider_profile()] {
        assert_eq!(profile.descriptor.id, profile.registry_name);
        assert_eq!(profile.descriptor.id.trim(), profile.descriptor.id);
        assert!(!profile.descriptor.id.is_empty());
        assert!(!profile.default_model.trim().is_empty());
        assert_eq!(profile.default_model, ZAI_GLM_5_3);
        assert_eq!(
            profile.descriptor.protocols,
            vec![ProviderProtocol::OpenAiChatCompletions]
        );
    }
}

#[test]
fn provider_profile_carries_the_plans_own_credential_reference() {
    assert_eq!(
        general().provider_profile().credential_reference.as_deref(),
        Some("ZAI_API_KEY")
    );
    assert_eq!(
        coding().provider_profile().credential_reference.as_deref(),
        Some("ZAI_CODING_API_KEY")
    );
}

#[test]
fn only_the_coding_plan_carries_the_published_usage_restriction() {
    assert_eq!(general().usage_restriction(), None);
    assert_eq!(
        coding().usage_restriction(),
        Some(ZAI_CODING_PLAN_USAGE_RESTRICTION)
    );
}

#[test]
fn a_coding_profile_keeps_the_anthropic_protocol_it_was_built_for() {
    let profile = ZaiProfile::<Coding>::documented(ZaiProtocol::AnthropicMessages).unwrap();
    assert_eq!(
        profile.endpoint().base_url(),
        "https://api.z.ai/api/anthropic"
    );
    assert_eq!(
        profile.provider_profile().descriptor.protocols,
        vec![ProviderProtocol::AnthropicMessages]
    );
}

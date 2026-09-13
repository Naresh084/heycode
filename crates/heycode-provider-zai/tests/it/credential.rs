//! PZA01 credentials: distinct references per plan, and no cross-plan reuse.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_provider_zai::{
    Coding, General, ZAI_CODING_API_KEY_REFERENCE, ZAI_GENERAL_API_KEY_REFERENCE, ZaiCredential,
    ZaiPlan, ZaiProfileError,
};

#[test]
fn each_plan_defaults_to_its_own_credential_reference() {
    let general = ZaiCredential::<General>::default_reference().unwrap();
    let coding = ZaiCredential::<Coding>::default_reference().unwrap();
    assert_eq!(general.plan(), ZaiPlan::General);
    assert_eq!(coding.plan(), ZaiPlan::Coding);
    assert_eq!(general.reference_name(), "ZAI_API_KEY");
    assert_eq!(coding.reference_name(), "ZAI_CODING_API_KEY");
    assert_ne!(general.reference_name(), coding.reference_name());
}

#[test]
fn a_plan_credential_refuses_the_other_plans_default_reference() {
    assert_eq!(
        ZaiCredential::<Coding>::reference(ZAI_GENERAL_API_KEY_REFERENCE).unwrap_err(),
        ZaiProfileError::CrossPlanReference {
            plan: ZaiPlan::Coding,
            reference: ZAI_GENERAL_API_KEY_REFERENCE.to_owned(),
        }
    );
    assert_eq!(
        ZaiCredential::<General>::reference(ZAI_CODING_API_KEY_REFERENCE).unwrap_err(),
        ZaiProfileError::CrossPlanReference {
            plan: ZaiPlan::General,
            reference: ZAI_CODING_API_KEY_REFERENCE.to_owned(),
        }
    );
}

#[test]
fn both_plans_share_the_api_key_credential_kind() {
    let general = ZaiCredential::<General>::default_reference().unwrap();
    let coding = ZaiCredential::<Coding>::default_reference().unwrap();
    assert_eq!(general.query().kind.as_str(), "api-key");
    assert_eq!(coding.query().kind.as_str(), "api-key");
    assert_ne!(general.query(), coding.query());
}

#[test]
fn an_explicit_reference_overrides_the_plan_default() {
    let credential = ZaiCredential::<Coding>::reference("MY_TEAM_PLAN_KEY").unwrap();
    assert_eq!(credential.reference_name(), "MY_TEAM_PLAN_KEY");
    assert_eq!(credential.plan(), ZaiPlan::Coding);
}

#[test]
fn a_malformed_reference_is_rejected_by_the_shared_grammar() {
    assert_eq!(
        ZaiCredential::<General>::reference("1 bad name").unwrap_err(),
        ZaiProfileError::InvalidReference {
            reference: "1 bad name".to_owned(),
        }
    );
}

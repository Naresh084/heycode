//! PMM01: admission binds a secret to one product and never repeats it.

use heycode_credentials::CredentialSecret;
use heycode_provider_minimax::{
    MiniMaxCredentialError, MiniMaxPlanId, MiniMaxProfile, PayAsYouGo, TokenPlan,
};

const TOKEN_PLAN_SECRET: &str = "sk-cp-pmm01-not-a-real-key";
const PAYG_SECRET: &str = "pmm01-pay-as-you-go-not-a-real-key";

fn payg() -> MiniMaxProfile<PayAsYouGo> {
    MiniMaxProfile::international()
}

fn token_plan() -> MiniMaxProfile<TokenPlan> {
    MiniMaxProfile::international()
}

#[test]
fn the_token_plan_admits_a_secret_carrying_the_documented_sk_cp_prefix() {
    let credential = token_plan()
        .admit(CredentialSecret::new(TOKEN_PLAN_SECRET))
        .expect("documented Token Plan prefix must be admitted");
    assert_eq!(credential.plan(), MiniMaxPlanId::TokenPlan);
    assert_eq!(credential.expose(), TOKEN_PLAN_SECRET);
}

#[test]
fn the_token_plan_refuses_a_secret_without_the_documented_sk_cp_prefix() {
    let error = token_plan()
        .admit(CredentialSecret::new(PAYG_SECRET))
        .expect_err("a Token Plan key must carry its documented prefix");
    assert_eq!(
        error,
        MiniMaxCredentialError::PrefixMismatch {
            plan: MiniMaxPlanId::TokenPlan,
            prefix: "sk-cp",
        }
    );
}

#[test]
fn pay_as_you_go_refuses_a_secret_carrying_the_token_plan_prefix() {
    let error = payg()
        .admit(CredentialSecret::new(TOKEN_PLAN_SECRET))
        .expect_err("an `sk-cp` secret is provably a Token Plan key");
    assert_eq!(
        error,
        MiniMaxCredentialError::ForeignPlan {
            plan: MiniMaxPlanId::PayAsYouGo,
            other: MiniMaxPlanId::TokenPlan,
            prefix: "sk-cp",
        }
    );
}

#[test]
fn pay_as_you_go_admits_a_secret_whose_prefix_minimax_does_not_document() {
    let credential = payg()
        .admit(CredentialSecret::new(PAYG_SECRET))
        .expect("no pay-as-you-go prefix is documented, so none may be required");
    assert_eq!(credential.plan(), MiniMaxPlanId::PayAsYouGo);
    // Guards the specific invention this row is exposed to: a third-party
    // `sk-api-` claim must not become a required prefix.
    payg()
        .admit(CredentialSecret::new("sk-api-pmm01-not-a-real-key"))
        .expect("an undocumented prefix must not be required either way");
}

#[test]
fn an_empty_secret_is_refused_for_both_products() {
    assert_eq!(
        payg().admit(CredentialSecret::new("")).unwrap_err(),
        MiniMaxCredentialError::Malformed {
            plan: MiniMaxPlanId::PayAsYouGo
        }
    );
    assert_eq!(
        token_plan().admit(CredentialSecret::new("")).unwrap_err(),
        MiniMaxCredentialError::Malformed {
            plan: MiniMaxPlanId::TokenPlan
        }
    );
}

#[test]
fn a_secret_holding_bytes_no_http_header_can_carry_is_refused() {
    for hostile in [
        "sk-cp-abc\r\nx-injected: 1",
        "sk-cp-abc\ndrop",
        "sk-cp abc",
        "sk-cp-abc\t",
        "sk-cp-abc\u{7f}",
        "sk-cp-ábc",
    ] {
        assert_eq!(
            token_plan()
                .admit(CredentialSecret::new(hostile))
                .unwrap_err(),
            MiniMaxCredentialError::Malformed {
                plan: MiniMaxPlanId::TokenPlan
            },
            "`{hostile}` must not be admitted as a header value"
        );
    }
}

#[test]
fn credential_debug_names_the_product_and_redacts_the_secret() {
    let payg_credential = payg().admit(CredentialSecret::new(PAYG_SECRET)).unwrap();
    let token_plan_credential = token_plan()
        .admit(CredentialSecret::new(TOKEN_PLAN_SECRET))
        .unwrap();

    let payg_debug = format!("{payg_credential:?}");
    let token_plan_debug = format!("{token_plan_credential:?}");

    assert_eq!(payg_debug, "MiniMaxCredential<minimax>([REDACTED])");
    assert_eq!(
        token_plan_debug,
        "MiniMaxCredential<minimax-token-plan>([REDACTED])"
    );
    assert!(!payg_debug.contains(PAYG_SECRET));
    assert!(!token_plan_debug.contains(TOKEN_PLAN_SECRET));
    assert!(!token_plan_debug.contains("sk-cp-pmm01"));
}

#[test]
fn admission_failures_never_repeat_the_rejected_secret() {
    let rejected = "sk-cp-pmm01-leaky-secret-value";
    let error = payg().admit(CredentialSecret::new(rejected)).unwrap_err();
    for rendered in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !rendered.contains(rejected),
            "rejected secret leaked into `{rendered}`"
        );
        assert!(
            !rendered.contains("pmm01-leaky"),
            "rejected secret leaked into `{rendered}`"
        );
    }
    // The documented prefix is public MiniMax documentation, not the secret.
    assert!(format!("{error}").contains("sk-cp"));
}

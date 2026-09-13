//! PZA01 visibility: what a user may see, and what they may never see.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_credentials::CredentialSource;
use heycode_provider_zai::{
    Coding, General, ZAI_CODING_PLAN_USAGE_RESTRICTION, ZaiPlan, ZaiProfile, ZaiProtocol,
    zai_plan_reports,
};

use super::support::credentials_with;

const GENERAL_SECRET: &str = "general-plan-secret-not-a-real-key";
const CODING_SECRET: &str = "coding-plan-secret-not-a-real-key";

fn general() -> ZaiProfile<General> {
    ZaiProfile::<General>::documented(ZaiProtocol::OpenAiChatCompletions).unwrap()
}

fn coding() -> ZaiProfile<Coding> {
    ZaiProfile::<Coding>::documented(ZaiProtocol::OpenAiChatCompletions).unwrap()
}

#[test]
fn a_report_names_the_plan_endpoint_and_credential_source() {
    let (_context, credentials, _calls) = credentials_with(&[("ZAI_API_KEY", GENERAL_SECRET)]);
    let report = general().report(&credentials).unwrap();
    assert_eq!(report.plan, ZaiPlan::General);
    assert_eq!(report.registry_name, "zai");
    assert_eq!(report.endpoint, "https://api.z.ai/api/paas/v4");
    assert_eq!(report.credential.reference.as_str(), "ZAI_API_KEY");
    assert!(report.credential.configured);
    assert_eq!(
        report.credential.source,
        Some(CredentialSource::Environment)
    );
    let summary = report.summary();
    assert!(
        summary.contains("https://api.z.ai/api/paas/v4"),
        "{summary}"
    );
    assert!(summary.contains("ZAI_API_KEY"), "{summary}");
    assert!(summary.contains("the process environment"), "{summary}");
}

#[test]
fn a_report_inspects_the_credential_registry_and_never_resolves_the_secret() {
    let (_context, credentials, calls) = credentials_with(&[("ZAI_API_KEY", GENERAL_SECRET)]);
    general().report(&credentials).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.inspected, vec!["ZAI_API_KEY".to_owned()]);
    assert!(
        calls.resolved.is_empty(),
        "reporting must not materialize the secret: {:?}",
        calls.resolved
    );
}

#[test]
fn a_report_of_a_configured_plan_never_contains_the_secret_value() {
    let (_context, credentials, _calls) = credentials_with(&[("ZAI_API_KEY", GENERAL_SECRET)]);
    let report = general().report(&credentials).unwrap();
    let debug = format!("{report:?}");
    assert!(!debug.contains(GENERAL_SECRET), "{debug}");
    assert!(!report.summary().contains(GENERAL_SECRET));
    assert!(debug.contains("ZAI_API_KEY"), "{debug}");
}

#[test]
fn an_unconfigured_plan_reports_no_source_instead_of_guessing() {
    let (_context, credentials, _calls) = credentials_with(&[]);
    let report = coding().report(&credentials).unwrap();
    assert!(!report.credential.configured);
    assert_eq!(report.credential.source, None);
    assert!(report.summary().contains("not configured"));
}

#[test]
fn the_two_plan_reports_differ_in_endpoint_and_credential_reference() {
    let (_context, credentials, _calls) = credentials_with(&[]);
    let [general, coding] = zai_plan_reports(&credentials).unwrap();
    assert_eq!(general.plan, ZaiPlan::General);
    assert_eq!(coding.plan, ZaiPlan::Coding);
    assert_ne!(general.endpoint, coding.endpoint);
    assert_ne!(
        general.credential.reference.as_str(),
        coding.credential.reference.as_str()
    );
    assert_ne!(general.registry_name, coding.registry_name);
}

#[test]
fn a_coding_report_ignores_a_configured_general_credential() {
    let (_context, credentials, _calls) = credentials_with(&[("ZAI_API_KEY", GENERAL_SECRET)]);
    let report = coding().report(&credentials).unwrap();
    assert_eq!(report.credential.reference.as_str(), "ZAI_CODING_API_KEY");
    assert!(
        !report.credential.configured,
        "the general key must not satisfy the Coding Plan"
    );
    let debug = format!("{report:?}");
    assert!(!debug.contains(GENERAL_SECRET), "{debug}");
    assert!(!debug.contains("ZAI_API_KEY\""), "{debug}");
}

#[test]
fn each_plan_reports_only_its_own_configured_credential() {
    let (_context, credentials, _calls) = credentials_with(&[
        ("ZAI_API_KEY", GENERAL_SECRET),
        ("ZAI_CODING_API_KEY", CODING_SECRET),
    ]);
    let [general, coding] = zai_plan_reports(&credentials).unwrap();
    assert!(general.credential.configured);
    assert!(coding.credential.configured);
    assert_eq!(general.credential.reference.as_str(), "ZAI_API_KEY");
    assert_eq!(coding.credential.reference.as_str(), "ZAI_CODING_API_KEY");
    for report in [&general, &coding] {
        let debug = format!("{report:?}");
        assert!(!debug.contains(GENERAL_SECRET), "{debug}");
        assert!(!debug.contains(CODING_SECRET), "{debug}");
    }
}

#[test]
fn a_coding_summary_shows_the_published_usage_restriction() {
    let (_context, credentials, _calls) = credentials_with(&[]);
    let [general, coding] = zai_plan_reports(&credentials).unwrap();
    assert_eq!(general.usage_restriction, None);
    assert!(!general.summary().contains("officially supported tools"));
    assert_eq!(
        coding.usage_restriction,
        Some(ZAI_CODING_PLAN_USAGE_RESTRICTION)
    );
    assert!(
        coding.summary().contains(ZAI_CODING_PLAN_USAGE_RESTRICTION),
        "{}",
        coding.summary()
    );
}

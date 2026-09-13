//! Q16 deterministic fresh-machine evidence matrix.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_install::{
    EvidenceSource, FreshMachineCheck, FreshMachineMatrix, FreshMachineObservation,
    OnboardingPlatform, TurnEvidence, parse_fresh_machine_evidence,
};

fn complete_run(
    platform: OnboardingPlatform,
    source: EvidenceSource,
    turn: TurnEvidence,
) -> Vec<FreshMachineObservation> {
    [
        FreshMachineCheck::AttestationVerified,
        FreshMachineCheck::FreshInstall,
        FreshMachineCheck::FirstRunReady,
        FreshMachineCheck::TurnCompleted(turn),
    ]
    .into_iter()
    .map(|check| FreshMachineObservation::passed(platform, source.clone(), check).unwrap())
    .collect()
}

#[test]
fn workflow_definitions_are_visible_but_never_count_as_observed_native_evidence() {
    let observations = OnboardingPlatform::ALL
        .into_iter()
        .map(|platform| {
            FreshMachineObservation::passed(
                platform,
                EvidenceSource::WorkflowDefinition,
                FreshMachineCheck::FreshInstall,
            )
            .unwrap()
        })
        .collect();
    let matrix = FreshMachineMatrix::evaluate(observations).unwrap();

    assert!(matrix.has_definition_for_all_platforms());
    assert!(!matrix.deterministic_native_complete());
    assert!(!matrix.q16_real_turn_complete());
}

#[test]
fn one_local_native_run_is_reported_only_for_its_platform() {
    let matrix = FreshMachineMatrix::evaluate(complete_run(
        OnboardingPlatform::Macos,
        EvidenceSource::LocalNative,
        TurnEvidence::DeterministicFake,
    ))
    .unwrap();

    assert!(
        matrix
            .platform(OnboardingPlatform::Macos)
            .deterministic_native_complete()
    );
    assert!(matrix.platform(OnboardingPlatform::Linux).is_unobserved());
    assert!(!matrix.deterministic_native_complete());
}

#[test]
fn hosted_native_fake_turns_complete_the_deterministic_harness_not_q16() {
    let mut observations = Vec::new();
    for (index, platform) in OnboardingPlatform::ALL.into_iter().enumerate() {
        observations.extend(complete_run(
            platform,
            EvidenceSource::HostedNative {
                run_id: 100 + u64::try_from(index).unwrap(),
            },
            TurnEvidence::DeterministicFake,
        ));
    }
    let matrix = FreshMachineMatrix::evaluate(observations).unwrap();

    assert!(matrix.deterministic_native_complete());
    assert!(!matrix.q16_real_turn_complete());
}

#[test]
fn q16_requires_a_real_provider_turn_on_all_three_native_platforms() {
    let mut observations = Vec::new();
    for (index, platform) in OnboardingPlatform::ALL.into_iter().enumerate() {
        observations.extend(complete_run(
            platform,
            EvidenceSource::HostedNative {
                run_id: 200 + u64::try_from(index).unwrap(),
            },
            TurnEvidence::RealProvider,
        ));
    }
    let matrix = FreshMachineMatrix::evaluate(observations).unwrap();

    assert!(matrix.deterministic_native_complete());
    assert!(matrix.q16_real_turn_complete());
}

#[test]
fn cross_compilation_cannot_satisfy_a_native_onboarding_cell() {
    let matrix = FreshMachineMatrix::evaluate(complete_run(
        OnboardingPlatform::Windows,
        EvidenceSource::CrossCompiled,
        TurnEvidence::RealProvider,
    ))
    .unwrap();

    assert!(
        !matrix
            .platform(OnboardingPlatform::Windows)
            .native_observed()
    );
    assert!(!matrix.q16_real_turn_complete());
}

#[test]
fn contradictory_results_for_one_run_and_check_fail_instead_of_picking_the_green_one() {
    let source = EvidenceSource::HostedNative { run_id: 77 };
    let passed = FreshMachineObservation::passed(
        OnboardingPlatform::Linux,
        source.clone(),
        FreshMachineCheck::FreshInstall,
    )
    .unwrap();
    let failed = FreshMachineObservation::failed(
        OnboardingPlatform::Linux,
        source,
        FreshMachineCheck::FreshInstall,
    )
    .unwrap();

    assert!(FreshMachineMatrix::evaluate(vec![passed, failed]).is_err());
}

#[test]
fn workflow_evidence_documents_feed_the_exact_real_provider_matrix() {
    let documents = [
        ("macos-aarch64", 901),
        ("linux-x86_64", 902),
        ("windows-x86_64", 903),
    ];
    let mut observations = Vec::new();
    for (platform, run_id) in documents {
        let document = format!(
            r#"{{"schema_version":1,"platform":"{platform}","source":{{"kind":"hosted_native","run_id":{run_id}}},"checks":["attestation_verified","fresh_install","first_run_ready"],"turn":"real_provider"}}"#
        );
        observations.extend(parse_fresh_machine_evidence(document.as_bytes()).unwrap());
    }

    let matrix = FreshMachineMatrix::evaluate(observations).unwrap();
    assert!(matrix.q16_real_turn_complete());
}

#[test]
fn evidence_documents_reject_future_schema_unknown_checks_and_zero_run() {
    for document in [
        r#"{"schema_version":2,"platform":"linux-x86_64","source":{"kind":"hosted_native","run_id":1},"checks":["attestation_verified","fresh_install","first_run_ready"],"turn":"real_provider"}"#,
        r#"{"schema_version":1,"platform":"linux-x86_64","source":{"kind":"hosted_native","run_id":1},"checks":["attestation_verified","fresh_install","other"],"turn":"real_provider"}"#,
        r#"{"schema_version":1,"platform":"linux-x86_64","source":{"kind":"hosted_native","run_id":0},"checks":["attestation_verified","fresh_install","first_run_ready"],"turn":"real_provider"}"#,
    ] {
        assert!(parse_fresh_machine_evidence(document.as_bytes()).is_err());
    }
}

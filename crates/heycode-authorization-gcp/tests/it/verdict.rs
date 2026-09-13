//! PGCP01 tri-state discipline: what each state projects to, and the rule that
//! an unknown answer is never rounded up.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use heycode_authorization_gcp::{
    ENV_GOOGLE_APPLICATION_CREDENTIALS, ENV_GOOGLE_CLOUD_LOCATION, GcpAccountHealth,
    GcpAdcCredentialType, GcpAdcFault, GcpAdcOrigin, GcpHealth, GcpLocation, GcpLocationError,
    GcpLocationHealth, GcpLocationOrigin, GcpProjectHealth, GcpProjectId, GcpProjectIdError,
    GcpProjectOrigin, GcpUncertainty,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    FakeMetadata, SERVICE_ACCOUNT_JSON, home_only, no_metadata_server, service, unix_probing,
};

fn account_states() -> Vec<(GcpAccountHealth, GcpHealth)> {
    let origin = GcpAdcOrigin::WellKnownFile {
        path: PathBuf::from("/home/fixture/.config/gcloud/application_default_credentials.json"),
    };
    vec![
        (GcpAccountHealth::Absent, GcpHealth::Unhealthy),
        (
            GcpAccountHealth::Faulted {
                origin: origin.clone(),
                fault: GcpAdcFault::NotJsonObject,
            },
            GcpHealth::Unhealthy,
        ),
        (
            GcpAccountHealth::Configured {
                origin,
                credential: Some(GcpAdcCredentialType::ServiceAccount),
            },
            GcpHealth::Unknown,
        ),
        (
            GcpAccountHealth::Undetermined {
                reason: GcpUncertainty::ProbeTimedOut,
            },
            GcpHealth::Unknown,
        ),
    ]
}

fn project_states() -> Vec<(GcpProjectHealth, GcpHealth)> {
    let project = GcpProjectId::new("pgcp01-fixture").unwrap();
    vec![
        (GcpProjectHealth::Unset, GcpHealth::Unhealthy),
        (
            GcpProjectHealth::Malformed {
                origin: GcpProjectOrigin::Explicit,
                error: GcpProjectIdError::Charset,
            },
            GcpHealth::Unhealthy,
        ),
        (
            GcpProjectHealth::Confirmed {
                project: project.clone(),
                origin: GcpProjectOrigin::MetadataServer,
            },
            GcpHealth::Healthy,
        ),
        (
            GcpProjectHealth::Unconfirmed {
                project,
                origin: GcpProjectOrigin::Explicit,
                reason: GcpUncertainty::NoAmbientAttestation,
            },
            GcpHealth::Unknown,
        ),
        (
            GcpProjectHealth::Undetermined {
                reason: GcpUncertainty::ProbeDisabled,
            },
            GcpHealth::Unknown,
        ),
    ]
}

fn location_states() -> Vec<(GcpLocationHealth, GcpHealth)> {
    vec![
        (GcpLocationHealth::Unset, GcpHealth::Unhealthy),
        (
            GcpLocationHealth::Malformed {
                origin: GcpLocationOrigin::Explicit,
                error: GcpLocationError::ZoneNotRegion,
            },
            GcpHealth::Unhealthy,
        ),
        (
            GcpLocationHealth::Selected {
                location: GcpLocation::new("us-central1").unwrap(),
                origin: GcpLocationOrigin::Explicit,
            },
            GcpHealth::Healthy,
        ),
        (
            GcpLocationHealth::Undetermined {
                reason: GcpUncertainty::Cancelled,
            },
            GcpHealth::Unknown,
        ),
    ]
}

#[test]
fn weaker_keeps_unhealthy_over_unknown_and_unknown_over_healthy() {
    use GcpHealth::{Healthy, Unhealthy, Unknown};
    for (left, right, expected) in [
        (Healthy, Healthy, Healthy),
        (Healthy, Unknown, Unknown),
        (Unknown, Healthy, Unknown),
        (Unknown, Unknown, Unknown),
        (Healthy, Unhealthy, Unhealthy),
        (Unhealthy, Healthy, Unhealthy),
        (Unknown, Unhealthy, Unhealthy),
        (Unhealthy, Unknown, Unhealthy),
        (Unhealthy, Unhealthy, Unhealthy),
    ] {
        assert_eq!(
            left.weaker(right),
            expected,
            "weaker({left:?}, {right:?}) must be {expected:?}"
        );
    }
}

#[test]
fn every_account_state_projects_to_its_pinned_verdict_and_none_is_healthy() {
    for (state, expected) in account_states() {
        assert_eq!(
            state.verdict(),
            expected,
            "{} projected wrongly",
            state.code()
        );
        assert_ne!(
            state.verdict(),
            GcpHealth::Healthy,
            "presence is not authentication: {} must not be healthy",
            state.code()
        );
    }
}

#[test]
fn every_project_state_projects_to_its_pinned_verdict() {
    for (state, expected) in project_states() {
        assert_eq!(
            state.verdict(),
            expected,
            "{} projected wrongly",
            state.code()
        );
    }
}

#[test]
fn every_location_state_projects_to_its_pinned_verdict() {
    for (state, expected) in location_states() {
        assert_eq!(
            state.verdict(),
            expected,
            "{} projected wrongly",
            state.code()
        );
    }
}

#[test]
fn no_undetermined_or_unconfirmed_state_projects_to_healthy() {
    let unknowns: Vec<(&'static str, GcpHealth)> = account_states()
        .into_iter()
        .map(|(state, _)| (state.code(), state.verdict()))
        .chain(
            project_states()
                .into_iter()
                .map(|(state, _)| (state.code(), state.verdict())),
        )
        .chain(
            location_states()
                .into_iter()
                .map(|(state, _)| (state.code(), state.verdict())),
        )
        .filter(|(code, _)| code.contains("undetermined") || code.contains("unconfirmed"))
        .collect();

    assert_eq!(
        unknowns.len(),
        4,
        "every subject contributes its unknown states"
    );
    for (code, verdict) in unknowns {
        assert_eq!(verdict, GcpHealth::Unknown, "{code} must stay unknown");
    }
}

#[test]
fn every_subject_state_has_a_distinct_stable_code() {
    let codes: BTreeSet<&'static str> = account_states()
        .into_iter()
        .map(|(state, _)| state.code())
        .chain(project_states().into_iter().map(|(state, _)| state.code()))
        .chain(location_states().into_iter().map(|(state, _)| state.code()))
        .collect();

    assert_eq!(codes.len(), 4 + 5 + 4);
}

#[tokio::test]
async fn the_profile_verdict_is_the_weakest_of_its_three_subjects() {
    // Determinate no-metadata host, a readable credential document and a valid
    // location: account is unknown, project unconfirmed, location healthy.
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", SERVICE_ACCOUNT_JSON)
        .with_var(ENV_GOOGLE_CLOUD_LOCATION, "us-central1");
    let unknown = service(environment, Arc::new(no_metadata_server()))
        .resolve(unix_probing(), CancellationToken::new())
        .await;
    assert_eq!(unknown.account().verdict(), GcpHealth::Unknown);
    assert_eq!(unknown.location().verdict(), GcpHealth::Healthy);
    assert_eq!(unknown.verdict(), GcpHealth::Unknown);

    // Add one determinate failure and the aggregate must drop to unhealthy.
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", SERVICE_ACCOUNT_JSON)
        .with_var(ENV_GOOGLE_CLOUD_LOCATION, "us-central1-a");
    let unhealthy = service(environment, Arc::new(no_metadata_server()))
        .resolve(unix_probing(), CancellationToken::new())
        .await;
    assert_eq!(unhealthy.location().verdict(), GcpHealth::Unhealthy);
    assert_eq!(unhealthy.verdict(), GcpHealth::Unhealthy);
}

#[tokio::test]
async fn the_profile_verdict_is_never_healthy_because_account_usability_is_unproven() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_CLOUD_LOCATION, "us-central1")
        .with_var("GOOGLE_CLOUD_PROJECT", "ambient-project-x");
    let profile = service(
        environment,
        Arc::new(FakeMetadata::gce(
            "ambient-project-x",
            "projects/12345/zones/us-central1-a",
        )),
    )
    .resolve(unix_probing(), CancellationToken::new())
    .await;

    assert_eq!(profile.project().verdict(), GcpHealth::Healthy);
    assert_eq!(profile.location().verdict(), GcpHealth::Healthy);
    assert_eq!(
        profile.verdict(),
        GcpHealth::Unknown,
        "no state of this row can prove an ADC credential usable, so the aggregate stops at unknown"
    );
}

#[tokio::test]
async fn a_resolved_profile_carries_the_instant_it_was_checked() {
    let profile = service(home_only(), Arc::new(no_metadata_server()))
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert!(profile.checked_at_ms().is_some_and(|value| value > 0));
}

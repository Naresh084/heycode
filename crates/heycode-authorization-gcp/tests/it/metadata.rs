//! PGCP01 ambient probe: what it sends, what it refuses to send, and the rule
//! that an undetermined probe never becomes a negative finding.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_authorization_gcp::testing::MapGcpEnvironment;
use heycode_authorization_gcp::{
    ENV_GCE_METADATA_HOST, ENV_NO_GCE_CHECK, GcpAccountHealth, GcpHealth, GcpLocationHealth,
    GcpProjectHealth, GcpUncertainty,
};
use heycode_http::TransportError;
use tokio_util::sync::CancellationToken;

use super::support::{
    FakeMetadata, PROJECT_URL, SERVICE_ACCOUNT_URL, ZONE_URL, home_only, metadata_ok,
    metadata_response, no_metadata_server, service, unix_offline, unix_probing,
};

#[tokio::test]
async fn every_probe_request_carries_the_metadata_flavor_header() {
    let transport = Arc::new(FakeMetadata::gce(
        "ambient-project-x",
        "projects/12345/zones/us-central1-a",
    ));
    service(home_only(), transport.clone())
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    for (url, headers) in requests {
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "metadata-flavor" && value == "Google"),
            "{url} must carry the documented Metadata-Flavor request header"
        );
    }
}

#[tokio::test]
async fn the_probe_reads_only_the_three_non_secret_paths_and_never_a_token() {
    let transport = Arc::new(FakeMetadata::gce(
        "ambient-project-x",
        "projects/12345/zones/us-central1-a",
    ));
    service(home_only(), transport.clone())
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(
        transport.urls(),
        vec![
            SERVICE_ACCOUNT_URL.to_owned(),
            PROJECT_URL.to_owned(),
            ZONE_URL.to_owned(),
        ]
    );
    assert!(
        !transport.urls().iter().any(|url| url.contains("token")),
        "the probe must never request an access token"
    );
}

#[tokio::test]
async fn a_two_hundred_without_the_flavor_marker_proves_no_metadata_server() {
    let profile = service(home_only(), Arc::new(no_metadata_server()))
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(profile.account(), &GcpAccountHealth::Absent);
}

#[tokio::test]
async fn an_unreachable_probe_leaves_every_subject_unknown_rather_than_absent() {
    let profile = service(home_only(), Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::ProbeUnreachable,
        }
    );
    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Undetermined {
            reason: GcpUncertainty::ProbeUnreachable,
        }
    );
    assert_eq!(
        profile.location(),
        &GcpLocationHealth::Undetermined {
            reason: GcpUncertainty::ProbeUnreachable,
        }
    );
    assert_eq!(profile.verdict(), GcpHealth::Unknown);
}

#[tokio::test]
async fn a_transport_timeout_is_reported_as_timed_out_and_not_as_an_absence() {
    let profile = service(
        home_only(),
        Arc::new(FakeMetadata::failing(TransportError::Timeout)),
    )
    .resolve(unix_probing(), CancellationToken::new())
    .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::ProbeTimedOut,
        }
    );
}

#[tokio::test]
async fn a_probe_that_never_settles_is_bounded_by_its_budget() {
    let resolved = tokio::time::timeout(
        Duration::from_secs(5),
        service(home_only(), Arc::new(FakeMetadata::stalling()))
            .resolve(unix_probing(), CancellationToken::new()),
    )
    .await
    .expect("the probe budget must bound a transport that never settles");

    assert_eq!(
        resolved.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::ProbeTimedOut,
        }
    );
}

#[tokio::test]
async fn cancellation_before_the_probe_yields_cancelled_and_no_request() {
    let transport = Arc::new(FakeMetadata::gce(
        "ambient-project-x",
        "projects/12345/zones/us-central1-a",
    ));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let profile = service(home_only(), transport.clone())
        .resolve(unix_probing(), cancellation)
        .await;

    assert!(transport.requests().is_empty());
    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::Cancelled,
        }
    );
    assert_ne!(profile.account().verdict(), GcpHealth::Unhealthy);
}

#[tokio::test]
async fn cancellation_during_the_probe_settles_as_cancelled() {
    let transport = Arc::new(FakeMetadata::stalling());
    let cancellation = CancellationToken::new();
    let waker = cancellation.clone();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        waker.cancel();
    });
    let profile = service(home_only(), transport.clone())
        .resolve(
            heycode_authorization_gcp::GcpProfileRequest {
                platform: heycode_authorization_gcp::GcpHostPlatform::Unix,
                metadata: heycode_authorization_gcp::GcpMetadataPolicy::Probe {
                    budget: Duration::from_secs(5),
                },
                ..heycode_authorization_gcp::GcpProfileRequest::default()
            },
            cancellation,
        )
        .await;
    handle.await.expect("waker task joins");

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::Cancelled,
        }
    );
}

#[tokio::test]
async fn a_disabled_policy_performs_no_request_and_leaves_ambient_facts_unknown() {
    let transport = Arc::new(FakeMetadata::gce(
        "ambient-project-x",
        "projects/12345/zones/us-central1-a",
    ));
    let profile = service(home_only(), transport.clone())
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(transport.requests().is_empty());
    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Undetermined {
            reason: GcpUncertainty::ProbeDisabled,
        },
        "a source that was never inspected cannot report the project unset"
    );
    assert_eq!(
        profile.location(),
        &GcpLocationHealth::Undetermined {
            reason: GcpUncertainty::ProbeDisabled,
        }
    );
}

#[tokio::test]
async fn no_gce_check_true_suppresses_the_probe_without_claiming_an_absence() {
    let transport = Arc::new(FakeMetadata::gce(
        "ambient-project-x",
        "projects/12345/zones/us-central1-a",
    ));
    let environment = home_only().with_var(ENV_NO_GCE_CHECK, "true");
    let profile = service(environment, transport.clone())
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert!(transport.requests().is_empty());
    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::ProbeSuppressed,
        }
    );
}

#[tokio::test]
async fn no_gce_check_with_any_other_value_still_probes() {
    for value in ["false", "0", "TRUE", "yes"] {
        let transport = Arc::new(FakeMetadata::gce(
            "ambient-project-x",
            "projects/12345/zones/us-central1-a",
        ));
        let environment = home_only().with_var(ENV_NO_GCE_CHECK, value);
        service(environment, transport.clone())
            .resolve(unix_probing(), CancellationToken::new())
            .await;

        assert!(
            !transport.requests().is_empty(),
            "`NO_GCE_CHECK={value}` is not the documented suppression value"
        );
    }
}

#[tokio::test]
async fn gce_metadata_host_override_is_used_for_every_probe_request() {
    let transport = Arc::new(FakeMetadata::answering(vec![
        (
            "http://127.0.0.1:8080/computeMetadata/v1/instance/service-accounts/default/",
            metadata_ok("default\n"),
        ),
        (
            "http://127.0.0.1:8080/computeMetadata/v1/project/project-id",
            metadata_ok("emulated-project"),
        ),
        (
            "http://127.0.0.1:8080/computeMetadata/v1/instance/zone",
            metadata_ok("projects/1/zones/us-east1-c"),
        ),
    ]));
    let environment = home_only().with_var(ENV_GCE_METADATA_HOST, "127.0.0.1:8080");
    let profile = service(environment, transport.clone())
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert!(
        transport
            .urls()
            .iter()
            .all(|url| url.starts_with("http://127.0.0.1:8080/computeMetadata/v1/"))
    );
    assert_eq!(
        profile.project().project().map(|id| id.as_str()),
        Some("emulated-project")
    );
}

#[tokio::test]
async fn a_metadata_host_carrying_a_path_is_refused_before_any_request() {
    let transport = Arc::new(FakeMetadata::gce(
        "ambient-project-x",
        "projects/12345/zones/us-central1-a",
    ));
    let environment = home_only().with_var(ENV_GCE_METADATA_HOST, "evil.example.com/steal");
    let profile = service(environment, transport.clone())
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert!(transport.requests().is_empty());
    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::ProbeHostInvalid,
        }
    );
}

#[tokio::test]
async fn an_undocumented_metadata_status_is_undetermined_and_not_an_absence() {
    let transport = Arc::new(FakeMetadata::answering(vec![(
        SERVICE_ACCOUNT_URL,
        metadata_response(500, true, ""),
    )]));
    let profile = service(home_only(), transport)
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::ProbeUnexpectedStatus,
        }
    );
}

#[tokio::test]
async fn a_metadata_server_that_reports_no_project_leaves_the_project_unset() {
    let transport = Arc::new(FakeMetadata::answering(vec![
        (SERVICE_ACCOUNT_URL, metadata_ok("default\n")),
        (PROJECT_URL, metadata_ok("   ")),
        (ZONE_URL, metadata_ok("projects/1/zones/us-east1-c")),
    ]));
    let profile = service(
        MapGcpEnvironment::new().with_var("HOME", "/home/fixture"),
        transport,
    )
    .resolve(unix_probing(), CancellationToken::new())
    .await;

    assert_eq!(profile.project(), &GcpProjectHealth::Unset);
    assert!(matches!(
        profile.location(),
        GcpLocationHealth::Selected { .. }
    ));
}

#[tokio::test]
async fn a_project_lookup_that_never_settles_leaves_the_project_undetermined() {
    let transport = Arc::new(FakeMetadata::answering(vec![
        (SERVICE_ACCOUNT_URL, metadata_ok("default\n")),
        (ZONE_URL, metadata_ok("projects/1/zones/us-east1-c")),
    ]));
    let profile = service(home_only(), transport)
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Undetermined {
            reason: GcpUncertainty::ProbeUnreachable,
        },
        "a live metadata server whose project lookup failed says nothing about the project"
    );
    assert!(matches!(
        profile.location(),
        GcpLocationHealth::Selected { .. }
    ));
}

#[tokio::test]
async fn a_zone_lookup_that_never_settles_leaves_the_location_undetermined() {
    let transport = Arc::new(FakeMetadata::answering(vec![
        (SERVICE_ACCOUNT_URL, metadata_ok("default\n")),
        (PROJECT_URL, metadata_ok("ambient-project-x")),
    ]));
    let profile = service(home_only(), transport)
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.location(),
        &GcpLocationHealth::Undetermined {
            reason: GcpUncertainty::ProbeUnreachable,
        }
    );
    assert!(matches!(
        profile.project(),
        GcpProjectHealth::Confirmed { .. }
    ));
}

#[tokio::test]
async fn a_configured_project_the_host_could_not_attest_stays_unconfirmed_with_that_reason() {
    let transport = Arc::new(FakeMetadata::answering(vec![
        (SERVICE_ACCOUNT_URL, metadata_ok("default\n")),
        (ZONE_URL, metadata_ok("projects/1/zones/us-east1-c")),
    ]));
    let environment = home_only().with_var("GOOGLE_CLOUD_PROJECT", "chosen-project-y");
    let profile = service(environment, transport)
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert!(
        matches!(
            profile.project(),
            GcpProjectHealth::Unconfirmed {
                reason: GcpUncertainty::ProbeUnreachable,
                ..
            }
        ),
        "an attestation that could not be read is not an attestation that disagreed"
    );
}

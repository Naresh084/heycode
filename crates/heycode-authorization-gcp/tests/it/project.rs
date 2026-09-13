//! PGCP01 project health: source precedence, ambient attestation, and the
//! separation of "unset", "malformed" and "unconfirmed".

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_authorization_gcp::{
    ENV_CLOUDSDK_CORE_PROJECT, ENV_GCLOUD_PROJECT, ENV_GOOGLE_APPLICATION_CREDENTIALS,
    ENV_GOOGLE_CLOUD_PROJECT, GcpHealth, GcpProjectHealth, GcpProjectId, GcpProjectIdError,
    GcpProjectIdKind, GcpProjectOrigin, GcpUncertainty,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    FakeMetadata, SERVICE_ACCOUNT_JSON, WELL_KNOWN_UNIX, home_only, no_metadata_server, service,
    unix_offline, unix_probing,
};

#[tokio::test]
async fn explicit_request_project_outranks_every_discovered_source() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_CLOUD_PROJECT, "env-project-one")
        .with_file(WELL_KNOWN_UNIX, SERVICE_ACCOUNT_JSON);
    let mut request = unix_offline();
    request.project = Some("caller-project-one".to_owned());
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(request, CancellationToken::new())
        .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Unconfirmed {
            project: GcpProjectId::new("caller-project-one").unwrap(),
            origin: GcpProjectOrigin::Explicit,
            reason: GcpUncertainty::ProbeDisabled,
        }
    );
}

#[tokio::test]
async fn google_cloud_project_outranks_the_superseded_gcloud_project() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_CLOUD_PROJECT, "current-project-a")
        .with_var(ENV_GCLOUD_PROJECT, "legacy-project-b");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Unconfirmed {
            project: GcpProjectId::new("current-project-a").unwrap(),
            origin: GcpProjectOrigin::EnvironmentVariable(ENV_GOOGLE_CLOUD_PROJECT),
            reason: GcpUncertainty::ProbeDisabled,
        }
    );
}

#[tokio::test]
async fn credential_document_supplies_the_project_when_no_environment_variable_does() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", SERVICE_ACCOUNT_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Unconfirmed {
            project: GcpProjectId::new("pgcp01-fixture").unwrap(),
            origin: GcpProjectOrigin::CredentialDocument,
            reason: GcpUncertainty::ProbeDisabled,
        }
    );
}

#[tokio::test]
async fn cloudsdk_core_project_is_consulted_only_after_the_credential_document() {
    let with_document = home_only()
        .with_var(ENV_CLOUDSDK_CORE_PROJECT, "gcloud-project-c")
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", SERVICE_ACCOUNT_JSON);
    let document_wins = service(with_document, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;
    assert_eq!(
        document_wins.project().project().map(|id| id.as_str()),
        Some("pgcp01-fixture")
    );

    let without_document = home_only().with_var(ENV_CLOUDSDK_CORE_PROJECT, "gcloud-project-c");
    let property_wins = service(without_document, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;
    assert_eq!(
        property_wins.project(),
        &GcpProjectHealth::Unconfirmed {
            project: GcpProjectId::new("gcloud-project-c").unwrap(),
            origin: GcpProjectOrigin::EnvironmentVariable(ENV_CLOUDSDK_CORE_PROJECT),
            reason: GcpUncertainty::ProbeDisabled,
        }
    );
}

#[tokio::test]
async fn ambient_project_is_confirmed_by_the_host_that_reported_it() {
    let profile = service(
        home_only(),
        Arc::new(FakeMetadata::gce(
            "ambient-project-x",
            "projects/12345/zones/us-central1-a",
        )),
    )
    .resolve(unix_probing(), CancellationToken::new())
    .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Confirmed {
            project: GcpProjectId::new("ambient-project-x").unwrap(),
            origin: GcpProjectOrigin::MetadataServer,
        }
    );
    assert_eq!(profile.project().verdict(), GcpHealth::Healthy);
}

#[tokio::test]
async fn configured_project_matching_the_ambient_host_is_confirmed() {
    let environment = home_only().with_var(ENV_GOOGLE_CLOUD_PROJECT, "ambient-project-x");
    let profile = service(
        environment,
        Arc::new(FakeMetadata::gce(
            "ambient-project-x",
            "projects/12345/zones/us-central1-a",
        )),
    )
    .resolve(unix_probing(), CancellationToken::new())
    .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Confirmed {
            project: GcpProjectId::new("ambient-project-x").unwrap(),
            origin: GcpProjectOrigin::EnvironmentVariable(ENV_GOOGLE_CLOUD_PROJECT),
        }
    );
}

#[tokio::test]
async fn configured_project_differing_from_the_ambient_host_is_unconfirmed_not_malformed() {
    let environment = home_only().with_var(ENV_GOOGLE_CLOUD_PROJECT, "chosen-project-y");
    let profile = service(
        environment,
        Arc::new(FakeMetadata::gce(
            "ambient-project-x",
            "projects/12345/zones/us-central1-a",
        )),
    )
    .resolve(unix_probing(), CancellationToken::new())
    .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Unconfirmed {
            project: GcpProjectId::new("chosen-project-y").unwrap(),
            origin: GcpProjectOrigin::EnvironmentVariable(ENV_GOOGLE_CLOUD_PROJECT),
            reason: GcpUncertainty::AmbientAttestationDiffers,
        },
        "targeting a different project than the host runs in is legal, so it is unknown, not wrong"
    );
    assert_eq!(profile.project().verdict(), GcpHealth::Unknown);
}

#[tokio::test]
async fn no_project_source_is_unset_and_that_is_not_the_same_state_as_malformed() {
    let profile = service(home_only(), Arc::new(no_metadata_server()))
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(profile.project(), &GcpProjectHealth::Unset);
    assert_ne!(
        profile.project().code(),
        GcpProjectHealth::Malformed {
            origin: GcpProjectOrigin::Explicit,
            error: GcpProjectIdError::Charset,
        }
        .code()
    );
}

#[tokio::test]
async fn malformed_project_names_the_exact_rule_it_violated() {
    for (value, expected) in [
        ("Sh0rt", GcpProjectIdError::Charset),
        ("abc", GcpProjectIdError::Length),
        ("project-", GcpProjectIdError::TrailingHyphen),
        ("0123456789012345678901", GcpProjectIdError::Number),
    ] {
        let mut request = unix_offline();
        request.project = Some(value.to_owned());
        let profile = service(home_only(), Arc::new(FakeMetadata::unreachable()))
            .resolve(request, CancellationToken::new())
            .await;

        assert_eq!(
            profile.project(),
            &GcpProjectHealth::Malformed {
                origin: GcpProjectOrigin::Explicit,
                error: expected,
            },
            "`{value}` must be rejected by its own rule"
        );
    }
}

#[tokio::test]
async fn a_project_number_is_accepted_alongside_a_project_id() {
    let mut request = unix_offline();
    request.project = Some("464036093014".to_owned());
    let profile = service(home_only(), Arc::new(FakeMetadata::unreachable()))
        .resolve(request, CancellationToken::new())
        .await;

    let project = profile.project().project().expect("project retained");
    assert_eq!(project.kind(), GcpProjectIdKind::Number);
    assert_eq!(project.as_str(), "464036093014");
}

#[tokio::test]
async fn an_undetermined_credential_document_leaves_the_project_undetermined_not_unset() {
    let profile = service(
        heycode_authorization_gcp::testing::MapGcpEnvironment::new(),
        Arc::new(FakeMetadata::unreachable()),
    )
    .resolve(unix_offline(), CancellationToken::new())
    .await;

    assert_eq!(
        profile.project(),
        &GcpProjectHealth::Undetermined {
            reason: GcpUncertainty::ConfigDirectoryUnknown,
        },
        "a document that could not be inspected could still carry a project id"
    );
    assert_eq!(profile.project().verdict(), GcpHealth::Unknown);
}

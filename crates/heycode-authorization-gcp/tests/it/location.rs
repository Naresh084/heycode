//! PGCP01 location health: Vertex AI location precedence and shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_authorization_gcp::{
    ENV_CLOUDSDK_COMPUTE_REGION, ENV_GOOGLE_CLOUD_LOCATION, GcpHealth, GcpLocationError,
    GcpLocationHealth, GcpLocationKind, GcpLocationOrigin,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    FakeMetadata, home_only, no_metadata_server, service, unix_offline, unix_probing,
};

#[tokio::test]
async fn google_cloud_location_outranks_the_gcloud_compute_region() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_CLOUD_LOCATION, "europe-west4")
        .with_var(ENV_CLOUDSDK_COMPUTE_REGION, "us-central1");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.location(),
        &GcpLocationHealth::Selected {
            location: heycode_authorization_gcp::GcpLocation::new("europe-west4").unwrap(),
            origin: GcpLocationOrigin::EnvironmentVariable(ENV_GOOGLE_CLOUD_LOCATION),
        }
    );
}

#[tokio::test]
async fn explicit_request_location_outranks_every_discovered_source() {
    let environment = home_only().with_var(ENV_GOOGLE_CLOUD_LOCATION, "europe-west4");
    let mut request = unix_offline();
    request.location = Some("asia-northeast1".to_owned());
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(request, CancellationToken::new())
        .await;

    assert_eq!(
        profile.location(),
        &GcpLocationHealth::Selected {
            location: heycode_authorization_gcp::GcpLocation::new("asia-northeast1").unwrap(),
            origin: GcpLocationOrigin::Explicit,
        }
    );
}

#[tokio::test]
async fn global_is_a_valid_vertex_location_and_keeps_its_own_kind() {
    let environment = home_only().with_var(ENV_GOOGLE_CLOUD_LOCATION, "global");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    let location = profile.location().location().expect("location retained");
    assert_eq!(location.kind(), GcpLocationKind::Global);
    assert_eq!(profile.location().verdict(), GcpHealth::Healthy);
}

#[tokio::test]
async fn region_shaped_values_across_every_documented_area_are_accepted() {
    for value in [
        "us-central1",
        "europe-west4",
        "asia-northeast3",
        "me-central2",
        "northamerica-northeast1",
        "australia-southeast2",
        "africa-south1",
    ] {
        let environment = home_only().with_var(ENV_GOOGLE_CLOUD_LOCATION, value);
        let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
            .resolve(unix_offline(), CancellationToken::new())
            .await;

        assert!(
            matches!(profile.location(), GcpLocationHealth::Selected { .. }),
            "`{value}` is a documented region shape"
        );
    }
}

#[tokio::test]
async fn a_zone_is_refused_as_a_zone_rather_than_as_a_generic_shape_error() {
    let environment = home_only().with_var(ENV_GOOGLE_CLOUD_LOCATION, "us-central1-a");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.location(),
        &GcpLocationHealth::Malformed {
            origin: GcpLocationOrigin::EnvironmentVariable(ENV_GOOGLE_CLOUD_LOCATION),
            error: GcpLocationError::ZoneNotRegion,
        },
        "pasting a zone where a region belongs deserves its own message"
    );
}

#[tokio::test]
async fn an_uppercase_location_is_refused_on_charset_grounds() {
    let environment = home_only().with_var(ENV_GOOGLE_CLOUD_LOCATION, "US-CENTRAL1");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.location(),
        GcpLocationHealth::Malformed {
            error: GcpLocationError::Charset,
            ..
        }
    ));
}

#[tokio::test]
async fn a_value_that_is_neither_global_nor_region_shaped_fails_on_shape() {
    let environment = home_only().with_var(ENV_GOOGLE_CLOUD_LOCATION, "central");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.location(),
        GcpLocationHealth::Malformed {
            error: GcpLocationError::Shape,
            ..
        }
    ));
}

#[tokio::test]
async fn metadata_zone_supplies_the_region_when_nothing_else_is_configured() {
    let profile = service(
        home_only(),
        Arc::new(FakeMetadata::gce(
            "ambient-project-x",
            "projects/12345/zones/europe-west4-b",
        )),
    )
    .resolve(unix_probing(), CancellationToken::new())
    .await;

    assert_eq!(
        profile.location(),
        &GcpLocationHealth::Selected {
            location: heycode_authorization_gcp::GcpLocation::new("europe-west4").unwrap(),
            origin: GcpLocationOrigin::MetadataZone,
        }
    );
}

#[tokio::test]
async fn no_location_source_is_unset_and_that_is_not_the_same_state_as_malformed() {
    let profile = service(home_only(), Arc::new(no_metadata_server()))
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(profile.location(), &GcpLocationHealth::Unset);
    assert_eq!(profile.location().verdict(), GcpHealth::Unhealthy);
}

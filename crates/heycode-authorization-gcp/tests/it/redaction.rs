//! PGCP01 secret boundary: an ADC document's key material must not reach any
//! formatted value, while the location it came from must.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_authorization_gcp::{ENV_GOOGLE_APPLICATION_CREDENTIALS, GcpAdcOrigin, GcpAuthProfile};
use tokio_util::sync::CancellationToken;

use super::support::{
    AUTHORIZED_USER_JSON, CANARIES, FakeMetadata, SERVICE_ACCOUNT_JSON, WELL_KNOWN_UNIX, home_only,
    service, unix_offline,
};

/// Everything a consumer could reasonably format out of one profile.
fn rendered(profile: &GcpAuthProfile) -> String {
    format!(
        "{profile}|{profile:?}|{}|{:?}|{}|{:?}|{}|{:?}|{}|{}|{}|{}",
        profile.account(),
        profile.account(),
        profile.project(),
        profile.project(),
        profile.location(),
        profile.location(),
        profile.verdict(),
        profile.account().code(),
        profile.project().code(),
        profile.location().code(),
    )
}

#[tokio::test]
async fn a_service_account_document_never_reaches_a_formatted_value() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", SERVICE_ACCOUNT_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    let rendered = rendered(&profile);
    for canary in CANARIES {
        assert!(
            !rendered.contains(canary),
            "`{canary}` leaked into a rendered profile: {rendered}"
        );
    }
}

#[tokio::test]
async fn an_authorized_user_document_never_reaches_a_formatted_value() {
    let environment = home_only().with_file(WELL_KNOWN_UNIX, AUTHORIZED_USER_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    let rendered = rendered(&profile);
    for canary in CANARIES {
        assert!(
            !rendered.contains(canary),
            "`{canary}` leaked into a rendered profile: {rendered}"
        );
    }
}

#[tokio::test]
async fn a_rejected_document_never_echoes_the_bytes_that_were_rejected() {
    let poisoned = format!("not json {}", SERVICE_ACCOUNT_JSON);
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", poisoned);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    let rendered = rendered(&profile);
    for canary in CANARIES {
        assert!(
            !rendered.contains(canary),
            "`{canary}` leaked through a parse failure: {rendered}"
        );
    }
}

#[tokio::test]
async fn the_test_canaries_are_actually_present_in_the_fixtures() {
    // Without this the redaction cases would pass against an empty fixture and
    // prove nothing at all.
    for canary in CANARIES {
        assert!(
            SERVICE_ACCOUNT_JSON.contains(canary) || AUTHORIZED_USER_JSON.contains(canary),
            "`{canary}` is not in any fixture, so no case can catch it leaking"
        );
    }
}

#[tokio::test]
async fn the_origin_reports_where_the_credential_lives_because_that_is_the_repair() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", SERVICE_ACCOUNT_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    let rendered = format!("{}", profile.account());
    assert!(
        rendered.contains(ENV_GOOGLE_APPLICATION_CREDENTIALS),
        "the environment variable that selected the credential is safe and actionable: {rendered}"
    );
    assert!(
        rendered.contains("/keys/sa.json"),
        "the path is a location, not contents: {rendered}"
    );
    assert!(
        rendered.contains("service_account"),
        "the declared credential type is non-secret metadata: {rendered}"
    );
}

#[tokio::test]
async fn the_metadata_origin_reports_the_host_it_asked() {
    let origin = GcpAdcOrigin::MetadataServer {
        host: "metadata.google.internal".to_owned(),
    };
    assert_eq!(
        format!("{origin}"),
        "metadata server metadata.google.internal"
    );
}

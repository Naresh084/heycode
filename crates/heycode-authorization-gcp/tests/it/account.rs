//! PGCP01 account health: Application Default Credentials discovery, origin
//! attribution and the determinate faults that stop the chain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use heycode_authorization_gcp::testing::MapGcpEnvironment;
use heycode_authorization_gcp::{
    ENV_APPDATA, ENV_CLOUDSDK_CONFIG, ENV_GOOGLE_APPLICATION_CREDENTIALS, ENV_SYSTEM_DRIVE,
    GcpAccountHealth, GcpAdcCredentialType, GcpAdcFault, GcpAdcOrigin, GcpHealth, GcpUncertainty,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    AUTHORIZED_USER_JSON, FakeMetadata, SERVICE_ACCOUNT_JSON, WELL_KNOWN_UNIX, home_only,
    metadata_response, service, unix_offline, unix_probing, windows_offline,
};

#[tokio::test]
async fn google_application_credentials_file_is_configured_with_its_environment_origin() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/sa.json")
        .with_file("/keys/sa.json", SERVICE_ACCOUNT_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Configured {
            origin: GcpAdcOrigin::EnvironmentVariable {
                name: ENV_GOOGLE_APPLICATION_CREDENTIALS,
                path: PathBuf::from("/keys/sa.json"),
            },
            credential: Some(GcpAdcCredentialType::ServiceAccount),
        }
    );
}

#[tokio::test]
async fn google_application_credentials_naming_a_missing_file_faults_instead_of_falling_through() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/absent.json")
        .with_file(WELL_KNOWN_UNIX, AUTHORIZED_USER_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Faulted {
            origin: GcpAdcOrigin::EnvironmentVariable {
                name: ENV_GOOGLE_APPLICATION_CREDENTIALS,
                path: PathBuf::from("/keys/absent.json"),
            },
            fault: GcpAdcFault::FileMissing,
        },
        "a named-but-missing credential file must not silently fall through to the gcloud file"
    );
}

#[tokio::test]
async fn unreadable_credential_file_is_a_fault_and_not_an_absence() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/locked.json")
        .with_unreadable("/keys/locked.json");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.account(),
        GcpAccountHealth::Faulted {
            fault: GcpAdcFault::FileUnreadable,
            ..
        }
    ));
    assert_eq!(profile.account().verdict(), GcpHealth::Unhealthy);
}

#[tokio::test]
async fn oversized_credential_file_is_refused_before_it_is_parsed() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/huge.json")
        .with_file("/keys/huge.json", vec![b'x'; 64 * 1024 + 1]);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.account(),
        GcpAccountHealth::Faulted {
            fault: GcpAdcFault::FileTooLarge,
            ..
        }
    ));
}

#[tokio::test]
async fn credential_file_that_is_not_a_json_document_faults_as_unreadable_json() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/bad.json")
        .with_file("/keys/bad.json", "not json at all");
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.account(),
        GcpAccountHealth::Faulted {
            fault: GcpAdcFault::NotJsonObject,
            ..
        }
    ));
}

#[tokio::test]
async fn credential_document_without_a_type_field_is_distinct_from_an_unsupported_type() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/typeless.json")
        .with_file("/keys/typeless.json", r#"{"project_id":"pgcp01-fixture"}"#);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.account(),
        GcpAccountHealth::Faulted {
            fault: GcpAdcFault::TypeMissing,
            ..
        }
    ));
}

#[tokio::test]
async fn credential_document_with_an_undocumented_type_faults_as_unsupported() {
    let environment = home_only()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/odd.json")
        .with_file("/keys/odd.json", r#"{"type":"quantum_account"}"#);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.account(),
        GcpAccountHealth::Faulted {
            fault: GcpAdcFault::TypeUnsupported,
            ..
        }
    ));
}

#[tokio::test]
async fn every_documented_adc_credential_type_is_accepted() {
    for (wire, expected) in [
        ("service_account", GcpAdcCredentialType::ServiceAccount),
        ("authorized_user", GcpAdcCredentialType::AuthorizedUser),
        ("external_account", GcpAdcCredentialType::ExternalAccount),
        (
            "external_account_authorized_user",
            GcpAdcCredentialType::ExternalAccountAuthorizedUser,
        ),
        (
            "impersonated_service_account",
            GcpAdcCredentialType::ImpersonatedServiceAccount,
        ),
        (
            "gdch_service_account",
            GcpAdcCredentialType::GdchServiceAccount,
        ),
    ] {
        let environment = home_only()
            .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, "/keys/any.json")
            .with_file("/keys/any.json", format!(r#"{{"type":"{wire}"}}"#));
        let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
            .resolve(unix_offline(), CancellationToken::new())
            .await;

        assert!(
            matches!(
                profile.account(),
                GcpAccountHealth::Configured { credential: Some(kind), .. } if *kind == expected
            ),
            "documented ADC type `{wire}` must be accepted"
        );
    }
}

#[tokio::test]
async fn well_known_unix_file_is_used_when_the_environment_variable_is_unset() {
    let environment = home_only().with_file(WELL_KNOWN_UNIX, AUTHORIZED_USER_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Configured {
            origin: GcpAdcOrigin::WellKnownFile {
                path: PathBuf::from(WELL_KNOWN_UNIX),
            },
            credential: Some(GcpAdcCredentialType::AuthorizedUser),
        }
    );
}

#[tokio::test]
async fn cloudsdk_config_overrides_the_well_known_directory() {
    let environment = home_only()
        .with_var(ENV_CLOUDSDK_CONFIG, "/opt/gcloud-config")
        .with_file(
            "/opt/gcloud-config/application_default_credentials.json",
            AUTHORIZED_USER_JSON,
        )
        .with_file(WELL_KNOWN_UNIX, SERVICE_ACCOUNT_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Configured {
            origin: GcpAdcOrigin::WellKnownFile {
                path: PathBuf::from("/opt/gcloud-config/application_default_credentials.json"),
            },
            credential: Some(GcpAdcCredentialType::AuthorizedUser),
        }
    );
}

#[tokio::test]
async fn windows_layout_reads_appdata_gcloud_and_not_the_unix_home() {
    let environment = MapGcpEnvironment::new()
        .with_var("HOME", "/home/fixture")
        .with_var(ENV_APPDATA, r"C:\Users\fixture\AppData\Roaming")
        .with_file(WELL_KNOWN_UNIX, SERVICE_ACCOUNT_JSON)
        .with_file(
            r"C:\Users\fixture\AppData\Roaming\gcloud\application_default_credentials.json",
            AUTHORIZED_USER_JSON,
        );
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(windows_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Configured {
            origin: GcpAdcOrigin::WellKnownFile {
                path: PathBuf::from(
                    r"C:\Users\fixture\AppData\Roaming\gcloud\application_default_credentials.json"
                ),
            },
            credential: Some(GcpAdcCredentialType::AuthorizedUser),
        }
    );
}

#[tokio::test]
async fn windows_layout_falls_back_to_the_system_drive_when_appdata_is_unset() {
    let environment = MapGcpEnvironment::new()
        .with_var(ENV_SYSTEM_DRIVE, "D:")
        .with_file(
            r"D:\gcloud\application_default_credentials.json",
            AUTHORIZED_USER_JSON,
        );
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(windows_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Configured {
            origin: GcpAdcOrigin::WellKnownFile {
                path: PathBuf::from(r"D:\gcloud\application_default_credentials.json"),
            },
            credential: Some(GcpAdcCredentialType::AuthorizedUser),
        }
    );
}

#[tokio::test]
async fn missing_unix_home_leaves_the_account_undetermined_rather_than_absent() {
    let environment = MapGcpEnvironment::new();
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert_eq!(
        profile.account(),
        &GcpAccountHealth::Undetermined {
            reason: GcpUncertainty::ConfigDirectoryUnknown,
        },
        "an unlocatable gcloud directory can neither confirm nor rule out a credential"
    );
}

#[tokio::test]
async fn attached_service_account_is_configured_from_the_metadata_origin() {
    let environment = home_only();
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
        profile.account(),
        &GcpAccountHealth::Configured {
            origin: GcpAdcOrigin::MetadataServer {
                host: "metadata.google.internal".to_owned(),
            },
            credential: None,
        }
    );
}

#[tokio::test]
async fn metadata_server_without_a_default_service_account_is_a_determinate_absence() {
    let transport = Arc::new(FakeMetadata::answering(vec![(
        super::support::SERVICE_ACCOUNT_URL,
        metadata_response(404, true, ""),
    )]));
    let profile = service(home_only(), transport)
        .resolve(unix_probing(), CancellationToken::new())
        .await;

    assert_eq!(profile.account(), &GcpAccountHealth::Absent);
    assert_eq!(profile.account().verdict(), GcpHealth::Unhealthy);
}

#[tokio::test]
async fn a_configured_account_is_unknown_because_presence_is_not_authentication() {
    let environment = home_only().with_file(WELL_KNOWN_UNIX, SERVICE_ACCOUNT_JSON);
    let profile = service(environment, Arc::new(FakeMetadata::unreachable()))
        .resolve(unix_offline(), CancellationToken::new())
        .await;

    assert!(matches!(
        profile.account(),
        GcpAccountHealth::Configured { .. }
    ));
    assert_eq!(
        profile.account().verdict(),
        GcpHealth::Unknown,
        "a present credential is not a proven one; only a live exchange could say Healthy"
    );
}

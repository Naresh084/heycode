//! S11 credentials-owned health contribution without secret resolution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::compose;
use heycode_credentials::{credentials_doctor_plugin, credentials_plugin};
use heycode_doctor::{DoctorRegistry, DoctorStatus, SERVICE_DOCTOR, doctor_plugin};
use heycode_settings::{SettingsDocuments, settings_plugin};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn empty_credential_registry_warns_without_exposing_a_value_surface() {
    let plugins = vec![
        doctor_plugin(),
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        credentials_doctor_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let doctor = context.get::<DoctorRegistry>(SERVICE_DOCTOR).unwrap();
    let report = doctor.run(CancellationToken::new()).await.unwrap();

    assert!(
        report.healthy,
        "provider absence is repairable before selection"
    );
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].id.as_str(), "credentials");
    assert_eq!(report.checks[0].status, DoctorStatus::Warning);
    assert_eq!(report.checks[0].code.as_str(), "credentials.no-providers");
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("value"), "{json}");
    assert!(!json.contains("secret"), "{json}");
    context.shutdown();
}

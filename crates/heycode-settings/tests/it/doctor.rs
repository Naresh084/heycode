//! S11 settings-owned health contribution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::compose;
use heycode_doctor::{DoctorRegistry, DoctorStatus, SERVICE_DOCTOR, doctor_plugin};
use heycode_settings::{SettingsDocuments, settings_doctor_plugin, settings_plugin};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn read_only_settings_service_contributes_a_visible_warning() {
    let plugins = vec![
        doctor_plugin(),
        settings_plugin(SettingsDocuments::new()),
        settings_doctor_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let doctor = context.get::<DoctorRegistry>(SERVICE_DOCTOR).unwrap();
    let report = doctor.run(CancellationToken::new()).await.unwrap();

    assert!(report.healthy, "warnings are non-blocking");
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].id.as_str(), "settings");
    assert_eq!(report.checks[0].status, DoctorStatus::Warning);
    assert_eq!(report.checks[0].code.as_str(), "settings.read-only");
    context.shutdown();
}

/// S15: the doctor plane renders settings health, never settings values.
const CANARY_DOCTOR: &str = "sk-canary-doctor-0000000000000020";

#[tokio::test]
async fn doctor_output_never_renders_a_settings_value() {
    let namespace = heycode_settings::SettingsNamespace::new("agent-runtime").unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(namespace, serde_json::json!({"note": CANARY_DOCTOR}))
        .unwrap();
    let plugins = vec![
        doctor_plugin(),
        settings_plugin(documents),
        settings_doctor_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let doctor = context.get::<DoctorRegistry>(SERVICE_DOCTOR).unwrap();
    let report = doctor.run(CancellationToken::new()).await.unwrap();

    let rendered = format!("{report:?}");
    assert!(
        !rendered.contains(CANARY_DOCTOR),
        "a health report must carry no layer value"
    );
    context.shutdown();
}

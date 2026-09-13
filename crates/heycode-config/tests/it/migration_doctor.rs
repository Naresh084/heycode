//! B10 config-owned migration health contribution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{
    ConfigMigrationChange, ConfigMigrationDisposition, ConfigMigrationNotice, ConfigVersionState,
    config_migration_doctor_plugin,
};
use heycode_core::{ContributionKind, compose};
use heycode_doctor::{DoctorRegistry, DoctorStatus, SERVICE_DOCTOR, doctor_plugin};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn current_config_is_pass_and_pending_semantic_plan_is_warning() {
    let current_plugins = vec![doctor_plugin(), config_migration_doctor_plugin(None)];
    let mut current = compose(&current_plugins).unwrap();
    let doctor = current.get::<DoctorRegistry>(SERVICE_DOCTOR).unwrap();
    let report = doctor.run(CancellationToken::new()).await.unwrap();
    assert_eq!(report.checks[0].id.as_str(), "config-migration");
    assert_eq!(report.checks[0].status, DoctorStatus::Pass);
    assert_eq!(report.checks[0].code.as_str(), "config.current");
    assert!(report.checks[0].evidence.is_none());
    current.shutdown();

    let notice = ConfigMigrationNotice {
        path: "/project/heycode.toml".into(),
        from: ConfigVersionState::Older(2),
        to: 3,
        changes: vec![ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin: "agent-options".to_owned(),
            required_by: "agent".to_owned(),
        }],
        disposition: ConfigMigrationDisposition::Pending,
    };
    let pending_plugins = vec![
        doctor_plugin(),
        config_migration_doctor_plugin(Some(notice)),
    ];
    let mut pending = compose(&pending_plugins).unwrap();
    let doctor = pending.get::<DoctorRegistry>(SERVICE_DOCTOR).unwrap();
    let report = doctor.run(CancellationToken::new()).await.unwrap();
    assert!(report.healthy, "pending migration is a visible warning");
    assert_eq!(report.checks[0].status, DoctorStatus::Warning);
    assert_eq!(report.checks[0].code.as_str(), "config.migration-pending");
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("agent-options"), "{json}");
    assert!(json.contains("user_owned_pending"), "{json}");
    let inventory = pending.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "doctor-config"
            && row.kind == ContributionKind::DoctorCheck
            && row.name == "config-migration"
    }));
    pending.shutdown();
}

//! Q17 saved-config migration matrix and downgrade guidance.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{
    CONFIG_SCHEMA_VERSION, Config, ConfigDowngradeGuidance, ConfigMigrationChange,
    ConfigMigrationPlan, ConfigVersionState, MigrationApplyOutcome,
};

const CURRENT_BUILTIN_PROFILE: &[&str] = &[];

const SAVED_CONFIGS: &[(Option<u32>, &str)] = &[
    (None, include_str!("../fixtures/config-matrix/v0.toml")),
    (Some(1), include_str!("../fixtures/config-matrix/v1.toml")),
    (Some(2), include_str!("../fixtures/config-matrix/v2.toml")),
    (Some(3), include_str!("../fixtures/config-matrix/v3.toml")),
    (Some(4), include_str!("../fixtures/config-matrix/v4.toml")),
    (Some(5), include_str!("../fixtures/config-matrix/v5.toml")),
    (Some(6), include_str!("../fixtures/config-matrix/v6.toml")),
    (Some(7), include_str!("../fixtures/config-matrix/v7.toml")),
    (Some(8), include_str!("../fixtures/config-matrix/v8.toml")),
    (Some(9), include_str!("../fixtures/config-matrix/v9.toml")),
    (Some(10), include_str!("../fixtures/config-matrix/v10.toml")),
    (Some(11), include_str!("../fixtures/config-matrix/v11.toml")),
    (Some(12), include_str!("../fixtures/config-matrix/v12.toml")),
    (Some(13), include_str!("../fixtures/config-matrix/v13.toml")),
    (Some(14), include_str!("../fixtures/config-matrix/v14.toml")),
    (Some(15), include_str!("../fixtures/config-matrix/v15.toml")),
    (Some(16), include_str!("../fixtures/config-matrix/v16.toml")),
    (Some(17), include_str!("../fixtures/config-matrix/v17.toml")),
    (Some(18), include_str!("../fixtures/config-matrix/v18.toml")),
    (Some(19), include_str!("../fixtures/config-matrix/v19.toml")),
    (Some(20), include_str!("../fixtures/config-matrix/v20.toml")),
    (Some(21), include_str!("../fixtures/config-matrix/v21.toml")),
    (Some(22), include_str!("../fixtures/config-matrix/v22.toml")),
    (Some(23), include_str!("../fixtures/config-matrix/v23.toml")),
    (Some(24), include_str!("../fixtures/config-matrix/v24.toml")),
    (Some(25), include_str!("../fixtures/config-matrix/v25.toml")),
    (Some(26), include_str!("../fixtures/config-matrix/v26.toml")),
    (Some(27), include_str!("../fixtures/config-matrix/v27.toml")),
    (Some(28), include_str!("../fixtures/config-matrix/v28.toml")),
    (Some(29), include_str!("../fixtures/config-matrix/v29.toml")),
    (Some(30), include_str!("../fixtures/config-matrix/v30.toml")),
    (Some(31), include_str!("../fixtures/config-matrix/v31.toml")),
];

#[test]
fn every_saved_config_schema_upgrades_once_and_then_is_byte_idempotent() {
    assert_eq!(SAVED_CONFIGS.len(), CONFIG_SCHEMA_VERSION as usize + 1);
    for &(version, fixture) in SAVED_CONFIGS {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path, fixture).unwrap();

        let classified = Config::classify_document(fixture).unwrap();
        assert_eq!(
            classified,
            version.map_or(ConfigVersionState::Unversioned, |value| {
                if value == CONFIG_SCHEMA_VERSION {
                    ConfigVersionState::Current(value)
                } else {
                    ConfigVersionState::Older(value)
                }
            })
        );

        if version == Some(CONFIG_SCHEMA_VERSION) {
            assert!(
                ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
                    .unwrap()
                    .is_none()
            );
            let parsed = Config::from_file(&path).unwrap();
            assert_eq!(parsed.schema_version, CONFIG_SCHEMA_VERSION);
            assert!(matches!(
                parsed.downgrade_guidance(CONFIG_SCHEMA_VERSION - 1),
                ConfigDowngradeGuidance::RequiresCompatibleCopy {
                    document_schema: CONFIG_SCHEMA_VERSION,
                    reader_max_schema,
                } if reader_max_schema == CONFIG_SCHEMA_VERSION - 1
            ));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), fixture);
            continue;
        }

        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("every historical saved schema needs an upgrade plan");
        let reader_max_schema = version.unwrap_or(0);
        assert!(matches!(
            plan.downgrade_guidance(reader_max_schema),
            ConfigDowngradeGuidance::RestoreBackup {
                reader_max_schema: actual_reader,
                backup_schema,
                ..
            } if actual_reader == reader_max_schema && backup_schema == version
        ));
        assert!(matches!(
            plan.downgrade_guidance(CONFIG_SCHEMA_VERSION),
            ConfigDowngradeGuidance::Compatible {
                document_schema: CONFIG_SCHEMA_VERSION,
                reader_max_schema: CONFIG_SCHEMA_VERSION,
            }
        ));

        let applied = plan.apply().unwrap();
        let backup = match &applied {
            MigrationApplyOutcome::Applied { backup_path } => backup_path,
            MigrationApplyOutcome::AlreadyApplied { .. } => {
                panic!("first application unexpectedly reported idempotent replay")
            }
        };
        assert_eq!(std::fs::read_to_string(backup).unwrap(), fixture);
        let migrated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            Config::from_file(&path).unwrap().schema_version,
            CONFIG_SCHEMA_VERSION
        );

        assert!(
            ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
                .unwrap()
                .is_none(),
            "schema {version:?} still planned work after migration"
        );
        assert!(matches!(
            plan.apply().unwrap(),
            MigrationApplyOutcome::AlreadyApplied { .. }
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);
    }
}

#[test]
fn newer_saved_config_is_refused_unchanged_with_upgrade_guidance() {
    let fixture = include_str!("../fixtures/config-matrix/v32-newer.toml");
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path, fixture).unwrap();

    let error = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .err()
        .expect("a future config must be refused")
        .to_string();
    assert!(error.contains("newer config schema 32"), "{error}");
    assert!(
        error.contains("upgrade heycode before loading it"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), fixture);
}

#[test]
fn generated_profile_guidance_uses_live_cmd04_rows_without_a_schema_special_case() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(
        &path,
        include_str!("../fixtures/config-v0-setup-generated.toml"),
    )
    .unwrap();
    let live_profile = [
        "session",
        "prompt",
        "tools",
        "llm",
        "approval",
        "commands",
        "agent",
        "mcp-management",
        "plugin-lifecycle",
        "tui",
        "panel-commands",
    ];
    let plan = ConfigMigrationPlan::read(&path, &live_profile)
        .unwrap()
        .expect("the historical generated profile is a frozen default snapshot");
    let activated = plan
        .changes()
        .iter()
        .find_map(|change| match change {
            ConfigMigrationChange::UseBuiltinProfile {
                activated_plugins, ..
            } => Some(activated_plugins.as_slice()),
            _ => None,
        })
        .expect("generated-profile migration must name restored live defaults");
    assert_eq!(
        activated,
        ["mcp-management", "plugin-lifecycle", "panel-commands"]
    );
}

#[test]
fn legacy_auto_keeps_its_behavior_under_full_access_and_current_auto_is_not_migrated() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let original =
        "schema_version = 29\n[approval]\nmode = \"auto\" # keep my explicit permission\n";
    std::fs::write(&path, original).unwrap();
    let explicit = Config::load_paths(
        heycode_config::ConfigPaths {
            explicit: Some(path.clone()),
            ..Default::default()
        },
        CURRENT_BUILTIN_PROFILE,
    )
    .unwrap();
    assert_eq!(
        explicit.config.approval.mode,
        Some(heycode_config::ApprovalMode::FullAccess)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .unwrap();
    assert!(
        plan.changes()
            .contains(&ConfigMigrationChange::RenameLegacyAutoApproval)
    );
    plan.apply().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("mode = \"full_access\" # keep my explicit permission"),
        "{text}"
    );
    assert_eq!(
        Config::from_file(&path).unwrap().approval.mode,
        Some(heycode_config::ApprovalMode::FullAccess)
    );
    assert!(
        ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .is_none()
    );
    std::fs::write(
        &path,
        format!("schema_version = {CONFIG_SCHEMA_VERSION}\n[approval]\nmode = \"auto\"\n"),
    )
    .unwrap();
    assert!(
        ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        Config::from_file(&path).unwrap().approval.mode,
        Some(heycode_config::ApprovalMode::Auto)
    );
}

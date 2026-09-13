//! PL11 QSEC01-independent curated inspection projection.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_extensions::lifecycle::PluginState;
use heycode_extensions::{
    ApiVersion, Architecture, ContributionKind, CuratedPluginInspector, ManifestValidator,
    OperatingSystem, PlatformTarget, PluginExecutionKind, PluginGenerationState,
    PluginInspectionPackage, PluginInspectorError, PluginPermission, PluginVersion,
};

fn manifest() -> heycode_extensions::PluginManifest {
    ManifestValidator::new(
        ApiVersion::new(1).unwrap(),
        PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
    )
    .validate_toml(
        r#"schema_version = 1
id = "acme/inspector"
name = "Inspector"
version = "1.2.3"
description = "plugin-description-private-canary"
license = "MIT"
default_enabled = true
requested_permissions = ["filesystem_read", "network_access", "credential_use"]
platforms = [{ os = "macos", architecture = "aarch64" }]
dependencies = []
conflicts = []

[[contributions]]
kind = "command"
id = "private-command-name-canary"
path = "commands/private-host-path-canary.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "command"
id = "second-command"
path = "commands/second.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "skill"
id = "review"
path = "skills/review/SKILL.md"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "private-source-locator-canary"
revision = "1.2.3"
update_channel = "pinned"

[authentication]
policy = "required"
credentials = [
  { reference = "acme/inspector/private-credential-reference-canary", kind = "api-key" },
]
"#,
    )
    .unwrap()
}

#[test]
fn curated_report_contains_only_closed_facts_and_counts() {
    let manifest = manifest();
    let package = PluginInspectionPackage::new(
        &manifest,
        PluginExecutionKind::WasiComponent,
        PluginGenerationState::Active,
        [PluginPermission::FilesystemRead],
    )
    .unwrap();
    let state = PluginState {
        id: manifest.id().clone(),
        active: manifest.version().clone(),
        previous: Some(PluginVersion::parse("1.2.2").unwrap()),
        enabled: true,
    };
    let report = CuratedPluginInspector::build([state], [package]).unwrap();
    let rows = report.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id().as_str(), "acme/inspector");
    assert_eq!(rows[0].version().as_str(), "1.2.3");
    assert!(rows[0].enabled());
    assert!(rows[0].rollback_available());
    assert_eq!(rows[0].execution(), PluginExecutionKind::WasiComponent);
    assert_eq!(rows[0].generation(), PluginGenerationState::Active);
    assert_eq!(
        rows[0]
            .contributions()
            .iter()
            .map(|row| (row.kind, row.count))
            .collect::<Vec<_>>(),
        [(ContributionKind::Skill, 1), (ContributionKind::Command, 2),]
    );
    assert_eq!(
        rows[0].requested_permissions(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::NetworkAccess,
            PluginPermission::CredentialUse,
        ]
    );
    assert_eq!(
        rows[0].granted_permissions(),
        [PluginPermission::FilesystemRead]
    );

    let rendered = report.render_model_text();
    let debug = format!("{report:?}");
    assert!(rendered.contains("acme/inspector 1.2.3 enabled"));
    assert!(rendered.contains("command=2"));
    assert!(rendered.contains("skill=1"));
    for forbidden in [
        "plugin-description-private-canary",
        "private-command-name-canary",
        "commands/private-host-path-canary.json",
        "private-source-locator-canary",
        "private-credential-reference-canary",
        "sha256:",
    ] {
        assert!(!rendered.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
}

#[test]
fn foreign_grants_missing_packages_and_version_drift_fail_loud() {
    let manifest = manifest();
    assert!(matches!(
        PluginInspectionPackage::new(
            &manifest,
            PluginExecutionKind::NativeProcess,
            PluginGenerationState::Unknown,
            [PluginPermission::ProcessSpawn],
        ),
        Err(PluginInspectorError::UnrequestedGrant(
            PluginPermission::ProcessSpawn
        ))
    ));

    let package = PluginInspectionPackage::new(
        &manifest,
        PluginExecutionKind::Declarative,
        PluginGenerationState::Inactive,
        [],
    )
    .unwrap();
    let drifted = PluginState {
        id: manifest.id().clone(),
        active: PluginVersion::parse("9.9.9").unwrap(),
        previous: None,
        enabled: false,
    };
    assert!(matches!(
        CuratedPluginInspector::build([drifted], [package]),
        Err(PluginInspectorError::VersionMismatch)
    ));

    let missing = PluginState {
        id: manifest.id().clone(),
        active: manifest.version().clone(),
        previous: None,
        enabled: false,
    };
    assert!(matches!(
        CuratedPluginInspector::build([missing], []),
        Err(PluginInspectorError::MissingPackage)
    ));
}

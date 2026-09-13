//! External plugin manifest v1 boundary and collision contracts.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_extensions::{
    ApiVersion, Architecture, ContributionKey, ContributionKind, ManifestError, ManifestValidator,
    OperatingSystem, PLUGIN_MANIFEST_SCHEMA_VERSION, PlatformTarget, PluginCodeRuntime,
    PluginPermission,
};

const CHECKSUM: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const VALID_MANIFEST: &str = include_str!("../fixtures/plugin-v1.toml");

fn validator() -> ManifestValidator {
    ManifestValidator::new(
        ApiVersion::new(1).unwrap(),
        PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
    )
}

fn valid_manifest(extra: &str) -> String {
    format!("{VALID_MANIFEST}\n{extra}\n")
}

#[test]
fn valid_v1_manifest_projects_only_validated_secret_free_metadata() {
    let manifest = validator().validate_toml(&valid_manifest("")).unwrap();

    assert_eq!(manifest.schema_version(), PLUGIN_MANIFEST_SCHEMA_VERSION);
    assert_eq!(manifest.id().as_str(), "acme/code-quality");
    assert_eq!(manifest.version().as_str(), "1.2.3-beta.1+build.7");
    assert_eq!(manifest.permissions().len(), 3);
    assert!(
        manifest
            .permissions()
            .contains(&PluginPermission::CredentialUse)
    );
    assert_eq!(
        manifest.contributions()[0].public_name(),
        "acme/code-quality::review"
    );
    assert_eq!(
        manifest.contributions()[1].public_name(),
        "acme/code-quality::quality"
    );
    assert!(manifest.code().is_none());

    let rendered = serde_json::to_string(&manifest).unwrap();
    assert!(rendered.contains("credential_references"));
    for forbidden in ["api_key", "access_token", "client_secret", "password"] {
        assert!(
            !rendered.contains(forbidden),
            "rendered secret field {forbidden}"
        );
    }
}

#[test]
fn code_runtime_and_entrypoint_are_typed_manifest_authority() {
    let native = validator()
        .validate_toml(&valid_manifest(
            r#"[code]
runtime = "native_process"
entrypoint = "bin/plugin""#,
        ))
        .unwrap();
    let code = native.code().unwrap();
    assert_eq!(code.runtime(), PluginCodeRuntime::NativeProcess);
    assert_eq!(code.entrypoint().as_str(), "bin/plugin");

    let component = validator()
        .validate_toml(&valid_manifest(
            r#"[code]
runtime = "wasi_component_v1"
entrypoint = "components/plugin.wasm""#,
        ))
        .unwrap();
    assert_eq!(
        component.code().unwrap().runtime(),
        PluginCodeRuntime::WasiComponentV1
    );

    for invalid in [
        valid_manifest(
            r#"[code]
runtime = "native_library"
entrypoint = "bin/plugin""#,
        ),
        valid_manifest(
            r#"[code]
runtime = "native_process"
entrypoint = "../bin/plugin""#,
        ),
        valid_manifest(
            r#"[code]
runtime = "wasi_component_v1"
entrypoint = "/tmp/plugin.wasm""#,
        ),
    ] {
        assert!(validator().validate_toml(&invalid).is_err());
    }
}

#[test]
fn schema_and_unknown_fields_fail_before_publication() {
    let unsupported = valid_manifest("").replacen("schema_version = 1", "schema_version = 2", 1);
    assert!(matches!(
        validator().validate_toml(&unsupported),
        Err(ManifestError::UnsupportedSchema { found: 2, .. })
    ));

    let canary = "never-print-this-secret";
    let unknown = valid_manifest(&format!("api_key = \"{canary}\""));
    let error = validator().validate_toml(&unknown).unwrap_err();
    assert!(matches!(error, ManifestError::InvalidDocument));
    assert!(!error.to_string().contains(canary));

    for unknown in [
        valid_manifest("").replace("kind = \"skill\"", "kind = \"native_library\""),
        valid_manifest("").replace(
            "\"filesystem_read\", \"network_access\"",
            "\"filesystem_read\", \"terminal_takeover\", \"network_access\"",
        ),
    ] {
        assert!(matches!(
            validator().validate_toml(&unknown),
            Err(ManifestError::InvalidDocument)
        ));
    }

    for missing in [
        valid_manifest("").replace(
            "requested_permissions = [\"filesystem_read\", \"network_access\", \"credential_use\"]\n",
            "",
        ),
        valid_manifest("").replace("exposure = { mode = \"namespaced\" }\n", ""),
    ] {
        assert!(matches!(
            validator().validate_toml(&missing),
            Err(ManifestError::InvalidDocument)
        ));
    }
}

#[test]
fn explicit_empty_permissions_and_no_auth_are_valid_for_a_declarative_skill() {
    let raw = r#"schema_version = 1
id = "local/readme"
name = "Readme helper"
version = "1.0.0"
description = "A local declarative skill."
license = "MIT"
default_enabled = true
requested_permissions = []
platforms = [{ os = "macos", architecture = "aarch64" }]
dependencies = []
conflicts = []
contributions = [{ kind = "skill", id = "readme", path = "skills/readme/SKILL.md", exposure = { mode = "namespaced" } }]

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "packages/readme"
update_channel = "pinned"
revision = "v1.0.0"

[authentication]
policy = "none"
credentials = []
"#;
    let manifest = validator().validate_toml(raw).unwrap();
    assert!(manifest.permissions().is_empty());
    assert!(manifest.authentication().credential_references.is_empty());
}

#[test]
fn incompatible_api_and_platform_fail_loud() {
    let too_new = valid_manifest("").replacen("minimum = 1", "minimum = 2", 1);
    assert!(matches!(
        validator().validate_toml(&too_new),
        Err(ManifestError::IncompatibleApi { .. })
    ));

    let linux_only =
        valid_manifest("").replace("  { os = \"macos\", architecture = \"aarch64\" },\n", "");
    assert!(matches!(
        validator().validate_toml(&linux_only),
        Err(ManifestError::UnsupportedPlatform { .. })
    ));
}

#[test]
fn ids_versions_paths_and_dependency_relationships_are_strict() {
    for invalid in [
        valid_manifest("").replacen("acme/code-quality", "not-namespaced", 1),
        valid_manifest("").replacen("1.2.3-beta.1+build.7", "01.2.3", 1),
        valid_manifest("").replacen("skills/review/SKILL.md", "../SKILL.md", 1),
        valid_manifest("").replacen("skills/review/SKILL.md", "commands/SKILL.md", 1),
        valid_manifest("").replacen("acme/shared", "acme/code-quality", 1),
        valid_manifest("").replacen("2.0.0", "0.5.0", 1),
    ] {
        assert!(
            validator().validate_toml(&invalid).is_err(),
            "accepted:\n{invalid}"
        );
    }
}

#[test]
fn package_paths_are_portable_relative_paths_on_every_supported_host() {
    for invalid in [
        valid_manifest("").replace(
            "schemas/config.schema.json",
            "C:/outside/config.schema.json",
        ),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/CON/SKILL.md"),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/COM0/SKILL.md"),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/COM¹/SKILL.md"),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/com⁹/SKILL.md"),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/LPT⁰/SKILL.md"),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/lpt².txt/SKILL.md"),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/review./SKILL.md"),
        valid_manifest("").replace("skills/review/SKILL.md", "skills/review/file:name"),
        valid_manifest("")
            .replace("kind = \"https\"", "kind = \"local\"")
            .replace(
                "https://plugins.example.test/acme/code-quality.tar.zst",
                "C:/outside/package",
            ),
    ] {
        assert!(
            validator().validate_toml(&invalid).is_err(),
            "accepted non-portable path:\n{invalid}"
        );
    }

    let case_collision = valid_manifest(
        r#"[[contributions]]
kind = "skill"
id = "review-copy"
path = "skills/REVIEW/skill.md"
exposure = { mode = "namespaced" }"#,
    );
    assert!(matches!(
        validator().validate_toml(&case_collision),
        Err(ManifestError::DuplicateField {
            field: "contributions.path"
        })
    ));
}

#[test]
fn duplicate_permissions_platforms_credentials_paths_and_contributions_fail() {
    let cases = [
        valid_manifest("").replace(
            "\"filesystem_read\", \"network_access\"",
            "\"filesystem_read\", \"filesystem_read\", \"network_access\"",
        ),
        valid_manifest("").replace(
            "  { os = \"linux\", architecture = \"x86_64\" },",
            "  { os = \"macos\", architecture = \"aarch64\" },",
        ),
        valid_manifest("").replace(
            "credentials = [{ reference = \"acme/code-quality/api\", kind = \"api-key\" }]",
            "credentials = [{ reference = \"acme/code-quality/api\", kind = \"api-key\" }, { reference = \"acme/code-quality/api\", kind = \"oauth-token\" }]",
        ),
        valid_manifest("").replace("commands/quality.toml", "skills/review/SKILL.md"),
        valid_manifest(
            r#"[[contributions]]
kind = "skill"
id = "review"
path = "skills/review-copy/SKILL.md"
exposure = { mode = "namespaced" }"#,
        ),
    ];
    for invalid in cases {
        assert!(
            validator().validate_toml(&invalid).is_err(),
            "accepted:\n{invalid}"
        );
    }
}

#[test]
fn auth_contains_references_only_and_requires_permission() {
    let without_permission = valid_manifest("").replace(
        "\"network_access\", \"credential_use\"",
        "\"network_access\"",
    );
    assert!(matches!(
        validator().validate_toml(&without_permission),
        Err(ManifestError::MissingPermission {
            permission: PluginPermission::CredentialUse,
            ..
        })
    ));

    let secret = valid_manifest("").replace(
        "kind = \"api-key\" }",
        "kind = \"api-key\", value = \"never-print-this-secret\" }",
    );
    let error = validator().validate_toml(&secret).unwrap_err();
    assert!(matches!(error, ManifestError::InvalidDocument));
    assert!(!error.to_string().contains("never-print-this-secret"));
}

#[test]
fn occupied_namespaced_contribution_fails_loud() {
    let occupied =
        ContributionKey::new(ContributionKind::Skill, "acme/code-quality::review").unwrap();
    let validator = validator().with_occupied([occupied]);
    assert!(matches!(
        validator.validate_toml(&valid_manifest("")),
        Err(ManifestError::ContributionCollision {
            kind: ContributionKind::Skill,
            ..
        })
    ));
}

#[test]
fn override_requires_permission_and_exact_registry_authorization() {
    let raw = valid_manifest("").replacen(
        "exposure = { mode = \"namespaced\" }",
        "exposure = { mode = \"override\", name = \"review\" }",
        1,
    );
    assert!(matches!(
        validator().validate_toml(&raw),
        Err(ManifestError::MissingPermission {
            permission: PluginPermission::ContributionOverride,
            ..
        })
    ));

    let raw = raw.replace(
        "\"credential_use\"]",
        "\"credential_use\", \"contribution_override\"]",
    );
    assert!(matches!(
        validator().validate_toml(&raw),
        Err(ManifestError::OverrideNotAllowed { .. })
    ));

    let key = ContributionKey::new(ContributionKind::Skill, "review").unwrap();
    let manifest = validator()
        .with_occupied([key.clone()])
        .with_allowed_overrides([key])
        .validate_toml(&raw)
        .unwrap();
    assert_eq!(manifest.contributions()[0].public_name(), "review");
}

#[test]
fn duplicate_local_id_fails_even_when_public_exposure_differs() {
    let raw = valid_manifest(
        r#"[[contributions]]
kind = "skill"
id = "review"
path = "skills/review-override/SKILL.md"
exposure = { mode = "override", name = "host-review" }"#,
    )
    .replace(
        "\"credential_use\"]",
        "\"credential_use\", \"contribution_override\"]",
    );
    let key = ContributionKey::new(ContributionKind::Skill, "host-review").unwrap();
    assert!(matches!(
        validator()
            .with_allowed_overrides([key])
            .validate_toml(&raw),
        Err(ManifestError::DuplicateField {
            field: "contributions.id"
        })
    ));
}

#[test]
fn batch_validation_is_atomic_across_plugin_ids_and_public_contributions() {
    let first = valid_manifest("");
    let duplicate_id = first.replace("version = \"1.2.3-beta.1+build.7\"", "version = \"1.3.0\"");
    assert!(matches!(
        validator().validate_batch_toml(&[&first, &duplicate_id]),
        Err(ManifestError::DuplicatePluginId)
    ));

    let override_one = first
        .replace(
            "\"credential_use\"]",
            "\"credential_use\", \"contribution_override\"]",
        )
        .replacen(
            "exposure = { mode = \"namespaced\" }",
            "exposure = { mode = \"override\", name = \"shared-review\" }",
            1,
        );
    let override_two = override_one.replace("acme/code-quality", "other/code-quality");
    let key = ContributionKey::new(ContributionKind::Skill, "shared-review").unwrap();
    assert!(matches!(
        validator()
            .with_allowed_overrides([key])
            .validate_batch_toml(&[&override_one, &override_two]),
        Err(ManifestError::ContributionCollision { .. })
    ));
}

#[test]
fn source_metadata_rejects_unpinned_pinned_channels_and_secret_bearing_urls() {
    let unpinned = valid_manifest("")
        .replace(&format!("checksum = \"{CHECKSUM}\"\n"), "")
        .replace("update_channel = \"stable\"", "update_channel = \"pinned\"");
    assert!(validator().validate_toml(&unpinned).is_err());

    let secret_url = valid_manifest("").replace(
        "https://plugins.example.test/acme/code-quality.tar.zst",
        "https://user:never-print-this-secret@plugins.example.test/plugin",
    );
    let error = validator().validate_toml(&secret_url).unwrap_err();
    assert!(!error.to_string().contains("never-print-this-secret"));

    for invalid in [
        valid_manifest("").replace("plugins.example.test", "plugins.example.test:65536"),
        valid_manifest("")
            .replace("kind = \"https\"", "kind = \"git\"")
            .replace(
                "https://plugins.example.test/acme/code-quality.tar.zst",
                "git@:repository",
            ),
    ] {
        assert!(validator().validate_toml(&invalid).is_err());
    }
}

#[test]
fn ed25519_signature_metadata_requires_one_canonical_64_byte_signature() {
    for replacement in [
        "YWJjZA==",
        "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5eg==",
        "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYR==",
    ] {
        let invalid = valid_manifest("").replace(
            "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYQ==",
            replacement,
        );
        assert!(validator().validate_toml(&invalid).is_err());
    }
}

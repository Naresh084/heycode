//! Versioned configuration and lossless migration contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{
    CONFIG_SCHEMA_VERSION, Config, ConfigMigrationChange, ConfigMigrationDisposition,
    ConfigMigrationPlan, ConfigSource, ConfigVersionState, MigrationApplyOutcome,
};

const CURRENT_BUILTIN_PROFILE: &[&str] = &[
    "doctor",
    "doctor-config",
    "trust",
    "ui",
    "settings",
    "doctor-settings",
    "settings-aws-bedrock",
    "settings-google-inference",
    "http",
    "sandbox",
    "subprocess-local",
    "shell-local",
    "filesystem-local",
    "credentials",
    "credentials-env",
    "credentials-command",
    "credentials-file",
    "doctor-credentials",
    "authorization",
    "secret-prompt",
    "authorization-api-key",
    "provider-openrouter",
    "onboarding",
    "session",
    "attachments-local",
    "session-query-jsonl",
    "prompt",
    "native-tools",
    "native-openai",
    "native-anthropic",
    "native-openrouter",
    "web",
    "web-portable",
    "web-extract",
    "web-policy",
    "tools",
    "native-tool-policy",
    "models",
    "runtimes",
    "runtime-claude",
    "runtime-codex",
    "mcp-registry",
    "catalog-cache-file",
    "catalog-deepseek",
    "catalog-openrouter",
    "llm",
    "agent-options",
    "approval",
    "commands",
    "status",
    "status-web",
    "init",
    "skills",
    "mcp",
    "compactions",
    "subagent",
    "plan",
    "agent",
    "product-hook-attachments",
    "agent-attachments",
    "agent-documents",
    "runtime-native",
    "app-server",
    "routing",
    "routing-auth",
    "app-server-controls",
    "tui",
];

const GENERATED_V0: &str = include_str!("../fixtures/config-v0-setup-generated.toml");
const CUSTOM_V0: &str = include_str!("../fixtures/config-v0-custom-profile.toml");

const DEEPSEEK_DEFAULT_V1: &str = r#"schema_version = 1

[llm]
provider = "deepseek"
model = "deepseek-chat" # historical setup default

[tools]
bash_timeout_ms = 30000
read_max_bytes = 262144
read_max_lines = 2000
"#;

#[test]
fn missing_old_current_and_newer_versions_classify_deterministically() {
    assert_eq!(
        Config::classify_document("[llm]\nprovider = \"openrouter\"\n").unwrap(),
        ConfigVersionState::Unversioned
    );
    assert_eq!(
        Config::classify_document("schema_version = 0\n").unwrap(),
        ConfigVersionState::Older(0)
    );
    assert_eq!(
        Config::classify_document(&format!("schema_version = {CONFIG_SCHEMA_VERSION}\n")).unwrap(),
        ConfigVersionState::Current(CONFIG_SCHEMA_VERSION)
    );
    assert_eq!(
        Config::classify_document(&format!("schema_version = {}\n", CONFIG_SCHEMA_VERSION + 1))
            .unwrap(),
        ConfigVersionState::Newer(CONFIG_SCHEMA_VERSION + 1)
    );
}

#[test]
fn generated_profile_preview_names_capabilities_restored_by_builtin_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, GENERATED_V0).unwrap();

    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("unversioned file needs migration");
    assert_eq!(plan.from(), ConfigVersionState::Unversioned);
    assert_eq!(plan.to(), CONFIG_SCHEMA_VERSION);
    assert_eq!(plan.path(), std::fs::canonicalize(path).unwrap());

    let profile_change = plan
        .changes()
        .iter()
        .find_map(|change| match change {
            ConfigMigrationChange::UseBuiltinProfile {
                frozen_plugins,
                activated_plugins,
            } => Some((frozen_plugins, activated_plugins)),
            ConfigMigrationChange::SetSchemaVersion { .. }
            | ConfigMigrationChange::AddRequiredProfilePlugin { .. }
            | ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin { .. }
            | ConfigMigrationChange::RenameLegacyAutoApproval
            | ConfigMigrationChange::UseHomeCredentialStore
            | ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { .. } => None,
        })
        .expect("known generated profile must be recognized");
    assert_eq!(
        profile_change.0,
        &[
            "session", "prompt", "tools", "llm", "approval", "commands", "agent", "tui"
        ]
    );
    assert_eq!(
        profile_change.1,
        &[
            "doctor",
            "doctor-config",
            "trust",
            "ui",
            "settings",
            "doctor-settings",
            "settings-aws-bedrock",
            "settings-google-inference",
            "http",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "credentials",
            "credentials-env",
            "credentials-command",
            "credentials-file",
            "doctor-credentials",
            "authorization",
            "secret-prompt",
            "authorization-api-key",
            "provider-openrouter",
            "onboarding",
            "attachments-local",
            "session-query-jsonl",
            "native-tools",
            "native-openai",
            "native-anthropic",
            "native-openrouter",
            "web",
            "web-portable",
            "web-extract",
            "web-policy",
            "native-tool-policy",
            "models",
            "runtimes",
            "runtime-claude",
            "runtime-codex",
            "mcp-registry",
            "catalog-cache-file",
            "catalog-deepseek",
            "catalog-openrouter",
            "agent-options",
            "status",
            "status-web",
            "init",
            "skills",
            "mcp",
            "compactions",
            "subagent",
            "plan",
            "product-hook-attachments",
            "agent-attachments",
            "agent-documents",
            "runtime-native",
            "app-server",
            "routing",
            "routing-auth",
            "app-server-controls"
        ]
    );
}

#[test]
fn generated_profile_migration_backs_up_is_atomic_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, GENERATED_V0).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .unwrap();

    let backup_path = match plan.apply().unwrap() {
        MigrationApplyOutcome::Applied { backup_path } => backup_path,
        other => panic!("first apply must write the migration: {other:?}"),
    };
    assert_eq!(std::fs::read_to_string(&backup_path).unwrap(), GENERATED_V0);

    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(
        migrated.contains(&format!("schema_version = {CONFIG_SCHEMA_VERSION}")),
        "{migrated}"
    );
    assert!(!migrated.contains("[profile]"), "{migrated}");
    let parsed = Config::from_file(&path).unwrap();
    assert!(
        parsed.profile.plugins.is_empty(),
        "empty means use the current built-in profile"
    );

    assert_eq!(
        plan.apply().unwrap(),
        MigrationApplyOutcome::AlreadyApplied { backup_path }
    );
    assert!(
        ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .is_none(),
        "a migrated file must not plan another migration"
    );
}

#[test]
fn v1_deepseek_setup_default_migrates_to_current_schema_with_backup_and_comment_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, DEEPSEEK_DEFAULT_V1).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v1 requires migration");
    assert_eq!(plan.from(), ConfigVersionState::Older(1));
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { from, to }
            if from == "deepseek-chat" && to == "deepseek-v4-flash"
    )));

    let backup_path = match plan.apply().unwrap() {
        MigrationApplyOutcome::Applied { backup_path } => backup_path,
        other => panic!("first apply must write the migration: {other:?}"),
    };
    assert_eq!(backup_path.file_name().unwrap(), "config.toml.v1.bak");
    assert_eq!(
        std::fs::read_to_string(&backup_path).unwrap(),
        DEEPSEEK_DEFAULT_V1
    );
    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("model = \"deepseek-v4-flash\" # historical setup default"));
    assert_eq!(
        Config::from_file(&path).unwrap().llm.model,
        "deepseek-v4-flash"
    );
    assert_eq!(
        plan.apply().unwrap(),
        MigrationApplyOutcome::AlreadyApplied { backup_path }
    );
}

#[test]
fn migration_preserves_nondefault_or_non_deepseek_model_intent() {
    for (provider, model) in [
        ("deepseek", "deepseek-reasoner"),
        ("openrouter", "deepseek-chat"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!("schema_version = 1\n[llm]\nprovider = \"{provider}\"\nmodel = \"{model}\"\n"),
        )
        .unwrap();
        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("schema v1 requires migration");
        assert!(
            plan.changes().iter().all(|change| !matches!(
                change,
                ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { .. }
            )),
            "unexpected model rewrite for {provider}/{model}"
        );
        plan.apply().unwrap();
        assert_eq!(Config::from_file(&path).unwrap().llm.model, model);
    }
}

#[test]
fn migration_does_not_rewrite_a_customized_v1_deepseek_pin() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 1
[llm]
provider = "deepseek"
model = "deepseek-chat"

[future_extension]
preserve_me = true
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v1 requires migration");
    assert!(plan.changes().iter().all(|change| !matches!(
        change,
        ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { .. }
    )));
    plan.apply().unwrap();
    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("model = \"deepseek-chat\""));
    assert!(migrated.contains("[future_extension]\npreserve_me = true"));
}

#[test]
fn custom_profile_and_unknown_content_are_preserved_byte_for_byte_where_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, CUSTOM_V0).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .unwrap();

    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin { plugin, required_by }
            if plugin == "shell-local" && required_by == "tools"
    )));
    plan.apply().unwrap();

    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("# This comment and the custom composition are user-owned."));
    assert!(migrated.contains(
        "plugins = [\"session\", \"prompt\", \"workspace-scope\", \"sandbox\", \"subprocess-local\", \"shell-local\", \"filesystem-local\", \"native-tools\", \"native-openrouter\", \"web\", \"web-portable\", \"tools\", \"http\", \"models\", \"llm\", \"catalog-openrouter\"]"
    ));
    assert!(migrated.contains("[future_extension]\npreserve_me = true"));
    let parsed = Config::from_file(&path).unwrap();
    assert_eq!(
        parsed.profile.plugins,
        [
            "session",
            "prompt",
            "workspace-scope",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "native-openrouter",
            "web",
            "web-portable",
            "tools",
            "http",
            "models",
            "llm",
            "catalog-openrouter",
        ]
    );
}

#[test]
fn v2_custom_agent_profile_materializes_its_new_explicit_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 2
# Keep this exact custom capability set.
[profile]
plugins = ["session", "prompt", "tools", "llm", "approval", "commands", "skills", "agent", "tui"]

[llm]
provider = "openrouter"
model = "custom/model"
"#;
    std::fs::write(&path, custom).unwrap();

    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v2 requires the explicit agent-options migration");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "agent-options" && required_by == "agent"
    )));
    plan.apply().unwrap();

    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("# Keep this exact custom capability set."));
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "session",
            "prompt",
            "workspace-scope",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "native-openrouter",
            "web",
            "web-portable",
            "tools",
            "http",
            "llm",
            "approval",
            "commands",
            "skills",
            "agent-options",
            "models",
            "catalog-openrouter",
            "token-counters",
            "compactions",
            "agent",
            "ui",
            "runtimes",
            "profiles",
            "session-query-jsonl",
            "settings",
            "runtime-native",
            "routing",
            "app-server",
            "tui",
        ]
    );
}

#[test]
fn v3_custom_tui_profile_materializes_its_new_ui_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 3
[profile]
plugins = ["session", "prompt", "tools", "llm", "tui"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v3 requires the explicit ui migration");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "ui" && required_by == "tui"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "session",
            "prompt",
            "workspace-scope",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "web",
            "web-portable",
            "tools",
            "llm",
            "ui",
            "runtimes",
            "profiles",
            "commands",
            "session-query-jsonl",
            "settings",
            "models",
            "runtime-native",
            "routing",
            "app-server",
            "tui"
        ]
    );
}

#[test]
fn v4_custom_tui_profile_materializes_its_runtime_registry_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 4
[profile]
plugins = ["session", "prompt", "tools", "llm", "ui", "tui"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v4 requires the explicit runtimes migration");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "runtimes" && required_by == "tui"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "session",
            "prompt",
            "workspace-scope",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "web",
            "web-portable",
            "tools",
            "llm",
            "ui",
            "runtimes",
            "profiles",
            "commands",
            "session-query-jsonl",
            "settings",
            "models",
            "runtime-native",
            "routing",
            "app-server",
            "tui"
        ]
    );
}

#[test]
fn v5_custom_tui_profile_materializes_its_routing_service_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 5
[profile]
plugins = ["session", "prompt", "tools", "llm", "ui", "runtimes", "tui"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v5 requires the explicit routing migration");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "routing" && required_by == "tui"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "session",
            "prompt",
            "workspace-scope",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "web",
            "web-portable",
            "tools",
            "llm",
            "ui",
            "runtimes",
            "profiles",
            "commands",
            "session-query-jsonl",
            "settings",
            "models",
            "runtime-native",
            "routing",
            "app-server",
            "tui"
        ]
    );
}

#[test]
fn v6_custom_tools_profile_materializes_shell_and_subprocess_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 6
[profile]
plugins = ["session", "prompt", "tools", "llm"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v6 requires explicit execution dependencies");
    for (plugin, required_by) in [
        ("shell-local", "tools"),
        ("subprocess-local", "shell-local"),
        ("sandbox", "subprocess-local"),
        ("filesystem-local", "tools"),
    ] {
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: actual,
                required_by: owner,
            } if actual == plugin && owner == required_by
        )));
    }
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "session",
            "prompt",
            "workspace-scope",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "web",
            "web-portable",
            "tools",
            "llm"
        ]
    );
}

#[test]
fn v7_custom_mcp_profile_materializes_subprocess_and_sandbox_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 7
[profile]
plugins = ["tools", "mcp"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v7 requires common process policy dependencies");
    for (plugin, required_by) in [
        ("shell-local", "tools"),
        ("subprocess-local", "shell-local"),
        ("sandbox", "subprocess-local"),
        ("filesystem-local", "tools"),
        ("mcp-registry", "mcp"),
    ] {
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: actual,
                required_by: owner,
            } if actual == plugin && owner == required_by
        )));
    }
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "session",
            "workspace-scope",
            "sandbox",
            "subprocess-local",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "web",
            "web-portable",
            "tools",
            "mcp-registry",
            "mcp"
        ]
    );
}

#[test]
fn v8_custom_tools_profile_materializes_the_filesystem_provider() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 8
[profile]
plugins = ["sandbox", "subprocess-local", "shell-local", "tools"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v8 requires the filesystem service dependency");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "filesystem-local" && required_by == "tools"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "sandbox",
            "subprocess-local",
            "session",
            "workspace-scope",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "web",
            "web-portable",
            "tools"
        ]
    );
}

#[test]
fn v9_custom_mcp_profile_materializes_the_registry_service() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 9
[profile]
plugins = ["sandbox", "subprocess-local", "shell-local", "filesystem-local", "tools", "mcp"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v9 requires the MCP registry dependency");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "mcp-registry" && required_by == "mcp"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "sandbox",
            "subprocess-local",
            "session",
            "workspace-scope",
            "shell-local",
            "filesystem-local",
            "native-tools",
            "web",
            "web-portable",
            "tools",
            "mcp-registry",
            "mcp"
        ]
    );
}

#[test]
fn v10_custom_profile_advances_without_injecting_optional_new_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 10
[profile]
plugins = ["session", "prompt"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v10 advances to the current default-capability schema");
    assert_eq!(
        plan.changes(),
        [ConfigMigrationChange::SetSchemaVersion {
            from: Some(10),
            to: CONFIG_SCHEMA_VERSION,
        }]
    );
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        ["session", "prompt"]
    );
}

#[test]
fn v10_custom_agent_profile_materializes_the_catalog_service_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 10
[profile]
plugins = ["session", "prompt", "agent-options", "agent"]
"#;
    std::fs::write(&path, custom).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v10 agent profile needs the explicit catalog dependency");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "models" && required_by == "agent"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "session",
            "prompt",
            "agent-options",
            "models",
            "token-counters",
            "compactions",
            "native-tools",
            "agent"
        ]
    );
}

#[test]
fn v11_custom_api_key_profile_preserves_openrouter_flow_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 11
[profile]
plugins = ["credentials", "authorization", "secret-prompt", "authorization-api-key", "onboarding"]
"#;
    std::fs::write(&path, custom).unwrap();

    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("schema v11 combined API-key ownership needs the OpenRouter provider plugin");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin {
            flow,
            from_plugin,
            to_plugin,
        } if flow == "openrouter-api-key"
            && from_plugin == "authorization-api-key"
            && to_plugin == "provider-openrouter"
    )));

    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "credentials",
            "authorization",
            "secret-prompt",
            "authorization-api-key",
            "provider-openrouter",
            "onboarding",
        ]
    );
}

#[test]
fn v12_openrouter_profile_materializes_catalog_for_strict_llm() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let custom = r#"schema_version = 12
[profile]
plugins = ["http", "models", "provider-openrouter", "llm"]

[llm]
provider = "openrouter"
model = "z-ai/glm-5.3-flash"
"#;
    std::fs::write(&path, custom).unwrap();

    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("strict OpenRouter dispatch requires its catalog source");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "catalog-openrouter" && required_by == "llm"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "http",
            "models",
            "provider-openrouter",
            "native-tools",
            "native-openrouter",
            "llm",
            "catalog-openrouter",
        ]
    );
}

#[test]
fn v13_tool_profile_materializes_native_tool_registry_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"schema_version = 13
[profile]
plugins = ["filesystem-local", "subprocess-local", "shell-local", "tools"]
"#,
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("tools now consumes the native-tool registry");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "native-tools" && required_by == "tools"
    )));
    plan.apply().unwrap();
    let plugins = Config::from_file(&path).unwrap().profile.plugins;
    let native = plugins
        .iter()
        .position(|plugin| plugin == "native-tools")
        .unwrap();
    let tools = plugins.iter().position(|plugin| plugin == "tools").unwrap();
    assert!(native < tools);
}

#[test]
fn v14_openrouter_profile_materializes_provider_native_web_contribution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"schema_version = 14
[profile]
plugins = ["native-tools", "llm"]

[llm]
provider = "openrouter"
model = "z-ai/glm-5.3-flash"
"#,
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("OpenRouter strict profiles now own a native web candidate");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "native-openrouter" && required_by == "llm"
    )));
    plan.apply().unwrap();
    assert_eq!(
        Config::from_file(&path).unwrap().profile.plugins,
        [
            "native-tools",
            "native-openrouter",
            "http",
            "models",
            "llm",
            "catalog-openrouter"
        ]
    );
}

#[test]
fn v15_web_enabled_tools_profile_materializes_registry_and_portable_provider() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"schema_version = 15
[profile]
plugins = ["native-tools", "tools"]

[web]
enabled = true
"#,
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("enabled web Consumers now require their provider registry");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "web" && required_by == "tools"
    )));
    plan.apply().unwrap();
    let plugins = Config::from_file(&path).unwrap().profile.plugins;
    let web = plugins.iter().position(|plugin| plugin == "web").unwrap();
    let portable = plugins
        .iter()
        .position(|plugin| plugin == "web-portable")
        .unwrap();
    let tools = plugins.iter().position(|plugin| plugin == "tools").unwrap();
    assert!(web < portable && portable < tools);
}

#[test]
fn v16_tui_profile_materializes_stable_app_server_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"schema_version = 16
[profile]
plugins = ["agent-options", "models", "agent", "runtimes", "runtime-native", "routing", "ui", "tui"]
"#,
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("TUI now requires the stable local app-server");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "app-server" && required_by == "tui"
    )));
    plan.apply().unwrap();
    let plugins = Config::from_file(&path).unwrap().profile.plugins;
    let runtime = plugins
        .iter()
        .position(|plugin| plugin == "runtime-native")
        .unwrap();
    let app_server = plugins
        .iter()
        .position(|plugin| plugin == "app-server")
        .unwrap();
    let tui = plugins.iter().position(|plugin| plugin == "tui").unwrap();
    assert!(runtime < app_server && app_server < tui);
}

#[test]
fn a_newer_schema_fails_before_partial_loading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            "schema_version = {}\n[llm]\nprovider = \"openrouter\"\n",
            CONFIG_SCHEMA_VERSION + 1
        ),
    )
    .unwrap();

    let err = Config::from_file(&path).unwrap_err().to_string();
    assert!(err.contains("newer config schema"), "{err}");
    assert!(
        err.contains(&(CONFIG_SCHEMA_VERSION + 1).to_string()),
        "{err}"
    );
}

#[test]
fn explicit_custom_profile_is_reported_but_never_rewritten_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom.toml");
    std::fs::write(&path, CUSTOM_V0).unwrap();

    let loaded = Config::load_for_startup(Some(&path), CURRENT_BUILTIN_PROFILE).unwrap();

    assert_eq!(loaded.source, ConfigSource::Explicit(path.clone()));
    let notice = loaded.migration.expect("legacy document needs a notice");
    assert_eq!(notice.disposition, ConfigMigrationDisposition::Pending);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CUSTOM_V0);
    assert_eq!(
        loaded.config.profile.plugins,
        ["session", "prompt", "tools", "llm"]
    );
    assert!(!path.with_file_name("custom.toml.unversioned.bak").exists());
}

#[test]
fn apply_refuses_source_or_backup_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, GENERATED_V0).unwrap();
    let stale_plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .unwrap();
    std::fs::write(&path, format!("{GENERATED_V0}\n# concurrent edit\n")).unwrap();
    let source_error = stale_plan.apply().unwrap_err().to_string();
    assert!(source_error.contains("changed after"), "{source_error}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("concurrent edit")
    );

    std::fs::write(&path, GENERATED_V0).unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .unwrap();
    let backup = path.with_file_name("config.toml.unversioned.bak");
    std::fs::write(&backup, "not this config").unwrap();
    let backup_error = plan.apply().unwrap_err().to_string();
    assert!(
        backup_error.contains("different contents"),
        "{backup_error}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), GENERATED_V0);
}

/// v18 -> v19: an exact profile that already selects OpenAI gains the
/// provider's own authorization flow and catalog, ordered after their
/// dependencies — the same rule v17 -> v18 applied to Anthropic.
#[test]
fn v18_openai_profile_gains_the_provider_owned_flow_and_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"schema_version = 18
[llm]
provider = "openai"
model = "gpt-5.6-sol"
[profile]
plugins = ["credentials", "authorization", "http", "models", "llm", "agent"]
"#,
    )
    .unwrap();

    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("an OpenAI profile now needs the provider-owned plugins");
    for expected in ["provider-openai", "catalog-openai"] {
        assert!(
            plan.changes().iter().any(|change| matches!(
                change,
                ConfigMigrationChange::AddRequiredProfilePlugin { plugin, .. }
                    if plugin == expected
            )),
            "migration must add `{expected}`"
        );
    }
    plan.apply().unwrap();

    let plugins = Config::from_file(&path).unwrap().profile.plugins;
    let at = |name: &str| plugins.iter().position(|plugin| plugin == name);
    let authorization = at("authorization").unwrap();
    let provider = at("provider-openai").unwrap();
    let llm = at("llm").unwrap();
    let catalog = at("catalog-openai").unwrap();
    assert!(
        authorization < provider,
        "the flow must follow the authorization registry it contributes to"
    );
    assert!(
        llm < catalog,
        "the catalog must follow the llm service it publishes into"
    );
}

/// The other plugins added to the default set in the same change publish
/// services nothing injects, so an exact profile that never asked for them is
/// left exactly as it was. Migrating in a plugin nothing depends on is a change
/// users pay for and nobody uses.
#[test]
fn v18_leaves_a_profile_alone_when_it_selects_neither_openai_nor_anthropic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = r#"schema_version = 18
[llm]
provider = "deepseek"
model = "deepseek-chat"
[profile]
plugins = ["credentials", "authorization", "http", "models", "llm", "agent"]
"#;
    std::fs::write(&path, original).unwrap();

    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE).unwrap();
    let plugins = plan.map_or_else(
        || Config::from_file(&path).unwrap().profile.plugins,
        |plan| {
            plan.apply().unwrap();
            Config::from_file(&path).unwrap().profile.plugins
        },
    );
    for absent in [
        "provider-openai",
        "catalog-openai",
        "authorization-aws",
        "authorization-gcp",
        "provider-lmstudio",
        "telemetry-local-off",
    ] {
        assert!(
            !plugins.iter().any(|plugin| plugin == absent),
            "`{absent}` must not be migrated into a profile that never asked for it"
        );
    }
}

/// v19 -> v20: whichever Agent Consumer appears first gets the newly required
/// token-counter service immediately before it, exactly once.
#[test]
fn v19_agent_profiles_gain_token_counters_before_the_first_consumer() {
    for (name, consumers) in [
        ("agent-only", vec!["agent"]),
        ("subagent-first", vec!["subagent", "agent"]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let plugins = consumers
            .iter()
            .map(|plugin| format!("\"{plugin}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            &path,
            format!("schema_version = 19\n[profile]\nplugins = [\"models\", {plugins}]\n"),
        )
        .unwrap();

        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("the Agent now requires token counters");
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin,
                required_by,
            } if plugin == "token-counters" && required_by == consumers[0]
        )));
        plan.apply().unwrap();

        let migrated = Config::from_file(&path).unwrap().profile.plugins;
        let token_index = migrated
            .iter()
            .position(|plugin| plugin == "token-counters")
            .unwrap();
        let consumer_index = migrated
            .iter()
            .position(|plugin| plugin == consumers[0])
            .unwrap();
        assert_eq!(
            migrated
                .iter()
                .filter(|plugin| plugin.as_str() == "token-counters")
                .count(),
            1,
            "{name}: migration must not duplicate the provider"
        );
        assert!(
            token_index < consumer_index,
            "{name}: token counters must compose before the first Consumer: {migrated:?}"
        );
    }
}

#[test]
fn v20_tui_profile_gains_the_shared_picker_service_and_command_registry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "schema_version = 20\n[profile]\nplugins = [\"ui\", \"app-server\", \"runtimes\", \"routing\", \"tui\"]\n",
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("the real profile picker adds explicit TUI dependencies");
    for expected in ["profiles", "commands"] {
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin,
                required_by,
            } if plugin == expected && required_by == "tui"
        )));
    }
    plan.apply().unwrap();
    let plugins = Config::from_file(&path).unwrap().profile.plugins;
    let tui = plugins.iter().position(|plugin| plugin == "tui").unwrap();
    for expected in ["profiles", "commands"] {
        assert!(
            plugins
                .iter()
                .position(|plugin| plugin == expected)
                .unwrap()
                < tui,
            "{expected} must compose before TUI: {plugins:?}"
        );
    }
}

#[test]
fn v21_agent_profiles_gain_compactions_before_the_first_consumer() {
    for (name, consumers, expected_consumer) in [
        ("agent-only", vec!["agent"], "agent"),
        ("subagent-first", vec!["subagent", "agent"], "subagent"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let consumers = consumers
            .iter()
            .map(|plugin| format!("\"{plugin}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            &path,
            format!(
                "schema_version = 21\n[profile]\nplugins = [\"token-counters\", {consumers}]\n"
            ),
        )
        .unwrap();
        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("C12 adds one explicit strategy-registry dependency");
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin,
                required_by,
            } if plugin == "compactions" && required_by == expected_consumer
        )));
        plan.apply().unwrap();
        let plugins = Config::from_file(&path).unwrap().profile.plugins;
        let compactions = plugins
            .iter()
            .position(|plugin| plugin == "compactions")
            .unwrap();
        let consumer = plugins
            .iter()
            .position(|plugin| plugin == expected_consumer)
            .unwrap();
        assert!(compactions < consumer, "{name}: {plugins:?}");
    }
}

#[test]
fn v22_tui_profile_gains_session_query_before_the_new_lifecycle_consumer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "schema_version = 22\n[profile]\nplugins = [\"session\", \"commands\", \"ui\", \"profiles\", \"settings\", \"app-server\", \"runtimes\", \"routing\", \"tui\"]\n",
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("U15 makes session-query-jsonl a required TUI dependency");
    assert!(plan.changes().iter().any(|change| matches!(
        change,
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } if plugin == "session-query-jsonl" && required_by == "tui"
    )));
    plan.apply().unwrap();
    let config = Config::from_file(&path).unwrap();
    assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
    let query = config
        .profile
        .plugins
        .iter()
        .position(|plugin| plugin == "session-query-jsonl")
        .unwrap();
    let tui = config
        .profile
        .plugins
        .iter()
        .position(|plugin| plugin == "tui")
        .unwrap();
    assert!(query < tui, "{:?}", config.profile.plugins);
}

#[test]
fn v23_provider_policy_profiles_gain_settings_before_their_owner() {
    for (provider, owner, model) in [
        ("openai", "provider-openai", "gpt-5.6-sol"),
        ("anthropic", "provider-anthropic", "claude-opus-5"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!(
                "schema_version = 23\n[llm]\nprovider = \"{provider}\"\nmodel = \"{model}\"\n[profile]\nplugins = [\"authorization\", \"{owner}\", \"http\", \"models\", \"llm\"]\n"
            ),
        )
        .unwrap();
        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("provider policy namespace adds an explicit Settings dependency");
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin,
                required_by,
            } if plugin == "settings" && required_by == owner
        )));
        plan.apply().unwrap();
        let plugins = Config::from_file(&path).unwrap().profile.plugins;
        let settings = plugins
            .iter()
            .position(|plugin| plugin == "settings")
            .unwrap();
        let owner = plugins.iter().position(|plugin| plugin == owner).unwrap();
        assert!(settings < owner, "{provider}: {plugins:?}");
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("counter.toml");
    std::fs::write(
        &path,
        "schema_version = 23\n[profile]\nplugins = [\"http\", \"credentials\", \"token-counters\", \"token-count-anthropic\"]\n",
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("the Anthropic counter shares the provider-owned policy namespace");
    plan.apply().unwrap();
    let plugins = Config::from_file(&path).unwrap().profile.plugins;
    let counter = plugins
        .iter()
        .position(|plugin| plugin == "token-count-anthropic")
        .unwrap();
    for dependency in ["authorization", "settings", "provider-anthropic"] {
        assert!(
            plugins
                .iter()
                .position(|plugin| plugin == dependency)
                .unwrap()
                < counter,
            "{dependency}: {plugins:?}"
        );
    }
}

#[test]
fn v24_subagent_profiles_gain_jobs_and_selected_delegated_runtime_bridges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "schema_version = 24\n[profile]\nplugins = [\"runtimes\", \"runtime-codex\", \"runtime-claude\", \"subagent\", \"agent\"]\n",
    )
    .unwrap();
    let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
        .unwrap()
        .expect("O05/R05/R08 add exact selected-profile bridges");
    for expected in ["subagent-codex", "subagent-claude", "subagent-jobs"] {
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin { plugin, .. }
                if plugin == expected
        )));
    }
    plan.apply().unwrap();
    let config = Config::from_file(&path).unwrap();
    assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
    let plugins = config.profile.plugins;
    for expected in ["subagent-codex", "subagent-claude", "subagent-jobs"] {
        assert!(
            plugins.iter().any(|plugin| plugin == expected),
            "{plugins:?}"
        );
    }
}

#[test]
fn v27_provider_policy_profiles_gain_their_settings_backed_activation_plugins() {
    for (provider, native) in [
        ("openai", "native-openai"),
        ("anthropic", "native-anthropic"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!(
                "schema_version = 27\n[llm]\nprovider = \"{provider}\"\nmodel = \"fixture\"\n[profile]\nplugins = [\"settings\", \"native-tools\", \"llm\"]\n"
            ),
        )
        .unwrap();
        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("settings-backed provider policy requires a schema migration");
        assert!(plan.changes().iter().any(|change| matches!(
            change,
            ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin,
                required_by,
            } if plugin == native && required_by == "llm"
        )));
        plan.apply().unwrap();
        let plugins = Config::from_file(&path).unwrap().profile.plugins;
        assert!(
            plugins.iter().position(|plugin| plugin == native).unwrap()
                < plugins.iter().position(|plugin| plugin == "llm").unwrap(),
            "{provider}: {plugins:?}"
        );
    }

    for (inference, settings_owner) in [
        ("inference-bedrock-converse", "settings-aws-bedrock"),
        ("inference-google-gemini", "settings-google-inference"),
        ("inference-google-vertex", "settings-google-inference"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!("schema_version = 27\n[profile]\nplugins = [\"settings\", \"{inference}\"]\n"),
        )
        .unwrap();
        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("cloud inference requires its provider-owned policy plugin");
        plan.apply().unwrap();
        let plugins = Config::from_file(&path).unwrap().profile.plugins;
        assert!(
            plugins
                .iter()
                .position(|plugin| plugin == settings_owner)
                .unwrap()
                < plugins
                    .iter()
                    .position(|plugin| plugin == inference)
                    .unwrap(),
            "{inference}: {plugins:?}"
        );
    }
}

#[test]
fn keychain_profiles_migrate_to_one_file_store_without_touching_credentials() {
    for plugins in [
        r#"["settings", "credentials", "credentials-keychain"]"#,
        r#"["settings", "credentials", "credentials-keychain", "credentials-file"]"#,
        r#"["settings", "credentials", "credentials-file", "credentials-keychain"]"#,
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let original = format!(
            "schema_version = 28\n# keep me\n[profile]\nplugins = {plugins}\n[llm]\nmodel = \"my-model\"\n"
        );
        std::fs::write(&path, &original).unwrap();
        let plan = ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
            .unwrap()
            .expect("the retired keychain selection needs a migration");
        plan.apply().unwrap();
        let config = Config::from_file(&path).unwrap();
        assert_eq!(
            config.profile.plugins,
            ["settings", "credentials", "credentials-file"]
        );
        assert_eq!(config.llm.model, "my-model");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# keep me")
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("config.toml.v28.bak")).unwrap(),
            original
        );
        assert!(!root.path().join("credentials.toml").exists());
        assert!(
            ConfigMigrationPlan::read(&path, CURRENT_BUILTIN_PROFILE)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn repeated_home_startups_do_not_rewrite_or_report_the_same_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "schema_version = 29\n[llm]\nprovider = \"openrouter\"\nmodel = \"anthropic/claude-sonnet-4\"\n";
    std::fs::write(&path, original).unwrap();
    let load = || {
        Config::load_paths(
            heycode_config::ConfigPaths {
                home: Some(path.clone()),
                ..Default::default()
            },
            CURRENT_BUILTIN_PROFILE,
        )
        .unwrap()
    };
    assert!(matches!(
        load().migration.unwrap().disposition,
        ConfigMigrationDisposition::Applied { .. }
    ));
    let migrated = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    for _ in 0..3 {
        let loaded = load();
        assert!(loaded.migration.is_none());
        assert_eq!(loaded.config.llm.model, "anthropic/claude-sonnet-4");
        assert_eq!(std::fs::read(&path).unwrap(), migrated);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.toml.v29.bak")).unwrap(),
        original
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

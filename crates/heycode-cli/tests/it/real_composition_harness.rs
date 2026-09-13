//! Q01 shared real-composition harness contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_cli::testing::RealCompositionHarness;

#[test]
fn shared_harness_boots_the_complete_default_product_world_through_the_real_loader() {
    let harness = RealCompositionHarness::new().unwrap();
    assert!(harness.sessions_dir().starts_with(harness.root()));
    assert!(harness.attachments_dir().starts_with(harness.root()));
    assert!(harness.settings_path().starts_with(harness.root()));
    assert!(harness.credentials_root().starts_with(harness.root()));
    assert!(harness.catalog_cache_path().starts_with(harness.root()));

    let world = harness.compose().unwrap();
    let context = world.context();
    assert_eq!(
        context.plugins(),
        [
            "doctor",
            "doctor-config",
            "trust",
            "ui",
            "settings-file",
            "doctor-settings",
            "settings-aws-bedrock",
            "settings-google-inference",
            "http-reqwest",
            "sandbox",
            "subprocess-local",
            "session",
            "workspace-scope",
            "terminal-registry",
            "shell-local",
            "hooks",
            "filesystem-local",
            "retained-output-local",
            "lsp-registry",
            "credentials",
            "credentials-env",
            "credentials-command",
            "credentials-file",
            "doctor-credentials",
            "authorization",
            "secret-prompt",
            "authorization-api-key",
            "provider-openrouter",
            "provider-anthropic",
            "provider-openai",
            "authorization-aws",
            "authorization-gcp",
            "provider-lmstudio",
            "onboarding",
            "profiles",
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
            "interactive-tools",
            "lsp-tools",
            "native-tool-policy",
            "models",
            "runtimes",
            "runtime-claude",
            "runtime-codex",
            "runtime-opencode",
            "runtime-grok",
            "runtime-deepseek-harness",
            "mcp-registry",
            "mcp-management",
            "plugin-lifecycle",
            "telemetry-local-off",
            "telemetry-metrics",
            "catalog-cache-file",
            "catalog-overrides",
            "catalog-deepseek",
            "catalog-openrouter",
            "catalog-compatible",
            "catalog-ollama",
            "catalog-anthropic",
            "catalog-openai",
            "catalog-google",
            "catalog-google-vertex",
            "catalog-google-claude-vertex",
            "catalog-azure-openai",
            "catalog-custom-openai",
            "catalog-minimax",
            "catalog-minimax-token-plan",
            "catalog-zai",
            "catalog-zai-coding",
            "catalog-lmstudio",
            "catalog-bedrock",
            "token-counters",
            "token-count-anthropic",
            "llm",
            "provider-activation",
            "request-transforms",
            "request-transforms-openrouter",
            "provider-telemetry",
            "agent-options",
            "approval",
            "commands",
            "lmstudio-control",
            "status",
            "status-context",
            "status-web",
            "health-history",
            "init",
            "skills",
            "mcp",
            "compactions",
            "subagent",
            "subagent-codex",
            "subagent-claude",
            "product-extensions",
            "config-import-resources",
            "plan",
            "agent",
            "advisor",
            "product-hook-attachments",
            "subagent-jobs",
            "execution-jobs",
            "goals",
            "workflows",
            "schedules",
            "teams",
            "work",
            "reviewer",
            "deferred-tools",
            "code-mode",
            "loop-budget-settings",
            "agent-attachments",
            "agent-documents",
            "runtime-native",
            "app-server",
            "routing",
            "routing-auth",
            "app-server-controls",
            "workspace-transitions",
            "config-import",
            "memory-commands",
            "autocompact",
            "tui",
            "panel-commands",
        ]
    );
    assert_eq!(context.plugin_descriptors().len(), context.plugins().len());
    assert!(
        context
            .get::<heycode_agent::AdvisorService>(heycode_agent::SERVICE_ADVISOR)
            .is_some()
    );
    let advisor_namespace = heycode_settings::SettingsNamespace::new("advisor").unwrap();
    assert!(
        context
            .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
            .unwrap()
            .get(&advisor_namespace)
            .unwrap()
            .is_some()
    );
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for (kind, name) in [
        (heycode_core::ContributionKind::SettingsNamespace, "advisor"),
        (heycode_core::ContributionKind::Tool, "advisor"),
        (
            heycode_core::ContributionKind::InterceptionLayer,
            "agent/request:advisor",
        ),
    ] {
        assert!(
            inventory
                .contributions
                .iter()
                .any(|row| { row.plugin == "advisor" && row.kind == kind && row.name == name })
        );
    }
    assert!(context.services().len() >= 20);
    let ui = context
        .get::<heycode_ui::UiRegistry>(heycode_ui::SERVICE_UI)
        .unwrap();
    for id in ["diff", "jobs", "agents"] {
        assert!(
            ui.snapshot().unwrap().iter().any(|row| {
                row.slot() == heycode_ui::UiSlot::SidePanel && row.id().as_str() == id
            })
        );
    }
    let subagents = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let provider_ids = subagents
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.id().as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(provider_ids, ["native", "codex", "claude"]);
    assert!(subagents.background_available());
    world.shutdown();
}

#[cfg(unix)]
#[test]
fn enabled_cached_declarative_package_reaches_the_production_skill_registry() {
    use std::os::unix::fs::PermissionsExt as _;

    use heycode_extensions::{
        ApiVersion, Architecture, ManifestValidator, OperatingSystem, PlatformTarget,
        PluginInstallCache,
    };

    let harness = RealCompositionHarness::new().unwrap();
    let source = harness.root().join("extension-source");
    std::fs::create_dir_all(source.join(".heycode-plugin")).unwrap();
    std::fs::create_dir_all(source.join("skills")).unwrap();
    let os = if cfg!(target_os = "macos") {
        OperatingSystem::Macos
    } else if cfg!(target_os = "freebsd") {
        OperatingSystem::Freebsd
    } else {
        OperatingSystem::Linux
    };
    let architecture = if cfg!(target_arch = "aarch64") {
        Architecture::Aarch64
    } else {
        Architecture::X86_64
    };
    let os_name = match os {
        OperatingSystem::Macos => "macos",
        OperatingSystem::Linux => "linux",
        OperatingSystem::Windows => "windows",
        OperatingSystem::Freebsd => "freebsd",
    };
    let architecture_name = match architecture {
        Architecture::Aarch64 => "aarch64",
        Architecture::X86_64 => "x86_64",
    };
    std::fs::write(
        source.join(".heycode-plugin/plugin.toml"),
        format!(
            r#"schema_version = 1
id = "acme/reachable"
name = "Reachable"
version = "1.0.0"
description = "Production composition reachability fixture."
license = "MIT"
default_enabled = true
requested_permissions = []
platforms = [{{ os = "{os_name}", architecture = "{architecture_name}" }}]
dependencies = []
conflicts = []

[[contributions]]
kind = "skill"
id = "reachable"
path = "skills/reachable.md"
exposure = {{ mode = "namespaced" }}

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixture/reachable"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#
        ),
    )
    .unwrap();
    std::fs::write(
        source.join("skills/reachable.md"),
        "---\nname: ignored\ndescription: production route\n---\nReachable body.\n",
    )
    .unwrap();
    std::fs::create_dir_all(harness.credentials_root()).unwrap();
    std::fs::set_permissions(
        harness.credentials_root(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let cache = PluginInstallCache::open(
        harness.credentials_root().join("plugins"),
        ManifestValidator::new(
            ApiVersion::new(1).unwrap(),
            PlatformTarget::new(os, architecture),
        ),
    )
    .unwrap();
    cache.install_directory(&source).unwrap();
    std::fs::write(
        harness.settings_path(),
        "schema_version = 1\n\n[settings.plugins.installed.\"acme/reachable\"]\nactive = \"1.0.0\"\nenabled = true\n",
    )
    .unwrap();
    std::fs::set_permissions(
        harness.settings_path(),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    let world = harness.compose().unwrap();
    let skills = world
        .context()
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    assert!(
        skills
            .snapshot()
            .unwrap()
            .iter()
            .any(|skill| skill.name == "acme/reachable::reachable")
    );
    assert!(
        world
            .context()
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| {
                row.plugin == "product-extensions"
                    && row.kind == heycode_core::ContributionKind::Skill
            })
    );
    world.shutdown();
    assert!(skills.snapshot().unwrap().is_empty());
}

#[test]
fn connection_catalog_keeps_openrouter_and_exposes_new_provider_flows_and_catalogs() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    let routes = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let authorization = context
        .get::<heycode_authorization::AuthorizationService>(
            heycode_authorization::SERVICE_AUTHORIZATION,
        )
        .unwrap();
    let models = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    for provider in [
        "openrouter",
        "fireworks",
        "groq",
        "mistral",
        "together",
        "xai",
        "bedrock",
    ] {
        let profile = routes
            .connection_profiles()
            .iter()
            .find(|profile| profile.registry_name == provider)
            .expect("connection profile");
        assert!(
            authorization.descriptors().unwrap().iter().any(|flow| Some(
                flow.query.reference.as_str()
            ) == profile
                .credential_reference
                .as_deref()),
            "{provider} authorization"
        );
        assert!(
            models
                .descriptors()
                .unwrap()
                .iter()
                .any(|row| row.id == provider),
            "{provider} catalog"
        );
    }
}

#[test]
fn local_connections_have_catalogs_without_invented_models_or_required_api_keys() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    let routes = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let models = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    for id in ["lmstudio", "ollama"] {
        let profile = routes
            .connection_profiles()
            .iter()
            .find(|profile| profile.registry_name == id)
            .unwrap();
        assert_eq!(profile.family, heycode_llm::ConnectionFamily::Local);
        assert!(profile.default_model.is_none());
        assert!(!heycode_cli::provider_requires_credential(id));
        assert!(
            models.descriptors().unwrap().iter().any(|row| row.id == id),
            "{id} must be discoverable before it is selected"
        );
    }
}

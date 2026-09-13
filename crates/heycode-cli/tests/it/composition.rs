//! Composition-root regressions: config-driven plugin order, factory registry,
//! and the two plugins that shipped documented-but-unmounted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_cli::testing::RealCompositionHarness;
use heycode_cli::{
    BUILTIN_SERVICE_KEYS, WorldOptions, compose_world, probe_world_activation,
    resolve_world_plugins,
};
use heycode_config::Config;
use heycode_core::{PluginContributionKind as Kind, PluginSource};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{Provider, StreamChunk};

fn fake() -> Arc<dyn Provider> {
    Arc::new(FakeProvider::new(vec![vec![
        StreamChunk::TextDelta("ok".into()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]]))
}

fn opts<'a>(cfg: &'a Config, dir: &std::path::Path) -> WorldOptions<'a> {
    WorldOptions {
        config: cfg,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir,
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.to_path_buf(),
        attachments_dir: dir.join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: dir.join("settings.toml"),
        credentials_root: dir.join("credentials-home"),
        catalog_cache_path: dir.join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.to_path_buf(),
        fake: Some(fake()),
        resume: None,
    }
}

/// AGENTS.md §3 lists a `"plan"` service key and §11 calls plan mode DONE, but
/// `plan_plugin()` was never added to the composition root — so `/plan`,
/// `exit_plan_mode` and the guard existed only inside their own test.
#[test]
fn default_world_mounts_plan_mode() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::defaults();
    let ctx = compose_world(&opts(&cfg, dir.path())).unwrap();

    assert!(
        ctx.get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
            .is_some(),
        "service `plan` missing from a default world"
    );
    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert!(
        tools.names().iter().any(|n| n == "exit_plan_mode"),
        "exit_plan_mode not registered: {:?}",
        tools.names()
    );
    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    assert!(
        commands.get("plan").unwrap().is_some(),
        "/plan not registered"
    );
}

#[test]
fn explicit_worktree_base_composes_runtime_specific_isolated_providers() {
    fn git(repository: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let state = root.path().join("state");
    std::fs::create_dir_all(&repository).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    git(&repository, &["init", "--quiet"]);
    git(
        &repository,
        &["config", "user.email", "heycode@example.invalid"],
    );
    git(&repository, &["config", "user.name", "heycode test"]);
    std::fs::write(repository.join("tracked.txt"), "base\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "--quiet", "-m", "base"]);
    let base = git(&repository, &["rev-parse", "HEAD"]);

    let mut cfg = Config::defaults();
    cfg.subagent.worktree_base = Some(base);
    let mut options = opts(&cfg, &repository);
    options.sessions_dir = state.join("sessions");
    options.attachments_dir = state.join("attachments");
    options.settings_user_path = state.join("settings.toml");
    options.credentials_root = state.join("home");
    options.catalog_cache_path = state.join("catalog.json");
    let mut context = compose_world(&options).unwrap();

    let subagents = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let ids = subagents
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.id().as_str().to_owned())
        .collect::<Vec<_>>();
    for expected in ["worktree-codex", "worktree-claude", "worktree-opencode"] {
        assert!(ids.iter().any(|id| id == expected), "{ids:?}");
    }
    for plugin in [
        "subagent-worktree-codex",
        "subagent-worktree-claude",
        "subagent-worktree-opencode",
    ] {
        assert!(context.plugins().contains(&plugin));
    }
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for (plugin, provider) in [
        ("subagent-worktree-codex", "worktree-codex"),
        ("subagent-worktree-claude", "worktree-claude"),
        ("subagent-worktree-opencode", "worktree-opencode"),
    ] {
        assert!(inventory.contributions.iter().any(|row| {
            row.plugin == plugin
                && row.kind == heycode_core::ContributionKind::SubagentProvider
                && row.name == provider
        }));
    }
    context.shutdown();
}

#[test]
fn non_commit_worktree_base_fails_before_composition_effects() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults();
    cfg.subagent.worktree_base = Some("HEAD".to_owned());
    let error = match resolve_world_plugins(&opts(&cfg, root.path())) {
        Ok(_) => panic!("mutable worktree base must fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("worktree base"), "{error:#}");
    assert!(!root.path().join("credentials-home/worktree-codex").exists());
}

#[test]
fn default_world_mounts_all_three_compaction_strategies() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::defaults();
    let mut context = compose_world(&opts(&cfg, dir.path())).unwrap();
    let registry = context
        .get::<heycode_agent::CompactionRegistry>(heycode_agent::SERVICE_COMPACTIONS)
        .expect("default world must publish compactions");
    let descriptors = registry.descriptors();
    assert_eq!(
        descriptors
            .iter()
            .map(|descriptor| (descriptor.id().as_str(), descriptor.kind().name()))
            .collect::<Vec<_>>(),
        [
            ("portable-summary", "portable"),
            ("provider-native", "native"),
            ("prune-oldest", "prune"),
        ]
    );
    context.shutdown();
    assert!(registry.descriptors().is_empty());
}

#[test]
fn dry_production_composition_is_healthy_and_has_no_session_side_effect() {
    let harness = RealCompositionHarness::new().unwrap();
    assert!(!harness.sessions_dir().exists());
    let report = harness.inspect().unwrap();
    assert!(report.healthy, "{}", report.render_human());
    assert!(!harness.sessions_dir().exists());
}

#[test]
fn production_deepseek_anthropic_dialect_is_explicit_and_operation_credentialed() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.provider = "deepseek".to_owned();
    harness.config_mut().llm.model = heycode_provider_deepseek::DEEPSEEK_V4_FLASH.to_owned();
    harness.config_mut().llm.protocol = heycode_config::LlmProtocolCfg::AnthropicMessages;
    harness.config_mut().llm.api_key_env = Some("HEYCODE_TEST_DEEPSEEK_ANTHROPIC_KEY".to_owned());
    let credential_root = harness.credentials_root();
    std::fs::create_dir_all(&credential_root).unwrap();
    let credential_file = credential_root.join("credentials.toml");
    std::fs::write(
        &credential_file,
        "schema_version = 1\n[credentials]\nHEYCODE_TEST_DEEPSEEK_ANTHROPIC_KEY = \"test-only-not-live\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&credential_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let world = harness.without_fake_provider().compose().unwrap();
    let providers = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let provider = providers.get("deepseek").unwrap();
    let adapter = provider.inference_adapter().unwrap();
    assert_eq!(
        adapter.descriptor().protocols,
        [heycode_core::ProviderProtocol::AnthropicMessages]
    );
    assert_eq!(
        adapter.authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new("HEYCODE_TEST_DEEPSEEK_ANTHROPIC_KEY").unwrap()
        )
    );
    world.shutdown();
}

#[test]
fn production_openai_route_exposes_native_compaction_without_dispatching() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.provider = "openai".to_owned();
    harness.config_mut().llm.model = heycode_provider_openai::OPENAI_GPT_5_6_SOL.to_owned();
    harness.config_mut().llm.api_key_env = Some("HEYCODE_TEST_OPENAI_KEY".to_owned());
    let credential_root = harness.credentials_root();
    std::fs::create_dir_all(&credential_root).unwrap();
    let credential_file = credential_root.join("credentials.toml");
    std::fs::write(
        &credential_file,
        "schema_version = 1\n[credentials]\nHEYCODE_TEST_OPENAI_KEY = \"test-only-not-live\"\n",
    )
    .unwrap();
    let settings_file = harness.settings_path();
    std::fs::write(
        &settings_file,
        "schema_version = 1\n[settings.openai-prompt-cache]\nenabled = true\nprompt_cache_key = \"composition-cache\"\nmode = \"explicit\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&credential_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&settings_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let world = harness.without_fake_provider().compose().unwrap();
    let providers = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let openai = providers.get("openai").expect("production OpenAI route");
    let adapter = openai
        .inference_adapter()
        .expect("OpenAI must use strict Responses inference");
    assert_eq!(
        adapter.authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new("HEYCODE_TEST_OPENAI_KEY").unwrap()
        )
    );
    assert!(adapter.native_compaction().is_some());
    assert_eq!(openai.request_options().len(), 1);
    assert_eq!(openai.request_options()[0].kind(), "prompt-cache");
    let native_tools = world
        .context()
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let native_routes = native_tools.resolve("openai").unwrap();
    assert_eq!(
        native_routes
            .iter()
            .filter(|route| route.provider() == Some("openai"))
            .map(|route| route.implementation())
            .collect::<Vec<_>>(),
        [
            "openai:code_interpreter",
            "openai:hosted_shell",
            "openai:web_search",
        ]
    );
    let model = heycode_llm::ModelDescriptor::unknown(heycode_provider_openai::OPENAI_GPT_5_6_SOL);
    let request_options = openai
        .request_options_for(heycode_llm::ProviderOptionContext::new(
            &model,
            &native_routes,
        ))
        .unwrap();
    assert_eq!(
        request_options
            .iter()
            .map(|option| option.kind())
            .collect::<Vec<_>>(),
        ["hosted-tools", "prompt-cache"]
    );
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert!(
        agent
            .compaction_strategies()
            .iter()
            .any(|strategy| strategy.id().as_str() == heycode_agent::NativeCompaction::ID)
    );
    assert!(world.context().services().iter().any(|(key, owner)| {
        *key == heycode_agent::SERVICE_COMPACTIONS && *owner == "compactions"
    }));
    world.shutdown();
}

/// GOTCHAS #298: every openai/anthropic composition test pinned the one model
/// constant the capability table admits, so the suite stayed green while any
/// other model id aborted composition outright. A non-flagship id must compose
/// — an unproven model yields an empty executable tool plan, not a refusal —
/// and this test is the guard that was missing.
#[test]
fn production_openai_route_composes_with_a_non_flagship_model_id() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.provider = "openai".to_owned();
    harness.config_mut().llm.model = "gpt-5.6-mini-test-only".to_owned();
    harness.config_mut().llm.api_key_env = Some("HEYCODE_TEST_OPENAI_KEY".to_owned());
    let credential_root = harness.credentials_root();
    std::fs::create_dir_all(&credential_root).unwrap();
    let credential_file = credential_root.join("credentials.toml");
    std::fs::write(
        &credential_file,
        "schema_version = 1\n[credentials]\nHEYCODE_TEST_OPENAI_KEY = \"test-only-not-live\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&credential_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let world = harness.without_fake_provider().compose().unwrap();
    let providers = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let openai = providers.get("openai").expect("production OpenAI route");
    assert!(
        openai.inference_adapter().is_some(),
        "the route must stay strict regardless of model id"
    );
    world.shutdown();
}

#[test]
fn production_google_route_is_provider_owned_and_operation_credentialed() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.provider = "google".to_owned();
    harness.config_mut().llm.model = heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH.to_owned();
    harness.config_mut().llm.api_key_env = Some("HEYCODE_TEST_GOOGLE_KEY".to_owned());
    let settings_file = harness.settings_path();
    std::fs::write(
        &settings_file,
        format!(
            r#"schema_version = 1
[settings.google-inference.developer.google_search]
mode = "web"
models = ["{}"]
[settings.google-inference.developer.code_execution]
mode = "enabled"
models = ["{}"]
"#,
            heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH,
            heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH,
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&settings_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let world = harness.without_fake_provider().compose().unwrap();
    let providers = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let google = providers
        .get("google")
        .expect("provider-owned Google route");
    assert_eq!(
        google.info().default_model,
        heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH
    );
    let adapter = google
        .inference_adapter()
        .expect("Google must use strict GenerateContent inference");
    assert_eq!(
        adapter.authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new("HEYCODE_TEST_GOOGLE_KEY").unwrap()
        )
    );
    assert!(google.request_options().is_empty());
    let native_tools = world
        .context()
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let native_routes = native_tools.resolve("google").unwrap();
    assert_eq!(
        native_routes
            .iter()
            .filter(|route| route.provider() == Some("google"))
            .map(|route| route.implementation())
            .collect::<Vec<_>>(),
        ["google:code_execution", "google:google_search"]
    );
    let described = google.describe_model(heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH);
    assert_eq!(
        described.capabilities.native_web,
        heycode_llm::CapabilitySupport::Unknown,
        "a user Settings allowlist must not become provider capability evidence"
    );
    assert_eq!(
        described.capabilities.tools,
        heycode_llm::CapabilitySupport::Unknown,
        "configured code execution remains unproven until exact model evidence exists"
    );
    let inventory = world.context().plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "inference-google-gemini"
            && row.kind == heycode_core::ContributionKind::InferenceProvider
            && row.name == "google"
    }));
    world.shutdown();
    assert!(providers.get("google").is_none());
}

#[test]
fn shipping_binary_composes_lazy_cloud_routes_without_network_or_secret_output() {
    use std::process::Command;

    for (name, provider, model, protocol, reference, environment) in [
        (
            "bedrock-mantle",
            "bedrock-mantle",
            "openai.gpt-oss-120b-1:0",
            Some("openai_responses"),
            "HEYCODE_TEST_AWS_KEY",
            vec![("AWS_REGION", "us-east-1")],
        ),
        (
            "vertex-claude",
            "vertex-claude",
            heycode_provider_google::CLAUDE_VERTEX_DEFAULT_MODEL,
            None,
            "HEYCODE_TEST_GCP_TOKEN",
            vec![
                ("GOOGLE_CLOUD_PROJECT", "heycode-project"),
                ("GOOGLE_CLOUD_LOCATION", "global"),
            ],
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let protocol =
            protocol.map_or_else(String::new, |value| format!("protocol = \"{value}\"\n"));
        std::fs::write(
            home.join("config.toml"),
            format!(
                "schema_version = {}\n[llm]\nprovider = \"{provider}\"\nmodel = \"{model}\"\napi_key_env = \"{reference}\"\n{protocol}",
                heycode_config::CONFIG_SCHEMA_VERSION,
            ),
        )
        .unwrap();
        std::fs::write(
            home.join("credentials.toml"),
            format!("schema_version = 1\n[credentials]\n{reference} = \"test-only-not-live\"\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(
                home.join("config.toml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            std::fs::set_permissions(
                home.join("credentials.toml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_heycode"));
        command
            .current_dir(&workspace)
            .env("HEYCODE_HOME", &home)
            .args(["--restricted-workspace", "doctor", "--json"]);
        for (key, value) in environment {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["healthy"], true, "{name}: {report}");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!combined.contains("test-only-not-live"));
    }
}

#[test]
fn production_anthropic_route_exposes_native_compaction_without_dispatching() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.provider = "anthropic".to_owned();
    harness.config_mut().llm.model = heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5.to_owned();
    harness.config_mut().llm.api_key_env = Some("HEYCODE_TEST_ANTHROPIC_KEY".to_owned());
    let credential_root = harness.credentials_root();
    std::fs::create_dir_all(&credential_root).unwrap();
    let credential_file = credential_root.join("credentials.toml");
    std::fs::write(
        &credential_file,
        "schema_version = 1\n[credentials]\nHEYCODE_TEST_ANTHROPIC_KEY = \"test-only-not-live\"\n",
    )
    .unwrap();
    let settings_file = harness.settings_path();
    std::fs::write(
        &settings_file,
        r#"schema_version = 1
[settings.anthropic.prompt_cache]
mode = "automatic-1h"
[settings.anthropic.context_editing.thinking]
mode = "keep-turns"
keep_turns = 2
[settings.anthropic.context_editing.tools]
mode = "enabled"
clear_tool_inputs = true
exclude_tools = []
[settings.anthropic.context_editing.tools.trigger]
mode = "input-tokens"
value = 50000
[settings.anthropic.context_editing.tools.keep]
mode = "tool-uses"
value = 3
[settings.anthropic.context_editing.tools.clear_at_least]
mode = "input-tokens"
value = 5000
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&credential_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&settings_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let world = harness.without_fake_provider().compose().unwrap();
    let providers = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let anthropic = providers
        .get("anthropic")
        .expect("production Anthropic route");
    let adapter = anthropic
        .inference_adapter()
        .expect("Anthropic must use strict Messages inference");
    assert_eq!(
        adapter.authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new("HEYCODE_TEST_ANTHROPIC_KEY").unwrap()
        )
    );
    assert!(adapter.native_compaction().is_some());
    assert_eq!(
        anthropic
            .request_options()
            .iter()
            .map(|option| option.kind())
            .collect::<Vec<_>>(),
        ["context-editing", "prompt-cache"]
    );
    let native_tools = world
        .context()
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let native_routes = native_tools.resolve("anthropic").unwrap();
    assert_eq!(
        native_routes
            .iter()
            .filter(|route| route.provider() == Some("anthropic"))
            .map(|route| route.implementation())
            .collect::<Vec<_>>(),
        ["anthropic:code_execution", "anthropic:web_search"]
    );
    let model =
        heycode_llm::ModelDescriptor::unknown(heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5);
    let request_options = anthropic
        .request_options_for(heycode_llm::ProviderOptionContext::new(
            &model,
            &native_routes,
        ))
        .unwrap();
    assert_eq!(
        request_options
            .iter()
            .map(|option| option.kind())
            .collect::<Vec<_>>(),
        ["context-editing", "prompt-cache", "server-tools"]
    );
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert!(
        agent
            .compaction_strategies()
            .iter()
            .any(|strategy| strategy.id().as_str() == heycode_agent::NativeCompaction::ID)
    );
    world.shutdown();
}

#[test]
fn user_catalog_override_is_composed_as_attribution_not_provider_evidence() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.root().join("catalog-overrides.toml"),
        r#"schema_version = 1
[[model]]
provider = "fixture"
model = "fixture-model"
[model.capabilities]
tools = "supported"
"#,
    )
    .unwrap();
    let world = harness.compose().unwrap();
    let overrides = world
        .context()
        .get::<heycode_catalog_file::CatalogOverrides>(
            heycode_catalog_file::SERVICE_CATALOG_OVERRIDES,
        )
        .unwrap();
    let snapshot = heycode_llm::CatalogSnapshot {
        provider: heycode_llm::ProviderDescriptor {
            id: "fixture".to_owned(),
            display_name: "Fixture".to_owned(),
            protocols: vec![heycode_core::ProviderProtocol::Unknown],
        },
        models: vec![heycode_llm::ModelDescriptor::unknown("fixture-model")],
        revision: 1,
        fetched_at_ms: 1,
    };
    let attributed = overrides.attribute(&snapshot);
    let assertion = attributed.model("fixture-model").unwrap().assertions()[0];
    assert_eq!(assertion.source().layer(), "user");
    assert_eq!(
        assertion.direction(),
        heycode_catalog_file::AssertionDirection::ClaimsUnevidencedSupport
    );
    assert_eq!(
        snapshot.models[0].capabilities.tools,
        heycode_llm::CapabilitySupport::Unknown,
        "attribution must not rewrite cached provider evidence"
    );
    world.shutdown();
}

#[test]
fn activation_probe_uses_isolated_state_and_never_starts_configured_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let product = dir.path().join("product");
    let marker = dir.path().join("mcp-started");
    std::fs::create_dir(&workspace).unwrap();
    let mut config = Config::defaults();
    config.mcp.servers.insert(
        "must-not-start".to_owned(),
        heycode_config::McpServerCfg {
            command: Some("/bin/sh".to_owned()),
            url: None,
            args: vec![
                "-c".to_owned(),
                format!("printf started > {}", marker.display()),
            ],
            env: std::collections::HashMap::new(),
            required: false,
        },
    );
    let options = WorldOptions {
        config: &config,
        trust: heycode_trust::WorkspaceTrustService::memory(
            &workspace,
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: product.join("sessions"),
        attachments_dir: product.join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: product.join("settings.toml"),
        credentials_root: product.join("credentials"),
        catalog_cache_path: product.join("cache/models.json"),
        settings_watch: true,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: workspace,
        fake: Some(fake()),
        resume: Some(product.join("session.jsonl")),
    };

    let report = probe_world_activation(&options);

    assert!(report.complete, "{}", report.render_human());
    assert!(report.healthy, "{}", report.render_human());
    for suppressed in [
        "provider_credentials_and_inference",
        "persistent_product_state",
        "configured_mcp_servers",
        "settings_watchers",
        "session_resume",
    ] {
        assert!(
            report.suppressed.iter().any(|row| row == suppressed),
            "{}",
            report.render_human()
        );
    }
    assert!(!marker.exists());
    assert!(!product.exists());
}

#[test]
fn fresh_deepseek_default_matches_the_current_selectable_catalog_id() {
    let config = Config::defaults();
    assert_eq!(config.llm.provider, heycode_llm::DeepSeekProvider::NAME);
    assert_eq!(
        config.llm.model,
        heycode_llm::DeepSeekProvider::DEFAULT_MODEL
    );
    assert_eq!(
        config.llm.model,
        heycode_provider_deepseek::DEEPSEEK_V4_FLASH
    );
}

#[test]
fn named_profile_picker_layer_and_world_share_one_resolution_path() {
    let harness = RealCompositionHarness::new().unwrap();
    let profiles = harness.root().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("minimal.toml"),
        "schema_version = 1\nname = \"minimal\"\n\n[[plugins]]\nid = \"mcp\"\nenabled = false\n\n[[plugins]]\nid = \"skills\"\nenabled = true\n",
    )
    .unwrap();
    let store = heycode_config::NamedProfileStore::new(harness.root());
    assert_eq!(store.list().unwrap()[0].name, "minimal");
    let picker_layer = store.load("minimal").unwrap();
    let world = harness.with_profile_layer(picker_layer).compose().unwrap();
    let context = world.context();
    assert!(!context.plugins().contains(&"mcp"));
    let skills = context
        .plugins()
        .iter()
        .position(|plugin| *plugin == "skills")
        .unwrap();
    assert_eq!(
        context.plugin_scopes()[skills],
        heycode_core::PluginScope::User
    );
    world.shutdown();
}

#[test]
fn managed_capability_constraint_rejects_the_production_world_before_activation() {
    let document = heycode_config::ProfileDocument::from_toml(
        r#"
schema_version = 2
plugins = []

[constraints]
allowed_sources = ["built_in"]
denied_capabilities = ["external_process"]
"#,
    )
    .unwrap();
    let layer = heycode_config::ProfileLayer::new(
        heycode_core::PluginScope::Managed,
        heycode_config::ProfileSource::managed("no-processes"),
        document,
    )
    .unwrap();
    let error = match RealCompositionHarness::new()
        .unwrap()
        .with_profile_layer(layer)
        .compose()
    {
        Ok(_) => panic!("managed policy must reject external-process plugins"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("external_process"), "{error}");
    assert!(error.contains("managed policy"), "{error}");
}

#[test]
fn opt_in_zai_native_search_reaches_the_production_registry() {
    let document = heycode_config::ProfileDocument::from_toml(
        "schema_version = 1\n\n[[plugins]]\nid = \"native-zai\"\nenabled = true\n",
    )
    .unwrap();
    let layer = heycode_config::ProfileLayer::new(
        heycode_core::PluginScope::User,
        heycode_config::ProfileSource::named("/profiles/zai-native.toml"),
        document,
    )
    .unwrap();
    let world = RealCompositionHarness::new()
        .unwrap()
        .with_profile_layer(layer)
        .compose()
        .unwrap();
    let registry = world
        .context()
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    assert!(registry.resolve("deepseek").unwrap().iter().all(|route| {
        route.implementation() != heycode_provider_zai::ZAI_WEB_SEARCH_IMPLEMENTATION
    }));
    assert!(registry.resolve("zai").unwrap().iter().any(|route| {
        route.logical() == heycode_provider_zai::ZAI_WEB_SEARCH_LOGICAL
            && route.implementation() == heycode_provider_zai::ZAI_WEB_SEARCH_IMPLEMENTATION
            && route.provider() == Some("zai")
    }));
    let inventory = world.context().plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "native-zai"
            && row.kind == heycode_core::ContributionKind::NativeTool
            && row.name == heycode_provider_zai::ZAI_WEB_SEARCH_IMPLEMENTATION
    }));
    world.shutdown();
}

#[tokio::test]
async fn named_profile_command_uses_the_composed_store_and_emits_a_typed_selection() {
    let harness = RealCompositionHarness::new().unwrap();
    let profiles = harness.root().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("minimal.toml"),
        "schema_version = 1\nname = \"minimal\"\n\n[[plugins]]\nid = \"mcp\"\nenabled = false\n",
    )
    .unwrap();
    let world = harness.compose().unwrap();
    let context = world.context();
    let command = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("profile")
        .unwrap()
        .expect("the TUI contributes /profile");
    assert_eq!(
        command.descriptor().timing(),
        heycode_agent::CommandTiming::Queued
    );
    assert!(command.availability().is_available());
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        captured.lock().unwrap().push(event.clone());
    });

    command.execute(&agent, "minimal").await.unwrap();

    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::ProfileSelected { name } if name == "minimal"
    )));
    world.shutdown();
}

#[tokio::test]
async fn settings_command_opens_the_composed_schema_browser() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    let command = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("settings")
        .unwrap()
        .expect("the TUI contributes /settings");
    let mut state =
        heycode_tui::app::AppState::new("test-model", std::path::PathBuf::from("/workspace"));
    state.set_settings_services(
        context
            .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
            .unwrap(),
        context
            .get::<heycode_ui::settings_ui::SettingsUiRegistry>(heycode_ui::SERVICE_SETTINGS_UI)
            .unwrap(),
    );
    assert!(command.availability().is_available());
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    agent
        .ui()
        .on::<heycode_agent::UiEvent>(move |event| captured.lock().unwrap().push(event.clone()));

    command.execute(&agent, "").await.unwrap();
    let event = events
        .lock()
        .unwrap()
        .iter()
        .find(|event| {
            matches!(
                event,
                heycode_agent::UiEvent::SettingsShellRequested {
                    tab: heycode_agent::ui::SettingsShellTab::Config,
                    ..
                }
            )
        })
        .cloned()
        .expect("/settings must request the authoritative Config shell");
    state.apply(&event);

    let panel = state
        .settings_panel()
        .expect("the composed settings browser must open");
    assert!(panel.rows().iter().any(|row| row.namespace() == "routing"));
    assert!(panel.rows().iter().any(|row| row.namespace() == "web"));
    assert!(
        panel
            .rows()
            .iter()
            .any(|row| row.namespace() == "lmstudio-load")
    );
    let namespaces = panel
        .rows()
        .iter()
        .map(|row| row.namespace().to_owned())
        .collect::<Vec<_>>();
    events.lock().unwrap().clear();
    context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("config")
        .unwrap()
        .expect("status contributes /config")
        .execute(&agent, "")
        .await
        .unwrap();
    let event = events
        .lock()
        .unwrap()
        .iter()
        .find(|event| {
            matches!(
                event,
                heycode_agent::UiEvent::SettingsShellRequested {
                    tab: heycode_agent::ui::SettingsShellTab::Config,
                    ..
                }
            )
        })
        .cloned()
        .expect("/config opens the same authoritative settings owner");
    state.apply(&event);
    let reopened = state.settings_panel().expect("config settings browser");
    assert_eq!(
        reopened
            .rows()
            .iter()
            .map(|row| row.namespace().to_owned())
            .collect::<Vec<_>>(),
        namespaces
    );
    world.shutdown();
}

#[tokio::test]
async fn cmd04_commands_open_their_owning_panels_from_composed_services() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let handle = context
        .get::<heycode_tui::TuiHandle>(heycode_tui::SERVICE_TUI)
        .unwrap();
    let mut state =
        heycode_tui::app::AppState::new("test-model", std::path::PathBuf::from("/workspace"));
    state.set_panel_commands(handle.panels());
    state.set_mcp_services(
        context
            .get::<heycode_mcp::management::McpManagement>(heycode_mcp::SERVICE_MCP_MANAGEMENT)
            .unwrap(),
        context.get::<heycode_mcp::McpRegistry>(heycode_mcp::SERVICE_MCP),
    );
    state.set_plugin_services(
        context
            .get::<heycode_extensions::lifecycle::PluginLifecycle>(
                heycode_extensions::lifecycle::SERVICE_PLUGIN_LIFECYCLE,
            )
            .unwrap(),
        None,
    );
    state.set_capability_services(
        context.get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS),
        context.get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS),
        context.get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS),
    );

    for (id, expected) in [
        ("mcp", heycode_tui::CapabilityPanel::Mcp),
        ("agents", heycode_tui::CapabilityPanel::Agents),
        ("hooks", heycode_tui::CapabilityPanel::Hooks),
    ] {
        let command = commands.get(id).unwrap().expect("CMD04 command");
        assert!(command.availability().is_available(), "{id}");
        command.execute(&agent, "").await.unwrap();
        let requested = state.take_panel_open_request().expect("panel request");
        assert_eq!(requested, expected);
        state.open_capability_panel(requested);
    }
    assert_eq!(
        state.capability_catalog().map(|panel| panel.panel()),
        Some(heycode_tui::CapabilityPanel::Hooks)
    );

    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        captured.lock().unwrap().push(event.clone());
    });
    for (id, expected) in [
        ("plugins", heycode_tui::CapabilityPanel::Plugins),
        ("skills", heycode_tui::CapabilityPanel::Skills),
    ] {
        commands
            .get(id)
            .unwrap()
            .expect("capability-owned command")
            .execute(&agent, "")
            .await
            .unwrap();
        let event = events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event,
                    heycode_agent::UiEvent::CapabilityPanelRequested { panel }
                        if panel.as_str() == id
                )
            })
            .cloned()
            .expect("capability panel event");
        state.apply(&event);
        match expected {
            heycode_tui::CapabilityPanel::Plugins => assert!(state.plugin_panel().is_some()),
            heycode_tui::CapabilityPanel::Skills => {
                let screen = heycode_tui::ScreenReaderSnapshot::from_state(&state);
                assert!(
                    screen.as_text().lines().any(|line| line == "== Skills =="),
                    "{}",
                    screen.as_text()
                );
            }
            _ => unreachable!(),
        }
    }
    world.shutdown();
}

#[tokio::test]
async fn lmstudio_control_command_is_reachable_and_refuses_incomplete_operations_before_io() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    let command = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("lmstudio")
        .unwrap()
        .expect("PLM04 contributes /lmstudio");
    assert_eq!(
        command.descriptor().timing(),
        heycode_agent::CommandTiming::Queued
    );
    let error = command
        .execute(
            &context
                .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
                .unwrap(),
            "load",
        )
        .await
        .expect_err("load without a model must fail before local HTTP");
    assert_eq!(
        error.to_string(),
        "usage: /lmstudio <load|unload> <model-or-instance>"
    );
    world.shutdown();
}

#[cfg(unix)]
#[tokio::test]
async fn retained_output_and_lsp_tools_are_reachable_without_starting_an_unconfigured_server() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    assert!(
        context
            .get::<heycode_exec::RetainedOutputService>(heycode_exec::SERVICE_RETAINED_OUTPUT)
            .is_some()
    );
    let lsp = context
        .get::<heycode_exec::LspService>(heycode_exec::SERVICE_LSP)
        .unwrap();
    assert!(lsp.servers().unwrap().is_empty());
    let listing = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .get("lsp_servers")
        .expect("E08 contributes the safe discovery tool")
        .run(
            serde_json::json!({}),
            &heycode_tools::ToolCtx::default().with_cwd(std::path::PathBuf::from("/workspace")),
        )
        .await
        .unwrap();
    assert_eq!(listing, serde_json::json!([]));
    world.shutdown();
}

#[tokio::test]
async fn default_deferred_selector_keeps_loop_limits_disabled_in_a_real_composed_turn() {
    let world = RealCompositionHarness::new()
        .unwrap()
        .with_provider(Arc::new(FakeProvider::new(vec![vec![
            StreamChunk::TextDelta("bounded".to_owned()),
            StreamChunk::Finish(heycode_llm::FinishReason::Stop),
        ]])))
        .compose()
        .unwrap();
    let context = world.context();
    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .get(
            &heycode_settings::SettingsNamespace::new(
                heycode_agent::LOOP_BUDGET_SETTINGS_NAMESPACE,
            )
            .unwrap(),
        )
        .unwrap()
        .expect("A09 contributes explicit settings");
    assert_eq!(
        snapshot.applies(),
        heycode_settings::SettingsApplies::Restart
    );
    assert!(
        heycode_agent::LoopBudgetPolicy::from_value(snapshot.resolved())
            .unwrap()
            .is_unlimited()
    );
    assert_eq!(snapshot.resolved()["unknown_usage"], "allow-lower-bound");
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    // Loop budget and explicitly authorized saved-fallback preparation both run before dispatch.
    assert_eq!(agent.pre_step_seam().len(), 2);
    assert_eq!(
        agent.send("one bounded turn").await.unwrap().text,
        "bounded"
    );
    let metrics = agent
        .deferred_tool_metrics()
        .expect("A08 records the selected catalog");
    assert_eq!(metrics.catalog_entries(), metrics.selected_entries());
    world.shutdown();
}

#[test]
fn named_profile_refuses_ambiguous_legacy_complete_profile_combination() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().profile.plugins = vec![
        "session".to_owned(),
        "prompt".to_owned(),
        "tools".to_owned(),
    ];
    let document = heycode_config::ProfileDocument::from_toml(
        "schema_version = 1\n[[plugins]]\nid = \"mcp\"\nenabled = false\n",
    )
    .unwrap();
    let layer = heycode_config::ProfileLayer::new(
        heycode_core::PluginScope::User,
        heycode_config::ProfileSource::named("/profiles/minimal.toml"),
        document,
    )
    .unwrap();
    let error = match harness.with_profile_layer(layer).compose() {
        Ok(_) => panic!("ambiguous legacy and named profiles must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("cannot be combined"), "{error}");
}

#[test]
fn project_profile_layers_are_deferred_until_the_preopened_service_is_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::defaults();
    let document = heycode_config::ProfileDocument::from_toml(
        "schema_version = 1\n[[plugins]]\nid = \"skills\"\nenabled = true\n",
    )
    .unwrap();
    let layer = heycode_config::ProfileLayer::new(
        heycode_core::PluginScope::Project,
        heycode_config::ProfileSource::project(dir.path().join("profile.toml")),
        document,
    )
    .unwrap();
    let layers = [layer];

    let mut unknown = opts(&config, dir.path());
    unknown.profile_layers = &layers;
    let error = match compose_world(&unknown) {
        Ok(_) => panic!("unknown workspace must not activate a project profile"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("deferred until this workspace is trusted"));

    let trust = heycode_trust::WorkspaceTrustService::memory(
        dir.path(),
        heycode_cli::project_content_policy(),
    )
    .unwrap();
    trust
        .set_session(heycode_trust::WorkspaceTrustDecision::Trusted, 0)
        .unwrap();
    let mut trusted = opts(&config, dir.path());
    trusted.trust = trust;
    trusted.profile_layers = &layers;
    let mut context = compose_world(&trusted).unwrap();
    let skills = context
        .plugins()
        .iter()
        .position(|plugin| *plugin == "skills")
        .unwrap();
    assert_eq!(
        context.plugin_scopes()[skills],
        heycode_core::PluginScope::Project
    );
    context.shutdown();
}

#[test]
fn managed_code_authority_fails_loud_without_a_matching_pl08_generation_source() {
    let document = heycode_config::ProfileDocument::from_toml(
        r#"schema_version = 3
[code_authority]
pl08_generation = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
packages = []
"#,
    )
    .unwrap();
    let layer = heycode_config::ProfileLayer::new(
        heycode_core::PluginScope::Managed,
        heycode_config::ProfileSource::managed("test-policy"),
        document,
    )
    .unwrap();
    let error = RealCompositionHarness::new()
        .unwrap()
        .with_profile_layer(layer)
        .compose()
        .err()
        .expect("an unbound managed generation must not silently become code authority");
    assert!(
        error
            .to_string()
            .contains("managed PL08 admission generation source"),
        "{error}"
    );
}

#[test]
fn composition_refuses_trust_authority_from_a_different_workspace() {
    let root = tempfile::tempdir().unwrap();
    let trusted = root.path().join("trusted");
    let requested = root.path().join("requested");
    std::fs::create_dir_all(&trusted).unwrap();
    std::fs::create_dir_all(&requested).unwrap();
    let cfg = Config::defaults();
    let options = WorldOptions {
        config: &cfg,
        trust: heycode_trust::WorkspaceTrustService::memory(
            &trusted,
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: root.path().join("sessions"),
        attachments_dir: root.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: root.path().join("settings.toml"),
        credentials_root: root.path().join("credentials-home"),
        catalog_cache_path: root.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: requested,
        fake: Some(std::sync::Arc::new(
            heycode_llm::testing::FakeProvider::new(Vec::new()),
        )),
        resume: None,
    };
    let error = match resolve_world_plugins(&options) {
        Ok(_) => panic!("mismatched trust authority must fail"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("workspace trust identity does not match composition cwd"),
        "{error}"
    );
    assert!(!options.sessions_dir.exists());
}

#[test]
fn project_skill_discovery_is_blocked_while_unknown_and_allowed_when_restricted() {
    let dir = tempfile::tempdir().unwrap();
    for (root, name) in [
        (dir.path().join(".heycode/skills/project"), "project-skill"),
        (
            dir.path().join("credentials-home/skills/home"),
            "home-skill",
        ),
    ] {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: fixture\n---\nbody\n"),
        )
        .unwrap();
    }
    let config = Config::defaults();

    let mut unknown = compose_world(&opts(&config, dir.path())).unwrap();
    let unknown_skills = unknown
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    assert_eq!(
        unknown_skills
            .snapshot()
            .unwrap()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["home-skill"]
    );
    unknown.shutdown();

    let trust = heycode_trust::WorkspaceTrustService::memory(
        dir.path(),
        heycode_cli::project_content_policy(),
    )
    .unwrap();
    trust
        .set_session(heycode_trust::WorkspaceTrustDecision::Restricted, 0)
        .unwrap();
    let mut restricted_options = opts(&config, dir.path());
    restricted_options.trust = trust;
    let mut restricted = compose_world(&restricted_options).unwrap();
    let restricted_skills = restricted
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    assert_eq!(
        restricted_skills
            .snapshot()
            .unwrap()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["project-skill", "home-skill"]
    );
    restricted.shutdown();
}

#[test]
fn built_in_service_key_registry_is_unique_and_matches_the_constitution() {
    let names: Vec<_> = BUILTIN_SERVICE_KEYS
        .iter()
        .map(|key| key.as_str())
        .collect();
    let unique: std::collections::BTreeSet<_> = names.iter().copied().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "duplicate service key: {names:?}"
    );
    assert_eq!(
        names,
        [
            "doctor",
            "trust",
            "ui",
            "settings-ui",
            "settings",
            "http",
            "sandbox",
            "subprocess",
            "terminal",
            "shell",
            "hooks",
            "filesystem",
            "retained-output",
            "lsp",
            "credentials",
            "authorization",
            "secret-prompt",
            "aws-auth",
            "gcp-auth",
            "lmstudio",
            "lmstudio/model-control",
            "lmstudio/models",
            "ollama",
            "ollama/catalog",
            "ollama/profile",
            "ollama/inference",
            "onboarding",
            "profiles",
            "session",
            "workspace-transition",
            "memory-sources",
            "attachments",
            "session-query",
            "prompt",
            "native-tools",
            "web",
            "document-extractor",
            "providers",
            "llm",
            "provider-interception",
            "request-transforms",
            "models",
            "catalog-overrides",
            "runtimes",
            "mcp",
            "mcp-management",
            "plugin-lifecycle",
            "telemetry",
            "tools",
            "seam/pre_tool",
            "approval",
            "approval-interactive",
            "approval-switch",
            "commands",
            "health-history",
            "routing",
            "agent-options",
            "compactions",
            "agent",
            "advisor",
            "app-server",
            "plan",
            "subagents",
            "jobs",
            "execution-jobs",
            "goals",
            "workflows",
            "schedules",
            "teams",
            "reviews",
            "token-counters",
            "skills",
            "release-manager",
            "tui",
        ]
    );
}

#[test]
fn default_world_persists_settings_only_at_the_explicit_world_path() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::defaults();
    let mut context = compose_world(&opts(&cfg, dir.path())).unwrap();
    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let namespace = heycode_settings::SettingsNamespace::new("smoke-settings").unwrap();
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({"type": "object"}),
        serde_json::json!({"enabled": false}),
        |value| {
            value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .map(|_| ())
                .ok_or_else(|| "enabled must be boolean".to_owned())
        },
    )
    .unwrap();
    settings
        .register(
            &context,
            heycode_settings::SettingsDefinition::new(namespace.clone(), schema),
        )
        .unwrap();
    settings
        .replace_user(&namespace, serde_json::json!({"enabled": true}), Some(0))
        .unwrap();

    let web = context
        .get::<heycode_web::WebRegistry>(heycode_web::SERVICE_WEB)
        .unwrap();
    assert_eq!(
        web.capability_report().unwrap().search,
        heycode_web::WebProviderSelection::Selected {
            provider: "portable".to_owned(),
            configured: false,
        }
    );
    settings
        .replace_user(
            &heycode_web::web_policy_namespace().unwrap(),
            serde_json::json!({
                "search_provider":"portable",
                "fetch_provider":"portable",
                "search_domains":{"allow":["docs.rs"],"block":[]},
                "fetch_domains":{"allow":[],"block":["ads.example"]}
            }),
            Some(0),
        )
        .unwrap();
    let report = web.capability_report().unwrap();
    assert_eq!(
        report.search,
        heycode_web::WebProviderSelection::Selected {
            provider: "portable".to_owned(),
            configured: true,
        }
    );
    assert_eq!(report.search_domains.allow(), ["docs.rs"]);
    assert_eq!(report.fetch_domains.block(), ["ads.example"]);

    let path = dir.path().join("settings.toml");
    let persisted = std::fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("[settings.smoke-settings]"));
    assert!(persisted.contains("enabled = true"));
    assert!(persisted.contains("[settings.web]"));
    assert!(persisted.contains("search_provider = \"portable\""));
    context.shutdown();
}

#[tokio::test]
async fn default_world_attachment_store_commits_bytes_then_session_metadata() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    let store = context
        .get::<heycode_attachments::AttachmentStore>(heycode_attachments::SERVICE_ATTACHMENTS)
        .unwrap();
    let admission = store
        .admit(
            heycode_attachments::AttachmentInput::new(
                b"production composition attachment".to_vec(),
                Some("text/plain"),
                Some("evidence.txt"),
            )
            .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(admission.event_seq(), 1, "session/created is seq zero");
    assert_eq!(
        store
            .read(
                admission.metadata(),
                tokio_util::sync::CancellationToken::new(),
            )
            .unwrap(),
        b"production composition attachment"
    );
    let session = context
        .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    assert!(matches!(
        &session.lock().unwrap().events()[1].kind,
        heycode_session::SessionEventKind::AttachmentAdded { attachment }
            if attachment.as_ref() == admission.metadata()
    ));
    let web = context
        .get::<heycode_web::WebRegistry>(heycode_web::SERVICE_WEB)
        .unwrap();
    let extracted = web
        .extract(
            heycode_web::WebRawDocument::new(
                "https://example.test/source",
                Some("text/html"),
                b"<html><body><main>production extraction</main></body></html>".to_vec(),
                false,
                4_096,
            )
            .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(extracted.content().contains("production extraction"));
    assert!(extracted.source().content_id().is_some());
    assert!(matches!(
        &session.lock().unwrap().events().last().unwrap().kind,
        heycode_session::SessionEventKind::AttachmentAdded { attachment }
            if attachment.source().is_some_and(|source| source.url() == "https://example.test/source")
    ));
    assert_eq!(
        web.processor_descriptors().unwrap()[0].id(),
        "portable-readable"
    );
}

#[test]
fn real_llm_plugin_resolves_after_legacy_file_migration() {
    let dir = tempfile::tempdir().unwrap();
    let credentials_root = dir.path().join("credentials-home");
    std::fs::create_dir_all(&credentials_root).unwrap();
    let reference = "HEYCODE_TEST_OPENROUTER_KEY_91D4";
    std::fs::write(
        credentials_root.join("credentials"),
        format!("{reference}=sk-or-test-construction-only\n"),
    )
    .unwrap();
    let mut config = Config::defaults();
    config.llm.provider = "openrouter".to_owned();
    config.llm.model = "test/model".to_owned();
    config.llm.api_key_env = Some(reference.to_owned());
    let mut context = compose_world(&WorldOptions {
        config: &config,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().join("sessions"),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: dir.path().join("settings.toml"),
        credentials_root: credentials_root.clone(),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: None,
        resume: None,
    })
    .unwrap();

    let providers = context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    assert_eq!(providers.names(), ["openrouter"]);
    let openrouter = providers.get("openrouter").unwrap();
    assert_eq!(
        openrouter
            .inference_adapter()
            .unwrap()
            .authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new(reference).unwrap()
        )
    );
    let request_options = openrouter.request_options();
    assert_eq!(
        request_options
            .iter()
            .map(heycode_core::ProviderRequestOption::kind)
            .collect::<Vec<_>>(),
        ["routing", "caching", "transforms"]
    );
    assert_eq!(
        request_options[2].data()["plugins"],
        serde_json::json!([
            {"id":"context-compression","enabled":false},
            {"id":"file-parser","enabled":false},
            {"id":"response-healing","enabled":false}
        ])
    );
    assert!(!credentials_root.join("credentials").exists());
    assert!(credentials_root.join("credentials.toml").is_file());
    assert!(credentials_root.join("credentials.legacy.bak").is_file());
    context.shutdown();
}

#[test]
fn onboarding_required_world_composes_with_disconnected_provider_and_blocks_setup() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::defaults();
    config.llm.provider = "openrouter".to_owned();
    config.llm.model = "not-connected".to_owned();
    config.llm.api_key_env = Some("HEYCODE_TEST_MISSING_ONBOARDING_KEY_6C2E".to_owned());
    let mut context = compose_world(&WorldOptions {
        config: &config,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().join("sessions"),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: dir.path().join("settings.toml"),
        credentials_root: dir.path().join("credentials-home"),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: true,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: None,
        resume: None,
    })
    .unwrap();

    let onboarding = context
        .get::<heycode_onboarding::OnboardingService>(heycode_onboarding::SERVICE_ONBOARDING)
        .unwrap();
    assert!(onboarding.snapshot().unwrap().active);
    let providers = context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    assert_eq!(providers.names(), ["openrouter"]);
    assert!(
        context
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .is_some()
    );
    context.shutdown();
}

#[tokio::test]
async fn durable_logout_latch_wins_a_still_present_credential_and_forces_fresh_welcome() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.toml");
    let credentials_root = dir.path().join("credentials-home");
    std::fs::write(
        &settings_path,
        r#"schema_version = 1

[settings.routing]
setup_required = true
runtime = "codex"
provider = "openrouter"
model = "z-ai/glm-5.3-flash"
"#,
    )
    .unwrap();
    heycode_cli::write_credential_at(
        heycode_llm::OpenRouterProvider::API_KEY_ENV,
        "credential-must-not-reactivate",
        &credentials_root,
    )
    .unwrap();
    let config = Config::defaults();
    let mut context = compose_world(&WorldOptions {
        config: &config,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().join("sessions"),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Interactive,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: settings_path,
        credentials_root: credentials_root.clone(),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: None,
        resume: None,
    })
    .unwrap();

    let onboarding = context
        .get::<heycode_onboarding::OnboardingService>(heycode_onboarding::SERVICE_ONBOARDING)
        .unwrap()
        .snapshot()
        .unwrap();
    assert!(onboarding.active);
    assert_eq!(onboarding.step, heycode_onboarding::OnboardingStep::Welcome);
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert!(!agent.inference_connected());
    assert!(agent.send("must not run").await.is_err());
    assert!(
        heycode_cli::lookup_credential_at(
            heycode_llm::OpenRouterProvider::API_KEY_ENV,
            &credentials_root,
        )
        .unwrap()
        .is_some(),
        "the remaining credential proves startup did not rely on its absence"
    );
    let routing = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == config.llm.provider)
        .unwrap();
    routing
        .stage_connection(
            &profile.registry_name,
            profile.default_model.as_deref().unwrap(),
            None,
        )
        .unwrap();
    assert!(!routing.requires_setup().unwrap());
    assert!(
        !agent.inference_connected(),
        "the old world stays blocked until the staged route is recomposed"
    );
    context.shutdown();
}

#[test]
fn durable_logout_latch_bypasses_an_incomplete_configured_cloud_route() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.toml");
    std::fs::write(
        &settings_path,
        "schema_version = 1\n\n[settings.routing]\nsetup_required = true\n",
    )
    .unwrap();
    let mut config = Config::defaults();
    config.llm.provider = "bedrock".to_owned();
    config.llm.model = "incomplete-cloud-route".to_owned();
    let mut context = compose_world(&WorldOptions {
        config: &config,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().join("sessions"),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Interactive,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: settings_path,
        credentials_root: dir.path().join("credentials-home"),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: None,
        resume: None,
    })
    .unwrap();

    assert!(
        context
            .get::<heycode_onboarding::OnboardingService>(heycode_onboarding::SERVICE_ONBOARDING)
            .unwrap()
            .snapshot()
            .unwrap()
            .active
    );
    assert!(
        !context
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .unwrap()
            .inference_connected()
    );
    context.shutdown();
}

#[test]
fn explicitly_configured_ollama_reaches_provider_catalog_and_picker_without_credentials_or_io() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::defaults();
    config.llm.provider = heycode_provider_lmstudio::OLLAMA_PROVIDER.to_owned();
    config.llm.model = "local-fixture".to_owned();
    config.llm.api_key_env = None;
    let mut options = opts(&config, dir.path());
    options.fake = None;
    let mut context = compose_world(&options).unwrap();

    let providers = context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    assert_eq!(providers.names(), ["ollama"]);
    let profile = providers.profiles().pop().unwrap();
    assert_eq!(profile.registry_name, "ollama");
    assert_eq!(profile.default_model, "local-fixture");
    assert!(profile.credential_reference.is_none());
    assert_eq!(
        profile.descriptor.protocols,
        [heycode_core::ProviderProtocol::OpenAiChatCompletions]
    );
    let selected = context
        .get::<heycode_llm::LlmSelection>(heycode_llm::SERVICE_LLM)
        .unwrap();
    assert_eq!(selected.provider_name, "ollama");
    assert_eq!(selected.model, "local-fixture");
    assert!(
        context
            .get::<heycode_provider_lmstudio::OllamaInference>(
                heycode_provider_lmstudio::SERVICE_OLLAMA_INFERENCE,
            )
            .is_some()
    );
    let catalogs = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        catalogs
            .descriptors()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor.id == "ollama"
                && descriptor.protocols == [heycode_core::ProviderProtocol::OpenAiChatCompletions])
    );
    assert!(context.plugins().contains(&"provider-ollama"));
    context.shutdown();
}

#[test]
fn ollama_preserves_a_credential_reference_instead_of_ignoring_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::defaults();
    config.llm.provider = heycode_provider_lmstudio::OLLAMA_PROVIDER.to_owned();
    config.llm.model = "local-fixture".to_owned();
    config.llm.api_key_env = Some("SHOULD_NOT_BE_READ".to_owned());
    let mut options = opts(&config, dir.path());
    options.fake = None;
    let world = compose_world(&options).unwrap();
    let providers = world
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let provider = providers.get("ollama").unwrap();
    assert_eq!(provider.credential_reference(), Some("SHOULD_NOT_BE_READ"));
    assert_eq!(
        provider
            .inference_adapter()
            .unwrap()
            .authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new("SHOULD_NOT_BE_READ").unwrap()
        )
    );
}

/// The baseline audit is intentionally exact. A new default plugin, service,
/// command, tool, or provider must update this inventory in the same change so
/// STATUS claims cannot drift away from the world users actually compose.
#[test]
fn default_world_reports_exact_live_inventory() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let ctx = world.context();

    let settings = ctx
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .expect("default world must mount settings before consumers");
    let settings_descriptors = settings.describe().unwrap();
    assert_eq!(settings_descriptors.len(), 19);
    assert_eq!(
        settings_descriptors
            .iter()
            .map(|snapshot| snapshot.namespace().as_str())
            .collect::<Vec<_>>(),
        [
            "advisor",
            "anthropic",
            "anthropic-server-tools",
            "autocompact",
            "aws-bedrock",
            "credentials",
            "google-inference",
            "keymap",
            "lmstudio-load",
            "loop-budget",
            "mcp-servers",
            "native-tools",
            "openai-hosted-tools",
            "openai-prompt-cache",
            "plugins",
            "routing",
            "skills-preferences",
            "ui-preferences",
            "web"
        ]
    );
    assert!(
        settings.writable(),
        "default world must mount file settings"
    );
    let authorization = ctx
        .get::<heycode_authorization::AuthorizationService>(
            heycode_authorization::SERVICE_AUTHORIZATION,
        )
        .unwrap();
    let flow_ids: Vec<_> = authorization
        .descriptors()
        .unwrap()
        .into_iter()
        .map(|descriptor| descriptor.id.as_str().to_owned())
        .collect();
    assert_eq!(
        flow_ids,
        [
            "anthropic-api-key",
            "aws-bedrock-api-key",
            "deepseek-api-key",
            "fireworks-api-key",
            "groq-api-key",
            "mistral-api-key",
            "openai-api-key",
            "openrouter-api-key",
            "together-api-key",
            "xai-api-key"
        ]
    );

    assert_eq!(
        ctx.plugins(),
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
    assert!(
        ctx.plugin_scopes()
            .iter()
            .all(|scope| *scope == heycode_core::PluginScope::BuiltIn)
    );

    let mut services: Vec<_> = ctx
        .services()
        .into_iter()
        .map(|(key, owner)| (key.as_str(), owner))
        .collect();
    services.sort_unstable();
    assert_eq!(
        services,
        [
            ("add-directory-prompt", "workspace-transitions"),
            ("advisor", "advisor"),
            ("agent", "agent"),
            ("agent-declarations", "product-extensions"),
            ("agent-options", "agent-options"),
            ("app-server", "app-server"),
            ("approval", "approval"),
            ("attachments", "attachments-local"),
            ("authorization", "authorization"),
            ("aws-auth", "authorization-aws"),
            ("catalog-overrides", "catalog-overrides"),
            ("commands", "commands"),
            ("compactions", "compactions"),
            ("config-import", "config-import"),
            ("config-import-mount", "config-import-resources"),
            ("credentials", "credentials"),
            ("doctor", "doctor"),
            ("document-extractor", "web-extract"),
            ("execution-jobs", "execution-jobs"),
            ("filesystem", "filesystem-local"),
            ("gcp-auth", "authorization-gcp"),
            ("goals", "goals"),
            ("health-history", "health-history"),
            ("hook-execution-gate", "plan"),
            ("hooks", "hooks"),
            ("http", "http-reqwest"),
            ("jobs", "agent"),
            ("llm", "llm"),
            ("lmstudio", "provider-lmstudio"),
            ("lmstudio/model-control", "provider-lmstudio"),
            ("lmstudio/models", "catalog-lmstudio"),
            ("lsp", "lsp-registry"),
            ("mcp", "mcp-registry"),
            ("mcp-management", "mcp-management"),
            ("mcp-runtime-control", "mcp"),
            ("memory-sources", "memory-commands"),
            ("models", "models"),
            ("native-tools", "native-tools"),
            ("onboarding", "onboarding"),
            ("plan", "plan"),
            ("plugin-lifecycle", "plugin-lifecycle"),
            ("profiles", "profiles"),
            ("prompt", "prompt"),
            ("provider-interception", "llm"),
            ("providers", "llm"),
            ("questions", "agent"),
            ("request-transforms", "request-transforms"),
            ("retained-output", "retained-output-local"),
            ("reviews", "reviewer"),
            ("routing", "routing"),
            ("runtimes", "runtimes"),
            ("sandbox", "sandbox"),
            ("schedules", "schedules"),
            ("seam/pre_tool", "tools"),
            ("secret-prompt", "secret-prompt"),
            ("session", "session"),
            ("session-query", "session-query-jsonl"),
            ("settings", "settings-file"),
            ("settings-shell-snapshot", "status"),
            ("settings-ui", "ui"),
            ("shell", "shell-local"),
            ("skills", "skills"),
            ("subagents", "subagent"),
            ("subprocess", "subprocess-local"),
            ("teams", "teams"),
            ("telemetry", "telemetry-local-off"),
            ("terminal", "terminal-registry"),
            ("token-counters", "token-counters"),
            ("tools", "tools"),
            ("trust", "trust"),
            ("tui", "tui"),
            ("ui", "ui"),
            ("web", "web"),
            ("work", "work"),
            ("workflows", "workflows"),
            ("workspace-transition", "workspace-scope"),
        ]
    );
    let runtimes = ctx
        .get::<heycode_runtime::AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    assert_eq!(
        runtimes.ids().unwrap(),
        [
            "claude",
            "codex",
            "deepseek-harness",
            "grok",
            "native",
            "opencode"
        ]
    );

    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    assert_eq!(
        commands.names().unwrap(),
        [
            "help",
            "plugins",
            "tools",
            "compact",
            "title",
            "quit",
            "recap",
            "btw",
            "output-style",
            "questions",
            "answer",
            "lmstudio",
            "status",
            "doctor",
            "permissions",
            "sandbox",
            "config",
            "context",
            "usage",
            "stats",
            "web",
            "health",
            "init",
            "skills",
            "skill",
            "skill-doctor",
            "reload-skills",
            "agent-config",
            "plan",
            "tasks",
            "ps",
            "stop",
            "goal",
            "review-runtime",
            "scripts",
            "attach",
            "document",
            "fallback",
            "provider",
            "model",
            "effort",
            "connect",
            "logout",
            "add-dir",
            "cd",
            "worktree",
            "import",
            "memory",
            "autocompact",
            "profile",
            "diff",
            "copy",
            "mention",
            "focus",
            "color",
            "advisor",
            "review",
            "ask-advisor",
            "security-review",
            "theme",
            "keymap",
            "vim",
            "scroll-speed",
            "statusline",
            "release-notes",
            "insights",
            "reload-plugins",
            "tui",
            "settings",
            "background",
            "fork",
            "sessions",
            "new",
            "resume",
            "branch",
            "rename",
            "archive",
            "delete",
            "export",
            "rewind",
            "voice",
            "mcp",
            "agents",
            "hooks",
            "workflows",
            "list-agents",
            "subtask",
            "schedule",
        ]
    );

    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    assert_eq!(
        tools.names(),
        [
            "read",
            "read_many",
            "write",
            "edit",
            "multi_edit",
            "bash",
            "glob",
            "grep",
            "web_fetch",
            "notebook_read",
            "notebook_edit",
            "artifact",
            "SendUserFile",
            "transcribe_audio",
            "computer",
            "browser",
            "lsp",
            "lsp_servers",
            "lsp_definition",
            "lsp_references",
            "lsp_diagnostics",
            "load_skill",
            "list_mcp_resources",
            "read_mcp_resource",
            "wait_for_mcp_servers",
            "agent",
            "send_message",
            "list_agents",
            "interrupt_task",
            "agent_control",
            "enter_plan_mode",
            "exit_plan_mode",
            "ask_user_question",
            "ask_user_question_async",
            "list_jobs",
            "cancel_job",
            "advisor",
            "terminal_open",
            "terminal_write",
            "terminal_read",
            "terminal_resize",
            "terminal_kill",
            "terminal_list",
            "background_shell",
            "background_terminal",
            "job_output",
            "job_control",
            "monitor",
            "run_tool",
            "goal",
            "workflow",
            "schedule_create",
            "schedule_list",
            "schedule_delete",
            "schedule_wakeup",
            "team",
            "task_create",
            "task_get",
            "task_list",
            "task_update",
            "report_findings",
            "run_code",
            "tool_search",
            "enter_worktree",
            "exit_worktree",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    );

    let providers = ctx
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    assert_eq!(providers.names(), ["fake"]);
    let catalogs = ctx
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(catalogs.has_persistence().unwrap());
    let ui = ctx
        .get::<heycode_ui::UiRegistry>(heycode_ui::SERVICE_UI)
        .unwrap();
    assert_eq!(
        ui.snapshot()
            .unwrap()
            .iter()
            .map(|row| (row.slot().as_str(), row.id().as_str(), row.priority()))
            .collect::<Vec<_>>(),
        [
            ("panel", "transcript", 100),
            ("panel", "sessions", 90),
            ("panel", "agents", 80),
            ("panel", "hooks", 80),
            ("panel", "skills", 80),
            ("panel", "mcp", 60),
            ("panel", "plugins", 55),
            ("dialog", "approval", 100),
            ("status", "session", 100),
            ("side-panel", "agents", 50),
            ("side-panel", "diff", 50),
            ("side-panel", "jobs", 50),
        ]
    );
    let doctor = ctx
        .get::<heycode_doctor::DoctorRegistry>(heycode_doctor::SERVICE_DOCTOR)
        .unwrap();
    assert_eq!(
        doctor
            .check_ids()
            .unwrap()
            .iter()
            .map(heycode_doctor::DoctorCheckId::as_str)
            .collect::<Vec<_>>(),
        ["config-migration", "settings", "credentials"]
    );

    let exact = ctx.plugin_inventory().snapshot().unwrap();
    let command_catalog = commands.catalog().unwrap();
    assert_eq!(
        command_catalog
            .iter()
            .map(|entry| {
                (
                    entry.descriptor.id(),
                    entry.descriptor.timing().as_str(),
                    entry.descriptor.source().plugin(),
                    entry.descriptor.synopsis(),
                )
            })
            .collect::<Vec<_>>(),
        [
            ("help", "immediate", "commands", "/help".to_owned()),
            (
                "plugins",
                "immediate",
                "commands",
                "/plugins [verbose]".to_owned()
            ),
            (
                "tools",
                "immediate",
                "commands",
                "/tools [name] [json]".to_owned()
            ),
            (
                "compact",
                "model_scheduling",
                "commands",
                "/compact [strategy] [keep] [instructions...]".to_owned()
            ),
            ("title", "queued", "commands", "/title [text...]".to_owned()),
            ("quit", "interrupting", "commands", "/quit".to_owned()),
            (
                "recap",
                "immediate",
                "commands",
                "/recap [mode...]".to_owned()
            ),
            (
                "btw",
                "immediate",
                "commands",
                "/btw [question...]".to_owned()
            ),
            (
                "output-style",
                "immediate",
                "commands",
                "/output-style [style...]".to_owned()
            ),
            (
                "questions",
                "immediate",
                "commands",
                "/questions [cancel-id...]".to_owned()
            ),
            (
                "answer",
                "immediate",
                "commands",
                "/answer [id-answer...]".to_owned()
            ),
            (
                "lmstudio",
                "queued",
                "lmstudio-control",
                "/lmstudio <operation> <target>".to_owned()
            ),
            ("status", "immediate", "status", "/status".to_owned()),
            ("doctor", "immediate", "status", "/doctor".to_owned()),
            (
                "permissions",
                "immediate",
                "status",
                "/permissions [mode]".to_owned()
            ),
            ("sandbox", "immediate", "status", "/sandbox".to_owned()),
            (
                "config",
                "immediate",
                "status",
                "/config [action]".to_owned()
            ),
            (
                "context",
                "immediate",
                "status-context",
                "/context".to_owned()
            ),
            ("usage", "immediate", "status-context", "/usage".to_owned()),
            ("stats", "immediate", "status-context", "/stats".to_owned()),
            ("web", "immediate", "status-web", "/web".to_owned()),
            (
                "health",
                "immediate",
                "health-history",
                "/health".to_owned()
            ),
            (
                "init",
                "queued",
                "init",
                "/init [action] [token]".to_owned()
            ),
            ("skills", "immediate", "skills", "/skills".to_owned()),
            (
                "skill",
                "model_scheduling",
                "skills",
                "/skill <name> [prompt...]".to_owned()
            ),
            (
                "skill-doctor",
                "immediate",
                "skills",
                "/skill-doctor".to_owned()
            ),
            (
                "reload-skills",
                "immediate",
                "skills",
                "/reload-skills".to_owned()
            ),
            (
                "agent-config",
                "immediate",
                "product-extensions",
                "/agent-config [action...]".to_owned()
            ),
            ("plan", "queued", "plan", "/plan [intent...]".to_owned()),
            ("tasks", "immediate", "execution-jobs", "/tasks".to_owned()),
            ("ps", "immediate", "execution-jobs", "/ps".to_owned()),
            (
                "stop",
                "immediate",
                "execution-jobs",
                "/stop <target>".to_owned()
            ),
            ("goal", "queued", "goals", "/goal [operation...]".to_owned()),
            (
                "review-runtime",
                "model_scheduling",
                "reviewer",
                "/review-runtime <runtime> [instructions...]".to_owned()
            ),
            (
                "scripts",
                "immediate",
                "code-mode",
                "/scripts [action] [run-id]".to_owned()
            ),
            (
                "attach",
                "immediate",
                "agent-attachments",
                "/attach <path...>".to_owned()
            ),
            (
                "document",
                "immediate",
                "agent-documents",
                "/document <path...>".to_owned()
            ),
            (
                "fallback",
                "queued",
                "routing",
                "/fallback [provider] [model]".to_owned()
            ),
            (
                "provider",
                "queued",
                "routing",
                "/provider [id] [opaque]".to_owned()
            ),
            (
                "model",
                "queued",
                "routing",
                "/model [id] [opaque]".to_owned()
            ),
            ("effort", "queued", "routing", "/effort [level]".to_owned()),
            (
                "connect",
                "queued",
                "routing-auth",
                "/connect [target]".to_owned()
            ),
            (
                "logout",
                "immediate",
                "routing-auth",
                "/logout [provider]".to_owned()
            ),
            (
                "add-dir",
                "queued",
                "workspace-transitions",
                "/add-dir [path...]".to_owned()
            ),
            (
                "cd",
                "queued",
                "workspace-transitions",
                "/cd <path...>".to_owned()
            ),
            (
                "worktree",
                "queued",
                "workspace-transitions",
                "/worktree [action]".to_owned()
            ),
            (
                "import",
                "immediate",
                "config-import",
                "/import [action]".to_owned()
            ),
            (
                "memory",
                "queued",
                "memory-commands",
                "/memory [action] [arguments...]".to_owned()
            ),
            (
                "autocompact",
                "immediate",
                "autocompact",
                "/autocompact [threshold]".to_owned()
            ),
            ("profile", "queued", "tui", "/profile [name]".to_owned()),
            ("diff", "immediate", "tui", "/diff".to_owned()),
            ("copy", "immediate", "tui", "/copy [number]".to_owned()),
            (
                "mention",
                "immediate",
                "tui",
                "/mention [reference...]".to_owned()
            ),
            ("focus", "immediate", "tui", "/focus".to_owned()),
            ("color", "immediate", "tui", "/color [color]".to_owned()),
            (
                "advisor",
                "immediate",
                "tui",
                "/advisor [selection...]".to_owned()
            ),
            (
                "review",
                "model_scheduling",
                "tui",
                "/review [instructions...]".to_owned()
            ),
            (
                "ask-advisor",
                "model_scheduling",
                "tui",
                "/ask-advisor [instructions...]".to_owned()
            ),
            (
                "security-review",
                "model_scheduling",
                "tui",
                "/security-review [instructions...]".to_owned()
            ),
            ("theme", "immediate", "tui", "/theme [theme]".to_owned()),
            (
                "keymap",
                "immediate",
                "tui",
                "/keymap [action] [chord]".to_owned()
            ),
            ("vim", "immediate", "tui", "/vim [state]".to_owned()),
            (
                "scroll-speed",
                "immediate",
                "tui",
                "/scroll-speed".to_owned()
            ),
            (
                "statusline",
                "immediate",
                "tui",
                "/statusline [action] [value]".to_owned()
            ),
            (
                "release-notes",
                "immediate",
                "tui",
                "/release-notes [full]".to_owned()
            ),
            ("insights", "queued", "tui", "/insights".to_owned()),
            (
                "reload-plugins",
                "queued",
                "tui",
                "/reload-plugins".to_owned()
            ),
            ("tui", "queued", "tui", "/tui [mode]".to_owned()),
            ("settings", "immediate", "tui", "/settings".to_owned()),
            ("background", "immediate", "tui", "/background".to_owned()),
            ("fork", "queued", "tui", "/fork".to_owned()),
            ("sessions", "immediate", "tui", "/sessions".to_owned()),
            ("new", "queued", "tui", "/new [title...]".to_owned()),
            (
                "resume",
                "queued",
                "tui",
                "/resume [session-or-search...]".to_owned()
            ),
            ("branch", "queued", "tui", "/branch [name...]".to_owned()),
            ("rename", "queued", "tui", "/rename [title...]".to_owned()),
            ("archive", "queued", "tui", "/archive [session]".to_owned()),
            ("delete", "queued", "tui", "/delete [session]".to_owned()),
            (
                "export",
                "queued",
                "tui",
                "/export [target] [format-or-path...]".to_owned()
            ),
            (
                "rewind",
                "queued",
                "tui",
                "/rewind [turn] [files]".to_owned()
            ),
            ("voice", "immediate", "tui", "/voice [action]".to_owned()),
            (
                "mcp",
                "immediate",
                "panel-commands",
                "/mcp [action] [server]".to_owned()
            ),
            (
                "agents",
                "immediate",
                "panel-commands",
                "/agents [view]".to_owned()
            ),
            ("hooks", "immediate", "panel-commands", "/hooks".to_owned()),
            (
                "workflows",
                "immediate",
                "panel-commands",
                "/workflows".to_owned()
            ),
            (
                "list-agents",
                "immediate",
                "panel-commands",
                "/list-agents".to_owned()
            ),
            (
                "subtask",
                "model_scheduling",
                "panel-commands",
                "/subtask <task...>".to_owned()
            ),
            (
                "schedule",
                "model_scheduling",
                "panel-commands",
                "/schedule [action] [arguments...]".to_owned()
            ),
        ]
    );
    for entry in &command_catalog {
        let owner = exact
            .contributions
            .iter()
            .find(|row| {
                row.kind == heycode_core::ContributionKind::Command
                    && row.name == entry.descriptor.id()
            })
            .map(|row| row.plugin)
            .unwrap_or_else(|| panic!("missing command contribution: {}", entry.descriptor.id()));
        assert_eq!(entry.descriptor.source().plugin(), owner);
        if !entry.availability.is_available() {
            assert!(entry.availability.reason().is_some());
        }
    }
    let exact_services: std::collections::BTreeSet<_> = exact
        .contributions
        .iter()
        .filter(|row| row.kind == heycode_core::ContributionKind::Service)
        .map(|row| row.name.as_str())
        .collect();
    let live_services: std::collections::BTreeSet<_> =
        ctx.services().iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(exact_services, live_services);
    let exact_tools: Vec<_> = exact
        .contributions
        .iter()
        .filter(|row| row.kind == heycode_core::ContributionKind::Tool)
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(exact_tools, tools.names());
    let exact_commands: Vec<_> = exact
        .contributions
        .iter()
        .filter(|row| row.kind == heycode_core::ContributionKind::Command)
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(exact_commands, commands.names().unwrap());
    let mut exact_runtimes: Vec<_> = exact
        .contributions
        .iter()
        .filter(|row| row.kind == heycode_core::ContributionKind::AgentRuntime)
        .map(|row| row.name.clone())
        .collect();
    exact_runtimes.sort();
    assert_eq!(exact_runtimes, runtimes.ids().unwrap());
    let prompt = ctx
        .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
        .unwrap();
    let exact_prompt_sections: Vec<_> = exact
        .contributions
        .iter()
        .filter(|row| row.kind == heycode_core::ContributionKind::PromptSection)
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(exact_prompt_sections, prompt.names());

    let descriptors = ctx.plugin_descriptors();
    assert_eq!(descriptors.len(), ctx.plugins().len());
    assert!(descriptors.iter().all(|descriptor| {
        descriptor.source == PluginSource::BuiltIn
            && descriptor.version == env!("CARGO_PKG_VERSION")
            && !descriptor.contributions.is_empty()
    }));
    let contribution_rows: Vec<_> = descriptors
        .iter()
        .map(|descriptor| (descriptor.id, descriptor.contributions.to_vec()))
        .collect();
    assert_eq!(
        contribution_rows,
        vec![
            ("doctor", vec![Kind::Service]),
            ("doctor-config", vec![Kind::Diagnostic]),
            ("trust", vec![Kind::Service]),
            ("ui", vec![Kind::Service]),
            ("settings-file", vec![Kind::Service, Kind::Provider]),
            ("doctor-settings", vec![Kind::Diagnostic]),
            ("settings-aws-bedrock", vec![Kind::Service]),
            ("settings-google-inference", vec![Kind::Service]),
            ("http-reqwest", vec![Kind::Service]),
            ("sandbox", vec![Kind::Service]),
            ("subprocess-local", vec![Kind::Service]),
            ("session", vec![Kind::Service]),
            ("workspace-scope", vec![Kind::Service]),
            ("terminal-registry", vec![Kind::Service]),
            ("shell-local", vec![Kind::Service]),
            ("hooks", vec![Kind::Service]),
            ("filesystem-local", vec![Kind::Service]),
            ("retained-output-local", vec![Kind::Service]),
            ("lsp-registry", vec![Kind::Service]),
            ("credentials", vec![Kind::Service]),
            ("credentials-env", vec![Kind::Provider]),
            ("credentials-command", vec![Kind::Provider]),
            ("credentials-file", vec![Kind::Provider]),
            ("doctor-credentials", vec![Kind::Diagnostic]),
            ("authorization", vec![Kind::Service]),
            ("secret-prompt", vec![Kind::Service]),
            ("authorization-api-key", vec![Kind::Provider]),
            ("provider-openrouter", vec![Kind::Provider]),
            ("provider-anthropic", vec![Kind::Provider, Kind::Service]),
            ("provider-openai", vec![Kind::Provider, Kind::Service]),
            ("authorization-aws", vec![Kind::Provider, Kind::Service]),
            ("authorization-gcp", vec![Kind::Service]),
            ("provider-lmstudio", vec![Kind::Service]),
            ("onboarding", vec![Kind::Service]),
            ("profiles", vec![Kind::Service]),
            ("attachments-local", vec![Kind::Service]),
            ("session-query-jsonl", vec![Kind::Service]),
            ("prompt", vec![Kind::Service, Kind::PromptSection]),
            ("native-tools", vec![Kind::Service]),
            ("native-openai", vec![Kind::Service, Kind::Tool]),
            ("native-anthropic", vec![Kind::Service, Kind::Tool]),
            ("native-openrouter", vec![Kind::Tool]),
            ("web", vec![Kind::Service]),
            ("web-portable", vec![Kind::Provider]),
            ("web-extract", vec![Kind::Service, Kind::Provider]),
            ("web-policy", vec![Kind::Service]),
            ("tools", vec![Kind::Service, Kind::Tool, Kind::Waterfall]),
            ("interactive-tools", vec![Kind::Tool]),
            ("lsp-tools", vec![Kind::Tool]),
            ("native-tool-policy", vec![Kind::Service]),
            ("models", vec![Kind::Service]),
            ("runtimes", vec![Kind::Service]),
            ("runtime-claude", vec![Kind::Provider]),
            ("runtime-codex", vec![Kind::Provider]),
            ("runtime-opencode", vec![Kind::Provider]),
            ("runtime-grok", vec![Kind::Provider]),
            ("runtime-deepseek-harness", vec![Kind::Provider]),
            ("mcp-registry", vec![Kind::Service]),
            ("mcp-management", vec![Kind::Service]),
            ("plugin-lifecycle", vec![Kind::Service]),
            ("telemetry-local-off", vec![Kind::Service]),
            ("telemetry-metrics", vec![Kind::Diagnostic]),
            ("catalog-cache-file", vec![Kind::Provider]),
            ("catalog-overrides", vec![Kind::Service]),
            ("catalog-deepseek", vec![Kind::Provider]),
            ("catalog-openrouter", vec![Kind::Provider]),
            ("catalog-compatible", vec![Kind::Provider]),
            ("catalog-ollama", vec![Kind::Provider]),
            ("catalog-anthropic", vec![Kind::Provider]),
            ("catalog-openai", vec![Kind::Provider]),
            ("catalog-google", vec![Kind::Provider]),
            ("catalog-google-vertex", vec![Kind::Provider]),
            ("catalog-google-claude-vertex", vec![Kind::Provider]),
            ("catalog-azure-openai", vec![Kind::Provider]),
            ("catalog-custom-openai", vec![Kind::Provider]),
            ("catalog-minimax", vec![Kind::Provider]),
            ("catalog-minimax-token-plan", vec![Kind::Provider]),
            ("catalog-zai", vec![Kind::Provider]),
            ("catalog-zai-coding", vec![Kind::Provider]),
            ("catalog-lmstudio", vec![Kind::Provider, Kind::Service]),
            ("catalog-bedrock", vec![Kind::Provider]),
            ("token-counters", vec![Kind::Service, Kind::Provider]),
            ("token-count-anthropic", vec![Kind::Provider]),
            ("llm", vec![Kind::Service, Kind::Provider, Kind::Waterfall]),
            ("provider-activation", vec![Kind::Provider]),
            ("request-transforms", vec![Kind::Service, Kind::Waterfall]),
            ("request-transforms-openrouter", vec![Kind::Provider]),
            ("provider-telemetry", vec![Kind::Waterfall]),
            ("agent-options", vec![Kind::Service]),
            ("approval", vec![Kind::Service]),
            ("commands", vec![Kind::Service, Kind::Command]),
            ("lmstudio-control", vec![Kind::Service, Kind::Command]),
            ("status", vec![Kind::Service, Kind::Command]),
            ("status-context", vec![Kind::Command]),
            ("status-web", vec![Kind::Command]),
            ("health-history", vec![Kind::Service, Kind::Command]),
            ("init", vec![Kind::Command]),
            (
                "skills",
                vec![
                    Kind::Service,
                    Kind::Tool,
                    Kind::PromptSection,
                    Kind::Command
                ]
            ),
            (
                "mcp",
                vec![Kind::Tool, Kind::ExternalProcess, Kind::Service]
            ),
            ("compactions", vec![Kind::Service]),
            ("subagent", vec![Kind::Tool, Kind::Service, Kind::Provider]),
            ("subagent-codex", vec![Kind::Provider]),
            ("subagent-claude", vec![Kind::Provider]),
            (
                "product-extensions",
                vec![
                    Kind::Service,
                    Kind::PromptSection,
                    Kind::Command,
                    Kind::Provider,
                    Kind::Waterfall,
                    Kind::UserInterface,
                    Kind::ExternalProcess,
                    Kind::Tool
                ]
            ),
            (
                "config-import-resources",
                vec![Kind::Service, Kind::Command, Kind::PromptSection]
            ),
            (
                "plan",
                vec![
                    Kind::Service,
                    Kind::Tool,
                    Kind::PromptSection,
                    Kind::Command,
                    Kind::Waterfall
                ]
            ),
            ("agent", vec![Kind::Service, Kind::Tool, Kind::Waterfall]),
            ("advisor", vec![Kind::Service, Kind::Tool, Kind::Waterfall]),
            ("product-hook-attachments", vec![Kind::Waterfall]),
            ("subagent-jobs", vec![Kind::Service]),
            (
                "execution-jobs",
                vec![Kind::Service, Kind::Command, Kind::Tool]
            ),
            ("goals", vec![Kind::Service, Kind::Tool, Kind::Command]),
            ("workflows", vec![Kind::Service, Kind::Tool]),
            ("schedules", vec![Kind::Service, Kind::Tool]),
            ("teams", vec![Kind::Service, Kind::Tool]),
            ("work", vec![Kind::Service, Kind::Tool]),
            ("reviewer", vec![Kind::Service, Kind::Tool, Kind::Command]),
            ("deferred-tools", vec![Kind::Waterfall]),
            ("code-mode", vec![Kind::Tool, Kind::Command]),
            ("loop-budget-settings", vec![Kind::Service, Kind::Waterfall]),
            ("agent-attachments", vec![Kind::Command]),
            ("agent-documents", vec![Kind::Command]),
            ("runtime-native", vec![Kind::Provider]),
            ("app-server", vec![Kind::Service]),
            ("routing", vec![Kind::Service, Kind::Command]),
            ("routing-auth", vec![Kind::Command]),
            ("app-server-controls", vec![Kind::Service]),
            (
                "workspace-transitions",
                vec![Kind::Tool, Kind::Command, Kind::Service]
            ),
            ("config-import", vec![Kind::Command, Kind::Service]),
            ("memory-commands", vec![Kind::Service, Kind::Command]),
            ("autocompact", vec![Kind::Service, Kind::Command]),
            (
                "tui",
                vec![Kind::Service, Kind::UserInterface, Kind::Command]
            ),
            ("panel-commands", vec![Kind::Command]),
        ]
    );
    world.shutdown();
    assert!(ui.snapshot().unwrap().is_empty());
}

/// The service is mandatory even while effective mode is off; only its backend
/// is optional. Every subprocess injects this exact policy owner.
#[test]
fn sandbox_mounts_as_a_service_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults();
    cfg.apply_patch("sandbox.mode=workspace").unwrap();

    let ctx = match compose_world(&opts(&cfg, dir.path())) {
        Ok(ctx) => ctx,
        // A host with no sandbox backend fails loud; that is correct behavior
        // and not what this test is pinning.
        Err(e) if e.to_string().contains("no sandbox provider") => return,
        Err(e) => panic!("compose failed: {e}"),
    };
    let sandbox = ctx
        .get::<heycode_exec::SandboxService>(heycode_exec::SERVICE_SANDBOX)
        .expect("sandbox service while workspace mode is active");
    assert_eq!(
        sandbox.policy().mode,
        heycode_exec::SandboxMode::WorkspaceWrite
    );
    assert!(sandbox.backend_name().is_some());
    let sandbox_descriptor = ctx
        .plugin_descriptors()
        .iter()
        .find(|descriptor| descriptor.id == "sandbox")
        .expect("sandbox descriptor");
    assert_eq!(sandbox_descriptor.source, PluginSource::BuiltIn);
    assert_eq!(sandbox_descriptor.contributions, &[Kind::Service]);

    // Off remains on the mandatory path but advertises no OS backend.
    let cfg = Config::defaults();
    let ctx = compose_world(&opts(&cfg, dir.path())).unwrap();
    let sandbox = ctx
        .get::<heycode_exec::SandboxService>(heycode_exec::SERVICE_SANDBOX)
        .expect("off policy still owns the mandatory process path");
    assert_eq!(sandbox.policy().mode, heycode_exec::SandboxMode::Off);
    let report = sandbox.capability_report();
    assert_eq!(report.active_backend, None);
    assert!(
        report
            .choice(heycode_exec::SandboxMode::Off)
            .unwrap()
            .selectable
    );
    assert_eq!(report.available_backend, sandbox.backend_name());
    for mode in [
        heycode_exec::SandboxMode::ReadOnly,
        heycode_exec::SandboxMode::WorkspaceWrite,
    ] {
        let choice = report.choice(mode).unwrap();
        assert_eq!(choice.selectable, report.available_backend.is_some());
        if choice.selectable {
            assert_eq!(choice.network, heycode_exec::NetworkScope::Host);
        }
    }
}

#[test]
fn non_default_builtin_plugin_variants_are_described() {
    let dir = tempfile::tempdir().unwrap();

    let mut ask_config = Config::defaults();
    ask_config.approval.mode = Some(heycode_config::ApprovalMode::Ask);
    let ask_context = compose_world(&opts(&ask_config, dir.path())).unwrap();
    let ask = ask_context
        .plugin_descriptors()
        .iter()
        .find(|descriptor| descriptor.id == "approval-ask")
        .expect("interactive approval descriptor");
    assert_eq!(ask.source, PluginSource::BuiltIn);
    assert_eq!(ask.contributions, &[Kind::Service]);

    let session_dir = dir.path().join("resumable");
    let session = heycode_session::Session::create(&session_dir).unwrap();
    let resume_path = session.path().to_path_buf();
    drop(session);
    let config = Config::defaults();
    let resume_context = compose_world(&WorldOptions {
        config: &config,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().to_path_buf(),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: dir.path().join("settings.toml"),
        credentials_root: dir.path().join("credentials-home"),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: Some(fake()),
        resume: Some(resume_path),
    })
    .unwrap();
    let resume = resume_context
        .plugin_descriptors()
        .iter()
        .find(|descriptor| descriptor.id == "session-resume")
        .expect("resume descriptor");
    assert_eq!(resume.source, PluginSource::BuiltIn);
    assert_eq!(resume.contributions, &[Kind::Service]);
}

#[tokio::test]
async fn opt_in_otlp_profile_replaces_local_off_and_is_product_discoverable() {
    let document = heycode_config::ProfileDocument::from_toml(
        "schema_version = 1\n\n[[plugins]]\nid = \"telemetry-local-off\"\nenabled = false\n\n[[plugins]]\nid = \"telemetry-otlp-http\"\nenabled = true\n",
    )
    .unwrap();
    let layer = heycode_config::ProfileLayer::new(
        heycode_core::PluginScope::User,
        heycode_config::ProfileSource::named("/profiles/telemetry-otlp.toml"),
        document,
    )
    .unwrap();
    let world = RealCompositionHarness::new()
        .unwrap()
        .with_profile_layer(layer)
        .compose()
        .unwrap();
    let context = world.context();

    assert!(!context.plugins().contains(&"telemetry-local-off"));
    assert!(context.plugins().contains(&"telemetry-otlp-http"));
    assert_eq!(
        context.owner_of(heycode_telemetry::SERVICE_TELEMETRY),
        Some("telemetry-otlp-http")
    );
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(heycode_telemetry::SERVICE_TELEMETRY)
        .unwrap();
    assert_eq!(telemetry.egress(), heycode_telemetry::EgressKind::Exporting);
    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .get(&heycode_telemetry_otlp::settings_namespace().unwrap())
        .unwrap()
        .expect("selected provider publishes its settings namespace");
    assert_eq!(
        snapshot.applies(),
        heycode_settings::SettingsApplies::Restart
    );
    assert!(snapshot.wire_exposed());
    assert_eq!(
        snapshot.wire_projection().unwrap().resolved()["protocol"],
        "http/json"
    );

    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "telemetry-otlp-http"
            && row.kind == heycode_core::ContributionKind::SettingsNamespace
            && row.name == "telemetry-otlp"
    }));
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let plugins = commands.get("plugins").unwrap().unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        captured.lock().unwrap().push(event.clone());
    });
    plugins.execute(&agent, "verbose").await.unwrap();
    let rendered = events
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            heycode_agent::UiEvent::Info { text } if text.starts_with("plugins:") => {
                Some(text.clone())
            }
            _ => None,
        })
        .unwrap();
    for expected in [
        "plugin telemetry-otlp-http@",
        "service: telemetry",
        "settings_namespace: telemetry-otlp",
    ] {
        assert!(
            rendered.contains(expected),
            "missing `{expected}` in:\n{rendered}"
        );
    }
    world.shutdown();
}

/// AGENTS.md §1.1/§2/§7 promise composition order comes from
/// `[profile] plugins`. `resolve_plugins` existed but was called only by its
/// own unit tests; `compose_world` hardcoded the vector.
#[test]
fn profile_plugins_drives_composition_and_unknown_names_fail_loud() {
    let dir = tempfile::tempdir().unwrap();

    // A trimmed profile composes exactly what it lists — and nothing else.
    let mut cfg = Config::defaults();
    cfg.profile.plugins = vec![
        "session".into(),
        "prompt".into(),
        "sandbox".into(),
        "subprocess-local".into(),
        "workspace-scope".into(),
        "shell-local".into(),
        "filesystem-local".into(),
        "native-tools".into(),
        "web".into(),
        "web-portable".into(),
        "tools".into(),
        "models".into(),
        "token-counters".into(),
        "llm".into(),
        "agent-options".into(),
        "approval".into(),
        "commands".into(),
        "compactions".into(),
        "agent".into(),
    ];
    let ctx = compose_world(&opts(&cfg, dir.path())).unwrap();
    assert_eq!(ctx.plugins(), cfg.profile.plugins);
    assert!(
        ctx.get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .is_some()
    );
    assert!(
        ctx.get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
            .is_none(),
        "skills was not listed in the profile but composed anyway"
    );

    // An unknown name fails loud, naming the offender AND what is available.
    let mut cfg = Config::defaults();
    cfg.profile.plugins = vec!["session".into(), "nope".into()];
    let err = match compose_world(&opts(&cfg, dir.path())) {
        Ok(_) => panic!("unknown plugin must fail composition"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("nope"), "must name the offender: {err}");
    assert!(
        err.contains("session"),
        "must list what IS available: {err}"
    );
}

/// `AGENTS.md` / `CLAUDE.md` written by the user reach the model as the
/// `# Project instructions` prompt section when the workspace is trusted, and
/// are blocked — while `~/.heycode/AGENTS.md` still applies — when it is not.
/// Reference: Codex prepends `AGENTS.md`; Claude Code loads `CLAUDE.md`.
#[test]
fn project_instructions_reach_the_system_prompt_under_trust_and_are_blocked_without_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "ALWAYS ANSWER IN FRENCH").unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "CLAUDE RULE HERE").unwrap();
    let config = Config::defaults();

    let render = |context: &heycode_core::Context| {
        let prompt = context
            .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
            .unwrap();
        prompt.render(&heycode_prompt::RenderContext {
            cwd: dir.path().to_path_buf(),
            model: "m".to_owned(),
            tool_names: Vec::new(),
            plan_active: false,
        })
    };

    let trust = heycode_trust::WorkspaceTrustService::memory(
        dir.path(),
        heycode_cli::project_content_policy(),
    )
    .unwrap();
    trust
        .set_session(heycode_trust::WorkspaceTrustDecision::Trusted, 0)
        .unwrap();
    let mut trusted = opts(&config, dir.path());
    trusted.trust = trust;
    let mut context = compose_world(&trusted).unwrap();
    let rendered = render(&context);
    assert!(rendered.contains("# Project instructions"), "{rendered}");
    assert!(rendered.contains("## AGENTS.md"), "{rendered}");
    assert!(rendered.contains("ALWAYS ANSWER IN FRENCH"), "{rendered}");
    assert!(rendered.contains("CLAUDE RULE HERE"), "{rendered}");
    let prompt = context
        .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
        .unwrap();
    assert_eq!(
        prompt.instruction_sources(),
        vec!["AGENTS.md".to_owned(), "CLAUDE.md".to_owned()]
    );
    context.shutdown();

    // Restricted deliberately permits non-executable instructions read-only —
    // only settings and executable authority are deferred — so `--restricted-
    // workspace` still reads them.
    let restricted = heycode_trust::WorkspaceTrustService::memory(
        dir.path(),
        heycode_cli::project_content_policy(),
    )
    .unwrap();
    restricted
        .set_session(heycode_trust::WorkspaceTrustDecision::Restricted, 0)
        .unwrap();
    let mut options = opts(&config, dir.path());
    options.trust = restricted;
    let mut context = compose_world(&options).unwrap();
    assert!(render(&context).contains("ALWAYS ANSWER IN FRENCH"));
    context.shutdown();

    // An unknown workspace blocks every project input, instructions included.
    let mut context = compose_world(&opts(&config, dir.path())).unwrap();
    let rendered = render(&context);
    assert!(
        !rendered.contains("ALWAYS ANSWER IN FRENCH"),
        "an unknown workspace's instructions never reach the model: {rendered}"
    );
    assert!(
        context
            .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
            .unwrap()
            .instruction_sources()
            .is_empty()
    );
    context.shutdown();
}

/// A user adds a hook or an agent by writing one JSON file — no plugin to
/// author, install or enable. `$HEYCODE_HOME/hooks/*.json` and
/// `$HEYCODE_HOME/agents/*.json` always load; a trusted project's `.heycode/` ones
/// load too. Reference: Claude Code `hooks` settings and `.claude/agents/`.
#[cfg(unix)]
#[test]
fn file_authored_hooks_and_agents_are_registered_in_the_composed_world() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".heycode/hooks")).unwrap();
    std::fs::write(
        workspace.join(".heycode/hooks/announce.json"),
        r#"{"phase":"pre","event":"turn","action":{"type":"prompt","prompt":"Say hi first."}}"#,
    )
    .unwrap();
    let mut harness = heycode_cli::testing::RealCompositionHarness::new().unwrap();
    let home = harness.credentials_root();
    std::fs::create_dir_all(home.join("hooks")).unwrap();
    std::fs::create_dir_all(home.join("agents")).unwrap();
    std::fs::write(
        home.join("hooks/lint.json"),
        r#"{"phase":"post","event":"tool_use","action":{"type":"command","command":"cargo fmt --check"}}"#,
    )
    .unwrap();
    std::fs::write(
        home.join("agents/reviewer.json"),
        r#"{"display":"Reviewer","instructions":"Review the diff and list risks.","mode":"oneshot"}"#,
    )
    .unwrap();
    let _ = harness.config_mut();
    let mut world = harness.compose().unwrap();
    let hooks = world
        .context()
        .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
        .unwrap();
    let owners: Vec<String> = hooks
        .matching(
            heycode_hooks::HookPhase::Post,
            heycode_hooks::HookEvent::ToolUse,
        )
        .into_iter()
        .map(|hook| hook.owner.clone())
        .collect();
    assert!(owners.contains(&"user:lint".to_owned()), "{owners:?}");
    let subagents = world
        .context()
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    assert!(
        subagents
            .presets()
            .iter()
            .any(|preset| preset.id().as_str() == "user-reviewer"),
        "{:?}",
        subagents
            .presets()
            .iter()
            .map(|preset| preset.id().as_str().to_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        subagents.preset("reviewer").unwrap().instructions(),
        "Review the diff and list risks.",
        "the user alias must shadow the built-in fallback in the real world",
    );
    let inventory = world.context().plugin_inventory().snapshot().unwrap();
    assert!(
        inventory
            .contributions
            .iter()
            .any(|row| row.plugin == "subagent"
                && row.kind == heycode_core::ContributionKind::AgentPresetFallback
                && row.name == "reviewer")
    );
    assert!(
        inventory
            .contributions
            .iter()
            .any(|row| row.plugin == "product-extensions"
                && row.kind == heycode_core::ContributionKind::AgentPreset
                && row.name == "reviewer")
    );
    let declarations = world
        .context()
        .get::<heycode_extension_host::AgentDeclarationService>(
            heycode_extension_host::SERVICE_AGENT_DECLARATIONS,
        )
        .unwrap();
    std::fs::remove_file(home.join("agents/reviewer.json")).unwrap();
    declarations.reload().unwrap();
    assert!(subagents.preset("user-reviewer").is_none());
    let builtin = heycode_agent::builtin_native_presets()
        .unwrap()
        .into_iter()
        .find(|preset| preset.id().as_str() == "reviewer")
        .unwrap();
    assert_eq!(
        subagents.preset("reviewer").unwrap(),
        builtin,
        "removing a user override must reveal the untouched built-in fallback"
    );
    // Effects unwind with the context: nothing leaks past shutdown.
    world.context_mut().shutdown();
    assert!(subagents.presets().is_empty());
}

#[test]
fn production_new_provider_routes_construct_real_adapters_with_exact_credentials() {
    for provider_id in [
        "fireworks",
        "groq",
        "mistral",
        "together",
        "xai",
        "lmstudio",
        "ollama",
    ] {
        let mut harness = RealCompositionHarness::new().unwrap();
        harness.config_mut().llm.provider = provider_id.into();
        harness.config_mut().llm.model = heycode_provider_compatible::spec(provider_id)
            .map_or("local-model", |spec| spec.default_model)
            .into();
        let reference = "HEYCODE_TEST_COMPATIBLE_ROUTE_KEY";
        if provider_id != "lmstudio" {
            harness.config_mut().llm.api_key_env = Some(reference.into());
            let root = harness.credentials_root();
            std::fs::create_dir_all(&root).unwrap();
            let file = root.join("credentials.toml");
            std::fs::write(
                &file,
                format!(
                    "schema_version = 1\n[credentials]\n{reference} = \"test-only-not-live\"\n"
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        let world = harness.without_fake_provider().compose().unwrap();
        let providers = world
            .context()
            .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
            .unwrap();
        let provider = providers.get(provider_id).unwrap();
        let adapter = provider
            .inference_adapter()
            .expect("production strict adapter");
        assert_eq!(adapter.descriptor().id, provider_id);
        if provider_id != "lmstudio" {
            assert_eq!(
                adapter.authentication_binding(),
                heycode_llm::AuthenticationBinding::Credential(
                    heycode_llm::CredentialHandle::new(reference).unwrap()
                )
            );
            assert_eq!(provider.credential_reference(), Some(reference));
        }
    }
}

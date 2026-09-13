#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Real-composition PL03/PL04 activation and teardown contract.

use std::path::Path;
use std::sync::Arc;

use heycode_core::{Context, CoreResult, Plugin};
use heycode_extension_host::product_extensions_plugin;
use heycode_extensions::{
    ApiVersion, Architecture, DeclarativePackage, ManifestValidator, OperatingSystem,
    PlatformTarget, PluginInstallCache,
};

const SERVICES: &[heycode_core::ServiceKey] = &[
    heycode_skills::SERVICE_SKILLS,
    heycode_agent::SERVICE_COMMANDS,
    heycode_agent::SERVICE_SUBAGENTS,
    heycode_hooks::SERVICE_HOOKS,
    heycode_ui::SERVICE_UI,
    heycode_llm::SERVICE_PROVIDERS,
    heycode_credentials::SERVICE_CREDENTIALS,
    heycode_tools::SERVICE_TOOLS,
    heycode_exec::SERVICE_SUBPROCESS,
];

struct FixtureServices(std::path::PathBuf);

impl Plugin for FixtureServices {
    fn name(&self) -> &'static str {
        "fixture-services"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            self.name(),
            "1.0.0",
            &[heycode_core::PluginContributionKind::Service],
        )
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        SERVICES
    }

    fn apply(&self, context: &mut Context) -> CoreResult<()> {
        let shell = Arc::new(heycode_exec::ShellService::local(
            heycode_exec::LocalShellConfig::platform(
                self.0.clone(),
                std::time::Duration::from_secs(10),
            )
            .unwrap(),
        ));
        context.provide(
            heycode_skills::SERVICE_SKILLS,
            self.name(),
            heycode_skills::SkillSet::new(Vec::new()).unwrap(),
        )?;
        context.provide(
            heycode_agent::SERVICE_COMMANDS,
            self.name(),
            heycode_agent::CommandRegistry::new(),
        )?;
        context.provide(
            heycode_agent::SERVICE_SUBAGENTS,
            self.name(),
            heycode_agent::SubagentRegistry::new(),
        )?;
        context.provide(
            heycode_hooks::SERVICE_HOOKS,
            self.name(),
            heycode_hooks::HookService::new(shell, heycode_trust::WorkspaceTrustDecision::Trusted),
        )?;
        context.provide(
            heycode_ui::SERVICE_UI,
            self.name(),
            heycode_ui::UiRegistry::new(),
        )?;
        context.provide(
            heycode_llm::SERVICE_PROVIDERS,
            self.name(),
            heycode_llm::ProviderRegistry::new(),
        )?;
        context.provide(
            heycode_credentials::SERVICE_CREDENTIALS,
            self.name(),
            heycode_credentials::CredentialsService::new(),
        )?;
        context.provide(
            heycode_tools::SERVICE_TOOLS,
            self.name(),
            heycode_tools::ToolRegistry::new(),
        )?;
        context.provide(
            heycode_exec::SERVICE_SUBPROCESS,
            self.name(),
            heycode_exec::SubprocessService::local(),
        )
    }
}

fn platform() -> PlatformTarget {
    #[cfg(target_os = "macos")]
    let os = OperatingSystem::Macos;
    #[cfg(target_os = "linux")]
    let os = OperatingSystem::Linux;
    #[cfg(target_os = "freebsd")]
    let os = OperatingSystem::Freebsd;
    #[cfg(target_arch = "aarch64")]
    let architecture = Architecture::Aarch64;
    #[cfg(target_arch = "x86_64")]
    let architecture = Architecture::X86_64;
    PlatformTarget::new(os, architecture)
}

fn manifest() -> &'static str {
    r#"schema_version = 1
id = "acme/product"
name = "Product fixture"
version = "1.0.0"
description = "Concrete product activation fixture."
license = "MIT"
default_enabled = true
requested_permissions = ["network_access", "credential_use", "hook_registration", "mcp_connect", "process_spawn"]
platforms = [
  { os = "macos", architecture = "aarch64" },
  { os = "macos", architecture = "x86_64" },
  { os = "linux", architecture = "aarch64" },
  { os = "linux", architecture = "x86_64" },
  { os = "freebsd", architecture = "aarch64" },
  { os = "freebsd", architecture = "x86_64" },
]
dependencies = []
conflicts = []

[[contributions]]
kind = "skill"
id = "review"
path = "skills/review.md"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "command"
id = "review"
path = "commands/review.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "agent"
id = "reviewer"
path = "agents/reviewer.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "hook"
id = "turn-review"
path = "hooks/turn-review.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "theme"
id = "ember"
path = "themes/ember.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "provider"
id = "gateway"
path = "providers/gateway.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "mcp"
id = "fixture"
path = "mcp/fixture.json"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixture/product"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#
}

fn python_executable() -> Option<String> {
    let output = std::process::Command::new("python3")
        .args([
            "-c",
            "import os,sys;print(os.path.realpath(sys.executable))",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

fn write_package(root: &Path, python: &str) {
    let rows = [
        (
            ".heycode-plugin/plugin.toml",
            manifest().to_owned(),
            false,
        ),
        (
            "skills/review.md",
            "---\nname: ignored\ndescription: Review carefully\n---\nReview the change carefully."
                .to_owned(),
            false,
        ),
        (
            "commands/review.json",
            r#"{"description":"Review one change","prompt":"Review the supplied change."}"#
                .to_owned(),
            false,
        ),
        (
            "agents/reviewer.json",
            r#"{"display":"Careful reviewer","instructions":"Inspect correctness and safety.","mode":"oneshot"}"#
                .to_owned(),
            false,
        ),
        (
            "hooks/turn-review.json",
            r#"{"phase":"pre","event":"turn","action":{"type":"prompt","prompt":"Check the turn."}}"#
                .to_owned(),
            false,
        ),
        (
            "themes/ember.json",
            r##"{"title":"Ember","colors":{"accent":"#cc6633","success":"#44aa55","error":"#dd3344","warn":"#ddaa33","text":"#eeeeee","dim":"#888888","border":"#444444"}}"##
                .to_owned(),
            false,
        ),
        (
            "providers/gateway.json",
            r#"{"protocol":"open_ai_chat_completions","display_name":"Fixture gateway","base_url":"https://example.invalid/v1","default_model":"fixture/model","credential_reference":"FIXTURE_API_KEY","credential_kind":"api-key"}"#
                .to_owned(),
            false,
        ),
        (
            "mcp/fixture.json",
            r#"{"transport":"stdio","command":"bin/mcp-server","display_name":"Bundled fixture","tools":{"enabled":["echo","blocked"],"default_approval":"deny","approval":{"echo":"allow","blocked":"deny"}},"exposure":{"resources":false,"prompts":false,"instructions":false}}"#
                .to_owned(),
            false,
        ),
        (
            "bin/mcp-server",
            format!(
                "#!{python}\n{}",
                r#"import json,sys
for line in sys.stdin:
    request=json.loads(line); method=request.get("method"); ident=request.get("id")
    if method=="initialize": result={"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"bundled","version":"1"}}
    elif method=="tools/list": result={"tools":[{"name":n,"description":n,"inputSchema":{"type":"object","additionalProperties":False}} for n in ("echo","blocked","unlisted")]}
    elif method=="tools/call": result={"content":[{"type":"text","text":"ok"}],"isError":False}
    else: result={}
    if ident is not None:
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":ident,"result":result})+"\n"); sys.stdout.flush()
"#
            ),
            true,
        ),
    ];
    for (relative, body, executable) in rows {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        if executable {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn all_product_domains_and_bundled_mcp_activate_and_dispose() {
    let Some(python) = python_executable() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source, &python);
    let cache = PluginInstallCache::open(
        temp.path().join("cache"),
        ManifestValidator::new(ApiVersion::new(1).unwrap(), platform()),
    )
    .unwrap();
    let installed = cache.install_directory(&source).unwrap();
    let package = DeclarativePackage::load(&installed).unwrap();
    assert_eq!(package.contributions().len(), 6);
    assert_eq!(package.mcp_contributions().len(), 1);

    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        heycode_mcp::mcp_registry_plugin(),
        product_extensions_plugin(vec![package]).unwrap(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let skills = context
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let subagents = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let hooks = context
        .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
        .unwrap();
    let ui = context
        .get::<heycode_ui::UiRegistry>(heycode_ui::SERVICE_UI)
        .unwrap();
    let providers = context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let mcp = context
        .get::<heycode_mcp::McpRegistry>(heycode_mcp::SERVICE_MCP)
        .unwrap();
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();

    assert_eq!(
        skills
            .snapshot()
            .unwrap()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["acme/product::review"]
    );
    let legacy_skill_id = commands
        .names()
        .unwrap()
        .into_iter()
        .find(|id| id.starts_with("ext-review-"))
        .unwrap();
    assert_eq!(
        skills.get(&legacy_skill_id).unwrap().unwrap().name,
        "acme/product::review",
        "the former opaque invocation id remains a compatibility alias"
    );
    assert!(legacy_skill_id.starts_with("ext-review-"));
    assert_eq!(subagents.presets().len(), 1);
    assert_eq!(hooks.len(), 1);
    assert!(
        ui.themes()
            .unwrap()
            .iter()
            .any(|theme| theme.title() == "Ember")
    );
    assert!(
        providers
            .names()
            .iter()
            .any(|id| id.starts_with("ext-gateway-"))
    );
    let snapshot = mcp.snapshot().unwrap();
    assert_eq!(snapshot.servers().len(), 1);
    assert!(matches!(
        snapshot.servers()[0].state(),
        heycode_mcp::McpConnectionState::Ready { .. }
    ));
    // The bundled server advertises three tools and the manifest admits exactly one of them:
    // `echo` is allow-listed and approved, `blocked` is allow-listed but explicitly denied, and
    // `unlisted` is absent from the allow-list. Publication is the only enforcement point for a
    // bundled server, so a denied or unlisted row must never reach the model-visible registry.
    let published = tools.names();
    assert!(published.iter().any(|name| name.ends_with("__echo")));
    assert!(!published.iter().any(|name| name.ends_with("__blocked")));
    assert!(!published.iter().any(|name| name.ends_with("__unlisted")));

    let inventory = context.plugin_inventory().snapshot().unwrap();
    for kind in [
        heycode_core::ContributionKind::Skill,
        heycode_core::ContributionKind::Command,
        heycode_core::ContributionKind::AgentPreset,
        heycode_core::ContributionKind::Hook,
        heycode_core::ContributionKind::Theme,
        heycode_core::ContributionKind::InferenceProvider,
        heycode_core::ContributionKind::McpServer,
    ] {
        assert!(
            inventory
                .contributions
                .iter()
                .any(|row| { row.plugin == "product-extensions" && row.kind == kind })
        );
    }

    context.shutdown();
    assert!(skills.snapshot().unwrap().is_empty());
    assert!(
        !commands
            .names()
            .unwrap()
            .iter()
            .any(|id| id.starts_with("ext-"))
    );
    assert!(subagents.presets().is_empty());
    assert_eq!(hooks.len(), 0);
    assert!(
        !ui.themes()
            .unwrap()
            .iter()
            .any(|theme| theme.title() == "Ember")
    );
    assert!(!providers.names().iter().any(|id| id.starts_with("ext-")));
    assert!(mcp.snapshot().unwrap().servers().is_empty());
    assert!(
        !tools
            .names()
            .iter()
            .any(|name| name.starts_with("mcp__ext-"))
    );
}

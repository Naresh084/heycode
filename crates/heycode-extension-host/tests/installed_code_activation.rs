//! Installed PL09 product activation through real registries and lifecycle state.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use futures::StreamExt as _;
use heycode_core::{Context, CoreResult, Plugin};
use heycode_extension_host::{
    InstalledCodePluginAuthority, InstalledCodePluginAuthorityProvider, InstalledCodePluginError,
    InstalledCodePluginResources, ManagedCodePluginAuthorityGeneration,
    ManagedCodePluginAuthorityRule, ManagedCodePluginResourceSpec, ManagedCodePluginSessionPolicy,
    ManagedInstalledCodePluginAuthorityProvider, ManagedWasiNetworkEndpointSpec,
    ManagedWasiPreopenSpec, installed_product_extensions_plugin,
    installed_product_extensions_plugin_from_root_with_code_provider,
    installed_product_extensions_plugin_with_code,
};
use heycode_extensions::lifecycle::{
    InstalledVersions, LifecycleError, PluginLifecycle, PluginState, PluginStateStore,
    SERVICE_PLUGIN_LIFECYCLE,
};
use heycode_extensions::{
    ApiVersion, Architecture, CatalogDigest, CodePluginSessionId, ContributionKind,
    ManagedCapabilityPolicy, ManagedChecksumRequirement, ManagedPluginAdmissionGeneration,
    ManagedPluginAdmissionGenerationId, ManagedPluginPolicy, ManagedPluginPolicyRule,
    ManagedSignatureRequirement, ManifestValidator, MarketplaceCatalog, MarketplaceId,
    MarketplaceSource, MarketplaceSourceKind, OperatingSystem, PackageContentHash,
    PackageProvenance, PlatformTarget, PluginCodeRuntime, PluginId, PluginInstallCache, PluginPath,
    PluginPermission, PluginVersion, UpdateChannel, WasiPreopenAccess,
};
use heycode_llm::{ChatMessage, ChatRequest, FinishReason, StreamChunk};

const PRODUCT_SERVICES: &[heycode_core::ServiceKey] = &[
    heycode_skills::SERVICE_SKILLS,
    heycode_agent::SERVICE_COMMANDS,
    heycode_agent::SERVICE_SUBAGENTS,
    heycode_hooks::SERVICE_HOOKS,
    heycode_ui::SERVICE_UI,
    heycode_llm::SERVICE_PROVIDERS,
    heycode_credentials::SERVICE_CREDENTIALS,
    heycode_tools::SERVICE_TOOLS,
    heycode_exec::SERVICE_SUBPROCESS,
    heycode_mcp::SERVICE_MCP,
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
        PRODUCT_SERVICES
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
        )?;
        context.provide(
            heycode_mcp::SERVICE_MCP,
            self.name(),
            heycode_mcp::McpRegistry::new(),
        )
    }
}

struct FixtureStateStore(Mutex<BTreeMap<String, PluginState>>);

impl PluginStateStore for FixtureStateStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        *self.0.lock().unwrap() = states.clone();
        Ok(())
    }
}

struct FixtureVersions {
    id: PluginId,
    version: PluginVersion,
}

impl InstalledVersions for FixtureVersions {
    fn versions(&self, id: &PluginId) -> Vec<PluginVersion> {
        if id == &self.id {
            vec![self.version.clone()]
        } else {
            Vec::new()
        }
    }
}

struct FixtureLifecycle {
    id: PluginId,
    version: PluginVersion,
}

impl Plugin for FixtureLifecycle {
    fn name(&self) -> &'static str {
        "fixture-lifecycle"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            self.name(),
            "1.0.0",
            &[heycode_core::PluginContributionKind::Service],
        )
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_PLUGIN_LIFECYCLE]
    }

    fn apply(&self, context: &mut Context) -> CoreResult<()> {
        let state = PluginState {
            id: self.id.clone(),
            active: self.version.clone(),
            previous: None,
            enabled: true,
        };
        let store = Arc::new(FixtureStateStore(Mutex::new(BTreeMap::from([(
            self.id.as_str().to_owned(),
            state,
        )]))));
        let versions = Arc::new(FixtureVersions {
            id: self.id.clone(),
            version: self.version.clone(),
        });
        context.provide(
            SERVICE_PLUGIN_LIFECYCLE,
            self.name(),
            PluginLifecycle::new(store, versions),
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
id = "acme/installed-code"
name = "Installed code"
version = "1.0.0"
description = "Installed code product fixture."
license = "MIT"
default_enabled = true
requested_permissions = ["filesystem_read", "filesystem_write", "network_access", "process_spawn", "hook_registration"]
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
id = "remote"
path = "providers/remote.json"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixture/installed-code"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []

[code]
runtime = "native_process"
entrypoint = "bin/plugin"
"#
}

fn write_package(root: &Path) {
    let rows = [
        (".heycode-plugin/plugin.toml", manifest()),
        (
            "skills/review.md",
            "---\nname: ignored\ndescription: Review carefully\n---\nReview the change carefully.",
        ),
        (
            "commands/review.json",
            r#"{"description":"Run installed review code","timing":"immediate"}"#,
        ),
        (
            "agents/reviewer.json",
            r#"{"display":"Installed reviewer","instructions":"Inspect correctness and safety.","mode":"oneshot"}"#,
        ),
        (
            "hooks/turn-review.json",
            r#"{"phase":"pre","event":"turn","action":{"type":"prompt","prompt":"Check the turn."}}"#,
        ),
        (
            "themes/ember.json",
            r##"{"title":"Installed Ember","colors":{"accent":"#cc6633","success":"#44aa55","error":"#dd3344","warn":"#ddaa33","text":"#eeeeee","dim":"#888888","border":"#444444"}}"##,
        ),
        (
            "providers/remote.json",
            r#"{"protocol":"code_plugin_v1","display_name":"Installed remote","default_model":"remote/default"}"#,
        ),
    ];
    for (relative, body) in rows {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let entrypoint = root.join("bin/plugin");
    std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    let status = std::process::Command::new("rustc")
        .arg("--edition=2024")
        .arg("-O")
        .arg("-o")
        .arg(&entrypoint)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/code_plugin_process.rs"
        ))
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::set_permissions(&entrypoint, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct InstalledFixture {
    _temp: tempfile::TempDir,
    cache: PluginInstallCache,
    id: PluginId,
    version: PluginVersion,
    provenance: PackageProvenance,
}

fn installed_fixture() -> InstalledFixture {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source);
    let cache = PluginInstallCache::open(
        temp.path().join("cache"),
        ManifestValidator::new(ApiVersion::new(1).unwrap(), platform()),
    )
    .unwrap();
    let installed = cache.install_directory(source).unwrap();
    InstalledFixture {
        _temp: temp,
        id: installed.manifest().id().clone(),
        version: installed.manifest().version().clone(),
        provenance: PackageProvenance::operator_path(&installed),
        cache,
    }
}

fn installed_wasi_fixture() -> InstalledFixture {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let manifest = r#"schema_version = 1
id = "acme/installed-wasi"
name = "Installed WASI"
version = "1.0.0"
description = "Installed WASI product fixture."
license = "MIT"
default_enabled = true
requested_permissions = []
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
kind = "command"
id = "review"
path = "commands/review.json"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixture/installed-wasi"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []

[code]
runtime = "wasi_component_v1"
entrypoint = "components/plugin.wasm"
"#;
    for (relative, body) in [
        (".heycode-plugin/plugin.toml", manifest),
        (
            "commands/review.json",
            r#"{"description":"Run installed WASI review","timing":"queued"}"#,
        ),
    ] {
        let path = source.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let component = base64::engine::general_purpose::STANDARD
        .decode(include_str!("fixtures/code_plugin_component.b64").replace('\n', ""))
        .unwrap();
    let path = source.join("components/plugin.wasm");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, component).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cache = PluginInstallCache::open(
        temp.path().join("cache"),
        ManifestValidator::new(ApiVersion::new(1).unwrap(), platform()),
    )
    .unwrap();
    let installed = cache.install_directory(source).unwrap();
    InstalledFixture {
        _temp: temp,
        id: installed.manifest().id().clone(),
        version: installed.manifest().version().clone(),
        provenance: PackageProvenance::operator_path(&installed),
        cache,
    }
}

struct ManagedInstalledFixture {
    _temp: tempfile::TempDir,
    cache: PluginInstallCache,
    id: PluginId,
    version: PluginVersion,
    package_digest: PackageContentHash,
    admission: ManagedPluginAdmissionGeneration,
}

fn managed_installed_fixture() -> ManagedInstalledFixture {
    let temp = tempfile::tempdir().unwrap();
    let source_root = temp.path().join("source");
    write_package(&source_root);
    let validator = ManifestValidator::new(ApiVersion::new(1).unwrap(), platform());
    let scratch = PluginInstallCache::open(temp.path().join("scratch"), validator.clone()).unwrap();
    let inspected = scratch.install_directory(&source_root).unwrap();
    let catalog_raw = format!(
        r#"schema_version = 1
marketplace = "acme"

[[packages]]
id = "acme/installed-code"
version = "1.0.0"
source_kind = "local"
locator = "fixture/installed-code"
content = "{}"
"#,
        inspected.content_hash().as_str()
    );
    let marketplace = MarketplaceSource::new(
        MarketplaceId::new("acme").unwrap(),
        MarketplaceSourceKind::LocalDirectory,
        "catalog.toml",
        CatalogDigest::of_bytes(catalog_raw.as_bytes()).as_str(),
    )
    .unwrap();
    let catalog = MarketplaceCatalog::parse_pinned(&marketplace, catalog_raw.as_bytes()).unwrap();
    let id = PluginId::new("acme/installed-code").unwrap();
    let version = PluginVersion::parse("1.0.0").unwrap();
    let policy_rule = ManagedPluginPolicyRule::from_catalog(
        &marketplace,
        &catalog,
        &id,
        &version,
        UpdateChannel::Pinned,
        platform(),
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        ManagedCapabilityPolicy::new(
            [
                PluginPermission::FilesystemRead,
                PluginPermission::FilesystemWrite,
                PluginPermission::NetworkAccess,
                PluginPermission::ProcessSpawn,
                PluginPermission::HookRegistration,
            ],
            [
                ContributionKind::Skill,
                ContributionKind::Command,
                ContributionKind::Agent,
                ContributionKind::Hook,
                ContributionKind::Theme,
                ContributionKind::Provider,
            ],
        )
        .unwrap(),
    )
    .unwrap();
    let policy = ManagedPluginPolicy::new([policy_rule]).unwrap();
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator).unwrap();
    let receipt = cache
        .install_managed_marketplace(
            &source_root,
            &marketplace,
            &catalog,
            &id,
            &version,
            platform(),
            &policy,
        )
        .unwrap();
    let package_digest = receipt.provenance().content().clone();
    let admission =
        ManagedPluginAdmissionGeneration::new(marketplace, catalog, platform(), policy).unwrap();
    ManagedInstalledFixture {
        _temp: temp,
        cache,
        id,
        version,
        package_digest,
        admission,
    }
}

fn managed_wasi_fixture() -> ManagedInstalledFixture {
    let ordinary = installed_wasi_fixture();
    let source_root = ordinary._temp.path().join("source");
    let manifest_path = source_root.join(".heycode-plugin/plugin.toml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap().replace(
        "requested_permissions = []",
        "requested_permissions = [\"filesystem_read\", \"network_access\"]",
    );
    std::fs::write(&manifest_path, manifest).unwrap();
    let validator = ManifestValidator::new(ApiVersion::new(1).unwrap(), platform());
    let scratch = PluginInstallCache::open(
        ordinary._temp.path().join("managed-wasi-scratch"),
        validator.clone(),
    )
    .unwrap();
    let inspected = scratch.install_directory(&source_root).unwrap();
    let catalog_raw = format!(
        r#"schema_version = 1
marketplace = "acme"

[[packages]]
id = "acme/installed-wasi"
version = "1.0.0"
source_kind = "local"
locator = "fixture/installed-wasi"
content = "{}"
"#,
        inspected.content_hash().as_str()
    );
    let marketplace = MarketplaceSource::new(
        MarketplaceId::new("acme").unwrap(),
        MarketplaceSourceKind::LocalDirectory,
        "catalog.toml",
        CatalogDigest::of_bytes(catalog_raw.as_bytes()).as_str(),
    )
    .unwrap();
    let catalog = MarketplaceCatalog::parse_pinned(&marketplace, catalog_raw.as_bytes()).unwrap();
    let id = PluginId::new("acme/installed-wasi").unwrap();
    let version = PluginVersion::parse("1.0.0").unwrap();
    let policy = ManagedPluginPolicy::new([ManagedPluginPolicyRule::from_catalog(
        &marketplace,
        &catalog,
        &id,
        &version,
        UpdateChannel::Pinned,
        platform(),
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        ManagedCapabilityPolicy::new(
            [
                PluginPermission::FilesystemRead,
                PluginPermission::NetworkAccess,
            ],
            [ContributionKind::Command],
        )
        .unwrap(),
    )
    .unwrap()])
    .unwrap();
    let cache =
        PluginInstallCache::open(ordinary._temp.path().join("managed-wasi-cache"), validator)
            .unwrap();
    let receipt = cache
        .install_managed_marketplace(
            &source_root,
            &marketplace,
            &catalog,
            &id,
            &version,
            platform(),
            &policy,
        )
        .unwrap();
    let package_digest = receipt.provenance().content().clone();
    let admission =
        ManagedPluginAdmissionGeneration::new(marketplace, catalog, platform(), policy).unwrap();
    ManagedInstalledFixture {
        _temp: ordinary._temp,
        cache,
        id,
        version,
        package_digest,
        admission,
    }
}

fn managed_native_rule(fixture: &ManagedInstalledFixture) -> ManagedCodePluginAuthorityRule {
    ManagedCodePluginAuthorityRule::new(
        fixture.id.clone(),
        fixture.version.clone(),
        fixture.package_digest.clone(),
        PluginCodeRuntime::NativeProcess,
        PluginPath::parse("bin/plugin").unwrap(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::FilesystemWrite,
            PluginPermission::NetworkAccess,
            PluginPermission::ProcessSpawn,
            PluginPermission::HookRegistration,
        ],
        ManagedCodePluginSessionPolicy::UniquePerActivation,
        ManagedCodePluginResourceSpec::NativeProcess,
    )
    .unwrap()
}

fn enabled(fixture: &ManagedInstalledFixture) -> Vec<PluginState> {
    vec![PluginState {
        id: fixture.id.clone(),
        active: fixture.version.clone(),
        previous: None,
        enabled: true,
    }]
}

fn authority(provenance: PackageProvenance) -> InstalledCodePluginAuthority {
    InstalledCodePluginAuthority::new(
        provenance,
        CodePluginSessionId::from_u128(0x5151),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::FilesystemWrite,
            PluginPermission::NetworkAccess,
            PluginPermission::ProcessSpawn,
            PluginPermission::HookRegistration,
        ],
        InstalledCodePluginResources::NativeProcess,
    )
    .unwrap()
}

#[tokio::test]
async fn installed_native_code_activates_real_six_domain_rows_and_crash_retires_them() {
    let fixture = installed_fixture();
    let plugin = installed_product_extensions_plugin_with_code(
        fixture.cache,
        vec![authority(fixture.provenance)],
    )
    .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(fixture._temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        Box::new(FixtureLifecycle {
            id: fixture.id,
            version: fixture.version,
        }),
        plugin,
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

    assert_eq!(
        skills
            .snapshot()
            .unwrap()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["acme/installed-code::review"]
    );
    let command_names = commands.names().unwrap();
    assert_eq!(command_names.len(), 1);
    assert_eq!(
        skills.get(&command_names[0]).unwrap().unwrap().name,
        "acme/installed-code::review",
        "the former opaque invocation id remains a compatibility alias"
    );
    assert_eq!(subagents.presets().len(), 1);
    assert_eq!(hooks.len(), 1);
    assert!(
        ui.themes()
            .unwrap()
            .iter()
            .any(|theme| theme.title() == "Installed Ember")
    );
    assert_eq!(providers.names().len(), 1);

    let provider = providers.get(&providers.names()[0]).unwrap();
    let chunks = provider
        .stream(ChatRequest {
            model: "remote/default".to_owned(),
            messages: vec![ChatMessage::user("hello")],
            tools: None,
            temperature: None,
            max_tokens: None,
        })
        .collect::<Vec<_>>()
        .await;
    assert_eq!(chunks.len(), 3);
    assert!(matches!(
        &chunks[0],
        Ok(StreamChunk::TextDelta(text)) if text == "installed"
    ));
    assert!(matches!(
        &chunks[1],
        Ok(StreamChunk::Usage(usage))
            if usage.prompt_tokens == 3 && usage.completion_tokens == 1
    ));
    assert!(matches!(
        chunks[2],
        Ok(StreamChunk::Finish(FinishReason::Stop))
    ));

    let crashed = provider
        .stream(ChatRequest {
            model: "crash".to_owned(),
            messages: vec![ChatMessage::user("crash")],
            tools: None,
            temperature: None,
            max_tokens: None,
        })
        .collect::<Vec<_>>()
        .await;
    assert_eq!(crashed.len(), 1);
    assert!(crashed[0].is_err());
    assert!(skills.snapshot().unwrap().is_empty());
    assert!(commands.names().unwrap().is_empty());
    assert!(subagents.presets().is_empty());
    assert_eq!(hooks.len(), 0);
    assert!(
        !ui.themes()
            .unwrap()
            .iter()
            .any(|theme| theme.title() == "Installed Ember")
    );
    assert!(providers.names().is_empty());

    context.shutdown();
}

#[test]
fn enabled_code_package_without_explicit_authority_fails_before_registry_publication() {
    let fixture = installed_fixture();
    let plugin = installed_product_extensions_plugin_with_code(fixture.cache, Vec::new()).unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(fixture._temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        Box::new(FixtureLifecycle {
            id: fixture.id,
            version: fixture.version,
        }),
        plugin,
    ];
    assert!(heycode_core::compose(&plugins).is_err());
}

#[test]
fn declarative_only_installed_path_refuses_to_downgrade_a_code_package() {
    let fixture = installed_fixture();
    let plugin = installed_product_extensions_plugin(fixture.cache);
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(fixture._temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        Box::new(FixtureLifecycle {
            id: fixture.id,
            version: fixture.version,
        }),
        plugin,
    ];
    assert!(heycode_core::compose(&plugins).is_err());
}

#[test]
fn duplicate_grants_and_sessions_fail_before_activation() {
    let fixture = installed_fixture();
    assert!(
        InstalledCodePluginAuthority::new(
            fixture.provenance.clone(),
            CodePluginSessionId::from_u128(9),
            [
                PluginPermission::NetworkAccess,
                PluginPermission::NetworkAccess,
            ],
            InstalledCodePluginResources::NativeProcess,
        )
        .is_err()
    );
    let first = authority(fixture.provenance.clone());
    let second = authority(fixture.provenance);
    assert!(
        installed_product_extensions_plugin_with_code(fixture.cache, vec![first, second]).is_err()
    );
}

#[test]
fn installed_wasi_component_uses_manifest_runtime_and_empty_default_resources() {
    let fixture = installed_wasi_fixture();
    let authority = InstalledCodePluginAuthority::new(
        fixture.provenance,
        CodePluginSessionId::from_u128(0x7171),
        [],
        InstalledCodePluginResources::WasiComponentV1 {
            preopens: Vec::new(),
            network_endpoints: Vec::new(),
        },
    )
    .unwrap();
    let plugin =
        installed_product_extensions_plugin_with_code(fixture.cache, vec![authority]).unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(fixture._temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        Box::new(FixtureLifecycle {
            id: fixture.id,
            version: fixture.version,
        }),
        plugin,
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    assert_eq!(commands.names().unwrap().len(), 1);
    context.shutdown();
    assert!(commands.names().unwrap().is_empty());
}

#[test]
fn lazy_authority_provider_runs_inside_apply_with_the_enabled_lifecycle_snapshot() {
    let fixture = installed_fixture();
    let root = fixture.cache.root().to_path_buf();
    let authority = authority(fixture.provenance);
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let provider = Arc::new(
        move |_cache: &PluginInstallCache,
              enabled: &[PluginState]|
              -> Result<
            Vec<InstalledCodePluginAuthority>,
            heycode_extension_host::InstalledCodePluginError,
        > {
            provider_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(enabled.len(), 1);
            Ok(vec![authority.clone()])
        },
    );
    let plugin = installed_product_extensions_plugin_from_root_with_code_provider(
        root,
        ManifestValidator::new(ApiVersion::new(1).unwrap(), platform()),
        provider,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(fixture._temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        Box::new(FixtureLifecycle {
            id: fixture.id,
            version: fixture.version,
        }),
        plugin,
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    context.shutdown();
}

#[test]
fn managed_provider_readmits_pl08_and_mints_a_unique_session_per_activation() {
    let fixture = managed_installed_fixture();
    let generation = ManagedCodePluginAuthorityGeneration::new(
        fixture.admission.id().clone(),
        [managed_native_rule(&fixture)],
    )
    .unwrap();
    let provider =
        ManagedInstalledCodePluginAuthorityProvider::new(fixture.admission.clone(), generation)
            .unwrap();

    let first = provider
        .authorities(&fixture.cache, &enabled(&fixture))
        .unwrap();
    let second = provider
        .authorities(&fixture.cache, &enabled(&fixture))
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id(), &fixture.id);
    assert_eq!(first[0].version(), &fixture.version);
    assert_eq!(
        first[0].granted_capabilities(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::FilesystemWrite,
            PluginPermission::NetworkAccess,
            PluginPermission::ProcessSpawn,
            PluginPermission::HookRegistration,
        ]
    );
    assert_ne!(first[0].session_id(), second[0].session_id());
}

#[test]
fn managed_provider_constructor_is_zero_io_and_refuses_a_stale_pl08_generation() {
    let fixture = managed_installed_fixture();
    let stale =
        ManagedPluginAdmissionGenerationId::parse(format!("sha256:{}", "d".repeat(64))).unwrap();
    let generation =
        ManagedCodePluginAuthorityGeneration::new(stale, [managed_native_rule(&fixture)]).unwrap();

    let error =
        ManagedInstalledCodePluginAuthorityProvider::new(fixture.admission.clone(), generation)
            .unwrap_err();
    assert!(matches!(
        error,
        InstalledCodePluginError::StaleManagedGeneration
    ));
}

#[test]
fn managed_provider_fails_closed_on_missing_version_digest_entrypoint_and_capability_drift() {
    let fixture = managed_installed_fixture();

    let empty = ManagedCodePluginAuthorityGeneration::new(
        fixture.admission.id().clone(),
        Vec::<ManagedCodePluginAuthorityRule>::new(),
    )
    .unwrap();
    let provider =
        ManagedInstalledCodePluginAuthorityProvider::new(fixture.admission.clone(), empty).unwrap();
    assert!(matches!(
        provider.authorities(&fixture.cache, &enabled(&fixture)),
        Err(InstalledCodePluginError::MissingAuthority(_))
    ));

    let mismatches = [
        ManagedCodePluginAuthorityRule::new(
            fixture.id.clone(),
            PluginVersion::parse("2.0.0").unwrap(),
            fixture.package_digest.clone(),
            PluginCodeRuntime::NativeProcess,
            PluginPath::parse("bin/plugin").unwrap(),
            [PluginPermission::ProcessSpawn],
            ManagedCodePluginSessionPolicy::UniquePerActivation,
            ManagedCodePluginResourceSpec::NativeProcess,
        )
        .unwrap(),
        ManagedCodePluginAuthorityRule::new(
            fixture.id.clone(),
            fixture.version.clone(),
            PackageContentHash::parse(&format!("sha256:{}", "e".repeat(64))).unwrap(),
            PluginCodeRuntime::NativeProcess,
            PluginPath::parse("bin/plugin").unwrap(),
            [PluginPermission::ProcessSpawn],
            ManagedCodePluginSessionPolicy::UniquePerActivation,
            ManagedCodePluginResourceSpec::NativeProcess,
        )
        .unwrap(),
        ManagedCodePluginAuthorityRule::new(
            fixture.id.clone(),
            fixture.version.clone(),
            fixture.package_digest.clone(),
            PluginCodeRuntime::NativeProcess,
            PluginPath::parse("bin/other").unwrap(),
            [PluginPermission::ProcessSpawn],
            ManagedCodePluginSessionPolicy::UniquePerActivation,
            ManagedCodePluginResourceSpec::NativeProcess,
        )
        .unwrap(),
        ManagedCodePluginAuthorityRule::new(
            fixture.id.clone(),
            fixture.version.clone(),
            fixture.package_digest.clone(),
            PluginCodeRuntime::NativeProcess,
            PluginPath::parse("bin/plugin").unwrap(),
            [PluginPermission::CredentialUse],
            ManagedCodePluginSessionPolicy::UniquePerActivation,
            ManagedCodePluginResourceSpec::NativeProcess,
        )
        .unwrap(),
    ];
    let expected = ["lifecycle", "digest", "entrypoint", "capability"];
    for (rule, expected) in mismatches.into_iter().zip(expected) {
        let generation =
            ManagedCodePluginAuthorityGeneration::new(fixture.admission.id().clone(), [rule])
                .unwrap();
        let provider =
            ManagedInstalledCodePluginAuthorityProvider::new(fixture.admission.clone(), generation)
                .unwrap();
        let error = provider
            .authorities(&fixture.cache, &enabled(&fixture))
            .unwrap_err();
        let matched = matches!(
            (&error, expected),
            (InstalledCodePluginError::LifecycleMismatch(_), "lifecycle")
                | (InstalledCodePluginError::PackageDigestMismatch(_), "digest")
                | (
                    InstalledCodePluginError::EntrypointMismatch(_),
                    "entrypoint"
                )
                | (
                    InstalledCodePluginError::CapabilityMismatch(_),
                    "capability"
                )
        );
        assert!(matched, "expected {expected}, got {error:?}");
    }
}

#[test]
fn managed_provider_reaches_real_product_registries_only_after_apply() {
    let fixture = managed_installed_fixture();
    let root = fixture.cache.root().to_path_buf();
    let generation = ManagedCodePluginAuthorityGeneration::new(
        fixture.admission.id().clone(),
        [managed_native_rule(&fixture)],
    )
    .unwrap();
    let provider = Arc::new(
        ManagedInstalledCodePluginAuthorityProvider::new(fixture.admission.clone(), generation)
            .unwrap(),
    );
    let plugin = installed_product_extensions_plugin_from_root_with_code_provider(
        root,
        ManifestValidator::new(ApiVersion::new(1).unwrap(), platform()),
        provider,
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(fixture._temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        Box::new(FixtureLifecycle {
            id: fixture.id,
            version: fixture.version,
        }),
        plugin,
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    assert_eq!(commands.names().unwrap().len(), 1);
    context.shutdown();
    assert!(commands.names().unwrap().is_empty());
}

#[test]
fn managed_wasi_resources_are_exact_and_host_io_is_deferred_to_apply() {
    let fixture = managed_wasi_fixture();
    let preopen_root = fixture._temp.path().join("wasi-input");
    std::fs::create_dir(&preopen_root).unwrap();
    let endpoint =
        ManagedWasiNetworkEndpointSpec::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 443)
            .unwrap();
    let rule = ManagedCodePluginAuthorityRule::new(
        fixture.id.clone(),
        fixture.version.clone(),
        fixture.package_digest.clone(),
        PluginCodeRuntime::WasiComponentV1,
        PluginPath::parse("components/plugin.wasm").unwrap(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::NetworkAccess,
        ],
        ManagedCodePluginSessionPolicy::UniquePerActivation,
        ManagedCodePluginResourceSpec::WasiComponentV1 {
            preopens: vec![
                ManagedWasiPreopenSpec::new(&preopen_root, "/input", WasiPreopenAccess::ReadOnly)
                    .unwrap(),
            ],
            network_endpoints: vec![endpoint],
        },
    )
    .unwrap();
    let provider = ManagedInstalledCodePluginAuthorityProvider::new(
        fixture.admission.clone(),
        ManagedCodePluginAuthorityGeneration::new(fixture.admission.id().clone(), [rule]).unwrap(),
    )
    .unwrap();
    let authorities = provider
        .authorities(&fixture.cache, &enabled(&fixture))
        .unwrap();
    match authorities[0].resources() {
        InstalledCodePluginResources::WasiComponentV1 {
            preopens,
            network_endpoints,
        } => {
            assert_eq!(preopens.len(), 1);
            assert_eq!(
                preopens[0].host_path(),
                std::fs::canonicalize(&preopen_root).unwrap()
            );
            assert_eq!(preopens[0].guest_path(), "/input");
            assert_eq!(network_endpoints.len(), 1);
            assert_eq!(network_endpoints[0].host(), "203.0.113.7");
            assert_eq!(network_endpoints[0].port(), 443);
        }
        InstalledCodePluginResources::NativeProcess => panic!("WASI rule became native"),
    }

    let missing = fixture._temp.path().join("not-created");
    let deferred = ManagedWasiPreopenSpec::new(missing, "/missing", WasiPreopenAccess::ReadOnly)
        .expect("constructing the provider must not touch the host path");
    let rule = ManagedCodePluginAuthorityRule::new(
        fixture.id.clone(),
        fixture.version.clone(),
        fixture.package_digest.clone(),
        PluginCodeRuntime::WasiComponentV1,
        PluginPath::parse("components/plugin.wasm").unwrap(),
        [PluginPermission::FilesystemRead],
        ManagedCodePluginSessionPolicy::UniquePerActivation,
        ManagedCodePluginResourceSpec::WasiComponentV1 {
            preopens: vec![deferred],
            network_endpoints: Vec::new(),
        },
    )
    .unwrap();
    let provider = ManagedInstalledCodePluginAuthorityProvider::new(
        fixture.admission.clone(),
        ManagedCodePluginAuthorityGeneration::new(fixture.admission.id().clone(), [rule]).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        provider.authorities(&fixture.cache, &enabled(&fixture)),
        Err(InstalledCodePluginError::ResourceMismatch(_))
    ));
}

#[test]
fn installed_product_registers_live_agent_authoring_and_keeps_bad_files_diagnostic() {
    let fixture = installed_fixture();
    let root = fixture.cache.root().to_path_buf();
    let authority = authority(fixture.provenance);
    let provider = Arc::new(
        move |_cache: &PluginInstallCache,
              _enabled: &[PluginState]|
              -> Result<Vec<InstalledCodePluginAuthority>, InstalledCodePluginError> {
            Ok(vec![authority.clone()])
        },
    );
    let home = fixture._temp.path().join("user-home");
    std::fs::create_dir_all(home.join("agents")).unwrap();
    std::fs::write(home.join("agents/reviewer.json"),r#"{"display":"User reviewer","instructions":"Review precisely.","config":{"permissions":"read_only"}}"#).unwrap();
    std::fs::write(
        home.join("agents/broken.json"),
        r#"{"display":"Bad","instructions":"Bad","config":{"ignored_policy":true}}"#,
    )
    .unwrap();
    let plugin = heycode_extension_host::installed_product_extensions_plugin_with_user_declarations(
        root,
        ManifestValidator::new(ApiVersion::new(1).unwrap(), platform()),
        provider,
        heycode_extension_host::user_declarations::UserDeclarationRoots {
            user_home: Some(home.clone()),
            workspace: None,
        },
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FixtureServices(fixture._temp.path().to_path_buf())),
        heycode_http::http_plugin(),
        Box::new(FixtureLifecycle {
            id: fixture.id,
            version: fixture.version,
        }),
        plugin,
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    assert!(commands.get("agent-config").unwrap().is_some());
    let service = context
        .get::<heycode_extension_host::AgentDeclarationService>(
            heycode_extension_host::SERVICE_AGENT_DECLARATIONS,
        )
        .unwrap();
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    assert_eq!(
        registry.preset("reviewer").unwrap().display(),
        "User reviewer"
    );
    assert!(
        service
            .diagnostics()
            .iter()
            .any(|row| row.contains("ignored_policy"))
    );
    assert!(service.reload().is_err());
    std::fs::remove_file(home.join("agents/broken.json")).unwrap();
    service
        .save(
            "second",
            r#"{"display":"Second","instructions":"Second instructions"}"#,
            false,
        )
        .unwrap();
    service.reload().unwrap();
    assert!(registry.preset("second").is_some());
    context.shutdown();
    assert!(registry.preset("reviewer").is_none());
    assert!(commands.get("agent-config").unwrap().is_none());
    assert!(service.reload().is_err());
}

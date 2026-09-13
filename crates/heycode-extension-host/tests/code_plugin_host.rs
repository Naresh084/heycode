//! PL09 product-host generation transaction over the host-neutral protocol.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_core::{Context, PluginContributionSpec, compose};
use heycode_exec::SubprocessService;
use heycode_extension_host::{
    CodePluginInvocation, CodePluginProductAdapter, CodePluginProductRegistration,
    HeycodeExecCodePluginLauncher, ProductCodePluginHost,
};
use heycode_extensions::{
    ApiVersion, Architecture, CodePluginCancellationToken as CancellationToken,
    CodePluginCapabilityGrants, CodePluginContribution, CodePluginExit, CodePluginInvocationError,
    CodePluginLaunchSpec, CodePluginLauncher, CodePluginPackage, CodePluginProcess,
    CodePluginSessionId, CodePluginTransportFault, ContributionKind, HostActivationFailure,
    ManifestValidator, OperatingSystem, PackageProvenance, PlatformTarget, PluginInstallCache,
    PluginPermission, code_plugin_activation_plugin,
};
use serde_json::{Value, json};

fn validator() -> ManifestValidator {
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
    ManifestValidator::new(
        ApiVersion::new(1).unwrap(),
        PlatformTarget::new(os, architecture),
    )
}

fn manifest() -> &'static str {
    r#"schema_version = 1
id = "acme/code-host"
name = "Code host"
version = "1.0.0"
description = "Product host transaction fixture."
license = "MIT"
default_enabled = true
requested_permissions = ["filesystem_read", "filesystem_write", "network_access", "process_spawn"]
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
path = "skills/review/SKILL.md"
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
id = "pre-review"
path = "hooks/pre-review.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "theme"
id = "sunset"
path = "themes/sunset.json"
exposure = { mode = "namespaced" }

[[contributions]]
kind = "provider"
id = "example"
path = "providers/example.json"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixtures/code-host"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#
}

fn package() -> (tempfile::TempDir, CodePluginPackage) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir_all(source.join(".heycode-plugin")).unwrap();
    std::fs::write(source.join(".heycode-plugin/plugin.toml"), manifest()).unwrap();
    for (relative, body) in [
        ("skills/review/SKILL.md", "Review carefully."),
        ("commands/review.json", "{}"),
        ("agents/reviewer.json", "{}"),
        ("hooks/pre-review.json", "{}"),
        ("themes/sunset.json", "{}"),
        ("providers/example.json", "{}"),
    ] {
        let path = source.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let entrypoint = source.join("bin/plugin");
    std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    std::fs::write(&entrypoint, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&entrypoint, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator()).unwrap();
    let installed = cache.install_directory(&source).unwrap();
    let provenance = PackageProvenance::operator_path(&installed);
    let package = CodePluginPackage::load(
        &cache,
        installed.manifest().id(),
        installed.manifest().version(),
        &provenance,
        "bin/plugin",
    )
    .unwrap();
    (temp, package)
}

fn real_package() -> (tempfile::TempDir, CodePluginPackage) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir_all(source.join(".heycode-plugin")).unwrap();
    std::fs::write(source.join(".heycode-plugin/plugin.toml"), manifest()).unwrap();
    for (relative, body) in [
        ("skills/review/SKILL.md", "Review carefully."),
        ("commands/review.json", "{}"),
        ("agents/reviewer.json", "{}"),
        ("hooks/pre-review.json", "{}"),
        ("themes/sunset.json", "{}"),
        ("providers/example.json", "{}"),
    ] {
        let path = source.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let entrypoint = source.join("bin/plugin");
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
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator()).unwrap();
    let installed = cache.install_directory(&source).unwrap();
    let provenance = PackageProvenance::operator_path(&installed);
    let package = CodePluginPackage::load(
        &cache,
        installed.manifest().id(),
        installed.manifest().version(),
        &provenance,
        "bin/plugin",
    )
    .unwrap();
    (temp, package)
}

type ExitListener = Arc<dyn Fn(CodePluginExit) + Send + Sync>;

struct FakeProcess {
    listener: Mutex<Option<ExitListener>>,
    shutdowns: AtomicUsize,
}

impl FakeProcess {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            listener: Mutex::new(None),
            shutdowns: AtomicUsize::new(0),
        })
    }

    fn crash(&self) {
        if let Some(listener) = self.listener.lock().unwrap().clone() {
            listener(CodePluginExit::Crashed);
        }
    }
}

impl CodePluginProcess for FakeProcess {
    fn exchange(
        &self,
        request: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, CodePluginTransportFault> {
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        let request: Value =
            serde_json::from_slice(request).map_err(|_| CodePluginTransportFault::Protocol)?;
        let response = match request["kind"].as_str() {
            Some("initialize") => json!({
                "protocol_version": request["protocol_version"],
                "kind": "ready",
                "session_id": request["session_id"],
                "package_id": request["package_id"],
                "package_version": request["package_version"],
                "package_digest": request["package_digest"],
                "executable_digest": request["executable_digest"],
                "accepted_capabilities": request["granted_capabilities"],
                "contributions": request["contributions"],
            }),
            Some("invoke") => json!({
                "protocol_version": request["protocol_version"],
                "kind": "result",
                "session_id": request["session_id"],
                "request_id": request["request_id"],
                "output": {"ok": true},
            }),
            _ => return Err(CodePluginTransportFault::Protocol),
        };
        serde_json::to_vec(&response).map_err(|_| CodePluginTransportFault::Protocol)
    }

    fn set_exit_listener(
        &self,
        listener: Arc<dyn Fn(CodePluginExit) + Send + Sync>,
    ) -> Result<(), CodePluginTransportFault> {
        *self.listener.lock().unwrap() = Some(listener);
        Ok(())
    }

    fn shutdown(&self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

struct FakeLauncher(Arc<FakeProcess>);

impl CodePluginLauncher for FakeLauncher {
    fn launch(
        &self,
        _spec: &CodePluginLaunchSpec,
        cancellation: &CancellationToken,
    ) -> Result<Arc<dyn CodePluginProcess>, CodePluginTransportFault> {
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        Ok(Arc::clone(&self.0) as Arc<dyn CodePluginProcess>)
    }
}

struct FakeRegistration {
    live: Arc<AtomicBool>,
    withdrawals: Arc<AtomicUsize>,
}

impl CodePluginProductRegistration for FakeRegistration {
    fn withdraw(self: Box<Self>) {
        self.live.store(false, Ordering::SeqCst);
        self.withdrawals.fetch_add(1, Ordering::SeqCst);
    }
}

struct FakeAdapter {
    kind: ContributionKind,
    fail: bool,
    live: Arc<AtomicBool>,
    withdrawals: Arc<AtomicUsize>,
    invocation: Arc<Mutex<Option<CodePluginInvocation>>>,
}

impl CodePluginProductAdapter for FakeAdapter {
    fn kind(&self) -> ContributionKind {
        self.kind
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        let kind = match contribution.kind() {
            ContributionKind::Skill => heycode_core::ContributionKind::Skill,
            ContributionKind::Command => heycode_core::ContributionKind::Command,
            ContributionKind::Agent => heycode_core::ContributionKind::AgentPreset,
            ContributionKind::Hook => heycode_core::ContributionKind::Hook,
            ContributionKind::Theme => heycode_core::ContributionKind::Theme,
            ContributionKind::Provider => heycode_core::ContributionKind::InferenceProvider,
            ContributionKind::Mcp => heycode_core::ContributionKind::McpServer,
        };
        vec![PluginContributionSpec::new(
            kind,
            contribution.public_name(),
        )]
    }

    fn register_inactive(
        &self,
        _context: &Context,
        _contribution: &CodePluginContribution,
        invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure> {
        if self.fail {
            return Err(HostActivationFailure::InvalidDefinition);
        }
        assert!(!invocation.is_active());
        self.live.store(true, Ordering::SeqCst);
        *self.invocation.lock().unwrap() = Some(invocation);
        Ok(Box::new(FakeRegistration {
            live: Arc::clone(&self.live),
            withdrawals: Arc::clone(&self.withdrawals),
        }))
    }
}

struct AdapterFixture {
    adapters: Vec<Arc<dyn CodePluginProductAdapter>>,
    live: BTreeMap<ContributionKind, Arc<AtomicBool>>,
    withdrawals: BTreeMap<ContributionKind, Arc<AtomicUsize>>,
    invocations: BTreeMap<ContributionKind, Arc<Mutex<Option<CodePluginInvocation>>>>,
}

fn adapters(failing: Option<ContributionKind>) -> AdapterFixture {
    let mut rows: Vec<Arc<dyn CodePluginProductAdapter>> = Vec::new();
    let mut live = BTreeMap::new();
    let mut withdrawals = BTreeMap::new();
    let mut invocations = BTreeMap::new();
    for kind in [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ] {
        let row_live = Arc::new(AtomicBool::new(false));
        let row_withdrawals = Arc::new(AtomicUsize::new(0));
        let invocation = Arc::new(Mutex::new(None));
        rows.push(Arc::new(FakeAdapter {
            kind,
            fail: failing == Some(kind),
            live: Arc::clone(&row_live),
            withdrawals: Arc::clone(&row_withdrawals),
            invocation: Arc::clone(&invocation),
        }));
        live.insert(kind, row_live);
        withdrawals.insert(kind, row_withdrawals);
        invocations.insert(kind, invocation);
    }
    AdapterFixture {
        adapters: rows,
        live,
        withdrawals,
        invocations,
    }
}

#[test]
fn product_host_commits_once_then_crash_closes_every_proxy_before_withdrawal() {
    let (_temp, package) = package();
    let process = FakeProcess::new();
    let grants = CodePluginCapabilityGrants::deny_all(&package);
    let prepared = package
        .launch(
            grants,
            CodePluginSessionId::from_u128(1),
            Arc::new(FakeLauncher(Arc::clone(&process))),
            &CancellationToken::new(),
        )
        .unwrap();
    let fixture = adapters(None);
    let host = ProductCodePluginHost::new(&[], fixture.adapters).unwrap();
    let plugin = code_plugin_activation_plugin(vec![prepared], Arc::new(host)).unwrap();
    let mut context = compose(&[plugin]).unwrap();

    for kind in [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ] {
        assert!(fixture.live[&kind].load(Ordering::SeqCst));
        let invocation = fixture.invocations[&kind].lock().unwrap().clone().unwrap();
        assert!(invocation.is_active());
    }

    process.crash();
    for kind in [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ] {
        assert!(!fixture.live[&kind].load(Ordering::SeqCst));
        assert_eq!(fixture.withdrawals[&kind].load(Ordering::SeqCst), 1);
        assert!(
            !fixture.invocations[&kind]
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_active()
        );
    }
    context.shutdown();
    assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
}

#[test]
fn product_host_refusal_rolls_back_prepared_prefix_without_committing_it() {
    let (_temp, package) = package();
    let process = FakeProcess::new();
    let grants = CodePluginCapabilityGrants::deny_all(&package);
    let prepared = package
        .launch(
            grants,
            CodePluginSessionId::from_u128(2),
            Arc::new(FakeLauncher(Arc::clone(&process))),
            &CancellationToken::new(),
        )
        .unwrap();
    let fixture = adapters(Some(ContributionKind::Command));
    let host = ProductCodePluginHost::new(&[], fixture.adapters).unwrap();
    let plugin = code_plugin_activation_plugin(vec![prepared], Arc::new(host)).unwrap();

    assert!(compose(&[plugin]).is_err());
    assert!(!fixture.live[&ContributionKind::Skill].load(Ordering::SeqCst));
    assert_eq!(
        fixture.withdrawals[&ContributionKind::Skill].load(Ordering::SeqCst),
        1
    );
    let invocation = fixture.invocations[&ContributionKind::Skill]
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert!(!invocation.is_active());
    assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
}

#[test]
fn heycode_exec_launcher_frames_real_process_and_withdraws_all_six_domains() {
    let (_temp, package) = real_package();
    let grants = CodePluginCapabilityGrants::new(
        &package,
        [
            PluginPermission::FilesystemRead,
            PluginPermission::FilesystemWrite,
            PluginPermission::NetworkAccess,
            PluginPermission::ProcessSpawn,
        ],
    )
    .unwrap();
    let prepared = package
        .launch(
            grants,
            CodePluginSessionId::from_u128(3),
            Arc::new(HeycodeExecCodePluginLauncher::new(
                SubprocessService::local(),
            )),
            &CancellationToken::new(),
        )
        .unwrap();
    let fixture = adapters(None);
    let host = ProductCodePluginHost::new(&[], fixture.adapters).unwrap();
    let plugin = code_plugin_activation_plugin(vec![prepared], Arc::new(host)).unwrap();
    let mut context = compose(&[plugin]).unwrap();

    let invocation = fixture.invocations[&ContributionKind::Command]
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(
        invocation
            .invoke(
                "ping",
                json!({"private": "request"}),
                &CancellationToken::new()
            )
            .unwrap(),
        json!({"ok": true})
    );
    for kind in [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ] {
        assert!(fixture.live[&kind].load(Ordering::SeqCst));
    }

    context.shutdown();
    for kind in [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ] {
        assert!(!fixture.live[&kind].load(Ordering::SeqCst));
        assert_eq!(fixture.withdrawals[&kind].load(Ordering::SeqCst), 1);
    }
}

#[test]
fn caller_cancellation_reaps_descendants_before_atomic_generation_retirement_returns() {
    let (temp, package) = real_package();
    let grants = CodePluginCapabilityGrants::new(
        &package,
        [
            PluginPermission::FilesystemRead,
            PluginPermission::FilesystemWrite,
            PluginPermission::NetworkAccess,
            PluginPermission::ProcessSpawn,
        ],
    )
    .unwrap();
    let prepared = package
        .launch(
            grants,
            CodePluginSessionId::from_u128(4),
            Arc::new(HeycodeExecCodePluginLauncher::new(
                SubprocessService::local(),
            )),
            &CancellationToken::new(),
        )
        .unwrap();
    let fixture = adapters(None);
    let host = ProductCodePluginHost::new(&[], fixture.adapters).unwrap();
    let plugin = code_plugin_activation_plugin(vec![prepared], Arc::new(host)).unwrap();
    let mut context = compose(&[plugin]).unwrap();
    let invocation = fixture.invocations[&ContributionKind::Command]
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    let ready = temp.path().join("descendant-ready");
    let survived = temp.path().join("descendant-survived");
    let cancellation = CancellationToken::new();
    let caller_cancellation = cancellation.clone();
    let ready_input = ready.to_string_lossy().into_owned();
    let survived_input = survived.to_string_lossy().into_owned();
    let caller = std::thread::spawn(move || {
        invocation.invoke(
            "block-descendant",
            json!({"ready": ready_input, "survived": survived_input}),
            &caller_cancellation,
        )
    });
    for _ in 0..200 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "fixture descendant never reached admission");
    cancellation.cancel();
    let error = caller.join().unwrap().unwrap_err();
    assert!(matches!(
        error,
        CodePluginInvocationError::Cancelled
            | CodePluginInvocationError::Transport(CodePluginTransportFault::Cancelled)
            | CodePluginInvocationError::Closed
    ));
    for kind in [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ] {
        assert!(!fixture.live[&kind].load(Ordering::SeqCst));
        assert_eq!(fixture.withdrawals[&kind].load(Ordering::SeqCst), 1);
    }
    std::thread::sleep(Duration::from_millis(2300));
    assert!(!survived.exists(), "cancelled plugin descendant escaped");
    context.shutdown();
}

#[test]
fn hostile_length_prefix_retires_the_complete_generation() {
    let (_temp, package) = real_package();
    let grants = CodePluginCapabilityGrants::new(
        &package,
        [
            PluginPermission::FilesystemRead,
            PluginPermission::FilesystemWrite,
            PluginPermission::NetworkAccess,
            PluginPermission::ProcessSpawn,
        ],
    )
    .unwrap();
    let prepared = package
        .launch(
            grants,
            CodePluginSessionId::from_u128(5),
            Arc::new(HeycodeExecCodePluginLauncher::new(
                SubprocessService::local(),
            )),
            &CancellationToken::new(),
        )
        .unwrap();
    let fixture = adapters(None);
    let host = ProductCodePluginHost::new(&[], fixture.adapters).unwrap();
    let plugin = code_plugin_activation_plugin(vec![prepared], Arc::new(host)).unwrap();
    let mut context = compose(&[plugin]).unwrap();
    let invocation = fixture.invocations[&ContributionKind::Command]
        .lock()
        .unwrap()
        .clone()
        .unwrap();

    assert!(matches!(
        invocation
            .invoke("oversized-frame", json!({}), &CancellationToken::new())
            .unwrap_err(),
        CodePluginInvocationError::Transport(CodePluginTransportFault::Protocol)
    ));
    for kind in [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ] {
        assert!(!fixture.live[&kind].load(Ordering::SeqCst));
        assert_eq!(fixture.withdrawals[&kind].load(Ordering::SeqCst), 1);
    }
    context.shutdown();
}

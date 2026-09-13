//! PL10 Component Model ABI and capability-isolation boundary.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use heycode_extensions::CodePluginCancellationToken as CancellationToken;
use heycode_extensions::{
    ApiVersion, Architecture, CodePluginCapabilityGrants, CodePluginExit, CodePluginProcess,
    CodePluginSessionId, CodePluginTransportFault, ManifestValidator, OperatingSystem,
    PackageProvenance, PlatformTarget, PluginInstallCache, PluginPermission,
    WASI_CODE_PLUGIN_WIT_V1, WasiComponentAbiReport, WasiComponentCapabilityPolicy,
    WasiComponentEngine, WasiComponentEngineFault, WasiComponentError, WasiComponentInstance,
    WasiComponentInstantiation, WasiComponentPackage, WasiComponentWorld, WasiNetworkEndpoint,
    WasiPreopen, WasiPreopenAccess,
};
use serde_json::{Value, json};

const COMPONENT_HEADER: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

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
id = "acme/wasi-reviewer"
name = "WASI reviewer"
version = "1.0.0"
description = "Component Model fixture."
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
kind = "command"
id = "review"
path = "commands/review.json"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixtures/wasi-reviewer"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#
}

fn installed_component(
    component_bytes: &[u8],
) -> (
    tempfile::TempDir,
    PluginInstallCache,
    heycode_extensions::InstalledPlugin,
) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir_all(source.join(".heycode-plugin")).unwrap();
    std::fs::write(source.join(".heycode-plugin/plugin.toml"), manifest()).unwrap();
    std::fs::create_dir_all(source.join("commands")).unwrap();
    std::fs::write(source.join("commands/review.json"), "{}").unwrap();
    std::fs::create_dir_all(source.join("components")).unwrap();
    let component = source.join("components/plugin.wasm");
    std::fs::write(&component, component_bytes).unwrap();
    std::fs::set_permissions(&component, std::fs::Permissions::from_mode(0o600)).unwrap();
    let dylib = source.join("components/plugin.dylib");
    std::fs::write(&dylib, b"not-a-component").unwrap();
    std::fs::set_permissions(&dylib, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator()).unwrap();
    let installed = cache.install_directory(&source).unwrap();
    (temp, cache, installed)
}

fn component_package() -> (tempfile::TempDir, WasiComponentPackage) {
    let (temp, cache, installed) = installed_component(&COMPONENT_HEADER);
    let provenance = PackageProvenance::operator_path(&installed);
    let package = WasiComponentPackage::load(
        &cache,
        installed.manifest().id(),
        installed.manifest().version(),
        &provenance,
        "components/plugin.wasm",
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct EngineObservation {
    world: WasiComponentWorld,
    component_bytes: usize,
    preopens: usize,
    endpoints: usize,
    inherits_environment: bool,
    inherits_arguments: bool,
    inherits_stdio: bool,
    safe_debug: String,
}

struct FakeEngine {
    process: Arc<FakeProcess>,
    mismatch: bool,
    observations: Mutex<Vec<EngineObservation>>,
}

struct SubsetAbiEngine {
    process: Arc<FakeProcess>,
}

impl WasiComponentEngine for SubsetAbiEngine {
    fn instantiate(
        &self,
        request: &WasiComponentInstantiation,
        _cancellation: &CancellationToken,
    ) -> Result<WasiComponentInstance, WasiComponentEngineFault> {
        let report = WasiComponentAbiReport::new(
            request.world().as_str(),
            request.wit_digest().as_str(),
            ["wasi:filesystem/types@0.3.1"],
            ["dshx:code-plugin/plugin@1.0.0"],
        )
        .map_err(|_| WasiComponentEngineFault::InvalidComponent)?;
        Ok(WasiComponentInstance::new(
            report,
            Arc::clone(&self.process) as Arc<dyn CodePluginProcess>,
        ))
    }
}

impl FakeEngine {
    fn new(process: Arc<FakeProcess>, mismatch: bool) -> Arc<Self> {
        Arc::new(Self {
            process,
            mismatch,
            observations: Mutex::new(Vec::new()),
        })
    }
}

impl WasiComponentEngine for FakeEngine {
    fn instantiate(
        &self,
        request: &WasiComponentInstantiation,
        cancellation: &CancellationToken,
    ) -> Result<WasiComponentInstance, WasiComponentEngineFault> {
        if cancellation.is_cancelled() {
            return Err(WasiComponentEngineFault::Cancelled);
        }
        self.observations.lock().unwrap().push(EngineObservation {
            world: request.world(),
            component_bytes: request.component_bytes().len(),
            preopens: request.preopens().len(),
            endpoints: request.network_endpoints().len(),
            inherits_environment: request.inherits_environment(),
            inherits_arguments: request.inherits_arguments(),
            inherits_stdio: request.inherits_stdio(),
            safe_debug: format!("{request:?}"),
        });
        let report = if self.mismatch {
            WasiComponentAbiReport::new(
                request.world().as_str(),
                request.wit_digest().as_str(),
                ["wasi:cli/environment@0.2.8"],
                ["dshx:code-plugin/plugin@1.0.0"],
            )
            .map_err(|_| WasiComponentEngineFault::InvalidComponent)?
        } else {
            WasiComponentAbiReport::exact_v1(request.world())
        };
        Ok(WasiComponentInstance::new(
            report,
            Arc::clone(&self.process) as Arc<dyn CodePluginProcess>,
        ))
    }
}

#[test]
fn wit_v1_and_component_layer_are_exact_and_a_dylib_is_not_a_component() {
    assert!(WASI_CODE_PLUGIN_WIT_V1.contains("package dshx:code-plugin@1.0.0;"));
    assert!(WASI_CODE_PLUGIN_WIT_V1.contains("world code-plugin"));
    assert!(WASI_CODE_PLUGIN_WIT_V1.contains("world code-plugin-filesystem-network"));
    assert!(WASI_CODE_PLUGIN_WIT_V1.contains("include wasi:filesystem/imports@0.3.1;"));
    assert!(WASI_CODE_PLUGIN_WIT_V1.contains("include wasi:sockets/imports@0.3.1;"));
    assert!(!WASI_CODE_PLUGIN_WIT_V1.contains("@0.2."));

    let (_temp, cache, installed) = installed_component(&COMPONENT_HEADER);
    let provenance = PackageProvenance::operator_path(&installed);
    assert!(
        WasiComponentPackage::load(
            &cache,
            installed.manifest().id(),
            installed.manifest().version(),
            &provenance,
            "components/plugin.wasm",
        )
        .is_ok()
    );
    assert!(matches!(
        WasiComponentPackage::load(
            &cache,
            installed.manifest().id(),
            installed.manifest().version(),
            &provenance,
            "components/plugin.dylib",
        ),
        Err(WasiComponentError::InvalidComponent)
    ));

    let (_other_temp, other_cache, other) = installed_component(b"\0asm\x01\0\0\0");
    let other_provenance = PackageProvenance::operator_path(&other);
    assert!(matches!(
        WasiComponentPackage::load(
            &other_cache,
            other.manifest().id(),
            other.manifest().version(),
            &other_provenance,
            "components/plugin.wasm",
        ),
        Err(WasiComponentError::InvalidComponent)
    ));
}

#[test]
fn deny_all_selects_the_pure_world_and_inherits_no_ambient_authority() {
    let (temp, package) = component_package();
    let grants = CodePluginCapabilityGrants::deny_all(package.code_package());
    let policy = WasiComponentCapabilityPolicy::deny_all(&package);
    let process = FakeProcess::new();
    let engine = FakeEngine::new(Arc::clone(&process), false);
    let _prepared = package
        .launch(
            grants,
            policy,
            CodePluginSessionId::from_u128(7),
            Arc::clone(&engine) as Arc<dyn WasiComponentEngine>,
            &CancellationToken::new(),
        )
        .unwrap();
    let observations = engine.observations.lock().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0],
        EngineObservation {
            world: WasiComponentWorld::Pure,
            component_bytes: COMPONENT_HEADER.len(),
            preopens: 0,
            endpoints: 0,
            inherits_environment: false,
            inherits_arguments: false,
            inherits_stdio: false,
            safe_debug: observations[0].safe_debug.clone(),
        }
    );
    assert!(
        !observations[0]
            .safe_debug
            .contains(temp.path().to_string_lossy().as_ref())
    );
    assert!(!observations[0].safe_debug.contains("api.example.com"));
}

#[test]
fn capability_policy_requires_scoped_resources_and_refuses_process_spawn() {
    let (temp, package) = component_package();
    let read_root = temp.path().join("read");
    let write_root = temp.path().join("write");
    std::fs::create_dir_all(&read_root).unwrap();
    std::fs::create_dir_all(&write_root).unwrap();
    let read = WasiPreopen::new(read_root, "/package", WasiPreopenAccess::ReadOnly).unwrap();
    let write = WasiPreopen::new(write_root, "/scratch", WasiPreopenAccess::ReadWrite).unwrap();
    let endpoint = WasiNetworkEndpoint::new("api.example.com", 443).unwrap();

    let read_grant =
        CodePluginCapabilityGrants::new(package.code_package(), [PluginPermission::FilesystemRead])
            .unwrap();
    assert!(matches!(
        WasiComponentCapabilityPolicy::new(&package, &read_grant, [], []),
        Err(WasiComponentError::MissingCapabilityResource(
            PluginPermission::FilesystemRead
        ))
    ));

    let deny_all = CodePluginCapabilityGrants::deny_all(package.code_package());
    assert!(matches!(
        WasiComponentCapabilityPolicy::new(&package, &deny_all, [read.clone()], []),
        Err(WasiComponentError::UnrequestedCapability(
            PluginPermission::FilesystemRead
        ))
    ));

    let process =
        CodePluginCapabilityGrants::new(package.code_package(), [PluginPermission::ProcessSpawn])
            .unwrap();
    assert!(matches!(
        WasiComponentCapabilityPolicy::new(&package, &process, [], []),
        Err(WasiComponentError::UnsupportedCapability(
            PluginPermission::ProcessSpawn
        ))
    ));

    let exact = CodePluginCapabilityGrants::new(
        package.code_package(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::FilesystemWrite,
            PluginPermission::NetworkAccess,
        ],
    )
    .unwrap();
    let policy =
        WasiComponentCapabilityPolicy::new(&package, &exact, [read, write], [endpoint]).unwrap();
    assert_eq!(policy.world(), WasiComponentWorld::FilesystemNetwork);
    let debug = format!("{policy:?}");
    assert!(!debug.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!debug.contains("api.example.com"));
}

#[test]
fn runtime_abi_mismatch_shuts_the_instance_before_pl09_handshake() {
    let (_temp, package) = component_package();
    let grants = CodePluginCapabilityGrants::deny_all(package.code_package());
    let policy = WasiComponentCapabilityPolicy::deny_all(&package);
    let process = FakeProcess::new();
    let engine = FakeEngine::new(Arc::clone(&process), true);
    assert!(matches!(
        package.launch(
            grants,
            policy,
            CodePluginSessionId::from_u128(9),
            engine as Arc<dyn WasiComponentEngine>,
            &CancellationToken::new(),
        ),
        Err(WasiComponentError::Transport(
            CodePluginTransportFault::Protocol
        ))
    ));
    assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
}

#[test]
fn actual_component_imports_may_be_a_strict_subset_of_the_selected_world() {
    let (temp, package) = component_package();
    let root = temp.path().join("read");
    std::fs::create_dir_all(&root).unwrap();
    let grants = CodePluginCapabilityGrants::new(
        package.code_package(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::NetworkAccess,
        ],
    )
    .unwrap();
    let policy = WasiComponentCapabilityPolicy::new(
        &package,
        &grants,
        [WasiPreopen::new(root, "/read", WasiPreopenAccess::ReadOnly).unwrap()],
        [WasiNetworkEndpoint::new("127.0.0.1", 9).unwrap()],
    )
    .unwrap();
    let process = FakeProcess::new();
    let prepared = package
        .launch(
            grants,
            policy,
            CodePluginSessionId::from_u128(10),
            Arc::new(SubsetAbiEngine { process }),
            &CancellationToken::new(),
        )
        .unwrap();
    drop(prepared);
}

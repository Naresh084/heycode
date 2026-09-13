//! Real Wasmtime PL10 engine and checked-in WIT round-trip.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use heycode_core::{Context, PluginContributionSpec, ServiceKey, compose};
use heycode_extension_host::WasmtimeWasiComponentEngine;
use heycode_extensions::{
    ApiVersion, Architecture, CodePluginCancellationToken as CancellationToken, CodePluginClient,
    CodePluginContribution, CodePluginContributionGeneration, CodePluginContributionHost,
    CodePluginInvocationError, CodePluginSessionId, CodePluginTransportFault, ContributionKind,
    HostActivationFailure, ManifestValidator, OperatingSystem, PackageProvenance, PlatformTarget,
    PluginInstallCache, WasiComponentCapabilityPolicy, WasiComponentEngine, WasiComponentPackage,
    WasiNetworkEndpoint, WasiPreopen, WasiPreopenAccess, code_plugin_activation_plugin,
};
use serde_json::json;

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
id = "acme/wasmtime-reviewer"
name = "Wasmtime reviewer"
version = "1.0.0"
description = "Real Component Model fixture."
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
locator = "fixtures/wasmtime-reviewer"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#
}

fn component_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(include_str!("fixtures/code_plugin_component.b64").replace('\n', ""))
        .unwrap()
}

fn scoped_component_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(include_str!("fixtures/code_plugin_scoped_component.b64").replace('\n', ""))
        .unwrap()
}

fn package(bytes: &[u8]) -> (tempfile::TempDir, WasiComponentPackage) {
    package_with_manifest(bytes, manifest())
}

fn scoped_package(bytes: &[u8]) -> (tempfile::TempDir, WasiComponentPackage) {
    let manifest = manifest().replace(
        "requested_permissions = []",
        "requested_permissions = [\"filesystem_read\", \"network_access\"]",
    );
    package_with_manifest(bytes, &manifest)
}

fn package_with_manifest(
    bytes: &[u8],
    package_manifest: &str,
) -> (tempfile::TempDir, WasiComponentPackage) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir_all(source.join(".heycode-plugin")).unwrap();
    std::fs::write(source.join(".heycode-plugin/plugin.toml"), package_manifest).unwrap();
    std::fs::create_dir_all(source.join("commands")).unwrap();
    std::fs::write(source.join("commands/review.json"), "{}").unwrap();
    std::fs::create_dir_all(source.join("components")).unwrap();
    let component = source.join("components/plugin.wasm");
    std::fs::write(&component, bytes).unwrap();
    std::fs::set_permissions(&component, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator()).unwrap();
    let installed = cache.install_directory(&source).unwrap();
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

struct Generation(AtomicBool);

impl CodePluginContributionGeneration for Generation {
    fn commit(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn withdraw(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

struct CaptureHost {
    client: Arc<Mutex<Option<CodePluginClient>>>,
}

impl CodePluginContributionHost for CaptureHost {
    fn required_services(&self) -> &'static [ServiceKey] {
        &[]
    }

    fn inventory(&self, _contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        Vec::new()
    }

    fn activate_generation(
        &self,
        _context: &Context,
        client: CodePluginClient,
        _contributions: &[CodePluginContribution],
    ) -> Result<Arc<dyn CodePluginContributionGeneration>, HostActivationFailure> {
        *self.client.lock().unwrap() = Some(client);
        Ok(Arc::new(Generation(AtomicBool::new(false))))
    }
}

#[test]
fn real_engine_typechecks_checked_in_wit_and_runs_initialize_and_invoke_deny_all() {
    let (_temp, package) = package(&component_bytes());
    let grants = heycode_extensions::CodePluginCapabilityGrants::deny_all(package.code_package());
    let policy = WasiComponentCapabilityPolicy::deny_all(&package);
    let engine = WasmtimeWasiComponentEngine::new().unwrap();
    let prepared = package
        .launch(
            grants,
            policy,
            CodePluginSessionId::from_u128(11),
            Arc::new(engine) as Arc<dyn WasiComponentEngine>,
            &CancellationToken::new(),
        )
        .unwrap();
    let public_name = prepared.contributions()[0].public_name().to_owned();
    let client = Arc::new(Mutex::new(None));
    let host = Arc::new(CaptureHost {
        client: Arc::clone(&client),
    });
    let plugin = code_plugin_activation_plugin(vec![prepared], host).unwrap();
    let mut context = compose(&[plugin]).unwrap();
    let client = client.lock().unwrap().clone().unwrap();

    assert_eq!(
        client
            .invoke(
                ContributionKind::Command,
                &public_name,
                "echo",
                json!({"opaque": [1, 2, 3]}),
                &CancellationToken::new(),
            )
            .unwrap(),
        json!({"opaque": [1, 2, 3]})
    );
    context.shutdown();
}

#[test]
fn component_with_valid_preamble_but_corrupt_body_fails_real_engine_validation() {
    let mut bytes = component_bytes();
    bytes.truncate(bytes.len() / 2);
    let (_temp, package) = package(&bytes);
    let grants = heycode_extensions::CodePluginCapabilityGrants::deny_all(package.code_package());
    let policy = WasiComponentCapabilityPolicy::deny_all(&package);
    let engine = WasmtimeWasiComponentEngine::new().unwrap();

    assert!(
        package
            .launch(
                grants,
                policy,
                CodePluginSessionId::from_u128(12),
                Arc::new(engine),
                &CancellationToken::new(),
            )
            .is_err()
    );
}

#[test]
fn scoped_world_links_only_with_minted_preopen_and_endpoint_policy() {
    let bytes = scoped_component_bytes();
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model(true)
        .wasm_component_model_async(true);
    let reflection_engine = wasmtime::Engine::new(&config).unwrap();
    let reflected = wasmtime::component::Component::new(&reflection_engine, &bytes).unwrap();
    let imports = reflected
        .component_type()
        .imports(&reflection_engine)
        .map(|(name, _)| name.to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        imports,
        vec![
            "wasi:filesystem/types@0.3.1",
            "wasi:filesystem/preopens@0.3.1",
            "wasi:sockets/types@0.3.1",
            "wasi:sockets/ip-name-lookup@0.3.1",
        ]
    );
    let (temp, package) = scoped_package(&bytes);
    let shared = temp.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let grants = heycode_extensions::CodePluginCapabilityGrants::new(
        package.code_package(),
        [
            heycode_extensions::PluginPermission::FilesystemRead,
            heycode_extensions::PluginPermission::NetworkAccess,
        ],
    )
    .unwrap();
    let policy = WasiComponentCapabilityPolicy::new(
        &package,
        &grants,
        [WasiPreopen::new(shared, "/shared", WasiPreopenAccess::ReadOnly).unwrap()],
        [WasiNetworkEndpoint::new("127.0.0.1", 9).unwrap()],
    )
    .unwrap();
    let engine = WasmtimeWasiComponentEngine::new().unwrap();

    let prepared = package
        .launch(
            grants,
            policy,
            CodePluginSessionId::from_u128(13),
            Arc::new(engine),
            &CancellationToken::new(),
        )
        .unwrap();
    drop(prepared);
}

#[test]
fn scoped_component_cannot_enter_a_pure_deny_all_world() {
    let (_temp, package) = scoped_package(&scoped_component_bytes());
    let grants = heycode_extensions::CodePluginCapabilityGrants::deny_all(package.code_package());
    let policy = WasiComponentCapabilityPolicy::deny_all(&package);
    let engine = WasmtimeWasiComponentEngine::new().unwrap();

    assert!(
        package
            .launch(
                grants,
                policy,
                CodePluginSessionId::from_u128(14),
                Arc::new(engine),
                &CancellationToken::new(),
            )
            .is_err()
    );
}

#[test]
fn engine_refuses_authority_it_cannot_scope_without_widening() {
    let write_manifest = manifest().replace(
        "requested_permissions = []",
        "requested_permissions = [\"filesystem_write\"]",
    );
    let (write_temp, write_package) = package_with_manifest(&component_bytes(), &write_manifest);
    let write_root = write_temp.path().join("write-only");
    std::fs::create_dir_all(&write_root).unwrap();
    let write_grants = heycode_extensions::CodePluginCapabilityGrants::new(
        write_package.code_package(),
        [heycode_extensions::PluginPermission::FilesystemWrite],
    )
    .unwrap();
    let write_policy = WasiComponentCapabilityPolicy::new(
        &write_package,
        &write_grants,
        [WasiPreopen::new(write_root, "/write", WasiPreopenAccess::WriteOnly).unwrap()],
        [],
    )
    .unwrap();
    let engine = WasmtimeWasiComponentEngine::new().unwrap();
    assert!(
        write_package
            .launch(
                write_grants,
                write_policy,
                CodePluginSessionId::from_u128(15),
                Arc::new(engine),
                &CancellationToken::new(),
            )
            .is_err()
    );

    let network_manifest = manifest().replace(
        "requested_permissions = []",
        "requested_permissions = [\"network_access\"]",
    );
    let (_network_temp, network_package) =
        package_with_manifest(&component_bytes(), &network_manifest);
    let network_grants = heycode_extensions::CodePluginCapabilityGrants::new(
        network_package.code_package(),
        [heycode_extensions::PluginPermission::NetworkAccess],
    )
    .unwrap();
    let network_policy = WasiComponentCapabilityPolicy::new(
        &network_package,
        &network_grants,
        [],
        [WasiNetworkEndpoint::new("api.example.com", 443).unwrap()],
    )
    .unwrap();
    let engine = WasmtimeWasiComponentEngine::new().unwrap();
    assert!(
        network_package
            .launch(
                network_grants,
                network_policy,
                CodePluginSessionId::from_u128(16),
                Arc::new(engine),
                &CancellationToken::new(),
            )
            .is_err()
    );
}

#[test]
fn guest_cpu_is_bounded_and_a_fuel_trap_retires_the_generation() {
    let (temp, package) = scoped_package(&scoped_component_bytes());
    let shared = temp.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let grants = heycode_extensions::CodePluginCapabilityGrants::new(
        package.code_package(),
        [
            heycode_extensions::PluginPermission::FilesystemRead,
            heycode_extensions::PluginPermission::NetworkAccess,
        ],
    )
    .unwrap();
    let policy = WasiComponentCapabilityPolicy::new(
        &package,
        &grants,
        [WasiPreopen::new(shared, "/shared", WasiPreopenAccess::ReadOnly).unwrap()],
        [WasiNetworkEndpoint::new("127.0.0.1", 9).unwrap()],
    )
    .unwrap();
    let engine = WasmtimeWasiComponentEngine::new().unwrap();
    let prepared = package
        .launch(
            grants,
            policy,
            CodePluginSessionId::from_u128(17),
            Arc::new(engine),
            &CancellationToken::new(),
        )
        .unwrap();
    let public_name = prepared.contributions()[0].public_name().to_owned();
    let client = Arc::new(Mutex::new(None));
    let plugin = code_plugin_activation_plugin(
        vec![prepared],
        Arc::new(CaptureHost {
            client: Arc::clone(&client),
        }),
    )
    .unwrap();
    let mut context = compose(&[plugin]).unwrap();
    let client = client.lock().unwrap().clone().unwrap();
    let (sent, received) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = client.invoke(
            ContributionKind::Command,
            &public_name,
            "spin",
            json!({}),
            &CancellationToken::new(),
        );
        let _sent = sent.send(result);
    });

    let error = received
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error,
        CodePluginInvocationError::Transport(CodePluginTransportFault::Crashed)
    ));
    context.shutdown();
}

#[test]
fn preopen_path_substitution_is_rejected_before_component_start() {
    let filesystem_manifest = manifest().replace(
        "requested_permissions = []",
        "requested_permissions = [\"filesystem_read\"]",
    );
    let (temp, package) = package_with_manifest(&component_bytes(), &filesystem_manifest);
    let original = temp.path().join("preopen");
    let moved = temp.path().join("moved-preopen");
    std::fs::create_dir_all(&original).unwrap();
    let preopen = WasiPreopen::new(&original, "/shared", WasiPreopenAccess::ReadOnly).unwrap();
    std::fs::rename(&original, moved).unwrap();
    std::fs::create_dir_all(&original).unwrap();
    let grants = heycode_extensions::CodePluginCapabilityGrants::new(
        package.code_package(),
        [heycode_extensions::PluginPermission::FilesystemRead],
    )
    .unwrap();
    let policy = WasiComponentCapabilityPolicy::new(&package, &grants, [preopen], []).unwrap();
    let engine = WasmtimeWasiComponentEngine::new().unwrap();

    assert!(
        package
            .launch(
                grants,
                policy,
                CodePluginSessionId::from_u128(18),
                Arc::new(engine),
                &CancellationToken::new(),
            )
            .is_err()
    );
}

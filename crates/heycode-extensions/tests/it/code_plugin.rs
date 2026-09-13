//! PL09 host-neutral out-of-process protocol, authority and lifecycle tests.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use heycode_core::{Context, PluginContributionKind, PluginContributionSpec, compose};
use heycode_extensions::{
    ApiVersion, Architecture, CodePluginCapabilityGrants, CodePluginClient, CodePluginContribution,
    CodePluginContributionGeneration, CodePluginContributionHost, CodePluginError, CodePluginExit,
    CodePluginInvocationError, CodePluginLaunchSpec, CodePluginLauncher, CodePluginPackage,
    CodePluginProcess, CodePluginProtocolFault, CodePluginRemoteErrorCode, CodePluginSessionId,
    CodePluginTransportFault, ContributionKind, HostActivationFailure, ManifestValidator,
    OperatingSystem, PackageProvenance, PlatformTarget, PluginInstallCache, PluginPermission,
    code_plugin_activation_plugin,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

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
id = "acme/code-reviewer"
name = "Code Reviewer"
version = "1.0.0"
description = "Out-of-process contribution fixture."
license = "MIT"
default_enabled = true
requested_permissions = ["filesystem_read", "network_access"]
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
locator = "fixtures/code-reviewer"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#
}

fn write_package(root: &Path) {
    let documents = [
        ("skills/review/SKILL.md", "Review carefully."),
        ("commands/review.json", "{}"),
        ("agents/reviewer.json", "{}"),
        ("hooks/pre-review.json", "{}"),
        ("themes/sunset.json", "{}"),
        ("providers/example.json", "{}"),
    ];
    std::fs::create_dir_all(root.join(".heycode-plugin")).unwrap();
    std::fs::write(root.join(".heycode-plugin/plugin.toml"), manifest()).unwrap();
    for (relative, document) in documents {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, document).unwrap();
    }
    let entrypoint = root.join("bin/plugin");
    std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    std::fs::write(&entrypoint, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&entrypoint, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn package() -> (tempfile::TempDir, CodePluginPackage) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source);
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

#[derive(Clone, Copy)]
enum ReadyMutation {
    None,
    PackageDigest,
    ExtraCapability,
    ContributionName,
    ExtraField,
}

type ExitListener = Arc<dyn Fn(CodePluginExit) + Send + Sync>;

struct FakeProcess {
    ready_mutation: ReadyMutation,
    listener: Mutex<Option<ExitListener>>,
    exchanges: AtomicUsize,
    shutdowns: AtomicUsize,
    fail_exchange: AtomicBool,
    remote_error: AtomicBool,
    crash_during_invoke: AtomicBool,
    cancel_during_invoke: AtomicBool,
}

impl FakeProcess {
    fn new(ready_mutation: ReadyMutation) -> Arc<Self> {
        Arc::new(Self {
            ready_mutation,
            listener: Mutex::new(None),
            exchanges: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            fail_exchange: AtomicBool::new(false),
            remote_error: AtomicBool::new(false),
            crash_during_invoke: AtomicBool::new(false),
            cancel_during_invoke: AtomicBool::new(false),
        })
    }

    fn crash(&self) {
        if let Some(listener) = self.listener.lock().unwrap().clone() {
            listener(CodePluginExit::Crashed);
        }
    }

    fn ready(&self, request: &Value) -> Value {
        let mut contributions = request["contributions"].as_array().unwrap().clone();
        if matches!(self.ready_mutation, ReadyMutation::ContributionName) {
            contributions[0]["name"] = Value::String("attacker/override".to_owned());
        }
        let mut capabilities = request["granted_capabilities"].as_array().unwrap().clone();
        if matches!(self.ready_mutation, ReadyMutation::ExtraCapability) {
            capabilities.push(Value::String("credential_use".to_owned()));
        }
        let package_digest = if matches!(self.ready_mutation, ReadyMutation::PackageDigest) {
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        } else {
            request["package_digest"].as_str().unwrap()
        };
        let mut ready = json!({
            "protocol_version": request["protocol_version"],
            "kind": "ready",
            "session_id": request["session_id"],
            "package_id": request["package_id"],
            "package_version": request["package_version"],
            "package_digest": package_digest,
            "executable_digest": request["executable_digest"],
            "accepted_capabilities": capabilities,
            "contributions": contributions,
        });
        if matches!(self.ready_mutation, ReadyMutation::ExtraField) {
            ready["untrusted_annotation"] = Value::String("activate-everything".to_owned());
        }
        ready
    }
}

impl CodePluginProcess for FakeProcess {
    fn exchange(
        &self,
        request: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, CodePluginTransportFault> {
        self.exchanges.fetch_add(1, Ordering::SeqCst);
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        if self.fail_exchange.load(Ordering::SeqCst) {
            return Err(CodePluginTransportFault::Crashed);
        }
        let request: Value =
            serde_json::from_slice(request).map_err(|_| CodePluginTransportFault::Protocol)?;
        if request["kind"] == "invoke" {
            if self.crash_during_invoke.load(Ordering::SeqCst) {
                self.crash();
            }
            if self.cancel_during_invoke.load(Ordering::SeqCst) {
                cancellation.cancel();
            }
        }
        let response = match request["kind"].as_str() {
            Some("initialize") => self.ready(&request),
            Some("invoke") if self.remote_error.load(Ordering::SeqCst) => json!({
                "protocol_version": request["protocol_version"],
                "kind": "error",
                "session_id": request["session_id"],
                "request_id": request["request_id"],
                "code": "denied",
            }),
            Some("invoke") => json!({
                "protocol_version": request["protocol_version"],
                "kind": "result",
                "session_id": request["session_id"],
                "request_id": request["request_id"],
                "output": {"accepted": true},
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

struct FakeLauncher {
    process: Arc<FakeProcess>,
    launches: AtomicUsize,
    grants: Mutex<Vec<PluginPermission>>,
    safe_debug: Mutex<Option<String>>,
}

impl FakeLauncher {
    fn new(process: Arc<FakeProcess>) -> Arc<Self> {
        Arc::new(Self {
            process,
            launches: AtomicUsize::new(0),
            grants: Mutex::new(Vec::new()),
            safe_debug: Mutex::new(None),
        })
    }
}

impl CodePluginLauncher for FakeLauncher {
    fn launch(
        &self,
        spec: &CodePluginLaunchSpec,
        cancellation: &CancellationToken,
    ) -> Result<Arc<dyn CodePluginProcess>, CodePluginTransportFault> {
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        self.launches.fetch_add(1, Ordering::SeqCst);
        *self.grants.lock().unwrap() = spec.granted_capabilities().to_vec();
        *self.safe_debug.lock().unwrap() = Some(format!("{spec:?}"));
        assert!(spec.entrypoint().is_absolute());
        Ok(Arc::clone(&self.process) as Arc<dyn CodePluginProcess>)
    }
}

#[derive(Default)]
struct HostState {
    active: BTreeSet<(ContributionKind, String)>,
    history: Vec<BTreeSet<(ContributionKind, String)>>,
    token: Option<Weak<()>>,
}

struct HostGeneration {
    state: Weak<Mutex<HostState>>,
    token: Arc<()>,
    candidate: BTreeSet<(ContributionKind, String)>,
    committed: AtomicBool,
}

impl CodePluginContributionGeneration for HostGeneration {
    fn commit(&self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        let matches = state
            .token
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|active| Arc::ptr_eq(&active, &self.token));
        if matches && state.active.is_empty() && !self.committed.swap(true, Ordering::SeqCst) {
            state.active = self.candidate.clone();
            let snapshot = state.active.clone();
            state.history.push(snapshot);
        }
    }

    fn withdraw(&self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        let matches = state
            .token
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|active| Arc::ptr_eq(&active, &self.token));
        if matches {
            state.active.clear();
            state.token = None;
            if self.committed.load(Ordering::SeqCst) {
                let snapshot = state.active.clone();
                state.history.push(snapshot);
            }
        }
    }
}

#[derive(Default)]
struct Host {
    state: Arc<Mutex<HostState>>,
    client: Mutex<Option<CodePluginClient>>,
    fail: AtomicBool,
}

impl Host {
    fn snapshot(&self) -> BTreeSet<(ContributionKind, String)> {
        self.state.lock().unwrap().active.clone()
    }

    fn history(&self) -> Vec<BTreeSet<(ContributionKind, String)>> {
        self.state.lock().unwrap().history.clone()
    }

    fn client(&self) -> CodePluginClient {
        self.client.lock().unwrap().clone().unwrap()
    }
}

impl CodePluginContributionHost for Host {
    fn required_services(&self) -> &'static [heycode_core::ServiceKey] {
        &[]
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        let kind = match contribution.kind() {
            ContributionKind::Command => heycode_core::ContributionKind::Command,
            ContributionKind::Provider => heycode_core::ContributionKind::InferenceProvider,
            ContributionKind::Skill
            | ContributionKind::Agent
            | ContributionKind::Hook
            | ContributionKind::Theme
            | ContributionKind::Mcp => return Vec::new(),
        };
        vec![PluginContributionSpec::new(
            kind,
            contribution.public_name(),
        )]
    }

    fn activate_generation(
        &self,
        _context: &Context,
        client: CodePluginClient,
        contributions: &[CodePluginContribution],
    ) -> Result<Arc<dyn CodePluginContributionGeneration>, HostActivationFailure> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(HostActivationFailure::InvalidDefinition);
        }
        let candidate = contributions
            .iter()
            .map(|contribution| (contribution.kind(), contribution.public_name().to_owned()))
            .collect::<BTreeSet<_>>();
        let token = Arc::new(());
        let mut state = self
            .state
            .lock()
            .map_err(|_| HostActivationFailure::Unavailable)?;
        if !state.active.is_empty() || state.token.is_some() {
            return Err(HostActivationFailure::Duplicate);
        }
        state.token = Some(Arc::downgrade(&token));
        drop(state);
        *self.client.lock().unwrap() = Some(client);
        Ok(Arc::new(HostGeneration {
            state: Arc::downgrade(&self.state),
            token,
            candidate,
            committed: AtomicBool::new(false),
        }))
    }
}

fn prepared(
    package: CodePluginPackage,
    process: Arc<FakeProcess>,
    grants: CodePluginCapabilityGrants,
) -> (heycode_extensions::PreparedCodePlugin, Arc<FakeLauncher>) {
    let launcher = FakeLauncher::new(process);
    let prepared = package
        .launch(
            grants,
            CodePluginSessionId::from_u128(7),
            Arc::clone(&launcher) as Arc<dyn CodePluginLauncher>,
            &CancellationToken::new(),
        )
        .unwrap();
    (prepared, launcher)
}

#[test]
fn all_six_code_contributions_activate_in_one_generation_and_dispose_atomically() {
    let (_temp, package) = package();
    let grants = CodePluginCapabilityGrants::deny_all(&package);
    let process = FakeProcess::new(ReadyMutation::None);
    let (prepared, launcher) = prepared(package, Arc::clone(&process), grants);
    assert!(launcher.grants.lock().unwrap().is_empty());
    let safe_debug = launcher.safe_debug.lock().unwrap().clone().unwrap();
    assert!(!safe_debug.contains("/Users/"));
    assert!(!safe_debug.contains("#!/bin/sh"));

    let host = Arc::new(Host::default());
    let plugin = code_plugin_activation_plugin(
        vec![prepared],
        Arc::clone(&host) as Arc<dyn CodePluginContributionHost>,
    )
    .unwrap();
    let mut context = compose(&[plugin]).unwrap();
    assert_eq!(host.snapshot().len(), 6);
    let kinds = host
        .snapshot()
        .into_iter()
        .map(|(kind, _)| kind)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from([
            ContributionKind::Skill,
            ContributionKind::Command,
            ContributionKind::Agent,
            ContributionKind::Hook,
            ContributionKind::Theme,
            ContributionKind::Provider,
        ])
    );

    let result = host
        .client()
        .invoke(
            ContributionKind::Command,
            "acme/code-reviewer::review",
            "run",
            json!({"subject":"safe"}),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(result, json!({"accepted": true}));

    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.kind == heycode_core::ContributionKind::ExternalProcess
            && row.name == "acme/code-reviewer"
    }));
    context.shutdown();
    assert!(host.snapshot().is_empty());
    assert_eq!(
        host.history().iter().map(BTreeSet::len).collect::<Vec<_>>(),
        [6, 0]
    );
    assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
}

#[test]
fn capabilities_are_denied_by_default_and_unrequested_grants_never_launch() {
    let (_temp, package) = package();
    assert!(
        CodePluginCapabilityGrants::deny_all(&package)
            .as_slice()
            .is_empty()
    );
    let error =
        CodePluginCapabilityGrants::new(&package, [PluginPermission::CredentialUse]).unwrap_err();
    assert!(matches!(
        error,
        CodePluginError::UnrequestedCapability(PluginPermission::CredentialUse)
    ));

    let grants = CodePluginCapabilityGrants::new(
        &package,
        [
            PluginPermission::NetworkAccess,
            PluginPermission::FilesystemRead,
        ],
    )
    .unwrap();
    assert_eq!(
        grants.as_slice(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::NetworkAccess,
        ]
    );
    let process = FakeProcess::new(ReadyMutation::None);
    let (_prepared, launcher) = prepared(package, process, grants);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    assert_eq!(
        *launcher.grants.lock().unwrap(),
        [
            PluginPermission::FilesystemRead,
            PluginPermission::NetworkAccess,
        ]
    );
}

#[test]
fn substituted_identity_digest_capability_and_annotations_fail_before_activation() {
    for mutation in [
        ReadyMutation::PackageDigest,
        ReadyMutation::ExtraCapability,
        ReadyMutation::ContributionName,
        ReadyMutation::ExtraField,
    ] {
        let (_temp, package) = package();
        let process = FakeProcess::new(mutation);
        let launcher = FakeLauncher::new(Arc::clone(&process));
        let grants = CodePluginCapabilityGrants::deny_all(&package);
        let error = package
            .launch(
                grants,
                CodePluginSessionId::from_u128(9),
                launcher as Arc<dyn CodePluginLauncher>,
                &CancellationToken::new(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CodePluginError::Protocol(
                CodePluginProtocolFault::IdentityMismatch
                    | CodePluginProtocolFault::CapabilityMismatch
                    | CodePluginProtocolFault::ContributionMismatch
                    | CodePluginProtocolFault::InvalidFrame
            )
        ));
        assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("activate-everything"));
        assert!(!rendered.contains("attacker/override"));
    }
}

#[test]
fn crash_and_generation_cancellation_withdraw_the_complete_generation() {
    for crash in [true, false] {
        let (_temp, package) = package();
        let process = FakeProcess::new(ReadyMutation::None);
        let grants = CodePluginCapabilityGrants::deny_all(&package);
        let (prepared, _launcher) = prepared(package, Arc::clone(&process), grants);
        let host = Arc::new(Host::default());
        let plugin = code_plugin_activation_plugin(
            vec![prepared],
            Arc::clone(&host) as Arc<dyn CodePluginContributionHost>,
        )
        .unwrap();
        let mut context = compose(&[plugin]).unwrap();
        let client = host.client();
        assert_eq!(host.snapshot().len(), 6);
        if crash {
            process.crash();
        } else {
            client.cancel_generation();
        }
        assert!(host.snapshot().is_empty());
        assert!(matches!(
            client.invoke(
                ContributionKind::Command,
                "acme/code-reviewer::review",
                "run",
                json!({}),
                &CancellationToken::new(),
            ),
            Err(CodePluginInvocationError::Closed)
        ));
        assert_eq!(
            host.history().iter().map(BTreeSet::len).collect::<Vec<_>>(),
            [6, 0]
        );
        context.shutdown();
        assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn transport_failure_retires_rows_but_a_pre_cancelled_call_sends_nothing() {
    let (_temp, package) = package();
    let process = FakeProcess::new(ReadyMutation::None);
    let grants = CodePluginCapabilityGrants::deny_all(&package);
    let (prepared, _launcher) = prepared(package, Arc::clone(&process), grants);
    let host = Arc::new(Host::default());
    let plugin = code_plugin_activation_plugin(
        vec![prepared],
        Arc::clone(&host) as Arc<dyn CodePluginContributionHost>,
    )
    .unwrap();
    let mut context = compose(&[plugin]).unwrap();
    let client = host.client();

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        client.invoke(
            ContributionKind::Command,
            "acme/code-reviewer::review",
            "run",
            json!({}),
            &cancelled,
        ),
        Err(CodePluginInvocationError::Cancelled)
    ));
    assert_eq!(process.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(host.snapshot().len(), 6);

    process.fail_exchange.store(true, Ordering::SeqCst);
    assert!(matches!(
        client.invoke(
            ContributionKind::Command,
            "acme/code-reviewer::review",
            "run",
            json!({}),
            &CancellationToken::new(),
        ),
        Err(CodePluginInvocationError::Transport(
            CodePluginTransportFault::Crashed
        ))
    ));
    assert!(host.snapshot().is_empty());
    context.shutdown();
}

#[test]
fn remote_denial_is_closed_and_does_not_retire_a_healthy_generation() {
    let (_temp, package) = package();
    let process = FakeProcess::new(ReadyMutation::None);
    let grants = CodePluginCapabilityGrants::deny_all(&package);
    let (prepared, _launcher) = prepared(package, Arc::clone(&process), grants);
    let host = Arc::new(Host::default());
    let plugin = code_plugin_activation_plugin(
        vec![prepared],
        Arc::clone(&host) as Arc<dyn CodePluginContributionHost>,
    )
    .unwrap();
    let mut context = compose(&[plugin]).unwrap();
    process.remote_error.store(true, Ordering::SeqCst);
    assert!(matches!(
        host.client().invoke(
            ContributionKind::Command,
            "acme/code-reviewer::review",
            "run",
            json!({}),
            &CancellationToken::new(),
        ),
        Err(CodePluginInvocationError::Remote(
            CodePluginRemoteErrorCode::Denied
        ))
    ));
    assert_eq!(host.snapshot().len(), 6);
    context.shutdown();
}

#[test]
fn exit_or_cancellation_after_request_admission_cannot_publish_a_response() {
    for cancel in [false, true] {
        let (_temp, package) = package();
        let process = FakeProcess::new(ReadyMutation::None);
        let grants = CodePluginCapabilityGrants::deny_all(&package);
        let (prepared, _launcher) = prepared(package, Arc::clone(&process), grants);
        let host = Arc::new(Host::default());
        let plugin = code_plugin_activation_plugin(
            vec![prepared],
            Arc::clone(&host) as Arc<dyn CodePluginContributionHost>,
        )
        .unwrap();
        let mut context = compose(&[plugin]).unwrap();
        let operation = CancellationToken::new();
        if cancel {
            process.cancel_during_invoke.store(true, Ordering::SeqCst);
        } else {
            process.crash_during_invoke.store(true, Ordering::SeqCst);
        }

        let result = host.client().invoke(
            ContributionKind::Command,
            "acme/code-reviewer::review",
            "run",
            json!({}),
            &operation,
        );
        if cancel {
            assert!(matches!(result, Err(CodePluginInvocationError::Cancelled)));
        } else {
            assert!(matches!(result, Err(CodePluginInvocationError::Closed)));
        }
        assert!(host.snapshot().is_empty());
        context.shutdown();
    }
}

#[test]
fn host_generation_refusal_rolls_back_the_process_before_composition_returns() {
    let (_temp, package) = package();
    let process = FakeProcess::new(ReadyMutation::None);
    let grants = CodePluginCapabilityGrants::deny_all(&package);
    let (prepared, _launcher) = prepared(package, Arc::clone(&process), grants);
    let host = Arc::new(Host::default());
    host.fail.store(true, Ordering::SeqCst);
    let plugin = code_plugin_activation_plugin(
        vec![prepared],
        Arc::clone(&host) as Arc<dyn CodePluginContributionHost>,
    )
    .unwrap();
    let error = compose(&[plugin])
        .err()
        .expect("the host refuses the complete generation");
    assert!(error.to_string().contains("code plugin"));
    assert!(host.snapshot().is_empty());
    assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
}

#[test]
fn code_bridge_descriptor_declares_external_process_authority() {
    assert!(
        PluginContributionKind::ExternalProcess
            .as_str()
            .contains("external")
    );
}

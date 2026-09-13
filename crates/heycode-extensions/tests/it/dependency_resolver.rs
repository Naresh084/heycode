//! PL07 deterministic dependency/conflict/platform generation contracts.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use heycode_core::{Context, PluginContributionKind, compose};
use heycode_extensions::lifecycle::{
    InstalledVersions, LifecycleError, PluginLifecycle, PluginLifecycleAdmission, PluginOperation,
    PluginState, PluginStateStore,
};
use heycode_extensions::{
    ApiVersion, Architecture, DeclarativeContribution, DeclarativeContributionHost,
    DeclarativeContributionRegistration, DeclarativePackage, DuplicatePluginDiagnostic,
    HostActivationFailure, IncompatibleDependencyVersionDiagnostic, ManifestValidator,
    OperatingSystem, PlatformTarget, PluginGraphResolver, PluginId, PluginInstallCache,
    PluginResolutionError, PluginVersion, ResolvedPluginActivationError,
    ResolvedPluginInstallError, UnsupportedPlatformDiagnostic,
    resolved_declarative_activation_plugin,
};

const MAC: PlatformTarget = PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64);
const LINUX: PlatformTarget = PlatformTarget::new(OperatingSystem::Linux, Architecture::X86_64);

#[derive(Clone, Copy)]
struct Dependency<'a> {
    id: &'a str,
    minimum: &'a str,
    maximum: Option<&'a str>,
    optional: bool,
}

fn platform_toml(platform: PlatformTarget) -> String {
    format!(
        "{{ os = \"{}\", architecture = \"{}\" }}",
        platform.os(),
        platform.architecture()
    )
}

fn manifest_raw(
    id: &str,
    version: &str,
    dependencies: &[Dependency<'_>],
    conflicts: &[&str],
    platforms: &[PlatformTarget],
) -> String {
    let dependencies = dependencies
        .iter()
        .map(|dependency| {
            let maximum = dependency.maximum.map_or_else(String::new, |maximum| {
                format!(", maximum_version_exclusive = \"{maximum}\"")
            });
            format!(
                "{{ id = \"{}\", minimum_version = \"{}\"{maximum}, optional = {} }}",
                dependency.id, dependency.minimum, dependency.optional
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let conflicts = conflicts
        .iter()
        .map(|conflict| format!("\"{conflict}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let platforms = platforms
        .iter()
        .copied()
        .map(platform_toml)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"schema_version = 1
id = "{id}"
name = "Resolver fixture"
version = "{version}"
description = "resolver-body-canary"
license = "MIT"
default_enabled = false
requested_permissions = []
platforms = [{platforms}]
dependencies = [{dependencies}]
conflicts = [{conflicts}]
contributions = [{{ kind = "skill", id = "main", path = "skills/main/SKILL.md", exposure = {{ mode = "namespaced" }} }}]

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "packages/{id}"
revision = "resolver-fixture"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#,
    )
}

fn manifest(
    validation_platform: PlatformTarget,
    id: &str,
    version: &str,
    dependencies: &[Dependency<'_>],
    conflicts: &[&str],
    platforms: &[PlatformTarget],
) -> heycode_extensions::PluginManifest {
    ManifestValidator::new(ApiVersion::new(1).unwrap(), validation_platform)
        .validate_toml(&manifest_raw(
            id,
            version,
            dependencies,
            conflicts,
            platforms,
        ))
        .unwrap()
}

fn id(value: &str) -> PluginId {
    PluginId::new(value).unwrap()
}

fn version(value: &str) -> PluginVersion {
    PluginVersion::parse(value).unwrap()
}

fn ids(graph: &heycode_extensions::ResolvedPluginGraph) -> Vec<String> {
    graph
        .manifests()
        .iter()
        .map(|manifest| manifest.id().as_str().to_owned())
        .collect()
}

#[test]
fn dependency_first_order_is_stable_for_every_input_order() {
    let base = manifest(MAC, "acme/base", "1.2.0+build.5", &[], &[], &[MAC]);
    let middle = manifest(
        MAC,
        "acme/middle",
        "2.0.0",
        &[Dependency {
            id: "acme/base",
            minimum: "1.2.0+different-build",
            maximum: Some("2.0.0"),
            optional: false,
        }],
        &[],
        &[MAC],
    );
    let application = manifest(
        MAC,
        "acme/application",
        "3.0.0",
        &[
            Dependency {
                id: "acme/optional",
                minimum: "1.0.0",
                maximum: None,
                optional: true,
            },
            Dependency {
                id: "acme/middle",
                minimum: "2.0.0",
                maximum: Some("3.0.0"),
                optional: false,
            },
        ],
        &[],
        &[MAC],
    );
    let resolver = PluginGraphResolver::new(MAC);
    let permutations = [
        vec![base.clone(), middle.clone(), application.clone()],
        vec![base.clone(), application.clone(), middle.clone()],
        vec![middle.clone(), base.clone(), application.clone()],
        vec![middle.clone(), application.clone(), base.clone()],
        vec![application.clone(), base.clone(), middle.clone()],
        vec![application, middle, base],
    ];

    for input in permutations {
        let graph = resolver.resolve(input).unwrap();
        assert_eq!(graph.platform(), MAC);
        assert_eq!(
            ids(&graph),
            ["acme/base", "acme/middle", "acme/application"]
        );
    }
}

#[test]
fn independent_ready_rows_use_plugin_id_order_not_registration_order() {
    let alpha = manifest(MAC, "acme/alpha", "1.0.0", &[], &[], &[MAC]);
    let middle = manifest(MAC, "acme/middle", "1.0.0", &[], &[], &[MAC]);
    let zeta = manifest(MAC, "acme/zeta", "1.0.0", &[], &[], &[MAC]);
    let resolver = PluginGraphResolver::new(MAC);
    assert_eq!(
        ids(&resolver.resolve([zeta, middle, alpha]).unwrap()),
        ["acme/alpha", "acme/middle", "acme/zeta"]
    );
}

#[test]
fn missing_and_incompatible_dependencies_have_distinct_actionable_diagnostics() {
    let missing = manifest(
        MAC,
        "acme/application",
        "1.0.0",
        &[Dependency {
            id: "acme/base",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    let resolver = PluginGraphResolver::new(MAC);
    assert_eq!(
        resolver.resolve([missing]).unwrap_err(),
        PluginResolutionError::MissingDependency {
            plugin: id("acme/application"),
            dependency: id("acme/base"),
        }
    );

    let base = manifest(MAC, "acme/base", "2.0.0", &[], &[], &[MAC]);
    let optional = manifest(
        MAC,
        "acme/application",
        "1.0.0",
        &[Dependency {
            id: "acme/base",
            minimum: "1.0.0",
            maximum: Some("2.0.0"),
            optional: true,
        }],
        &[],
        &[MAC],
    );
    assert_eq!(
        resolver.resolve([optional, base]).unwrap_err(),
        PluginResolutionError::IncompatibleDependencyVersion(Box::new(
            IncompatibleDependencyVersionDiagnostic {
                plugin: id("acme/application"),
                dependency: id("acme/base"),
                found: version("2.0.0"),
                minimum: version("1.0.0"),
                maximum_exclusive: Some(version("2.0.0")),
                optional: true,
            },
        ))
    );

    let old_base = manifest(MAC, "acme/base", "0.9.9", &[], &[], &[MAC]);
    let required = manifest(
        MAC,
        "acme/application",
        "1.0.0",
        &[Dependency {
            id: "acme/base",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    assert_eq!(
        resolver.resolve([old_base, required]).unwrap_err(),
        PluginResolutionError::IncompatibleDependencyVersion(Box::new(
            IncompatibleDependencyVersionDiagnostic {
                plugin: id("acme/application"),
                dependency: id("acme/base"),
                found: version("0.9.9"),
                minimum: version("1.0.0"),
                maximum_exclusive: None,
                optional: false,
            },
        ))
    );
}

#[test]
fn absent_optional_dependencies_are_ignored_but_present_ones_order_the_graph() {
    let application = manifest(
        MAC,
        "acme/application",
        "1.0.0",
        &[Dependency {
            id: "acme/optional",
            minimum: "1.0.0",
            maximum: None,
            optional: true,
        }],
        &[],
        &[MAC],
    );
    let resolver = PluginGraphResolver::new(MAC);
    assert_eq!(
        ids(&resolver.resolve([application.clone()]).unwrap()),
        ["acme/application"]
    );
    let optional = manifest(MAC, "acme/optional", "1.0.0", &[], &[], &[MAC]);
    assert_eq!(
        ids(&resolver.resolve([application, optional]).unwrap()),
        ["acme/optional", "acme/application"]
    );
}

#[test]
fn conflicts_and_duplicate_identities_are_canonical_across_input_order() {
    let modern = manifest(MAC, "acme/modern", "1.0.0", &[], &["legacy/modern"], &[MAC]);
    let legacy = manifest(MAC, "legacy/modern", "9.0.0", &[], &[], &[MAC]);
    let resolver = PluginGraphResolver::new(MAC);
    let expected = PluginResolutionError::Conflict {
        first: id("acme/modern"),
        second: id("legacy/modern"),
    };
    assert_eq!(
        resolver
            .resolve([modern.clone(), legacy.clone()])
            .unwrap_err(),
        expected
    );
    assert_eq!(resolver.resolve([legacy, modern]).unwrap_err(), expected);

    let first = manifest(MAC, "acme/same", "2.0.0", &[], &[], &[MAC]);
    let second = manifest(MAC, "acme/same", "1.0.0", &[], &[], &[MAC]);
    let expected = PluginResolutionError::DuplicatePlugin(Box::new(DuplicatePluginDiagnostic {
        plugin: id("acme/same"),
        versions: vec![version("1.0.0"), version("2.0.0")],
    }));
    assert_eq!(
        resolver
            .resolve([first.clone(), second.clone()])
            .unwrap_err(),
        expected
    );
    assert_eq!(resolver.resolve([second, first]).unwrap_err(), expected);
}

#[test]
fn cycles_report_one_stable_closed_dependency_path() {
    let a = manifest(
        MAC,
        "acme/a",
        "1.0.0",
        &[Dependency {
            id: "acme/b",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    let b = manifest(
        MAC,
        "acme/b",
        "1.0.0",
        &[Dependency {
            id: "acme/c",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    let c = manifest(
        MAC,
        "acme/c",
        "1.0.0",
        &[Dependency {
            id: "acme/a",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    let expected = PluginResolutionError::DependencyCycle {
        path: vec![id("acme/a"), id("acme/b"), id("acme/c"), id("acme/a")],
    };
    let resolver = PluginGraphResolver::new(MAC);
    assert_eq!(
        resolver
            .resolve([a.clone(), b.clone(), c.clone()])
            .unwrap_err(),
        expected
    );
    assert_eq!(resolver.resolve([c, a, b]).unwrap_err(), expected);
}

#[test]
fn platform_evidence_uses_the_explicit_current_target_without_host_inference() {
    let linux_only = manifest(LINUX, "acme/linux-only", "1.0.0", &[], &[], &[LINUX]);
    let error = PluginGraphResolver::new(MAC)
        .resolve([linux_only])
        .unwrap_err();
    assert_eq!(
        error,
        PluginResolutionError::UnsupportedPlatform(Box::new(UnsupportedPlatformDiagnostic {
            plugin: id("acme/linux-only"),
            version: version("1.0.0"),
            current: MAC,
            supported: vec![LINUX],
        }))
    );
    let rendered = format!("{error:?} {error}");
    assert!(rendered.contains("macos/aarch64"));
    assert!(!rendered.contains("resolver-body-canary"));
    assert!(!rendered.contains("packages/acme/linux-only"));
}

fn write_package(root: &Path, raw: &str, body: &str) {
    std::fs::create_dir_all(root.join(".heycode-plugin")).unwrap();
    std::fs::create_dir_all(root.join("skills/main")).unwrap();
    std::fs::write(root.join(".heycode-plugin/plugin.toml"), raw).unwrap();
    std::fs::write(root.join("skills/main/SKILL.md"), body).unwrap();
}

fn assert_cache_empty(cache: &PluginInstallCache) {
    assert!(cache.inspect().unwrap().packages.is_empty());
    for relative in [".objects/sha256", ".refs", ".staging"] {
        assert_eq!(
            std::fs::read_dir(cache.root().join(relative))
                .unwrap()
                .count(),
            0,
            "graph refusal left cache state in {relative}"
        );
    }
}

#[test]
fn unresolved_or_different_manifests_cannot_publish_cache_state() {
    let temp = tempfile::tempdir().unwrap();
    let expected_raw = manifest_raw("acme/base", "1.0.0", &[], &[], &[MAC]);
    let expected = ManifestValidator::new(ApiVersion::new(1).unwrap(), MAC)
        .validate_toml(&expected_raw)
        .unwrap();
    let graph = PluginGraphResolver::new(MAC).resolve([expected]).unwrap();
    let unexpected_source = temp.path().join("unexpected");
    write_package(
        &unexpected_source,
        &manifest_raw("acme/other", "1.0.0", &[], &[], &[MAC]),
        "unexpected-body-canary",
    );
    let cache = PluginInstallCache::open(
        temp.path().join("cache"),
        ManifestValidator::new(ApiVersion::new(1).unwrap(), MAC),
    )
    .unwrap();

    let error = cache
        .install_resolved_directory(&graph, &unexpected_source)
        .err()
        .expect("an unexpected manifest cannot enter the resolved cache generation");
    assert!(matches!(
        error,
        ResolvedPluginInstallError::Resolution(ref resolution)
            if matches!(
                resolution.as_ref(),
                PluginResolutionError::UnexpectedGenerationPlugin { .. }
            )
    ));
    assert_cache_empty(&cache);

    let drifted_source = temp.path().join("drifted");
    write_package(
        &drifted_source,
        &manifest_raw(
            "acme/base",
            "1.0.0",
            &[Dependency {
                id: "acme/optional",
                minimum: "1.0.0",
                maximum: None,
                optional: true,
            }],
            &[],
            &[MAC],
        ),
        "drifted-body-canary",
    );
    let error = cache
        .install_resolved_directory(&graph, &drifted_source)
        .err()
        .expect("same-id metadata drift cannot enter the resolved cache generation");
    assert!(matches!(
        error,
        ResolvedPluginInstallError::Resolution(ref resolution)
            if matches!(
                resolution.as_ref(),
                PluginResolutionError::MismatchedGenerationManifest { .. }
            )
    ));
    assert_cache_empty(&cache);

    let missing = manifest(
        MAC,
        "acme/application",
        "1.0.0",
        &[Dependency {
            id: "acme/missing",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    assert!(matches!(
        PluginGraphResolver::new(MAC).resolve([missing]),
        Err(PluginResolutionError::MissingDependency { .. })
    ));
    assert_cache_empty(&cache);
}

#[derive(Default)]
struct MemoryStore {
    states: Mutex<BTreeMap<String, PluginState>>,
    writes: Mutex<usize>,
}

impl PluginStateStore for MemoryStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        Ok(self.states.lock().unwrap().clone())
    }

    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        *self.states.lock().unwrap() = states.clone();
        *self.writes.lock().unwrap() += 1;
        Ok(())
    }
}

struct Cached(BTreeMap<String, Vec<PluginVersion>>);

impl InstalledVersions for Cached {
    fn versions(&self, id: &PluginId) -> Vec<PluginVersion> {
        self.0.get(id.as_str()).cloned().unwrap_or_default()
    }
}

struct DenyAdmission;

impl PluginLifecycleAdmission for DenyAdmission {
    fn authorize(
        &self,
        operation: PluginOperation,
        id: &PluginId,
        _version: &PluginVersion,
    ) -> Result<(), LifecycleError> {
        Err(LifecycleError::ManagedPolicyRejected {
            operation,
            id: id.as_str().to_owned(),
        })
    }
}

fn make_lifecycle(
    cached: impl IntoIterator<Item = (&'static str, &'static str)>,
) -> (Arc<MemoryStore>, PluginLifecycle) {
    let store = Arc::new(MemoryStore::default());
    let mut versions = BTreeMap::<String, Vec<PluginVersion>>::new();
    for (plugin, value) in cached {
        versions
            .entry(plugin.to_owned())
            .or_default()
            .push(version(value));
    }
    let lifecycle = PluginLifecycle::new(
        Arc::clone(&store) as Arc<dyn PluginStateStore>,
        Arc::new(Cached(versions)) as Arc<dyn InstalledVersions>,
    );
    (store, lifecycle)
}

#[test]
fn lifecycle_reconciliation_checks_the_complete_graph_before_one_state_write() {
    let base = manifest(MAC, "acme/base", "1.0.0", &[], &[], &[MAC]);
    let application = manifest(
        MAC,
        "acme/application",
        "1.0.0",
        &[Dependency {
            id: "acme/base",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    let graph = PluginGraphResolver::new(MAC)
        .resolve([application, base.clone()])
        .unwrap();
    let (store, lifecycle) =
        make_lifecycle([("acme/base", "1.0.0"), ("acme/application", "1.0.0")]);
    lifecycle.apply_resolved_graph(&graph).unwrap();
    assert_eq!(*store.writes.lock().unwrap(), 1);
    let states = lifecycle.list().unwrap();
    assert_eq!(states.len(), 2);
    assert!(states.iter().all(|state| state.enabled));

    let base_only = PluginGraphResolver::new(MAC).resolve([base]).unwrap();
    lifecycle.apply_resolved_graph(&base_only).unwrap();
    assert_eq!(*store.writes.lock().unwrap(), 2);
    let states = lifecycle.list().unwrap();
    assert!(
        states
            .iter()
            .find(|state| state.id == id("acme/base"))
            .is_some_and(|state| state.enabled)
    );
    assert!(
        states
            .iter()
            .find(|state| state.id == id("acme/application"))
            .is_some_and(|state| !state.enabled)
    );
    lifecycle.apply_resolved_graph(&base_only).unwrap();
    assert_eq!(
        *store.writes.lock().unwrap(),
        2,
        "an identical generation must not rewrite lifecycle state"
    );

    let (store, lifecycle) = make_lifecycle([("acme/base", "1.0.0")]);
    assert_eq!(
        lifecycle.apply_resolved_graph(&graph).unwrap_err(),
        LifecycleError::VersionUnavailable {
            id: "acme/application".to_owned(),
            version: "1.0.0".to_owned(),
        }
    );
    assert_eq!(*store.writes.lock().unwrap(), 0);
    assert!(lifecycle.list().unwrap().is_empty());
}

#[test]
fn lifecycle_graph_reconciliation_keeps_managed_admission_before_persistence() {
    let base = manifest(MAC, "acme/base", "1.0.0", &[], &[], &[MAC]);
    let graph = PluginGraphResolver::new(MAC).resolve([base]).unwrap();
    let store = Arc::new(MemoryStore::default());
    let lifecycle = PluginLifecycle::with_admission(
        Arc::clone(&store) as Arc<dyn PluginStateStore>,
        Arc::new(Cached(BTreeMap::from([(
            "acme/base".to_owned(),
            vec![version("1.0.0")],
        )]))) as Arc<dyn InstalledVersions>,
        Arc::new(DenyAdmission),
    );

    assert!(matches!(
        lifecycle.apply_resolved_graph(&graph),
        Err(LifecycleError::ManagedPolicyRejected { .. })
    ));
    assert_eq!(*store.writes.lock().unwrap(), 0);
    assert!(lifecycle.list().unwrap().is_empty());
}

#[derive(Default)]
struct RecordingHost {
    activated: Mutex<Vec<String>>,
}

struct NoopRegistration;

impl DeclarativeContributionRegistration for NoopRegistration {
    fn withdraw(self: Box<Self>) {}
}

impl RecordingHost {
    fn activate(
        &self,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activated
            .lock()
            .unwrap()
            .push(contribution.package_id().as_str().to_owned());
        Ok(Box::new(NoopRegistration))
    }
}

impl DeclarativeContributionHost for RecordingHost {
    fn required_services(&self) -> &'static [heycode_core::ServiceKey] {
        &[]
    }

    fn descriptor_families(&self) -> &'static [PluginContributionKind] {
        &[]
    }

    fn activate_skill(
        &self,
        _context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate(contribution)
    }

    fn activate_command(
        &self,
        _context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate(contribution)
    }

    fn activate_agent(
        &self,
        _context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate(contribution)
    }

    fn activate_hook(
        &self,
        _context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate(contribution)
    }

    fn activate_theme(
        &self,
        _context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate(contribution)
    }

    fn activate_provider(
        &self,
        _context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate(contribution)
    }
}

fn installed_package(cache: &PluginInstallCache, root: PathBuf, raw: &str) -> DeclarativePackage {
    write_package(&root, raw, "activation-body-canary");
    DeclarativePackage::load(&cache.install_directory(root).unwrap()).unwrap()
}

#[test]
fn activation_uses_dependency_order_and_generation_mismatch_calls_no_host() {
    let temp = tempfile::tempdir().unwrap();
    let base_raw = manifest_raw("acme/base", "1.0.0", &[], &[], &[MAC]);
    let application_raw = manifest_raw(
        "acme/application",
        "1.0.0",
        &[Dependency {
            id: "acme/base",
            minimum: "1.0.0",
            maximum: None,
            optional: false,
        }],
        &[],
        &[MAC],
    );
    let validator = ManifestValidator::new(ApiVersion::new(1).unwrap(), MAC);
    let graph = PluginGraphResolver::new(MAC)
        .resolve([
            validator.validate_toml(&application_raw).unwrap(),
            validator.validate_toml(&base_raw).unwrap(),
        ])
        .unwrap();
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator).unwrap();
    let base = installed_package(&cache, temp.path().join("base"), &base_raw);
    let application = installed_package(&cache, temp.path().join("application"), &application_raw);

    let refused_host = Arc::new(RecordingHost::default());
    let error = resolved_declarative_activation_plugin(
        &graph,
        vec![base.clone(), base.clone(), application.clone()],
        Arc::clone(&refused_host) as Arc<dyn DeclarativeContributionHost>,
    )
    .err()
    .expect("one resolved identity cannot be supplied twice");
    assert!(matches!(
        error,
        ResolvedPluginActivationError::Resolution(ref resolution)
            if matches!(
                resolution.as_ref(),
                PluginResolutionError::DuplicateGenerationPlugin { .. }
            )
    ));
    assert!(refused_host.activated.lock().unwrap().is_empty());

    let error = resolved_declarative_activation_plugin(
        &graph,
        vec![base.clone()],
        Arc::clone(&refused_host) as Arc<dyn DeclarativeContributionHost>,
    )
    .err()
    .expect("the graph requires the application package too");
    assert!(matches!(
        error,
        ResolvedPluginActivationError::Resolution(ref resolution)
            if matches!(
                resolution.as_ref(),
                PluginResolutionError::MissingGenerationPlugin { .. }
            )
    ));
    assert!(refused_host.activated.lock().unwrap().is_empty());

    let host = Arc::new(RecordingHost::default());
    let plugin = resolved_declarative_activation_plugin(
        &graph,
        vec![application, base],
        Arc::clone(&host) as Arc<dyn DeclarativeContributionHost>,
    )
    .unwrap();
    let mut context = compose(&[plugin]).unwrap();
    assert_eq!(
        *host.activated.lock().unwrap(),
        ["acme/base", "acme/application"]
    );
    context.shutdown();
}

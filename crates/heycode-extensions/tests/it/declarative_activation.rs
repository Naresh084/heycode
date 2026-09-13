//! PL03 library boundary: six declarative document kinds enter and leave one
//! real core composition transaction.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use heycode_core::{
    Context, ContributionKind as CoreContributionKind, PluginContributionKind,
    PluginContributionSpec, compose,
};
use heycode_extensions::{
    ApiVersion, Architecture, ContributionKind, DeclarativeContribution,
    DeclarativeContributionHost, DeclarativeContributionRegistration, DeclarativeDocumentFault,
    DeclarativePackage, DeclarativePluginError, HostActivationFailure, InstalledPlugin,
    ManifestValidator, OperatingSystem, PlatformTarget, PluginInstallCache,
    declarative_activation_plugin,
};

#[derive(Clone)]
struct Entry {
    document: String,
    token: Arc<()>,
}

#[derive(Default)]
struct Host {
    entries: Arc<Mutex<BTreeMap<(ContributionKind, String), Entry>>>,
    attempted: Arc<Mutex<Vec<ContributionKind>>>,
    fail_on: Option<ContributionKind>,
    required_services: &'static [heycode_core::ServiceKey],
}

struct Registration {
    entries: std::sync::Weak<Mutex<BTreeMap<(ContributionKind, String), Entry>>>,
    key: (ContributionKind, String),
    token: Arc<()>,
}

impl DeclarativeContributionRegistration for Registration {
    fn withdraw(self: Box<Self>) {
        let Some(entries) = self.entries.upgrade() else {
            return;
        };
        let Ok(mut entries) = entries.lock() else {
            return;
        };
        let matches = entries
            .get(&self.key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token));
        if matches {
            entries.remove(&self.key);
        }
    }
}

impl Host {
    fn failing(kind: ContributionKind) -> Self {
        Self {
            fail_on: Some(kind),
            ..Self::default()
        }
    }

    fn requiring(required_services: &'static [heycode_core::ServiceKey]) -> Self {
        Self {
            required_services,
            ..Self::default()
        }
    }

    fn activate(
        &self,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.attempted.lock().unwrap().push(contribution.kind());
        if self.fail_on == Some(contribution.kind()) {
            return Err(HostActivationFailure::InvalidDefinition);
        }
        let key = (contribution.kind(), contribution.public_name().to_owned());
        let token = Arc::new(());
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| HostActivationFailure::Unavailable)?;
        if entries.contains_key(&key) {
            return Err(HostActivationFailure::Duplicate);
        }
        entries.insert(
            key.clone(),
            Entry {
                document: contribution.document().to_owned(),
                token: Arc::clone(&token),
            },
        );
        drop(entries);

        Ok(Box::new(Registration {
            entries: Arc::downgrade(&self.entries),
            key,
            token,
        }))
    }

    fn snapshot(&self) -> BTreeMap<(ContributionKind, String), String> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|(key, entry)| (key.clone(), entry.document.clone()))
            .collect()
    }

    fn attempted(&self) -> Vec<ContributionKind> {
        self.attempted.lock().unwrap().clone()
    }
}

impl DeclarativeContributionHost for Host {
    fn required_services(&self) -> &'static [heycode_core::ServiceKey] {
        self.required_services
    }

    fn descriptor_families(&self) -> &'static [PluginContributionKind] {
        &[
            PluginContributionKind::Command,
            PluginContributionKind::Provider,
        ]
    }

    fn inventory(&self, contribution: &DeclarativeContribution) -> Vec<PluginContributionSpec> {
        let kind = match contribution.kind() {
            ContributionKind::Command => CoreContributionKind::Command,
            ContributionKind::Provider => CoreContributionKind::InferenceProvider,
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
id = "acme/reviewer"
name = "Reviewer"
version = "1.0.0"
description = "Six declarative contribution fixtures."
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

[[contributions]]
kind = "mcp"
id = "bundled"
path = "mcp/bundled.json"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixtures/reviewer"
revision = "1.0.0"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#
}

fn write_package(root: &Path) {
    let documents = [
        (
            "skills/review/SKILL.md",
            "---\nname: review\ndescription: Review code\n---\nReview this change.",
        ),
        ("commands/review.json", r#"{"timing":"queued"}"#),
        (
            "agents/reviewer.json",
            r#"{"instructions":"Review carefully"}"#,
        ),
        ("hooks/pre-review.json", r#"{"phase":"pre","event":"turn"}"#),
        ("themes/sunset.json", r#"{"title":"Sunset"}"#),
        (
            "providers/example.json",
            r#"{"protocol":"openai_chat_completions"}"#,
        ),
        ("mcp/bundled.json", r#"{"transport":"stdio"}"#),
    ];
    std::fs::create_dir_all(root.join(".heycode-plugin")).unwrap();
    std::fs::write(root.join(".heycode-plugin/plugin.toml"), manifest()).unwrap();
    for (relative, document) in documents {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, document).unwrap();
    }
}

fn installed_package() -> (tempfile::TempDir, InstalledPlugin) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source);
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator()).unwrap();
    let installed = cache.install_directory(&source).unwrap();
    (temp, installed)
}

fn package() -> (tempfile::TempDir, DeclarativePackage) {
    let (temp, installed) = installed_package();
    let package = DeclarativePackage::load(&installed).unwrap();
    (temp, package)
}

#[test]
fn all_six_declarative_kinds_activate_and_dispose_in_one_real_composition() {
    let (_temp, package) = package();
    assert_eq!(package.contributions().len(), 6);
    assert_eq!(package.deferred_mcp_contributions(), 1);
    let host = Arc::new(Host::default());
    let plugin = declarative_activation_plugin(
        vec![package],
        Arc::clone(&host) as Arc<dyn DeclarativeContributionHost>,
    )
    .unwrap();

    let mut context = compose(&[plugin]).unwrap();
    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.kind == CoreContributionKind::Command && row.name == "acme/reviewer::review"
    }));
    assert!(inventory.contributions.iter().any(|row| {
        row.kind == CoreContributionKind::InferenceProvider && row.name == "acme/reviewer::example"
    }));
    let live = host.snapshot();
    assert_eq!(live.len(), 6);
    let kinds = [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ];
    for kind in kinds {
        assert!(live.keys().any(|(registered, _)| *registered == kind));
    }
    assert_eq!(host.attempted(), kinds);

    context.shutdown();
    assert!(host.snapshot().is_empty());
}

#[test]
fn every_kind_can_refuse_and_rolls_back_its_exact_prefix() {
    let (_temp, package) = package();
    let kinds = [
        ContributionKind::Skill,
        ContributionKind::Command,
        ContributionKind::Agent,
        ContributionKind::Hook,
        ContributionKind::Theme,
        ContributionKind::Provider,
    ];
    for (index, kind) in kinds.iter().copied().enumerate() {
        let host = Arc::new(Host::failing(kind));
        let plugin = declarative_activation_plugin(
            vec![package.clone()],
            Arc::clone(&host) as Arc<dyn DeclarativeContributionHost>,
        )
        .unwrap();

        let error = compose(&[plugin])
            .err()
            .expect("the selected contribution candidate fails");
        assert!(error.to_string().contains(kind.as_str()));
        assert_eq!(host.attempted(), kinds[..=index]);
        assert!(host.snapshot().is_empty(), "earlier kinds must roll back");
    }
}

#[test]
fn active_generation_rejects_a_duplicate_package_before_composition() {
    let (_temp, package) = package();
    let host = Arc::new(Host::default());
    let error = declarative_activation_plugin(
        vec![package.clone(), package],
        host as Arc<dyn DeclarativeContributionHost>,
    )
    .err()
    .expect("one package id cannot be active twice");
    assert!(error.to_string().contains("more than once"));
}

#[test]
fn host_service_dependencies_fail_admission_before_any_document_activates() {
    const MISSING: heycode_core::ServiceKey = heycode_core::ServiceKey::new("missing-registry");
    let (_temp, package) = package();
    let host = Arc::new(Host::requiring(&[MISSING]));
    let plugin = declarative_activation_plugin(
        vec![package],
        Arc::clone(&host) as Arc<dyn DeclarativeContributionHost>,
    )
    .unwrap();

    let error = compose(&[plugin])
        .err()
        .expect("the required host registry is absent");
    assert!(error.to_string().contains("missing-registry"));
    assert!(host.attempted().is_empty());
    assert!(host.snapshot().is_empty());
}

#[test]
fn installed_documents_are_rechecked_and_never_rendered_by_debug() {
    let (_temp, package) = package();
    let skill = package
        .contributions()
        .iter()
        .find(|contribution| contribution.kind() == ContributionKind::Skill)
        .unwrap();
    let rendered = format!("{skill:?}");
    assert!(!rendered.contains("Review this change"));
    assert!(rendered.contains("document_bytes"));

    let (_temp, installed) = installed_package();
    let document = installed.package_root().join("commands/review.json");
    std::fs::write(&document, [0xff, 0xfe]).unwrap();
    let error = DeclarativePackage::load(&installed).unwrap_err();
    assert!(matches!(
        error,
        DeclarativePluginError::Document {
            kind: ContributionKind::Command,
            fault: DeclarativeDocumentFault::NotUtf8,
            ..
        }
    ));
}

#[test]
fn a_hard_link_or_oversized_primary_document_is_refused_before_composition() {
    let (_temp, installed) = installed_package();
    let command = installed.package_root().join("commands/review.json");
    let alias = installed.package_root().join("commands/alias.json");
    std::fs::hard_link(&command, &alias).unwrap();
    let error = DeclarativePackage::load(&installed).unwrap_err();
    assert!(matches!(
        error,
        DeclarativePluginError::Document {
            kind: ContributionKind::Command,
            fault: DeclarativeDocumentFault::Unsafe,
            ..
        }
    ));

    let (_temp, installed) = installed_package();
    std::fs::write(
        installed.package_root().join("commands/review.json"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .unwrap();
    let error = DeclarativePackage::load(&installed).unwrap_err();
    assert!(matches!(
        error,
        DeclarativePluginError::Document {
            kind: ContributionKind::Command,
            fault: DeclarativeDocumentFault::TooLarge,
            ..
        }
    ));
}

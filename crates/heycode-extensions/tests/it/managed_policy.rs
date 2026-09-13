//! PL08 managed marketplace/plugin admission contracts.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use heycode_core::{Context, PluginContributionKind, compose};
use heycode_extensions::lifecycle::{PluginLifecycleAdmission, PluginOperation};
use heycode_extensions::{
    ApiVersion, Architecture, CatalogDigest, ContributionKind, DeclarativeContribution,
    DeclarativeContributionHost, DeclarativeContributionRegistration, HostActivationFailure,
    InstallDisposition, ManagedCapabilityPolicy, ManagedChecksumRequirement,
    ManagedLifecycleAdmission, ManagedPluginAdmissionGeneration,
    ManagedPluginAdmissionGenerationId, ManagedPluginError, ManagedPluginPolicy,
    ManagedPluginPolicyRule, ManagedPolicyAxis, ManagedPolicyConfigurationError,
    ManagedPolicyVerdict, ManagedSignatureRequirement, ManifestValidator, MarketplaceCatalog,
    MarketplaceId, MarketplaceSource, MarketplaceSourceKind, OperatingSystem, PackageOrigin,
    PlatformTarget, PluginGraphResolver, PluginId, PluginInstallCache, PluginPermission,
    PluginVersion, SignatureState, UpdateChannel, resolved_managed_declarative_activation_plugin,
};

const SIGNATURE: &str =
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
const HOST: PlatformTarget = PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64);

struct Fixture {
    _temp: tempfile::TempDir,
    package: PathBuf,
    source: MarketplaceSource,
    catalog: MarketplaceCatalog,
    id: PluginId,
    version: PluginVersion,
    installed: heycode_extensions::InstalledPlugin,
}

fn validator() -> ManifestValidator {
    ManifestValidator::new(ApiVersion::new(1).unwrap(), HOST)
}

fn manifest(channel: &str, permission: Option<&str>, upstream_checksum: bool) -> String {
    let permissions = permission
        .map(|value| format!("[\"{value}\"]"))
        .unwrap_or_else(|| "[]".to_owned());
    let checksum = if upstream_checksum {
        format!("checksum = \"sha256:{}\"\n", "a".repeat(64))
    } else {
        String::new()
    };
    format!(
        r#"schema_version = 1
id = "acme/reviewer"
name = "Reviewer"
version = "1.0.0"
description = "Managed policy fixture."
license = "MIT"
default_enabled = false
requested_permissions = {permissions}
platforms = [
  {{ os = "macos", architecture = "aarch64" }},
  {{ os = "linux", architecture = "x86_64" }},
]
dependencies = []
conflicts = []
contributions = [{{ kind = "skill", id = "review", path = "skills/review/SKILL.md", exposure = {{ mode = "namespaced" }} }}]

[api]
minimum = 1
maximum = 1

[source]
kind = "https"
locator = "https://packages.example.com/acme/reviewer"
revision = "1.0.0"
{checksum}update_channel = "{channel}"

[authentication]
policy = "none"
credentials = []
"#,
    )
}

fn write_package(
    root: &Path,
    body: &str,
    channel: &str,
    permission: Option<&str>,
    upstream_checksum: bool,
) {
    std::fs::create_dir_all(root.join(".heycode-plugin")).unwrap();
    std::fs::create_dir_all(root.join("skills/review")).unwrap();
    std::fs::write(
        root.join(".heycode-plugin/plugin.toml"),
        manifest(channel, permission, upstream_checksum),
    )
    .unwrap();
    std::fs::write(root.join("skills/review/SKILL.md"), body).unwrap();
}

fn catalog_document(content: &str, signature: bool) -> String {
    let signature = if signature {
        format!(
            "signature = {{ algorithm = \"ed25519\", key_id = \"acme-release\", value = \"{SIGNATURE}\" }}\n"
        )
    } else {
        String::new()
    };
    format!(
        r#"schema_version = 1
marketplace = "acme"

[[packages]]
id = "acme/reviewer"
version = "1.0.0"
source_kind = "https"
locator = "https://packages.example.com/acme/reviewer"
content = "{content}"
{signature}"#,
    )
}

fn fixture(
    signature: bool,
    channel: &str,
    permission: Option<&str>,
    upstream_checksum: bool,
) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    write_package(
        &package,
        "Review only the requested change.",
        channel,
        permission,
        upstream_checksum,
    );
    let scratch = PluginInstallCache::open(temp.path().join("scratch"), validator()).unwrap();
    let installed = scratch.install_directory(&package).unwrap();
    let catalog_raw = catalog_document(installed.content_hash().as_str(), signature);
    let source = MarketplaceSource::new(
        MarketplaceId::new("acme").unwrap(),
        MarketplaceSourceKind::Https,
        "https://marketplace.example.com/catalog.toml",
        CatalogDigest::of_bytes(catalog_raw.as_bytes()).as_str(),
    )
    .unwrap();
    let catalog = MarketplaceCatalog::parse_pinned(&source, catalog_raw.as_bytes()).unwrap();
    Fixture {
        _temp: temp,
        package,
        source,
        catalog,
        id: PluginId::new("acme/reviewer").unwrap(),
        version: PluginVersion::parse("1.0.0").unwrap(),
        installed,
    }
}

fn capabilities(
    permissions: impl IntoIterator<Item = PluginPermission>,
    contributions: impl IntoIterator<Item = ContributionKind>,
) -> ManagedCapabilityPolicy {
    ManagedCapabilityPolicy::new(permissions, contributions).unwrap()
}

fn policy(
    fixture: &Fixture,
    channel: UpdateChannel,
    host: PlatformTarget,
    checksum: ManagedChecksumRequirement,
    signature: ManagedSignatureRequirement,
    capabilities: ManagedCapabilityPolicy,
) -> ManagedPluginPolicy {
    let rule = ManagedPluginPolicyRule::from_catalog(
        &fixture.source,
        &fixture.catalog,
        &fixture.id,
        &fixture.version,
        channel,
        host,
        checksum,
        signature,
        capabilities,
    )
    .unwrap();
    ManagedPluginPolicy::new([rule]).unwrap()
}

fn permissive_policy(
    fixture: &Fixture,
    signature: ManagedSignatureRequirement,
) -> ManagedPluginPolicy {
    policy(
        fixture,
        UpdateChannel::Pinned,
        HOST,
        ManagedChecksumRequirement::CatalogAndPackage,
        signature,
        capabilities([], [ContributionKind::Skill]),
    )
}

fn evaluate(
    fixture: &Fixture,
    policy: &ManagedPluginPolicy,
    host: PlatformTarget,
) -> heycode_extensions::ManagedPolicyEvaluation {
    policy.evaluate_marketplace(
        &fixture.source,
        &fixture.catalog,
        &fixture.id,
        &fixture.version,
        fixture.installed.manifest(),
        fixture.installed.content_hash(),
        host,
    )
}

fn resolved_graph(fixture: &Fixture) -> heycode_extensions::ResolvedPluginGraph {
    PluginGraphResolver::new(HOST)
        .resolve([fixture.installed.manifest().clone()])
        .unwrap()
}

fn assert_cache_unmodified(cache: &PluginInstallCache) {
    assert!(cache.inspect().unwrap().packages.is_empty());
    for relative in [".objects/sha256", ".refs", ".staging"] {
        assert_eq!(
            std::fs::read_dir(cache.root().join(relative))
                .unwrap()
                .count(),
            0,
            "managed refusal left cache state in {relative}"
        );
    }
}

#[test]
fn every_policy_axis_is_explicit_for_an_exact_pinned_package() {
    let fixture = fixture(true, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::Declared);
    let evaluation = evaluate(&fixture, &policy, HOST);

    assert_eq!(
        ManagedPolicyAxis::ALL,
        [
            ManagedPolicyAxis::Source,
            ManagedPolicyAxis::Channel,
            ManagedPolicyAxis::Publisher,
            ManagedPolicyAxis::Version,
            ManagedPolicyAxis::Digest,
            ManagedPolicyAxis::Signature,
            ManagedPolicyAxis::Platform,
            ManagedPolicyAxis::Capability,
        ]
    );
    for axis in ManagedPolicyAxis::ALL {
        assert_eq!(
            evaluation.verdict(axis),
            ManagedPolicyVerdict::Allowed,
            "{axis:?} was not affirmatively allowed"
        );
    }
    evaluation.ensure_allowed().unwrap();
}

#[test]
fn a_forbidden_or_unpinned_marketplace_source_cannot_mutate_the_cache() {
    let fixture = fixture(false, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::NotRequired);
    let graph = resolved_graph(&fixture);
    let mirror = MarketplaceSource::new(
        MarketplaceId::new("acme").unwrap(),
        MarketplaceSourceKind::Https,
        "https://mirror.example.com/catalog.toml",
        fixture.source.catalog_digest().as_str(),
    )
    .unwrap();
    let cache =
        PluginInstallCache::open(fixture._temp.path().join("managed"), validator()).unwrap();

    let error = cache
        .install_resolved_managed_marketplace(
            &graph,
            &fixture.package,
            &mirror,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &policy,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ManagedPluginError::Policy(ref rejection)
            if rejection.axis() == ManagedPolicyAxis::Source
                && rejection.verdict() == ManagedPolicyVerdict::Denied
    ));
    assert_cache_unmodified(&cache);

    let no_rules = ManagedPluginPolicy::new([]).unwrap();
    let error = cache
        .install_resolved_managed_marketplace(
            &graph,
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &no_rules,
        )
        .unwrap_err();
    assert!(matches!(error, ManagedPluginError::Policy(_)));
    assert_cache_unmodified(&cache);
}

#[test]
fn substituted_package_bytes_are_denied_before_a_reference_or_object_publishes() {
    let fixture = fixture(false, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::NotRequired);
    std::fs::write(
        fixture.package.join("skills/review/SKILL.md"),
        "Substituted package body.",
    )
    .unwrap();
    let cache =
        PluginInstallCache::open(fixture._temp.path().join("managed"), validator()).unwrap();

    let error = cache
        .install_managed_marketplace(
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &policy,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ManagedPluginError::Policy(ref rejection)
            if rejection.axis() == ManagedPolicyAxis::Digest
    ));
    assert_cache_unmodified(&cache);
}

#[test]
fn unverified_signature_and_upstream_checksum_evidence_stay_unknown_and_denied() {
    let signed = fixture(true, "pinned", None, false);
    let signature_policy = permissive_policy(&signed, ManagedSignatureRequirement::Verified);
    let signature = evaluate(&signed, &signature_policy, HOST);
    assert_eq!(
        signature.verdict(ManagedPolicyAxis::Signature),
        ManagedPolicyVerdict::Unknown
    );
    let rejection = signature.ensure_allowed().unwrap_err();
    assert_eq!(rejection.axis(), ManagedPolicyAxis::Signature);
    assert_eq!(rejection.verdict(), ManagedPolicyVerdict::Unknown);

    let checksummed = fixture(false, "pinned", None, true);
    let checksum_policy = policy(
        &checksummed,
        UpdateChannel::Pinned,
        HOST,
        ManagedChecksumRequirement::VerifiedUpstreamArtifact,
        ManagedSignatureRequirement::NotRequired,
        capabilities([], [ContributionKind::Skill]),
    );
    let checksum = evaluate(&checksummed, &checksum_policy, HOST);
    assert_eq!(
        checksum.verdict(ManagedPolicyAxis::Digest),
        ManagedPolicyVerdict::Unknown
    );
    assert_eq!(
        checksum.ensure_allowed().unwrap_err().axis(),
        ManagedPolicyAxis::Digest
    );
    let cache =
        PluginInstallCache::open(checksummed._temp.path().join("managed"), validator()).unwrap();
    let error = cache
        .install_managed_marketplace(
            &checksummed.package,
            &checksummed.source,
            &checksummed.catalog,
            &checksummed.id,
            &checksummed.version,
            HOST,
            &checksum_policy,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ManagedPluginError::Policy(ref rejection)
            if rejection.axis() == ManagedPolicyAxis::Digest
                && rejection.verdict() == ManagedPolicyVerdict::Unknown
    ));
    assert_cache_unmodified(&cache);
}

#[test]
fn channel_platform_and_capability_constraints_each_deny_their_own_axis() {
    let fixture = fixture(false, "pinned", Some("process_spawn"), false);
    let stable = policy(
        &fixture,
        UpdateChannel::Stable,
        HOST,
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        capabilities([PluginPermission::ProcessSpawn], [ContributionKind::Skill]),
    );
    assert_eq!(
        evaluate(&fixture, &stable, HOST).verdict(ManagedPolicyAxis::Channel),
        ManagedPolicyVerdict::Denied
    );

    let linux = policy(
        &fixture,
        UpdateChannel::Pinned,
        PlatformTarget::new(OperatingSystem::Linux, Architecture::X86_64),
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        capabilities([PluginPermission::ProcessSpawn], [ContributionKind::Skill]),
    );
    assert_eq!(
        evaluate(&fixture, &linux, HOST).verdict(ManagedPolicyAxis::Platform),
        ManagedPolicyVerdict::Denied
    );

    let restricted = policy(
        &fixture,
        UpdateChannel::Pinned,
        HOST,
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        capabilities([], [ContributionKind::Skill]),
    );
    assert_eq!(
        evaluate(&fixture, &restricted, HOST).verdict(ManagedPolicyAxis::Capability),
        ManagedPolicyVerdict::Denied
    );
}

#[test]
fn publisher_version_and_absent_signature_evidence_cannot_hide_in_other_axes() {
    let fixture = fixture(false, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::NotRequired);
    let impostor = MarketplaceSource::new(
        MarketplaceId::new("evil").unwrap(),
        MarketplaceSourceKind::Https,
        fixture.source.locator(),
        fixture.source.catalog_digest().as_str(),
    )
    .unwrap();
    let publisher = policy.evaluate_marketplace(
        &impostor,
        &fixture.catalog,
        &fixture.id,
        &fixture.version,
        fixture.installed.manifest(),
        fixture.installed.content_hash(),
        HOST,
    );
    assert_eq!(
        publisher.verdict(ManagedPolicyAxis::Publisher),
        ManagedPolicyVerdict::Denied
    );

    let mismatched_manifest = validator()
        .validate_toml(&manifest("pinned", None, false).replacen(
            "version = \"1.0.0\"",
            "version = \"2.0.0\"",
            1,
        ))
        .unwrap();
    let version = policy.evaluate_marketplace(
        &fixture.source,
        &fixture.catalog,
        &fixture.id,
        &fixture.version,
        &mismatched_manifest,
        fixture.installed.content_hash(),
        HOST,
    );
    assert_eq!(
        version.verdict(ManagedPolicyAxis::Version),
        ManagedPolicyVerdict::Denied
    );

    let signature_policy = permissive_policy(&fixture, ManagedSignatureRequirement::Declared);
    assert_eq!(
        evaluate(&fixture, &signature_policy, HOST).verdict(ManagedPolicyAxis::Signature),
        ManagedPolicyVerdict::Denied
    );
}

#[test]
fn malformed_administrator_rules_fail_instead_of_being_normalized() {
    assert_eq!(
        ManagedCapabilityPolicy::new(
            [
                PluginPermission::ProcessSpawn,
                PluginPermission::ProcessSpawn
            ],
            [ContributionKind::Skill],
        )
        .unwrap_err(),
        ManagedPolicyConfigurationError::DuplicatePermission
    );
    assert_eq!(
        ManagedCapabilityPolicy::new([], [ContributionKind::Skill, ContributionKind::Skill],)
            .unwrap_err(),
        ManagedPolicyConfigurationError::DuplicateContribution
    );

    let fixture = fixture(false, "pinned", None, false);
    let rule = ManagedPluginPolicyRule::from_catalog(
        &fixture.source,
        &fixture.catalog,
        &fixture.id,
        &fixture.version,
        UpdateChannel::Pinned,
        HOST,
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        capabilities([], [ContributionKind::Skill]),
    )
    .unwrap();
    assert_eq!(
        ManagedPluginPolicy::new([rule.clone(), rule]).unwrap_err(),
        ManagedPolicyConfigurationError::DuplicateRule
    );

    let wrong_marketplace = MarketplaceSource::new(
        MarketplaceId::new("evil").unwrap(),
        MarketplaceSourceKind::Https,
        fixture.source.locator(),
        fixture.source.catalog_digest().as_str(),
    )
    .unwrap();
    assert_eq!(
        ManagedPluginPolicyRule::from_catalog(
            &wrong_marketplace,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            UpdateChannel::Pinned,
            HOST,
            ManagedChecksumRequirement::CatalogAndPackage,
            ManagedSignatureRequirement::NotRequired,
            capabilities([], [ContributionKind::Skill]),
        )
        .unwrap_err(),
        ManagedPolicyConfigurationError::CatalogSourceMismatch
    );
}

#[test]
fn an_allowed_install_commits_once_and_returns_pl05_provenance() {
    let fixture = fixture(true, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::Declared);
    let graph = resolved_graph(&fixture);
    let cache =
        PluginInstallCache::open(fixture._temp.path().join("managed"), validator()).unwrap();

    let receipt = cache
        .install_resolved_managed_marketplace(
            &graph,
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &policy,
        )
        .unwrap();
    assert_eq!(
        receipt.installed().disposition(),
        InstallDisposition::Installed
    );
    assert!(matches!(
        receipt.provenance().origin(),
        PackageOrigin::Marketplace { marketplace }
            if marketplace.as_str() == "acme"
    ));
    assert_eq!(
        receipt.provenance().content(),
        receipt.installed().content_hash()
    );
    assert!(matches!(
        receipt.provenance().signature(),
        SignatureState::Present(_)
    ));
    receipt.evaluation().ensure_allowed().unwrap();
    assert_eq!(cache.inspect().unwrap().packages.len(), 1);

    let repeated = cache
        .install_resolved_managed_marketplace(
            &graph,
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &policy,
        )
        .unwrap();
    assert_eq!(
        repeated.installed().disposition(),
        InstallDisposition::AlreadyPresent
    );
    assert_eq!(cache.inspect().unwrap().packages.len(), 1);
}

#[test]
fn managed_lifecycle_admission_rechecks_the_cached_version_before_enable() {
    let fixture = fixture(true, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::Declared);
    let cache = Arc::new(
        PluginInstallCache::open(fixture._temp.path().join("managed-lifecycle"), validator())
            .unwrap(),
    );
    cache
        .install_managed_marketplace(
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &policy,
        )
        .unwrap();
    let admission = ManagedLifecycleAdmission::new(
        cache,
        fixture.source.clone(),
        fixture.catalog.clone(),
        HOST,
        policy,
    )
    .unwrap();
    admission
        .authorize(PluginOperation::Enable, &fixture.id, &fixture.version)
        .unwrap();
}

#[test]
fn one_fingerprinted_pl08_generation_drives_lifecycle_and_code_readmission() {
    let fixture = fixture(true, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::Declared);
    let cache = Arc::new(
        PluginInstallCache::open(fixture._temp.path().join("shared-generation"), validator())
            .unwrap(),
    );
    cache
        .install_managed_marketplace(
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &policy,
        )
        .unwrap();
    let generation = ManagedPluginAdmissionGeneration::new(
        fixture.source.clone(),
        fixture.catalog.clone(),
        HOST,
        policy,
    )
    .unwrap();
    let parsed = ManagedPluginAdmissionGenerationId::parse(generation.id().as_str()).unwrap();
    assert_eq!(&parsed, generation.id());

    let admission = ManagedLifecycleAdmission::from_generation(cache.clone(), generation.clone());
    admission
        .authorize(PluginOperation::Enable, &fixture.id, &fixture.version)
        .unwrap();
    let package = generation
        .prepare_managed_declarative(&cache, &fixture.id, &fixture.version)
        .unwrap();
    assert_eq!(
        package.provenance().content().as_str(),
        fixture.installed.content_hash().as_str()
    );
    package.evaluation().ensure_allowed().unwrap();
}

#[test]
fn pl08_generation_identity_changes_with_the_capability_ceiling() {
    let fixture = fixture(false, "pinned", None, false);
    let allowed = ManagedPluginAdmissionGeneration::new(
        fixture.source.clone(),
        fixture.catalog.clone(),
        HOST,
        permissive_policy(&fixture, ManagedSignatureRequirement::NotRequired),
    )
    .unwrap();
    let denied = ManagedPluginAdmissionGeneration::new(
        fixture.source.clone(),
        fixture.catalog.clone(),
        HOST,
        policy(
            &fixture,
            UpdateChannel::Pinned,
            HOST,
            ManagedChecksumRequirement::CatalogAndPackage,
            ManagedSignatureRequirement::NotRequired,
            capabilities([], []),
        ),
    )
    .unwrap();

    assert_ne!(allowed.id(), denied.id());
    assert!(
        ManagedPluginAdmissionGenerationId::parse(
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        )
        .is_err(),
        "generation ids use one canonical lowercase encoding"
    );
}

#[test]
fn pl08_generation_cannot_join_policy_rules_from_another_catalog() {
    let selected = fixture(false, "pinned", None, false);
    let mirror = MarketplaceSource::new(
        MarketplaceId::new("acme").unwrap(),
        MarketplaceSourceKind::Https,
        "https://mirror.example.com/catalog.toml",
        selected.source.catalog_digest().as_str(),
    )
    .unwrap();
    let foreign_rule = ManagedPluginPolicyRule::from_catalog(
        &mirror,
        &selected.catalog,
        &selected.id,
        &selected.version,
        UpdateChannel::Pinned,
        HOST,
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        capabilities([], [ContributionKind::Skill]),
    )
    .unwrap();
    let error = ManagedPluginAdmissionGeneration::new(
        selected.source.clone(),
        selected.catalog.clone(),
        HOST,
        ManagedPluginPolicy::new([foreign_rule]).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ManagedPolicyConfigurationError::CatalogSourceMismatch
    );
}

#[derive(Default)]
struct CountingHost {
    attempts: AtomicUsize,
}

struct NoopRegistration;

impl DeclarativeContributionRegistration for NoopRegistration {
    fn withdraw(self: Box<Self>) {}
}

impl CountingHost {
    fn activate(
        &self,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(NoopRegistration))
    }
}

impl DeclarativeContributionHost for CountingHost {
    fn required_services(&self) -> &'static [heycode_core::ServiceKey] {
        &[]
    }

    fn descriptor_families(&self) -> &'static [PluginContributionKind] {
        &[]
    }

    fn activate_skill(
        &self,
        _context: &Context,
        _contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate()
    }

    fn activate_command(
        &self,
        _context: &Context,
        _contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate()
    }

    fn activate_agent(
        &self,
        _context: &Context,
        _contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate()
    }

    fn activate_hook(
        &self,
        _context: &Context,
        _contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate()
    }

    fn activate_theme(
        &self,
        _context: &Context,
        _contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate()
    }

    fn activate_provider(
        &self,
        _context: &Context,
        _contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.activate()
    }
}

#[test]
fn current_policy_is_rechecked_before_any_declarative_host_activation() {
    let fixture = fixture(false, "pinned", None, false);
    let allowed = permissive_policy(&fixture, ManagedSignatureRequirement::NotRequired);
    let graph = resolved_graph(&fixture);
    let cache =
        PluginInstallCache::open(fixture._temp.path().join("managed"), validator()).unwrap();
    let receipt = cache
        .install_resolved_managed_marketplace(
            &graph,
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &allowed,
        )
        .unwrap();

    let denied = policy(
        &fixture,
        UpdateChannel::Pinned,
        HOST,
        ManagedChecksumRequirement::CatalogAndPackage,
        ManagedSignatureRequirement::NotRequired,
        capabilities([], []),
    );
    let error = cache
        .prepare_managed_declarative(
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &denied,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ManagedPluginError::Policy(ref rejection)
            if rejection.axis() == ManagedPolicyAxis::Capability
    ));

    let host = Arc::new(CountingHost::default());
    assert_eq!(host.attempts.load(Ordering::SeqCst), 0);

    let installed_document = receipt
        .installed()
        .package_root()
        .join("skills/review/SKILL.md");
    std::fs::write(&installed_document, "Tampered after install.").unwrap();
    let error = cache
        .prepare_managed_declarative(
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &allowed,
        )
        .unwrap_err();
    assert!(matches!(error, ManagedPluginError::Cache(_)));
    assert_eq!(host.attempts.load(Ordering::SeqCst), 0);
    std::fs::write(&installed_document, "Review only the requested change.").unwrap();

    let package = cache
        .prepare_managed_declarative(
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &allowed,
        )
        .unwrap();
    let plugin = resolved_managed_declarative_activation_plugin(
        &graph,
        vec![package],
        Arc::clone(&host) as Arc<dyn DeclarativeContributionHost>,
    )
    .unwrap();
    let mut context = compose(&[plugin]).unwrap();
    assert_eq!(host.attempts.load(Ordering::SeqCst), 1);
    context.shutdown();
}

#[test]
fn policy_failures_never_render_locators_signatures_or_package_bodies() {
    let fixture = fixture(true, "pinned", None, false);
    let policy = permissive_policy(&fixture, ManagedSignatureRequirement::Verified);
    let cache =
        PluginInstallCache::open(fixture._temp.path().join("managed"), validator()).unwrap();
    let error = cache
        .install_managed_marketplace(
            &fixture.package,
            &fixture.source,
            &fixture.catalog,
            &fixture.id,
            &fixture.version,
            HOST,
            &policy,
        )
        .unwrap_err();
    let rendered = format!("{error:?} {error}");
    for canary in [
        "marketplace.example.com",
        "packages.example.com",
        SIGNATURE,
        "Review only the requested change",
    ] {
        assert!(!rendered.contains(canary));
    }
    assert_cache_unmodified(&cache);
}
